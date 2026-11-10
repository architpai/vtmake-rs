//! The command line of `vtmake build`: all options of felt's `tippecanoe`
//! (`main.cpp:main`), with its getopt rules, defaults, checks, messages and
//! exit codes. [`Options`] keeps all settings. [`check_supported`] stops with
//! exit 101 if an option asks for an effect that vtmake does not have yet.
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use vtmake_tools::getopt::{GetoptLong, Got, LongOpt};

/// `EXIT_ARGS` and the other codes of `errors.hpp`.
pub const EXIT_ARGS: i32 = 101;
const EXIT_OPEN: i32 = 111;
const EXIT_JSON: i32 = 107;
/// `MAX_ZOOM` (`main.hpp`).
pub const MAX_ZOOM: i32 = 24;

#[derive(Debug, Clone)]
pub enum Output {
    Mbtiles(PathBuf),
    Dir(PathBuf),
}

/// `-s`: the projection of the input coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    Epsg4326,
    Epsg3857,
}

impl Projection {
    /// The `crs` name that `check_crs` expects.
    pub fn alias(self) -> &'static str {
        match self {
            Projection::Epsg4326 => "urn:ogc:def:crs:OGC:1.3:CRS84",
            Projection::Epsg3857 => "urn:ogc:def:crs:EPSG::3857",
        }
    }
}

/// One input (`struct source`): an operand, `-L` or standard input (`file` "").
#[derive(Debug, Clone, Default)]
pub struct Source {
    pub layer: String,
    pub file: String,
    pub description: String,
    pub format: String,
}

/// `-T` types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeOpt {
    Int,
    Float,
    String,
    Bool,
}

/// `-E` methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accum {
    Sum,
    Product,
    Mean,
    Max,
    Min,
    Concat,
    Comma,
    Count,
}

/// `--set-attribute` values: the type (number or string) and the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetValue {
    pub ty: vtmake_core::AttrType,
    pub text: String,
}

/// All settings of the command line, with tippecanoe's defaults.
#[derive(Debug, Clone)]
pub struct Options {
    pub only_tiles: Option<String>,
    pub known_hashes: Option<String>,
    pub hashes_out: Option<String>,
    pub argv: Vec<String>,
    pub output: Output,
    pub force: bool,
    pub allow_existing: bool,
    pub name: Option<String>,
    pub description: Option<String>,
    pub attribution: Option<String>,
    pub layer: Option<String>,
    /// `-L` sources, then the operands, or standard input.
    pub sources: Vec<Source>,
    pub read_parallel: bool,
    pub projection: Projection,
    pub maxzoom: i32,
    pub minzoom: i32,
    pub guess_maxzoom: bool,
    pub minimum_maxzoom: i32,
    pub extend_zooms_max: i64,
    /// `--one-tile`: (x, y) at the zoom `minzoom` = `maxzoom`.
    pub one_tile: Option<(u32, u32)>,
    pub full_detail: i32,
    pub low_detail: i32,
    pub min_detail: i32,
    pub extra_detail: i32,
    pub exclude: BTreeSet<String>,
    pub include: BTreeSet<String>,
    pub exclude_all: bool,
    pub attribute_types: BTreeMap<String, TypeOpt>,
    pub attribute_descriptions: BTreeMap<String, String>,
    pub attribute_accum: BTreeMap<String, Accum>,
    pub attribute_for_id: Option<String>,
    pub set_attributes: BTreeMap<String, SetValue>,
    pub maximum_string_attribute_length: i64,
    /// `-j` or `-J`: the filter as JSON text.
    pub filter: Option<String>,
    pub droprate: f64,
    pub retain_points_multiplier: i64,
    pub basezoom: i32,
    pub basezoom_marker_width: f64,
    pub drop_denser: i32,
    pub cluster_distance: i32,
    pub cluster_maxzoom: i32,
    pub guess_cluster_maxzoom: bool,
    pub preserve_point_density_threshold: i64,
    pub preserve_multiplier_density_threshold: i64,
    pub drop_by_attribute: Option<String>,
    pub drop_by_attribute_descending: bool,
    pub gamma: f64,
    pub simplification: f64,
    pub maxzoom_simplification: f64,
    pub tiny_polygon_size: i32,
    pub buffer: i32,
    /// `--order-by` and the others: (attribute, descending). `"\u{80}size"`
    /// stands for the feature size (`ORDER_BY_SIZE`).
    pub order_by: Vec<(String, bool)>,
    pub order_by_size: bool,
    pub clip_bboxes: Vec<[f64; 4]>,
    pub prefilter: Option<String>,
    pub postfilter: Option<String>,
    pub max_tile_size: i64,
    pub max_tile_features: i64,
    pub limit_tile_feature_count: i64,
    pub limit_tile_feature_count_at_maxzoom: i64,
    pub tilestats_attributes_limit: i64,
    pub tilestats_sample_values_limit: i64,
    pub tilestats_values_limit: i64,
    pub tmpdir: String,
    pub quiet: bool,
    pub quiet_progress: bool,
    pub json_progress: bool,
    pub progress_interval: f64,
    pub unidecode_data: Option<String>,
    /// `--pgcopy-schema`: the schema (JSON text or a file name) of the
    /// Postgres binary COPY input.
    pub pgcopy_schema: Option<String>,
    /// `-p` letters (`prevent[]`) and `-a` letters (`additional[]`).
    pub prevent: [bool; 128],
    pub additional: [bool; 128],
    /// `geometry_scale` (`main.cpp:main`).
    pub geometry_scale: i32,
    /// The options given (as `--name` or `-pX`), in order, for the check of
    /// the supported set.
    pub given: Vec<String>,
}

#[derive(Clone, Copy)]
enum Act {
    Heading,
    /// A short option letter (`val`).
    Short(char),
    /// A flag in `prevent[]` or `additional[]`.
    Prevent(u8),
    Additional(u8),
    /// `val` `'~'`: the long name selects the action.
    Tilde,
}

struct Opt {
    name: &'static str,
    arg: bool,
    act: Act,
}

const fn h(name: &'static str) -> Opt {
    Opt { name, arg: false, act: Act::Heading }
}
const fn s(name: &'static str, arg: bool, c: char) -> Opt {
    Opt { name, arg, act: Act::Short(c) }
}
const fn t(name: &'static str) -> Opt {
    Opt { name, arg: true, act: Act::Tilde }
}
const fn t0(name: &'static str) -> Opt {
    Opt { name, arg: false, act: Act::Tilde }
}
const fn pv(name: &'static str, c: u8) -> Opt {
    Opt { name, arg: false, act: Act::Prevent(c) }
}
const fn ad(name: &'static str, c: u8) -> Opt {
    Opt { name, arg: false, act: Act::Additional(c) }
}

/// `long_options_orig` (`main.cpp`), with the usage headings.
static OPTIONS: &[Opt] = &[
    h("Output tileset"),
    s("output", true, 'o'),
    s("output-to-directory", true, 'e'),
    s("force", false, 'f'),
    s("allow-existing", false, 'F'),
    h("Tileset description and attribution"),
    s("name", true, 'n'),
    s("attribution", true, 'A'),
    s("description", true, 'N'),
    h("Input files and layer names"),
    s("layer", true, 'l'),
    s("named-layer", true, 'L'),
    h("Parallel processing of input"),
    s("read-parallel", false, 'P'),
    t("only-tiles"),
    t("known-hashes"),
    t("hashes-out"),
    h("Projection of input"),
    s("projection", true, 's'),
    h("Zoom levels"),
    s("maximum-zoom", true, 'z'),
    s("minimum-zoom", true, 'Z'),
    t("smallest-maximum-zoom-guess"),
    ad("extend-zooms-if-still-dropping", b'e'),
    t("extend-zooms-if-still-dropping-maximum"),
    ad("generate-variable-depth-tile-pyramid", b't'),
    s("one-tile", true, 'R'),
    h("Tile resolution"),
    s("full-detail", true, 'd'),
    s("low-detail", true, 'D'),
    s("minimum-detail", true, 'm'),
    t("extra-detail"),
    h("Filtering feature attributes"),
    s("exclude", true, 'x'),
    s("include", true, 'y'),
    s("exclude-all", false, 'X'),
    h("Modifying feature attributes"),
    s("attribute-type", true, 'T'),
    s("attribute-description", true, 'Y'),
    s("accumulate-attribute", true, 'E'),
    pv("empty-csv-columns-are-null", b'e'),
    ad("convert-stringified-ids-to-numbers", b'I'),
    t("use-attribute-for-id"),
    pv("single-precision", b'N'),
    t("set-attribute"),
    t("maximum-string-attribute-length"),
    h("Filtering features by attributes"),
    s("feature-filter-file", true, 'J'),
    s("feature-filter", true, 'j'),
    h("Dropping a fixed fraction of features by zoom level"),
    s("drop-rate", true, 'r'),
    t("retain-points-multiplier"),
    s("base-zoom", true, 'B'),
    t("drop-denser"),
    pv("limit-base-zoom-to-maximum-zoom", b'b'),
    ad("drop-lines", b'l'),
    ad("drop-polygons", b'p'),
    s("cluster-distance", true, 'K'),
    s("cluster-maxzoom", true, 'k'),
    t("preserve-point-density-threshold"),
    t("preserve-multiplier-density-threshold"),
    h("Dropping or merging a fraction of features to keep under tile size limits"),
    ad("drop-densest-as-needed", b's'),
    ad("drop-fraction-as-needed", b'd'),
    ad("drop-smallest-as-needed", b'n'),
    t("drop-by-attribute-as-needed"),
    t("drop-by-attribute-order"),
    ad("coalesce-densest-as-needed", b'S'),
    ad("coalesce-fraction-as-needed", b'D'),
    ad("coalesce-smallest-as-needed", b'N'),
    pv("force-feature-limit", b'd'),
    ad("cluster-densest-as-needed", b'C'),
    ad("keep-point-cluster-position", b'a'),
    h("Dropping tightly overlapping features"),
    s("gamma", true, 'g'),
    ad("increase-gamma-as-needed", b'G'),
    h("Line and polygon simplification"),
    s("simplification", true, 'S'),
    pv("no-line-simplification", b's'),
    pv("simplify-only-low-zooms", b'S'),
    t("simplification-at-maximum-zoom"),
    pv("no-tiny-polygon-reduction", b't'),
    pv("no-tiny-polygon-reduction-at-maximum-zoom", b'T'),
    t("tiny-polygon-size"),
    pv("no-simplification-of-shared-nodes", b'n'),
    ad("visvalingam", b'v'),
    h("Attempts to improve shared polygon boundaries"),
    ad("detect-shared-borders", b'b'),
    ad("grid-low-zooms", b'L'),
    h("Controlling clipping to tile boundaries"),
    s("buffer", true, 'b'),
    pv("no-clipping", b'c'),
    pv("no-duplication", b'D'),
    h("Reordering features within each tile"),
    pv("preserve-input-order", b'i'),
    ad("reorder", b'o'),
    ad("coalesce", b'c'),
    ad("reverse", b'r'),
    ad("hilbert", b'h'),
    t("order-by"),
    t("order-descending-by"),
    t0("order-smallest-first"),
    t0("order-largest-first"),
    h("Adding calculated attributes"),
    ad("calculate-feature-density", b'g'),
    ad("generate-ids", b'i'),
    ad("calculate-feature-index", b'X'),
    h("Trying to correct bad source geometry"),
    ad("detect-longitude-wraparound", b'w'),
    pv("use-source-polygon-winding", b'w'),
    pv("reverse-source-polygon-winding", b'W'),
    t("clip-bounding-box"),
    ad("convert-polygons-to-label-points", b'P'),
    h("Filtering tile contents"),
    s("prefilter", true, 'C'),
    s("postfilter", true, 'c'),
    h("Setting or disabling tile size limits"),
    s("maximum-tile-bytes", true, 'M'),
    s("maximum-tile-features", true, 'O'),
    t("limit-tile-feature-count"),
    t("limit-tile-feature-count-at-maximum-zoom"),
    pv("no-feature-limit", b'f'),
    pv("no-tile-size-limit", b'k'),
    pv("no-tile-compression", b'C'),
    pv("no-tile-stats", b'g'),
    t("tile-stats-attributes-limit"),
    t("tile-stats-sample-values-limit"),
    t("tile-stats-values-limit"),
    h("Temporary storage"),
    s("temporary-directory", true, 't'),
    h("Progress indicator"),
    s("quiet", false, 'q'),
    s("no-progress-indicator", false, 'Q'),
    s("progress-interval", true, 'U'),
    s("json-progress", false, 'u'),
    h("Version"),
    s("version", false, 'v'),
    h(""),
    s("prevent", true, 'p'),
    s("additional", true, 'a'),
    ad("check-polygons", b'@'),
    pv("no-polygon-splitting", b'p'),
    ad("prefer-radix-sort", b'R'),
    t("unidecode-data"),
    s("help", false, 'H'),
    // vtmake: read Postgres binary COPY input (docs/pgcopy.md).
    t("pgcopy-schema"),
];

/// `print_usage` (`usage.cpp`) for tippecanoe's table.
fn usage_text(argv0: &str) -> String {
    let mut o = format!("Usage: {argv0} [options] [file.json ...]");
    let mut width = 80usize;
    let mut alternation_listed = false;
    for opt in OPTIONS.iter().take_while(|o| !o.name.is_empty()) {
        if let Act::Heading = opt.act {
            o += &format!("\n  {}\n{:8}", opt.name, "");
            width = 8;
            continue;
        }
        let text = match opt.name {
            "output" | "output-to-directory" => {
                if alternation_listed {
                    continue;
                }
                alternation_listed = true;
                "(--output=output.mbtiles | --output-to-directory=directory)".to_string()
            }
            n if opt.arg => format!("[--{n}=...]"),
            n => format!("[--{n}]"),
        };
        if width + 1 + text.len() >= 80 {
            o += &format!("\n{:8}", "");
            width = 8;
        }
        o += &format!(" {text}");
        width += 1 + text.len();
    }
    o.push('\n');
    o
}

/// `usage(argv, status)`.
fn usage(argv0: &str, status: i32) -> ! {
    eprint!("{}", usage_text(argv0));
    std::process::exit(status)
}

fn fail(msg: impl std::fmt::Display, code: i32) -> ! {
    eprintln!("{msg}");
    std::process::exit(code)
}

/// `strtol`/`strtoll` in base 10: the value (saturated) and the bytes used.
fn strtoll(s: &str) -> (i64, usize) {
    let b = s.as_bytes();
    let lead = b.iter().take_while(|&&c| matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')).count();
    let sign = usize::from(matches!(b.get(lead), Some(b'+' | b'-')));
    let digits = b[lead + sign..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return (0, 0);
    }
    let neg = b[lead] == b'-';
    let mut v: i128 = 0;
    for &c in &b[lead + sign..lead + sign + digits] {
        v = (v * 10 + (c - b'0') as i128).min(i64::MAX as i128 + 1);
    }
    let v = if neg { (-v).max(i64::MIN as i128) } else { v.min(i64::MAX as i128) };
    (v as i64, lead + sign + digits)
}

/// C `strtod`: the value and the bytes used.
fn strtod(s: &str) -> (f64, usize) {
    let c = std::ffi::CString::new(s.as_bytes().split(|&b| b == 0).next().unwrap_or(b"")).unwrap();
    unsafe extern "C" {
        fn strtod(s: *const std::ffi::c_char, end: *mut *mut std::ffi::c_char) -> f64;
    }
    let mut end: *mut std::ffi::c_char = std::ptr::null_mut();
    // SAFETY: `c` is a NUL-terminated string that lives during the call; `end`
    // points into it after the call.
    let v = unsafe { strtod(c.as_ptr(), &mut end) };
    (v, end as usize - c.as_ptr() as usize)
}

/// C `atoi`/`atoll`.
fn atoll(s: &str) -> i64 {
    strtoll(s).0
}

fn atoi(s: &str) -> i32 {
    atoll(s) as i32
}

/// The tippecanoe that `-v` names.
pub const FELT_VERSION: &str = "v2.82.0";

struct P<'a> {
    argv0: &'a str,
}

impl P<'_> {
    fn number_error(&self, what: &str, s: &str) -> ! {
        fail(format!("{}: {what} must be a number (got {s})", self.argv0), EXIT_ARGS)
    }

    /// `atoi_require` (`strtol` gives a long; the cast to int keeps the low bits).
    fn atoi_require(&self, s: &str, what: &str) -> i32 {
        let (v, n) = strtoll(s);
        if s.is_empty() || n != s.len() {
            self.number_error(what, s)
        }
        v as i32
    }

    fn atoll_require(&self, s: &str, what: &str) -> i64 {
        let (v, n) = strtoll(s);
        if s.is_empty() || n != s.len() {
            self.number_error(what, s)
        }
        v
    }

    fn atof_require(&self, s: &str, what: &str) -> f64 {
        let (v, n) = strtod(s);
        if s.is_empty() || n != s.len() {
            self.number_error(what, s)
        }
        v
    }
}

/// Parse a JSON object option (`-L`, `-E`, `--set-attribute` in JSON form).
fn json_tree(text: &str) -> Result<(vtmake_parse::pull::Pull, u32), &'static str> {
    vtmake_parse::pull::parse_tree(text.as_bytes())
}

fn json_string(p: &vtmake_parse::pull::Pull, o: u32) -> String {
    String::from_utf8_lossy(p.text(o)).into_owned()
}

/// `set_attribute_accum` for one name and method.
fn accum(name: &str, ty: &str, map: &mut BTreeMap<String, Accum>) {
    let t = match ty {
        "sum" => Accum::Sum,
        "product" => Accum::Product,
        "mean" => Accum::Mean,
        "max" => Accum::Max,
        "min" => Accum::Min,
        "concat" => Accum::Concat,
        "comma" => Accum::Comma,
        "count" => Accum::Count,
        _ => fail(format!("Attribute method ({ty}) must be sum, product, mean, max, min, concat, comma, or count"), EXIT_ARGS),
    };
    map.entry(name.to_string()).or_insert(t);
}

pub fn parse(argv: Vec<String>) -> Options {
    use vtmake_parse::pull::Kind;
    let argv0 = argv.first().cloned().unwrap_or_else(|| "tippecanoe".into());
    if argv.len() == 1 {
        usage(&argv0, EXIT_ARGS);
    }
    let p = P { argv0: &argv0 };
    let table: Vec<&Opt> = OPTIONS.iter().filter(|o| !matches!(o.act, Act::Heading)).collect();
    let longs: Vec<LongOpt> = table
        .iter()
        .map(|o| LongOpt {
            name: o.name,
            has_arg: o.arg,
            ident: match o.act {
                Act::Short(c) => c as u32,
                Act::Tilde => '~' as u32,
                Act::Prevent(c) => 0x1000 + c as u32,
                Act::Additional(c) => 0x2000 + c as u32,
                Act::Heading => unreachable!(),
            },
        })
        .collect();
    // `getopt_string`: the letters of the entries with `val > ' '` ('~' too).
    let mut optstring = String::new();
    for o in &table {
        match o.act {
            Act::Short(c) => optstring.push(c),
            Act::Tilde => optstring.push('~'),
            _ => continue,
        }
        if o.arg {
            optstring.push(':');
        }
    }

    let mut out_mbtiles: Option<String> = None;
    let mut out_dir: Option<String> = None;
    let mut o = Options {
        argv: argv.clone(),
        output: Output::Dir(PathBuf::new()),
        force: false,
        allow_existing: false,
        name: None,
        description: None,
        attribution: None,
        layer: None,
        sources: Vec::new(),
        read_parallel: false,
        projection: Projection::Epsg4326,
        maxzoom: 14,
        minzoom: 0,
        guess_maxzoom: false,
        minimum_maxzoom: 0,
        only_tiles: None,
        known_hashes: None,
        hashes_out: None,
        extend_zooms_max: 0,
        one_tile: None,
        full_detail: -1,
        low_detail: 12,
        min_detail: 7,
        extra_detail: -1,
        exclude: BTreeSet::new(),
        include: BTreeSet::new(),
        exclude_all: false,
        attribute_types: BTreeMap::new(),
        attribute_descriptions: BTreeMap::new(),
        attribute_accum: BTreeMap::new(),
        attribute_for_id: None,
        set_attributes: BTreeMap::new(),
        maximum_string_attribute_length: 0,
        filter: None,
        droprate: 2.5,
        retain_points_multiplier: 1,
        basezoom: -1,
        basezoom_marker_width: 1.0,
        drop_denser: 0,
        cluster_distance: 0,
        cluster_maxzoom: MAX_ZOOM,
        guess_cluster_maxzoom: false,
        preserve_point_density_threshold: 0,
        preserve_multiplier_density_threshold: 0,
        drop_by_attribute: None,
        drop_by_attribute_descending: false,
        gamma: 0.0,
        simplification: 1.0,
        maxzoom_simplification: -1.0,
        tiny_polygon_size: 2,
        buffer: 5,
        order_by: Vec::new(),
        order_by_size: false,
        clip_bboxes: Vec::new(),
        prefilter: None,
        postfilter: None,
        max_tile_size: 500000,
        max_tile_features: 200000,
        limit_tile_feature_count: 0,
        limit_tile_feature_count_at_maxzoom: 0,
        tilestats_attributes_limit: 1000,
        tilestats_sample_values_limit: 1000,
        tilestats_values_limit: 100,
        tmpdir: "/tmp".into(),
        quiet: false,
        quiet_progress: false,
        json_progress: false,
        progress_interval: 0.0,
        unidecode_data: None,
        pgcopy_schema: None,
        prevent: [false; 128],
        additional: [false; 128],
        geometry_scale: 0,
        given: Vec::new(),
    };

    let mut g = GetoptLong::new(&argv, &optstring, &longs);
    // `option_index` keeps the last long option (a short `-~` uses it).
    let mut option_index = 0usize;
    while let Some(got) = g.next_opt() {
        let (c, value) = match got {
            Got::Error => usage(&argv0, EXIT_ARGS),
            Got::Long(k, v) => {
                option_index = k;
                match table[k].act {
                    Act::Prevent(c) => {
                        o.prevent[c as usize] = true;
                        o.given.push(format!("--{}", table[k].name));
                        continue;
                    }
                    Act::Additional(c) => {
                        o.additional[c as usize] = true;
                        o.given.push(format!("--{}", table[k].name));
                        continue;
                    }
                    Act::Short(c) => (c, v),
                    Act::Tilde => ('~', v),
                    Act::Heading => unreachable!(),
                }
            }
            Got::Short(c, v) => (c, v),
        };
        let v = value.unwrap_or_default();
        let argv0 = argv0.as_str();
        // The name for the check of the supported set.
        let given = if c == '~' {
            format!("--{}", table[option_index].name)
        } else {
            let long = table.iter().find(|t| matches!(t.act, Act::Short(x) if x == c)).map_or("", |t| t.name);
            format!("--{long}")
        };
        match c {
            '~' => {
                let opt = table[option_index].name;
                match opt {
                    "only-tiles" => o.only_tiles = Some(v.clone()),
                    "known-hashes" => o.known_hashes = Some(v.clone()),
                    "hashes-out" => o.hashes_out = Some(v.clone()),
                    "tile-stats-attributes-limit" => o.tilestats_attributes_limit = atoll(&v) as i32 as i64,
                    "tile-stats-sample-values-limit" => o.tilestats_sample_values_limit = atoll(&v) as i32 as i64,
                    "tile-stats-values-limit" => o.tilestats_values_limit = atoll(&v) as i32 as i64,
                    "pgcopy-schema" => o.pgcopy_schema = Some(v.clone()),
                    "clip-bounding-box" => {
                        // `sscanf("%lf,%lf,%lf,%lf")`.
                        let mut rest = v.as_str();
                        let mut c4 = [0.0; 4];
                        let mut n = 0;
                        while n < 4 {
                            let r = rest.trim_start();
                            let (val, used) = strtod(r);
                            if used == 0 {
                                break;
                            }
                            c4[n] = val;
                            n += 1;
                            rest = &r[used..];
                            if n < 4 {
                                match rest.strip_prefix(',') {
                                    Some(x) => rest = x,
                                    None => break,
                                }
                            }
                        }
                        if n != 4 {
                            fail(format!("{argv0}: Can't parse bounding box --{opt}={v}"), EXIT_ARGS);
                        }
                        o.clip_bboxes.push(c4);
                    }
                    "use-attribute-for-id" => o.attribute_for_id = Some(v.clone()),
                    "set-attribute" => set_attribute(&mut o, &v, argv0),
                    "smallest-maximum-zoom-guess" => {
                        o.maxzoom = MAX_ZOOM;
                        o.guess_maxzoom = true;
                        o.minimum_maxzoom = p.atoi_require(&v, "Minimum maxzoom");
                        if o.minimum_maxzoom > MAX_ZOOM {
                            fail(format!("{argv0}: {v}: minimum maxzoom can be at most {MAX_ZOOM}"), EXIT_ARGS);
                        }
                    }
                    "tiny-polygon-size" => o.tiny_polygon_size = atoi(&v),
                    "extra-detail" => {
                        o.extra_detail = p.atoi_require(&v, "Extra detail");
                        if o.extra_detail > 30 {
                            fail(format!("{argv0}: --extra-detail can be at most 30"), EXIT_ARGS);
                        }
                    }
                    "order-by" => o.order_by.push((v.clone(), false)),
                    "order-descending-by" => o.order_by.push((v.clone(), true)),
                    "order-smallest-first" => {
                        o.order_by.push(("\u{80}size".into(), false));
                        o.order_by_size = true;
                    }
                    "order-largest-first" => {
                        o.order_by.push(("\u{80}size".into(), true));
                        o.order_by_size = true;
                    }
                    "drop-by-attribute-as-needed" => {
                        if v.is_empty() {
                            fail(format!("{argv0}: --drop-by-attribute-as-needed requires a non-empty attribute name"), EXIT_ARGS);
                        }
                        o.drop_by_attribute = Some(v.clone());
                        o.additional[b'A' as usize] = true;
                    }
                    "drop-by-attribute-order" => match v.as_str() {
                        "asc" => o.drop_by_attribute_descending = false,
                        "desc" => o.drop_by_attribute_descending = true,
                        _ => fail(format!("{argv0}: {v}: order must be one of: asc, desc"), EXIT_ARGS),
                    },
                    "simplification-at-maximum-zoom" => {
                        o.maxzoom_simplification = p.atof_require(&v, "Mazoom simplification");
                        if o.maxzoom_simplification <= 0.0 {
                            fail(format!("{argv0}: --simplification-at-maximum-zoom must be > 0"), EXIT_ARGS);
                        }
                    }
                    "limit-tile-feature-count" => o.limit_tile_feature_count = p.atoll_require(&v, "Limit tile feature count"),
                    "limit-tile-feature-count-at-maximum-zoom" => o.limit_tile_feature_count_at_maxzoom = p.atoll_require(&v, "Limit tile feature count at maxzoom"),
                    "drop-denser" => {
                        o.drop_denser = p.atoi_require(&v, "Drop denser rate");
                        // `unsigned int`: a negative value is large.
                        if o.drop_denser as u32 > 100 {
                            fail(format!("{argv0}: --drop-denser can be at most 100"), EXIT_ARGS);
                        }
                    }
                    "preserve-point-density-threshold" => o.preserve_point_density_threshold = p.atoll_require(&v, "Preserve point density threshold"),
                    "preserve-multiplier-density-threshold" => o.preserve_multiplier_density_threshold = p.atoll_require(&v, "Preserve multiplier density threshold"),
                    "extend-zooms-if-still-dropping-maximum" => o.extend_zooms_max = p.atoll_require(&v, "Maximum number by which to extend zooms"),
                    "retain-points-multiplier" => o.retain_points_multiplier = p.atoll_require(&v, "Multiply the fraction of points retained by zoom level") as i32 as i64,
                    "unidecode-data" => {
                        if let Err(e) = std::fs::File::open(&v) {
                            fail(format!("{v}: {}", crate::os_error(&e)), EXIT_OPEN);
                        }
                        o.unidecode_data = Some(v.clone());
                    }
                    "maximum-string-attribute-length" => o.maximum_string_attribute_length = p.atoll_require(&v, "Maximum string attribute length"),
                    _ => fail(format!("{argv0}: Unrecognized option --{opt}"), EXIT_ARGS),
                }
            }
            'n' => o.name = Some(v.clone()),
            'N' => o.description = Some(v.clone()),
            'l' => o.layer = Some(v.clone()),
            'A' => o.attribution = Some(v.clone()),
            'L' => {
                let mut src = Source::default();
                if v.starts_with('{') {
                    let (j, tree) = json_tree(&v).unwrap_or_else(|e| fail(format!("{argv0}: -L{v}: {e}"), EXIT_JSON));
                    if j.kind(tree) != Kind::Hash {
                        fail(format!("{argv0}: -L{v}: not a JSON object"), EXIT_JSON);
                    }
                    match j.hash_get(tree, "file") {
                        Some(f) if j.kind(f) == Kind::String => src.file = json_string(&j, f),
                        _ => fail(format!("{argv0}: -L{v}: requires \"file\": filename"), EXIT_JSON),
                    }
                    for (key, field) in [("layer", &mut src.layer), ("description", &mut src.description), ("format", &mut src.format)] {
                        if let Some(x) = j.hash_get(tree, key)
                            && j.kind(x) == Kind::String
                        {
                            *field = json_string(&j, x);
                        }
                    }
                } else {
                    match v.find(':') {
                        Some(k) if k > 0 => {
                            src.layer = v[..k].to_string();
                            src.file = v[k + 1..].to_string();
                        }
                        _ => fail(format!("{argv0}: -L requires layername:file"), EXIT_ARGS),
                    }
                }
                o.sources.push(src);
            }
            'z' => {
                if v == "g" {
                    o.maxzoom = MAX_ZOOM;
                    o.guess_maxzoom = true;
                } else {
                    o.maxzoom = p.atoi_require(&v, "Maxzoom");
                }
            }
            'Z' => o.minzoom = p.atoi_require(&v, "Minzoom"),
            'R' => {
                // `sscanf("%u/%u/%u")`.
                let parts: Vec<&str> = v.splitn(3, '/').collect();
                let num = |s: &str| -> Option<(u32, usize)> {
                    let (n, used) = strtoll(s.trim_start());
                    (used > 0).then_some((n as u32, used + s.len() - s.trim_start().len()))
                };
                let ok = parts.len() == 3
                    && num(parts[0]).is_some_and(|(_, u)| u == parts[0].len())
                    && num(parts[1]).is_some_and(|(_, u)| u == parts[1].len())
                    && num(parts[2]).is_some();
                if !ok {
                    fail("--one-tile argument must be z/x/y", EXIT_ARGS);
                }
                let z = num(parts[0]).unwrap().0;
                o.minzoom = z as i32;
                o.maxzoom = z as i32;
                o.one_tile = Some((num(parts[1]).unwrap().0, num(parts[2]).unwrap().0));
            }
            'B' => {
                if v == "g" {
                    o.basezoom = -2;
                } else if v.starts_with('g') || v.starts_with('f') {
                    o.basezoom = -2;
                    let w = p.atof_require(&v[1..], "Marker width");
                    o.basezoom_marker_width = if v.starts_with('g') { w } else { (50000.0 / w).sqrt() };
                    if o.basezoom_marker_width == 0.0 || w == 0.0 {
                        fail(format!("{argv0}: Must specify value >0 with -B{}", &v[..1]), EXIT_ARGS);
                    }
                } else {
                    o.basezoom = p.atoi_require(&v, "Basezoom");
                    if o.basezoom == 0 && v != "0" {
                        fail(format!("{argv0}: Couldn't understand -B{v}"), EXIT_ARGS);
                    }
                }
            }
            'K' => {
                o.cluster_distance = p.atoi_require(&v, "Cluster distance");
                if o.cluster_distance > 255 {
                    fail(format!("{argv0}: --cluster-distance {} is too big; limit is 255", o.cluster_distance), EXIT_ARGS);
                }
            }
            'k' => {
                if v == "g" {
                    o.cluster_maxzoom = MAX_ZOOM - 1;
                    o.guess_cluster_maxzoom = true;
                } else {
                    o.cluster_maxzoom = p.atoi_require(&v, "Cluster maxzoom");
                }
            }
            'd' => {
                o.full_detail = p.atoi_require(&v, "Full detail");
                if o.full_detail > 30 {
                    fail(format!("{argv0}: --full-detail can be at most 30"), EXIT_ARGS);
                }
            }
            'D' => {
                o.low_detail = p.atoi_require(&v, "Low detail");
                if o.low_detail > 30 {
                    fail(format!("{argv0}: --low-detail can be at most 30"), EXIT_ARGS);
                }
            }
            'm' => o.min_detail = p.atoi_require(&v, "Min detail"),
            'o' | 'e' => {
                if let Some(x) = out_mbtiles.as_ref().or(out_dir.as_ref()) {
                    fail(format!("{argv0}: Can't specify both {x} and {v} as output"), EXIT_ARGS);
                }
                if c == 'o' {
                    out_mbtiles = Some(v.clone());
                } else {
                    out_dir = Some(v.clone());
                }
            }
            'x' => {
                o.exclude.insert(v.clone());
            }
            'y' => {
                o.exclude_all = true;
                o.include.insert(v.clone());
            }
            'X' => o.exclude_all = true,
            'Y' => match v.find(':') {
                Some(k) if k > 0 => {
                    o.attribute_descriptions.entry(v[..k].to_string()).or_insert_with(|| v[k + 1..].to_string());
                }
                _ => fail(format!("{argv0}: -Y requires attribute:description"), EXIT_ARGS),
            },
            'J' => {
                let text = std::fs::read(&v).unwrap_or_else(|e| fail(format!("{v}: {}", crate::os_error(&e)), EXIT_OPEN));
                // `json_begin_file` + `json_read_tree` (a file can have NUL bytes; jsonpull reads them).
                let mut j = vtmake_parse::pull::Pull::new();
                j.set_input(&text, true);
                match j.read_tree() {
                    Some(t) => o.filter = Some(String::from_utf8_lossy(&j.stringify(t)).into_owned()),
                    None => fail(format!("{v}: {}", j.error.unwrap_or("")), EXIT_JSON),
                }
            }
            'j' => match json_tree(&v) {
                Ok((j, t)) => o.filter = Some(String::from_utf8_lossy(&j.stringify(t)).into_owned()),
                Err(e) => fail(format!("Could not parse filter {v}\n{e}"), EXIT_JSON),
            },
            'r' => {
                if v == "g" {
                    o.droprate = -2.0;
                } else if v == "p" {
                    o.droprate = -3.0;
                } else if v.starts_with('g') || v.starts_with('f') {
                    o.droprate = -2.0;
                    let w = p.atof_require(&v[1..], "Marker width");
                    o.basezoom_marker_width = if v.starts_with('g') { w } else { (50000.0 / w).sqrt() };
                    if o.basezoom_marker_width == 0.0 || w == 0.0 {
                        fail(format!("{argv0}: Must specify value >0 with -r{}", &v[..1]), EXIT_ARGS);
                    }
                } else {
                    o.droprate = p.atof_require(&v, "Drop rate");
                }
            }
            'b' => {
                o.buffer = p.atoi_require(&v, "Buffer");
                if o.buffer > 127 {
                    fail(format!("{argv0}: --buffer can be at most 127"), EXIT_ARGS);
                }
            }
            'f' => o.force = true,
            'F' => o.allow_existing = true,
            't' => {
                o.tmpdir = v.clone();
                if !v.starts_with('/') {
                    eprintln!("Warning: temp directory {v} doesn't begin with /");
                }
            }
            'g' => o.gamma = p.atof_require(&v, "Gamma"),
            'q' => o.quiet = true,
            'Q' => o.quiet_progress = true,
            'u' => {
                o.quiet = true;
                o.json_progress = true;
            }
            'U' => o.progress_interval = p.atof_require(&v, "Progress interval"),
            'p' | 'a' => {
                for b in v.bytes() {
                    let known = OPTIONS.iter().any(|t| match t.act {
                        Act::Prevent(x) => c == 'p' && x == b,
                        Act::Additional(x) => c == 'a' && x == b,
                        _ => false,
                    });
                    if !known {
                        fail(format!("{argv0}: Unknown option -{c}{}", b as char), EXIT_ARGS);
                    }
                    if c == 'p' {
                        o.prevent[b as usize] = true;
                    } else {
                        o.additional[b as usize] = true;
                    }
                    o.given.push(format!("-{c}{}", b as char));
                }
                continue;
            }
            'v' => {
                eprintln!("tippecanoe {FELT_VERSION} (vtmake v{})", env!("CARGO_PKG_VERSION"));
                std::process::exit(0)
            }
            'P' => o.read_parallel = true,
            's' => {
                o.projection = match v.as_str() {
                    "EPSG:4326" | "urn:ogc:def:crs:OGC:1.3:CRS84" => Projection::Epsg4326,
                    "EPSG:3857" | "urn:ogc:def:crs:EPSG::3857" => Projection::Epsg3857,
                    _ => fail(format!("Unknown projection (-s): {v}"), EXIT_ARGS),
                }
            }
            'S' => {
                o.simplification = p.atof_require(&v, "Simplification");
                if o.simplification <= 0.0 {
                    fail(format!("{argv0}: --simplification must be > 0"), EXIT_ARGS);
                }
            }
            'M' => o.max_tile_size = p.atoll_require(&v, "Max tile size"),
            'O' => o.max_tile_features = p.atoll_require(&v, "Max tile features"),
            'c' => o.postfilter = Some(v.clone()),
            'C' => o.prefilter = Some(v.clone()),
            'T' => {
                let Some(k) = v.find(':') else {
                    fail(format!("-T{v} option must be in the form -Tname:type"), EXIT_ARGS)
                };
                let ty = match &v[k + 1..] {
                    "int" => TypeOpt::Int,
                    "float" => TypeOpt::Float,
                    "string" => TypeOpt::String,
                    "bool" => TypeOpt::Bool,
                    other => fail(format!("Attribute type ({other}) must be int, float, string, or bool"), EXIT_ARGS),
                };
                o.attribute_types.entry(v[..k].to_string()).or_insert(ty);
            }
            'E' => {
                if v.starts_with('{') {
                    let (j, tree) = json_tree(&v).unwrap_or_else(|e| fail(format!("{argv0}: -E{v}: {e}"), EXIT_JSON));
                    if j.kind(tree) != Kind::Hash {
                        fail(format!("{argv0}: -E{v}: not a JSON object"), EXIT_JSON);
                    }
                    for (i, e) in j.items(tree).chunks(2).enumerate() {
                        if j.kind(e[0]) != Kind::String {
                            fail(format!("{argv0}: -E{v}: key {i} not a string"), EXIT_JSON);
                        }
                        if j.kind(e[1]) != Kind::String {
                            fail(format!("{argv0}: -E{v}: value {i} not a string"), EXIT_JSON);
                        }
                        accum(&json_string(&j, e[0]), &json_string(&j, e[1]), &mut o.attribute_accum);
                    }
                } else {
                    let Some(k) = v.find(':') else {
                        fail(format!("-E{v} option must be in the form -Ename:method"), EXIT_ARGS)
                    };
                    accum(&v[..k], &v[k + 1..], &mut o.attribute_accum);
                }
            }
            'H' => usage(argv0, 0),
            _ => {
                eprintln!("Unknown option -{c}");
                usage(argv0, EXIT_ARGS)
            }
        }
        o.given.push(given);
    }

    // After the loop (`main.cpp:main`).
    // `size_t` values in tippecanoe: a negative limit is very large.
    if (o.tilestats_sample_values_limit as u64) < (o.tilestats_values_limit as u64) {
        o.tilestats_sample_values_limit = o.tilestats_values_limit;
    }
    if o.full_detail <= 0 {
        o.full_detail = 12;
    }
    if o.droprate == -3.0 && !o.guess_maxzoom {
        fail("Can't use -rp without either -zg or --smallest-maximum-zoom-guess", EXIT_ARGS);
    }
    if o.maxzoom > MAX_ZOOM {
        o.maxzoom = MAX_ZOOM;
        eprintln!("Highest supported zoom is -z{}", o.maxzoom);
    }
    if !o.guess_maxzoom {
        if o.maxzoom > 32 - o.full_detail {
            o.full_detail = 32 - o.maxzoom;
            eprintln!("Highest supported detail with maxzoom {} is {}", o.maxzoom, o.full_detail);
        }
        if o.maxzoom > 33 - o.low_detail {
            o.low_detail = 33 - o.maxzoom;
            eprintln!("Highest supported low detail with maxzoom {} is {}", o.maxzoom, o.low_detail);
        }
    }
    if o.minzoom > o.maxzoom {
        fail(format!("{argv0}: Minimum zoom -Z{} cannot be greater than maxzoom -z{}", o.minzoom, o.maxzoom), EXIT_ARGS);
    }
    if o.full_detail < o.min_detail {
        o.min_detail = o.full_detail;
        eprintln!("{argv0}: Reducing minimum detail to match full detail {}", o.min_detail);
    }
    if o.low_detail < o.min_detail {
        o.min_detail = o.low_detail;
        eprintln!("{argv0}: Reducing minimum detail to match low detail {}", o.min_detail);
    }
    if o.basezoom == -1 && !o.guess_maxzoom {
        o.basezoom = o.maxzoom;
    }
    if o.extra_detail >= 0 || o.prevent[b'n' as usize] || o.additional[b'e' as usize] || o.extend_zooms_max > 0 {
        o.geometry_scale = 0;
    } else {
        o.geometry_scale = 32 - (o.full_detail + o.maxzoom);
        if o.geometry_scale < 0 {
            o.geometry_scale = 0;
            if !o.guess_maxzoom {
                eprintln!("Full detail + maxzoom > 32, so you are asking for more detail than is available.");
            }
        }
    }
    if (o.basezoom < 0 || o.droprate < 0.0) && o.gamma < 0.0 {
        o.gamma = 0.0;
        eprintln!("Forcing -g0 since -B or -r is not known");
    }
    o.output = match (out_mbtiles, out_dir) {
        (Some(m), None) => Output::Mbtiles(m.into()),
        (None, Some(d)) => Output::Dir(d.into()),
        (None, None) => fail(format!("{argv0}: must specify -o out.mbtiles or -e directory"), EXIT_ARGS),
        (Some(_), Some(_)) => fail(format!("{argv0}: Options -o and -e cannot be used together"), EXIT_ARGS),
    };
    for f in g.operands {
        o.sources.push(Source { file: f, ..Default::default() });
    }
    if o.sources.is_empty() {
        o.sources.push(Source::default());
    }
    if let Some(l) = &o.layer {
        for s in &mut o.sources {
            s.layer = l.clone();
        }
    }
    o
}

/// `set_attribute_value`.
fn set_attribute(o: &mut Options, arg: &str, argv0: &str) {
    use vtmake_core::AttrType;
    use vtmake_parse::pull::Kind;
    if arg.starts_with('{') {
        let (j, tree) = json_tree(arg).unwrap_or_else(|e| fail(format!("{argv0}: --set-attribute {arg}: {e}"), EXIT_JSON));
        if j.kind(tree) != Kind::Hash {
            fail(format!("{argv0}: --set-attribute {arg}: not a JSON object"), EXIT_JSON);
        }
        for (i, e) in j.items(tree).chunks(2).enumerate() {
            if j.kind(e[0]) != Kind::String {
                fail(format!("{argv0}: --set-attribute {arg}: key {i} not a string"), EXIT_JSON);
            }
            let v = e[1];
            let (ty, text) = match j.kind(v) {
                Kind::String => (AttrType::String, json_string(&j, v)),
                Kind::Number => (AttrType::Number, String::from_utf8_lossy(&j.stringify(v)).into_owned()),
                Kind::True => (AttrType::Bool, "true".into()),
                Kind::False => (AttrType::Bool, "false".into()),
                Kind::Null => (AttrType::Null, "null".into()),
                _ => (AttrType::String, String::from_utf8_lossy(&j.stringify(v)).into_owned()),
            };
            // `std::map::emplace`: the first value of a key stays.
            o.set_attributes.entry(json_string(&j, e[0])).or_insert(SetValue { ty, text });
        }
        return;
    }
    let Some(k) = arg.find(':') else {
        fail(format!("--set-attribute {arg} option must be in the form --set-attribute name:value"), EXIT_ARGS)
    };
    let value = &arg[k + 1..];
    let ty = if value.starts_with(|c: char| c.is_ascii_digit() || c == '-') { AttrType::Number } else { AttrType::String };
    o.set_attributes.entry(arg[..k].to_string()).or_insert(SetValue { ty, text: value.to_string() });
}

/// Stop with exit 101 if the command line asks for an effect that vtmake does
/// not have yet. vtmake never ignores an option that tippecanoe uses.
pub fn check_supported(o: &Options) {
    let stop = |what: &str| -> ! {
        eprintln!("vtmake build: option {what} is not supported yet");
        std::process::exit(EXIT_ARGS)
    };
    // Options with an effect in vtmake now. `-q`, `-Q` and `-U` only change
    // the progress display, which vtmake does not have. `-t` selects the
    // directory for the disk queues when tile limits are active.
    const DONE: &[&str] = &[
        "--only-tiles", "--known-hashes", "--hashes-out",
        "--output",
        "--output-to-directory",
        "--force",
        "--name",
        "--attribution",
        "--description",
        "--layer",
        "--named-layer",
        "--read-parallel",
        "--projection",
        "--maximum-zoom",
        "--minimum-zoom",
        "--buffer",
        "--one-tile",
        "--retain-points-multiplier",
        "--preserve-multiplier-density-threshold",
        "--drop-rate",
        "--base-zoom",
        "--gamma",
        "--preserve-point-density-threshold",
        "--drop-lines",
        "--drop-polygons",
        "--limit-base-zoom-to-maximum-zoom",
        "-al",
        "-ap",
        "-pb",
        "--full-detail",
        "--low-detail",
        "--minimum-detail",
        "--maximum-tile-bytes",
        "--maximum-tile-features",
        "--smallest-maximum-zoom-guess",
        "--attribute-description",
        "--temporary-directory",
        "--quiet",
        "--check-polygons",
        "-a@",
        "--no-progress-indicator",
        "--progress-interval",
        "--drop-densest-as-needed",
        "--drop-fraction-as-needed", "-ad",
        "--drop-smallest-as-needed", "-an",
        "--drop-by-attribute-as-needed", "--drop-by-attribute-order",
        "--increase-gamma-as-needed", "-aG",
        "--force-feature-limit", "-pd",
        "--hilbert", "-ah",
        "--drop-denser",
        "--cluster-distance", "--cluster-maxzoom", "--keep-point-cluster-position", "-aa",
        "--cluster-densest-as-needed", "-aC",
        "--coalesce-densest-as-needed", "-aS", "--coalesce-smallest-as-needed", "-aN", "--coalesce-fraction-as-needed", "-aD",
        "--coalesce", "-ac", "--reorder", "-ao", "--reverse", "-ar",
        "--calculate-feature-density", "-ag", "--order-largest-first", "--order-smallest-first",
        "--preserve-input-order", "-pi", "--order-by", "--order-descending-by",
        "--limit-tile-feature-count", "--limit-tile-feature-count-at-maximum-zoom",
        "--generate-variable-depth-tile-pyramid", "-at",
        // Plugins and progress (crate::plugin).
        "--prefilter", "--postfilter", "--json-progress",
        "--extend-zooms-if-still-dropping",
        "--extend-zooms-if-still-dropping-maximum",
        "--no-feature-limit",
        "--no-tile-size-limit",
        "--empty-csv-columns-are-null",
        "--convert-stringified-ids-to-numbers",
        "-as",
        "-ae",
        "-pf",
        "-pk",
        "-pe",
        "-aI",
        // Attributes and filters (crate::attrs, crate::filter).
        "--exclude",
        "--include",
        "--exclude-all",
        "--attribute-type",
        "--accumulate-attribute",
        "--use-attribute-for-id",
        "--single-precision",
        "-pN",
        "--set-attribute",
        "--maximum-string-attribute-length",
        "--feature-filter-file",
        "--feature-filter",
        "--generate-ids",
        "-ai",
        "--unidecode-data",
        // Geometry options.
        "--extra-detail",
        "--simplification",
        "--simplification-at-maximum-zoom",
        "--no-line-simplification",
        "-ps",
        "--simplify-only-low-zooms",
        "-pS",
        "--no-tiny-polygon-reduction",
        "-pt",
        "--no-tiny-polygon-reduction-at-maximum-zoom",
        "-pT",
        "--tiny-polygon-size",
        "--visvalingam",
        "-av",
        "--grid-low-zooms",
        "-aL",
        "--no-clipping",
        "-pc",
        "--no-duplication",
        "-pD",
        "--no-polygon-splitting",
        "-pp",
        "--prefer-radix-sort",
        "-aR",
        "--detect-longitude-wraparound",
        "-aw",
        "--use-source-polygon-winding",
        "-pw",
        "--reverse-source-polygon-winding",
        "-pW",
        "--clip-bounding-box",
        "--convert-polygons-to-label-points",
        "-aP",
        "--no-simplification-of-shared-nodes",
        "-pn",
        "--detect-shared-borders",
        "-ab",
        // Output and metadata (crates/vtmake-out, crate::meta).
        "--allow-existing",
        "--no-tile-compression",
        "--no-tile-stats",
        "-pC",
        "-pg",
        "--tile-stats-attributes-limit",
        "--tile-stats-sample-values-limit",
        "--tile-stats-values-limit",
        "--pgcopy-schema",
    ];
    for g in &o.given {
        if !DONE.contains(&g.as_str()) {
            stop(g);
        }
    }

    if o.minzoom < 0 {
        stop(&format!("--minimum-zoom={}", o.minzoom));
    }
}

/// The layer name tippecanoe makes from a file name (`main.cpp:read_input`): the
/// last path component, without `.json`, `.geojson`, `.geobuf`, `.mbtiles`,
/// `.pmtiles`, `.csv` and `.gz` at the end, and only with the bytes
/// `[A-Za-z0-9_]` and non-ASCII. An empty or not UTF-8 name: `unknown<l>`.
pub fn layer_from_path(path: &std::path::Path, l: usize) -> String {
    use std::os::unix::ffi::OsStrExt;
    let b = path.as_os_str().as_bytes();
    let mut start = 0;
    for i in 0..b.len() {
        if b[i] == b'/' && i + 1 < b.len() {
            start = i + 1;
        }
    }
    let mut t = &b[start..];
    const TRIM: [&[u8]; 7] = [b".json", b".geojson", b".geobuf", b".mbtiles", b".pmtiles", b".csv", b".gz"];
    let mut again = true;
    while again {
        again = false;
        for s in TRIM {
            if t.len() > s.len() && t.ends_with(s) {
                t = &t[..t.len() - s.len()];
                again = true;
            }
        }
    }
    let out: Vec<u8> = t.iter().copied().filter(|&c| c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80).collect();
    match String::from_utf8(out) {
        Ok(s) if !s.is_empty() => s,
        _ => format!("unknown{l}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn layer_names() {
        let n = |p: &str| layer_from_path(std::path::Path::new(p), 3);
        assert_eq!(n("/data/pub-lines.ndjson"), "publinesndjson");
        assert_eq!(n("a/b.geojson.gz"), "b");
        assert_eq!(n("out.mbtiles"), "out");
        assert_eq!(n("tiles/"), "tiles");
        assert_eq!(n("東京.json"), "東京");
        assert_eq!(n(".json"), "json");
        assert_eq!(n("---"), "unknown3");
    }

    #[test]
    fn plain_command_line() {
        let o = parse(args("tippecanoe -Z12 -z18 --read-parallel --buffer=5 --force --drop-densest-as-needed --extend-zooms-if-still-dropping --no-feature-limit --no-tile-size-limit --layer bldg -o out.mbtiles in.ndjson"));
        assert_eq!((o.minzoom, o.maxzoom, o.buffer, o.force, o.read_parallel), (12, 18, 5, true, true));
        assert!(o.prevent[b'f' as usize] && o.prevent[b'k' as usize] && o.additional[b's' as usize] && o.additional[b'e' as usize]);
        assert_eq!(o.sources.len(), 1);
        assert_eq!((o.sources[0].layer.as_str(), o.sources[0].file.as_str()), ("bldg", "in.ndjson"));
        assert_eq!(o.geometry_scale, 0);
        check_supported(&o);
        // The same with the short letter groups.
        let o = parse(args("tippecanoe -Z12 -z18 -P -b5 -f -as -ae -pfk -lbldg -oout.mbtiles in.ndjson"));
        assert!(o.prevent[b'f' as usize] && o.prevent[b'k' as usize] && o.additional[b's' as usize] && o.additional[b'e' as usize]);
        check_supported(&o);
    }

    #[test]
    fn sources_and_layers() {
        let o = parse(args("tippecanoe -o x.mbtiles -L a:one.json -L {\"file\":\"two.csv\",\"layer\":\"b\",\"description\":\"d\"} three.json"));
        let s: Vec<(&str, &str, &str)> = o.sources.iter().map(|s| (s.layer.as_str(), s.file.as_str(), s.description.as_str())).collect();
        assert_eq!(s, [("a", "one.json", ""), ("b", "two.csv", "d"), ("", "three.json", "")]);
        let o = parse(args("tippecanoe -e dir -n N -N D -A A"));
        assert_eq!(o.sources.len(), 1);
        assert_eq!(o.sources[0].file, "");
        assert_eq!((o.name.as_deref(), o.description.as_deref(), o.attribution.as_deref()), (Some("N"), Some("D"), Some("A")));
    }

    #[test]
    fn usage_matches_felt_layout() {
        let u = usage_text("tippecanoe");
        assert!(u.starts_with("Usage: tippecanoe [options] [file.json ...]\n  Output tileset\n         (--output=output.mbtiles | --output-to-directory=directory) [--force]\n         [--allow-existing]\n"));
        assert!(u.ends_with("  Version\n         [--version]\n"));
    }

    #[test]
    fn c_number_rules() {
        assert_eq!(strtoll(" -12x"), (-12, 4));
        assert_eq!(strtoll("x"), (0, 0));
        assert_eq!(strtod("1.5e3,"), (1500.0, 5));
        assert_eq!(atoi("4294967297"), 1);
    }
}
