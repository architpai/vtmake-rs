//! Postgres binary COPY sources (`--pgcopy-schema`): see
//! `vtmake_parse::pgcopy` and `docs/pgcopy.md`.
//!
//! A reader thread reads the input (a file, or standard input from a pipe
//! while Postgres still sends) in blocks. The main thread finds the complete
//! rows of a block and cuts them into pieces. The worker threads decode the
//! pieces in parallel and add the features to their chunks. Feature `i` of
//! row `r` has `seq = ((base + r) << SEQ_SHIFT) + i`: the row order is the
//! feature order. The memory for the input is bounded (two blocks in the
//! channel, the work buffer, and the blocks that come back for reuse).
//!
//! The input must end with the COPY trailer and nothing after it. If it does
//! not (for example, Postgres or the pipe failed), vtmake stops with exit
//! 107 and writes no tiles.

use super::stream::{BLOCK, Builders, Input, block_size, pinned};
use super::{Chunk, ReadOpts, SEQ_SHIFT, add_feature};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use vtmake_parse::ParseError;
use vtmake_parse::pgcopy::{Decoder, EXIT_DATA, Next, RowOut, Schema, header_len, next};

/// One piece of a block: `[start, end)` and the number of its first row.
struct Piece {
    start: usize,
    end: usize,
    first: u64,
}

fn fail(message: String) -> ! {
    ParseError { exit_code: EXIT_DATA, message }.exit()
}

/// The blocks of the input, in order. An empty block: the end of the input.
/// The blocks come back through `free` for reuse (the memory stays bounded
/// and the allocator does not keep blocks freed by the other thread).
fn produce(mut inp: Input<'_>, tx: SyncSender<Vec<u8>>, free: Receiver<Vec<u8>>) {
    loop {
        let mut b = free.try_recv().unwrap_or_else(|_| Vec::with_capacity(BLOCK));
        b.clear();
        // A read error of standard input ends the input (as `fread`); the
        // missing trailer then stops the run.
        let eof = inp.fill(&mut b, BLOCK).unwrap_or(true);
        if !b.is_empty() && tx.send(b).is_err() {
            return;
        }
        if eof {
            let _ = tx.send(Vec::new());
            return;
        }
    }
}

/// The consumer side of the reader thread.
struct Blocks {
    rx: Receiver<Vec<u8>>,
    free: SyncSender<Vec<u8>>,
}

impl Blocks {
    /// Append the next block to `buf`; `true` at the end of the input.
    fn more(&self, buf: &mut Vec<u8>) -> bool {
        match self.rx.recv() {
            Ok(b) if !b.is_empty() => {
                buf.extend_from_slice(&b);
                let _ = self.free.try_send(b);
                false
            }
            _ => true,
        }
    }
}

/// A COPY source: `(chunks, rows)`. `name`: the file name in the messages.
pub(super) fn read_pgcopy(file: &mut std::fs::File, name: &str, schema: &Schema, ro: &ReadOpts, base: u64, threads: usize) -> (Vec<Chunk>, u64) {
    let open_error = (name != "standard input").then_some(name);
    let inp = Input::new(file, false, open_error);
    let (block, piece) = block_size(threads);
    let (tx, rx) = sync_channel::<Vec<u8>>(2);
    let (free_tx, free_rx) = sync_channel::<Vec<u8>>(3);
    std::thread::scope(|scope| {
        scope.spawn(move || produce(inp, tx, free_rx));
        let blocks = Blocks { rx, free: free_tx };
        let result = consume(&blocks, name, schema, ro, base, block, piece);
        // Let the reader thread end (it may wait to send).
        drop(blocks);
        result
    })
}

fn consume(rx: &Blocks, name: &str, schema: &Schema, ro: &ReadOpts, base: u64, block: usize, piece: usize) -> (Vec<Chunk>, u64) {
    let ncols = schema.columns();
    let mut buf: Vec<u8> = Vec::with_capacity(block + BLOCK);
    let mut eof = false;
    loop {
        match header_len(&buf) {
            Err(e) => e.exit(),
            Ok(Some(h)) => {
                buf.drain(..h);
                break;
            }
            Ok(None) if eof => fail(format!("{name}: pgcopy: the input ends before the end of the COPY header")),
            Ok(None) => eof = rx.more(&mut buf),
        }
    }
    let builders = Builders::new(ro, base);
    let mut rows = 0u64;
    let mut features = 0u64;
    let mut with_geometry = 0u64;
    loop {
        while !eof && buf.len() < block {
            eof = rx.more(&mut buf);
        }
        // The complete rows, in pieces of about `piece` bytes.
        let mut pieces: Vec<Piece> = Vec::new();
        let mut p = 0usize;
        let mut trailer = false;
        let mut row = rows;
        loop {
            match next(&buf[p..], ncols) {
                Err(e) => fail(format!("{name}: {}", e.message)),
                Ok(Next::Row(n)) => {
                    match pieces.last_mut() {
                        Some(last) if last.end - last.start < piece => last.end += n,
                        _ => pieces.push(Piece { start: p, end: p + n, first: row }),
                    }
                    p += n;
                    row += 1;
                }
                Ok(Next::Trailer) => {
                    trailer = true;
                    break;
                }
                Ok(Next::More) => break,
            }
        }
        if pieces.is_empty() && !trailer {
            if eof {
                fail(format!("{name}: pgcopy: the input ends in a row or before the COPY trailer (the export failed?)"));
            }
            // A row that is larger than the block.
            eof = rx.more(&mut buf);
            continue;
        }
        let data = &buf[..p];
        let results: Vec<Result<(u64, u64), ParseError>> = pinned(pieces.len(), |j, w| {
            let pc = &pieces[j];
            let mut dec = Decoder::new(schema, ro.settings.clone(), name);
            let mut out = RowOut::default();
            builders.with(w, |b, full| {
                let mut n = (0u64, 0u64);
                let mut q = pc.start;
                let mut r = pc.first;
                while q < pc.end {
                    let Ok(Next::Row(len)) = next(&data[q..], ncols) else { unreachable!("the rows were checked") };
                    dec.row(&data[q..q + len], &mut out)?;
                    for i in 0..out.count {
                        add_feature(b, &out.features[i], ((base + r) << SEQ_SHIFT) + i as u64, out.attrs(schema, i));
                        full(b);
                    }
                    n.0 += u64::from(out.has_geometry);
                    n.1 += out.count as u64;
                    q += len;
                    r += 1;
                }
                Ok(n)
            })
        });
        // The first error in the row order.
        for r in results {
            match r {
                Ok((g, n)) => {
                    with_geometry += g;
                    features += n;
                }
                Err(e) => ParseError { exit_code: e.exit_code, message: format!("{name}: {}", e.message) }.exit(),
            }
        }
        rows = row;
        if trailer {
            // Nothing may follow the trailer.
            let rest = buf.len() - p - 2;
            if rest != 0 || (!eof && !rx.more(&mut Vec::new())) {
                fail(format!("{name}: pgcopy: data after the COPY trailer"));
            }
            break;
        }
        buf.drain(..p);
    }
    if !ro.quiet {
        eprintln!("{name}: pgcopy: {rows} rows, {with_geometry} with geometry, {features} features");
    }
    (builders.finish(), rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Projection;
    use std::io::Write;

    fn ro() -> ReadOpts {
        ReadOpts {
            projection: Projection::Epsg4326,
            geometry_scale: 0,
            settings: vtmake_parse::geojson::Settings { projection_alias: Projection::Epsg4326.alias(), quiet: true, ..Default::default() },
            read_parallel: false,
            felt_cpus: 4,
            empty_csv_null: false,
            quiet: true,
            pgcopy: None,
        }
    }

    fn copy(rows: &[(Option<&str>, i32)]) -> Vec<u8> {
        let mut b = vtmake_parse::pgcopy::SIGNATURE.to_vec();
        b.extend_from_slice(&[0; 8]);
        for (g, v) in rows {
            b.extend_from_slice(&2i16.to_be_bytes());
            match g {
                None => b.extend_from_slice(&(-1i32).to_be_bytes()),
                Some(g) => {
                    b.extend_from_slice(&(g.len() as i32).to_be_bytes());
                    b.extend_from_slice(g.as_bytes());
                }
            }
            b.extend_from_slice(&4i32.to_be_bytes());
            b.extend_from_slice(&v.to_be_bytes());
        }
        b.extend_from_slice(&(-1i16).to_be_bytes());
        b
    }

    #[test]
    fn rows_in_blocks_keep_their_order() {
        let schema = Schema::from_json(r#"{"format":"vtmake-pgcopy/1","columns":[{"name":"geom","type":"text","role":"geometry"},{"name":"v","type":"int4"}]}"#, 0, 14).unwrap();
        let rows: Vec<(String, i32)> = (0..300).map(|i| (format!(r#"{{"type":"Point","coordinates":[{},{}]}}"#, i % 170, i % 80), i)).collect();
        let mut input: Vec<(Option<&str>, i32)> = rows.iter().map(|(g, v)| (Some(g.as_str()), *v)).collect();
        input.insert(7, (None, -1));
        let data = copy(&input);
        let name = std::env::temp_dir().join(format!("vtmake-pgcopy-test-{}", std::process::id()));
        std::fs::File::create(&name).unwrap().write_all(&data).unwrap();
        let mut f = std::fs::File::open(&name).unwrap();
        std::fs::remove_file(&name).unwrap();
        let (chunks, n) = read_pgcopy(&mut f, "t", &schema, &ro(), 0, 2);
        assert_eq!(n, 301);
        let mut v: Vec<(u64, i64)> = chunks.iter().flat_map(|c| c.seqs.iter().zip(&c.metas).map(|(s, m)| (*s, m.bbox[0]))).collect();
        v.sort();
        let want: Vec<i64> = (0..300).map(|i| vtmake_geom::lonlat_to_world((i % 170) as f64, (i % 80) as f64).0).collect();
        assert_eq!(v.iter().map(|x| x.1).collect::<Vec<_>>(), want);
        // The NULL geometry row is skipped, but it has its row number.
        assert_eq!(v[7].0 >> SEQ_SHIFT, 8);
    }
}
