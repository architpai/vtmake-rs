//! Optional tile selection and raw-MVT hashes. No work when options are absent.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::Mutex;

type Key = (u8, u32, u32);
const HEADER: &str = "vtmake-hashes-v1 blake3-256 raw-mvt";

pub struct Selection {
    targets: Option<BTreeSet<Key>>,
    ancestors: BTreeSet<Key>,
    known: BTreeMap<Key, blake3::Hash>,
    hashes: Option<Mutex<BTreeMap<Key, blake3::Hash>>>,
    output: Option<String>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn key(s: &str) -> io::Result<Key> {
    let parts: Vec<_> = s.split('/').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|c| c.is_ascii_digit())) {
        return Err(invalid(format!("invalid tile key: {s}")));
    }
    let z: u8 = parts[0].parse().map_err(|_| invalid("invalid zoom"))?;
    let x: u32 = parts[1].parse().map_err(|_| invalid("invalid x"))?;
    let y: u32 = parts[2].parse().map_err(|_| invalid("invalid y"))?;
    if z > 24 || x as u64 >= 1u64 << z || y as u64 >= 1u64 << z {
        return Err(invalid(format!("tile outside XYZ range (zoom 0..24): {s}")));
    }
    Ok((z, x, y))
}

fn lines(path: &str) -> io::Result<Vec<String>> {
    io::BufReader::new(std::fs::File::open(path)?).lines().collect()
}

impl Selection {
    pub fn load(o: &crate::cli::Options) -> io::Result<Option<Self>> {
        if o.only_tiles.is_none() && o.known_hashes.is_none() && o.hashes_out.is_none() {
            return Ok(None);
        }
        // Limits need every tile's encoded/compressed size and global retries.
        if !o.prevent[b'f' as usize] || !o.prevent[b'k' as usize]
            || o.additional[b't' as usize] || o.prefilter.is_some() || o.postfilter.is_some() {
            return Err(invalid("tile selection/hashes require --no-feature-limit --no-tile-size-limit and do not support variable-depth pyramids or shell filters"));
        }
        if o.only_tiles.is_some() && o.allow_existing {
            return Err(invalid("--only-tiles does not support --allow-existing; use a fresh output or --force"));
        }
        if o.known_hashes.is_some() && o.hashes_out.is_none() {
            return Err(invalid("--known-hashes requires --hashes-out for the complete produced key set"));
        }
        if let Some(out) = &o.hashes_out {
            let path = Path::new(out);
            let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).canonicalize()?;
            let dest = parent.join(path.file_name().ok_or_else(|| invalid("invalid hashes output path"))?);
            let output = match &o.output { crate::cli::Output::Dir(p) | crate::cli::Output::Mbtiles(p) => p };
            let output_absolute = if output.is_absolute() { output.clone() } else { std::env::current_dir()?.join(output) };
            // Canonicalize the parent as well, so a not-yet-created MBTiles
            // path cannot be replaced by the manifest after the sink closes.
            let output_resolved = output.canonicalize().ok().or_else(|| {
                Some(output_absolute.parent()?.canonicalize().ok()?.join(output_absolute.file_name()?))
            });
            if output_resolved.as_ref().is_some_and(|p| dest == *p || matches!(&o.output, crate::cli::Output::Dir(_)) && dest.starts_with(p)) {
                return Err(invalid("hashes output must be outside the tile output"));
            }
            for p in o.sources.iter().map(|s| s.file.as_str()).chain(o.only_tiles.iter().map(String::as_str)).chain(std::iter::once(output.to_str().ok_or_else(|| invalid("non-UTF8 output path"))?)) {
                if !p.is_empty() && Path::new(p).canonicalize().ok().as_ref() == Some(&dest) {
                    return Err(invalid("hashes output must not replace an input or tile output"));
                }
            }
        }
        let targets = o.only_tiles.as_ref().map(|path| -> io::Result<_> {
            let mut set = BTreeSet::new();
            for line in lines(path)? {
                if !line.trim().is_empty() { set.insert(key(line.trim())?); }
            }
            Ok(set)
        }).transpose()?;
        let mut ancestors = BTreeSet::new();
        if let Some(targets) = &targets {
            for &(z, x, y) in targets {
                for a in 0..=z { ancestors.insert((a, x >> (z-a), y >> (z-a))); }
            }
        }
        let mut known = BTreeMap::new();
        if let Some(path) = &o.known_hashes {
            let input = lines(path)?;
            if input.first().map(String::as_str) != Some(HEADER) { return Err(invalid("invalid hash file version or algorithm")); }
            for line in &input[1..] {
                let fields: Vec<_> = line.split_whitespace().collect();
                if fields.len() != 2 { return Err(invalid("expected z/x/y and a 64-digit BLAKE3 hash")); }
                let k = key(fields[0])?;
                let hash = blake3::Hash::from_hex(fields[1]).map_err(|_| invalid("invalid BLAKE3 hash"))?;
                if known.insert(k, hash).is_some() { return Err(invalid("duplicate hash key")); }
            }
        }
        Ok(Some(Self { targets, ancestors, known, hashes: o.hashes_out.as_ref().map(|_| Mutex::new(BTreeMap::new())), output: o.hashes_out.clone() }))
    }

    pub fn descend(&self, z: u8, x: u32, y: u32) -> bool {
        self.targets.is_none() || self.ancestors.contains(&(z, x, y))
    }

    pub fn contains(&self, z: u8, x: u32, y: u32) -> bool {
        self.targets.as_ref().is_none_or(|t| t.contains(&(z, x, y)))
    }

    pub fn record(&self, z: u8, x: u32, y: u32, bytes: &[u8]) -> bool {
        if self.hashes.is_none() { return false; }
        let hash = blake3::hash(bytes);
        let k = (z, x, y);
        self.hashes.as_ref().unwrap().lock().unwrap().insert(k, hash);
        self.known.get(&k) == Some(&hash)
    }

    /// Publish only after the tile sink succeeds. Never expose a partial manifest.
    pub fn finish(&self) -> io::Result<()> {
        let Some(path) = &self.output else { return Ok(()); };
        let parent = Path::new(path).parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        {
            let mut out = io::BufWriter::new(temp.as_file_mut());
            writeln!(out, "{HEADER}")?;
            for (&(z, x, y), hash) in self.hashes.as_ref().unwrap().lock().unwrap().iter() {
                writeln!(out, "{z}/{x}/{y} {}", hash.to_hex())?;
            }
            out.flush()?;
        }
        temp.as_file().sync_all()?;
        temp.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xyz_validation() {
        assert_eq!(key("18/123/456").unwrap(), (18,123,456));
        for s in ["1/2/0", "25/0/0", "-1/0/0", "1/0", "1/0/0/0", "1/+0/0"] { assert!(key(s).is_err(), "{s}"); }
    }
    #[test]
    fn raw_hash_contract() {
        assert_eq!(blake3::hash(b"").to_hex().as_str(), "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262");
    }
}
