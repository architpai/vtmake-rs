//! Compile the vendored zlib 1.3.1 (deflate and inflate only, no gz* file
//! functions). Z_PREFIX and the changed zconf.h give all symbols the prefix
//! `vtmake_z_`, so this copy cannot clash with another zlib in the same binary.

fn main() {
    let files = [
        "adler32.c", "crc32.c", "deflate.c", "inffast.c", "inflate.c", "inftrees.c", "trees.c", "zutil.c",
    ];
    let mut b = cc::Build::new();
    b.include("zlib").define("Z_PREFIX", None).define("HAVE_HIDDEN", None).warnings(false).opt_level(3);
    for f in files {
        b.file(format!("zlib/{f}"));
        println!("cargo:rerun-if-changed=zlib/{f}");
    }
    for h in ["crc32.h", "deflate.h", "inffast.h", "inffixed.h", "inflate.h", "inftrees.h", "trees.h", "zconf.h", "zlib.h", "zutil.h"] {
        println!("cargo:rerun-if-changed=zlib/{h}");
    }
    b.compile("bt_z");
}
