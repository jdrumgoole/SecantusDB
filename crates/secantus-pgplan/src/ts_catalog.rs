//! PostgreSQL 15's text-search catalogs: `pg_ts_config`, `pg_ts_dict`,
//! `pg_ts_parser`, `pg_ts_template` and `pg_ts_config_map`, as `initdb`
//! leaves them (dumped from 15.19, `pg_ts_catalog.tsv`), and the
//! configurations `CREATE TEXT SEARCH CONFIGURATION` makes, which the
//! executor installs per statement ([`set_user_configs`]).
//!
//! They name what `regconfig` and `regdictionary` resolve to: `3748` is the
//! configuration `simple`, `'english'::regconfig::oid` is 13636. A
//! configuration is its map (`pg_ts_config_map`): for each token type of
//! its parser, the dictionaries it is offered to, in order. `to_tsvector`
//! and the rest read it, the built-in ones' as dumped.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

const DATA: &str = include_str!("pg_ts_catalog.tsv");

/// The default parser, the only one (`pg_ts_parser`).
pub const DEFAULT_PARSER: i64 = 3722;

/// One catalog: its columns and rows, each value as dumped (`\N` is NULL).
pub struct Catalog {
    pub columns: Vec<&'static str>,
    pub rows: Vec<Vec<Option<&'static str>>>,
}

/// The catalog named `name` (`pg_ts_config`, ...), or `None`: the built-in
/// rows.
pub fn catalog(name: &str) -> Option<Catalog> {
    let mut lines = DATA.lines().filter(|l| !l.starts_with('#'));
    let header = lines.by_ref().find(|l| {
        l.strip_prefix('[')
            .and_then(|r| r.split_once(']'))
            .is_some_and(|(n, _)| n == name)
    })?;
    let columns = header.split_once(']')?.1.split_whitespace().collect();
    let rows = lines
        .take_while(|l| !l.starts_with('['))
        .filter(|l| !l.is_empty())
        .map(|l| l.split('\t').map(|v| (v != "\\N").then_some(v)).collect())
        .collect();
    Some(Catalog { columns, rows })
}

/// The catalogs this module holds.
pub const NAMES: [&str; 5] = [
    "pg_ts_config",
    "pg_ts_dict",
    "pg_ts_parser",
    "pg_ts_template",
    "pg_ts_config_map",
];

/// A configuration `CREATE TEXT SEARCH CONFIGURATION` made.
#[derive(Debug, Clone, PartialEq)]
pub struct UserConfig {
    pub oid: i64,
    pub schema: String,
    pub name: String,
    pub owner: String,
    pub parser: i64,
    /// `pg_ts_config_map`'s rows: `(token type, seqno, dictionary oid)`.
    pub map: Vec<(i32, i32, i64)>,
}

/// A dictionary `CREATE TEXT SEARCH DICTIONARY` made, or a built-in one an
/// ALTER changed (under its own oid, hiding the dumped row).
#[derive(Debug, Clone, PartialEq)]
pub struct UserDict {
    pub oid: i64,
    pub schema: String,
    pub name: String,
    pub owner: String,
    pub template: i64,
    /// `dictinitoption`'s options, in order.
    pub options: Vec<(String, String)>,
    /// The options written as a number.
    pub numeric: Vec<String>,
}

thread_local! {
    static USER_DICTS: RefCell<Rc<Vec<UserDict>>> = RefCell::new(Rc::new(Vec::new()));
    /// Each installed dictionary's template state, built on first use --
    /// PostgreSQL's dictionary cache (`lookup_ts_dictionary_cache`), which
    /// a change to any dictionary empties.
    static LEXERS: RefCell<std::collections::HashMap<i64, Rc<crate::ts_dicts::Lexer>>> =
        RefCell::new(std::collections::HashMap::new());
}

/// Install the database's dictionaries for the statements that follow.
pub fn set_user_dicts(dicts: Vec<UserDict>) {
    USER_DICTS.with(|c| *c.borrow_mut() = Rc::new(dicts));
    LEXERS.with(|c| c.borrow_mut().clear());
}

/// The installed dictionaries.
pub fn user_dicts() -> Rc<Vec<UserDict>> {
    USER_DICTS.with(|c| Rc::clone(&c.borrow()))
}

/// A built-in dictionary's dumped row: `(name, template, options)`.
/// A dictionary's `(name, template, options)`.
pub type DictRow = (String, i64, Vec<(String, String)>);

pub fn builtin_dict(oid: i64) -> Option<DictRow> {
    let cat = catalog("pg_ts_dict")?;
    let row = cat
        .rows
        .iter()
        .find(|r| r.first().copied().flatten().and_then(|o| o.parse().ok()) == Some(oid))?;
    let at = |c: &str| cat.columns.iter().position(|x| *x == c);
    let name = row.get(at("dictname")?).copied().flatten()?.to_string();
    let template = row
        .get(at("dicttemplate")?)
        .copied()
        .flatten()?
        .parse()
        .ok()?;
    let options = row
        .get(at("dictinitoption")?)
        .copied()
        .flatten()
        .map(crate::ts_dicts::parse_options_text)
        .unwrap_or_default();
    Some((name, template, options))
}

/// User dictionary `oid`'s template state, its options read (a built-in
/// one keeps `fts`'s own lexing): `None` for a dictionary that is not a
/// user one.
pub fn user_lexer(oid: i64) -> Option<crate::Result<Rc<crate::ts_dicts::Lexer>>> {
    if let Some(l) = LEXERS.with(|c| c.borrow().get(&oid).cloned()) {
        return Some(Ok(l));
    }
    let dicts = user_dicts();
    let d = dicts.iter().find(|d| d.oid == oid)?;
    Some(crate::ts_dicts::init(d.template, &d.options).map(|l| {
        let l = Rc::new(l);
        LEXERS.with(|c| c.borrow_mut().insert(oid, Rc::clone(&l)));
        l
    }))
}

thread_local! {
    static USER_CONFIGS: RefCell<Rc<Vec<UserConfig>>> = RefCell::new(Rc::new(Vec::new()));
    static DROPPED_BUILTINS: RefCell<Rc<Vec<i64>>> = RefCell::new(Rc::new(Vec::new()));
}

/// Install the database's configurations for the statements that follow.
/// A built-in one a statement changed is among them under its own oid,
/// and hides the dumped row; `dropped` lists the built-in ones a DROP took.
pub fn set_user_configs(configs: Vec<UserConfig>) {
    USER_CONFIGS.with(|c| *c.borrow_mut() = Rc::new(configs));
}

/// Install the built-in configurations the database has dropped.
pub fn set_dropped_builtins(dropped: Vec<i64>) {
    DROPPED_BUILTINS.with(|c| *c.borrow_mut() = Rc::new(dropped));
}

/// Is the built-in configuration `oid` no longer the dumped row: changed
/// (the database's own row stands for it) or dropped?
pub fn builtin_hidden(oid: i64) -> bool {
    user_configs().iter().any(|c| c.oid == oid)
        || DROPPED_BUILTINS.with(|d| d.borrow().contains(&oid))
}

/// Is `oid` one of `initdb`'s configurations?
pub fn is_builtin_config(oid: i64) -> bool {
    builtin_names(false).iter().any(|(o, _)| *o == oid)
}

/// A built-in configuration's dumped row: `(name, map)`.
pub fn builtin_config(oid: i64) -> Option<(String, ConfigMap)> {
    let (_, name) = builtin_names(false).iter().find(|(o, _)| *o == oid)?;
    Some((
        (*name).to_string(),
        builtin_maps().get(&oid).cloned().unwrap_or_default(),
    ))
}

/// The installed configurations.
pub fn user_configs() -> Rc<Vec<UserConfig>> {
    USER_CONFIGS.with(|c| Rc::clone(&c.borrow()))
}

/// `(oid, name)` of each built-in row of `pg_ts_config` (`dict` false) or
/// `pg_ts_dict` (`dict` true).
fn builtin_names(dict: bool) -> &'static [(i64, &'static str)] {
    static CONFIGS: OnceLock<Vec<(i64, &'static str)>> = OnceLock::new();
    static DICTS: OnceLock<Vec<(i64, &'static str)>> = OnceLock::new();
    let load = || {
        catalog(if dict { "pg_ts_dict" } else { "pg_ts_config" })
            .map(|c| {
                c.rows
                    .iter()
                    .filter_map(|r| Some(((*r.first()?)?.parse().ok()?, (*r.get(1)?)?)))
                    .collect()
            })
            .unwrap_or_default()
    };
    if dict {
        DICTS.get_or_init(load)
    } else {
        CONFIGS.get_or_init(load)
    }
}

/// A configuration's `pg_ts_config_map` rows: `(token type, seqno,
/// dictionary)`.
pub type ConfigMap = Vec<(i32, i32, i64)>;

/// The built-in configurations' maps, by configuration oid.
fn builtin_maps() -> &'static HashMap<i64, ConfigMap> {
    static MAPS: OnceLock<HashMap<i64, ConfigMap>> = OnceLock::new();
    MAPS.get_or_init(|| {
        let mut maps: HashMap<i64, ConfigMap> = HashMap::new();
        if let Some(c) = catalog("pg_ts_config_map") {
            let at = |n: &str| c.columns.iter().position(|x| *x == n);
            let (Some(cfg), Some(ty), Some(seq), Some(dict)) = (
                at("mapcfg"),
                at("maptokentype"),
                at("mapseqno"),
                at("mapdict"),
            ) else {
                return maps;
            };
            for r in &c.rows {
                let num = |i: usize| {
                    r.get(i)
                        .copied()
                        .flatten()
                        .and_then(|v| v.parse::<i64>().ok())
                };
                if let (Some(cfg), Some(ty), Some(seq), Some(dict)) =
                    (num(cfg), num(ty), num(seq), num(dict))
                {
                    maps.entry(cfg)
                        .or_default()
                        .push((ty as i32, seq as i32, dict));
                }
            }
        }
        maps
    })
}

/// A configuration's map: `(token type, seqno, dictionary)`, or `None` for
/// no configuration.
pub fn map_of(oid: i64) -> Option<Vec<(i32, i32, i64)>> {
    if let Some(c) = user_configs().iter().find(|c| c.oid == oid) {
        return Some(c.map.clone());
    }
    if builtin_hidden(oid) {
        return None;
    }
    if let Some(m) = builtin_maps().get(&oid) {
        return Some(m.clone());
    }
    builtin_names(false)
        .iter()
        .any(|(o, _)| *o == oid)
        .then(Vec::new)
}

/// The dictionaries a configuration offers a token of type `toktype`, in
/// order.
pub fn dictionaries_for(cfg: i64, toktype: i32) -> Vec<i64> {
    let mut entries: Vec<(i32, i64)> = map_of(cfg)
        .unwrap_or_default()
        .into_iter()
        .filter(|(t, _, _)| *t == toktype)
        .map(|(_, seq, dict)| (seq, dict))
        .collect();
    entries.sort();
    entries.into_iter().map(|(_, d)| d).collect()
}

/// A configuration's (or, with `dict`, a dictionary's) schema and name by
/// oid.
pub fn schema_and_name(oid: i64, dict: bool) -> Option<(String, String)> {
    if dict {
        if let Some(d) = user_dicts().iter().find(|d| d.oid == oid) {
            return Some((d.schema.clone(), d.name.clone()));
        }
    }
    if !dict {
        if let Some(c) = user_configs().iter().find(|c| c.oid == oid) {
            return Some((c.schema.clone(), c.name.clone()));
        }
        if builtin_hidden(oid) {
            return None;
        }
    }
    builtin_names(dict)
        .iter()
        .find(|(o, _)| *o == oid)
        .map(|(_, n)| ("pg_catalog".into(), (*n).to_string()))
}

/// A configuration's (or dictionary's) name by oid.
pub fn name_of(oid: i64, dict: bool) -> Option<String> {
    schema_and_name(oid, dict).map(|(_, n)| n)
}

/// `schema.name`, as `default_text_search_config` keeps it.
pub fn qualified_name_of(oid: i64) -> Option<String> {
    schema_and_name(oid, false).map(|(s, n)| {
        format!(
            "{}.{}",
            crate::scalar::quote_identifier(&s),
            crate::scalar::quote_identifier(&n)
        )
    })
}

/// `regconfigout` / `regdictionaryout`: the name, qualified unless the
/// search path finds this object by it.
pub fn display_of(oid: i64, dict: bool) -> Option<String> {
    let (schema, name) = schema_and_name(oid, dict)?;
    let bare = crate::scalar::quote_identifier(&name);
    if oid_in_path(&name, dict) == Some(oid) {
        Some(bare)
    } else {
        Some(format!(
            "{}.{bare}",
            crate::scalar::quote_identifier(&schema)
        ))
    }
}

/// The parts of a possibly qualified name, each an identifier: unquoted is
/// folded to lower case.
pub(crate) fn name_parts(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut was_quoted = false;
    let mut chars = input.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => {
                quoted = !quoted;
                was_quoted = true;
            }
            '.' if !quoted => {
                parts.push(if was_quoted {
                    std::mem::take(&mut cur)
                } else {
                    std::mem::take(&mut cur).trim().to_ascii_lowercase()
                });
                was_quoted = false;
            }
            c => cur.push(c),
        }
    }
    parts.push(if was_quoted {
        cur
    } else {
        cur.trim().to_ascii_lowercase()
    });
    parts
}

/// The configuration (or dictionary) `name` in `schema`.
pub fn oid_in(schema: &str, name: &str, dict: bool) -> Option<i64> {
    if dict {
        if let Some(d) = user_dicts()
            .iter()
            .find(|d| d.schema == schema && d.name == name)
        {
            return Some(d.oid);
        }
    }
    if schema == "pg_catalog" {
        let hidden = |o: i64| {
            if dict {
                user_dicts().iter().any(|d| d.oid == o)
            } else {
                builtin_hidden(o)
            }
        };
        if let Some((o, _)) = builtin_names(dict)
            .iter()
            .find(|(o, n)| *n == name && !hidden(*o))
        {
            return Some(*o);
        }
    }
    if dict {
        return None;
    }
    user_configs()
        .iter()
        .find(|c| c.schema == schema && c.name == name)
        .map(|c| c.oid)
}

/// An unqualified name through the search path: `pg_catalog` first unless
/// the path places it.
fn oid_in_path(name: &str, dict: bool) -> Option<i64> {
    let path = crate::schemas::search_path();
    let mut schemas: Vec<String> = Vec::new();
    if !path.iter().any(|s| s == "pg_catalog") {
        schemas.push("pg_catalog".into());
    }
    schemas.extend(path);
    schemas.iter().find_map(|s| oid_in(s, name, dict))
}

/// A configuration's (or dictionary's) oid by name -- qualified or through
/// the search path -- or by its number.
pub fn oid_of(input: &str, dict: bool) -> Option<i64> {
    if let Ok(oid) = input.trim().parse::<i64>() {
        return schema_and_name(oid, dict).map(|_| oid);
    }
    match name_parts(input).as_slice() {
        [name] => oid_in_path(name, dict),
        [schema, name] => oid_in(schema, name, dict),
        [_, schema, name] => oid_in(schema, name, dict),
        _ => None,
    }
}
