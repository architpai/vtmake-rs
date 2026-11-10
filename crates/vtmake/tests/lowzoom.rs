//! Ordered large-tile work against the sequential geometry path.
use std::{collections::BTreeMap, fs, path::Path, process::Command};

fn render(input: &Path, out: &Path, threads: usize, flags: &[&str]) -> BTreeMap<String, Vec<u8>> {
    let result = Command::new(env!("CARGO_BIN_EXE_vtmake"))
        .args(["build", "-q", "-f", "-z3", "-l", "test", "-n", "test", "--drop-densest-as-needed"])
        .args(flags).arg("-e").arg(out).arg(input)
        .env("TIPPECANOE_MAX_THREADS", threads.to_string()).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    fn visit(root: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() { visit(root, &path, files); }
            else if path.extension().is_some_and(|e| e == "pbf") {
                files.insert(path.strip_prefix(root).unwrap().to_str().unwrap().into(), fs::read(path).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(out, out, &mut files);
    assert!(!files.is_empty());
    // generator_options contains the output path. All other metadata must match.
    let mut metadata: serde_json::Value = serde_json::from_slice(&fs::read(out.join("metadata.json")).unwrap()).unwrap();
    metadata.as_object_mut().unwrap().remove("generator_options");
    files.insert("metadata".into(), serde_json::to_vec(&metadata).unwrap());
    files
}

#[test]
fn large_tiles_keep_feature_order_and_tiny_polygon_state() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.ndjson");
    let mut data = String::new();
    for i in 0..40000 {
        let x = -30.0 + (i % 200) as f64 * 0.3;
        let y = -20.0 + (i / 200) as f64 * 0.2;
        let size = if i % 3 == 0 { 0.001 } else { 0.5 };
        let points: Vec<_> = (0..=24).map(|j| {
            let a = (j % 24) as f64 * std::f64::consts::TAU / 24.;
            format!("[{},{}]", x + size * a.cos(), y + size * a.sin())
        }).collect();
        let extra = if i % 127 == 0 { r#","tippecanoe":{"minzoom":2}"# }
            else if i % 131 == 0 { r#","tippecanoe":{"maxzoom":1}"# } else { "" };
        data.push_str(&format!(r#"{{"type":"Feature","id":{i},"properties":{{"group":{},"value":{i}}},"geometry":{{"type":"Polygon","coordinates":[[{}]]}}{extra}}}
"#, i % 11, points.join(",")));
    }
    fs::write(&input, data).unwrap();
    for (case, flags) in [
        vec!["-M12000"],
        vec!["-M12000", "-pt"],
        vec!["-M12000", "-Evalue:sum"],
        vec!["-M12000", "--preserve-input-order"],
    ].iter().enumerate() {
        // This explicit default disables Geo::plain, selecting the original
        // clip path. It has the same simplification at the maximum zoom.
        let mut original = flags.clone();
        original.push("--simplification-at-maximum-zoom=1");
        let expected = render(&input, &dir.path().join(format!("ref-{case}")), 1, &original);
        for threads in [1, 2, 7] {
            let got = render(&input, &dir.path().join(format!("case-{case}-{threads}")), threads, flags);
            for (key, bytes) in &expected {
                assert!(got.get(key) == Some(bytes), "case {case}, threads {threads}, {key} differs");
            }
            assert_eq!(expected.len(), got.len());
        }
    }
}

#[test]
fn point_rate_drops_and_retries_are_ordered() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("points.ndjson");
    let data: String = (0..40000).map(|i| format!(r#"{{"type":"Feature","properties":{{"id":{i}}},"geometry":{{"type":"Point","coordinates":[{},{}]}}}}
"#, -30. + (i % 200) as f64 * 0.3, -20. + (i / 200) as f64 * 0.2)).collect();
    fs::write(&input, data).unwrap();
    let flags = ["-M10000", "--extend-zooms-if-still-dropping"];
    let original = [flags[0], flags[1], "--simplification-at-maximum-zoom=1"];
    let expected = render(&input, &dir.path().join("ref"), 1, &original);
    for threads in [1, 2, 7] {
        let got = render(&input, &dir.path().join(format!("points-{threads}")), threads, &flags);
        assert!(expected == got, "threads {threads}: point tiles or metadata differ");
    }
}
