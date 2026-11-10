//! Input reading with tippecanoe's semantics (`main.cpp:read_input`) on top of
//! vtmake-parse.
//!
//! GeoJSON. tippecanoe reads a source as one jsonpull stream ("serial"), or,
//! with `--read-parallel` (or a file that starts with RS, 0x1E), as CPUS
//! streams that it splits at the separator ("segments"). A stream is one
//! jsonpull parser: values can go over lines, a FeatureCollection can be one
//! large object, and a syntax error stops the stream.
//!
//! vtmake reads each stream in two phases:
//!
//! 1. In parallel, by lines: the fast parser (`vtmake_parse::stream`) reads
//!    each line that is one complete simple Feature (optionally with a comma
//!    after it). Each line is put in a run: blank, clean (fast parser), clean
//!    with a comma, or dirty (all other lines).
//! 2. In order, per stream: the jsonpull port reads the dirty lines and all
//!    lines that are not at the top level of the JSON (for example inside a
//!    FeatureCollection that is not one feature per line). A clean line
//!    between top-level values, or in a list of features, gives the same
//!    feature as jsonpull, so its phase 1 result stays. The phase 1 results
//!    of the other lines are removed.
//!
//! So NDJSON is read fully in parallel, and other GeoJSON gets tippecanoe's
//! exact result. The feature order is the input order: `seq` is
//! `(byte position << SEQ_SHIFT) + n`.
//!
//! A regular file is read through bounded memory mappings. Standard input from a pipe,
//! `.gz` files, CSV and FlatGeobuf are streamed (`stream`): the memory for the
//! input text is bounded.

mod pgcopy;
mod stream;

use crate::cli::{Projection, Source};
use crate::store::{Chunk, FeatMeta, encode_geom, put_varint};
use rayon::prelude::*;
use std::collections::HashMap;
use vtmake_core::{Draw, GeomType};
use vtmake_parse::ParsedFeature;

/// `seq` is `(position << SEQ_SHIFT) + n`. `n` counts the members of a
/// GeometryCollection, which end at the same position.
pub const SEQ_SHIFT: u32 = 20;

/// The size of the phase 1 pieces.
const PIECE: u64 = 128 * 1024 * 1024;

/// A piece or a region of the jsonpull port ends after a `\n` in this
/// distance after its planned end. Without one, it ends in the line (a line
/// can be very long, for example a FeatureCollection on one line).
#[cfg(not(test))]
const MAX_LINE: u64 = 64 * 1024 * 1024;
#[cfg(test)]
const MAX_LINE: u64 = 3;

/// The size of a region of the jsonpull port.
const REGION: u64 = if cfg!(test) { 5 } else { 64 * 1024 * 1024 };

/// tippecanoe exit codes.
const EXIT_ARGS: i32 = 101;
const EXIT_OPEN: i32 = 111;
const EXIT_READ: i32 = 114;

/// The settings for the readers.
pub struct ReadOpts {
    pub projection: Projection,
    pub geometry_scale: i32,
    pub settings: vtmake_parse::geojson::Settings,
    pub read_parallel: bool,
    /// tippecanoe's CPUS: the number of segments with `--read-parallel`.
    pub felt_cpus: usize,
    pub empty_csv_null: bool,
    pub quiet: bool,
    /// `--pgcopy-schema`: every source is a Postgres binary COPY stream.
    pub pgcopy: Option<std::sync::Arc<vtmake_parse::pgcopy::Schema>>,
}

/// The read-time geometry options: `-aw`, `-pw`, `-pW` and
/// `--clip-bounding-box` (`serial.cpp:serialize_feature`).
#[derive(Default)]
pub struct ReadGeom {
    pub wraparound: bool,
    pub use_winding: bool,
    pub reverse_winding: bool,
    /// The boxes in world coordinates: min x, min y, max x, max y.
    pub clip: Vec<[i64; 4]>,
    /// `-aP`.
    pub label_points: bool,
    /// `-pn`.
    pub shared_nodes: bool,
    /// `geometry_scale > 0` (no `-ae`): the stored geometry is rounded.
    pub scaled: bool,
    /// `-t`: the folder for temporary files.
    pub tmpdir: String,
    /// One of the options above.
    pub any: bool,
}

impl ReadGeom {
    pub fn new(o: &crate::cli::Options) -> ReadGeom {
        let project = |lon: f64, lat: f64| match o.projection {
            Projection::Epsg4326 => vtmake_geom::lonlat_to_world(lon, lat),
            Projection::Epsg3857 => vtmake_geom::epsg3857_to_world(lon, lat),
        };
        let clip = o
            .clip_bboxes
            .iter()
            .map(|c| {
                let (minx, maxy) = project(c[0], c[1]);
                let (maxx, miny) = project(c[2], c[3]);
                [minx, miny, maxx, maxy]
            })
            .collect();
        let mut r = ReadGeom {
            wraparound: o.additional[b'w' as usize],
            use_winding: o.prevent[b'w' as usize],
            reverse_winding: o.prevent[b'W' as usize],
            clip,
            label_points: o.additional[b'P' as usize],
            shared_nodes: o.prevent[b'n' as usize],
            scaled: o.geometry_scale > 0,
            tmpdir: o.tmpdir.clone(),
            any: false,
        };
        r.any = r.wraparound || r.use_winding || r.reverse_winding || !r.clip.is_empty() || r.label_points || r.shared_nodes || r.scaled;
        r
    }

    /// The stored geometry is not the input geometry: the metadata bounds
    /// come from the input geometry (`FeatExtra::raw`).
    pub fn changes_bounds(&self) -> bool {
        self.wraparound || !self.clip.is_empty() || self.scaled
    }
}

/// Set once by `main` before the reading starts.
pub static READ_GEOM: std::sync::OnceLock<ReadGeom> = std::sync::OnceLock::new();

/// tippecanoe's CPUS (`main.cpp:init_cpus`): `TIPPECANOE_MAX_THREADS` or the
/// online CPUs, at least 1, rounded down to a power of 2.
pub fn felt_cpus() -> usize {
    let n: i64 = match std::env::var("TIPPECANOE_MAX_THREADS") {
        Ok(v) => v.trim().parse::<i64>().unwrap_or(0),
        Err(_) => {
            unsafe extern "C" {
                fn sysconf(name: i32) -> i64;
            }
            // SAFETY: sysconf has no preconditions. 84 is _SC_NPROCESSORS_ONLN (Linux).
            unsafe { sysconf(84) }
        }
    };
    let n = n.clamp(1, 32767) as usize;
    1 << n.ilog2()
}

/// Where the bytes of a source (or of a window of a streamed source) are.
enum Data<'a> {
    File { file: &'a std::fs::File, off: u64 },
    Mem(&'a [u8]),
}

impl Data<'_> {
    fn read(&self, start: u64, end: u64, buf: &mut Vec<u8>) -> std::io::Result<()> {
        match self {
            Data::File { file, off } => {
                use std::os::unix::fs::FileExt;
                buf.resize((end - start) as usize, 0);
                file.read_exact_at(buf, off + start)
            }
            Data::Mem(m) => {
                buf.clear();
                buf.extend_from_slice(&m[start as usize..end as usize]);
                Ok(())
            }
        }
    }

    /// The position of the first `sep` at or after `p`, or `end`.
    fn find(&self, mut p: u64, end: u64, sep: u8) -> u64 {
        let mut buf = Vec::new();
        while p < end {
            let n = (end - p).min(64 * 1024);
            if self.read(p, p + n, &mut buf).is_err() {
                return end;
            }
            if let Some(i) = memchr::memchr(sep, &buf) {
                return p + i as u64;
            }
            p += n;
        }
        end
    }

    /// The position after the first `sep` at or after `p`, or `end`.
    fn after(&self, p: u64, end: u64, sep: u8) -> u64 {
        (self.find(p, end, sep) + 1).min(end)
    }

    /// A cut at `q` or after it, before `end`: after the first `\n` in
    /// `MAX_LINE` bytes, else at `q` (in a line; `true`).
    fn cut(&self, q: u64, end: u64) -> (u64, bool) {
        let lim = (q + MAX_LINE).min(end);
        let f = self.find(q, lim, b'\n');
        if f < lim {
            (f + 1, false)
        } else if lim == end {
            (end, false)
        } else {
            (q, true)
        }
    }

    fn byte_is(&self, p: u64, c: u8) -> bool {
        let mut b = Vec::new();
        self.read(p, p + 1, &mut b).is_ok() && b.first() == Some(&c)
    }
}

/// The kind of a run of lines (phase 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Blank,
    Clean,
    CleanComma,
    Dirty,
}

/// Lines of one kind, in `[start, end)` (input positions).
#[derive(Clone, Copy, Debug)]
struct Run {
    kind: Kind,
    start: u64,
    end: u64,
    lines: u64,
}

pub fn collect_free() {
    rayon::broadcast(|_| {
        // SAFETY: only free allocator pages are removed.
        unsafe { mi_collect(true); }
    });
    // SAFETY: the caller also uses mimalloc.
    unsafe { mi_collect(true); }
}

struct Builder {
    c: Chunk,
    proj: Projection,
    geometry_scale: i32,
    rg: &'static ReadGeom,
    clip_out: Vec<Draw>,
    clip_scratch: vtmake_geom::PolyClipScratch,
    scratch: Vec<Draw>,
    fixed: Vec<Draw>,
    keys: HashMap<Box<str>, u32, foldhash::fast::FixedState>,
    /// Value ids, keyed by a hash of (type, text) in `c.vals`.
    vals: hashbrown::HashTable<u32>,
    hasher: foldhash::fast::FixedState,
    layers: HashMap<Box<str>, u16>,
    previous_keys: Vec<u32>,
    previous_vals: Vec<u32>,
}

impl Builder {
    fn new(proj: Projection, geometry_scale: i32) -> Self {
        Builder {
            c: Chunk::default(),
            proj,
            geometry_scale,
            rg: READ_GEOM.get_or_init(ReadGeom::default),
            clip_out: Vec::new(),
            clip_scratch: Default::default(),
            scratch: Vec::new(),
            fixed: Vec::new(),
            keys: HashMap::with_hasher(foldhash::fast::FixedState::with_seed(0)),
            vals: hashbrown::HashTable::new(),
            hasher: foldhash::fast::FixedState::with_seed(0),
            layers: HashMap::new(),
            previous_keys: Vec::new(),
            previous_vals: Vec::new(),
        }
    }

    /// The chunk is full: its offsets are 32 bits.
    fn full(&self) -> bool {
        const L: usize = 1 << 30;
        self.c.geom.len() > L || self.c.attrs.len() > L || self.c.val_text.len() > L || self.c.metas.len() > L
    }

    fn finish(mut self) -> Chunk {
        self.c.vals.shrink_to_fit();
        self.c.val_text.shrink_to_fit();
        self.c.metas.shrink_to_fit();
        self.c.seqs.shrink_to_fit();
        self.c.ids.shrink_to_fit();
        self.c.geom.shrink_to_fit();
        self.c.attrs.shrink_to_fit();
        self.c.stat_attrs.shrink_to_fit();
        self.c.stat_offs.shrink_to_fit();
        self.c
    }

    fn add_parsed(&mut self, f: &ParsedFeature, seq: u64) {
        add_feature(self, f, seq, f.attrs.iter().map(|a| (a.key.as_ref(), a.ty, a.value.as_ref())));
    }
}

/// Phase 1 for one piece of a stream: the fast parser on each line.
/// `cuts`: the stream starts (input positions); a run does not go over one.
/// `head_cut`, `tail_cut`: the piece starts (ends) in a line; that part of
/// the line is dirty.
fn parse_piece(data: &[u8], base: u64, proj: Projection, geometry_scale: i32, cuts: &[u64], head_cut: bool, tail_cut: bool) -> (Chunk, Vec<Run>) {
    let mut next_cut = cuts.partition_point(|&c| c <= base);
    let mut b = Builder::new(proj, geometry_scale);
    let mut runs: Vec<Run> = Vec::new();
    // Most source lines produce one feature. Avoid repeated metadata copies.
    b.c.metas.reserve(memchr::memchr_iter(b'\n', data).count() + usize::from(!data.ends_with(b"\n")));
    let mut fast = vtmake_parse::stream::StreamParser::default();
    let mut off = 0usize;
    for end in memchr::memchr_iter(b'\n', data).chain(std::iter::once(data.len())) {
        if off > data.len() || (end == data.len() && off == end && !runs.is_empty()) {
            break;
        }
        let line = &data[off..end];
        let start = base + off as u64;
        let kind = if (head_cut && off == 0) || (tail_cut && end == data.len()) {
            Kind::Dirty
        } else if line.iter().all(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n' | 0x1E)) {
            Kind::Blank
        } else if fast.parse(line) {
            add_feature(&mut b, &fast.feature, start << SEQ_SHIFT, fast.attrs.iter().map(|a| (a.key.as_ref(), a.ty, a.value.as_ref())));
            if fast.comma { Kind::CleanComma } else { Kind::Clean }
        } else {
            Kind::Dirty
        };
        let stop = base + (end + 1).min(data.len()) as u64;
        let mut new_stream = false;
        while next_cut < cuts.len() && cuts[next_cut] <= start {
            new_stream = true;
            next_cut += 1;
        }
        match runs.last_mut() {
            Some(r) if r.kind == kind && !new_stream => {
                r.end = stop;
                r.lines += 1;
            }
            _ => runs.push(Run { kind, start, end: stop, lines: 1 }),
        }
        off = end + 1;
    }
    (b.finish(), runs)
}

/// The streams of a GeoJSON source and how to read them.
struct Plan {
    /// `[start, end)` of each jsonpull stream.
    streams: Vec<(u64, u64)>,
    /// Separator 0x1E: every stream goes to the jsonpull port.
    pull_only: bool,
}

/// `do_read_parallel`: split `[start, end)` into `cpus` segments at `sep`.
#[cfg(test)]
fn segments(d: &Data<'_>, start: u64, end: u64, cpus: usize, sep: u8) -> Vec<(u64, u64)> {
    segments_with_bases(d, start, end, cpus, sep).into_iter().map(|s| (s.0, s.1)).collect()
}

/// `do_read_parallel`: split `[start, end)` into `cpus` segments at `sep`,
/// with tippecanoe's start of each segment (the position of
/// the separator; the first sequence number of the segment, see
/// `crate::attrs::SeqPlan`).
fn segments_with_bases(d: &Data<'_>, start: u64, end: u64, cpus: usize, sep: u8) -> Vec<(u64, u64, u64)> {
    let len = end - start;
    let mut segs = vec![start];
    for i in 1..cpus {
        let p = start + (len as u128 * i as u128 / cpus as u128) as u64;
        // The segment starts at the separator (or at the end).
        let s = d.find(p, end, sep).max(*segs.last().unwrap());
        segs.push(s);
    }
    segs.push(end);
    let mut out = Vec::new();
    for w in segs.windows(2) {
        // A '\n' at the start of a segment is white space at the end of the
        // last one: the streams start at line starts.
        let (mut s, e) = (w[0], w[1]);
        if sep == b'\n' && s > start && s < e {
            s += 1;
        }
        if let Some(last) = out.last_mut() {
            let (_, le, _): &mut (u64, u64, u64) = last;
            if sep == b'\n' && *le < s {
                *le = s;
            }
        }
        out.push((s, e, w[0]));
    }
    out.retain(|&(s, e, _)| s < e);
    out
}

/// Phase 2 state for one stream.
struct Fixer<'a> {
    ro: &'a ReadOpts,
    fname: &'a str,
    base: u64,
    pull: vtmake_parse::pull::Pull,
    lp: vtmake_parse::geojson::Loop,
    stopped: bool,
    /// Phase 1 results to remove: `[start, end)` positions.
    dead: Vec<(u64, u64)>,
    out: Vec<Chunk>,
    builder: Builder,
    buf: Vec<u8>,
    found: Vec<(usize, ParsedFeature)>,
}

impl<'a> Fixer<'a> {
    fn new(ro: &'a ReadOpts, fname: &'a str, base: u64) -> Self {
        Fixer {
            ro,
            fname,
            base,
            pull: vtmake_parse::pull::Pull::new(),
            lp: vtmake_parse::geojson::Loop::default(),
            stopped: false,
            dead: Vec::new(),
            out: Vec::new(),
            builder: Builder::new(ro.projection, ro.geometry_scale),
            buf: Vec::new(),
            found: Vec::new(),
        }
    }

    /// Give `[start, end)` to the jsonpull port. `last`: the stream ends at `end`.
    fn feed(&mut self, data: &Data<'_>, start: u64, end: u64, last: bool) {
        let mut p = start;
        loop {
            if self.stopped {
                return;
            }
            // Regions of about 64 MB that end after a newline (or in a long line).
            let mut q = (p + REGION).min(end);
            if q < end {
                q = data.cut(q, end).0;
            }
            let mut buf = std::mem::take(&mut self.buf);
            if let Err(e) = data.read(p, q, &mut buf) {
                eprintln!("{}: {e}", self.fname);
                std::process::exit(EXIT_READ)
            }
            let final_piece = last && q == end;
            self.pull.set_input(&buf, final_piece);
            self.buf = buf;
            let go_on = self.lp.run(&mut self.pull, &self.ro.settings, self.fname, &mut self.found);
            let mut prev = usize::MAX;
            let mut k = 0;
            for (pos, f) in std::mem::take(&mut self.found) {
                k = if pos == prev { k + 1 } else { 0 };
                prev = pos;
                let seq = ((self.base + p + pos as u64) << SEQ_SHIFT) + k;
                self.builder.add_parsed(&f, seq);
                if self.builder.full() {
                    let b = std::mem::replace(&mut self.builder, Builder::new(self.ro.projection, self.ro.geometry_scale));
                    self.out.push(b.finish());
                }
            }
            if !go_on {
                self.stopped = true;
            }
            if q >= end {
                return;
            }
            p = q;
        }
    }

    fn stream(&mut self, data: &Data<'_>, runs: &[Run], end: u64) {
        self.start_stream();
        self.runs(data, runs, end);
        self.end_stream();
    }

    fn start_stream(&mut self) {
        self.pull = vtmake_parse::pull::Pull::new();
        self.lp = vtmake_parse::geojson::Loop::default();
        self.stopped = false;
    }

    /// The runs of a stream (or of a part of it) in order. `end`: the end of
    /// the stream, or `u64::MAX` if it is not in `data`.
    fn runs(&mut self, data: &Data<'_>, runs: &[Run], end: u64) {
        for r in runs {
            if self.stopped {
                if matches!(r.kind, Kind::Clean | Kind::CleanComma) {
                    self.dead.push((r.start, r.end));
                }
                continue;
            }
            match r.kind {
                Kind::Blank => {
                    if !self.pull.at_top() {
                        self.feed(data, r.start, r.end, r.end == end);
                    } else {
                        self.pull.line += r.lines as i32;
                    }
                }
                Kind::Clean | Kind::CleanComma if self.pull.at_top() => {
                    self.lp.found_features += r.lines as i64;
                    self.pull.line += r.lines as i32;
                }
                Kind::CleanComma if self.pull.in_feature_list() => {
                    self.lp.found_features += r.lines as i64;
                    self.pull.line += r.lines as i32;
                    self.pull.skip_item(true);
                }
                Kind::Clean if self.pull.in_feature_list() => {
                    // The first line is an item; a second item needs a comma first.
                    self.lp.found_features += 1;
                    self.pull.line += 1;
                    self.pull.skip_item(false);
                    if r.lines > 1 {
                        let second = data.after(r.start, r.end, b'\n');
                        self.dead.push((second, r.end));
                        self.feed(data, second, r.end, r.end == end);
                    }
                }
                Kind::Clean | Kind::CleanComma => {
                    self.dead.push((r.start, r.end));
                    self.feed(data, r.start, r.end, r.end == end);
                }
                Kind::Dirty => self.feed(data, r.start, r.end, r.end == end),
            }
        }
    }

    fn end_stream(&mut self) {
        if !self.stopped {
            // The end of the stream (an open container is an error).
            self.pull.set_input(&[], true);
            self.lp.run(&mut self.pull, &self.ro.settings, self.fname, &mut self.found);
            self.found.clear();
        }
    }
}

/// A phase 1 piece: `[s, e)`, and if it starts (ends) in a line.
#[derive(Clone, Copy)]
struct Piece {
    s: u64,
    e: u64,
    head_cut: bool,
    tail_cut: bool,
}

/// Split `[s, e)` into pieces of about `size` bytes that end after a `\n`
/// (or in a long line).
fn plan_pieces(d: &Data<'_>, s: u64, e: u64, size: u64, out: &mut Vec<Piece>) {
    let mut p = s;
    let mut head_cut = false;
    while p < e {
        let (q, tail_cut) = if e - p > size { d.cut(p + size, e) } else { (e, false) };
        out.push(Piece { s: p, e: q, head_cut, tail_cut });
        head_cut = tail_cut;
        p = q;
    }
}

/// Read cached pieces first, before the feature store can evict them. Results
/// are restored to input order after parsing. This changes I/O order only.
#[cfg(target_os = "linux")]
fn cached_piece_order(file: &std::fs::File, off: u64, pieces: &[Piece]) -> Vec<usize> {
    use std::os::unix::fs::MetadataExt;
    let mut order: Vec<_> = (0..pieces.len()).collect();
    // Linux can hide residency for files that the caller does not own.
    // SAFETY: these functions have no preconditions.
    let (uid, page) = unsafe { (geteuid(), getpagesize() as usize) };
    let Ok(meta) = file.metadata() else { return order; };
    if meta.uid() != uid { return order; }
    let mut cached = vec![0usize; pieces.len()];
    for (i, p) in pieces.iter().enumerate() {
        let base = (off + p.s) / page as u64 * page as u64;
        let len = (off + p.e - base) as usize;
        // SAFETY: the regular input stays open and is read only. mincore
        // checks residency without reading cold pages or adding mapped RSS.
        let Ok(map) = (unsafe { memmap2::MmapOptions::new().offset(base).len(len).map(file) }) else { continue; };
        let mut resident = vec![0u8; len.div_ceil(page)];
        // SAFETY: base is page aligned and the vector has one byte per page.
        if unsafe { mincore(map.as_ptr().cast_mut().cast(), len, resident.as_mut_ptr()) } == 0 {
            cached[i] = resident.into_iter().filter(|state| state & 1 != 0).count();
        }
    }
    order.sort_by_key(|&i| (cached[i] * page < (pieces[i].e - pieces[i].s) as usize / 2, std::cmp::Reverse(cached[i]), i));
    if std::env::var_os("VTMAKE_VERBOSE").is_some() {
        eprintln!("input cache: {} MiB; cached pieces first", cached.iter().sum::<usize>() * page / 1048576);
    }
    order
}

/// Phase 1 on all pieces, in parallel; `readers`: the number of pieces at a
/// time for a file. The runs have positions in `d`.
fn phase1(d: &Data<'_>, pieces: &[Piece], base: u64, ro: &ReadOpts, fname: &str, cuts_abs: &[u64], readers: usize) -> Vec<(Chunk, Vec<Run>)> {
    let proj = ro.projection;
    let geometry_scale = ro.geometry_scale;
    // Buffered reads are faster for cached small files. For larger files,
    // mappings avoid charging an extra input copy to the cgroup.
    let bytes = pieces.first().zip(pieces.last()).map_or(0, |(a, b)| b.e - a.s);
    let pread = match std::env::var("VTMAKE_READ").as_deref() {
        Ok("pread") => true,
        Ok("mmap") => false,
        _ => bytes <= crate::tile::memory_limit() / 4,
    };
    let order: Vec<_> = {
        #[cfg(target_os = "linux")]
        if !pread && let Data::File { file, off } = d {
            cached_piece_order(file, *off, pieces)
        } else { (0..pieces.len()).collect() }
        #[cfg(not(target_os = "linux"))]
        { (0..pieces.len()).collect() }
    };
    #[cfg(target_os = "linux")]
    if !pread && let Data::File { file, .. } = d {
        use std::os::fd::AsRawFd;
        // SAFETY: this only doubles the bounded kernel read-ahead window.
        unsafe { posix_fadvise(file.as_raw_fd(), 0, 0, 2) };
    }
    let phase1 = |order: &[usize]| {
        order
            .par_iter()
            .map_init(Vec::new, |buf, &i| {
                let p = &pieces[i];
                let (s, e) = (p.s, p.e);
                let r = match d {
                    Data::Mem(m) => parse_piece(&m[s as usize..e as usize], base + s, proj, geometry_scale, cuts_abs, p.head_cut, p.tail_cut),
                    Data::File { file, off } if !pread => {
                        // Bound mapped RSS to one piece per reader. Dropping the
                        // mapping releases RSS while clean file pages remain
                        // available for normal cgroup cache reclaim.
                        // SAFETY: the input file stays open and is read only.
                        let map = unsafe { memmap2::MmapOptions::new().offset(off + s).len((e - s) as usize).map(*file) }.unwrap_or_else(|err| {
                            eprintln!("{fname}: {err}");
                            std::process::exit(EXIT_READ)
                        });
                        // Larger read ahead keeps the disk busy while the
                        // worker parses the current mapped block.
                        let _ = map.advise(memmap2::Advice::Sequential);
                        let r = parse_piece(&map, base + s, proj, geometry_scale, cuts_abs, p.head_cut, p.tail_cut);
                        drop(map);
                        #[cfg(target_os = "linux")]
                        {
                            use std::os::fd::AsRawFd;
                            // SAFETY: the mapping is gone. Release only this
                            // parsed block so that future input stays cached.
                            unsafe { posix_fadvise(file.as_raw_fd(), (off + s) as i64, (e - s) as i64, 4) };
                        }
                        r
                    }
                    Data::File { file, off } => {
                        if let Err(err) = d.read(s, e, buf) {
                            eprintln!("{fname}: {err}");
                            std::process::exit(EXIT_READ)
                        }
                        let r = parse_piece(buf, base + s, proj, geometry_scale, cuts_abs, p.head_cut, p.tail_cut);
                        #[cfg(target_os = "linux")]
                        {
                            use std::os::fd::AsRawFd;
                            // SAFETY: this only removes clean cache pages.
                            unsafe { posix_fadvise(file.as_raw_fd(), (off + s) as i64, (e - s) as i64, 4) };
                        }
                        r
                    }
                };
                let mut r = r;
                for run in &mut r.1 {
                    run.start -= base;
                    run.end -= base;
                }
                (i, r)
            })
            .collect::<Vec<_>>()
    };
    let mut out = if rayon::current_num_threads() > readers && matches!(d, Data::File { .. }) {
        // Bounded input buffers: `readers` pieces at a time.
        rayon::ThreadPoolBuilder::new().num_threads(readers).build().unwrap().install(|| phase1(&order))
    } else {
        phase1(&order)
    };
    #[cfg(target_os = "linux")]
    if !pread && let Data::File { file, .. } = d {
        use std::os::fd::AsRawFd;
        // SAFETY: restore the normal policy for any later jsonpull reads.
        unsafe { posix_fadvise(file.as_raw_fd(), 0, 0, 0) };
    }
    out.sort_unstable_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}

/// Read a GeoJSON source with the plan.
fn read_geojson(d: &Data<'_>, plan: &Plan, base: u64, ro: &ReadOpts, fname: &str, readers: usize) -> Vec<Chunk> {
    let chunks = read_streams(d, plan, base, ro, fname, readers, PIECE);
    collect_free();
    chunks
}

/// Read complete streams with the plan; phase 1 pieces of about `piece`
/// bytes.
fn read_streams(d: &Data<'_>, plan: &Plan, base: u64, ro: &ReadOpts, fname: &str, readers: usize, piece: u64) -> Vec<Chunk> {
    // Phase 1: pieces of the input at line ends (the stream starts are line
    // starts; the runs do not go over them).
    let mut pieces: Vec<Piece> = Vec::new();
    let mut cuts: Vec<u64> = plan.streams.iter().map(|s| s.0).collect();
    if !plan.pull_only && let (Some(&(s, _)), Some(&(_, e))) = (plan.streams.first(), plan.streams.last()) {
        if e - s > piece * readers as u64 && plan.streams.len() == readers {
            // Keep both readers' byte ranges equal even when the last piece
            // is short. These are the same stream cuts used by felt.
            for &(start, end) in &plan.streams { plan_pieces(d, start, end, piece, &mut pieces); }
        } else {
            plan_pieces(d, s, e, piece, &mut pieces);
        }
    }
    // Positions between the streams (white space at a segment start) are not
    // in a stream: a run there is put in the stream before it.
    cuts.dedup();
    let cuts_abs: Vec<u64> = cuts.iter().map(|c| c + base).collect();
    let results = phase1(d, &pieces, base, ro, fname, &cuts_abs, readers);

    let mut chunks = Vec::with_capacity(results.len() + 1);
    let mut stream_runs: Vec<Vec<Run>> = vec![Vec::new(); plan.streams.len()];
    for (c, runs) in results {
        for r in runs {
            let si = plan.streams.partition_point(|s| s.0 <= r.start).saturating_sub(1);
            stream_runs[si].push(r);
        }
        chunks.push(c);
    }
    if plan.pull_only {
        for (si, &(s, e)) in plan.streams.iter().enumerate() {
            stream_runs[si].push(Run { kind: Kind::Dirty, start: s, end: e, lines: 0 });
        }
    }

    // Phase 2, in stream order.
    let simple = stream_runs.iter().all(|rs| rs.iter().all(|r| matches!(r.kind, Kind::Blank | Kind::Clean)));
    if !simple {
        let mut fx = Fixer::new(ro, fname, base);
        for (si, runs) in stream_runs.iter().enumerate() {
            fx.stream(d, runs, plan.streams[si].1);
        }
        let Fixer { mut dead, out, builder, .. } = fx;
        if !dead.is_empty() {
            dead.sort_unstable();
            chunks.par_iter_mut().for_each(|c| remove_dead(c, &dead, base));
        }
        chunks.extend(out);
        chunks.push(builder.finish());
    }
    chunks.retain(|c| !c.metas.is_empty());
    chunks
}

/// Remove the features whose position is in a dead range.
fn remove_dead(c: &mut Chunk, dead: &[(u64, u64)], base: u64) {
    let is_dead = |seq: u64| {
        let p = (seq >> SEQ_SHIFT) - base;
        let i = dead.partition_point(|r| r.0 <= p);
        i > 0 && p < dead[i - 1].1
    };
    if !c.seqs.iter().chain(c.clipped_away.iter().map(|(s, _)| s)).chain(c.node_points.iter().map(|(s, _)| s)).any(|&s| is_dead(s)) {
        return;
    }
    let keep: Vec<bool> = c.seqs.iter().map(|&s| !is_dead(s)).collect();
    let mut k = keep.iter();
    c.metas.retain(|_| *k.next().unwrap());
    if !c.extra.is_empty() {
        let mut k = keep.iter();
        c.extra.retain(|_| *k.next().unwrap());
    }
    let mut k = keep.iter();
    c.seqs.retain(|_| *k.next().unwrap());
    if !c.stat_offs.is_empty() {
        let mut k = keep.iter();
        c.stat_offs.retain(|_| *k.next().unwrap());
    }
    c.clipped_away.retain(|&(s, _)| !is_dead(s));
    c.node_vertices.retain(|&(s, _)| !is_dead(s));
    c.node_points.retain(|&(s, _)| !is_dead(s));
}

fn os_error(e: &std::io::Error) -> String {
    crate::os_error(e)
}

/// The input of one source: `(chunks, positions used)`. `None`: tippecanoe
/// could not open it (the message is written) and goes to the next source.
///
/// `layer_file` is the file of the source whose index is the layer id
/// (tippecanoe uses `sources[layer].file` to decide on `.gz`).
pub fn read_source(src: &Source, layer_file: &str, ro: &ReadOpts, base: u64, threads: usize, argv0: &str) -> Option<(Vec<Chunk>, u64)> {
    use std::os::fd::FromRawFd;
    let stdin = src.file.is_empty();
    let fname = if stdin { "standard input".to_string() } else { src.file.clone() };
    let mut owned;
    // SAFETY: fd 0 is open for the whole program; ManuallyDrop keeps it open.
    let mut stdin_file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(0) });
    let file: &mut std::fs::File = if stdin {
        &mut stdin_file
    } else {
        match std::fs::File::open(&src.file) {
            Ok(f) => {
                owned = f;
                &mut owned
            }
            Err(e) => {
                eprintln!("{}: {}", src.file, os_error(&e));
                return None;
            }
        }
    };
    let ends = |s: &str, x: &str| s.len() > x.len() && s.ends_with(x);
    if let Some(schema) = &ro.pgcopy {
        let (mut chunks, n) = pgcopy::read_pgcopy(file, &fname, schema, ro, base, threads);
        crate::attrs::generate_ids(&mut chunks, base, &crate::attrs::SeqPlan::Serial);
        return Some((chunks, n + 1));
    }
    if src.format == "fgb" || ends(&src.file, ".fgb") {
        let meta = file.metadata().ok().filter(|m| m.is_file());
        let Some(meta) = meta else {
            eprintln!("{argv0}: mmap: {fname}: No such device");
            std::process::exit(108)
        };
        let (mut chunks, n) = stream::read_fgb(file, meta.len(), &fname, ro, base, threads);
        crate::attrs::generate_ids(&mut chunks, base, &crate::attrs::SeqPlan::Fgb { queued: n, cpus: ro.felt_cpus as u64 });
        return Some((chunks, n + 1));
    }
    if src.format == "geobuf" || ends(&src.file, ".geobuf") {
        eprintln!("vtmake build: geobuf input ({fname}) is not supported yet");
        std::process::exit(EXIT_ARGS)
    }
    if src.format == "csv" || ends(&src.file, ".csv") {
        let (mut chunks, n) = stream::read_csv(file, &src.file, ro, base, threads);
        crate::attrs::generate_ids(&mut chunks, base, &crate::attrs::SeqPlan::Serial);
        return Some((chunks, n + 1));
    }

    // GeoJSON. The mapped path: a regular file that is not `.gz`.
    let regular = file.metadata().ok().filter(|m| m.is_file()).map(|m| m.len());
    let off = if stdin {
        use std::io::Seek;
        file.stream_position().ok()
    } else {
        Some(0)
    };
    if let (Some(size), Some(off)) = (regular, off)
        && !ends(&src.file, ".gz")
        && size > off
    {
        let d = Data::File { file, off };
        let len = size - off;
        let sep = if d.byte_is(0, 0x1E) { 0x1E } else if ro.read_parallel { b'\n' } else { 0 };
        let streams = if sep != 0 { segments_with_bases(&d, 0, len, ro.felt_cpus, sep) } else { vec![(0, len, 0)] };
        let plan = Plan { streams: streams.iter().map(|s| (s.0, s.1)).collect(), pull_only: sep == 0x1E };
        let mut chunks = read_geojson(&d, &plan, base, ro, &fname, 4.min(threads));
        crate::attrs::generate_ids(&mut chunks, base, &seq_plan(sep, &streams, len));
        return Some((chunks, len + 1));
    }

    // The stream path: standard input from a pipe, `.gz` files, empty files.
    let (mut chunks, sep, streams, len) = stream::read_geojson(file, ends(layer_file, ".gz"), &fname, ro, base, threads, argv0);
    crate::attrs::generate_ids(&mut chunks, base, &seq_plan(sep, &streams, len));
    Some((chunks, len + 1))
}

/// How tippecanoe numbers the features of a GeoJSON source (`--generate-ids`).
fn seq_plan(sep: u8, streams: &[(u64, u64, u64)], len: u64) -> crate::attrs::SeqPlan {
    if sep == 0 {
        return crate::attrs::SeqPlan::Serial;
    }
    crate::attrs::SeqPlan::Parallel { starts: streams.iter().map(|s| s.0).collect(), bases: streams.iter().map(|s| s.2).collect(), len }
}

fn add_feature<'a>(
    b: &mut Builder,
    f: &ParsedFeature,
    seq: u64,
    attrs: impl ExactSizeIterator<Item = (&'a str, vtmake_core::AttrType, &'a str)>,
) {
    let geom_type = f.geom_type;
    b.scratch.clear();
    let mut first = None;
    let proj = b.proj;
    for p in &f.geom {
        let draw = if p.op == vtmake_core::VT_CLOSEPATH {
            Draw::new(p.op, 0, 0)
        } else {
            let bits = (p.lon.to_bits(), p.lat.to_bits());
            let (x, y) = match first {
                Some((key, xy)) if key == bits => xy,
                _ => match proj {
                    Projection::Epsg4326 => vtmake_geom::lonlat_to_world(p.lon, p.lat),
                    Projection::Epsg3857 => vtmake_geom::epsg3857_to_world(p.lon, p.lat),
                },
            };
            if p.op == vtmake_core::VT_MOVETO {
                first = Some((bits, (x, y)));
            }
            Draw::new(p.op, x, y)
        };
        b.scratch.push(draw);
    }
    // Without the read-time geometry options: the steps of the plain path.
    let (g, bbox): (&[Draw], [i64; 4]) = if b.rg.any {
        match read_geom_options(b, geom_type, seq) {
            Some(bbox) => (&b.clip_out, bbox),
            None => return,
        }
    } else {
        let bbox = vtmake_geom::geometry_bbox(&b.scratch);
        if b.geometry_scale > 0 {
            let scale = (1u64 << b.geometry_scale) as f64;
            for d in &mut b.scratch {
                if d.op == vtmake_core::VT_MOVETO || d.op == vtmake_core::VT_LINETO {
                    d.x = (d.x as f64 / scale).round() as i64;
                    d.y = (d.y as f64 / scale).round() as i64;
                }
            }
        }
        let g: &mut [Draw] = if geom_type == GeomType::Polygon {
            b.fixed.clear();
            vtmake_geom::fix_polygon(&b.scratch, &mut b.fixed);
            &mut b.fixed
        } else {
            &mut b.scratch
        };
        if g.is_empty() {
            return;
        }
        if b.geometry_scale > 0 {
            for d in g.iter_mut() {
                if d.op == vtmake_core::VT_MOVETO || d.op == vtmake_core::VT_LINETO { d.x <<= b.geometry_scale; d.y <<= b.geometry_scale; }
            }
        }
        (&*g, bbox)
    };
    let index = vtmake_geom::feature_index(geom_type, g, &bbox);
    let geom_off = b.c.geom.len() as u32;
    let ndraws = g.len() as u32;
    encode_geom(g, &mut b.c.geom);
    let layer = match &f.layer {
        Some(name) => match b.layers.get(name.as_ref()) {
            Some(&n) => n,
            None => {
                let n = u16::try_from(b.layers.len()).ok().filter(|&n| n < u16::MAX).unwrap_or_else(|| {
                    eprintln!("vtmake build: more than 65534 layers are not supported yet");
                    std::process::exit(101)
                });
                b.layers.insert(name.clone(), n);
                b.c.layers.push(name.clone());
                n
            }
        },
        None => u16::MAX,
    };
    let attr_off = b.c.attrs.len() as u32;
    let mut id = f.id;
    let nattrs = match crate::attrs::rules().filter(|r| r.changes_attrs) {
        None => put_attrs(b, attrs),
        Some(r) => {
            // The attribute options (crate::attrs). The tile statistics see the
            // strings before `--maximum-string-attribute-length` cuts them.
            let mut own: Vec<crate::attrs::Owned> = attrs.map(|(k, t, v)| (k.into(), t, v.to_string())).collect();
            r.apply(&mut own, &mut id);
            if r.max_len > 0 {
                b.c.stat_offs.push(b.c.stat_attrs.len() as u32);
                for (k, t, v) in &own {
                    let key = intern_key(b, k);
                    let val = intern_val(b, *t, v.as_bytes());
                    put_varint(&mut b.c.stat_attrs, key as u64);
                    put_varint(&mut b.c.stat_attrs, val as u64);
                }
            }
            put_attrs(b, own.iter().map(|(k, t, v)| (&**k, *t, r.truncate(*t, v))))
        }
    };
    let id = if let Some(id) = id {
        let pos = b.c.ids.len() as u32;
        b.c.ids.push(id);
        pos
    } else { u32::MAX };
    b.c.seqs.push(seq);
    b.c.metas.push(FeatMeta {
        index,
        bbox,
        id,
        geom_off,
        attr_off,
        ndraws,
        nattrs: u16::try_from(nattrs).unwrap_or_else(|_| {
            eprintln!("vtmake build: a feature with more than 65535 attributes is not supported yet");
            std::process::exit(101)
        }),
        layer,
        geom_type,
        minzoom: f.minzoom,
        maxzoom: f.maxzoom,
        drop_minzoom: -1,
    });
}


/// Add the attributes of a feature to the chunk; returns their number.
#[inline]
fn put_attrs<'a>(b: &mut Builder, attrs: impl Iterator<Item = (&'a str, vtmake_core::AttrType, &'a str)>) -> usize {
    let mut n = 0;
    for (pos, (name, ty, value)) in attrs.enumerate() {
        let cached = b
            .previous_keys
            .get(pos)
            .copied()
            .filter(|&k| b.c.keys[k as usize].as_ref() == name);
        let key = cached.unwrap_or_else(|| intern_key(b, name));
        if pos == b.previous_keys.len() {
            b.previous_keys.push(key);
        } else {
            b.previous_keys[pos] = key;
        }
        let text = value.as_bytes();
        let previous = b.previous_vals.get(pos).copied().filter(|&i| {
            let v = b.c.vals[i as usize];
            v.ty == ty && &b.c.val_text[v.off as usize..(v.off + v.len) as usize] == text
        });
        let val = if let Some(i) = previous { i } else { intern_val(b, ty, text) };
        if pos == b.previous_vals.len() {
            b.previous_vals.push(val);
        } else {
            b.previous_vals[pos] = val;
        }
        put_varint(&mut b.c.attrs, key as u64);
        put_varint(&mut b.c.attrs, val as u64);
        n += 1;
    }
    n
}

/// The key id of `name` in the chunk (a new id for a new key).
#[inline]
fn intern_key(b: &mut Builder, name: &str) -> u32 {
    match b.keys.get(name) {
        Some(&k) => k,
        None => {
            let n = b.c.push_key(name.into());
            b.keys.insert(name.into(), n);
            n
        }
    }
}

/// The value id of (`ty`, `text`) in the chunk (a new id for a new value).
#[inline]
fn intern_val(b: &mut Builder, ty: vtmake_core::AttrType, text: &[u8]) -> u32 {
    let h = {
        use std::hash::{BuildHasher, Hasher};
        let mut st = b.hasher.build_hasher();
        st.write_u8(ty as u8);
        st.write(text);
        st.finish()
    };
    let c = &mut b.c;
    let found = b.vals.find(h, |&i| {
        let v = c.vals[i as usize];
        v.ty == ty && &c.val_text[v.off as usize..(v.off + v.len) as usize] == text
    });
    match found {
        Some(&i) => i,
        None => {
            let n = c.push_val(ty, text);
            let (vals, hasher) = (&c.vals, &b.hasher);
            let vt = &c.val_text;
            b.vals.insert_unique(h, n, |&i| {
                use std::hash::{BuildHasher, Hasher};
                let v = vals[i as usize];
                let mut st = hasher.build_hasher();
                st.write_u8(v.ty as u8);
                st.write(&vt[v.off as usize..(v.off + v.len) as usize]);
                st.finish()
            });
            n
        }
    }
}

/// The read-time geometry options (`-aw`, `-pw`, `-pW`,
/// `--clip-bounding-box`, `-pn`, `-aP`) for the projected geometry in
/// `b.scratch`, in the order of `serial.cpp:serialize_feature` (also with a
/// `geometry_scale`). The result is in `b.clip_out` (world coordinates);
/// returns its bbox, or `None` if the feature has no geometry.
#[inline(never)]
fn read_geom_options(b: &mut Builder, geom_type: GeomType, seq: u64) -> Option<[i64; 4]> {
    let rg = b.rg;
    let gs = b.geometry_scale;
    let is_point = |d: &Draw| d.op == vtmake_core::VT_MOVETO || d.op == vtmake_core::VT_LINETO;
    // `SHIFT_RIGHT`: `round(x / 2^gs)`. (`-aL` rounds with `x * 2^-gs`: the same.)
    let shift_right = |v: i64| if gs > 0 { (v as f64 / (1u64 << gs) as f64).round() as i64 } else { v };
    let mut raw = crate::meta::RAW_BBOX_EMPTY;
    if rg.changes_bounds() {
        for d in b.scratch.iter().filter(|d| is_point(d)) {
            crate::meta::add_raw_vertex(&mut raw, d.x, d.y);
        }
        if rg.wraparound {
            wraparound(&mut b.scratch);
        }
    }
    // `scale_geometry`: the bbox in world coordinates, then the scale.
    let mut bbox = vtmake_geom::geometry_bbox(&b.scratch);
    if gs > 0 {
        for d in b.scratch.iter_mut().filter(|d| is_point(d)) {
            d.x = shift_right(d.x);
            d.y = shift_right(d.y);
        }
    }
    let mut cur = std::mem::take(&mut b.clip_out);
    cur.clear();
    if geom_type == GeomType::Polygon {
        vtmake_geom::fix_polygon_opts(&b.scratch, &mut cur, rg.use_winding, rg.reverse_winding);
    } else {
        cur.extend_from_slice(&b.scratch);
    }
    if !rg.clip.is_empty() {
        let mut out = Vec::new();
        for c in &rg.clip {
            let c = [shift_right(c[0]), shift_right(c[1]), shift_right(c[2]), shift_right(c[3])];
            match geom_type {
                GeomType::Polygon => {
                    out.clear();
                    vtmake_geom::simple_clip_poly_box(&cur, c[0], c[1], c[2], c[3], &mut out, &mut b.clip_scratch);
                    std::mem::swap(&mut cur, &mut out);
                }
                GeomType::Line => {
                    out.clear();
                    vtmake_geom::clip_lines_box(&cur, c[0], c[1], c[2], c[3], &mut out);
                    std::mem::swap(&mut cur, &mut out);
                }
                GeomType::Point => vtmake_geom::clip_point_box(&mut cur, c[0], c[1], c[2], c[3]),
            }
            vtmake_geom::remove_noop(&mut cur, geom_type, 0);
            // The bbox of the clipped geometry (`SHIFT_LEFT` of each point).
            let sb = vtmake_geom::geometry_bbox(&cur);
            bbox = [sb[0] << gs, sb[1] << gs, sb[2] << gs, sb[3] << gs];
        }
        if cur.is_empty() {
            b.clip_out = cur;
            b.c.clipped_away.push((seq, raw));
            return None;
        }
    }
    if cur.is_empty() {
        b.clip_out = cur;
        return None;
    }
    // `-pn` (it sets `geometry_scale` 0).
    if rg.shared_nodes {
        vtmake_geom::remove_noop(&mut cur, geom_type, 0);
        // remove_noop can empty a degenerate line or polygon (one repeated
        // point). Drop such a feature here, as an empty geometry (review F4).
        if cur.is_empty() {
            b.clip_out = cur;
            return None;
        }
        if geom_type != GeomType::Point {
            add_nodes(&mut b.c, &cur, geom_type, seq);
        }
    }
    if rg.changes_bounds() || rg.label_points {
        let label_point = if rg.label_points && geom_type == GeomType::Polygon {
            vtmake_geom::label::polygon_to_anchor(&cur).map_or(0, |(x, y)| {
                vtmake_geom::encode_quadkey(((x << gs) & 0xFFFF_FFFF) as u32, ((y << gs) & 0xFFFF_FFFF) as u32)
            })
        } else {
            0
        };
        b.c.extra.push(crate::store::FeatExtra { label_point, raw });
    }
    if gs > 0 {
        for d in cur.iter_mut().filter(|d| is_point(d)) {
            d.x <<= gs;
            d.y <<= gs;
        }
    }
    b.clip_out = cur;
    Some(bbox)
}

/// `-pn` (`serial.cpp:serialize_feature`): the vertices and the nodes of a
/// line or polygon (after `remove_noop`) for the global shared nodes.
fn add_nodes(c: &mut Chunk, g: &[Draw], t: GeomType, seq: u64) {
    let p = |d: &Draw| (d.x, d.y);
    let lt = |a: &Draw, b: &Draw| a.y < b.y || (a.y == b.y && a.x < b.x);
    let vertex = |one: &Draw, mid: &Draw, two: &Draw| {
        if lt(one, two) { [p(mid), p(one), p(two)] } else { [p(mid), p(two), p(one)] }
    };
    let key = |d: &Draw| vtmake_geom::encode_vertex(d.x as u32, d.y as u32);
    let mut i = 0;
    while i < g.len() {
        if g[i].op != vtmake_core::VT_MOVETO {
            i += 1;
            continue;
        }
        let j = vtmake_geom::polygon::part_end(g, i);
        let n = j - i;
        if t == GeomType::Polygon && n >= 4 {
            for k in i..j - 1 {
                let at = |o: usize| &g[(k - i + o) % (n - 1) + i];
                c.node_vertices.push((seq, vertex(at(0), at(1), at(2))));
            }
        } else if t == GeomType::Line && n >= 2 {
            for k in i..j.saturating_sub(2) {
                c.node_vertices.push((seq, vertex(&g[k], &g[k + 1], &g[k + 2])));
            }
        }
        c.node_points.push((seq, key(&g[i])));
        if t == GeomType::Line && n >= 2 {
            c.node_points.push((seq, key(&g[j - 1])));
        } else if t == GeomType::Polygon && n >= 4 {
            // The point furthest from the start, then the point furthest
            // from the line between those two.
            let mut far = 0.0f64;
            let mut which = i;
            for k in i + 1..j - 1 {
                let xd = (g[k].x - g[i].x) as f64;
                let yd = (g[k].y - g[i].y) as f64;
                let d = xd * xd + yd * yd;
                if d > far || (d == far && lt(&g[k], &g[which])) {
                    far = d;
                    which = k;
                }
            }
            c.node_points.push((seq, key(&g[which])));
            far = 0.0;
            let mut which2 = i;
            for k in i + 1..j - 1 {
                let d = vtmake_geom::distance_from_line(g[k].x, g[k].y, g[i].x, g[i].y, g[which].x, g[which].y);
                if d > far || (d == far && lt(&g[k], &g[which2])) {
                    far = d;
                    which2 = k;
                }
            }
            c.node_points.push((seq, key(&g[which2])));
        }
        i = j;
    }
}

/// `main.cpp`: the global shared nodes of `-pn`, sorted. A vertex mid point
/// is a node if it has two different pairs of neighbours (the scan of the
/// sorted vertices, which starts with three (0, 0) points); the points of
/// `add_scaled_node` are nodes too.
pub fn shared_nodes(chunks: &mut [Chunk]) -> Vec<u64> {
    let mut v: Vec<[(i64, i64); 3]> = chunks.iter_mut().flat_map(|c| std::mem::take(&mut c.node_vertices)).map(|(_, v)| v).collect();
    // `vertexcmp`: mid, then the ends, each by (y, x).
    let k = |a: &[(i64, i64); 3]| [(a[0].1, a[0].0), (a[1].1, a[1].0), (a[2].1, a[2].0)];
    v.par_sort_unstable_by_key(k);
    let mut nodes: Vec<u64> = chunks.iter_mut().flat_map(|c| std::mem::take(&mut c.node_points)).map(|(_, n)| n).collect();
    let mut prev = [(0i64, 0i64); 3];
    for w in &v {
        if w[0] == prev[0] && (w[1] != prev[1] || w[2] != prev[2]) {
            nodes.push(vtmake_geom::encode_vertex(w[0].0 as u32, w[0].1 as u32));
        }
        prev = *w;
    }
    nodes.par_sort_unstable();
    nodes.dedup();
    nodes
}

/// `-aw` (`serial.cpp:scale_geometry`): in each line or ring, a jump of
/// more than 180 degrees (but not exactly 360) moves the rest of the part by
/// 360 degrees. tippecanoe does not test the first segment of the first
/// part. In place.
fn wraparound(g: &mut [Draw]) {
    let mut offset = 0i64;
    let mut prev = 0i64;
    let mut has_prev = false;
    for d in g.iter_mut() {
        if d.op == vtmake_core::VT_LINETO {
            let mut x = d.x + offset;
            if has_prev {
                if x - prev > 1 << 31 && x - prev != 1 << 32 {
                    offset -= 1 << 32;
                    x -= 1 << 32;
                } else if prev - x > 1 << 31 && prev - x != 1 << 32 {
                    offset += 1 << 32;
                    x += 1 << 32;
                }
            }
            has_prev = true;
            prev = x;
            d.x = x;
        } else if d.op == vtmake_core::VT_MOVETO {
            offset = 0;
            prev = d.x;
        }
    }
}

unsafe extern "C" {
    fn mi_collect(force: bool);
    #[cfg(target_os = "linux")]
    fn posix_fadvise(fd: i32, offset: i64, len: i64, advice: i32) -> i32;
    #[cfg(target_os = "linux")]
    fn mincore(addr: *mut std::ffi::c_void, len: usize, vec: *mut u8) -> i32;
    #[cfg(target_os = "linux")]
    fn geteuid() -> u32;
    #[cfg(target_os = "linux")]
    fn getpagesize() -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ro(parallel: bool, cpus: usize) -> ReadOpts {
        ReadOpts {
            projection: Projection::Epsg4326,
            geometry_scale: 0,
            settings: vtmake_parse::geojson::Settings { projection_alias: Projection::Epsg4326.alias(), quiet: true, ..Default::default() },
            read_parallel: parallel,
            felt_cpus: cpus,
            empty_csv_null: false,
            quiet: true,
            pgcopy: None,
        }
    }

    /// (seq, x) of all features in order.
    fn read(text: &str, parallel: bool, cpus: usize) -> Vec<(u64, i64)> {
        let d = Data::Mem(text.as_bytes());
        let len = text.len() as u64;
        let plan = if parallel { Plan { streams: segments(&d, 0, len, cpus, b'\n'), pull_only: false } } else { Plan { streams: vec![(0, len)], pull_only: false } };
        let chunks = read_geojson(&d, &plan, 0, &ro(parallel, cpus), "t", 1);
        let mut v: Vec<(u64, i64)> = chunks.iter().flat_map(|c| c.seqs.iter().zip(&c.metas).map(|(s, m)| (*s, m.bbox[0]))).collect();
        v.sort();
        // The same features with small pieces and regions (cuts in lines).
        for piece in [1, 7, 64] {
            let chunks = read_streams(&d, &plan, 0, &ro(parallel, cpus), "t", 1, piece);
            let mut w: Vec<(u64, i64)> = chunks.iter().flat_map(|c| c.seqs.iter().zip(&c.metas).map(|(s, m)| (*s, m.bbox[0]))).collect();
            w.sort();
            assert_eq!(w.iter().map(|x| x.1).collect::<Vec<_>>(), v.iter().map(|x| x.1).collect::<Vec<_>>(), "{text} {piece}");
        }
        v
    }

    const F1: &str = r#"{"type":"Feature","properties":{"a":"x","b":7},"geometry":{"type":"Point","coordinates":[1,2]},"id":42}"#;
    const F2: &str = r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[3,4]}}"#;

    #[test]
    fn ndjson_and_feature_collections_agree() {
        let nd = format!("\n{F1}\r\n\n{F2}");
        let a = read(&nd, false, 1);
        assert_eq!(a.len(), 2);
        let fc1 = format!("{{\"type\":\"FeatureCollection\",\"features\":[\n{F1},\n{F2}\n]}}\n");
        let fc2 = format!("{{\"type\":\"FeatureCollection\",\n \"features\": [ {F1}, {F2} ] }}");
        let pretty = format!("{{\"type\":\"FeatureCollection\",\"features\":[\n{}\n,\n{}\n]}}", F1.replace(',', ",\n  "), F2.replace(':', ":\n"));
        for fc in [fc1, fc2, pretty] {
            let b = read(&fc, false, 1);
            assert_eq!(b.iter().map(|x| x.1).collect::<Vec<_>>(), a.iter().map(|x| x.1).collect::<Vec<_>>(), "{fc}");
        }
    }

    #[test]
    fn a_syntax_error_stops_the_stream() {
        let t = format!("{F1}\n{{oops\n{F2}\n");
        assert_eq!(read(&t, false, 1).len(), 1);
        // With --read-parallel and 2 segments, the second segment (from the
        // fifth line) has its own parser.
        let t = format!("{F1}\n{{oops\n{F2}\n{F2}\n{F2}\n{F2}\n");
        assert_eq!(read(&t, true, 2).len(), 3);
    }

    #[test]
    fn a_line_inside_an_object_is_not_a_feature_of_its_own() {
        // The second feature is the value of "x", in an unknown object: jsonpull
        // still reads it as a feature (nested), after the first one.
        let t = format!("{{\"x\":\n{F2}\n,\"y\":[\n{F1}\n]}}\n");
        assert_eq!(read(&t, false, 1).len(), 2);
        // In properties, it is not a feature.
        let t = format!("{{\"type\":\"Feature\",\"geometry\":{{\"type\":\"Point\",\"coordinates\":[0,0]}},\"properties\":{{\"p\":[\n{F1}\n]}}}}\n");
        assert_eq!(read(&t, false, 1).len(), 1);
    }
}
