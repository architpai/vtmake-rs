//! PMTiles v3 directories and Hilbert order, ported from tippecanoe
//! pmtiles_file.cpp and pmtiles/pmtiles.hpp. Copyright (c) 2022, Protomaps LLC;
//! Copyright (c) 2014, Mapbox Inc. BSD 2-Clause: LICENSE-tippecanoe.md.
use rusqlite::{Connection, params};
use std::io;

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Malformed PMTiles archive")
}
fn slice(b: &[u8], off: u64, len: u64) -> io::Result<&[u8]> {
    b.get(
        usize::try_from(off).map_err(|_| invalid())?
            ..usize::try_from(off.checked_add(len).ok_or_else(invalid)?).map_err(|_| invalid())?,
    )
    .ok_or_else(invalid)
}
fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}
fn unpack(b: &[u8], compression: u8) -> io::Result<Vec<u8>> {
    match compression {
        1 => Ok(b.to_vec()),
        2 => {
            let mut o = Vec::new();
            if vtmake_zlib::gunzip(b, &mut o) {
                Ok(o)
            } else {
                Err(invalid())
            }
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Unknown or unsupported pmtiles compression: {compression}"),
        )),
    }
}
fn zip(b: &[u8]) -> Vec<u8> {
    let mut o = Vec::new();
    vtmake_zlib::GzipEncoder::new().compress(b, &mut o);
    o
}
fn var(b: &mut &[u8]) -> io::Result<u64> {
    let mut n = 0;
    for shift in (0..64).step_by(7) {
        let (&c, rest) = b.split_first().ok_or_else(invalid)?;
        *b = rest;
        if shift == 63 && c > 1 {
            return Err(invalid());
        }
        n |= ((c & 127) as u64) << shift;
        if c < 128 {
            return Ok(n);
        }
    }
    Err(invalid())
}
fn put(mut n: u64, b: &mut Vec<u8>) {
    while n >= 128 {
        b.push(n as u8 | 128);
        n >>= 7;
    }
    b.push(n as u8);
}
#[derive(Clone, Default)]
struct Entry {
    id: u64,
    offset: u64,
    len: u64,
    run: u64,
}
fn directory(b: &[u8]) -> io::Result<Vec<Entry>> {
    let mut b = b;
    let n = usize::try_from(var(&mut b)?).map_err(|_| invalid())?;
    if n > b.len() / 4 {
        return Err(invalid());
    }
    let mut es = vec![Entry::default(); n];
    let mut id = 0u64;
    for e in &mut es {
        id = id.checked_add(var(&mut b)?).ok_or_else(invalid)?;
        e.id = id;
    }
    for e in &mut es {
        e.run = var(&mut b)?;
    }
    for e in &mut es {
        e.len = var(&mut b)?;
    }
    for i in 0..n {
        let v = var(&mut b)?;
        es[i].offset = if v == 0 && i > 0 {
            es[i - 1]
                .offset
                .checked_add(es[i - 1].len)
                .ok_or_else(invalid)?
        } else {
            v.checked_sub(1).ok_or_else(invalid)?
        };
    }
    if !b.is_empty() {
        return Err(invalid());
    }
    Ok(es)
}
fn encode(es: &[Entry]) -> Vec<u8> {
    let mut b = Vec::new();
    put(es.len() as u64, &mut b);
    let mut last = 0;
    for e in es {
        put(e.id - last, &mut b);
        last = e.id;
    }
    for e in es {
        put(e.run, &mut b);
    }
    for e in es {
        put(e.len, &mut b);
    }
    for (i, e) in es.iter().enumerate() {
        put(
            if i > 0 && e.offset == es[i - 1].offset + es[i - 1].len {
                0
            } else {
                e.offset + 1
            },
            &mut b,
        );
    }
    b
}
fn rotate(s: i64, x: &mut i64, y: &mut i64, rx: i64, ry: i64) {
    if ry == 0 {
        if rx == 1 {
            *x = s - 1 - *x;
            *y = s - 1 - *y;
        }
        std::mem::swap(x, y);
    }
}
pub fn tile_id(z: u8, x: u32, y: u32) -> u64 {
    let (mut x, mut y) = (x as i64, y as i64);
    let mut d = 0;
    let mut s = (1i64 << z) / 2;
    while s > 0 {
        let (rx, ry) = ((x & s != 0) as i64, (y & s != 0) as i64);
        d += s * s * ((3 * rx) ^ ry);
        rotate(s, &mut x, &mut y, rx, ry);
        s /= 2;
    }
    ((1u64 << (2 * z)) - 1) / 3 + d as u64
}
pub fn zxy(id: u64) -> io::Result<(u8, u32, u32)> {
    let mut base = 0;
    for z in 0..32u8 {
        let n = 1u64 << (2 * z);
        if id < base + n {
            let mut t = id - base;
            let (mut x, mut y) = (0i64, 0i64);
            let mut s = 1;
            while s < (1i64 << z) {
                let rx = ((t / 2) & 1) as i64;
                let ry = ((t ^ rx as u64) & 1) as i64;
                rotate(s, &mut x, &mut y, rx, ry);
                x += s * rx;
                y += s * ry;
                t /= 4;
                s *= 2;
            }
            return Ok((z, x as u32, y as u32));
        }
        base += n;
    }
    Err(invalid())
}

/// Read PMTiles into the same SQL interface as the existing tileset tools.
/// Directories and tile ranges are checked before use.
pub fn open(path: &str) -> io::Result<Connection> {
    let b = std::fs::read(path)?;
    if b.len() < 127 || &b[..8] != b"PMTiles\x03" {
        return Err(invalid());
    }
    let db = Connection::open_in_memory().map_err(io::Error::other)?;
    db.execute_batch("CREATE TABLE metadata(name text,value text); CREATE TABLE tiles(zoom_level integer,tile_column integer,tile_row integer,tile_data blob);").map_err(io::Error::other)?;
    let add = |k: &str, v: String| {
        db.execute("INSERT INTO metadata VALUES (?,?)", params![k, v])
            .map(|_| ())
            .map_err(io::Error::other)
    };
    add("minzoom", b[100].to_string())?;
    add("maxzoom", b[101].to_string())?;
    let coord = |i| i32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as f64 / 1e7;
    add(
        "center",
        format!("{:.6},{:.6},{}", coord(119), coord(123), b[118]),
    )?;
    add(
        "bounds",
        format!(
            "{:.6},{:.6},{:.6},{:.6}",
            coord(102),
            coord(106),
            coord(110),
            coord(114)
        ),
    )?;
    let text = unpack(slice(&b, u64_at(&b, 24), u64_at(&b, 32))?, b[97])?;
    use vtmake_parse::pull::{Kind, parse_tree};
    let (p, obj) = parse_tree(&text).map_err(io::Error::other)?;
    if p.kind(obj) != Kind::Hash {
        return Err(invalid());
    }
    let mut json = Vec::new();
    for pair in p.items(obj).as_chunks::<2>().0 {
        let k = String::from_utf8_lossy(p.text(pair[0]));
        let v = pair[1];
        let ty = p.kind(v);
        let stringify = || String::from_utf8_lossy(&p.stringify(v)).into_owned();
        if (k == "vector_layers" && ty == Kind::Array) || (k == "tilestats" && ty == Kind::Hash) {
            json.push(format!("{}:{}", crate::json_string(&k), stringify()));
        } else if (k == "strategies" && ty == Kind::Array)
            || (k == "tippecanoe_decisions" && ty == Kind::Hash)
        {
            add(&k, stringify())?;
        } else if ty == Kind::String {
            add(&k, String::from_utf8_lossy(p.text(v)).into_owned())?;
        } else {
            eprintln!("{k}\n{path}: non-string in metadata");
        }
    }
    if !json.is_empty() {
        add("json", format!("{{{}}}", json.join(",")))?;
    }
    fn collect(b: &[u8], db: &Connection, off: u64, len: u64, depth: u8) -> io::Result<()> {
        if depth > 4 {
            return Err(invalid());
        }
        for e in directory(&unpack(slice(b, off, len)?, b[97])?)? {
            if e.run == 0 {
                collect(
                    b,
                    db,
                    u64_at(b, 40).checked_add(e.offset).ok_or_else(invalid)?,
                    e.len,
                    depth + 1,
                )?;
            } else {
                let data = slice(
                    b,
                    u64_at(b, 56).checked_add(e.offset).ok_or_else(invalid)?,
                    e.len,
                )?;
                for id in e.id..e.id.checked_add(e.run).ok_or_else(invalid)? {
                    let (z, x, y) = zxy(id)?;
                    db.execute(
                        "INSERT INTO tiles VALUES (?,?,?,?)",
                        params![z, x, (1u64 << z) - 1 - y as u64, data],
                    )
                    .map_err(io::Error::other)?;
                }
            }
        }
        Ok(())
    }
    collect(&b, &db, u64_at(&b, 8), u64_at(&b, 16), 0)?;
    db.execute_batch("CREATE INDEX tiles_zxy ON tiles(zoom_level,tile_column,tile_row);")
        .map_err(io::Error::other)?;
    Ok(db)
}

/// Convert the completed MBTiles database at `path` to felt's clustered
/// PMTiles layout (`mbtiles_map_image_to_pmtiles`), in the same file. The tile
/// data goes to `<path>.tmp` first, in tile ID order, so only the directory
/// entries stay in memory. `tile_gzip`: the tile compression of the header
/// (`None`: from the first tile).
pub fn convert(path: &std::path::Path, db: Connection, meta: &crate::Metadata, tile_gzip: Option<bool>) -> io::Result<()> {
    let get = |k: &str| {
        meta.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    };
    let mut ids = {
        let mut st = db.prepare("SELECT zoom_level, tile_column, tile_row FROM map").map_err(io::Error::other)?;
        st.query_map([], |r| {
            let z = r.get::<_, u8>(0)?;
            let x = r.get::<_, u32>(1)?;
            let y = r.get::<_, u32>(2)?;
            Ok(tile_id(z, x, (1u32 << z) - 1 - y))
        })
        .map_err(io::Error::other)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)?
    };
    ids.sort_unstable();
    let tmpname = format!("{}.tmp", path.display());
    let mut tmp = io::BufWriter::new(std::fs::File::create(&tmpname)?);
    let mut offset = 0u64;
    let mut first_gzip = None;
    // The map `tile_id` (the fnv1a hash of the tile) to the offset and length.
    let mut seen: std::collections::HashMap<String, (u64, u64)> = std::collections::HashMap::new();
    let mut es: Vec<Entry> = Vec::new();
    {
        let mut map_st = db.prepare("SELECT tile_id FROM map WHERE zoom_level = ? AND tile_column = ? AND tile_row = ?").map_err(io::Error::other)?;
        let mut image_st = db.prepare("SELECT tile_data FROM images WHERE zoom_level = ? AND tile_id = ?").map_err(io::Error::other)?;
        for &id in &ids {
            let (z, x, y) = zxy(id)?;
            let row = (1u64 << z) - 1 - y as u64;
            let hash: String = map_st.query_row(params![z, x, row], |r| r.get(0)).map_err(io::Error::other)?;
            if let Some(&(off, len)) = seen.get(&hash) {
                if let Some(e) = es.last_mut().filter(|e| id == e.id + 1 && e.offset == off) {
                    e.run += 1;
                } else {
                    es.push(Entry { id, offset: off, len, run: 1 });
                }
            } else {
                let bytes: Vec<u8> = image_st.query_row(params![z, hash], |r| r.get(0)).map_err(io::Error::other)?;
                first_gzip.get_or_insert(bytes.starts_with(&[31, 139]));
                io::Write::write_all(&mut tmp, &bytes)?;
                let len = bytes.len() as u64;
                es.push(Entry { id, offset, len, run: 1 });
                seen.insert(hash, (offset, len));
                offset += len;
            }
        }
    }
    io::Write::flush(&mut tmp)?;
    drop(tmp);
    db.close().map_err(|(_, e)| io::Error::other(e))?;
    let mut root = zip(&encode(&es));
    let mut leaves = Vec::new();
    if root.len() > 16384 - 127 {
        let mut size = 4096;
        loop {
            leaves.clear();
            let mut roots = Vec::new();
            for chunk in es.chunks(size) {
                let b = zip(&encode(chunk));
                roots.push(Entry {
                    id: chunk[0].id,
                    offset: leaves.len() as u64,
                    len: b.len() as u64,
                    run: 0,
                });
                leaves.extend(b);
            }
            root = zip(&encode(&roots));
            if root.len() < 16384 - 127 {
                break;
            }
            size *= 2;
        }
    }
    let mut fields = Vec::new();
    for k in [
        "name",
        "format",
        "type",
        "description",
        "version",
        "attribution",
        "strategies",
        "tippecanoe_decisions",
        "generator",
        "generator_options",
        "antimeridian_adjusted_bounds",
    ] {
        let v = get(k);
        let v = if k == "generator" && v.starts_with("vtmake v") { "tippecanoe v2.82.0" } else { v };
        if v.is_empty() && matches!(k, "attribution" | "strategies" | "tippecanoe_decisions") {
            continue;
        }
        fields.push(format!(
            "{}: {}",
            crate::json_string(k),
            if matches!(k, "strategies" | "tippecanoe_decisions") {
                v.into()
            } else {
                crate::json_string(v)
            }
        ));
    }
    // These values are already in felt's JSON layout. Keep that layout.
    let j = get("json");
    if !j.is_empty() {
        let obj: std::collections::BTreeMap<String, Box<serde_json::value::RawValue>> =
            serde_json::from_str(j).map_err(io::Error::other)?;
        for k in ["vector_layers", "tilestats"] {
            if let Some(v) = obj.get(k) {
                fields.push(format!("{}: {}", crate::json_string(k), v.get()));
            }
        }
    }
    let json = zip(format!("{{\n{}\n}}\n", fields.join(",\n")).as_bytes());
    let mut h = vec![0u8; 127];
    h[..8].copy_from_slice(b"PMTiles\x03");
    let offsets = [
        127,
        root.len() as u64,
        127 + root.len() as u64,
        json.len() as u64,
        127 + root.len() as u64 + json.len() as u64,
        leaves.len() as u64,
        127 + root.len() as u64 + json.len() as u64 + leaves.len() as u64,
        offset,
        ids.len() as u64,
        es.len() as u64,
        seen.len() as u64,
    ];
    for (i, v) in offsets.iter().enumerate() {
        h[8 + i * 8..16 + i * 8].copy_from_slice(&v.to_le_bytes());
    }
    h[96..100].copy_from_slice(&[
        1,
        2,
        if tile_gzip.or(first_gzip).unwrap_or(true) { 2 } else { 1 },
        1,
    ]);
    h[100] = get("minzoom").parse::<i32>().unwrap_or(0) as u8;
    h[101] = get("maxzoom").parse::<i32>().unwrap_or(0) as u8;
    for (i, v) in get("bounds").split(',').take(4).enumerate() {
        let n = (v.parse::<f64>().unwrap_or(0.) * 1e7) as i32;
        h[102 + i * 4..106 + i * 4].copy_from_slice(&n.to_le_bytes());
    }
    let c: Vec<_> = get("center").split(',').collect();
    if c.len() == 3 {
        h[118] = c[2].parse().unwrap_or(0);
        for i in 0..2 {
            let n = (c[i].parse::<f64>().unwrap_or(0.) * 1e7) as i32;
            h[119 + i * 4..123 + i * 4].copy_from_slice(&n.to_le_bytes());
        }
    }
    use std::io::Write;
    let mut out = io::BufWriter::new(std::fs::File::create(path)?);
    for b in [&h, &root, &json, &leaves] {
        out.write_all(b)?;
    }
    io::copy(&mut std::fs::File::open(&tmpname)?, &mut out)?;
    out.flush()?;
    std::fs::remove_file(&tmpname)
}

/// Felt's single-tile lookup uses a signed 32-bit difference in its binary
/// search. Keep that rule, including its behaviour across distant zooms.
pub fn tile(path: &str, z: u8, x: u32, y: u32) -> io::Result<Option<Vec<u8>>> {
    let b=std::fs::read(path)?;
    if b.len()<127 || &b[..8]!=b"PMTiles\x03" {return Err(invalid());}
    if z>31 || x as u64>=1u64<<z || y as u64>=1u64<<z {return Err(invalid());}
    let id=tile_id(z,x,y); let mut off=u64_at(&b,8); let mut len=u64_at(&b,16);
    for _ in 0..4 {
        let es=directory(&unpack(slice(&b,off,len)?,b[97])?)?;
        let (mut m,mut n)=(0i64,es.len() as i64-1); let mut found=None;
        while m<=n {let k=(m+n)>>1; let cmp=id.wrapping_sub(es[k as usize].id) as i32;
            if cmp>0 {m=k+1;} else if cmp<0 {n=k-1;} else {found=Some(&es[k as usize]);break;}
        }
        if found.is_none() && n>=0 {let e=&es[n as usize]; if e.run==0 || id.wrapping_sub(e.id)<e.run {found=Some(e);}}
        let Some(e)=found.filter(|e|e.len>0) else {return Ok(None)};
        if e.run>0 {return Ok(Some(slice(&b,u64_at(&b,56).checked_add(e.offset).ok_or_else(invalid)?,e.len)?.to_vec()));}
        off=u64_at(&b,40).checked_add(e.offset).ok_or_else(invalid)?;len=e.len;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hilbert_roundtrip() {
        for z in 0..7 {
            for x in 0..1 << z {
                for y in 0..1 << z {
                    assert_eq!(zxy(tile_id(z, x, y)).unwrap(), (z, x, y));
                }
            }
        }
    }
    #[test]
    fn truncated_directory() {
        for b in [&[][..], &[128], &[1, 0, 1, 1], &[255; 10]] {
            assert!(directory(b).is_err());
        }
    }
}
