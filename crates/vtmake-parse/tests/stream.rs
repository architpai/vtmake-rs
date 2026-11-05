use vtmake_parse::{ParsedLine, parse_feature, stream::StreamParser};
fn check(line: &[u8]) {
    let mut p = StreamParser::default();
    if p.parse(line) {
        let mut old = ParsedLine::default();
        parse_feature(line, &mut old).unwrap();
        assert!(old.warnings.is_empty());
        assert_eq!(old.features.len(), 1);
        for (a, b) in p.feature.geom.iter().zip(&old.features[0].geom) {
            assert_eq!(
                (a.lon.to_bits(), a.lat.to_bits()),
                (b.lon.to_bits(), b.lat.to_bits())
            );
        }
        let mut f = p.feature.clone();
        f.attrs = p
            .attrs
            .iter()
            .map(|a| vtmake_core::Attr {
                key: a.key.as_ref().into(),
                ty: a.ty,
                value: a.value.as_ref().into(),
            })
            .collect();
        assert_eq!(f, old.features[0]);
    }
}
#[test]
fn stream_and_fallback() {
    for kind in [
        "Point",
        "MultiPoint",
        "LineString",
        "MultiLineString",
        "Polygon",
        "MultiPolygon",
    ] {
        let depth = match kind {
            "Point" => 0,
            "MultiPoint" | "LineString" => 1,
            "MultiLineString" | "Polygon" => 2,
            _ => 3,
        };
        let coords = format!("{}[-0.0,1e-20]{}", "[".repeat(depth), "]".repeat(depth));
        let line = format!(
            r#"{{"type":"Feature","properties":{{"a":"x","a":"y","n":9007199254740993,"e":"a\nb","b":true,"z":null}},"geometry":{{"type":"{kind}","coordinates":{coords}}},"tippecanoe":{{"minzoom":0,"maxzoom":32,"layer":"x"}},"id":42}}"#
        );
        check(line.as_bytes());
        for i in 0..line.len() {
            check(&line.as_bytes()[..i]);
        }
        for i in 0..line.len() {
            let mut mutated = line.as_bytes().to_vec();
            mutated[i] = b' ';
            check(&mutated);
        }
    }
    for line in [
        r#"{"type":"Feature","properties":null,"geometry":{"type":"Polygon","coordinates":[]}}"#,
        r#"{"type":"Feature","properties":{"x":{"a":[1,true]}},"geometry":{"type":"Point","coordinates":[1,2,3]}}"#,
        r#"{"type":"Feature","properties":{},"geometry":{"coordinates":[1,2],"type":"Point"}}"#,
        r#"{"type":"Feature","properties":{},"geometry":null}"#,
        r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]},"type":"FeatureCollection"}"#,
    ] {
        check(line.as_bytes());
    }
}

#[test]
fn decimal_bits() {
    let mut seed = 0x123456789abcdef0u64;
    for _ in 0..100_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let digits = (seed % 999_999_999_999_999 + 1).to_string();
        let split = (seed as usize >> 16) % (digits.len() + 1);
        let raw = if split == 0 {
            format!("0.{digits}")
        } else if split == digits.len() {
            digits
        } else {
            format!("{}.{}", &digits[..split], &digits[split..])
        };
        let sign = if seed & 1 == 0 { "" } else { "-" };
        let line = format!(
            r#"{{"type":"Feature","properties":{{}},"geometry":{{"type":"Point","coordinates":[{sign}{raw},0]}}}}"#
        );
        check(line.as_bytes());
    }
}
