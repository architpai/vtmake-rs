//! Disk queues for tiling with limits. Each queue uses an unlinked file in `-t`.
//! Ported from tippecanoe's disk-backed zoom passes.
//! Copyright (c) 2022, Protomaps LLC; Copyright (c) 2014, Mapbox Inc.
//! BSD 2-Clause license: LICENSE-tippecanoe.md.
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::fs::{File, OpenOptions};
use std::sync::atomic::{AtomicU64, Ordering};
use crate::tile::{TF, TileTask};
use vtmake_core::Draw;

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
    fn get(r: &mut impl Read) -> u64 {
        let mut n = 0;
        for shift in (0..70).step_by(7) {
            let mut b = [0]; r.read_exact(&mut b).expect("read tile queue");
            n |= ((b[0] & 127) as u64) << shift;
            if b[0] < 128 { return n; }
        }
        panic!("invalid tile queue integer")
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
        for n in [z as u64, x as u64, y as u64, feats.len() as u64, draws.len() as u64] { self.put(n); }
        for f in feats { for n in [f.pos as u64, f.start as u64, f.len as u64, f.gap] { self.put(n); } }
        let (mut px, mut py) = (0i64, 0i64);
        for d in draws {
            self.put(d.op as u64 | ((d.necessary as u64) << 3));
            for delta in [d.x.wrapping_sub(px), d.y.wrapping_sub(py)] {
                self.put(((delta << 1) ^ (delta >> 63)) as u64);
            }
            (px, py) = (d.x, d.y);
        }
        self.count += 1;
    }
    pub fn read_task(r: &mut impl Read) -> TileTask {
        let (z,x,y,n,m) = (Self::get(r) as u8, Self::get(r) as u32, Self::get(r) as u32, Self::get(r), Self::get(r));
        let feats = (0..n).map(|_| TF { pos: Self::get(r) as u32, start: Self::get(r) as u32, len: Self::get(r) as u32, gap: Self::get(r) }).collect();
        let (mut x0, mut y0) = (0i64, 0i64);
        let draws = (0..m).map(|_| {
            let op = Self::get(r) as u8;
            let dx = Self::get(r); let dy = Self::get(r);
            x0 = x0.wrapping_add(((dx >> 1) as i64) ^ -((dx & 1) as i64));
            y0 = y0.wrapping_add(((dy >> 1) as i64) ^ -((dy & 1) as i64));
            Draw { op: op & 7, x: x0, y: y0, necessary: op & 8 != 0 }
        }).collect();
        (z,x,y,feats,draws)
    }
    pub fn push_tile(&mut self, (z,x,y,data): (u8,u32,u32,Vec<u8>)) {
        for n in [z as u64,x as u64,y as u64,data.len() as u64] { self.put(n); }
        self.file.write_all(&data).expect("write tile queue"); self.count += 1;
    }
    pub fn read_tile(r: &mut impl Read) -> (u8,u32,u32,Vec<u8>) {
        let (z,x,y,n) = (Self::get(r) as u8, Self::get(r) as u32, Self::get(r) as u32, Self::get(r) as usize);
        let mut data = vec![0;n]; r.read_exact(&mut data).expect("read tile queue"); (z,x,y,data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
