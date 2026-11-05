use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use flate2::read::GzDecoder;
use prost::Message;
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Parser)]
#[command(about = "Compare decoded vector tiles on a 4096-unit grid")]
struct Args {
    a: PathBuf,
    b: PathBuf,
    #[arg(long)]
    json: Option<PathBuf>,
    #[arg(long)]
    match_key: Option<String>,
    #[arg(long, default_value_t = 1.0)]
    max_dist: f64,
    #[arg(long, default_value_t = 20)]
    list_limit: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct Coord {
    z: u32,
    x: u32,
    y: u32,
}
struct Tileset {
    path: PathBuf,
    tiles: BTreeSet<Coord>,
    metadata: Value,
    directory: bool,
}
fn connection(path: &Path) -> Result<Connection> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)
}
impl Tileset {
    fn open(path: PathBuf) -> Result<Self> {
        let directory = path.is_dir();
        let mut tiles = BTreeSet::new();
        let metadata = if directory {
            for entry in walkdir::WalkDir::new(&path) {
                let entry = entry?;
                if !entry.file_type().is_file()
                    || entry.path().extension().and_then(|s| s.to_str()) != Some("pbf")
                {
                    continue;
                }
                let rel = entry.path().strip_prefix(&path)?;
                let parts: Vec<_> = rel.iter().collect();
                if parts.len() != 3 {
                    continue;
                }
                let z = parts[0].to_str().context("bad z")?.parse()?;
                let x = parts[1].to_str().context("bad x")?.parse()?;
                let y = Path::new(parts[2])
                    .file_stem()
                    .context("bad y")?
                    .to_str()
                    .context("bad y")?
                    .parse()?;
                validate(Coord { z, x, y })?;
                tiles.insert(Coord { z, x, y });
            }
            if path.join("metadata.json").exists() {
                serde_json::from_slice(&fs::read(path.join("metadata.json"))?)?
            } else {
                json!({})
            }
        } else {
            let c = connection(&path)?;
            let mut stmt = c.prepare("SELECT zoom_level,tile_column,tile_row FROM tiles")?;
            for row in stmt.query_map([], |r| {
                Ok((
                    r.get::<_, u32>(0)?,
                    r.get::<_, u32>(1)?,
                    r.get::<_, u32>(2)?,
                ))
            })? {
                let (z, x, tms) = row?;
                ensure!(z <= 30 && tms < (1u32 << z), "invalid TMS coordinate");
                let coord = Coord {
                    z,
                    x,
                    y: (1 << z) - 1 - tms,
                };
                validate(coord)?;
                tiles.insert(coord);
            }
            let mut m = serde_json::Map::new();
            let mut stmt = c.prepare("SELECT name,value FROM metadata")?;
            for row in
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            {
                let (k, v) = row?;
                m.insert(k, Value::String(v));
            }
            Value::Object(m)
        };
        Ok(Self {
            path,
            tiles,
            metadata: normalize_metadata(metadata),
            directory,
        })
    }
    fn read(&self, c: &Option<Connection>, p: Coord) -> Result<Vec<u8>> {
        if self.directory {
            Ok(fs::read(
                self.path.join(format!("{}/{}/{}.pbf", p.z, p.x, p.y)),
            )?)
        } else {
            Ok(c.as_ref().context("missing connection")?.query_row("SELECT tile_data FROM tiles WHERE zoom_level=?1 AND tile_column=?2 AND tile_row=?3", (p.z,p.x,(1u32<<p.z)-1-p.y), |r|r.get(0))?)
        }
    }
    fn reader(&self) -> Result<Option<Connection>> {
        if self.directory {
            Ok(None)
        } else {
            Ok(Some(connection(&self.path)?))
        }
    }
}
fn validate(c: Coord) -> Result<()> {
    ensure!(
        c.z <= 30 && c.x < (1 << c.z) && c.y < (1 << c.z),
        "invalid XYZ coordinate"
    );
    Ok(())
}
fn normalize_metadata(mut m: Value) -> Value {
    if let Some(o) = m.as_object_mut() {
        for key in [
            "json",
            "vector_layers",
            "tilestats",
            "minzoom",
            "maxzoom",
            "bounds",
            "center",
        ] {
            if let Some(Value::String(s)) = o.get(key) {
                let parsed = serde_json::from_str::<Value>(s).ok().or_else(|| {
                    if key == "bounds" || key == "center" {
                        s.split(',')
                            .map(|n| n.parse::<f64>().ok().map(|v| json!(v)))
                            .collect::<Option<Vec<_>>>()
                            .map(Value::Array)
                    } else {
                        None
                    }
                });
                if let Some(v) = parsed {
                    o.insert(key.into(), v);
                }
            }
        }
    }
    m
}
#[derive(Clone, PartialEq, Message)]
struct Tile {
    #[prost(message, repeated, tag = "3")]
    layers: Vec<Layer>,
}
#[derive(Clone, PartialEq, Message)]
struct Layer {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(message, repeated, tag = "2")]
    features: Vec<Feature>,
    #[prost(string, repeated, tag = "3")]
    keys: Vec<String>,
    #[prost(message, repeated, tag = "4")]
    values: Vec<MvtValue>,
    #[prost(uint32, optional, tag = "5")]
    extent: Option<u32>,
}
#[derive(Clone, PartialEq, Message)]
struct Feature {
    #[prost(uint64, optional, tag = "1")]
    id: Option<u64>,
    #[prost(uint32, repeated, packed = "true", tag = "2")]
    tags: Vec<u32>,
    #[prost(uint32, tag = "3")]
    kind: u32,
    #[prost(uint32, repeated, packed = "true", tag = "4")]
    geometry: Vec<u32>,
}
#[derive(Clone, PartialEq, Message)]
struct MvtValue {
    #[prost(string, optional, tag = "1")]
    string: Option<String>,
    #[prost(float, optional, tag = "2")]
    float: Option<f32>,
    #[prost(double, optional, tag = "3")]
    double: Option<f64>,
    #[prost(int64, optional, tag = "4")]
    int: Option<i64>,
    #[prost(uint64, optional, tag = "5")]
    uint: Option<u64>,
    #[prost(sint64, optional, tag = "6")]
    sint: Option<i64>,
    #[prost(bool, optional, tag = "7")]
    boolean: Option<bool>,
}
impl MvtValue {
    fn json(&self) -> Value {
        if let Some(v) = &self.string {
            json!({"string":v})
        } else if let Some(v) = self.float {
            json!({"float":v})
        } else if let Some(v) = self.double {
            json!({"double":v})
        } else if let Some(v) = self.int {
            json!({"int":v})
        } else if let Some(v) = self.uint {
            json!({"uint":v})
        } else if let Some(v) = self.sint {
            json!({"sint":v})
        } else if let Some(v) = self.boolean {
            json!({"bool":v})
        } else {
            Value::Null
        }
    }
}
type Point = [f64; 2];
#[derive(Clone, Serialize)]
struct Decoded {
    id: Option<u64>,
    kind: u32,
    attributes: BTreeMap<String, Value>,
    paths: Vec<Vec<Point>>,
}
impl Decoded {
    fn vertices(&self) -> usize {
        self.paths.iter().map(Vec::len).sum()
    }
}
fn decode(bytes: &[u8]) -> Result<BTreeMap<String, Vec<Decoded>>> {
    let mut raw = Vec::new();
    let data = if bytes.starts_with(&[31, 139]) {
        GzDecoder::new(bytes)
            .take(512 * 1024 * 1024)
            .read_to_end(&mut raw)?;
        &raw[..]
    } else {
        bytes
    };
    let tile = Tile::decode(data)?;
    let mut layers = BTreeMap::new();
    for l in tile.layers {
        let extent = l.extent.unwrap_or(4096);
        ensure!(extent > 0, "zero extent");
        let mut features = Vec::new();
        for f in l.features {
            ensure!(f.tags.len() % 2 == 0, "odd tags");
            let mut attributes = BTreeMap::new();
            for tag in f.tags.as_chunks::<2>().0 {
                attributes.insert(
                    l.keys
                        .get(tag[0] as usize)
                        .context("invalid key index")?
                        .clone(),
                    l.values
                        .get(tag[1] as usize)
                        .context("invalid value index")?
                        .json(),
                );
            }
            features.push(Decoded {
                id: f.id,
                kind: f.kind,
                attributes,
                paths: geometry(&f.geometry, 4096.0 / extent as f64)?,
            });
        }
        ensure!(layers.insert(l.name, features).is_none(), "duplicate layer");
    }
    Ok(layers)
}
fn geometry(g: &[u32], scale: f64) -> Result<Vec<Vec<Point>>> {
    let (mut x, mut y) = (0i64, 0i64);
    let mut paths: Vec<Vec<Point>> = Vec::new();
    let mut i = 0;
    while i < g.len() {
        let cmd = g[i] & 7;
        let count = (g[i] >> 3) as usize;
        i += 1;
        ensure!(count > 0, "zero command count");
        match cmd {
            1 | 2 => {
                ensure!(count <= (g.len() - i) / 2, "truncated geometry");
                for _ in 0..count {
                    x += ((g[i] >> 1) as i64) ^ (-((g[i] & 1) as i64));
                    y += ((g[i + 1] >> 1) as i64) ^ (-((g[i + 1] & 1) as i64));
                    i += 2;
                    if cmd == 1 {
                        paths.push(Vec::new());
                    }
                    paths
                        .last_mut()
                        .context("line without move")?
                        .push([x as f64 * scale, y as f64 * scale]);
                }
            }
            7 => {
                ensure!(count == 1, "invalid close count");
                let p = paths.last_mut().context("close without move")?;
                if let Some(first) = p.first().copied() {
                    p.push(first);
                }
            }
            _ => bail!("unknown geometry command {cmd}"),
        }
    }
    Ok(paths)
}
fn distance(a: Point, b: Point) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}
fn segment(p: Point, a: Point, b: Point) -> f64 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let d = dx * dx + dy * dy;
    if d == 0.0 {
        return distance(p, a);
    }
    let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / d).clamp(0.0, 1.0);
    distance(p, [a[0] + t * dx, a[1] + t * dy])
}
fn directed(a: &Decoded, b: &Decoded) -> Vec<f64> {
    a.paths
        .iter()
        .flatten()
        .map(|&p| {
            let mut nearest = f64::INFINITY;
            for path in &b.paths {
                if b.kind == 1 || path.len() < 2 {
                    for &q in path {
                        nearest = nearest.min(distance(p, q));
                    }
                } else {
                    for w in path.windows(2) {
                        nearest = nearest.min(segment(p, w[0], w[1]));
                    }
                }
            }
            nearest
        })
        .collect()
}
fn area(f: &Decoded) -> f64 {
    f.paths
        .iter()
        .map(|p| {
            p.windows(2)
                .map(|w| w[0][0] * w[1][1] - w[1][0] * w[0][1])
                .sum::<f64>()
                / 2.0
        })
        .sum::<f64>()
        .abs()
}
fn metric(a: &Decoded, b: &Decoded) -> (f64, f64) {
    if a.paths == b.paths {
        return (0.0, 0.0);
    }
    let ds: Vec<_> = directed(a, b).into_iter().chain(directed(b, a)).collect();
    if ds.is_empty() {
        return (0.0, 0.0);
    }
    (
        ds.iter().copied().fold(0.0, f64::max),
        ds.iter().sum::<f64>() / ds.len() as f64,
    )
}
fn match_id(f: &Decoded, key: Option<&str>) -> Result<String> {
    if let Some(k) = key {
        Ok(serde_json::to_string(f.attributes.get(k).with_context(
            || format!("feature lacks match key {k}"),
        )?)?)
    } else {
        Ok(format!(
            "{}:{}",
            f.kind,
            serde_json::to_string(&f.attributes)?
        ))
    }
}
#[derive(Default, Serialize)]
struct Totals {
    layers_a: usize,
    layers_b: usize,
    features_a: usize,
    features_b: usize,
    vertices_a: usize,
    vertices_b: usize,
    matched: usize,
    unmatched_a: usize,
    unmatched_b: usize,
    attribute_changes: usize,
    geometry_type_changes: usize,
    max_distance: f64,
    mean_distance: f64,
    polygon_area_difference: f64,
}
#[derive(Serialize)]
struct TileReport {
    tile: Coord,
    byte_identical: bool,
    passed: bool,
    totals: Totals,
    layers: Vec<Value>,
}
fn compare_tile(p: Coord, a: Vec<u8>, b: Vec<u8>, args: &Args) -> Result<TileReport> {
    let same = a == b;
    let a = decode(&a)?;
    let b = if same { a.clone() } else { decode(&b)? };
    let mut totals = Totals {
        layers_a: a.len(),
        layers_b: b.len(),
        ..Default::default()
    };
    let mut reports = Vec::new();
    let mut passed = true;
    for name in a.keys().chain(b.keys()).collect::<BTreeSet<_>>() {
        let aa = a.get(name).map(Vec::as_slice).unwrap_or(&[]);
        let bb = b.get(name).map(Vec::as_slice).unwrap_or(&[]);
        let va: usize = aa.iter().map(Decoded::vertices).sum();
        let vb: usize = bb.iter().map(Decoded::vertices).sum();
        totals.features_a += aa.len();
        totals.features_b += bb.len();
        totals.vertices_a += va;
        totals.vertices_b += vb;
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, f) in bb.iter().enumerate() {
            groups
                .entry(match_id(f, args.match_key.as_deref())?)
                .or_default()
                .push(i);
        }
        let mut matches = Vec::new();
        let mut missing_a = Vec::new();
        let mut used = vec![false; bb.len()];
        for (i, f) in aa.iter().enumerate() {
            let candidates = groups.get_mut(&match_id(f, args.match_key.as_deref())?);
            let j = candidates.and_then(|c| {
                // Exact matches first. Otherwise use nearest first vertex, with stable ties.
                let pos = c
                    .iter()
                    .position(|&j| f.paths == bb[j].paths && f.kind == bb[j].kind)
                    .or_else(|| {
                        c.iter()
                            .enumerate()
                            .min_by(|(_, j), (_, k)| {
                                let anchor = |v: &Decoded| {
                                    v.paths
                                        .iter()
                                        .flatten()
                                        .next()
                                        .copied()
                                        .unwrap_or([0.0, 0.0])
                                };
                                distance(anchor(f), anchor(&bb[**j]))
                                    .total_cmp(&distance(anchor(f), anchor(&bb[**k])))
                            })
                            .map(|(pos, _)| pos)
                    });
                pos.map(|pos| c.remove(pos))
            });
            if let Some(j) = j {
                used[j] = true;
                let g = &bb[j];
                let (dist, mean) = metric(f, g);
                ensure!(dist.is_finite(), "empty/nonempty geometry mismatch");
                let area_diff = if f.kind == 3 && g.kind == 3 {
                    (area(f) - area(g)).abs()
                } else {
                    0.0
                };
                totals.matched += 1;
                totals.max_distance = totals.max_distance.max(dist);
                totals.mean_distance += mean;
                totals.polygon_area_difference += area_diff;
                totals.attribute_changes += usize::from(f.attributes != g.attributes);
                totals.geometry_type_changes += usize::from(f.kind != g.kind);
                passed &= dist <= args.max_dist && f.kind == g.kind && f.attributes == g.attributes;
                matches.push(json!({"a":i,"b":j,"id_a":f.id,"id_b":g.id,"type_a":f.kind,"type_b":g.kind,"attributes_a":f.attributes,"attributes_b":g.attributes,"vertices_a":f.vertices(),"vertices_b":g.vertices(),"max_distance":dist,"mean_distance":mean,"polygon_area_difference":area_diff}));
            } else {
                missing_a.push(json!({"index":i,"feature":f}));
                totals.unmatched_a += 1;
                passed = false;
            }
        }
        let missing_b: Vec<_> = bb
            .iter()
            .enumerate()
            .filter(|(i, _)| !used[*i])
            .map(|(i, f)| json!({"index":i,"feature":f}))
            .collect();
        totals.unmatched_b += missing_b.len();
        passed &= missing_b.is_empty() && a.contains_key(name) && b.contains_key(name);
        let keys = |ff: &[Decoded]| {
            ff.iter()
                .flat_map(|f| f.attributes.keys().cloned())
                .collect::<BTreeSet<_>>()
        };
        reports.push(json!({"name":name,"present_a":a.contains_key(name),"present_b":b.contains_key(name),"features_a":aa.len(),"features_b":bb.len(),"vertices_a":va,"vertices_b":vb,"keys_a":keys(aa),"keys_b":keys(bb),"matches":matches,"only_a":missing_a,"only_b":missing_b}));
    }
    if totals.matched > 0 {
        totals.mean_distance /= totals.matched as f64;
    }
    Ok(TileReport {
        tile: p,
        byte_identical: same,
        passed,
        totals,
        layers: reports,
    })
}
fn run() -> Result<bool> {
    let args = Args::parse();
    ensure!(
        args.max_dist.is_finite() && args.max_dist >= 0.0,
        "max-dist must be finite and nonnegative"
    );
    let a = Tileset::open(args.a.clone())?;
    let b = Tileset::open(args.b.clone())?;
    let only_a: Vec<_> = a.tiles.difference(&b.tiles).copied().collect();
    let only_b: Vec<_> = b.tiles.difference(&a.tiles).copied().collect();
    let common: Vec<_> = a.tiles.intersection(&b.tiles).copied().collect();
    let union: Vec<_> = a.tiles.union(&b.tiles).copied().collect();
    let writer = if let Some(path) = &args.json {
        ensure!(
            path != &args.a && path != &args.b,
            "JSON output must not replace an input"
        );
        let mut w = std::io::BufWriter::new(fs::File::create(path)?);
        w.write_all(b"{\"tiles\":[")?;
        Some(Mutex::new((w, true)))
    } else {
        None
    };
    // Each parallel chunk has its own SQLite readers. Serialize details as tiles
    // finish, then discard them. Memory is bounded by active chunks, not tileset size.
    let chunks: Vec<Result<Vec<TileReport>>> = union
        .par_chunks(32)
        .map(|chunk| {
            let ca = a.reader()?;
            let cb = b.reader()?;
            let mut results = Vec::new();
            for &p in chunk {
                let bytes_a = if a.tiles.contains(&p) {
                    a.read(&ca, p)?
                } else {
                    Vec::new()
                };
                let bytes_b = if b.tiles.contains(&p) {
                    b.read(&cb, p)?
                } else {
                    Vec::new()
                };
                let mut r = compare_tile(p, bytes_a, bytes_b, &args)
                    .with_context(|| format!("tile {}/{}/{}", p.z, p.x, p.y))?;
                r.passed &= a.tiles.contains(&p) && b.tiles.contains(&p);
                r.byte_identical &= a.tiles.contains(&p) && b.tiles.contains(&p);
                if let Some(writer) = &writer {
                    let bytes = serde_json::to_vec(&r)?;
                    let mut guard = writer
                        .lock()
                        .map_err(|_| anyhow::anyhow!("report writer poisoned"))?;
                    if !guard.1 {
                        guard.0.write_all(b",")?;
                    }
                    guard.0.write_all(&bytes)?;
                    guard.1 = false;
                }
                for layer in &mut r.layers {
                    if let Some(o) = layer.as_object_mut() {
                        o.remove("matches");
                        o.remove("only_a");
                        o.remove("only_b");
                    }
                }
                results.push(r);
            }
            Ok(results)
        })
        .collect();
    let mut tiles = Vec::new();
    for chunk in chunks {
        tiles.extend(chunk?);
    }
    let semantic = |m: &Value| {
        let mut v = serde_json::Map::new();
        for key in [
            "minzoom",
            "maxzoom",
            "bounds",
            "center",
            "vector_layers",
            "tilestats",
        ] {
            if let Some(value) = m
                .get(key)
                .or_else(|| m.get("json").and_then(|j| j.get(key)))
            {
                v.insert(key.into(), value.clone());
            }
        }
        // Preserve additional JSON metadata as well as the two standard sections.
        if let Some(j) = m.get("json").and_then(Value::as_object) {
            for (k, val) in j {
                v.insert(k.clone(), val.clone());
            }
        }
        Value::Object(v)
    };
    let semantic_a = semantic(&a.metadata);
    let semantic_b = semantic(&b.metadata);
    let metadata_equal = semantic_a == semantic_b;
    let metadata_differences: Vec<_> = semantic_a
        .as_object()
        .unwrap()
        .keys()
        .chain(semantic_b.as_object().unwrap().keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|k| semantic_a.get(*k) != semantic_b.get(*k))
        .map(|k| json!({"key":k,"a":semantic_a.get(k),"b":semantic_b.get(k)}))
        .collect();
    for d in &metadata_differences {
        println!("Metadata differs: {}", d["key"]);
    }

    let passed =
        only_a.is_empty() && only_b.is_empty() && metadata_equal && tiles.iter().all(|t| t.passed);
    let identical = tiles.iter().filter(|t| t.byte_identical).count();
    let mut layer_totals: BTreeMap<String, Value> = BTreeMap::new();
    for tile in &tiles {
        for l in &tile.layers {
            let entry=layer_totals.entry(l["name"].as_str().unwrap().into()).or_insert_with(||json!({"features_a":0,"features_b":0,"vertices_a":0,"vertices_b":0,"keys_a":[],"keys_b":[]}));
            for k in ["features_a", "features_b", "vertices_a", "vertices_b"] {
                entry[k] = json!(entry[k].as_u64().unwrap() + l[k].as_u64().unwrap());
            }
            for k in ["keys_a", "keys_b"] {
                let keys: BTreeSet<String> = entry[k]
                    .as_array()
                    .unwrap()
                    .iter()
                    .chain(l[k].as_array().unwrap())
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect();
                entry[k] = json!(keys);
            }
        }
    }
    let mut totals = Totals::default();
    for t in &tiles {
        let v = &t.totals;
        totals.layers_a += v.layers_a;
        totals.layers_b += v.layers_b;
        totals.features_a += v.features_a;
        totals.features_b += v.features_b;
        totals.vertices_a += v.vertices_a;
        totals.vertices_b += v.vertices_b;
        totals.matched += v.matched;
        totals.unmatched_a += v.unmatched_a;
        totals.unmatched_b += v.unmatched_b;
        totals.attribute_changes += v.attribute_changes;
        totals.geometry_type_changes += v.geometry_type_changes;
        totals.max_distance = totals.max_distance.max(v.max_distance);
        totals.mean_distance += v.mean_distance * v.matched as f64;
        totals.polygon_area_difference += v.polygon_area_difference;
    }
    if totals.matched > 0 {
        totals.mean_distance /= totals.matched as f64;
    }
    println!(
        "Tiles A={} B={} common={} byte-identical={} only-A={} only-B={}",
        a.tiles.len(),
        b.tiles.len(),
        common.len(),
        identical,
        only_a.len(),
        only_b.len()
    );
    println!(
        "Only A (first {}): {:?}\nOnly B (first {}): {:?}",
        args.list_limit,
        only_a.iter().take(args.list_limit).collect::<Vec<_>>(),
        args.list_limit,
        only_b.iter().take(args.list_limit).collect::<Vec<_>>()
    );
    println!(
        "Features A={} B={} unmatched A={} B={} max distance={:.4} mean={:.4} area difference={:.4}; metadata equal={}; {}",
        totals.features_a,
        totals.features_b,
        totals.unmatched_a,
        totals.unmatched_b,
        totals.max_distance,
        totals.mean_distance,
        totals.polygon_area_difference,
        metadata_equal,
        if passed { "PASS" } else { "FAIL" }
    );
    println!(
        "Layer occurrences A={} B={}; vertices A={} B={}; attribute changes={}; type changes={}",
        totals.layers_a,
        totals.layers_b,
        totals.vertices_a,
        totals.vertices_b,
        totals.attribute_changes,
        totals.geometry_type_changes
    );
    for (name, v) in &layer_totals {
        println!(
            "Layer {name}: features A={} B={}, vertices A={} B={}",
            v["features_a"], v["features_b"], v["vertices_a"], v["vertices_b"]
        );
    }
    for t in tiles.iter().filter(|t| !t.passed).take(args.list_limit) {
        println!(
            "Changed {}/{}/{}: unmatched A={} B={}, max distance={:.4}",
            t.tile.z,
            t.tile.x,
            t.tile.y,
            t.totals.unmatched_a,
            t.totals.unmatched_b,
            t.totals.max_distance
        );
    }
    if let Some(writer) = writer {
        let (mut w, _) = writer
            .into_inner()
            .map_err(|_| anyhow::anyhow!("report writer poisoned"))?;
        let suffix = json!({"version":1,"passed":passed,"max_dist":args.max_dist,"match_key":args.match_key,"tile_sets":{"a":a.tiles.len(),"b":b.tiles.len(),"byte_identical":identical,"only_a":only_a,"only_b":only_b},"metadata":{"equal":metadata_equal,"all_equal":a.metadata==b.metadata,"differences":metadata_differences,"a":a.metadata,"b":b.metadata},"totals":totals,"layer_totals":layer_totals});
        w.write_all(b"],")?;
        let bytes = serde_json::to_vec(&suffix)?;
        w.write_all(&bytes[1..])?;
        w.flush()?;
    }
    Ok(passed)
}
fn main() {
    match run() {
        Ok(true) => (),
        Ok(false) => std::process::exit(1),
        Err(e) => {
            eprintln!("{e:#}");
            std::process::exit(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tile(x: u32, extent: u32, kind: u32) -> Vec<u8> {
        Tile {
            layers: vec![Layer {
                name: "test".into(),
                features: vec![Feature {
                    id: Some(1),
                    tags: vec![0, 0],
                    kind,
                    geometry: vec![9, x * 2, 20],
                }],
                keys: vec!["id".into()],
                values: vec![MvtValue {
                    uint: Some(1),
                    ..Default::default()
                }],
                extent: Some(extent),
            }],
        }
        .encode_to_vec()
    }
    fn args() -> Args {
        Args {
            a: "".into(),
            b: "".into(),
            json: None,
            match_key: None,
            max_dist: 1.0,
            list_limit: 20,
        }
    }
    #[test]
    fn identical_and_shift() {
        let p = Coord { z: 0, x: 0, y: 0 };
        assert!(
            compare_tile(p, tile(10, 4096, 1), tile(10, 4096, 1), &args())
                .unwrap()
                .passed
        );
        let r = compare_tile(p, tile(10, 4096, 1), tile(13, 4096, 1), &args()).unwrap();
        assert!(!r.passed);
        assert_eq!(r.totals.max_distance, 3.0);
    }
    #[test]
    fn extent_normalized() {
        let f = decode(&tile(10, 2048, 1)).unwrap();
        assert_eq!(f["test"][0].paths[0][0], [20.0, 20.0]);
    }
    #[test]
    fn type_change_with_key() {
        let mut a = args();
        a.match_key = Some("id".into());
        let r = compare_tile(
            Coord { z: 0, x: 0, y: 0 },
            tile(10, 4096, 1),
            tile(10, 4096, 2),
            &a,
        )
        .unwrap();
        assert_eq!(r.totals.geometry_type_changes, 1);
        assert!(!r.passed);
    }
    #[test]
    fn segments_and_area() {
        assert_eq!(segment([5.0, 3.0], [0.0, 0.0], [10.0, 0.0]), 3.0);
        let f = Decoded {
            id: None,
            kind: 3,
            attributes: BTreeMap::new(),
            paths: vec![vec![[0., 0.], [10., 0.], [10., 10.], [0., 10.], [0., 0.]]],
        };
        assert_eq!(area(&f), 100.0);
        assert_eq!(metric(&f, &f), (0.0, 0.0));
    }
    #[test]
    fn invalid_geometry() {
        assert!(geometry(&[9, 2], 1.).is_err());
        assert!(geometry(&[0], 1.).is_err());
        assert!(geometry(&[10, 0, 0], 1.).is_err());
    }
    #[test]
    fn gzip() {
        use std::io::Write;
        let raw = tile(2, 4096, 1);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&raw).unwrap();
        assert_eq!(
            decode(&raw).unwrap()["test"][0].paths,
            decode(&gz.finish().unwrap()).unwrap()["test"][0].paths
        );
    }
    #[test]
    fn missing_layer_and_attributes() {
        let p = Coord { z: 0, x: 0, y: 0 };
        let r = compare_tile(p, tile(10, 4096, 1), Vec::new(), &args()).unwrap();
        assert!(!r.passed);
        assert_eq!(r.totals.unmatched_a, 1);
        let mut changed = Tile::decode(tile(10, 4096, 1).as_slice()).unwrap();
        changed.layers[0].values[0].uint = Some(2);
        let r = compare_tile(p, tile(10, 4096, 1), changed.encode_to_vec(), &args()).unwrap();
        assert!(!r.passed);
        assert_eq!(r.totals.unmatched_a, 1);
        assert_eq!(r.totals.unmatched_b, 1);
    }
    #[test]
    fn holes_and_multipart_cursor() {
        let f = Decoded {
            id: None,
            kind: 3,
            attributes: BTreeMap::new(),
            paths: vec![
                vec![[0., 0.], [10., 0.], [10., 10.], [0., 10.], [0., 0.]],
                vec![[2., 2.], [2., 8.], [8., 8.], [8., 2.], [2., 2.]],
            ],
        };
        assert_eq!(area(&f), 64.0);
        assert_eq!(
            geometry(&[9, 20, 20, 9, 2, 4], 1.0).unwrap(),
            vec![vec![[10., 10.]], vec![[11., 12.]]]
        );
    }
    #[test]
    fn feature_order_ignored() {
        let mut a = Tile::decode(tile(10, 4096, 1).as_slice()).unwrap();
        let mut f = a.layers[0].features[0].clone();
        f.geometry[1] = 40;
        a.layers[0].features.push(f);
        let mut b = a.clone();
        b.layers[0].features.reverse();
        let r = compare_tile(
            Coord { z: 0, x: 0, y: 0 },
            a.encode_to_vec(),
            b.encode_to_vec(),
            &args(),
        )
        .unwrap();
        assert!(r.passed);
        assert_eq!(r.totals.matched, 2);
    }
    #[test]
    fn line_distance_uses_segments() {
        let f = |points| Decoded {
            id: None,
            kind: 2,
            attributes: BTreeMap::new(),
            paths: vec![points],
        };
        let a = f(vec![[0., 0.], [10., 0.]]);
        let b = f(vec![[0., 3.], [5., 3.], [10., 3.]]);
        assert_eq!(metric(&a, &b), (3., 3.));
        let c = f(vec![[0., 0.], [5., 0.], [10., 0.]]);
        assert_eq!(metric(&a, &c), (0., 0.));
    }
    #[test]
    fn mbtiles_tms_reader() {
        let path = std::env::temp_dir().join(format!(
            "vtmake-compare-test-{}-{}.mbtiles",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let c = Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE tiles(zoom_level,tile_column,tile_row,tile_data);CREATE TABLE metadata(name,value);").unwrap();
        let bytes = tile(10, 4096, 1);
        c.execute("INSERT INTO tiles VALUES(1,1,1,?1)", [&bytes])
            .unwrap();
        drop(c);
        let set = Tileset::open(path.clone()).unwrap();
        let coord = Coord { z: 1, x: 1, y: 0 };
        assert!(set.tiles.contains(&coord));
        assert_eq!(set.read(&set.reader().unwrap(), coord).unwrap(), bytes);
        drop(set);
        fs::remove_file(path).unwrap();
    }
}
