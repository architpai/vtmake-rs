fn main() {
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
            .include("vendor")
            .include(&source)
            .flag("-ffunction-sections")
            .flag("-fdata-sections")
            .file("tests/reference.cpp")
            .file(format!("{source}/jsonpull/jsonpull.cpp"))
            .file(format!("{source}/read_json.cpp"))
            .file(format!("{source}/text.cpp"))
            .warnings(false)
            .compile("bt_parse_reference");
    }
}
