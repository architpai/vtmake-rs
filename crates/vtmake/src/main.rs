//! vtmake: fast vector tiles. A reimplementation of felt/tippecanoe (see NOTICE).
//!
//! Commands: `vtmake build` (tippecanoe), `vtmake join` (tile-join),
//! `vtmake decode` (tippecanoe-decode), `vtmake enumerate`, `vtmake overzoom`
//! and `vtmake json-tool`, each with the flags of the tool it replaces. Started
//! under the name of the felt tool (for example through a symlink), it acts as
//! that tool.

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod attrs;
mod cli;
mod filter;
mod defaults;
mod drop;
mod meta;
mod plugin;
mod progress;
mod read;
mod store;
mod spool;
mod tile;
mod selection;

use vtmake_out::{DirWriter, MbtilesWriter, TileSink};
use rayon::prelude::*;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;
use store::{Chunk, Sorted};

/// tippecanoe exit codes (`errors.hpp`).
const EXIT_NODATA: i32 = 110;
const EXIT_OPEN: i32 = 111;

/// The text of an OS error without " (os error N)", as `perror` writes it.
pub fn os_error(e: &std::io::Error) -> String {
    let msg = e.to_string();
    msg.split(" (os error").next().unwrap_or(&msg).to_string()
}

/// tippecanoe makes its temporary files in `-t` (`main.cpp:read_input`) and
/// stops with exit 111 if it cannot. vtmake does not use them, but it checks.
fn check_tmpdir(dir: &str) {
    let name = format!("{dir}/pool.XXXXXXXX");
    let probe = format!("{dir}/.vtmake-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
        }
        Err(e) => {
            eprintln!("{name}: {}", os_error(&e));
            std::process::exit(EXIT_OPEN)
        }
    }
}

fn threads() -> usize {
    // tippecanoe's rules (`main.cpp:init_cpus`, `atoi_require`): a value that is
    // not a whole number stops the run; the count is 1 to 32767. tippecanoe also
    // rounds it down to a power of 2; the output of vtmake does not depend on the
    // thread count, so the pool keeps the full count (review F9).
    let n: i64 = match std::env::var("TIPPECANOE_MAX_THREADS") {
        Ok(v) => {
            let t = v.trim_start();
            match t.parse::<i64>() {
                Ok(n) if !t.is_empty() => n,
                _ => {
                    let prog = std::env::args().next().unwrap_or_else(|| "vtmake".into());
                    eprintln!("{prog}: TIPPECANOE_MAX_THREADS must be a number (got {v})");
                    std::process::exit(101)
                }
            }
        }
        Err(_) => std::thread::available_parallelism().map(|n| n.get() as i64).unwrap_or(1),
    };
    n.clamp(1, 32767) as usize
}

/// Tile gzip mode from `VTMAKE_GZIP`: `exact` (default, the same bytes as
/// tippecanoe) or `fast` (libdeflate, other gzip bytes, the same decoded tiles).
fn gzip_mode() -> vtmake_mvt::gzip::Mode {
    match std::env::var("VTMAKE_GZIP") {
        Err(_) => vtmake_mvt::gzip::Mode::Exact,
        Ok(v) => vtmake_mvt::gzip::Mode::from_name(&v).unwrap_or_else(|| {
            eprintln!("VTMAKE_GZIP={v}: use \"exact\" or \"fast\"");
            std::process::exit(1)
        }),
    }
}

/// `file_bbox` (`main.cpp:choose_first_zoom`): the union of the feature bboxes.
/// A bbox off the plane on the left or right covers the whole width.
fn file_bbox(chunks: &[Chunk]) -> [i64; 4] {
    let mut fb = chunks
        .par_iter()
        .map(|c| {
            c.metas.iter().fold([u32::MAX as i64, u32::MAX as i64, 0, 0], |b, m| {
                [b[0].min(m.bbox[0]), b[1].min(m.bbox[1]), b[2].max(m.bbox[2]), b[3].max(m.bbox[3])]
            })
        })
        .reduce(|| [u32::MAX as i64, u32::MAX as i64, 0, 0], |a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]);
    if fb[0] < 0 || fb[2] > 0xFFFF_FFFF {
        fb[0] = 0;
        fb[2] = 0xFFFF_FFFF;
    }
    fb[1] = fb[1].max(0);
    fb[3] = fb[3].min(0xFFFF_FFFF);
    fb
}

/// `iz` (`main.cpp:choose_first_zoom`): the highest zoom at or below `-Z` where the
/// file bbox plus the buffer is in one tile. tippecanoe starts to tile there.
fn first_zoom(fb: &[i64; 4], minzoom: u8, buffer: i64) -> u8 {
    for z in (0..=minzoom).rev() {
        let shift = 1i64 << (32 - z as u32);
        let b = buffer * shift / 256;
        if (fb[0] - b) / shift == (fb[2] + b) / shift && (fb[1] - b) / shift == (fb[3] + b) / shift {
            return z;
        }
    }
    0
}

/// The command and its argv (argv[0] = the program name, then the flags).
fn command() -> (&'static str, Vec<String>) {
    let mut args: Vec<String> = std::env::args().collect();
    let prog = std::path::Path::new(args.first().map(String::as_str).unwrap_or("vtmake"))
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match prog.as_str() {
        "tippecanoe" => return ("build", args),
        "tippecanoe-enumerate" => return ("enumerate", args),
        "tile-join" => return ("join", args),
        "tippecanoe-decode" => return ("decode", args),
        "tippecanoe-overzoom" => return ("overzoom", args),
        "tippecanoe-json-tool" => return ("json-tool", args),
        _ => {}
    }
    match args.get(1).map(String::as_str) {
        Some(c @ ("build" | "join" | "decode" | "enumerate" | "overzoom" | "json-tool")) => {
            let c = match c {
                "build" => "build",
                "join" => "join",
                "enumerate" => "enumerate",
                "overzoom" => "overzoom",
                "json-tool" => "json-tool",
                _ => "decode",
            };
            let first = format!("{} {c}", args[0]);
            args.remove(0);
            args[0] = first;
            (c, args)
        }
        Some("--version" | "-v" | "version") => {
            println!("vtmake v{}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0)
        }
        _ => {
            eprintln!("Usage: vtmake build|join|decode|enumerate|overzoom|json-tool [options]");
            eprintln!("  build      make vector tiles (the flags of tippecanoe)");
            eprintln!("  join       join tilesets (the flags of tile-join)");
            eprintln!("  decode     decode tiles to GeoJSON (the flags of tippecanoe-decode)");
            eprintln!("  enumerate  list the tiles of tilesets (tippecanoe-enumerate)");
            eprintln!("  overzoom   make a tile from its parent tiles (the flags of tippecanoe-overzoom)");
            eprintln!("  json-tool  write GeoJSON features one per line (the flags of tippecanoe-json-tool)");
            std::process::exit(101)
        }
    }
}

fn main() {
    let (cmd, argv) = command();
    match cmd {
        "build" => build(argv),
        "enumerate" => vtmake_tools::enumerate::main(argv),
        "overzoom" => vtmake_tools::overzoom::main(argv),
        "json-tool" => vtmake_tools::jsontool::main(argv),
        "join" => vtmake_tools::join::main(argv, &format!("vtmake v{}", env!("CARGO_PKG_VERSION"))),
        _ => vtmake_tools::decode::main(argv),
    }
}

fn build(argv: Vec<String>) {
    let t0 = Instant::now();
    let mut opts = cli::parse(argv);
    cli::check_supported(&opts);
    let selection = selection::Selection::load(&opts).unwrap_or_else(|e| { eprintln!("vtmake build: {e}"); std::process::exit(101) });
    attrs::init(&opts);
    filter::init(opts.filter.as_deref());
    let gzip = if opts.prevent[b'C' as usize] { vtmake_mvt::gzip::Mode::None } else { gzip_mode() };
    meta::init(&opts);
    let nthreads = threads();
    rayon::ThreadPoolBuilder::new().num_threads(nthreads).build_global().unwrap();
    let verbose = std::env::var_os("VTMAKE_VERBOSE").is_some();
    let log = |what: &str| {
        if verbose {
            let st = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
            let kb = |k: &str| st.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok()).unwrap_or(0);
            eprintln!("[{:7.2}s] {what} (anon {} MB, file {} MB)", t0.elapsed().as_secs_f64(), kb("RssAnon:") / 1024, kb("RssFile:") / 1024);
        }
    };

    // tippecanoe makes the output before it reads the input (`-f`, `-F`, `-pC`).
    let argv0 = opts.argv.first().cloned().unwrap_or_default();
    let sink: Box<dyn TileSink> = match &opts.output {
        cli::Output::Mbtiles(p) => {
            let mut w = MbtilesWriter::open(p, opts.force, opts.allow_existing, &argv0);
            w.set_tile_compression(!opts.prevent[b'C' as usize]);
            Box::new(w)
        }
        cli::Output::Dir(p) => Box::new(DirWriter::open(p, opts.force, opts.allow_existing, &argv0)),
    };

    // tippecanoe makes its temporary files in `-t` now; it stops if it cannot.
    check_tmpdir(&opts.tmpdir);

    // Read (`main.cpp:read_input`). Each source has its default layer: `-l`,
    // `-L`, else a name from the file name (standard input: from `-n` or the
    // output name).
    let out_name = opts.name.clone().unwrap_or_else(|| match &opts.output {
        cli::Output::Mbtiles(p) | cli::Output::Dir(p) => p.to_string_lossy().into_owned(),
    });
    let mut sources = opts.sources.clone();
    for (l, src) in sources.iter_mut().enumerate() {
        if src.layer.is_empty() {
            let from = if src.file.is_empty() { out_name.as_str() } else { src.file.as_str() };
            src.layer = cli::layer_from_path(std::path::Path::new(from), l);
            if !opts.quiet {
                eprintln!("For layer {l}, using name \"{}\"", src.layer);
            }
        }
    }
    let _ = read::READ_GEOM.set(read::ReadGeom::new(&opts));
    let ro = read::ReadOpts {
        projection: opts.projection,
        geometry_scale: opts.geometry_scale,
        settings: vtmake_parse::geojson::Settings {
            convert_stringified_ids: opts.additional[b'I' as usize],
            quiet: opts.quiet,
            projection_alias: opts.projection.alias(),
        },
        read_parallel: opts.read_parallel,
        felt_cpus: read::felt_cpus(),
        empty_csv_null: opts.prevent[b'e' as usize],
        quiet: opts.quiet,
        pgcopy: opts.pgcopy_schema.as_deref().map(|s| {
            // Inline JSON, or the name of a JSON file.
            let text = if s.trim_start().starts_with('{') {
                s.to_string()
            } else {
                std::fs::read_to_string(s).unwrap_or_else(|e| {
                    eprintln!("{s}: {}", os_error(&e));
                    std::process::exit(EXIT_OPEN)
                })
            };
            std::sync::Arc::new(vtmake_parse::pgcopy::Schema::from_json(&text, opts.minzoom, opts.maxzoom).unwrap_or_else(|e| e.exit()))
        }),
    };
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut chunk_layer: Vec<String> = Vec::new();
    let mut base = 0u64;
    for src in &sources {
        // The layer id is the first source with this layer name; tippecanoe
        // takes the `.gz` test from the file of that source.
        let layer_file = &sources.iter().find(|s| s.layer == src.layer).unwrap().file;
        let Some((cs, size)) = read::read_source(src, layer_file, &ro, base, nthreads, &argv0) else { continue };
        base += size;
        chunk_layer.extend(std::iter::repeat_n(src.layer.clone(), cs.len()));
        chunks.extend(cs);
    }
    let nfeat: usize = chunks.iter().map(|c| c.metas.len()).sum();
    let mut geo = tile::Geo::new(&opts);
    if geo.opts.shared_nodes {
        geo.nodes = read::shared_nodes(&mut chunks);
    }
    log(&format!("read {nfeat} features"));
    if verbose {
        let sizes = chunks.iter().fold([0usize; 6], |mut a, c| {
            for (i, n) in [c.metas.capacity() * std::mem::size_of::<store::FeatMeta>() + (c.seqs.capacity() + c.ids.capacity()) * 8, c.geom.capacity(), c.attrs.capacity(), c.vals.capacity() * std::mem::size_of::<store::Val>(), c.val_text.capacity(), c.keys.iter().map(|k| k.len()).sum()].into_iter().enumerate() { a[i] += n; }
            a
        });
        eprintln!("store MB: metadata {}, geometry {}, attributes {}, values {}, text {}, keys {}", sizes[0] / 1048576, sizes[1] / 1048576, sizes[2] / 1048576, sizes[3] / 1048576, sizes[4] / 1048576, sizes[5] / 1048576);
    }

    if nfeat == 0 {
        // As tippecanoe: close the output with no tiles and no metadata.
        eprintln!("Did not read any valid geometries");
        sink.finish(&Vec::new()).expect("finish output");
        // A header-only manifest: no produced tiles. A caller must never
        // read the manifest of an earlier run as the result of this one.
        if let Some(s) = &selection { s.finish().unwrap_or_else(|e| { eprintln!("vtmake build: {e}"); std::process::exit(111) }); }
        std::process::exit(EXIT_NODATA);
    }

    let spacing = defaults::prepare(&mut chunks, &opts);

    // Sort by (index, seq).
    let mut keys: Vec<_> = chunks
        .iter()
        .enumerate()
        .flat_map(|(ci, c)| c.metas.iter().enumerate().map(move |(li, m)| ((m.index, c.seqs[li]), Sorted { chunk: ci as u32, local: li as u32 })))
        .collect();
    keys.par_sort_unstable_by_key(|s| s.0);
    let order: Vec<Sorted> = keys.iter().map(|s| s.1).collect();
    drop(keys);
    if !opts.prevent[b'i' as usize] && opts.retain_points_multiplier <= 1 { for c in &mut chunks { c.seqs = Vec::new(); } }
    read::collect_free();
    if opts.guess_maxzoom {
        defaults::guess_zoom(&mut opts, &chunks, &order, spacing);
    }
    if opts.guess_cluster_maxzoom && opts.cluster_maxzoom >= opts.maxzoom { opts.cluster_maxzoom = opts.maxzoom - 1; }
    defaults::guess_base(&mut opts, &chunks, &order);
    defaults::drop_pass(&mut chunks, &order, &opts);
    log("sorted");

    let fb = file_bbox(&chunks);
    let iz = first_zoom(&fb, opts.minzoom as u8, opts.buffer as i64);

    // Assign features to the tiles at -Z (bbox plus buffer).
    let z0 = opts.minzoom as u8;
    let shift = 32 - z0 as u32;
    // The feature bbox comes from the input coordinates. Without -ae the stored
    // geometry is rounded to 2^geometry_scale, which can move a point by up to
    // that step: widen the candidate box by it, else such points miss a tile
    // whose buffer they reach (review F2).
    let round_margin = if opts.geometry_scale > 0 { 1i64 << opts.geometry_scale } else { 0 };
    let b = opts.buffer as i64 * (1i64 << shift) / 256 + round_margin;
    let lim = (1i64 << z0) - 1;
    let world = 1i64 << 32;
    let edge = world * opts.buffer as i64 / 256;
    let mut pairs: Vec<(u64, u32, u32)> = order
        .par_iter()
        .enumerate()
        .flat_map_iter(|(pos, s)| {
            let m = &chunks[s.chunk as usize].metas[s.local as usize];
            // At z = 0, tippecanoe copies a feature near the date line 360 degrees
            // east or west (spec 6.8). The copies come to the tiles at -Z on the
            // other side when tiling starts at z = 0.
            let wrap = iz == 0 && z0 > 0;
            let shifts = [(0, true), (world, wrap && m.bbox[0] <= edge), (-world, wrap && m.bbox[2] >= world - edge)];
            shifts.into_iter().filter(|s| s.1).flat_map(move |(d, _)| {
                // A superset of the tiles whose buffer box touches the bbox (the clip
                // at the tile decides, and its edges are inclusive).
                let x0 = ((m.bbox[0] + d - b - 1) >> shift).clamp(0, lim);
                let y0 = ((m.bbox[1] - b - 1) >> shift).clamp(0, lim);
                let x1 = ((m.bbox[2] + d + b + 1) >> shift).clamp(0, lim);
                let y1 = ((m.bbox[3] + b + 1) >> shift).clamp(0, lim);
                (x0..=x1).flat_map(move |x| (y0..=y1).map(move |y| (((x as u64) << 32) | y as u64, pos as u32, m.ndraws)))
            })
        })
        .collect();
    if let Some((x, y)) = opts.one_tile {
        let key = ((x as u64) << 32) | y as u64;
        pairs.retain(|p| p.0 == key);
    }
    pairs.par_sort_unstable();
    pairs.dedup();
    let mut groups: Vec<(u64, std::ops::Range<usize>)> = Vec::new();
    let mut group_costs = Vec::new();
    let mut i = 0;
    while i < pairs.len() {
        let k = pairs[i].0;
        let s = i;
        let mut cost = 0usize;
        while i < pairs.len() && pairs[i].0 == k {
            cost += pairs[i].2 as usize * 72 + 64;
            i += 1;
        }
        groups.push((k, s..i));
        group_costs.push(cost);
    }
    let positions: Vec<u32> = pairs.iter().map(|p| p.1).collect();
    drop(pairs);
    read::collect_free();
    log(&format!("{} tiles at z{z0}", groups.len()));

    // Writer thread.
    let (tx, rx) = crossbeam_channel::bounded::<(u8, u32, u32, Vec<u8>)>(1024);
    let writer = std::thread::spawn(move || {
        let mut sink = sink;
        for (z, x, y, data) in rx {
            sink.write_tile(z, x, y, &data).expect("write tile");
        }
        sink
    });

    let default_layers: Vec<&str> = chunk_layer.iter().map(String::as_str).collect();
    plugin::init(&opts);
    let ctx = tile::Ctx {
        selection: selection.as_ref(),
        skip_tiles: Default::default(),
        trial_blocked: Default::default(),
        drop_state: std::sync::Mutex::new((drop::Threshold { gap: 0, sequence: 0, extent: 0, attribute: 0, gamma: opts.gamma }, drop::Threshold { gap: 0, sequence: 0, extent: 0, attribute: 0, gamma: opts.gamma })),
        staged: std::sync::Mutex::new(tile::Spool::new(&opts.tmpdir)),
        still_dropping: std::sync::atomic::AtomicBool::new(false),
        extents: tile::feature_extents(&opts, &chunks, &order),
        gaps: tile::feature_gaps(&opts, &chunks, &order),
        opts: &opts,
        pending: (!opts.prevent[b'f' as usize] || !opts.prevent[b'k' as usize]).then(|| std::sync::Mutex::new(tile::Spool::new(&opts.tmpdir))),
        failed: AtomicUsize::new(usize::MAX),
        incomplete: std::sync::atomic::AtomicBool::new(false),
        chunks: &chunks,
        order: &order,
        minzoom: (opts.minzoom as u8),
        maxzoom: AtomicUsize::new(opts.maxzoom as usize),
        buffer: opts.buffer as i64,
        default_layers: &default_layers,
        gzip,
        out: tx,
        max_tiled: AtomicUsize::new(0),
        starts_at_minzoom: iz == (opts.minzoom as u8),
        first_zoom: iz,
        tiles: AtomicU64::new(0),
        parallel_bytes: AtomicUsize::new(0),
        parallel_tasks: AtomicUsize::new(0),
        parallel_limit: nthreads.saturating_sub(1) * 4,
        parallel_budget: tile::memory_budget() / 4,
        center: std::sync::Mutex::new((0, u64::MAX)),
        strategies: (0..=32).map(|_| Default::default()).collect(),
        geo,
    };
    // The metadata statistics run in parallel with the tiling.
    let (mut stats, ()) = rayon::join(
        || meta::collect(&chunks, &default_layers),
        || {
            let budget = tile::group_memory_budget();
            let mut start = 0;
            while start < groups.len() {
                let mut end = start;
                let mut bytes = 0usize;
                while end < groups.len() {
                    let cost = group_costs[end];
                    if end > start && bytes.saturating_add(cost) > budget {
                        break;
                    }
                    bytes = bytes.saturating_add(cost);
                    end += 1;
                }
                groups[start..end].par_iter().for_each(|(k, r)| {
                    let (x, y) = ((k >> 32) as u32, *k as u32);
                    if selection.as_ref().is_some_and(|s| !s.descend(z0, x, y)) { return; }
                    let mut feats = Vec::with_capacity(r.len());
                    let mut draws = if ctx.pending.is_some() {
                        Vec::with_capacity(positions[r.clone()].iter().map(|&pos| ctx.meta(pos).1.ndraws as usize).sum())
                    } else { Vec::new() };
                    let mut g = Vec::new();
                    for &pos in &positions[r.clone()] {
                        let s = order[pos as usize];
                        let c = &chunks[s.chunk as usize];
                        let m = &c.metas[s.local as usize];
                        let start = draws.len() as u32;
                        c.decode_geom(m, &mut draws);
                        if ctx.first_zoom < z0 {
                            g.clear();
                            g.extend_from_slice(&draws[start as usize..]);
                            draws.truncate(start as usize);
                            if !tile::clip_ancestors(&ctx, m, x, y, &mut g) {
                                continue;
                            }
                            draws.extend_from_slice(&g);
                        }
                        feats.push(tile::TF {
                            gap: ctx.gaps.get(pos as usize).copied().unwrap_or(0),
                            pos,
                            start,
                            len: (draws.len() - start as usize) as u32,
                        });
                    }
                    if let Some(pending) = &ctx.pending {
                        let task = tile::Spool::encode_task_refs(z0, x, y, &feats, &draws, (0, 0), 0);
                        pending.lock().unwrap().push_encoded(task);
                    } else {
                        tile::process(&ctx, z0, x, y, feats, draws);
                    }
                });
                read::collect_free();
                start = end;
            }
            if let Some(pending) = &ctx.pending {
                loop {
                    if ctx.failed.load(Ordering::Relaxed) != usize::MAX || ctx.incomplete.load(Ordering::Relaxed) { break; }
                    let mut tasks = std::mem::replace(&mut *pending.lock().unwrap(), tile::Spool::new(&opts.tmpdir));
                    if tasks.count == 0 { break; }
                    tile::process_zoom(&ctx, &mut tasks);
                }
            }

        },
    );
    let failed = ctx.failed.load(Ordering::Relaxed);
    let incomplete = ctx.incomplete.load(Ordering::Relaxed);
    let ntiles = ctx.tiles.load(Ordering::Relaxed);
    let center = *ctx.center.lock().unwrap();
    let mut result = meta::TileResult {
        maxzoom: match ctx.max_tiled.load(Ordering::Relaxed) {
            // No feature comes to `-Z`: tippecanoe gives the highest zoom below `-Z`
            // that it tiled. That zoom depends on its CPU count (spec 5.4); use the
            // highest feature maxzoom (all are below `-Z`), but not below `iz`.
            0 => chunks.iter().flat_map(|c| c.metas.iter().map(|m| m.maxzoom)).max().unwrap_or(0).max(iz as i8) as u8,
            z => (z - 1) as u8,
        },
        center: (center.1 != u64::MAX).then_some(center.1),
        strategies: ctx.strategies[..=if failed == usize::MAX { ctx.maxzoom.load(Ordering::Relaxed) } else { failed }].iter().map(|a| a.each_ref().map(|v| v.load(Ordering::Relaxed))).collect(),
    };
    if failed != usize::MAX { result.maxzoom = failed.saturating_sub(1) as u8; }
    drop(ctx);
    log(&format!("tiled: {ntiles} tiles"));

    let sink = writer.join().unwrap();
    log("writer drained");
    if incomplete {
        sink.finish(&Vec::new()).expect("finish output");
        std::process::exit(100);
    }
    if plugin::active(&opts) {
        plugin::layer_stats(&mut stats);
    }
    attrs::merge_tile_stats(&mut stats);
    if failed != usize::MAX && failed <= opts.minzoom as usize + 1 {
        eprintln!("{argv0}: No zoom levels were successfully written");
        sink.finish(&Vec::new()).expect("finish output");
        std::process::exit(110);
    }
    // `layermap`: the description of the first source of each layer name.
    let mut layer_descriptions = std::collections::BTreeMap::new();
    for src in &sources {
        layer_descriptions.entry(src.layer.clone()).or_insert_with(|| src.description.clone());
    }
    let meta = meta::metadata(&opts, stats, fb, &result, &layer_descriptions);
    sink.finish(&meta).expect("finish output");
    if failed == usize::MAX {
        if let Some(s) = &selection { s.finish().unwrap_or_else(|e| { eprintln!("vtmake build: {e}"); std::process::exit(111) }); }
    }
    log("done");
    if failed != usize::MAX {
        eprintln!("\n\n\n*** NOTE TILES ONLY COMPLETE THROUGH ZOOM {} ***", result.maxzoom);
        std::process::exit(100);
    }
    if std::env::var("VTMAKE_WAGYU_MODE").as_deref() == Ok("stats") {
        vtmake_wagyu::print_statistics();
    }
    if std::env::var("VTMAKE_WAGYU_MODE").as_deref() == Ok("verify") {
        let (calls, hits) = vtmake_wagyu::verification_counts();
        eprintln!("wagyu verification: {calls} calls, {hits} fast hits, 0 mismatches");
    }
}
