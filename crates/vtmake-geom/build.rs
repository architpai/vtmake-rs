//! With the `cpp-ref` feature: compile tippecanoe's real C++ code (without
//! change) from `TIPPECANOE_SRC` and the test shim `cpp-ref/shim.cpp`, and
//! link them. Without the feature this script does nothing. With the feature
//! but without `TIPPECANOE_SRC`, it prints a warning and builds nothing; the
//! test binaries then do not link.

fn main() {
    #[cfg(feature = "cpp-ref")]
    cpp_ref::build();
}

#[cfg(feature = "cpp-ref")]
mod cpp_ref {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    // The object files of the tippecanoe binary (Makefile), without main.o
    // and tile.o. tile.cpp is included by shim.cpp; main.cpp is compiled
    // alone and its `main` symbol is renamed.
    const SOURCES: &[&str] = &[
        "geojson.cpp",
        "jsonpull/jsonpull.cpp",
        "pool.cpp",
        "mbtiles.cpp",
        "geometry.cpp",
        "projection.cpp",
        "memfile.cpp",
        "mvt.cpp",
        "serial.cpp",
        "platform.cpp",
        "text.cpp",
        "dirtiles.cpp",
        "pmtiles_file.cpp",
        "plugin.cpp",
        "read_json.cpp",
        "write_json.cpp",
        "geobuf.cpp",
        "flatgeobuf.cpp",
        "evaluator.cpp",
        "geocsv.cpp",
        "csv.cpp",
        "geojson-loop.cpp",
        "json_logger.cpp",
        "visvalingam.cpp",
        "compression.cpp",
        "clip.cpp",
        "sort.cpp",
        "attribute.cpp",
        "thread.cpp",
        "shared_borders.cpp",
        "usage.cpp",
        "clipper2/src/clipper.engine.cpp",
        "main.cpp",
    ];

    fn base(src: &Path) -> cc::Build {
        // The Release flags of the tippecanoe Makefile: -O3 -DNDEBUG -std=c++17.
        let mut b = cc::Build::new();
        b.cpp(true)
            .compiler("g++")
            .opt_level(3)
            .debug(false)
            .warnings(false)
            .extra_warnings(false)
            .flag("-std=c++17")
            .flag("-fPIE")
            .define("NDEBUG", None)
            .define("BUILD_INFO", "")
            .include(src)
            .include(src.join("clipper2/include"))
            .cargo_metadata(false);
        b
    }

    pub fn build() {
        println!("cargo:rerun-if-env-changed=TIPPECANOE_SRC");
        println!("cargo:rerun-if-changed=build.rs");
        let Some(src) = std::env::var_os("TIPPECANOE_SRC") else {
            println!("cargo:warning=TIPPECANOE_SRC is not set: the C++ reference is not built");
            return;
        };
        let src = PathBuf::from(src);
        let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let shim = manifest.join("cpp-ref/shim.cpp");
        println!("cargo:rerun-if-changed={}", shim.display());

        let mut files: Vec<PathBuf> = SOURCES.iter().map(|f| src.join(f)).collect();
        files.push(shim);

        // Compile the files in parallel (one thread for each file).
        let objects: Vec<Vec<PathBuf>> = std::thread::scope(|s| {
            let handles: Vec<_> = files
                .iter()
                .map(|f| {
                    let src = &src;
                    s.spawn(move || base(src).file(f).compile_intermediates())
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        let mut all = Vec::new();
        for (f, objs) in files.iter().zip(objects) {
            for o in objs {
                if f.ends_with("main.cpp") {
                    // The test binary has its own `main`.
                    let st = Command::new("objcopy")
                        .arg("--redefine-sym")
                        .arg("main=tippecanoe_main")
                        .arg(&o)
                        .status()
                        .expect("run objcopy");
                    assert!(st.success(), "objcopy failed");
                }
                all.push(o);
            }
        }

        let mut ar = base(&src);
        ar.cargo_metadata(true);
        ar.objects(&all).compile("tcref");
        println!("cargo:rustc-link-lib=z");
        println!("cargo:rustc-link-lib=sqlite3");
        println!("cargo:rustc-link-lib=pthread");
    }
}
