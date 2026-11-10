//! `vtmake build --pgcopy-schema` on bad COPY streams: the run must fail and
//! write no tiles (for example, a stream that does not match the schema).
use std::path::{Path, PathBuf};
use std::process::Command;

const SCHEMA: &str = r#"{"format":"vtmake-pgcopy/1","layer":"t","columns":[
  {"name":"geom","type":"text","role":"geometry"},{"name":"i2","type":"int2"},{"name":"i4","type":"int4"},
  {"name":"i8","type":"int8"},{"name":"b","type":"bool"},{"name":"u","type":"uuid"},{"name":"n","type":"numeric"},{"name":"j","type":"jsonb"}]}"#;

fn field(d: &[u8]) -> Vec<u8> {
    let mut v = (d.len() as i32).to_be_bytes().to_vec();
    v.extend_from_slice(d);
    v
}

/// One good row: a point and one value of each type.
fn good_fields() -> Vec<Vec<u8>> {
    let numeric = [0u8, 1, 0, 0, 0, 0, 0, 0, 0, 7]; // ndigits 1, weight 0, sign +, dscale 0, digit 7
    vec![
        field(br#"{"type":"Point","coordinates":[1,2]}"#),
        field(&3i16.to_be_bytes()),
        field(&4i32.to_be_bytes()),
        field(&5i64.to_be_bytes()),
        field(&[1]),
        field(&[7u8; 16]),
        field(&numeric),
        field(b"\x01{\"k\": 1}"),
    ]
}

fn stream(rows: &[(i16, Vec<Vec<u8>>)], trailer: bool) -> Vec<u8> {
    let mut b = b"PGCOPY\n\xff\r\n\0".to_vec();
    b.extend_from_slice(&[0; 8]);
    for (n, fields) in rows {
        b.extend_from_slice(&n.to_be_bytes());
        for f in fields {
            b.extend_from_slice(f);
        }
    }
    if trailer {
        b.extend_from_slice(&(-1i16).to_be_bytes());
    }
    b
}

fn tiles(dir: &Path) -> usize {
    fn walk(d: &Path, n: &mut usize) {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, n)
                } else if p.extension().is_some_and(|x| x == "pbf") {
                    *n += 1
                }
            }
        }
    }
    let mut n = 0;
    walk(dir, &mut n);
    n
}

/// Run vtmake on `data`; (exit code, number of tiles).
fn run(name: &str, data: &[u8]) -> (i32, usize) {
    let dir: PathBuf = std::env::temp_dir().join(format!("vtmake-pgcopy-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("in.pgcopy"), data).unwrap();
    std::fs::write(dir.join("schema.json"), SCHEMA).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vtmake"))
        .current_dir(&dir)
        .env("TIPPECANOE_MAX_THREADS", "2")
        .args(["build", "-q", "-Z0", "-z2", "--force", "--no-feature-limit", "--no-tile-size-limit", "--pgcopy-schema", "schema.json", "-e", "tiles", "in.pgcopy"])
        .output()
        .unwrap();
    let r = (out.status.code().unwrap_or(-1), tiles(&dir.join("tiles")));
    let _ = std::fs::remove_dir_all(&dir);
    r
}

#[test]
fn a_good_stream_makes_tiles() {
    let (rc, n) = run("good", &stream(&[(8, good_fields()), (8, good_fields())], true));
    assert_eq!(rc, 0);
    assert!(n > 0);
}

#[test]
fn a_field_count_that_differs_from_the_schema_fails() {
    let mut fewer = good_fields();
    fewer.pop();
    let mut more = good_fields();
    more.push(field(b"x"));
    for (name, row) in [("fewer", (7, fewer)), ("more", (9, more))] {
        // The bad row is the second one: the first row is already decoded.
        let (rc, n) = run(name, &stream(&[(8, good_fields()), row], true));
        assert_eq!((rc, n), (107, 0), "{name}");
    }
}

#[test]
fn a_fixed_width_field_with_another_length_fails() {
    // For example an int4 column that became int8 or float8 after the describe.
    let cases: [(&str, usize, Vec<u8>); 7] = [
        ("int2-as-4", 1, 3i32.to_be_bytes().to_vec()),
        ("int4-as-8", 2, 4i64.to_be_bytes().to_vec()),
        ("int4-as-2", 2, 4i16.to_be_bytes().to_vec()),
        ("int8-as-4", 3, 5i32.to_be_bytes().to_vec()),
        ("bool-as-4", 4, 1i32.to_be_bytes().to_vec()),
        ("uuid-as-8", 5, vec![7; 8]),
        ("numeric-short", 6, vec![0, 2, 0, 0, 0, 0, 0, 0, 0, 7]),
    ];
    for (name, col, value) in cases {
        let mut row = good_fields();
        row[col] = field(&value);
        let (rc, n) = run(name, &stream(&[(8, good_fields()), (8, row)], true));
        assert_eq!((rc, n), (107, 0), "{name}");
    }
}

#[test]
fn a_cut_stream_fails() {
    let full = stream(&[(8, good_fields()), (8, good_fields())], true);
    for (name, data) in [("no-trailer", full[..full.len() - 2].to_vec()), ("cut-row", full[..full.len() - 9].to_vec()), ("after-trailer", [full.clone(), vec![0]].concat())] {
        assert_eq!(run(name, &data), (107, 0), "{name}");
    }
}
