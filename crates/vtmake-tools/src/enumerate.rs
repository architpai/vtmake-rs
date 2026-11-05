//! Ported from tippecanoe's enumerate.cpp. Copyright (c) 2022, Protomaps LLC;
//! Copyright (c) 2014, Mapbox Inc. BSD 2-Clause: LICENSE-tippecanoe.md.
use crate::common::*;
use rusqlite::Connection;

pub fn main(argv: Vec<String>) {
    let usage = || {
        eprintln!("Usage: {} file.mbtiles ...", argv[0]);
        std::process::exit(EXIT_ARGS);
    };
    let p = crate::getopt::parse(&argv, &[], &[], &usage);
    if p.operands.is_empty() {
        usage();
    }
    for name in p.operands {
        if let Err(e) = enumerate(&name) {
            e.exit();
        }
    }
}

fn enumerate(name: &str) -> Result<(), Fatal> {
    let db =
        Connection::open(name).or_else(|e| fatal(format!("{name}: {}", sql_msg(&e)), EXIT_OPEN))?;
    db.execute_batch("PRAGMA integrity_check;").or_else(|e| {
        fatal(
            format!("{name}: integrity_check: {}", sql_msg(&e)),
            EXIT_SQLITE,
        )
    })?;
    {
        let mut st = db.prepare("SELECT zoom_level, tile_column, tile_row from tiles order by zoom_level, tile_column, tile_row;")
            .or_else(|e| fatal(format!("{name}: select failed: {}", sql_msg(&e)), EXIT_SQLITE))?;
        let mut rows = st.query([]).or_else(|e| {
            fatal(
                format!("{name}: select failed: {}", sql_msg(&e)),
                EXIT_SQLITE,
            )
        })?;
        while let Ok(Some(row)) = rows.next() {
            let z = row.get_ref(0).map(col_int).unwrap_or(0);
            let x = row.get_ref(1).map(col_int).unwrap_or(0);
            let y = row.get_ref(2).map(col_int).unwrap_or(0);
            if !(0..=31).contains(&z) {
                return fatal(
                    format!("Corrupt mbtiles file: impossible zoom level {z}"),
                    EXIT_IMPOSSIBLE,
                );
            }
            println!("{name} {z} {x} {}", (1i64 << z) - 1 - y as i64);
        }
    }
    db.close().or_else(|(_, e)| {
        fatal(
            format!("{name}: could not close database: {}", sql_msg(&e)),
            EXIT_CLOSE,
        )
    })
}
