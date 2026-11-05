//! `vtmake decode`: a port of `tippecanoe-decode` (`decode.cpp`) and the
//! GeoJSON writer that it uses (`write_json.cpp:layer_to_geojson`). The output
//! text is byte-identical to felt's tool.
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

use crate::common::*;
use crate::getopt::{self, Long, Short};
use crate::jsonw::JsonWriter;
use crate::mvtdec::{self, CLOSEPATH, LINETO, Layer, MOVETO, Value};
use rusqlite::Connection;
use std::collections::BTreeSet;

/// The output projection (`projection.cpp:projections`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Projection {
    Epsg4326,
    Epsg3857,
}

impl Projection {
    fn alias(self) -> &'static str {
        match self {
            Projection::Epsg4326 => "urn:ogc:def:crs:OGC:1.3:CRS84",
            Projection::Epsg3857 => "urn:ogc:def:crs:EPSG::3857",
        }
    }

    /// `unproject(wx, wy, 32)`: world coordinates to (x, y) in the projection.
    fn unproject(self, wx: i64, wy: i64) -> (f64, f64) {
        match self {
            Projection::Epsg4326 => vtmake_geom::tile2lonlat(wx, wy, 32),
            Projection::Epsg3857 => {
                // `tiletoepsg3857` with zoom 32: no shift.
                let ox = (wx.wrapping_sub(1 << 31)) as f64 * std::f64::consts::PI * 6378137.0 / (1i64 << 31) as f64;
                let oy = ((1i64 << 32) - 1 - wy - (1i64 << 31)) as f64 * std::f64::consts::PI * 6378137.0 / (1i64 << 31) as f64;
                (ox, oy)
            }
        }
    }
}

struct Opts {
    minzoom: i32,
    maxzoom: i32,
    force: bool,
    include_attr: BTreeSet<Vec<u8>>,
    to_decode: BTreeSet<Vec<u8>>,
    pipeline: bool,
    stats: bool,
    exclude_meta: BTreeSet<Vec<u8>>,
    /// 0: projected, 1: fraction of the extent (`-F`), 2: tile integers (`-I`).
    coordinate_mode: i32,
    projection: Projection,
}

/// The state of the decoder (the static `warned` flags of `layer_to_geojson`).
struct Dec<'o> {
    o: &'o Opts,
    warned_ring: bool,
    warned_inner: bool,
}

#[derive(Clone, Copy)]
struct LonLat {
    op: u32,
    lon: f64,
    lat: f64,
    x: i64,
    y: i64,
}

fn write_coords(w: &mut JsonWriter, ll: &LonLat, scale: f64) {
    if scale == 0.0 {
        w.write_float(ll.lon);
        w.write_float(ll.lat);
    } else {
        w.write_number(ll.x as f64 / scale);
        w.write_number(ll.y as f64 / scale);
    }
}

impl Dec<'_> {
    /// `layer_to_geojson` with the arguments of `decode.cpp`: no dropped flag,
    /// index, sequence or extent.
    #[allow(clippy::too_many_arguments)]
    fn layer_to_geojson(&mut self, w: &mut JsonWriter, layer: &Layer, z: u32, x: u32, y: u32, comma: bool, name: bool, zoom: bool, complain: bool, scale: f64) -> Result<(), Fatal> {
        let mut ops: Vec<LonLat> = Vec::new();
        for feat in &layer.features {
            w.write_hash();
            w.write_str("type");
            w.write_str("Feature");

            if let Some(id) = feat.id {
                w.write_str("id");
                w.write_unsigned(id);
            }

            if name || zoom {
                w.write_str("tippecanoe");
                w.write_hash();
                if name {
                    w.write_str("layer");
                    w.write_string(&layer.name);
                }
                if zoom {
                    w.write_str("minzoom");
                    w.write_unsigned(z as u64);
                    w.write_str("maxzoom");
                    w.write_unsigned(z as u64);
                }
                w.end_hash();
            }

            w.write_str("properties");
            w.write_hash();
            let mut t = 0;
            while t + 1 < feat.tags.len() {
                let (k, v) = (feat.tags[t] as usize, feat.tags[t + 1] as usize);
                t += 2;
                if k >= layer.keys.len() {
                    return fatal(format!("Error: out of bounds feature key ({k} in {})", layer.keys.len()), EXIT_IMPOSSIBLE);
                }
                if v >= layer.values.len() {
                    return fatal(format!("Error: out of bounds feature value ({v} in {})", layer.values.len()), EXIT_IMPOSSIBLE);
                }
                if !self.o.include_attr.is_empty() && !self.o.include_attr.contains(&layer.keys[k]) {
                    continue;
                }
                // The key is used as a C string.
                w.write_string(cstr(&layer.keys[k]));
                match &layer.values[v] {
                    Value::String(s) => w.write_string(s),
                    Value::Int(i) | Value::Sint(i) => w.write_signed(*i),
                    Value::Double(d) => w.write_number(*d),
                    Value::Float(f) => w.write_number(*f as f64),
                    Value::Uint(u) => w.write_unsigned(*u),
                    Value::Bool(b) => w.write_bool(*b),
                    Value::Null => w.write_null(),
                }
            }
            w.end_hash();

            w.write_str("geometry");
            w.write_hash();

            ops.clear();
            let extent = layer.extent as i64;
            for g in &feat.geometry {
                if g.op == MOVETO || g.op == LINETO {
                    let wscale = 1i64.wrapping_shl(32u32.wrapping_sub(z));
                    let wx = wscale.wrapping_mul(x as i64).wrapping_add((wscale / extent).wrapping_mul(g.x));
                    let wy = wscale.wrapping_mul(y as i64).wrapping_add((wscale / extent).wrapping_mul(g.y));
                    let (lon, lat) = self.o.projection.unproject(wx, wy);
                    ops.push(LonLat { op: g.op, lon, lat, x: g.x, y: g.y });
                } else {
                    ops.push(LonLat { op: g.op, lon: 0.0, lat: 0.0, x: 0, y: 0 });
                }
            }

            match feat.ty {
                1 => {
                    if ops.len() == 1 {
                        w.write_str("type");
                        w.write_str("Point");
                        w.write_str("coordinates");
                        w.write_array();
                        write_coords(w, &ops[0], scale);
                        w.end_array();
                    } else {
                        w.write_str("type");
                        w.write_str("MultiPoint");
                        w.write_str("coordinates");
                        w.write_array();
                        for o in &ops {
                            w.write_array();
                            write_coords(w, o, scale);
                            w.end_array();
                        }
                        w.end_array();
                    }
                }
                2 => {
                    let movetos = ops.iter().filter(|o| o.op == MOVETO).count();
                    if movetos < 2 {
                        w.write_str("type");
                        w.write_str("LineString");
                        w.write_str("coordinates");
                        w.write_array();
                        for o in &ops {
                            w.write_array();
                            write_coords(w, o, scale);
                            w.end_array();
                        }
                        w.end_array();
                    } else {
                        w.write_str("type");
                        w.write_str("MultiLineString");
                        w.write_str("coordinates");
                        w.write_array();
                        w.write_array();
                        let mut sstate = 0;
                        for o in &ops {
                            if o.op == MOVETO && sstate != 0 {
                                w.end_array();
                                w.write_array();
                            }
                            if o.op == MOVETO {
                                sstate = 1;
                            }
                            w.write_array();
                            write_coords(w, o, scale);
                            w.end_array();
                        }
                        w.end_array();
                        w.end_array();
                    }
                }
                3 => self.polygon(w, &ops, complain, scale)?,
                _ => {}
            }

            w.end_hash();
            w.end_hash();

            if comma {
                w.write_newline();
                w.comma_newline();
            }
            w.maybe_flush();
        }
        Ok(())
    }

    fn polygon(&mut self, w: &mut JsonWriter, ops: &[LonLat], complain: bool, scale: f64) -> Result<(), Fatal> {
        let mut rings: Vec<Vec<LonLat>> = Vec::new();
        for (i, o) in ops.iter().enumerate() {
            if o.op == MOVETO {
                rings.push(Vec::new());
            }
            if let Some(r) = rings.last_mut() {
                if o.op == CLOSEPATH {
                    let first = r[0];
                    r.push(first);
                } else {
                    r.push(*o);
                }
            }
            if (i + 1 >= ops.len() || ops[i + 1].op == MOVETO) && o.op != CLOSEPATH && !self.warned_ring {
                eprintln!("Ring does not end with closepath (ends with {})", o.op);
                if complain {
                    return fatal("", EXIT_IMPOSSIBLE);
                }
                self.warned_ring = true;
            }
        }

        let mut outer = 0;
        let mut areas = Vec::with_capacity(rings.len());
        for (i, r) in rings.iter().enumerate() {
            let mut area = 0.0f64;
            for k in 0..r.len() {
                if r[k].op != CLOSEPATH {
                    let n = &r[(k + 1) % r.len()];
                    area += r[k].x as f64 * n.y as f64;
                    area -= r[k].y as f64 * n.x as f64;
                }
            }
            area /= 2.0;
            areas.push(area);
            if area >= 0.0 || i == 0 {
                outer += 1;
            }
        }

        w.write_str("type");
        if outer > 1 {
            w.write_str("MultiPolygon");
            w.write_str("coordinates");
            w.write_array();
            w.write_array();
            w.write_array();
        } else {
            w.write_str("Polygon");
            w.write_str("coordinates");
            w.write_array();
            w.write_array();
        }

        let mut sstate = 0;
        for (i, r) in rings.iter().enumerate() {
            if i == 0 && areas[i] < 0.0 && !self.warned_inner {
                eprintln!("Polygon begins with an inner ring");
                if complain {
                    return fatal("", EXIT_IMPOSSIBLE);
                }
                self.warned_inner = true;
            }
            if areas[i] >= 0.0 {
                if sstate != 0 {
                    // A new polygon of the multipolygon.
                    w.end_array();
                    w.end_array();
                    w.write_array();
                    w.write_array();
                }
                sstate = 1;
            }
            if sstate == 2 {
                // A new ring of the same polygon.
                w.end_array();
                w.write_array();
            }
            for p in r {
                w.write_array();
                write_coords(w, p, scale);
                w.end_array();
            }
            sstate = 2;
        }

        w.end_array();
        w.end_array();
        if outer > 1 {
            w.end_array();
        }
        Ok(())
    }

    fn do_stats(w: &mut JsonWriter, layers: &[Layer], size: usize, compressed: bool, z: i32, x: u32, y: u32) {
        w.write_hash();
        w.write_str("zoom");
        w.write_signed(z as i64);
        w.write_str("x");
        w.write_unsigned(x as u64);
        w.write_str("y");
        w.write_unsigned(y as u64);
        w.write_str("bytes");
        w.write_unsigned(size as u64);
        w.write_str("compressed");
        w.write_bool(compressed);
        w.write_str("layers");
        w.write_hash();
        for l in layers {
            w.write_string(&l.name);
            let count = |t: i32| l.features.iter().filter(|f| f.ty == t).count() as u64;
            w.write_hash();
            w.write_str("points");
            w.write_unsigned(count(1));
            w.write_str("lines");
            w.write_unsigned(count(2));
            w.write_str("polygons");
            w.write_unsigned(count(3));
            w.write_str("extent");
            w.write_signed(l.extent as i64);
            w.end_hash();
        }
        w.end_hash();
        w.end_hash();
        w.write_newline();
    }

    /// `handle`: decode one tile and write it.
    fn handle(&mut self, w: &mut JsonWriter, message: &[u8], z: i32, x: u32, y: u32) -> Result<(), Fatal> {
        let (layers, was_compressed) = match mvtdec::decode(message) {
            Ok(v) => v,
            Err(mvtdec::Error::Decompress) => return fatal("Decompression error: data error", EXIT_MVT),
            Err(mvtdec::Error::Pbf) => return fatal(format!("PBF decoding error in tile {z}/{x}/{y}"), EXIT_PROTOBUF),
        };
        let o = self.o;
        if o.stats {
            Self::do_stats(w, &layers, message.len(), was_compressed, z, x, y);
            return Ok(());
        }

        if !o.pipeline {
            w.write_hash();
            w.write_str("type");
            w.write_str("FeatureCollection");
            w.write_str("properties");
            w.write_hash();
            w.write_str("zoom");
            w.write_signed(z as i64);
            w.write_str("x");
            w.write_signed(x as i64);
            w.write_str("y");
            w.write_signed(y as i64);
            if !was_compressed {
                w.write_str("compressed");
                w.write_bool(false);
            }
            w.end_hash();
            if o.projection != Projection::Epsg4326 {
                w.write_str("crs");
                w.write_hash();
                w.write_str("type");
                w.write_str("name");
                w.write_str("properties");
                w.write_hash();
                w.write_str("name");
                w.write_str(o.projection.alias());
                w.end_hash();
                w.end_hash();
            }
            w.write_str("features");
            w.write_array();
            w.write_newline();
        }

        let mut first_layer = true;
        for layer in &layers {
            if layer.extent == 0 {
                return fatal(format!("Impossible layer extent {} in mbtiles", layer.extent), EXIT_IMPOSSIBLE);
            }
            if !o.to_decode.is_empty() && !o.to_decode.contains(&layer.name) {
                continue;
            }
            if !o.pipeline {
                if !first_layer {
                    w.comma_newline();
                }
                w.write_hash();
                w.write_str("type");
                w.write_str("FeatureCollection");
                w.write_str("properties");
                w.write_hash();
                w.write_str("layer");
                w.write_string(&layer.name);
                w.write_str("version");
                w.write_signed(layer.version as i64);
                w.write_str("extent");
                w.write_signed(layer.extent as i64);
                w.end_hash();
                w.write_str("features");
                w.write_array();
                w.write_newline();
                first_layer = false;
            }

            // x and y are unsigned, so there is no test for < 0.
            let lim = if (0..64).contains(&z) { 1u64 << z } else { u64::MAX };
            if x as u64 > lim || y as u64 > lim {
                return fatal(format!("Impossible tile {z}/{x}/{y}"), EXIT_IMPOSSIBLE);
            }

            let scale = match o.coordinate_mode {
                1 => layer.extent as f64,
                2 => 1.0,
                _ => 0.0,
            };
            self.layer_to_geojson(w, layer, z as u32, x, y, !o.pipeline, o.pipeline, o.pipeline, !o.force, scale)?;

            if !o.pipeline {
                w.end_array();
                w.end_hash();
                w.write_newline();
            }
        }

        if !o.pipeline {
            w.end_array();
            w.end_hash();
            w.write_newline();
        }
        w.maybe_flush();
        Ok(())
    }

    /// The tiles of a whole tileset, with the metadata first.
    fn whole(&mut self, w: &mut JsonWriter, fname: &str, db: &Connection, dirtiles: Option<&[Zxy]>) -> Result<(), Fatal> {
        let o = self.o;
        let (pipeline, stats) = (o.pipeline, o.stats);
        if !pipeline && !stats {
            w.write_hash();
            w.write_str("type");
            w.write_str("FeatureCollection");
            w.write_str("properties");
            w.write_hash();
            w.write_newline();

            let mut st = db.prepare("SELECT name, value from metadata order by name;").or_else(|e| fatal(format!("{fname}: select failed: {}", sql_msg(&e)), EXIT_SQLITE))?;
            let mut rows = st.query([]).or_else(|e| fatal(format!("{fname}: select failed: {}", sql_msg(&e)), EXIT_SQLITE))?;
            let mut within = false;
            while let Ok(Some(row)) = rows.next() {
                let name = row.get_ref(0).ok().and_then(col_text);
                let value = row.get_ref(1).ok().and_then(col_text);
                let (Some(name), Some(value)) = (name, value) else {
                    return fatal("Corrupt mbtiles file: null metadata", EXIT_SQLITE);
                };
                if !o.exclude_meta.contains(&name) {
                    if within {
                        w.comma_newline();
                    }
                    within = true;
                    w.write_string(&name);
                    w.write_string(&value);
                }
            }
            w.write_newline();
            w.wantnl = false;
        }

        if stats {
            w.write_array();
            w.write_newline();
        }

        if !pipeline && !stats {
            w.end_hash();
            w.write_str("features");
            w.write_array();
            w.write_newline();
        }

        let mut within = false;
        let mut sep = |w: &mut JsonWriter| {
            if within {
                w.comma_newline();
            }
            within = true;
        };
        if let Some(tiles) = dirtiles {
            for t in tiles {
                if (!pipeline && !stats) || stats {
                    sep(w);
                }
                let fn_ = format!("{fname}/{}", t.path());
                let s = std::fs::read(&fn_).or_else(|e| fatal(format!("{fn_}: {}", strerror(&e)), EXIT_OPEN))?;
                self.handle(w, &s, t.z as i32, t.x as u32, t.y as u32)?;
            }
        } else {
            let sql = "SELECT tile_data, zoom_level, tile_column, tile_row from tiles where zoom_level between ? and ? order by zoom_level, tile_column, tile_row;";
            let mut st = db.prepare(sql).or_else(|e| fatal(format!("{fname}: select failed: {}", sql_msg(&e)), EXIT_SQLITE))?;
            let mut rows = st.query([o.minzoom, o.maxzoom]).or_else(|e| fatal(format!("{fname}: select failed: {}", sql_msg(&e)), EXIT_SQLITE))?;
            while let Ok(Some(row)) = rows.next() {
                if (!pipeline && !stats) || stats {
                    sep(w);
                }
                let tz = row.get_ref(1).map(col_int).unwrap_or(0);
                let tx = row.get_ref(2).map(col_int).unwrap_or(0);
                let ty = row.get_ref(3).map(col_int).unwrap_or(0);
                if !(0..32).contains(&tz) {
                    return fatal(format!("Impossible zoom level {tz} in mbtiles"), EXIT_IMPOSSIBLE);
                }
                let ty = ((1i64 << tz) - 1 - ty as i64) as i32;
                let data = match row.get_ref(0) {
                    Ok(rusqlite::types::ValueRef::Blob(b) | rusqlite::types::ValueRef::Text(b)) => b,
                    _ => return fatal("Corrupt mbtiles file: null entry in tiles table", EXIT_SQLITE),
                };
                self.handle(w, data, tz, tx as u32, ty as u32)?;
            }
        }

        if !pipeline && !stats {
            w.end_array();
            w.end_hash();
            w.write_newline();
        }
        if stats {
            w.end_array();
            w.write_newline();
        }
        if pipeline {
            w.write_newline();
        }
        Ok(())
    }

    /// One tile of a tileset. If there is no tile at `z`, try the parent tiles.
    fn single(&mut self, w: &mut JsonWriter, fname: &str, db: &Connection, z: i32, x: u32, y: u32) -> Result<(), Fatal> {
        let (oz, ox, oy) = (z, x, y);
        let (mut z, mut x, mut y) = (z, x, y);
        let mut handled = false;
        if pmtiles_has_suffix(fname) {
            while z>=0 {
                if let Some(data)=vtmake_out::pmtiles::tile(fname,z as u8,x,y).or_else(|e|fatal(format!("{fname}: {}",strerror(&e)),EXIT_OPEN))? {
                    if z!=oz {eprintln!("{fname}: Warning: using tile {z}/{x}/{y} instead of {oz}/{ox}/{oy}");}
                    return self.handle(w,&data,z,x,y);
                }
                z-=1;x/=2;y/=2;
            }
            return Ok(());
        }
        while z >= 0 && !handled {
            let sql = "SELECT tile_data from tiles where zoom_level = ? and tile_column = ? and tile_row = ?;";
            let mut st = db.prepare(sql).or_else(|e| fatal(format!("{fname}: select failed: {}", sql_msg(&e)), EXIT_SQLITE))?;
            let row_y = (1i64.wrapping_shl(z as u32) - 1 - y as i64) as i32;
            let mut rows = st.query([z, x as i32, row_y]).or_else(|e| fatal(format!("{fname}: select failed: {}", sql_msg(&e)), EXIT_SQLITE))?;
            while let Ok(Some(row)) = rows.next() {
                let data = match row.get_ref(0) {
                    Ok(rusqlite::types::ValueRef::Blob(b) | rusqlite::types::ValueRef::Text(b)) => b,
                    _ => return fatal("Corrupt mbtiles file: null entry in tiles table", EXIT_SQLITE),
                };
                if z != oz {
                    eprintln!("{fname}: Warning: using tile {z}/{x}/{y} instead of {oz}/{ox}/{oy}");
                }
                self.handle(w, data, z, x, y)?;
                handled = true;
            }
            z -= 1;
            x /= 2;
            y /= 2;
        }
        Ok(())
    }
}

/// `decode`: the tileset or tile `fname`. `z < 0`: the whole tileset.
fn decode(o: &Opts, w: &mut JsonWriter, fname: &str, z: i32, x: u32, y: u32) -> Result<(), Fatal> {
    let mut d = Dec { o, warned_ring: false, warned_inner: false };

    // A file that is not SQLite or PMTiles is one tile.
    match std::fs::File::open(fname) {
        Ok(f) => {
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            if f.metadata().map(|m| m.is_file()).unwrap_or(false) && len > 0 {
                let mut head = [0u8; 16];
                let n = std::io::Read::read(&mut &f, &mut head).unwrap_or(0);
                let head = &head[..n];
                let sqlite = head.starts_with(b"SQLite format 3") && (n == 15 || head[15] == 0);
                let pmtiles = head.starts_with(b"PMTiles");
                if !sqlite && !pmtiles {
                    if z < 0 {
                        return fatal("Must specify zoom/x/y to decode a single pbf file", EXIT_ARGS);
                    }
                    if len > 250 * 1024 * 1024 {
                        return fatal(format!("{fname}: unrealistically large single-tile size {len}"), EXIT_MEMORY);
                    }
                    let s = std::fs::read(fname).or_else(|e| fatal(format!("{fname}: {}", strerror(&e)), EXIT_OPEN))?;
                    return d.handle(w, &s, z, x, y);
                }
            }
        }
        Err(e) => eprintln!("{fname}: {}", strerror(&e)),
    }

    let mut dirtiles = None;
    let db = if is_dir(fname) {
        let db = dirmeta2tmp(fname)?;
        dirtiles = Some(enumerate_dirtiles(fname, o.minzoom, o.maxzoom)?);
        db
    } else if pmtiles_has_suffix(fname) {
        vtmake_out::pmtiles::open(fname).or_else(|e| fatal(format!("{fname}: {}", strerror(&e)), EXIT_OPEN))?
    } else {
        let db = Connection::open(fname).or_else(|e| fatal(format!("{fname}: {}", sql_msg(&e)), EXIT_OPEN))?;
        // tippecanoe runs `PRAGMA integrity_check`; it fails only if the file is
        // not a database. Reading the schema finds the same error, faster.
        if let Err(e) = db.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)) {
            return fatal(format!("{fname}: integrity_check: {}", sql_msg(&e)), EXIT_SQLITE);
        }
        db
    };

    if z < 0 {
        d.whole(w, fname, &db, dirtiles.as_deref())
    } else {
        d.single(w, fname, &db, z, x, y)
    }
}

fn usage(argv0: &str) -> ! {
    eprintln!("Usage: {argv0} [options] tileset");
    eprintln!("   or: {argv0} [options] tileset zoom x y");
    eprintln!("  Tiles to decode");
    eprintln!("         [--minimum-zoom=...] [--maximum-zoom=...] [--layer=...]");
    eprintln!("  Output format");
    eprintln!("         [--projection=...] [--fractional-coordinates] [--integer-coordinates]");
    eprintln!("         [--tag-layer-and-zoom] [--stats]");
    eprintln!("  Filtering the output");
    eprintln!("         [--include=...] [--exclude-metadata-row=...]");
    eprintln!("  Ignoring errors in the input");
    eprintln!("         [--force]");
    eprintln!();
    eprintln!("The tileset can be an .mbtiles or .pmtiles file or a directory of tiles,");
    eprintln!("or, if zoom/x/y is specified, a single .pbf tile.");
    std::process::exit(EXIT_ARGS)
}

/// `vtmake decode` (argv[0] is the program name).
pub fn main(argv: Vec<String>) {
    let argv0 = argv.first().cloned().unwrap_or_else(|| "tippecanoe-decode".into());
    let shorts = [
        Short { ch: 'Z', arg: true, key: "Z" },
        Short { ch: 'z', arg: true, key: "z" },
        Short { ch: 'l', arg: true, key: "l" },
        Short { ch: 's', arg: true, key: "s" },
        Short { ch: 'F', arg: false, key: "F" },
        Short { ch: 'I', arg: false, key: "I" },
        Short { ch: 'c', arg: false, key: "c" },
        Short { ch: 'S', arg: false, key: "S" },
        Short { ch: 'y', arg: true, key: "y" },
        Short { ch: 'x', arg: true, key: "x" },
        Short { ch: 'f', arg: false, key: "f" },
    ];
    let longs = [
        Long { name: "minimum-zoom", arg: true, key: "Z" },
        Long { name: "maximum-zoom", arg: true, key: "z" },
        Long { name: "layer", arg: true, key: "l" },
        Long { name: "projection", arg: true, key: "s" },
        Long { name: "fractional-coordinates", arg: false, key: "F" },
        Long { name: "integer-coordinates", arg: false, key: "I" },
        Long { name: "tag-layer-and-zoom", arg: false, key: "c" },
        Long { name: "stats", arg: false, key: "S" },
        Long { name: "include", arg: true, key: "y" },
        Long { name: "exclude-metadata-row", arg: true, key: "x" },
        Long { name: "force", arg: false, key: "f" },
    ];
    let u = || usage(&argv0);
    let p = getopt::parse(&argv, &shorts, &longs, &u);
    let mut o = Opts {
        minzoom: 0,
        maxzoom: 32,
        force: false,
        include_attr: BTreeSet::new(),
        to_decode: BTreeSet::new(),
        pipeline: false,
        stats: false,
        exclude_meta: BTreeSet::new(),
        coordinate_mode: 0,
        projection: Projection::Epsg4326,
    };
    for (k, v) in p.opts {
        let v = v.unwrap_or_default();
        match k {
            "s" => {
                o.projection = match v.as_str() {
                    "EPSG:4326" | "urn:ogc:def:crs:OGC:1.3:CRS84" => Projection::Epsg4326,
                    "EPSG:3857" | "urn:ogc:def:crs:EPSG::3857" => Projection::Epsg3857,
                    _ => {
                        eprintln!("Unknown projection (-s): {v}");
                        std::process::exit(EXIT_ARGS)
                    }
                }
            }
            "F" => o.coordinate_mode = 1,
            "I" => o.coordinate_mode = 2,
            "z" => o.maxzoom = atoi(&v),
            "Z" => o.minzoom = atoi(&v),
            "l" => drop(o.to_decode.insert(v.into_bytes())),
            "c" => o.pipeline = true,
            "S" => o.stats = true,
            "f" => o.force = true,
            "x" => drop(o.exclude_meta.insert(v.into_bytes())),
            "y" => drop(o.include_attr.insert(v.into_bytes())),
            _ => unreachable!(),
        }
    }

    let (fname, z, x, y) = match p.operands.as_slice() {
        [f, z, x, y] => (f.clone(), atoi(z), atoi(x) as u32, atoi(y) as u32),
        [f] => (f.clone(), -1, u32::MAX, u32::MAX),
        _ => usage(&argv0),
    };

    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let mut w = JsonWriter::to(&mut lock);
    let r = decode(&o, &mut w, &fname, z, x, y);
    // As C's exit(): the buffered output goes out first.
    w.flush();
    if let Err(f) = r {
        f.exit();
    }
    w.check_closed();
}
