//! A small port of GNU `getopt_long`, as tippecanoe's tools use it: short
//! options in clusters (`-fq`, `-ofile`), long options with `=value` or a
//! separate value, unique prefixes of long options, `--` to stop, and the
//! operands kept in their order (GNU permutation).

/// One long option. `key` is the value that [`parse`] gives for it.
pub struct Long {
    pub name: &'static str,
    pub arg: bool,
    pub key: &'static str,
}

/// One short option: the letter, whether it takes a value, and its key.
pub struct Short {
    pub ch: char,
    pub arg: bool,
    pub key: &'static str,
}

pub struct Parsed {
    /// The options in command line order: (key, value).
    pub opts: Vec<(&'static str, Option<String>)>,
    /// The operands, in their order.
    pub operands: Vec<String>,
}

/// Parse `argv` (argv[0] is the program name). An error prints the GNU
/// message and calls `usage`.
pub fn parse(argv: &[String], shorts: &[Short], longs: &[Long], usage: &dyn Fn()) -> Parsed {
    let usage = || -> ! {
        usage();
        unreachable!("usage() returns")
    };
    let prog = argv.first().map(String::as_str).unwrap_or("");
    let mut opts = Vec::new();
    let mut operands = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let a = &argv[i];
        i += 1;
        if a == "--" {
            operands.extend(argv[i..].iter().cloned());
            break;
        }
        if let Some(body) = a.strip_prefix("--") {
            let (name, value) = match body.find('=') {
                Some(p) => (&body[..p], Some(body[p + 1..].to_string())),
                None => (body, None),
            };
            let exact = longs.iter().find(|l| l.name == name);
            let l = match exact {
                Some(l) => l,
                None => {
                    let cands: Vec<&Long> = longs.iter().filter(|l| l.name.starts_with(name)).collect();
                    match cands.first() {
                        None => {
                            eprintln!("{prog}: unrecognized option '--{name}'");
                            usage()
                        }
                        Some(&f) if cands.iter().all(|c| c.key == f.key && c.arg == f.arg) => f,
                        Some(_) => {
                            let list: Vec<String> = cands.iter().map(|c| format!("'--{}'", c.name)).collect();
                            eprintln!("{prog}: option '--{name}' is ambiguous; possibilities: {}", list.join(" "));
                            usage()
                        }
                    }
                }
            };
            if l.arg {
                let v = match value {
                    Some(v) => v,
                    None if i < argv.len() => {
                        i += 1;
                        argv[i - 1].clone()
                    }
                    None => {
                        eprintln!("{prog}: option '--{}' requires an argument", l.name);
                        usage()
                    }
                };
                opts.push((l.key, Some(v)));
            } else {
                if value.is_some() {
                    eprintln!("{prog}: option '--{}' doesn't allow an argument", l.name);
                    usage()
                }
                opts.push((l.key, None));
            }
            continue;
        }
        if a.len() > 1 && a.starts_with('-') {
            let chars: Vec<char> = a[1..].chars().collect();
            let mut j = 0;
            while j < chars.len() {
                let c = chars[j];
                j += 1;
                let Some(s) = shorts.iter().find(|s| s.ch == c) else {
                    eprintln!("{prog}: invalid option -- '{c}'");
                    usage()
                };
                if s.arg {
                    let v = if j < chars.len() {
                        chars[j..].iter().collect()
                    } else if i < argv.len() {
                        i += 1;
                        argv[i - 1].clone()
                    } else {
                        eprintln!("{prog}: option requires an argument -- '{c}'");
                        usage()
                    };
                    opts.push((s.key, Some(v)));
                    break;
                }
                opts.push((s.key, None));
            }
            continue;
        }
        operands.push(a.clone());
    }
    Parsed { opts, operands }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never() {
        panic!("usage")
    }

    #[test]
    fn clusters_prefixes_and_operands() {
        let shorts = [Short { ch: 'f', arg: false, key: "force" }, Short { ch: 'o', arg: true, key: "output" }, Short { ch: 'p', arg: true, key: "prevent" }];
        let longs = [Long { name: "force", arg: false, key: "force" }, Long { name: "output", arg: true, key: "output" }, Long { name: "no-tile-size-limit", arg: false, key: "pk" }];
        let argv: Vec<String> = ["tile-join", "in1", "-fo", "out", "--no-tile", "-pk", "--out=x", "--", "-f"].iter().map(|s| s.to_string()).collect();
        let p = parse(&argv, &shorts, &longs, &never);
        let keys: Vec<(&str, Option<&str>)> = p.opts.iter().map(|(k, v)| (*k, v.as_deref())).collect();
        assert_eq!(keys, [("force", None), ("output", Some("out")), ("pk", None), ("prevent", Some("k")), ("output", Some("x"))]);
        assert_eq!(p.operands, ["in1", "-f"]);
    }
}

/// One entry of a `struct option` table for [`GetoptLong`].
pub struct LongOpt {
    pub name: &'static str,
    pub has_arg: bool,
    /// The `(flag, val)` identity of the entry: GNU getopt does not call an
    /// abbreviation ambiguous if all its matches have the same identity and
    /// `has_arg`.
    pub ident: u32,
}

/// One result of [`GetoptLong::next`].
#[derive(Debug, PartialEq, Eq)]
pub enum Got {
    /// A long option: its index in the table and its value.
    Long(usize, Option<String>),
    /// A short option and its value.
    Short(char, Option<String>),
    /// An error. GNU getopt has written the message (`'?'`).
    Error,
}

/// GNU `getopt_long` (glibc) with argument permutation: the options in their
/// order, the operands in [`GetoptLong::operands`]. The messages are glibc's.
pub struct GetoptLong<'a> {
    argv: &'a [String],
    shorts: Vec<(char, bool)>,
    longs: &'a [LongOpt],
    i: usize,
    /// The position in a cluster of short options (`-abc`).
    cluster: Option<(usize, usize)>,
    pub operands: Vec<String>,
}

impl<'a> GetoptLong<'a> {
    /// `optstring` in getopt syntax (`"ab:c"`).
    pub fn new(argv: &'a [String], optstring: &str, longs: &'a [LongOpt]) -> Self {
        let b: Vec<char> = optstring.chars().collect();
        let mut shorts = Vec::new();
        let mut k = 0;
        while k < b.len() {
            let arg = b.get(k + 1) == Some(&':');
            shorts.push((b[k], arg));
            k += 1 + usize::from(arg);
        }
        GetoptLong { argv, shorts, longs, i: 1, cluster: None, operands: Vec::new() }
    }

    fn prog(&self) -> &str {
        self.argv.first().map(String::as_str).unwrap_or("")
    }

    pub fn next_opt(&mut self) -> Option<Got> {
        if let Some((arg, pos)) = self.cluster {
            return Some(self.short(arg, pos));
        }
        loop {
            let a = self.argv.get(self.i)?;
            if a == "--" {
                self.operands.extend(self.argv[self.i + 1..].iter().cloned());
                self.i = self.argv.len();
                return None;
            }
            if let Some(body) = a.strip_prefix("--") {
                self.i += 1;
                return Some(self.long(body));
            }
            if a.len() > 1 && a.starts_with('-') {
                self.i += 1;
                return Some(self.short(self.i - 1, 1));
            }
            self.operands.push(a.clone());
            self.i += 1;
        }
    }

    fn long(&mut self, body: &str) -> Got {
        let prog = self.prog().to_string();
        let (name, value) = match body.find('=') {
            Some(p) => (&body[..p], Some(body[p + 1..].to_string())),
            None => (body, None),
        };
        let found = match self.longs.iter().position(|l| l.name == name) {
            Some(k) => k,
            None => {
                let mut found: Option<usize> = None;
                let mut ambiguous: Vec<usize> = Vec::new();
                for (k, l) in self.longs.iter().enumerate() {
                    if !l.name.starts_with(name) {
                        continue;
                    }
                    match found {
                        None => found = Some(k),
                        Some(f) => {
                            let p = &self.longs[f];
                            if p.has_arg != l.has_arg || p.ident != l.ident {
                                if ambiguous.is_empty() {
                                    ambiguous.push(f);
                                }
                                ambiguous.push(k);
                            }
                        }
                    }
                }
                if !ambiguous.is_empty() {
                    let list: String = ambiguous.iter().map(|&k| format!(" '--{}'", self.longs[k].name)).collect();
                    eprintln!("{prog}: option '--{body}' is ambiguous; possibilities:{list}");
                    return Got::Error;
                }
                match found {
                    Some(k) => k,
                    None => {
                        eprintln!("{prog}: unrecognized option '--{body}'");
                        return Got::Error;
                    }
                }
            }
        };
        let l = &self.longs[found];
        if l.has_arg {
            let v = match value {
                Some(v) => v,
                None if self.i < self.argv.len() => {
                    self.i += 1;
                    self.argv[self.i - 1].clone()
                }
                None => {
                    eprintln!("{prog}: option '--{}' requires an argument", l.name);
                    return Got::Error;
                }
            };
            Got::Long(found, Some(v))
        } else if value.is_some() {
            eprintln!("{prog}: option '--{}' doesn't allow an argument", l.name);
            Got::Error
        } else {
            Got::Long(found, None)
        }
    }

    fn short(&mut self, arg: usize, pos: usize) -> Got {
        let prog = self.prog().to_string();
        let a: Vec<char> = self.argv[arg].chars().collect();
        let c = a[pos];
        let next = pos + 1;
        self.cluster = (next < a.len()).then_some((arg, next));
        let Some(&(_, has_arg)) = self.shorts.iter().find(|s| s.0 == c && c != ':') else {
            eprintln!("{prog}: invalid option -- '{c}'");
            return Got::Error;
        };
        if !has_arg {
            return Got::Short(c, None);
        }
        self.cluster = None;
        if next < a.len() {
            return Got::Short(c, Some(a[next..].iter().collect()));
        }
        if self.i < self.argv.len() {
            self.i += 1;
            return Got::Short(c, Some(self.argv[self.i - 1].clone()));
        }
        eprintln!("{prog}: option requires an argument -- '{c}'");
        Got::Error
    }
}

#[cfg(test)]
mod long_tests {
    use super::*;

    #[test]
    fn gnu_rules() {
        let longs = [
            LongOpt { name: "output", has_arg: true, ident: 'o' as u32 },
            LongOpt { name: "tile-a", has_arg: true, ident: '~' as u32 },
            LongOpt { name: "tile-b", has_arg: true, ident: '~' as u32 },
            LongOpt { name: "no-x", has_arg: false, ident: 1000 },
            LongOpt { name: "no-y", has_arg: false, ident: 1001 },
        ];
        let argv: Vec<String> = ["t", "in", "-fo", "out", "--tile=3", "-pk", "--out", "x", "-", "--", "-f"].iter().map(|s| s.to_string()).collect();
        let mut g = GetoptLong::new(&argv, "fo:p:", &longs);
        let mut got = Vec::new();
        while let Some(x) = g.next_opt() {
            got.push(x);
        }
        assert_eq!(
            got,
            [
                Got::Short('f', None),
                Got::Short('o', Some("out".into())),
                Got::Long(1, Some("3".into())),
                Got::Short('p', Some("k".into())),
                Got::Long(0, Some("x".into())),
            ]
        );
        assert_eq!(g.operands, ["in", "-", "-f"]);
        let argv: Vec<String> = ["t", "--no"].iter().map(|s| s.to_string()).collect();
        assert_eq!(GetoptLong::new(&argv, "", &longs).next_opt(), Some(Got::Error));
    }
}
