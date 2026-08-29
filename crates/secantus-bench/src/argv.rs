//! A very small `--flag value` / `--flag=value` parser.
//!
//! Hand-rolled to match `secantus-server`'s `args.rs` rather than pulling in
//! `clap`: the workspace keeps its dependency graph small on purpose, and the
//! surface here is flags and one subcommand.

use std::collections::{HashMap, HashSet};

use crate::BenchResult;

#[derive(Debug, Default)]
pub struct Args {
    pub command: String,
    pub positional: Vec<String>,
    values: HashMap<String, String>,
    present: HashSet<String>,
}

impl Args {
    /// `argv` excludes the program name. The first element is the subcommand.
    ///
    /// `bool_flags` names the flags that take no value; anything else consumes
    /// the next argument. Getting that wrong silently swallows the following
    /// flag, so unknown flags are rejected outright instead.
    pub fn parse(argv: &[String], bool_flags: &[&str], value_flags: &[&str]) -> BenchResult<Args> {
        let mut args = Args::default();
        if argv.is_empty() {
            return Err("missing subcommand".to_string());
        }
        let mut idx = 0;
        if !argv[0].starts_with('-') {
            args.command = argv[0].clone();
            idx = 1;
        }
        while idx < argv.len() {
            let item = &argv[idx];
            if !item.starts_with("--") {
                args.positional.push(item.clone());
                idx += 1;
                continue;
            }
            let (name, inline) = match item.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (item.clone(), None),
            };
            if bool_flags.contains(&name.as_str()) {
                if inline.is_some() {
                    return Err(format!("{name} takes no value"));
                }
                args.present.insert(name);
                idx += 1;
                continue;
            }
            if !value_flags.contains(&name.as_str()) {
                return Err(format!("unknown flag {name}"));
            }
            let value = match inline {
                Some(v) => {
                    idx += 1;
                    v
                }
                None => {
                    idx += 1;
                    let v = argv
                        .get(idx)
                        .ok_or_else(|| format!("{name} requires a value"))?
                        .clone();
                    idx += 1;
                    v
                }
            };
            args.present.insert(name.clone());
            args.values.insert(name, value);
        }
        Ok(args)
    }

    pub fn has(&self, flag: &str) -> bool {
        self.present.contains(flag)
    }

    pub fn str_or(&self, flag: &str, default: &str) -> String {
        self.values
            .get(flag)
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    pub fn required(&self, flag: &str) -> BenchResult<String> {
        self.values
            .get(flag)
            .cloned()
            .ok_or_else(|| format!("{flag} is required"))
    }

    pub fn f64_or(&self, flag: &str, default: f64) -> BenchResult<f64> {
        match self.values.get(flag) {
            None => Ok(default),
            Some(raw) => raw
                .parse()
                .map_err(|_| format!("{flag}: {raw:?} is not a number")),
        }
    }

    pub fn usize_or(&self, flag: &str, default: usize) -> BenchResult<usize> {
        match self.values.get(flag) {
            None => Ok(default),
            Some(raw) => raw
                .parse()
                .map_err(|_| format!("{flag}: {raw:?} is not an integer")),
        }
    }

    pub fn i64_or(&self, flag: &str, default: i64) -> BenchResult<i64> {
        match self.values.get(flag) {
            None => Ok(default),
            Some(raw) => raw
                .parse()
                .map_err(|_| format!("{flag}: {raw:?} is not an integer")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    const BOOLS: [&str; 2] = ["--fresh", "--keep-data"];
    const VALUES: [&str; 2] = ["--duration", "--op-mix"];

    #[test]
    fn separate_and_inline_values_both_work() {
        let a = Args::parse(
            &argv(&["run", "--duration", "30", "--op-mix=insert=100"]),
            &BOOLS,
            &VALUES,
        )
        .unwrap();
        assert_eq!(a.command, "run");
        assert_eq!(a.f64_or("--duration", 0.0).unwrap(), 30.0);
        assert_eq!(a.str_or("--op-mix", ""), "insert=100");
    }

    #[test]
    fn bool_flags_take_no_value() {
        let a = Args::parse(&argv(&["up", "--fresh"]), &BOOLS, &VALUES).unwrap();
        assert!(a.has("--fresh"));
        assert!(!a.has("--keep-data"));
        assert!(Args::parse(&argv(&["up", "--fresh=1"]), &BOOLS, &VALUES).is_err());
    }

    #[test]
    fn unknown_flags_are_rejected_not_swallowed() {
        // Accepting an unknown flag would consume the NEXT flag as its value.
        let err = Args::parse(
            &argv(&["run", "--typo", "--duration", "5"]),
            &BOOLS,
            &VALUES,
        );
        assert!(err.is_err());
    }

    #[test]
    fn a_value_flag_without_a_value_is_an_error() {
        assert!(Args::parse(&argv(&["run", "--duration"]), &BOOLS, &VALUES).is_err());
    }

    #[test]
    fn positionals_are_kept() {
        let a = Args::parse(&argv(&["ssh", "server"]), &BOOLS, &VALUES).unwrap();
        assert_eq!(a.command, "ssh");
        assert_eq!(a.positional, ["server"]);
    }

    #[test]
    fn bad_numbers_are_reported() {
        let a = Args::parse(&argv(&["run", "--duration", "soon"]), &BOOLS, &VALUES).unwrap();
        assert!(a.f64_or("--duration", 0.0).is_err());
    }
}
