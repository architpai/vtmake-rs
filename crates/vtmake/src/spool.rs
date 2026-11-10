//! Disk queues for tiling with limits. Each queue uses an unlinked file in `-t`.
//! Ported from tippecanoe's disk-backed zoom passes.
//! Copyright (c) 2022, Protomaps LLC; Copyright (c) 2014, Mapbox Inc.
//! BSD 2-Clause license: LICENSE-tippecanoe.md.
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, Write};
use std::fs::{File, OpenOptions};
use std::sync::atomic::{AtomicU64, Ordering};
use crate::tile::{TF, TileTask};
use rayon::prelude::*;
use vtmake_core::Draw;

const BLOCK_FEATURES: usize = 8192;

pub struct EncodedTask {
    header: [u64; 6],
    blocks: Vec<(Vec<u8>, usize)>,
}

pub struct Spool {
    file: BufWriter<File>,
    pub count: usize,
}
impl Spool {
    pub fn new(dir: &str) -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::path::Path::new(dir).join(format!("vtmake-{}-{}", std::process::id(), ID.fetch_add(1, Ordering::Relaxed)));
            match OpenOptions::new().read(true).write(true).create_new(true).open(&path) {
                Ok(file) => {
                    std::fs::remove_file(path).expect("unlink tile queue");
                    return Self { file: BufWriter::new(file), count: 0 };
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("create tile queue in {dir}: {e}"),
            }
        }
    }
    fn put(&mut self, mut n: u64) {
        let mut bytes = [0u8; 10];
        let mut len = 0;
        while n >= 128 { bytes[len] = n as u8 | 128; len += 1; n >>= 7; }
        bytes[len] = n as u8;
        self.file.write_all(&bytes[..=len]).expect("write tile queue");
    }
    #[inline]
    fn get(r: &mut BufReader<File>) -> u64 {
        let mut n = 0;
        let mut shift = 0;
        loop {
            let bytes = r.fill_buf().expect("read tile queue");
            assert!(!bytes.is_empty(), "truncated tile queue");
            let mut used = 0;
            let mut done = false;
            for &b in bytes {
                assert!(shift < 70, "invalid tile queue integer");
                n |= ((b & 127) as u64) << shift;
                used += 1;
                shift += 7;
                if b < 128 { done = true; break; }
            }
            r.consume(used);
            if done { return n; }
        }
    }
    pub fn clear(&mut self) {
        self.file.flush().expect("flush tile queue");
        self.file.get_mut().set_len(0).expect("truncate tile queue");
        self.file.rewind().expect("rewind tile queue");
        self.count = 0;
    }
    pub fn reader(&mut self) -> BufReader<File> {
        self.file.flush().expect("flush tile queue");
        let mut f = self.file.get_ref().try_clone().expect("open tile queue reader");
        f.rewind().expect("rewind tile queue");
        BufReader::new(f)
    }
    pub fn push_task(&mut self, (z,x,y,feats,draws): TileTask) {
        // Some callers hold the queue mutex. Do not yield to Rayon here.
        self.push_encoded(Self::encode_task(z, x, y, &feats, &draws, (0, 0), 0, false));
    }
    /// Each block has independent coordinate deltas. Encode and decode large
    /// tasks in parallel, and keep the blocks and features in source order.
    /// Children read the parent's arena without an expanded geometry copy.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_task_refs(z: u8, x: u32, y: u32, feats: &[TF], draws: &[Draw], origin: (i64, i64), scale: i32) -> EncodedTask {
        Self::encode_task(z, x, y, feats, draws, origin, scale, true)
    }
    #[allow(clippy::too_many_arguments)]
    fn encode_task(z: u8, x: u32, y: u32, feats: &[TF], draws: &[Draw], origin: (i64, i64), scale: i32, parallel: bool) -> EncodedTask {
        let ndraws: usize = feats.iter().map(|f| f.len as usize).sum();
        let header = [z as u64, x as u64, y as u64, feats.len() as u64, ndraws as u64, feats.len().div_ceil(BLOCK_FEATURES) as u64];
        let encode = |fs: &[TF]| {
            let mut bytes = Vec::new();
            let (mut px, mut py) = (0i64, 0i64);
            for f in fs {
                for v in [f.pos as u64, f.len as u64, f.gap] { crate::store::put_varint(&mut bytes, v); }
                for d in &draws[f.start as usize..(f.start + f.len) as usize] {
                    let (mut dx, mut dy) = (d.x, d.y);
                    if d.op == vtmake_core::VT_MOVETO || d.op == vtmake_core::VT_LINETO {
                        dx += origin.0;
                        dy += origin.1;
                        if scale > 0 {
                            dx = ((dx as f64 / (1u64 << scale) as f64).round() as i64) << scale;
                            dy = ((dy as f64 / (1u64 << scale) as f64).round() as i64) << scale;
                        }
                    }
                    crate::store::put_varint(&mut bytes, d.op as u64 | ((d.necessary as u64) << 3));
                    for delta in [dx.wrapping_sub(px), dy.wrapping_sub(py)] { crate::store::put_varint(&mut bytes, ((delta << 1) ^ (delta >> 63)) as u64); }
                    (px, py) = (dx, dy);
                }
            }
            (bytes, fs.iter().map(|f| f.len as usize).sum::<usize>())
        };
        // Bound compressed buffers as well as expanded geometry. Small tasks
        // use the same codec without Rayon scheduling.
        let mut output = Vec::new();
        for batch in feats.chunks(BLOCK_FEATURES * 64) {
            let blocks: Vec<_> = if parallel && batch.len() > BLOCK_FEATURES && rayon::current_num_threads() > 2 {
                batch.par_chunks(BLOCK_FEATURES).map(encode).collect()
            } else { batch.chunks(BLOCK_FEATURES).map(encode).collect() };
            output.extend(blocks);
        }
        EncodedTask { header, blocks: output }
    }
    pub fn push_encoded(&mut self, task: EncodedTask) {
        for v in task.header { self.put(v); }
        for (bytes, ndraws) in task.blocks {
            self.put(bytes.len() as u64);
            self.put(ndraws as u64);
            self.file.write_all(&bytes).expect("write tile queue");
        }
        self.count += 1;
    }
    pub fn read_task(r: &mut BufReader<File>) -> TileTask {
        let (z,x,y,n,m,blocks) = (Self::get(r) as u8, Self::get(r) as u32, Self::get(r) as u32, Self::get(r) as usize, Self::get(r) as usize, Self::get(r) as usize);
        let mut feats = Vec::with_capacity(n);
        let mut draws = Vec::with_capacity(m);
        for base in (0..blocks).step_by(64) {
            let mut inputs = Vec::new();
            for block in base..blocks.min(base + 64) {
                let len = Self::get(r) as usize;
                let ndraws = Self::get(r) as usize;
                let mut bytes = vec![0; len];
                r.read_exact(&mut bytes).expect("read tile queue");
                inputs.push((bytes, (n - block * BLOCK_FEATURES).min(BLOCK_FEATURES), ndraws));
            }
            let decode = |(bytes, count, ndraws): (Vec<u8>, usize, usize)| {
                let mut cursor = 0;
                let mut get = || {
                    let mut value = 0;
                    for shift in (0..70).step_by(7) {
                        let b = bytes[cursor]; cursor += 1;
                        value |= ((b & 127) as u64) << shift;
                        if b < 128 { return value; }
                    }
                    panic!("invalid tile queue integer")
                };
                let mut fs = Vec::with_capacity(count);
                let mut ds = Vec::with_capacity(ndraws);
                let (mut x0, mut y0) = (0i64, 0i64);
                for _ in 0..count {
                    let (pos, len, gap) = (get() as u32, get() as u32, get());
                    fs.push(TF { pos, start: ds.len() as u32, len, gap });
                    for _ in 0..len {
                        let op = get() as u8;
                        let dx = get(); let dy = get();
                        x0 = x0.wrapping_add(((dx >> 1) as i64) ^ -((dx & 1) as i64));
                        y0 = y0.wrapping_add(((dy >> 1) as i64) ^ -((dy & 1) as i64));
                        ds.push(Draw { op: op & 7, x: x0, y: y0, necessary: op & 8 != 0 });
                    }
                }
                (fs, ds)
            };
            let decoded: Vec<_> = if inputs.len() > 1 && rayon::current_num_threads() > 2 {
                inputs.into_par_iter().map(decode).collect()
            } else { inputs.into_iter().map(decode).collect() };
            for (fs, ds) in decoded {
                let offset = draws.len() as u32;
                feats.extend(fs.into_iter().map(|f| TF { start: f.start + offset, ..f }));
                draws.extend(ds);
            }
        }
        (z,x,y,feats,draws)
    }
    pub fn push_tile(&mut self, (z,x,y,data): (u8,u32,u32,Vec<u8>)) {
        for n in [z as u64,x as u64,y as u64,data.len() as u64] { self.put(n); }
        self.file.write_all(&data).expect("write tile queue"); self.count += 1;
    }
    pub fn read_tile(r: &mut BufReader<File>) -> (u8,u32,u32,Vec<u8>) {
        let (z,x,y,n) = (Self::get(r) as u8, Self::get(r) as u32, Self::get(r) as u32, Self::get(r) as usize);
        let mut data = vec![0;n]; r.read_exact(&mut data).expect("read tile queue"); (z,x,y,data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_codec_preserves_order_and_child_coordinates() {
        let n = BLOCK_FEATURES * 3 + 17;
        let draws: Vec<_> = (0..n).map(|i| Draw { op: 1, x: i as i64 * 31 - 10000, y: i as i64 * -17, necessary: i % 5 == 0 }).collect();
        let feats: Vec<_> = (0..n).map(|i| TF { pos: i as u32, start: i as u32, len: 1, gap: (i as u64).wrapping_mul(u64::MAX) }).collect();
        for threads in [1, 2, 7] {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
            pool.install(|| {
                let mut q = Spool::new(std::env::temp_dir().to_str().unwrap());
                let encoded = Spool::encode_task_refs(2, 1, 3, &feats, &draws, (900, -300), 3);
                q.push_encoded(encoded);
                let (_, _, _, got, geometry) = Spool::read_task(&mut q.reader());
                assert_eq!(got.len(), n);
                for (i, (f, d)) in got.iter().zip(&geometry).enumerate() {
                    assert_eq!((f.pos, f.start, f.len, f.gap), (i as u32, i as u32, 1, feats[i].gap));
                    let round = |v: i64| ((v as f64 / 8.).round() as i64) << 3;
                    assert_eq!(*d, Draw { x: round(draws[i].x + 900), y: round(draws[i].y - 300), ..draws[i] });
                }
            });
        }
    }

    #[test]
    fn queue_round_trip() {
        let mut q = Spool::new(std::env::temp_dir().to_str().unwrap());
        let draws = vec![
            Draw { op: 1, x: i64::MIN, y: i64::MAX, necessary: true },
            Draw { op: 2, x: i64::MAX, y: i64::MIN, necessary: false },
            Draw { op: 7, x: 0, y: 0, necessary: true },
        ];
        q.push_task((3, 4, 5, vec![TF { pos: u32::MAX, start: 0, len: 3, gap: u64::MAX }], draws.clone()));
        let task = Spool::read_task(&mut q.reader());
        assert_eq!((task.0, task.1, task.2), (3, 4, 5));
        assert_eq!(task.3[0].pos, u32::MAX);
        assert_eq!(task.3[0].gap, u64::MAX);
        assert_eq!(task.4, draws);
        q.clear();
        q.push_tile((6, 7, 8, vec![0, 128, 255]));
        assert_eq!(Spool::read_tile(&mut q.reader()), (6, 7, 8, vec![0, 128, 255]));
    }
}
