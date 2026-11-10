//! `--only-tiles`, `--known-hashes` and `--hashes-out` against a full render of
//! the same binary, on a generated input (no data files, so CI can run it).

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

type Key = (u8, u32, u32);
const HEADER: &str = "vtmake-hashes-v1 blake3-256 raw-mvt";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_vtmake")
}

/// Polygons that cross tile edges, lines, and points, with per-feature zooms.
fn input(dir: &Path) -> PathBuf {
    let mut s = String::new();
    let mut seed: u64 = 0x9e3779b97f4a7c15;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    for i in 0..1500 {
        let lon = 139.70 + next() * 0.10;
        let lat = 35.64 + next() * 0.08;
        let d = 0.0002 + next() * 0.004;
        let minzoom = 12 + (i % 4);
        let geometry = match i % 3 {
            0 => format!(
                r#"{{"type":"Polygon","coordinates":[[[{lon},{lat}],[{},{lat}],[{},{}],[{lon},{}],[{lon},{lat}]]]}}"#,
                lon + d, lon + d, lat + d, lat + d
            ),
            1 => format!(r#"{{"type":"LineString","coordinates":[[{lon},{lat}],[{},{}],[{},{lat}]]}}"#, lon + d, lat + d / 2.0, lon + 2.0 * d),
            _ => format!(r#"{{"type":"Point","coordinates":[{lon},{lat}]}}"#),
        };
        s.push_str(&format!(
            r#"{{"type":"Feature","geometry":{geometry},"properties":{{"id":{i},"name":"f{i}","area":{d}}},"tippecanoe":{{"minzoom":{minzoom},"maxzoom":16}}}}"#
        ));
        s.push('\n');
    }
    let path = dir.join("in.ndjson");
    fs::write(&path, s).unwrap();
    path
}

const FLAGS: &[&str] = &[
    "build", "-q", "-Z12", "-z16", "--read-parallel", "--buffer=5", "--force",
    "--drop-densest-as-needed", "--extend-zooms-if-still-dropping",
    "--no-feature-limit", "--no-tile-size-limit", "--layer", "test",
];

fn run(args: &[&str], threads: u32) -> std::process::Output {
    Command::new(bin()).args(args).env("TIPPECANOE_MAX_THREADS", threads.to_string()).output().unwrap()
}

/// Render to a tile directory; return the tiles keyed by z/x/y (gzip bytes).
fn render(dir: &Path, name: &str, input: &Path, extra: &[&str], threads: u32) -> BTreeMap<Key, Vec<u8>> {
    let out = dir.join(name);
    let mut args: Vec<&str> = FLAGS.to_vec();
    args.extend_from_slice(extra);
    let out_s = out.to_str().unwrap().to_string();
    let in_s = input.to_str().unwrap().to_string();
    args.extend_from_slice(&["-e", &out_s, &in_s]);
    let r = run(&args, threads);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let mut tiles = BTreeMap::new();
    if !out.exists() {
        return tiles;
    }
    for z in fs::read_dir(&out).unwrap().flatten().filter(|e| e.path().is_dir()) {
        for x in fs::read_dir(z.path()).unwrap().flatten() {
            for y in fs::read_dir(x.path()).unwrap().flatten() {
                let p = y.path();
                let key = (
                    z.file_name().to_str().unwrap().parse().unwrap(),
                    x.file_name().to_str().unwrap().parse().unwrap(),
                    p.file_stem().unwrap().to_str().unwrap().parse().unwrap(),
                );
                tiles.insert(key, fs::read(&p).unwrap());
            }
        }
    }
    tiles
}

fn gunzip(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(b).read_to_end(&mut out).unwrap();
    out
}

fn manifest(rows: impl IntoIterator<Item = (Key, String)>) -> String {
    let mut s = format!("{HEADER}\n");
    for ((z, x, y), h) in rows {
        s.push_str(&format!("{z}/{x}/{y} {h}\n"));
    }
    s
}

fn write_keys(path: &Path, keys: impl IntoIterator<Item = Key>) {
    fs::write(path, keys.into_iter().map(|(z, x, y)| format!("{z}/{x}/{y}\n")).collect::<String>()).unwrap();
}

#[test]
fn selection_and_hashes_match_full_render() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let input = input(dir);
    let full_m = dir.join("full.hashes");
    let full_m_s = full_m.to_str().unwrap().to_string();
    let full = render(dir, "full", &input, &["--hashes-out", &full_m_s], 2);
    assert!(full.len() > 50, "too few tiles: {}", full.len());

    // The hash is BLAKE3 of the uncompressed MVT, and the manifest lists every tile.
    let hashes: BTreeMap<Key, String> = full.iter().map(|(k, v)| (*k, blake3::hash(&gunzip(v)).to_hex().to_string())).collect();
    assert_eq!(fs::read_to_string(&full_m).unwrap(), manifest(hashes.clone()));

    // Targets: produced keys plus keys the full render does not produce.
    let keys: Vec<Key> = full.keys().copied().collect();
    let mut targets: Vec<Key> = keys.iter().step_by(5).copied().collect();
    let (z, x, y) = keys[keys.len() / 2];
    targets.extend([(z + 1, 2 * x, 2 * y), (11, x >> (z - 11), y >> (z - 11)), (17, x << (17 - z), y << (17 - z)), (z, x + 4000, y)]);
    targets.retain(|k| k.0 <= 24);
    let t_file = dir.join("targets");
    write_keys(&t_file, targets.iter().copied());
    let t_s = t_file.to_str().unwrap().to_string();
    for threads in [1, 3] {
        let m = dir.join(format!("sel{threads}.hashes"));
        let m_s = m.to_str().unwrap().to_string();
        let got = render(dir, &format!("sel{threads}"), &input, &["--only-tiles", &t_s, "--hashes-out", &m_s], threads);
        let want: BTreeMap<Key, Vec<u8>> = full.iter().filter(|(k, _)| targets.contains(k)).map(|(k, v)| (*k, v.clone())).collect();
        assert_eq!(got, want, "selected tiles differ (threads {threads})");
        assert_eq!(fs::read_to_string(&m).unwrap(), manifest(hashes.iter().filter(|(k, _)| targets.contains(k)).map(|(k, h)| (*k, h.clone()))));
    }

    // Known hashes: equal -> not written; wrong or missing -> written; manifest complete.
    let mut known: BTreeMap<Key, String> = BTreeMap::new();
    for (i, (k, h)) in hashes.iter().enumerate() {
        match i % 3 {
            0 => { known.insert(*k, h.clone()); }
            1 => { known.insert(*k, "0".repeat(64)); }
            _ => {}
        }
    }
    known.insert((16, 1, 1), "1".repeat(64)); // not produced
    let k_file = dir.join("known.hashes");
    fs::write(&k_file, manifest(known.clone())).unwrap();
    let k_s = k_file.to_str().unwrap().to_string();
    let m = dir.join("known-out.hashes");
    let m_s = m.to_str().unwrap().to_string();
    let got = render(dir, "known", &input, &["--known-hashes", &k_s, "--hashes-out", &m_s], 2);
    let want: BTreeMap<Key, Vec<u8>> = full.iter().filter(|(k, _)| known.get(k) != hashes.get(k)).map(|(k, v)| (*k, v.clone())).collect();
    assert_eq!(got, want);
    assert_eq!(fs::read_to_string(&m).unwrap(), fs::read_to_string(&full_m).unwrap());

    // All known: nothing written, the manifest is still complete.
    let m2 = dir.join("all-out.hashes");
    let m2_s = m2.to_str().unwrap().to_string();
    assert!(render(dir, "all", &input, &["--known-hashes", &full_m_s, "--hashes-out", &m2_s], 2).is_empty());
    assert_eq!(fs::read_to_string(&m2).unwrap(), fs::read_to_string(&full_m).unwrap());
}

#[test]
fn unsafe_option_sets_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let input = input(dir);
    let in_s = input.to_str().unwrap().to_string();
    let m = dir.join("m.hashes").to_str().unwrap().to_string();
    let out = dir.join("out").to_str().unwrap().to_string();
    let base: Vec<&str> = FLAGS.to_vec();
    let no_limits: Vec<&str> = FLAGS.iter().copied().filter(|f| !matches!(*f, "--no-feature-limit" | "--no-tile-size-limit")).collect();
    let cases: Vec<Vec<&str>> = vec![
        [&base[..], &["-at", "--hashes-out", &m]].concat(),
        [&base[..], &["--prefilter", "cat", "--hashes-out", &m]].concat(),
        [&base[..], &["--postfilter", "cat", "--hashes-out", &m]].concat(),
        [&base[..], &["--known-hashes", &m]].concat(),
        [&no_limits[..], &["--hashes-out", &m]].concat(),
    ];
    for mut args in cases {
        args.extend_from_slice(&["-e", &out, &in_s]);
        let r = run(&args, 2);
        assert_eq!(r.status.code(), Some(101), "{args:?}: {}", String::from_utf8_lossy(&r.stderr));
        assert!(!Path::new(&out).join("12").exists(), "{args:?} wrote tiles");
    }
}

#[test]
fn no_data_replaces_the_manifest_with_an_empty_one() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let empty = dir.join("empty.ndjson");
    fs::write(&empty, "").unwrap();
    let m = dir.join("m.hashes");
    fs::write(&m, format!("{HEADER}\n12/1/1 {}\n", "a".repeat(64))).unwrap();
    let m_s = m.to_str().unwrap().to_string();
    let out = dir.join("out").to_str().unwrap().to_string();
    let mut args: Vec<&str> = FLAGS.to_vec();
    args.extend_from_slice(&["--hashes-out", &m_s, "-e", &out, empty.to_str().unwrap()]);
    let r = run(&args, 2);
    assert_eq!(r.status.code(), Some(110), "{}", String::from_utf8_lossy(&r.stderr));
    assert_eq!(fs::read_to_string(&m).unwrap(), format!("{HEADER}\n"));
}
