//! The option tables of the tools and their usage text (`usage.cpp`:
//! `getopt_string`, `strip_usage_headings`, `print_usage`).
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

use crate::getopt::LongOpt;

/// The options are wrapped to fit in this many columns.
const USAGE_WIDTH: usize = 80;
/// The indent of the continuation lines of the option list.
const USAGE_INDENT: usize = 8;

/// One entry of a `struct option` table. `val` 0 is a heading of the usage
/// text. A heading with an empty name ends the usage list (the options after
/// it are not shown).
pub struct Opt {
    pub name: &'static str,
    pub has_arg: bool,
    pub val: u32,
}

impl Opt {
    pub const fn new(name: &'static str, has_arg: bool, val: u32) -> Opt {
        Opt { name, has_arg, val }
    }
    pub const fn heading(name: &'static str) -> Opt {
        Opt { name, has_arg: false, val: 0 }
    }
}

/// `getopt_string` and the table without the headings. The `ident` of a long
/// option is its `val`.
pub fn getopt_table(opts: &[Opt]) -> (String, Vec<LongOpt>) {
    let mut s = String::new();
    let mut longs = Vec::new();
    for o in opts.iter().filter(|o| o.val != 0) {
        if o.val > ' ' as u32 {
            s.push(char::from_u32(o.val).unwrap());
            if o.has_arg {
                s.push(':');
            }
        }
        longs.push(LongOpt { name: o.name, has_arg: o.has_arg, ident: o.val });
    }
    (s, longs)
}

/// `print_usage`: the text. `required`: (name, placeholder, alternation).
pub fn usage_text(program: &str, forms: &[&str], opts: &[Opt], required: &[(&str, &str, i32)]) -> String {
    let mut out = String::new();
    for (f, form) in forms.iter().enumerate() {
        out.push_str(if f == 0 { "Usage: " } else { "\n   or: " });
        out.push_str(program);
        out.push(' ');
        out.push_str(form);
    }
    let req = |name: &str| required.iter().find(|r| r.0 == name);
    let text = |o: &Opt, placeholder: Option<&str>| {
        let mut t = format!("--{}", o.name);
        if o.has_arg {
            t.push('=');
            t.push_str(placeholder.unwrap_or("..."));
        }
        t
    };
    let mut width = USAGE_WIDTH;
    let mut listed: Vec<i32> = Vec::new();
    for o in opts.iter().take_while(|o| !o.name.is_empty()) {
        if o.val == 0 {
            out.push_str(&format!("\n  {}\n{:w$}", o.name, "", w = USAGE_INDENT));
            width = USAGE_INDENT;
            continue;
        }
        let t = match req(o.name) {
            None => format!("[{}]", text(o, None)),
            Some(r) if r.2 == 0 => text(o, Some(r.1)),
            Some(r) => {
                if listed.contains(&r.2) {
                    continue;
                }
                listed.push(r.2);
                let alts: Vec<String> = opts
                    .iter()
                    .take_while(|a| !a.name.is_empty())
                    .filter_map(|a| req(a.name).filter(|q| q.2 == r.2).map(|q| text(a, Some(q.1))))
                    .collect();
                if alts.len() > 1 { format!("({})", alts.join(" | ")) } else { alts.join("") }
            }
        };
        if width + 1 + t.len() >= USAGE_WIDTH {
            out.push_str(&format!("\n{:w$}", "", w = USAGE_INDENT));
            width = USAGE_INDENT;
        }
        out.push(' ');
        out.push_str(&t);
        width += 1 + t.len();
    }
    out.push('\n');
    out
}
