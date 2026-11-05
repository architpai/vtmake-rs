#![cfg(feature = "reference")]
use vtmake_core::AttrType;
use vtmake_parse::{ParsedLine, dtoa::dtoa_milo, parse_feature};
unsafe extern "C" {
    fn bt_dtoa_reference(v: f64, out: *mut u8) -> usize;
    fn bt_attrs_reference(input: *const u8, out: *mut u8, cap: usize) -> usize;
}
#[test]
fn millions_of_doubles() {
    let count = std::env::var("VTMAKE_DOUBLE_COUNT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2_000_000);
    let mut seed = 0x1ab0387f341298cdu64;
    let mut buffer = [0; 128];
    for _ in 0..count {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let v = f64::from_bits(seed);
        let len = unsafe { bt_dtoa_reference(v, buffer.as_mut_ptr()) };
        assert_eq!(dtoa_milo(v).as_bytes(), &buffer[..len], "{v:?} {seed:x}");
    }
    eprintln!("dtoa: {count} doubles, 0 mismatches");
}
fn compare(line: &[u8]) {
    let mut parsed = ParsedLine::default();
    parse_feature(line, &mut parsed).unwrap();
    let Some(feature) = parsed.features.first() else {
        return;
    };
    let mut actual = Vec::new();
    for a in &feature.attrs {
        actual.extend_from_slice(a.key.as_bytes());
        actual.push(0);
        actual.push(match a.ty {
            AttrType::String => b's',
            AttrType::Number => b'n',
            AttrType::Bool => b'b',
            AttrType::Null => b'0',
        });
        actual.extend_from_slice(a.value.as_bytes());
        actual.push(0);
    }
    let input = std::ffi::CString::new(line).unwrap();
    let mut expected = vec![0; actual.len() + 4096];
    let n =
        unsafe { bt_attrs_reference(input.as_ptr().cast(), expected.as_mut_ptr(), expected.len()) };
    assert!(n <= expected.len());
    expected.truncate(n);
    assert_eq!(actual, expected);
}
#[test]
fn edge_attributes() {
    compare(br#"{"type":"Feature","properties":{"n":-0.0,"n":1e21,"safe":9007199254740993,"big":18446744073709551615,"lo":-9223372036854775808,"nested":{"a":"\n\t\u0000\u2600","x":[1e-7,null,true,false,1.234567890123456]}},"geometry":{"type":"Point","coordinates":[1,2]}}"#);
}
#[test]
#[ignore = "Set VTMAKE_CORPUS to a public NDJSON file"]
fn corpus_attributes() {
    use std::io::BufRead;
    let Ok(path) = std::env::var("VTMAKE_CORPUS") else {
        eprintln!("VTMAKE_CORPUS not set: skip");
        return;
    };
    let file = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let mut count = 0;
    for line in file.split(b'\n') {
        compare(&line.unwrap());
        count += 1;
    }
    eprintln!("attributes: {count} lines, 0 mismatches");
}
#[test]
fn double_boundaries() {
    let mut buffer = [0; 128];
    for v in [
        0.,
        -0.,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        f64::MAX,
        1e-7,
        1e-6,
        1e20,
        1e21,
        9007199254740992.,
    ] {
        let n = unsafe { bt_dtoa_reference(v, buffer.as_mut_ptr()) };
        assert_eq!(dtoa_milo(v).as_bytes(), &buffer[..n]);
    }
}
