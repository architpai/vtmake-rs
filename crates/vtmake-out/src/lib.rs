//! Tile output: MBTiles (`-o`), PMTiles (`-o *.pmtiles`) and a tile directory
//! (`-e`), with tippecanoe's schema and file layout (spec 9.2 to 9.5).
//!
//! PMTiles output works as in tippecanoe: the tiles go to an MBTiles database
//! at the output path; at the end, the tile data goes to `<output>.tmp` in
//! the PMTiles order, and then the PMTiles file replaces the database. Thus
//! the memory use does not depend on the size of the tileset.
//!
//! [`MbtilesWriter::open`] and [`DirWriter::open`] do the checks of
//! `tippecanoe` (`-f`, `-F`) with its messages and exit codes.

pub mod pmtiles;

use rusqlite::{Connection, params};
use std::io;
use std::path::{Path, PathBuf};

/// Metadata rows in tippecanoe's mbtiles order (spec 9.3, 9.4). Values are
/// already formatted. No rows: tippecanoe found no features; then the mbtiles
/// file has no metadata rows and the directory has no `metadata.json`.
pub type Metadata = Vec<(String, String)>;

/// The field order of `metadata.json` (`dirtiles.cpp:dir_write_metadata`). It is
/// not the mbtiles order: `strategies` comes before `format`.
const DIR_ORDER: &[&str] = &[
    "name",
    "description",
    "version",
    "minzoom",
    "maxzoom",
    "center",
    "bounds",
    "antimeridian_adjusted_bounds",
    "type",
    "attribution",
    "strategies",
    "tippecanoe_decisions",
    "format",
    "generator",
    "generator_options",
    "json",
];

pub trait TileSink: Send {
    /// `data` is the gzip tile. `y` is in XYZ order.
    fn write_tile(&mut self, z: u8, x: u32, y: u32, data: &[u8]) -> io::Result<()>;
    fn finish(self: Box<Self>, metadata: &Metadata) -> io::Result<()>;
}

// Fresh output can be rebuilt after a failed command. Avoid rollback-journal
// I/O for that output; retain the journal for updates to an existing file.
fn configure_db(db: &Connection, fresh: bool) -> rusqlite::Result<()> {
    db.execute_batch("PRAGMA synchronous=0; PRAGMA locking_mode=EXCLUSIVE; PRAGMA cache_size=-262144;")?;
    db.execute_batch(if fresh { "PRAGMA journal_mode=OFF;" } else { "PRAGMA journal_mode=DELETE;" })
}

fn sql_err(e: rusqlite::Error) -> io::Error {
    io::Error::other(e)
}

/// fnv1a 64-bit, the tile id in the `images` table.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

pub struct MbtilesWriter {
    db: Connection,
    pending: usize,
    pmtiles: Option<PathBuf>,
    /// `--allow-existing`: a metadata row that is there already is not an error.
    allow_existing: bool,
    /// The PMTiles tile compression (gzip or none). `None`: from the first tile.
    tile_gzip: Option<bool>,
}

// Keep index pages in a bounded cache. The SQLite default (about 2 MiB)
// causes repeated page reads and journal writes on the 500 IOPS profile.
// A negative cache_size is in KiB; this adds at most 256 MiB per writer.
// Larger transactions avoid writing the same index pages to the rollback
// journal after every 4096 tiles. The page cache still has a fixed limit.
const BATCH: usize = 262144;

// tippecanoe exit codes (`errors.hpp`).
const EXIT_EXISTS: i32 = 104;
const EXIT_OPEN: i32 = 111;
const EXIT_SQLITE: i32 = 116;
const EXIT_UNLINK: i32 = 118;
const EXIT_WRITE: i32 = 120;

/// Write the message and stop (tippecanoe's `fprintf` and `exit`).
fn die(msg: &str, code: i32) -> ! {
    eprintln!("{msg}");
    std::process::exit(code)
}

/// The SQLite message of an error (`sqlite3_errmsg`).
fn sql_msg(e: &rusqlite::Error) -> String {
    match e {
        rusqlite::Error::SqliteFailure(_, Some(m)) => m.clone(),
        rusqlite::Error::SqlInputError { msg, .. } => msg.clone(),
        rusqlite::Error::SqliteFailure(f, None) => f.to_string(),
        e => e.to_string(),
    }
}

fn os_msg(e: &io::Error) -> String {
    let s = e.to_string();
    s.split(" (os error").next().unwrap_or(&s).to_string()
}

/// The schema statements of `mbtiles_open` after `CREATE TABLE metadata`, with
/// the label of each error message.
const SCHEMA: &[(&str, &str)] = &[
    ("create unique index name on metadata (name);", "index metadata"),
    ("CREATE TABLE map (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_id TEXT);", "create map table"),
    ("CREATE UNIQUE INDEX map_index ON map (zoom_level, tile_column, tile_row);", "create map index"),
    ("CREATE TABLE images (zoom_level integer, tile_data blob, tile_id text);", "create images table"),
    ("CREATE UNIQUE INDEX images_id ON images (zoom_level, tile_id);", "create images index"),
    (
        "CREATE VIEW tiles AS SELECT map.zoom_level AS zoom_level, map.tile_column AS tile_column, map.tile_row AS tile_row, images.tile_data AS tile_data FROM map JOIN images ON images.tile_id = map.tile_id and images.zoom_level = map.zoom_level;",
        "create tiles view",
    ),
];

/// The label of the error message for a metadata row (`mbtiles_write_metadata`).
fn metadata_label(name: &str) -> String {
    match name {
        "name" | "description" => format!("{name} in metadata"),
        "version" => "version ".into(),
        "antimeridian_adjusted_bounds" => "bounds".into(),
        "generator_options" => "commandline".into(),
        "tippecanoe_decisions" => "decisions".into(),
        n => n.into(),
    }
}

impl MbtilesWriter {
    /// Create a new MBTiles file. With `force`, delete the old file first. Without
    /// it, a file that has a `metadata` table is an error (`AlreadyExists`), as in
    /// tippecanoe (`mbtiles.cpp:mbtiles_open`).
    pub fn create(path: &Path, force: bool) -> io::Result<Self> {
        if force {
            match std::fs::remove_file(path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        let pmtiles = is_pmtiles(path);
        if pmtiles && path.exists() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{}: file exists", path.display())));
        }
        // As tippecanoe, also for PMTiles: an MBTiles database at the output path.
        let fresh = !path.exists();
        let db = Connection::open(path).map_err(sql_err)?;
        configure_db(&db, fresh).map_err(sql_err)?;
        if let Err(e) = db.execute_batch("CREATE TABLE metadata (name text, value text);") {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, e));
        }
        db.execute_batch(
            "create unique index name on metadata (name);
             CREATE TABLE map (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_id TEXT);
             CREATE UNIQUE INDEX map_index ON map (zoom_level, tile_column, tile_row);
             CREATE TABLE images (zoom_level integer, tile_data blob, tile_id text);
             CREATE UNIQUE INDEX images_id ON images (zoom_level, tile_id);
             CREATE VIEW tiles AS SELECT map.zoom_level AS zoom_level, map.tile_column AS tile_column, map.tile_row AS tile_row, images.tile_data AS tile_data FROM map JOIN images ON images.tile_id = map.tile_id and images.zoom_level = map.zoom_level;
             BEGIN;",
        )
        .map_err(sql_err)?;
        Ok(MbtilesWriter { db, pending: 0, pmtiles: pmtiles.then(|| path.to_path_buf()), allow_existing: false, tile_gzip: None })
    }

    /// The output of `tippecanoe -o` (`main.cpp` and `mbtiles_open`): `-f`
    /// deletes the old file; else an old PMTiles file is an error. With `-F`
    /// (`allow_existing`), an old MBTiles file is used again: its tables and
    /// metadata stay (tippecanoe writes a message for each one) and the new
    /// tiles are added. Errors write tippecanoe's message and stop.
    pub fn open(path: &Path, force: bool, allow_existing: bool, argv0: &str) -> MbtilesWriter {
        let name = path.display().to_string();
        let pmtiles = is_pmtiles(path);
        if force {
            let _ = std::fs::remove_file(path);
        } else if pmtiles && std::fs::metadata(path).is_ok() {
            // `check_pmtiles`.
            eprintln!("{argv0}: Tileset \"{name}\" already exists. You can use --force if you want to delete the old tileset.");
            eprintln!("{argv0}: {name}: file exists");
            if allow_existing {
                eprintln!("{argv0}: --allow-existing is not supported for pmtiles");
            }
            std::process::exit(EXIT_EXISTS);
        }
        let fresh = !path.exists();
        let db = Connection::open(path).unwrap_or_else(|e| {
            // rusqlite adds the path to `sqlite3_errmsg`.
            let m = sql_msg(&e);
            let m = m.strip_suffix(&format!(": {name}")).unwrap_or(&m);
            die(&format!("{argv0}: {name}: {m}"), EXIT_OPEN)
        });
        if let Err(e) = configure_db(&db, fresh) {
            die(&format!("{argv0}: async: {}", sql_msg(&e)), EXIT_SQLITE);
        }
        if let Err(e) = db.execute_batch("CREATE TABLE metadata (name text, value text);") {
            eprintln!("{argv0}: Tileset \"{name}\" already exists. You can use --force if you want to delete the old tileset.");
            eprintln!("{argv0}: {}", sql_msg(&e));
            if !allow_existing {
                std::process::exit(EXIT_EXISTS);
            }
        }
        for (sql, label) in SCHEMA {
            if let Err(e) = db.execute_batch(sql) {
                eprintln!("{argv0}: {label}: {}", sql_msg(&e));
                if !allow_existing {
                    std::process::exit(EXIT_EXISTS);
                }
            }
        }
        if let Err(e) = db.execute_batch("BEGIN;") {
            die(&format!("{argv0}: {name}: {}", sql_msg(&e)), EXIT_SQLITE);
        }
        MbtilesWriter { db, pending: 0, pmtiles: pmtiles.then(|| path.to_path_buf()), allow_existing, tile_gzip: None }
    }

    /// The tile compression for the PMTiles header (`-pC`: none).
    pub fn set_tile_compression(&mut self, gzip: bool) {
        self.tile_gzip = Some(gzip);
    }
}

/// `pmtiles_has_suffix`.
fn is_pmtiles(path: &Path) -> bool {
    path.to_str().is_some_and(|s| s.ends_with(".pmtiles"))
}

impl TileSink for MbtilesWriter {
    fn write_tile(&mut self, z: u8, x: u32, y: u32, data: &[u8]) -> io::Result<()> {
        let id = fnv1a(data).to_string();
        let row = (1u64 << z) - 1 - y as u64;
        // `mbtiles_write_tile`: an error stops with tippecanoe's message.
        if let Err(e) = self.db.prepare_cached("replace into images (zoom_level, tile_id, tile_data) values (?, ?, ?)").and_then(|mut s| s.execute(params![z, id, data])) {
            die(&format!("sqlite3 images insert failed: {}", sql_msg(&e)), EXIT_SQLITE);
        }
        if let Err(e) = self.db.prepare_cached("insert into map (zoom_level, tile_column, tile_row, tile_id) values (?, ?, ?, ?)").and_then(|mut s| s.execute(params![z, x, row, id])) {
            die(&format!("sqlite3 map insert failed: {}", sql_msg(&e)), EXIT_SQLITE);
        }
        self.pending += 1;
        if self.pending >= BATCH {
            self.db.execute_batch("COMMIT; BEGIN;").map_err(sql_err)?;
            self.pending = 0;
        }
        Ok(())
    }

    fn finish(self: Box<Self>, metadata: &Metadata) -> io::Result<()> {
        {
            let mut st = self.db.prepare("INSERT INTO metadata (name, value) VALUES (?, ?)").map_err(sql_err)?;
            for (k, v) in metadata {
                if let Err(e) = st.execute(params![k, v]) {
                    // `mbtiles_write_metadata`: with `-F`, the old row stays.
                    eprintln!("set {}: {}", metadata_label(k), sql_msg(&e));
                    if !self.allow_existing {
                        std::process::exit(EXIT_SQLITE);
                    }
                }
            }
        }
        let start = std::time::Instant::now();
        let verbose = std::env::var_os("VTMAKE_VERBOSE").is_some();
        let report = |phase| {
            if verbose { eprintln!("output {phase}: {:.3}s", start.elapsed().as_secs_f64()); }
        };
        self.db.execute_batch("COMMIT;").map_err(sql_err)?;
        report("commit");
        // A complete scan of every index is unnecessary for the tile lookup
        // queries. Bound the work at close, including dirty cache turnover.
        self.db.execute_batch("PRAGMA analysis_limit=1000; ANALYZE;").map_err(sql_err)?;
        report("analyze");
        let result = match &self.pmtiles {
            Some(path) => pmtiles::convert(path, self.db, metadata, self.tile_gzip),
            None => self.db.close().map_err(|(_, e)| sql_err(e)),
        };
        report("close");
        result
    }
}

/// `-e` output: `<dir>/<z>/<x>/<y>.pbf` (XYZ) and `metadata.json`.
pub struct DirWriter {
    root: PathBuf,
    made: std::collections::HashSet<(u8, u32)>,
}

/// The `z/x/y.pbf` and `z/x/y.mvt` files of a tile directory (`enumerate_dirtiles`).
fn dir_tiles(root: &Path) -> io::Result<Vec<PathBuf>> {
    let num = |p: &Path| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()));
    let mut out = Vec::new();
    let Ok(zs) = std::fs::read_dir(root) else { return Ok(out) };
    for z in zs.flatten().map(|e| e.path()).filter(|p| num(p)) {
        for x in std::fs::read_dir(&z)?.flatten().map(|e| e.path()).filter(|p| num(p)) {
            for f in std::fs::read_dir(&x)?.flatten().map(|e| e.path()) {
                let ext = f.extension().and_then(|e| e.to_str());
                let stem_num = f.file_stem().and_then(|n| n.to_str()).is_some_and(|n| n.bytes().all(|c| c.is_ascii_digit()));
                if stem_num && matches!(ext, Some("pbf") | Some("mvt")) {
                    out.push(f);
                }
            }
        }
    }
    Ok(out)
}

impl DirWriter {
    /// With `force`, delete `metadata.json` and the numeric `z/x/y.pbf|.mvt` files
    /// first (spec 9.5). Other files and the directories stay. Without `force`, an
    /// old `metadata.json` or tile is an error (`AlreadyExists`), as in tippecanoe
    /// (`dirtiles.cpp:check_dir`).
    pub fn create(root: &Path, force: bool) -> io::Result<Self> {
        std::fs::create_dir_all(root)?;
        let meta = root.join("metadata.json");
        if force {
            let _ = std::fs::remove_file(&meta);
        } else if meta.exists() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{}: file exists", meta.display())));
        }
        let num = |p: &Path| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.bytes().all(|c| c.is_ascii_digit()));
        for z in std::fs::read_dir(root)?.flatten().map(|e| e.path()).filter(|p| p.is_dir() && num(p)) {
            for x in std::fs::read_dir(&z)?.flatten().map(|e| e.path()).filter(|p| p.is_dir() && num(p)) {
                for f in std::fs::read_dir(&x)?.flatten().map(|e| e.path()) {
                    let ext = f.extension().and_then(|e| e.to_str());
                    let stem_num = f.file_stem().and_then(|n| n.to_str()).is_some_and(|n| n.bytes().all(|c| c.is_ascii_digit()));
                    if stem_num && matches!(ext, Some("pbf") | Some("mvt")) {
                        if !force {
                            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{}: file exists", f.display())));
                        }
                        std::fs::remove_file(&f)?;
                    }
                }
            }
        }
        Ok(DirWriter { root: root.to_path_buf(), made: Default::default() })
    }

    /// The output of `tippecanoe -e` (`check_dir`): `-f` deletes the old
    /// `metadata.json` and tiles. Without `-f`, an old `metadata.json` or tile
    /// is an error, but with `-F` (`allow_existing`) the old tiles and the old
    /// `metadata.json` stay. Errors write tippecanoe's message and stop.
    pub fn open(root: &Path, force: bool, allow_existing: bool, argv0: &str) -> DirWriter {
        let _ = std::fs::create_dir(root);
        let dir = root.display().to_string();
        let meta = format!("{dir}/metadata.json");
        if force {
            let _ = std::fs::remove_file(&meta);
        } else if std::fs::metadata(&meta).is_ok() {
            eprintln!("{argv0}: Tileset \"{dir}\" already exists. You can use --force if you want to delete the old tileset.");
            eprintln!("{argv0}: {meta}: file exists");
            if !allow_existing {
                std::process::exit(EXIT_EXISTS);
            }
        }
        let w = DirWriter { root: root.to_path_buf(), made: Default::default() };
        if allow_existing {
            return w;
        }
        let mut tiles = dir_tiles(root).unwrap_or_else(|e| die(&format!("{dir}: {}", os_msg(&e)), EXIT_OPEN));
        tiles.sort();
        for f in tiles {
            if !force {
                die(&format!("{}: file exists", f.display()), EXIT_EXISTS);
            }
            if let Err(e) = std::fs::remove_file(&f) {
                die(&format!("{}: {}", f.display(), os_msg(&e)), EXIT_UNLINK);
            }
        }
        w
    }
}

impl TileSink for DirWriter {
    fn write_tile(&mut self, z: u8, x: u32, y: u32, data: &[u8]) -> io::Result<()> {
        // `dir_write_tile`: make each folder level (errors do not stop here).
        let dir = self.root.join(z.to_string()).join(x.to_string());
        if self.made.insert((z, x)) {
            let _ = std::fs::create_dir(&self.root);
            let _ = std::fs::create_dir(self.root.join(z.to_string()));
            let _ = std::fs::create_dir(&dir);
        }
        let path = dir.join(format!("{y}.pbf"));
        let mut f = match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => f,
            // Only with `--allow-existing` can a tile be there.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => die(&format!("Can't write tile to already existing {}", path.display()), EXIT_EXISTS),
            Err(e) => die(&format!("{}: {}", path.display(), os_msg(&e)), EXIT_WRITE),
        };
        io::Write::write_all(&mut f, data).or_else(|e| die(&format!("{}: {}", path.display(), os_msg(&e)), EXIT_WRITE))
    }

    fn finish(self: Box<Self>, metadata: &Metadata) -> io::Result<()> {
        // `dir_write_metadata`: an old `metadata.json` (`--allow-existing`) stays.
        if metadata.is_empty() || self.root.join("metadata.json").exists() {
            return Ok(());
        }
        let mut rows: Vec<&(String, String)> = metadata.iter().collect();
        rows.sort_by_key(|(k, _)| DIR_ORDER.iter().position(|o| o == k).unwrap_or(DIR_ORDER.len()));
        let mut s = String::from("{\n");
        for (i, (k, v)) in rows.iter().enumerate() {
            s.push_str(&format!("{}: {}", json_string(k), json_string(v)));
            s.push_str(if i + 1 < metadata.len() { ",\n" } else { "\n" });
        }
        s.push_str("}\n");
        std::fs::write(self.root.join("metadata.json"), s)
    }
}

/// JSON string with tippecanoe's escapes (`\\`, `\"`, control bytes as `\u00xx`).
pub fn json_string(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("vtmake-out-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        let _ = std::fs::remove_file(&p);
        p
    }

    fn meta() -> Metadata {
        ["name", "type", "format", "generator", "strategies", "json"].iter().map(|k| (k.to_string(), "v".to_string())).collect()
    }

    #[test]
    fn mbtiles_tables_as_tippecanoe() {
        let p = tmp("a.mbtiles");
        let mut w: Box<dyn TileSink> = Box::new(MbtilesWriter::create(&p, true).unwrap());
        w.write_tile(1, 0, 0, b"x").unwrap();
        w.finish(&meta()).unwrap();
        let db = Connection::open(&p).unwrap();
        let names: Vec<String> =
            db.prepare("select name from sqlite_master where type = 'table' order by name").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        // No `sqlite_stat4` (the bundled SQLite is built without STAT4, .cargo/config.toml).
        assert_eq!(names, ["images", "map", "metadata", "sqlite_stat1"]);
        let rows: Vec<String> = db.prepare("select name from metadata order by rowid").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        assert_eq!(rows, ["name", "type", "format", "generator", "strategies", "json"]);
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn dir_metadata_order() {
        let p = tmp("dir");
        let w: Box<dyn TileSink> = Box::new(DirWriter::create(&p, true).unwrap());
        w.finish(&meta()).unwrap();
        let s = std::fs::read_to_string(p.join("metadata.json")).unwrap();
        let keys: Vec<&str> = s.lines().filter_map(|l| l.split('"').nth(1)).collect();
        assert_eq!(keys, ["name", "type", "strategies", "format", "generator", "json"]);
        std::fs::remove_dir_all(&p).unwrap();
    }

    #[test]
    fn no_metadata_when_empty() {
        let p = tmp("empty");
        let w: Box<dyn TileSink> = Box::new(DirWriter::create(&p, true).unwrap());
        w.finish(&Vec::new()).unwrap();
        assert!(p.is_dir() && !p.join("metadata.json").exists());
        std::fs::remove_dir_all(&p).unwrap();
    }
}
