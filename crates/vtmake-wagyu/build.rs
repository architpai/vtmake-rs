fn main() {
    println!("cargo:rerun-if-changed=src/shim.cpp");
    println!("cargo:rerun-if-changed=src/shim_clip.cpp");
    println!("cargo:rerun-if-changed=vendor");
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .include("vendor")
        .file("src/shim.cpp")
        .file("src/shim_clip.cpp")
        .warnings(false)
        .compile("vtmake_wagyu");
    if std::env::var_os("CARGO_FEATURE_REFERENCE").is_some() {
        println!("cargo:rerun-if-changed=tests/reference.cpp");
        println!("cargo:rerun-if-env-changed=TIPPECANOE_SRC");
        let Ok(source) = std::env::var("TIPPECANOE_SRC") else {
            println!("cargo:warning=TIPPECANOE_SRC is not set: the C++ reference is not built");
            return;
        };
        cc::Build::new()
            .cpp(true)
            .std("c++17")
            .include(&source)
            .include(format!("{source}/clipper2/include"))
            .flag("-ffunction-sections")
            .flag("-fdata-sections")
            .file(format!("{source}/clip.cpp"))
            .file("tests/reference.cpp")
            .warnings(false)
            .compile("vtmake_wagyu_reference");
    }
}
