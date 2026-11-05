use vtmake_parse::*;
fn parse(s: &str) -> ParsedLine {
    let mut out = ParsedLine::default();
    parse_feature(s.as_bytes(), &mut out).unwrap();
    out
}
#[test]
fn properties_and_collections() {
    assert!(
        parse(r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]}}"#)
            .warnings
            .contains(&Warning::MissingProperties)
    );
    let out = parse(
        r#"{"type":"Feature","properties":null,"id":18446744073709551615,"tippecanoe":{"minzoom":0,"maxzoom":32,"layer":"test"},"geometry":{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[1,2,3]},{"type":"Polygon","coordinates":[[[1,2],[2,3],[3,2],[1,2]],[[1,2],[2,2],[1,2]]]}]}}"#,
    );
    assert_eq!(out.features.len(), 2);
    assert_eq!(out.features[0].id, Some(u64::MAX));
    assert_eq!(out.features[0].maxzoom, 32);
    assert_eq!(out.features[1].geom.iter().filter(|d| d.op == 7).count(), 1);
    assert!(out.warnings.contains(&Warning::ExtraDimensions));
}
#[test]
fn errors_and_order() {
    let mut out = ParsedLine::default();
    for s in [
        r#"{"type":"Feature","properties":{},"tippecanoe":{"minzoom":1.5},"geometry":{"type":"Point","coordinates":[1,2]}}"#,
        r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1]}}"#,
    ] {
        assert_eq!(
            parse_feature(s.as_bytes(), &mut out).unwrap_err().exit_code,
            107
        );
    }
    assert_eq!(parse_feature(&[255], &mut out).unwrap_err().exit_code, 119);
    assert_eq!(parse_parallel(b"\n", 2).unwrap_err().exit_code, 110);
    let line=b"{\"type\":\"Feature\",\"properties\":{\"a\":1,\"a\":2},\"geometry\":{\"type\":\"Point\",\"coordinates\":[1,2]}}\n";
    let data = line.repeat(127);
    let a = parse_parallel(&data, 1).unwrap();
    let b = parse_parallel(&data, 30).unwrap();
    assert_eq!(a.features, b.features);
    assert_eq!(a.features[1].seq, line.len() as u64);
    assert_eq!(a.features[0].attrs.len(), 2);
}
#[test]
fn skipped_geometry_does_not_check_zoom() {
    let out =
        parse(r#"{"type":"Feature","properties":{},"tippecanoe":{"minzoom":1.2},"geometry":null}"#);
    assert!(out.features.is_empty());
    assert!(out.warnings.contains(&Warning::MissingType));
    let out = parse(
        r#"{"type":"FeatureCollection","crs":{"properties":{"name":"urn:ogc:def:crs:OGC:1.3:CRS84"}},"features":[]}"#,
    );
    assert!(out.warnings.is_empty());
}
#[test]
fn empty_polygon_is_not_a_valid_geometry() {
    let input =
        br#"{"type":"Feature","properties":null,"geometry":{"type":"Polygon","coordinates":[]}}"#;
    assert_eq!(parse_parallel(input, 1).unwrap_err().exit_code, 110);
}
