//! Streamed input (review F10): standard input from a pipe, `.gz` files, CSV
//! and FlatGeobuf. tippecanoe streams these inputs; vtmake reads them in
//! bounded parts and gets the same result as when it reads all at once.
//!
//! GeoJSON. A reader thread reads the text (and decompresses it) into
//! windows. The main thread reads each window in the two phases of
//! `crate::read`, while the reader thread fills the next window.
//!
//! - One stream (no `--read-parallel`): a window ends after its last `\n`.
//!   The jsonpull port keeps its state from one window to the next. A line
//!   that is longer than `MAX_LINE` is cut (the parts are dirty).
//! - `--read-parallel` or RS input: the windows are groups of tippecanoe's
//!   chunks (2000-byte reads; a read that ends with the separator after
//!   more than 10 MB ends a chunk). Each chunk is split into CPUS segments.
//!   A chunk that is larger than `SPILL` goes into a temporary file (in
//!   `-t`, as tippecanoe does for all chunks).
//!
//! CSV and FlatGeobuf. The input comes in blocks. The worker threads parse
//! the pieces of a block in parallel; then the main thread writes the
//! messages and the first error in input order (as tippecanoe's serial
//! parser); then each worker thread adds the features of its pieces to its
//! own chunk. The size of a block is bounded, so is the memory for the
//! parsed features.

use super::{Chunk, Data, Piece, Plan, ReadOpts, Run, phase1, plan_pieces, read_streams, remove_dead, segments_with_bases, Fixer, Builder, SEQ_SHIFT, EXIT_OPEN, EXIT_READ, MAX_LINE};
use rayon::prelude::*;
use std::io::Read;
use vtmake_parse::ParsedFeature;

// The tests use small sizes, so they read many windows, blocks and chunks.

/// The largest number of FlatGeobuf features in a piece.
const BATCH: usize = 1 << 16;

/// tippecanoe's stream reading with `--read-parallel`: 2000-byte reads, and
/// a chunk ends after more than `PARSE_MIN` bytes (`main.cpp:read_input`).
const READ_BUF: u64 = if cfg!(test) { 8 } else { 2000 };
const PARSE_MIN: u64 = if cfg!(test) { 20 } else { 10_000_000 };

/// A tippecanoe chunk that is larger than this goes into a temporary file.
const SPILL: u64 = if cfg!(test) { 300 } else { 512 * 1024 * 1024 };

/// One read from the input.
const BLOCK: usize = if cfg!(test) { 7 } else { 4 * 1024 * 1024 };

/// The size of a window of GeoJSON text.
fn window_size(threads: usize) -> usize {
    if cfg!(test) {
        return 16;
    }
    (threads.max(1) * (8 << 20)).clamp(64 << 20, 256 << 20)
}

/// The size of the phase 1 pieces in a window.
fn piece_size(threads: usize) -> u64 {
    if cfg!(test) {
        return 5;
    }
    (window_size(threads) / threads.max(1)).max(1 << 20) as u64
}

/// The bytes of a source.
struct Input<'a> {
    r: Box<dyn Read + Send + 'a>,
    /// A read error stops with this message and exit 114 (gzip); else it
    /// ends the input (as `fread`).
    gz: bool,
    /// The name for the read error of a file (CSV), else `None`.
    open_error: Option<&'a str>,
}

impl<'a> Input<'a> {
    /// `gz`: decompress if the input starts with the gzip magic bytes
    /// (zlib's `gzread` reads other data as it is).
    fn new(file: &'a mut std::fs::File, gz: bool, open_error: Option<&'a str>) -> Self {
        let mut head = Vec::new();
        let mut inp = Input { r: Box::new(std::io::empty()), gz: false, open_error };
        if gz {
            let f: &mut std::fs::File = file;
            while head.len() < 2 {
                let mut b = [0u8; 2];
                match f.read(&mut b[..2 - head.len()]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&b[..n]),
                }
            }
            let rest = std::io::Cursor::new(head.clone()).chain(f);
            if head == [0x1f, 0x8b] {
                inp.r = Box::new(flate2::read::MultiGzDecoder::new(std::io::BufReader::with_capacity(1 << 20, rest)));
                inp.gz = true;
            } else {
                inp.r = Box::new(rest);
            }
            return inp;
        }
        inp.r = Box::new(file);
        inp
    }

    /// Read `n` more bytes into `buf`; `Ok(true)` at the end of the input.
    fn fill(&mut self, buf: &mut Vec<u8>, n: usize) -> Result<bool, ()> {
        buf.reserve(n);
        match (&mut self.r).take(n as u64).read_to_end(buf) {
            Ok(k) => Ok(k < n),
            Err(_) if self.gz => Err(()),
            Err(e) => {
                if let Some(name) = self.open_error {
                    eprintln!("{name}: {}", super::os_error(&e));
                    std::process::exit(EXIT_OPEN)
                }
                Ok(true)
            }
        }
    }
}

/// A window of GeoJSON text.
struct Window {
    /// The source position of the first byte.
    start: u64,
    mem: Vec<u8>,
    /// A large tippecanoe chunk: the temporary file and its length.
    file: Option<(std::fs::File, u64)>,
    /// `--read-parallel` or RS: the streams in the window (source positions,
    /// with tippecanoe's start of the segment).
    streams: Vec<(u64, u64, u64)>,
    /// The separator of the streams (0: one stream).
    sep: u8,
    /// One stream: the window starts (ends) in a line.
    head_cut: bool,
    tail_cut: bool,
    eof: bool,
}

enum Msg {
    Win(Window),
    /// A gzip error.
    Error,
}

/// The separator of the streams: 0x1E if the input starts with it, `\n`
/// with `--read-parallel`, else 0 (one stream).
fn separator(first: Option<&u8>, read_parallel: bool) -> u8 {
    if first == Some(&0x1E) {
        0x1E
    } else if read_parallel {
        b'\n'
    } else {
        0
    }
}

/// The reader thread: windows of the input in order.
fn produce(mut inp: Input<'_>, ro: &ReadOpts, threads: usize, tx: std::sync::mpsc::SyncSender<Msg>) {
    let w = window_size(threads);
    let mut buf = Vec::with_capacity(w + BLOCK);
    let Ok(eof) = inp.fill(&mut buf, BLOCK) else {
        let _ = tx.send(Msg::Error);
        return;
    };
    let sep = separator(buf.first(), ro.read_parallel);
    let r = if sep == 0 { produce_serial(&mut inp, buf, eof, w, &tx) } else { produce_chunks(&mut inp, buf, eof, w, sep, ro, &tx) };
    if r.is_err() {
        let _ = tx.send(Msg::Error);
    }
}

fn window(start: u64, mem: Vec<u8>, sep: u8) -> Window {
    Window { start, mem, file: None, streams: Vec::new(), sep, head_cut: false, tail_cut: false, eof: false }
}

/// One stream: windows that end after a `\n`.
fn produce_serial(inp: &mut Input<'_>, mut buf: Vec<u8>, mut eof: bool, w: usize, tx: &std::sync::mpsc::SyncSender<Msg>) -> Result<(), ()> {
    let sep = 0;
    let mut start = 0u64;
    let mut head_cut = false;
    // `buf[..scanned]` has no `\n`.
    let mut scanned = 0;
    loop {
        if !eof {
            let need = if buf.len() < w { w - buf.len() } else { BLOCK };
            eof = inp.fill(&mut buf, need)?;
        }
        if eof {
            let _ = tx.send(Msg::Win(Window { head_cut, eof: true, ..window(start, buf, sep) }));
            return Ok(());
        }
        let (cut, tail_cut) = match memchr::memrchr(b'\n', &buf[scanned..]) {
            Some(i) => (scanned + i + 1, false),
            None if (buf.len() as u64) >= w as u64 + MAX_LINE => (buf.len(), true),
            None => {
                scanned = buf.len();
                continue;
            }
        };
        let mut next = Vec::with_capacity(w + BLOCK);
        next.extend_from_slice(&buf[cut..]);
        buf.truncate(cut);
        if tx.send(Msg::Win(Window { head_cut, tail_cut, ..window(start, std::mem::replace(&mut buf, next), sep) })).is_err() {
            return Ok(());
        }
        start += cut as u64;
        head_cut = tail_cut;
        scanned = 0;
    }
}

/// The first position at which a chunk that starts at `chunk_start` can end.
fn first_check(chunk_start: u64) -> u64 {
    ((chunk_start + PARSE_MIN) / READ_BUF + 1) * READ_BUF
}

/// `--read-parallel` or RS: windows of complete tippecanoe chunks.
fn produce_chunks(inp: &mut Input<'_>, mut buf: Vec<u8>, mut eof: bool, w: usize, sep: u8, ro: &ReadOpts, tx: &std::sync::mpsc::SyncSender<Msg>) -> Result<(), ()> {
    let cpus = ro.felt_cpus;
    // The source position of `buf[0]`, and the start of the chunk that is
    // not complete.
    let mut start = 0u64;
    let mut chunk_start = 0u64;
    let mut check = first_check(0);
    let mut streams: Vec<(u64, u64, u64)> = Vec::new();
    let add = |d: &Data<'_>, at: u64, s: u64, e: u64, streams: &mut Vec<(u64, u64, u64)>| {
        streams.extend(segments_with_bases(d, s - at, e - at, cpus, sep).into_iter().map(|(a, b, c)| (a + at, b + at, c + at)));
    };
    loop {
        let end = start + buf.len() as u64;
        while check <= end {
            if buf[(check - 1 - start) as usize] == sep {
                add(&Data::Mem(&buf), start, chunk_start, check, &mut streams);
                chunk_start = check;
                check = first_check(check);
            } else {
                check += READ_BUF;
            }
        }
        if eof {
            if end > chunk_start {
                add(&Data::Mem(&buf), start, chunk_start, end, &mut streams);
            }
            let _ = tx.send(Msg::Win(Window { streams, eof: true, ..window(start, buf, sep) }));
            return Ok(());
        }
        let done = (chunk_start - start) as usize;
        if done >= w || (end - chunk_start > SPILL && done > 0) {
            // A window of the complete chunks.
            let mut next = Vec::with_capacity(w + BLOCK);
            next.extend_from_slice(&buf[done..]);
            buf.truncate(done);
            if tx.send(Msg::Win(Window { streams: std::mem::take(&mut streams), ..window(start, std::mem::replace(&mut buf, next), sep) })).is_err() {
                return Ok(());
            }
            start = chunk_start;
        }
        if end - chunk_start > SPILL {
            // `buf` is the start of a large chunk: put the chunk in a file.
            let (file, len, rest, at_eof) = spill(inp, buf, start, &mut check, sep)?;
            let d = Data::File { file: &file, off: 0 };
            let mut s = Vec::new();
            add(&d, 0, 0, len, &mut s);
            let s = s.into_iter().map(|(a, b, c)| (a + start, b + start, c + start)).collect();
            if tx.send(Msg::Win(Window { file: Some((file, len)), streams: s, eof: at_eof && rest.is_empty(), ..window(start, Vec::new(), sep) })).is_err() {
                return Ok(());
            }
            start += len;
            chunk_start = start;
            if at_eof && rest.is_empty() {
                return Ok(());
            }
            buf = Vec::with_capacity(w + BLOCK);
            buf.extend_from_slice(&rest);
            eof = at_eof;
            continue;
        }
        eof = inp.fill(&mut buf, BLOCK)?;
    }
}

/// Write the chunk that starts at `start` (its first bytes are `buf`) to a
/// temporary file until it ends (or the input ends). Returns the file, the
/// length of the chunk, the bytes after it, and if the input is at its end.
fn spill(inp: &mut Input<'_>, mut buf: Vec<u8>, start: u64, check: &mut u64, sep: u8) -> Result<(std::fs::File, u64, Vec<u8>, bool), ()> {
    use std::io::Write;
    let (mut file, name) = temp_file();
    let write = |file: &mut std::fs::File, b: &[u8]| {
        if let Err(e) = file.write_all(b) {
            eprintln!("{name}: Write to temporary file failed: {}", super::os_error(&e));
            std::process::exit(120)
        }
    };
    let mut len = 0u64;
    loop {
        let end = start + len + buf.len() as u64;
        while *check <= end {
            if buf[(*check - 1 - start - len) as usize] == sep {
                let k = (*check - start - len) as usize;
                write(&mut file, &buf[..k]);
                len += k as u64;
                *check = first_check(start + len);
                return Ok((file, len, buf[k..].to_vec(), false));
            }
            *check += READ_BUF;
        }
        write(&mut file, &buf);
        len += buf.len() as u64;
        buf.clear();
        if inp.fill(&mut buf, BLOCK)? && buf.is_empty() {
            return Ok((file, len, Vec::new(), true));
        }
    }
}

/// A new temporary file in `-t` (`read.XXXXXXXX`, removed at once).
fn temp_file() -> (std::fs::File, String) {
    let dir = super::READ_GEOM.get().map_or("/tmp", |r| if r.tmpdir.is_empty() { "/tmp" } else { r.tmpdir.as_str() });
    let mut n = std::process::id() as u64 ^ (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64));
    loop {
        let name = format!("{dir}/read.{:08x}", n & 0xFFFF_FFFF);
        match std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&name) {
            Ok(f) => {
                let _ = std::fs::remove_file(&name);
                return (f, name);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n = n.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407),
            Err(e) => {
                eprintln!("{name}: {}", super::os_error(&e));
                std::process::exit(EXIT_OPEN)
            }
        }
    }
}

/// The result of [`read_geojson`]: the chunks, the separator of the streams
/// (0: one stream), the streams (start, end, tippecanoe's start of the
/// segment), and the length of the text.
pub(super) type Streamed = (Vec<Chunk>, u8, Vec<(u64, u64, u64)>, u64);

/// The stream path of a GeoJSON source.
pub(super) fn read_geojson(file: &mut std::fs::File, gz: bool, fname: &str, ro: &ReadOpts, base: u64, threads: usize, argv0: &str) -> Streamed {
    let piece = piece_size(threads);
    let mut chunks = Vec::new();
    let mut streams = Vec::new();
    let mut sep = 0;
    let mut len = 0;
    let mut fx = Fixer::new(ro, fname, base);
    std::thread::scope(|sc| {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let inp = Input::new(file, gz, None);
        sc.spawn(move || produce(inp, ro, threads, tx));
        for msg in rx {
            let Msg::Win(w) = msg else {
                eprintln!("{argv0}: Error reading compressed data");
                std::process::exit(EXIT_READ)
            };
            sep = w.sep;
            let wlen = w.file.as_ref().map_or(w.mem.len() as u64, |f| f.1);
            len = w.start + wlen;
            let wbase = base + w.start;
            if sep != 0 {
                // Complete streams.
                let d = match &w.file {
                    Some((f, _)) => Data::File { file: f, off: 0 },
                    None => Data::Mem(&w.mem),
                };
                let plan = Plan { streams: w.streams.iter().map(|s| (s.0 - w.start, s.1 - w.start)).collect(), pull_only: sep == 0x1E };
                chunks.extend(read_streams(&d, &plan, wbase, ro, fname, threads, piece));
                streams.extend(w.streams);
                continue;
            }
            serial_window(&mut fx, &w, wbase, ro, fname, threads, piece, &mut chunks);
        }
    });
    if sep == 0 {
        fx.end_stream();
        let Fixer { out, builder, .. } = fx;
        chunks.extend(out);
        chunks.push(builder.finish());
        chunks.retain(|c| !c.metas.is_empty());
        streams = vec![(0, len, 0)];
    }
    super::collect_free();
    (chunks, sep, streams, len)
}

/// Read a window of the one stream (phase 1 and phase 2); the jsonpull state
/// goes on in the next window.
#[allow(clippy::too_many_arguments)]
fn serial_window(fx: &mut Fixer<'_>, w: &Window, wbase: u64, ro: &ReadOpts, fname: &str, threads: usize, piece: u64, chunks: &mut Vec<Chunk>) {
    let d = Data::Mem(&w.mem);
    let len = w.mem.len() as u64;
    if fx.stopped {
        // A syntax error stopped the stream: tippecanoe reads no more of it.
        return;
    }
    let mut pieces: Vec<Piece> = Vec::new();
    plan_pieces(&d, 0, len, piece, &mut pieces);
    if let Some(p) = pieces.first_mut() {
        p.head_cut |= w.head_cut;
    }
    if let Some(p) = pieces.last_mut() {
        p.tail_cut |= w.tail_cut;
    }
    let results = phase1(&d, &pieces, wbase, ro, fname, &[], threads);
    let mut runs: Vec<Run> = Vec::new();
    let mut new = Vec::with_capacity(results.len());
    for (c, rs) in results {
        runs.extend(rs);
        new.push(c);
    }
    fx.base = wbase;
    fx.runs(&d, &runs, if w.eof { len } else { u64::MAX });
    let mut dead = std::mem::take(&mut fx.dead);
    if !dead.is_empty() {
        dead.sort_unstable();
        new.par_iter_mut().for_each(|c| remove_dead(c, &dead, wbase));
    }
    chunks.extend(new.into_iter().filter(|c| !c.metas.is_empty()));
    chunks.append(&mut fx.out);
}

/// The size of a block of CSV or FlatGeobuf input, and of its pieces.
fn block_size(threads: usize) -> (usize, usize) {
    if cfg!(test) {
        return (40, 9);
    }
    let block = (threads.max(1) << 20).clamp(4 << 20, 32 << 20);
    (block, (block / threads.max(1)).max(256 << 10))
}

/// The features of one piece of a block, the first row without a geometry
/// (CSV), and the first error.
#[derive(Default)]
struct Parsed {
    feats: Vec<ParsedFeature>,
    null: Option<usize>,
    err: Option<vtmake_parse::ParseError>,
}

/// `f(j, w)` for `j` in `0..n`, in parallel: worker thread `w = j % threads`
/// runs `f(j, w)`. The parse and the build of a piece run on the same
/// thread, so the thread that allocates the parsed features also frees them.
fn pinned<R: Send>(n: usize, f: impl Fn(usize, usize) -> R + Sync) -> Vec<R> {
    let parts: Vec<Vec<(usize, R)>> = rayon::broadcast(|ctx| (ctx.index()..n).step_by(ctx.num_threads()).map(|j| (j, f(j, ctx.index()))).collect());
    let mut out: Vec<Option<R>> = (0..n).map(|_| None).collect();
    for (j, r) in parts.into_iter().flatten() {
        out[j] = Some(r);
    }
    out.into_iter().map(|r| r.expect("each piece has a result")).collect()
}

/// The chunk builders of the worker threads. Each thread adds the features
/// of its pieces to its own chunk (the order of the features in a chunk
/// does not matter: `seq` gives the order), so the chunks are large.
struct Builders<'a> {
    ro: &'a ReadOpts,
    base: u64,
    open: Vec<std::sync::Mutex<Option<Builder>>>,
    done: std::sync::Mutex<Vec<Chunk>>,
}

impl<'a> Builders<'a> {
    fn new(ro: &'a ReadOpts, base: u64) -> Self {
        Builders { ro, base, open: (0..rayon::current_num_threads()).map(|_| std::sync::Mutex::new(None)).collect(), done: Default::default() }
    }

    /// Add the features of each piece (on the thread that parsed it, see
    /// [`pinned`]); `first`: the number of the first feature.
    fn add(&self, parsed: Vec<Parsed>, first: u64) {
        let mut offs = Vec::with_capacity(parsed.len());
        let mut n = first;
        for p in &parsed {
            offs.push(n);
            n += p.feats.len() as u64;
        }
        let slots: Vec<std::sync::Mutex<Option<Parsed>>> = parsed.into_iter().map(|p| std::sync::Mutex::new(Some(p))).collect();
        pinned(slots.len(), |j, w| {
            let p = slots[j].lock().unwrap().take().unwrap();
            let mut open = self.open[w % self.open.len()].lock().unwrap();
            let b = open.get_or_insert_with(|| Builder::new(self.ro.projection, self.ro.geometry_scale));
            for (i, f) in p.feats.iter().enumerate() {
                b.add_parsed(f, (self.base + offs[j] + i as u64) << SEQ_SHIFT);
                if b.full() {
                    let c = std::mem::replace(b, Builder::new(self.ro.projection, self.ro.geometry_scale)).finish();
                    self.done.lock().unwrap().push(c);
                }
            }
        });
    }

    fn finish(self) -> Vec<Chunk> {
        let mut out = self.done.into_inner().unwrap();
        out.extend(self.open.into_iter().filter_map(|b| b.into_inner().unwrap()).map(Builder::finish));
        out.retain(|c| !c.metas.is_empty());
        out
    }
}

/// `[s, e)` pieces of `data` of about `size` bytes that end after a `\n`.
fn line_pieces(data: &[u8], size: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut p = 0;
    while p < data.len() {
        let q = if data.len() - p > size { memchr::memchr(b'\n', &data[p + size..]).map_or(data.len(), |i| p + size + i + 1) } else { data.len() };
        out.push((p, q));
        p = q;
    }
    out
}

/// A CSV source: `(chunks, features)`. `name`: the file name in the
/// messages ("" for standard input).
pub(super) fn read_csv(file: &mut std::fs::File, name: &str, ro: &ReadOpts, base: u64, threads: usize) -> (Vec<Chunk>, u64) {
    // A read error of a file stops (exit 111); of standard input, it ends
    // the input.
    let mut inp = Input::new(file, false, (!name.is_empty()).then_some(name));
    let mut fill = |buf: &mut Vec<u8>, n: usize| inp.fill(buf, n).unwrap_or(true);
    let mut reader = vtmake_parse::csv::CsvReader::new(name, ro.empty_csv_null);
    let (block, piece) = block_size(threads);
    let mut buf: Vec<u8> = Vec::new();
    let mut eof = false;
    loop {
        if let Some(n) = reader.header(&buf, eof) {
            buf.drain(..n);
            break;
        }
        eof = fill(&mut buf, BLOCK);
    }
    let builders = Builders::new(ro, base);
    let mut n = 0u64;
    loop {
        while !eof && buf.len() < block {
            let n = block - buf.len();
            eof = fill(&mut buf, n);
        }
        // The complete lines.
        let end = if eof { buf.len() } else { memchr::memrchr(b'\n', &buf).map_or(0, |i| i + 1) };
        if end == 0 && !eof {
            eof = fill(&mut buf, BLOCK);
            continue;
        }
        let data = &buf[..end];
        let pieces = line_pieces(data, piece);
        let counts: Vec<usize> = pieces.par_iter().map(|&(s, e)| vtmake_parse::csv::lines(&data[s..e], eof).count()).collect();
        let mut starts = Vec::with_capacity(counts.len());
        for c in &counts {
            starts.push(reader.seq);
            reader.seq += c;
        }
        let parsed: Vec<Parsed> = pinned(pieces.len(), |j, _| {
                let ((s, e), first) = (pieces[j], starts[j]);
                let mut p = Parsed::default();
                for (i, line) in vtmake_parse::csv::lines(&data[s..e], eof).enumerate() {
                    let seq = first + i + 1;
                    match reader.row(line, seq) {
                        Ok(Some(f)) => p.feats.push(f),
                        Ok(None) => {
                            p.null.get_or_insert(seq);
                        }
                        Err(e) => {
                            p.err = Some(e);
                            break;
                        }
                    }
                }
                p
            });
        // The messages in the order of the lines.
        for p in &parsed {
            if let Some(seq) = p.null {
                reader.null_geometry(seq);
            }
            if let Some(e) = &p.err {
                e.exit()
            }
        }
        let count: u64 = parsed.iter().map(|p| p.feats.len() as u64).sum();
        builders.add(parsed, n);
        n += count;
        buf.drain(..end);
        if eof {
            return (builders.finish(), n);
        }
    }
}

/// A FlatGeobuf source of `size` bytes: `(chunks, features)`.
pub(super) fn read_fgb(file: &mut std::fs::File, size: u64, fname: &str, ro: &ReadOpts, base: u64, threads: usize) -> (Vec<Chunk>, u64) {
    use std::os::unix::fs::FileExt;
    use vtmake_parse::fgb::FgbReader;
    // Standard input from a file: from its position.
    let off = {
        use std::io::Seek;
        file.stream_position().unwrap_or(0)
    };
    let file: &std::fs::File = file;
    let mut flen = size.saturating_sub(off);
    // Read `[p, p + n)` (as `fread`, an error ends the input).
    let read = |p: u64, n: u64, buf: &mut Vec<u8>, flen: &mut u64| {
        let n = n.min(flen.saturating_sub(p));
        let k = buf.len();
        buf.resize(k + n as usize, 0);
        let mut got = 0;
        while got < n as usize {
            match file.read_at(&mut buf[k + got..], off + p + got as u64) {
                Ok(0) | Err(_) => {
                    *flen = p + got as u64;
                    break;
                }
                Ok(m) => got += m,
            }
        }
        buf.truncate(k + got);
    };
    let mut head = Vec::new();
    read(0, 12, &mut head, &mut flen);
    let hs = vtmake_parse::fgb::header_size(&head);
    head.clear();
    read(0, 12 + hs as u64, &mut head, &mut flen);
    let reader = FgbReader::new(&head, fname, ro.quiet);
    drop(head);
    let (block, piece) = block_size(threads);
    let mut buf: Vec<u8> = Vec::new();
    // The file position of `buf[0]`, and the number of the next feature.
    let mut at = reader.first;
    let mut index = 0u64;
    let builders = Builders::new(ro, base);
    while at < flen {
        let have = buf.len();
        if have < block {
            read(at + have as u64, (block - have) as u64, &mut buf, &mut flen);
        }
        let file_end = at + buf.len() as u64 >= flen;
        // The complete features in `buf`, in pieces.
        let mut pieces: Vec<Vec<(usize, usize)>> = Vec::new();
        let (mut p, mut bytes) = (0, usize::MAX);
        let mut truncated = false;
        while p < buf.len() {
            match FgbReader::feature_size(&buf[p..]) {
                Some(n) if p + n <= buf.len() => {
                    if bytes >= piece || pieces.last().is_some_and(|v| v.len() >= BATCH) {
                        pieces.push(Vec::new());
                        bytes = 0;
                    }
                    pieces.last_mut().unwrap().push((p, n));
                    bytes += n;
                    p += n;
                }
                _ => {
                    truncated = file_end;
                    break;
                }
            }
        }
        if pieces.is_empty() && !truncated {
            // A feature that is larger than the block.
            let need = FgbReader::feature_size(&buf).unwrap_or(4);
            let have = buf.len();
            read(at + have as u64, (need.max(have + 1) - have) as u64, &mut buf, &mut flen);
            continue;
        }
        let mut firsts = Vec::with_capacity(pieces.len());
        let mut k = index;
        for v in &pieces {
            firsts.push(k);
            k += v.len() as u64;
        }
        let parsed: Vec<Parsed> = pinned(pieces.len(), |j, _| {
                let (v, first) = (&pieces[j], firsts[j]);
                let mut out = Parsed { feats: Vec::with_capacity(v.len()), ..Default::default() };
                for (i, &(s, n)) in v.iter().enumerate() {
                    match reader.feature(&buf[s..s + n], first + i as u64) {
                        Ok(f) => out.feats.push(f),
                        Err(e) => {
                            out.err = Some(e);
                            break;
                        }
                    }
                }
                out
            });
        // The errors in the order of the features.
        for p in &parsed {
            if let Some(e) = &p.err {
                e.exit()
            }
        }
        if truncated {
            eprintln!("flatgeobuf feature buffer verification failed");
            std::process::exit(106)
        }
        builders.add(parsed, index);
        index = k;
        buf.drain(..p);
        at += p as u64;
    }
    (builders.finish(), index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Projection;

    fn ro(parallel: bool, cpus: usize) -> ReadOpts {
        ReadOpts {
            projection: Projection::Epsg4326,
            geometry_scale: 0,
            settings: vtmake_parse::geojson::Settings { projection_alias: Projection::Epsg4326.alias(), quiet: true, ..Default::default() },
            read_parallel: parallel,
            felt_cpus: cpus,
            empty_csv_null: false,
            quiet: true,
        }
    }

    /// The old reading of all the text: tippecanoe's chunks of a pipe.
    fn chunked_streams(mem: &[u8], cpus: usize, sep: u8) -> Vec<(u64, u64, u64)> {
        let d = Data::Mem(mem);
        let len = mem.len() as u64;
        let mut out = Vec::new();
        let mut chunk_start = 0;
        let mut p = 0;
        while p < len {
            p += READ_BUF.min(len - p);
            if mem[(p - 1) as usize] == sep && p - chunk_start > PARSE_MIN {
                out.extend(segments_with_bases(&d, chunk_start, p, cpus, sep));
                chunk_start = p;
            }
        }
        if p > chunk_start {
            out.extend(segments_with_bases(&d, chunk_start, p, cpus, sep));
        }
        out
    }

    fn file_with(text: &[u8], tag: &str) -> std::fs::File {
        use std::io::Write;
        let name = std::env::temp_dir().join(format!("vtmake-stream-test-{}-{tag}", std::process::id()));
        std::fs::File::create(&name).unwrap().write_all(text).unwrap();
        let f = std::fs::File::open(&name).unwrap();
        std::fs::remove_file(&name).unwrap();
        f
    }

    /// The x of all features in input order.
    fn xs(chunks: &[Chunk]) -> Vec<i64> {
        let mut v: Vec<(u64, i64)> = chunks.iter().flat_map(|c| c.seqs.iter().zip(&c.metas).map(|(s, m)| (*s, m.bbox[0]))).collect();
        v.sort();
        v.into_iter().map(|x| x.1).collect()
    }

    const F1: &str = r#"{"type":"Feature","properties":{"a":"x","b":7},"geometry":{"type":"Point","coordinates":[1,2]},"id":42}"#;
    const F2: &str = r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[3,4]}}"#;
    const F3: &str = r#"{"type":"Feature","properties":{"c":[1,2]},"geometry":{"type":"Point","coordinates":[5,6]}}"#;

    fn texts() -> Vec<String> {
        let nd: String = (0..40).map(|i| [F1, F2, F3][i % 3].to_string() + "\n").collect();
        let fc = format!("{{\"type\":\"FeatureCollection\",\"features\":[\n{}\n]}}\n", (0..20).map(|i| [F1, F2, F3][i % 3]).collect::<Vec<_>>().join(",\n"));
        let one_line = fc.replace('\n', "");
        let pretty = fc.replace(',', ",\n  ").replace(':', ":\n");
        let error = format!("{nd}{{oops\n{nd}");
        let rs: String = (0..30).map(|i| format!("\x1e{}\n", [F1, F2, F3][i % 3])).collect();
        vec![nd, fc, one_line, pretty, error, rs, String::new(), "\n \n".into()]
    }

    #[test]
    fn one_stream_in_windows_gives_the_same_features() {
        for t in texts() {
            if t.starts_with('\x1e') {
                continue;
            }
            let o = ro(false, 4);
            let d = Data::Mem(t.as_bytes());
            let plan = Plan { streams: vec![(0, t.len() as u64)], pull_only: false };
            let want = xs(&read_streams(&d, &plan, 0, &o, "t", 2, 1 << 20));
            let mut f = file_with(t.as_bytes(), "serial");
            let (chunks, sep, streams, len) = read_geojson(&mut f, false, "t", &o, 0, 2, "t");
            assert_eq!((sep, streams, len), (0, vec![(0, t.len() as u64, 0)], t.len() as u64));
            assert_eq!(xs(&chunks), want, "{t}");
        }
    }

    #[test]
    fn chunks_in_windows_give_the_same_streams_and_features() {
        for t in texts() {
            for cpus in [1, 2, 4] {
                let o = ro(true, cpus);
                let sep = if t.starts_with('\x1e') { 0x1E } else { b'\n' };
                let streams = chunked_streams(t.as_bytes(), cpus, sep);
                let d = Data::Mem(t.as_bytes());
                let plan = Plan { streams: streams.iter().map(|s| (s.0, s.1)).collect(), pull_only: sep == 0x1E };
                let want = xs(&read_streams(&d, &plan, 0, &o, "t", 2, 1 << 20));
                let mut f = file_with(t.as_bytes(), "chunks");
                let (chunks, got_sep, got, len) = read_geojson(&mut f, false, "t", &o, 0, 2, "t");
                assert_eq!((got_sep, &got, len), (sep, &streams, t.len() as u64), "{t}");
                assert_eq!(xs(&chunks), want, "{t} {cpus}");
            }
        }
    }

    #[test]
    fn gzip_input_is_read_as_the_text() {
        use std::io::Write;
        let t = &texts()[0];
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        z.write_all(t.as_bytes()).unwrap();
        let gz = z.finish().unwrap();
        let o = ro(false, 4);
        let (a, ..) = read_geojson(&mut file_with(t.as_bytes(), "plain"), true, "t", &o, 0, 2, "t");
        let (b, _, _, len) = read_geojson(&mut file_with(&gz, "gz"), true, "t", &o, 0, 2, "t");
        assert_eq!(len, t.len() as u64);
        assert_eq!(xs(&a), xs(&b));
    }

    #[test]
    fn csv_in_blocks_gives_the_rows_in_order() {
        let mut text = String::from("name,lat,lon\n");
        for i in 0..200 {
            text += &format!("r{i},{},{}\n", i % 80, i);
        }
        let mut f = file_with(text.as_bytes(), "csv");
        let (chunks, n) = read_csv(&mut f, "t.csv", &ro(false, 4), 0, 2);
        assert_eq!(n, 200);
        let want: Vec<i64> = (0..200).map(|i| vtmake_geom::lonlat_to_world(i as f64, (i % 80) as f64).0).collect();
        assert_eq!(xs(&chunks), want);
    }
}
