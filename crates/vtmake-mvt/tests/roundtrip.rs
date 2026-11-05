//! Decode every tile of a tippecanoe MBTiles file, encode it again with vtmake-mvt, gzip
//! it, and compare the bytes with the original tile.
//! Run: VTMAKE_MBTILES=/path/out.mbtiles cargo test -p vtmake-mvt --release --test roundtrip -- --nocapture

use vtmake_mvt::{MvtFeature, MvtLayer, MvtTile, decode::decode_tile, gzip};
use rayon::prelude::*;

#[test]
fn reencode_matches_tippecanoe_bytes() {
    let Ok(path) = std::env::var("VTMAKE_MBTILES") else {
        eprintln!("VTMAKE_MBTILES not set: skip");
        return;
    };
    let db = rusqlite::Connection::open(&path).unwrap();
    let mut st = db.prepare("select zoom_level, tile_column, tile_row, tile_data from tiles").unwrap();
    let tiles: Vec<(i64, i64, i64, Vec<u8>)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let bad: Vec<_> = tiles
        .par_iter()
        .filter_map(|(z, x, y, data)| {
            let mut raw = Vec::new();
            assert!(gzip::gunzip(data, &mut raw));
            let layers = decode_tile(&raw).expect("decode");
            let mut tile = MvtTile::default();
            for dl in layers {
                let mut l = MvtLayer::new(dl.name);
                l.version = dl.version;
                l.extent = dl.extent;
                for df in dl.features {
                    let mut f = MvtFeature { geom_type: df.geom_type, id: df.id, geometry: df.geometry, tags: vec![] };
                    for kv in df.tags.chunks(2) {
                        l.tag(&mut f, &dl.keys[kv[0] as usize], &dl.values[kv[1] as usize]);
                    }
                    l.features.push(f);
                }
                tile.layers.push(l);
            }
            let mut enc = Vec::new();
            tile.encode(&mut enc);
            if enc != raw {
                return Some(format!("{z}/{x}/{y}: pbf differs ({} vs {} bytes)", enc.len(), raw.len()));
            }
            let mut gz = Vec::new();
            gzip::gzip(&enc, &mut gz);
            (gz != *data).then(|| format!("{z}/{x}/{y}: gzip differs"))
        })
        .collect();
    eprintln!("{} tiles, {} differ", tiles.len(), bad.len());
    for b in bad.iter().take(10) {
        eprintln!("  {b}");
    }
    assert!(bad.is_empty());
}
