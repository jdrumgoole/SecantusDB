//! Full-text search: `tsvector`, `tsquery`, the `english` and `simple`
//! configurations, and the functions and operators over them.
//!
//! Both types travel as their CANONICAL text -- what PostgreSQL's output
//! function prints -- exactly as `hstore` does, so a stored value is already
//! what the wire sends. Every operation parses that text, works on the
//! structure, and renders it back.
//!
//! What was measured against PostgreSQL 14 rather than assumed:
//!
//! * The document parser emits a hyphenated word as the compound AND its
//!   parts (`foo-bar` is `'foo-bar':1 'foo':2 'bar':3`); a stop-word
//!   consumes a position, a blank does not.
//! * `english` lower-cases, drops the 127 words of `english.stop`, and stems
//!   with Snowball's English (Porter2) algorithm -- only the word token types;
//!   numbers, emails, hosts and URLs go through `simple` unstemmed.
//! * A stop-word in a query is removed and a phrase around it WIDENS:
//!   `fox <-> the <-> quick` is `'fox' <2> 'quick'` (PostgreSQL's
//!   `clean_stopword_intree`, transcribed).
//! * Precedence is `!` > `<->` > `&` > `|`, and the output parenthesises only
//!   where PostgreSQL's `infix` does.

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::{Error, Result};
use bson::Bson;

// ------------------------------------------------------------------------
// Configurations
// ------------------------------------------------------------------------

/// A text-search configuration this server implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Config {
    English,
    Simple,
    /// One of PostgreSQL's other Snowball configurations (`french`, ...).
    Snowball(Lang),
}

/// The Snowball languages PostgreSQL ships a configuration for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Arabic,
    Armenian,
    Basque,
    Catalan,
    Hindi,
    Indonesian,
    Irish,
    Lithuanian,
    Nepali,
    Serbian,
    Yiddish,
    Danish,
    Dutch,
    Finnish,
    French,
    German,
    Greek,
    Hungarian,
    Italian,
    Norwegian,
    Portuguese,
    Romanian,
    Russian,
    Spanish,
    Swedish,
    Tamil,
    Turkish,
}

const LANGS: [Lang; 27] = [
    Lang::Arabic,
    Lang::Armenian,
    Lang::Basque,
    Lang::Catalan,
    Lang::Hindi,
    Lang::Indonesian,
    Lang::Irish,
    Lang::Lithuanian,
    Lang::Nepali,
    Lang::Serbian,
    Lang::Yiddish,
    Lang::Danish,
    Lang::Dutch,
    Lang::Finnish,
    Lang::French,
    Lang::German,
    Lang::Greek,
    Lang::Hungarian,
    Lang::Italian,
    Lang::Norwegian,
    Lang::Portuguese,
    Lang::Romanian,
    Lang::Russian,
    Lang::Spanish,
    Lang::Swedish,
    Lang::Tamil,
    Lang::Turkish,
];

impl Lang {
    pub fn name(self) -> &'static str {
        match self {
            Lang::Arabic => "arabic",
            Lang::Armenian => "armenian",
            Lang::Basque => "basque",
            Lang::Catalan => "catalan",
            Lang::Hindi => "hindi",
            Lang::Indonesian => "indonesian",
            Lang::Irish => "irish",
            Lang::Lithuanian => "lithuanian",
            Lang::Nepali => "nepali",
            Lang::Serbian => "serbian",
            Lang::Yiddish => "yiddish",
            Lang::Danish => "danish",
            Lang::Dutch => "dutch",
            Lang::Finnish => "finnish",
            Lang::French => "french",
            Lang::German => "german",
            Lang::Greek => "greek",
            Lang::Hungarian => "hungarian",
            Lang::Italian => "italian",
            Lang::Norwegian => "norwegian",
            Lang::Portuguese => "portuguese",
            Lang::Romanian => "romanian",
            Lang::Russian => "russian",
            Lang::Spanish => "spanish",
            Lang::Swedish => "swedish",
            Lang::Tamil => "tamil",
            Lang::Turkish => "turkish",
        }
    }

    /// The algorithm when `snowball_stemmers_rs` runs it, else `None` (the
    /// `rust-stemmers` languages; see `algorithm`).
    fn newer_algorithm(self) -> Option<snowball_stemmers_rs::Algorithm> {
        use snowball_stemmers_rs::Algorithm as A;
        Some(match self {
            Lang::Armenian => A::Armenian,
            Lang::Basque => A::Basque,
            Lang::Catalan => A::Catalan,
            Lang::Hindi => A::Hindi,
            Lang::Indonesian => A::Indonesian,
            Lang::Irish => A::Irish,
            Lang::Lithuanian => A::Lithuanian,
            Lang::Nepali => A::Nepali,
            Lang::Serbian => A::Serbian,
            Lang::Yiddish => A::Yiddish,
            _ => return None,
        })
    }

    fn algorithm(self) -> rust_stemmers::Algorithm {
        use rust_stemmers::Algorithm as A;
        match self {
            Lang::Arabic => A::Arabic,
            Lang::Danish => A::Danish,
            Lang::Dutch => A::Dutch,
            Lang::Finnish => A::Finnish,
            Lang::French => A::French,
            Lang::German => A::German,
            Lang::Greek => A::Greek,
            Lang::Hungarian => A::Hungarian,
            Lang::Italian => A::Italian,
            Lang::Norwegian => A::Norwegian,
            Lang::Portuguese => A::Portuguese,
            Lang::Romanian => A::Romanian,
            Lang::Russian => A::Russian,
            Lang::Spanish => A::Spanish,
            Lang::Swedish => A::Swedish,
            Lang::Tamil => A::Tamil,
            Lang::Turkish => A::Turkish,
            // Stemmed by `newer_algorithm`; never reached.
            _ => A::English,
        }
    }

    /// The dictionary's `stopwords` file, when its `_stem` dictionary names
    /// one (PostgreSQL's `tsearch_data/<language>.stop`, vendored as is).
    fn stop_file(self) -> Option<&'static str> {
        Some(match self {
            Lang::Danish => include_str!("tsearch_data/danish.stop"),
            Lang::Dutch => include_str!("tsearch_data/dutch.stop"),
            Lang::Finnish => include_str!("tsearch_data/finnish.stop"),
            Lang::French => include_str!("tsearch_data/french.stop"),
            Lang::German => include_str!("tsearch_data/german.stop"),
            Lang::Hungarian => include_str!("tsearch_data/hungarian.stop"),
            Lang::Italian => include_str!("tsearch_data/italian.stop"),
            Lang::Norwegian => include_str!("tsearch_data/norwegian.stop"),
            Lang::Portuguese => include_str!("tsearch_data/portuguese.stop"),
            Lang::Russian => include_str!("tsearch_data/russian.stop"),
            Lang::Spanish => include_str!("tsearch_data/spanish.stop"),
            Lang::Swedish => include_str!("tsearch_data/swedish.stop"),
            Lang::Turkish => include_str!("tsearch_data/turkish.stop"),
            Lang::Nepali => include_str!("tsearch_data/nepali.stop"),
            _ => return None,
        })
    }

    fn is_stop(self, word: &str) -> bool {
        static SETS: std::sync::OnceLock<Vec<std::collections::HashSet<&'static str>>> =
            std::sync::OnceLock::new();
        let sets = SETS.get_or_init(|| {
            LANGS
                .iter()
                .map(|l| {
                    l.stop_file()
                        .map(|f| f.lines().map(str::trim).filter(|w| !w.is_empty()).collect())
                        .unwrap_or_default()
                })
                .collect()
        });
        LANGS
            .iter()
            .position(|l| *l == self)
            .is_some_and(|i| sets[i].contains(word))
    }

    fn stem(self, word: &str) -> String {
        match self.newer_algorithm() {
            Some(a) => snowball_stemmers_rs::Stemmer::create(a)
                .stem(word)
                .into_owned(),
            None => rust_stemmers::Stemmer::create(self.algorithm())
                .stem(word)
                .into_owned(),
        }
    }
}

impl Config {
    /// The configuration's name, as `regconfig` prints it.
    pub fn name(self) -> &'static str {
        match self {
            Config::English => "english",
            Config::Simple => "simple",
            Config::Snowball(l) => l.name(),
        }
    }
}

/// Resolve a configuration name (`english`, `pg_catalog.simple`, ...).
pub fn config(name: &str) -> Result<Config> {
    let n = name.trim().trim_matches('"').to_ascii_lowercase();
    let n = n.strip_prefix("pg_catalog.").unwrap_or(&n);
    if let Some(l) = LANGS.iter().find(|l| l.name() == n) {
        return Ok(Config::Snowball(*l));
    }
    match n {
        "english" => Ok(Config::English),
        "simple" => Ok(Config::Simple),
        _ => Err(Error::Sqlstate(
            "42704",
            format!(
                "text search configuration \"{}\" does not exist",
                name.trim()
            ),
        )),
    }
}

/// The session default, `default_text_search_config`.
pub const DEFAULT_CONFIG: Config = Config::English;

thread_local! {
    static NOTICES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn notice(msg: &str) {
    NOTICES.with(|n| n.borrow_mut().push(msg.to_string()));
}

/// NOTICEs raised since the last call (a stop-word-only query's).
pub(crate) fn notices_len() -> usize {
    NOTICES.with(|n| n.borrow().len())
}

pub(crate) fn truncate_notices(n: usize) {
    NOTICES.with(|v| v.borrow_mut().truncate(n));
}

pub fn take_notices() -> Vec<String> {
    NOTICES.with(|n| std::mem::take(&mut *n.borrow_mut()))
}

/// PostgreSQL's `english.stop`.
const ENGLISH_STOP: &[&str] = &[
    "i",
    "me",
    "my",
    "myself",
    "we",
    "our",
    "ours",
    "ourselves",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
    "he",
    "him",
    "his",
    "himself",
    "she",
    "her",
    "hers",
    "herself",
    "it",
    "its",
    "itself",
    "they",
    "them",
    "their",
    "theirs",
    "themselves",
    "what",
    "which",
    "who",
    "whom",
    "this",
    "that",
    "these",
    "those",
    "am",
    "is",
    "are",
    "was",
    "were",
    "be",
    "been",
    "being",
    "have",
    "has",
    "had",
    "having",
    "do",
    "does",
    "did",
    "doing",
    "a",
    "an",
    "the",
    "and",
    "but",
    "if",
    "or",
    "because",
    "as",
    "until",
    "while",
    "of",
    "at",
    "by",
    "for",
    "with",
    "about",
    "against",
    "between",
    "into",
    "through",
    "during",
    "before",
    "after",
    "above",
    "below",
    "to",
    "from",
    "up",
    "down",
    "in",
    "out",
    "on",
    "off",
    "over",
    "under",
    "again",
    "further",
    "then",
    "once",
    "here",
    "there",
    "when",
    "where",
    "why",
    "how",
    "all",
    "any",
    "both",
    "each",
    "few",
    "more",
    "most",
    "other",
    "some",
    "such",
    "no",
    "nor",
    "not",
    "only",
    "own",
    "same",
    "so",
    "than",
    "too",
    "very",
    "s",
    "t",
    "can",
    "will",
    "just",
    "don",
    "should",
    "now",
];

// ------------------------------------------------------------------------
// The document parser
// ------------------------------------------------------------------------

/// A token's class, as far as the configurations tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A word (possibly a hyphenated compound or part): stemmed by `english`.
    Word,
    /// Anything else indexed -- numbers, emails, hosts, URLs: `simple`.
    Other,
}

/// Split text into the tokens the default parser indexes, in order.
fn tokens(text: &str) -> Vec<(String, Kind)> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    let is_word = word_char;
    while i < n {
        let c = chars[i];
        // A protocol (`http://`) is a token no configuration indexes; the
        // URL after it is.
        if c.is_ascii_alphabetic() {
            let rest: String = chars[i..n.min(i + 10)].iter().collect();
            let lower = rest.to_ascii_lowercase();
            if let Some(p) = ["https://", "http://", "ftp://"]
                .iter()
                .find(|p| lower.starts_with(**p))
            {
                i += p.len();
                continue;
            }
        }
        let at_boundary = i == 0 || !is_word(chars[i - 1]);
        let signed = c == '-' && i + 1 < n && chars[i + 1].is_ascii_digit() && at_boundary;
        let path = c == '/'
            && i + 1 < n
            && is_word(chars[i + 1])
            && (i == 0 || chars[i - 1].is_whitespace());
        if !is_word(c) && !signed && !path {
            i += 1;
            continue;
        }
        // The longest run of word characters and inner connectors.
        let start = i;
        let mut j = i;
        if chars[j] == '-' || chars[j] == '/' {
            j += 1;
        }
        while j < n {
            let c = chars[j];
            let joiner = matches!(c, '-' | '.' | '@' | '/' | '_' | '+')
                && j + 1 < n
                && is_word(chars[j + 1])
                && j > start;
            if is_word(c) || joiner {
                j += 1;
            } else {
                break;
            }
        }
        let raw: String = chars[start..j].iter().collect();
        i = j;
        emit(&raw, &mut out);
    }
    out
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

fn is_number(s: &str) -> bool {
    let body = s.strip_prefix('-').unwrap_or(s);
    if all_digits(body) {
        return true;
    }
    // float, version, sfloat (`1e5`, `1.5e-3`)
    let (mant, exp) = match body.find(['e', 'E']) {
        Some(p) => (&body[..p], Some(&body[p + 1..])),
        None => (body, None),
    };
    let mant_ok = !mant.is_empty() && mant.split('.').all(all_digits);
    let exp_ok = exp.is_none_or(|e| all_digits(e.strip_prefix(['-', '+']).unwrap_or(e)));
    mant_ok && exp_ok
}

fn is_host(s: &str) -> bool {
    let labels: Vec<&str> = s.split('.').collect();
    labels.len() >= 2
        && labels
            .iter()
            .all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && labels
            .last()
            .is_some_and(|l| l.len() >= 2 && l.chars().all(|c| c.is_ascii_alphabetic()))
}

/// Classify one connected run into the tokens PostgreSQL's parser emits.
fn emit(raw: &str, out: &mut Vec<(String, Kind)>) {
    if is_number(raw) {
        out.push((raw.to_string(), Kind::Other));
        return;
    }
    if let Some((local, host)) = raw.split_once('@') {
        if !local.is_empty() && is_host(host) && !host.contains('@') {
            out.push((raw.to_string(), Kind::Other));
            return;
        }
    }
    if let Some(slash) = raw.find('/') {
        let (host, path) = raw.split_at(slash);
        if is_host(host) {
            out.push((raw.to_string(), Kind::Other));
            out.push((host.to_string(), Kind::Other));
            out.push((path.to_string(), Kind::Other));
            return;
        }
    }
    if is_host(raw) {
        out.push((raw.to_string(), Kind::Other));
        return;
    }
    // A path, or a dotted run of alphanumerics (`v1.2.3`, `file.txt`), is
    // one token.
    if raw.starts_with('/')
        || (raw.contains('.')
            && raw
                .split('.')
                .all(|seg| !seg.is_empty() && seg.chars().all(word_char)))
    {
        out.push((raw.to_string(), Kind::Other));
        return;
    }
    // Split on the connectors that are not part of a word token.
    let mut pieces: Vec<&str> = Vec::new();
    for seg in raw.split(['.', '@', '/', '_', '+']) {
        if !seg.is_empty() {
            pieces.push(seg);
        }
    }
    for seg in pieces {
        let parts: Vec<&str> = seg.split('-').filter(|p| !p.is_empty()).collect();
        let word_like = |p: &str| p.chars().any(letter_char);
        if parts.len() > 1
            && seg.split('-').all(|p| !p.is_empty())
            && parts.iter().any(|p| word_like(p))
        {
            // A hyphenated word: the compound, then each part.
            let numeric = parts.iter().any(|p| p.chars().any(|c| c.is_ascii_digit()));
            out.push((
                seg.to_string(),
                if numeric { Kind::Other } else { Kind::Word },
            ));
            for p in parts {
                let kind = if p.chars().any(|c| c.is_ascii_digit()) {
                    Kind::Other
                } else {
                    Kind::Word
                };
                out.push((p.to_string(), kind));
            }
        } else {
            for p in parts {
                let kind = if is_number(p) || p.chars().any(|c| c.is_ascii_digit()) {
                    Kind::Other
                } else {
                    Kind::Word
                };
                out.push((p.to_string(), kind));
            }
        }
    }
}

/// A letter to PostgreSQL's parser under the UTF-8 locale this server
/// reports (`C.UTF-8`): `iswalpha`, so a Unicode letter -- or a combining
/// mark, which stays inside its word (Devanagari's virama) -- and NOT the
/// punctuation or symbols around it (`«bonjour»` is `bonjour`, `x²` is
/// `x`). A `C`-locale server calls every non-ASCII character a letter.
fn letter_char(c: char) -> bool {
    c.is_ascii_alphabetic()
        || (!c.is_ascii()
            && (c.is_alphabetic() || unicode_normalization::char::is_combining_mark(c)))
}

/// A letter or an ASCII digit, by the same rule.
fn word_char(c: char) -> bool {
    c.is_ascii_digit() || letter_char(c)
}

/// One token through the configuration's dictionary: `None` for a
/// stop-word (which still takes a position).
fn normalise(token: &str, kind: Kind, cfg: Config) -> Option<String> {
    // Lower-casing follows the database's LC_CTYPE, which this server
    // reports as `C.UTF-8`: every letter folds, by the same simple mapping
    // `lower()` uses (`ÄBC` is `äbc`). A `C`-locale PostgreSQL folds ASCII
    // only; the server is consistent with the locale it reports.
    let lower: String = token.chars().map(crate::scalar::simple_lower).collect();
    let english = |lower: String| {
        if ENGLISH_STOP.contains(&lower.as_str()) {
            None
        } else {
            Some(stem(&lower))
        }
    };
    match (cfg, kind) {
        (Config::Simple, _) | (_, Kind::Other) => Some(lower),
        (Config::English, Kind::Word) => english(lower),
        // `russian` sends its ASCII words to english_stem.
        (Config::Snowball(Lang::Russian), Kind::Word) if lower.is_ascii() => english(lower),
        (Config::Snowball(lang), Kind::Word) => {
            if lang.is_stop(&lower) {
                None
            } else {
                Some(lang.stem(&lower))
            }
        }
    }
}

/// PostgreSQL's longest indexable lexeme, in bytes.
const MAX_LEXEME: usize = 2047;
/// The largest position; later ones are clamped to it.
const MAX_POS: u16 = 16383;

/// `(position, lexeme)` for every indexed token; a stop-word yields a
/// position with no lexeme.
fn parse_document(text: &str, cfg: Config) -> Vec<(u16, Option<String>)> {
    let mut out = Vec::new();
    let mut pos: u16 = 0;
    for (tok, kind) in tokens(text) {
        if tok.len() > MAX_LEXEME {
            notice("word is too long to be indexed");
            continue;
        }
        pos = (pos + 1).min(MAX_POS);
        out.push((pos, normalise(&tok, kind, cfg)));
    }
    out
}

// ------------------------------------------------------------------------
// tsvector
// ------------------------------------------------------------------------

/// A position's weight: 3 = A, 2 = B, 1 = C, 0 = D.
type Weight = u8;

/// A tsvector: lexemes in PostgreSQL's order, each with its positions.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TsVector(pub BTreeMap<String, Vec<(u16, Weight)>>);

impl TsVector {
    fn normalise(mut self) -> Self {
        for positions in self.0.values_mut() {
            positions.sort_by_key(|p| p.0);
            let mut out: Vec<(u16, Weight)> = Vec::with_capacity(positions.len());
            for &(p, w) in positions.iter() {
                match out.last_mut() {
                    Some(last) if last.0 == p => last.1 = last.1.max(w),
                    _ => out.push((p, w)),
                }
            }
            out.truncate(256);
            *positions = out;
        }
        self
    }
}

fn weight_letter(w: Weight) -> Option<char> {
    match w {
        3 => Some('A'),
        2 => Some('B'),
        1 => Some('C'),
        _ => None,
    }
}

fn quote_lexeme(lex: &str) -> String {
    let mut s = String::with_capacity(lex.len() + 2);
    s.push('\'');
    for c in lex.chars() {
        if c == '\'' || c == '\\' {
            s.push(c);
        }
        s.push(c);
    }
    s.push('\'');
    s
}

/// A tsvector's text form.
pub fn render_vector(v: &TsVector) -> String {
    let mut parts = Vec::with_capacity(v.0.len());
    for (lex, positions) in &v.0 {
        let mut s = quote_lexeme(lex);
        if !positions.is_empty() {
            s.push(':');
            let ps: Vec<String> = positions
                .iter()
                .map(|(p, w)| match weight_letter(*w) {
                    Some(l) => format!("{p}{l}"),
                    None => p.to_string(),
                })
                .collect();
            s.push_str(&ps.join(","));
        }
        parts.push(s);
    }
    parts.join(" ")
}

fn syntax(kind: &str, text: &str) -> Error {
    Error::Sqlstate("42601", format!("syntax error in {kind}: \"{text}\""))
}

/// Read one lexeme -- quoted (`'it''s'`) or bare -- honouring backslash
/// escapes. Stops a bare one at whitespace or `stop`.
fn read_word(chars: &[char], i: &mut usize, stop: &[char]) -> Option<(String, bool)> {
    let n = chars.len();
    let mut out = String::new();
    if *i < n && chars[*i] == '\'' {
        *i += 1;
        loop {
            if *i >= n {
                return None;
            }
            let c = chars[*i];
            if c == '\\' && *i + 1 < n {
                out.push(chars[*i + 1]);
                *i += 2;
            } else if c == '\'' {
                if *i + 1 < n && chars[*i + 1] == '\'' {
                    out.push('\'');
                    *i += 2;
                } else {
                    *i += 1;
                    return Some((out, true));
                }
            } else {
                out.push(c);
                *i += 1;
            }
        }
    }
    while *i < n {
        let c = chars[*i];
        if c.is_whitespace() || stop.contains(&c) {
            break;
        }
        if c == '\\' && *i + 1 < n {
            out.push(chars[*i + 1]);
            *i += 2;
            continue;
        }
        out.push(c);
        *i += 1;
    }
    Some((out, false))
}

/// `tsvector_in`.
pub fn parse_vector(text: &str) -> Result<TsVector> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut map: BTreeMap<String, Vec<(u16, Weight)>> = BTreeMap::new();
    loop {
        while i < n && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= n {
            break;
        }
        let (lex, _) = read_word(&chars, &mut i, &[':']).ok_or_else(|| syntax("tsvector", text))?;
        if lex.is_empty() {
            return Err(syntax("tsvector", text));
        }
        let entry = map.entry(lex).or_default();
        if i < n && chars[i] == ':' {
            i += 1;
            loop {
                let start = i;
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if i == start {
                    return Err(syntax("tsvector", text));
                }
                let p: u32 = chars[start..i]
                    .iter()
                    .collect::<String>()
                    .parse()
                    .unwrap_or(u32::MAX);
                if p == 0 {
                    return Err(Error::Sqlstate(
                        "42601",
                        format!("wrong position info in tsvector: \"{text}\""),
                    ));
                }
                let mut w = 0;
                if i < n {
                    match chars[i].to_ascii_uppercase() {
                        'A' => w = 3,
                        'B' => w = 2,
                        'C' => w = 1,
                        'D' | '*' => {}
                        _ => i -= 1,
                    }
                    i += 1;
                }
                entry.push((p.min(u32::from(MAX_POS)) as u16, w));
                if i < n && chars[i] == ',' {
                    i += 1;
                    continue;
                }
                break;
            }
        }
    }
    Ok(TsVector(map).normalise())
}

/// `to_tsvector(config, text)`.
pub fn to_tsvector(cfg: Config, text: &str) -> TsVector {
    let mut map: BTreeMap<String, Vec<(u16, Weight)>> = BTreeMap::new();
    for (pos, lex) in parse_document(text, cfg) {
        if let Some(lex) = lex {
            map.entry(lex).or_default().push((pos, 0));
        }
    }
    TsVector(map).normalise()
}

// ------------------------------------------------------------------------
// tsquery
// ------------------------------------------------------------------------

/// A tsquery node.
#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    /// An operand: the lexeme, a `:*` prefix flag, and a weight mask
    /// (A = 8, B = 4, C = 2, D = 1; 0 means any).
    Val {
        lex: String,
        prefix: bool,
        weight: u8,
    },
    /// A stop-word placeholder, removed by `clean`.
    Stop,
    Not(Box<Query>),
    And(Box<Query>, Box<Query>),
    Or(Box<Query>, Box<Query>),
    Phrase(Box<Query>, Box<Query>, u16),
}

fn priority(q: &Query) -> u8 {
    match q {
        Query::Or(..) => 1,
        Query::And(..) => 2,
        Query::Phrase(..) => 3,
        Query::Not(_) => 4,
        _ => 5,
    }
}

fn render_node(q: &Query, parent: u8, right_phrase: bool, out: &mut String) {
    match q {
        Query::Val {
            lex,
            prefix,
            weight,
        } => {
            out.push_str(&quote_lexeme(lex));
            if *prefix || *weight != 0 {
                out.push(':');
                if *prefix {
                    out.push('*');
                }
                for (bit, l) in [(8, 'A'), (4, 'B'), (2, 'C'), (1, 'D')] {
                    if weight & bit != 0 {
                        out.push(l);
                    }
                }
            }
        }
        Query::Stop => {}
        Query::Not(inner) => {
            let p = priority(q);
            let paren = p < parent;
            if paren {
                out.push_str("( ");
            }
            out.push('!');
            render_node(inner, p, false, out);
            if paren {
                out.push_str(" )");
            }
        }
        Query::And(l, r) | Query::Or(l, r) | Query::Phrase(l, r, _) => {
            let p = priority(q);
            let is_phrase = matches!(q, Query::Phrase(..));
            let paren = p < parent || (is_phrase && right_phrase);
            if paren {
                out.push_str("( ");
            }
            render_node(l, p, false, out);
            match q {
                Query::And(..) => out.push_str(" & "),
                Query::Or(..) => out.push_str(" | "),
                Query::Phrase(_, _, 1) => out.push_str(" <-> "),
                Query::Phrase(_, _, d) => out.push_str(&format!(" <{d}> ")),
                _ => unreachable!(),
            }
            render_node(r, p, is_phrase, out);
            if paren {
                out.push_str(" )");
            }
        }
    }
}

/// A tsquery's text form (`''` for the empty query).
pub fn render_query(q: &Option<Query>) -> String {
    let mut s = String::new();
    if let Some(q) = q {
        render_node(q, 0, false, &mut s);
    }
    s
}

/// How an operand is turned into lexemes.
#[derive(Clone, Copy)]
enum Morph {
    /// `::tsquery`: taken as written.
    Literal,
    /// `to_tsquery(config, ...)`: through the configuration.
    Config(Config),
}

struct QueryParser<'a> {
    chars: Vec<char>,
    i: usize,
    text: &'a str,
    morph: Morph,
}

impl QueryParser<'_> {
    fn ws(&mut self) {
        while self.i < self.chars.len() && self.chars[self.i].is_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn no_operand(&self) -> Error {
        Error::Sqlstate("42601", format!("no operand in tsquery: \"{}\"", self.text))
    }

    // or := and ('|' and)*
    fn parse_or(&mut self) -> Result<Query> {
        let mut left = self.parse_and()?;
        loop {
            self.ws();
            if self.peek() == Some('|') {
                self.i += 1;
                let right = self.parse_and()?;
                left = Query::Or(Box::new(left), Box::new(right));
            } else {
                return Ok(left);
            }
        }
    }

    fn parse_and(&mut self) -> Result<Query> {
        let mut left = self.parse_phrase()?;
        loop {
            self.ws();
            if self.peek() == Some('&') {
                self.i += 1;
                let right = self.parse_phrase()?;
                left = Query::And(Box::new(left), Box::new(right));
            } else {
                return Ok(left);
            }
        }
    }

    /// `<->` / `<N>`, if one starts here.
    fn phrase_op(&mut self) -> Result<Option<u16>> {
        self.ws();
        if self.peek() != Some('<') {
            return Ok(None);
        }
        let start = self.i;
        self.i += 1;
        if self.peek() == Some('-') && self.chars.get(self.i + 1) == Some(&'>') {
            self.i += 2;
            return Ok(Some(1));
        }
        let ds = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.i > ds && self.peek() == Some('>') {
            let d: u32 = self.chars[ds..self.i]
                .iter()
                .collect::<String>()
                .parse()
                .unwrap_or(u32::MAX);
            self.i += 1;
            if d > 16384 {
                return Err(Error::Sqlstate(
                    "22023",
                    "distance in phrase operator must be an integer value between zero and 16384 inclusive".to_string(),
                ));
            }
            return Ok(Some(d as u16));
        }
        self.i = start;
        Err(syntax("tsquery", self.text))
    }

    fn parse_phrase(&mut self) -> Result<Query> {
        let mut left = self.parse_unary()?;
        while let Some(d) = self.phrase_op()? {
            let right = self.parse_unary()?;
            left = Query::Phrase(Box::new(left), Box::new(right), d);
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Query> {
        self.ws();
        match self.peek() {
            None => Err(self.no_operand()),
            Some('!') => {
                self.i += 1;
                Ok(Query::Not(Box::new(self.parse_unary()?)))
            }
            Some('(') => {
                self.i += 1;
                let q = self.parse_or()?;
                self.ws();
                if self.peek() != Some(')') {
                    return Err(syntax("tsquery", self.text));
                }
                self.i += 1;
                Ok(q)
            }
            Some('&' | '|' | ')' | '<') => Err(syntax("tsquery", self.text)),
            Some(_) => self.operand(),
        }
    }

    fn operand(&mut self) -> Result<Query> {
        let (word, _) = read_word(
            &self.chars,
            &mut self.i,
            &['&', '|', '!', '(', ')', '<', ':'],
        )
        .ok_or_else(|| syntax("tsquery", self.text))?;
        let mut prefix = false;
        let mut weight = 0u8;
        if self.peek() == Some(':') {
            self.i += 1;
            while let Some(c) = self.peek() {
                match c.to_ascii_uppercase() {
                    '*' => prefix = true,
                    'A' => weight |= 8,
                    'B' => weight |= 4,
                    'C' => weight |= 2,
                    'D' => weight |= 1,
                    _ => break,
                }
                self.i += 1;
            }
        }
        if word.is_empty() {
            return Err(syntax("tsquery", self.text));
        }
        Ok(match self.morph {
            Morph::Literal => Query::Val {
                lex: word,
                prefix,
                weight,
            },
            Morph::Config(cfg) => morph(&word, cfg, prefix, weight, true),
        })
    }
}

/// An operand's text through the configuration: its lexemes joined by
/// `<->` (or `&` for `plainto_tsquery`) at their position distances, or a
/// stop-word placeholder when it has none -- PostgreSQL's `pushval_morph`.
fn morph(text: &str, cfg: Config, prefix: bool, weight: u8, phrase: bool) -> Query {
    let mut out: Option<Query> = None;
    let mut prev: u16 = 0;
    for (pos, lex) in parse_document(text, cfg) {
        let Some(lex) = lex else { continue };
        let v = Query::Val {
            lex,
            prefix,
            weight,
        };
        out = Some(match out {
            None => v,
            Some(l) if phrase => Query::Phrase(Box::new(l), Box::new(v), pos - prev),
            Some(l) => Query::And(Box::new(l), Box::new(v)),
        });
        prev = pos;
    }
    out.unwrap_or(Query::Stop)
}

/// Remove stop-word placeholders, widening the phrases around them --
/// `clean_stopword_intree`. `(node, ladd, radd)`.
#[allow(clippy::type_complexity)]
fn clean_stop(q: Query) -> (Option<Query>, u16, u16) {
    match q {
        Query::Val { .. } => (Some(q), 0, 0),
        Query::Stop => (None, 0, 0),
        Query::Not(inner) => match clean_stop(*inner) {
            (Some(i), l, r) => (Some(Query::Not(Box::new(i))), l, r),
            (None, _, _) => (None, 0, 0),
        },
        other => {
            let (is_phrase, dist, l, r, rebuild): (
                bool,
                u16,
                Query,
                Query,
                fn(Query, Query, u16) -> Query,
            ) = match other {
                Query::And(l, r) => (false, 0, *l, *r, |a, b, _| {
                    Query::And(Box::new(a), Box::new(b))
                }),
                Query::Or(l, r) => (false, 0, *l, *r, |a, b, _| {
                    Query::Or(Box::new(a), Box::new(b))
                }),
                Query::Phrase(l, r, d) => (true, d, *l, *r, |a, b, d| {
                    Query::Phrase(Box::new(a), Box::new(b), d)
                }),
                _ => unreachable!(),
            };
            let (left, lladd, lradd) = clean_stop(l);
            let (right, rladd, rradd) = clean_stop(r);
            match (left, right) {
                (None, None) => {
                    let add = if is_phrase { lladd + dist + rradd } else { 0 };
                    (None, add, add)
                }
                (None, Some(r)) => {
                    let ladd = if is_phrase { lladd + dist + rladd } else { 0 };
                    (Some(r), ladd, rradd)
                }
                (Some(l), None) => {
                    let radd = if is_phrase { lradd + dist + rradd } else { 0 };
                    (Some(l), lladd, radd)
                }
                (Some(l), Some(r)) => {
                    if is_phrase {
                        (Some(rebuild(l, r, dist + lradd + rladd)), lladd, rradd)
                    } else {
                        (Some(rebuild(l, r, 0)), 0, 0)
                    }
                }
            }
        }
    }
}

fn finish(q: Query) -> Option<Query> {
    let (q, _, _) = clean_stop(q);
    if q.is_none() {
        notice("text-search query contains only stop words or doesn't contain lexemes, ignored");
    }
    q
}

/// `tsquery_in`.
pub fn parse_query(text: &str) -> Result<Option<Query>> {
    parse_query_with(text, Morph::Literal)
}

fn parse_query_with(text: &str, morph: Morph) -> Result<Option<Query>> {
    let mut p = QueryParser {
        chars: text.chars().collect(),
        i: 0,
        text,
        morph,
    };
    p.ws();
    if p.i >= p.chars.len() {
        notice("text-search query doesn't contain lexemes: \"\"");
        return Ok(None);
    }
    let q = p.parse_or()?;
    p.ws();
    if p.i < p.chars.len() {
        return Err(syntax("tsquery", text));
    }
    Ok(match morph {
        Morph::Literal => Some(q),
        Morph::Config(_) => finish(q),
    })
}

/// `to_tsquery(config, text)`.
pub fn to_tsquery(cfg: Config, text: &str) -> Result<Option<Query>> {
    parse_query_with(text, Morph::Config(cfg))
}

/// `plainto_tsquery`.
pub fn plainto_tsquery(cfg: Config, text: &str) -> Option<Query> {
    finish(morph(text, cfg, false, 0, false))
}

/// `phraseto_tsquery`.
pub fn phraseto_tsquery(cfg: Config, text: &str) -> Option<Query> {
    finish(morph(text, cfg, false, 0, true))
}

/// `websearch_to_tsquery`: words AND together, `"..."` is a phrase, a
/// leading `-` negates, and the word `or` separates alternatives.
pub fn websearch_to_tsquery(cfg: Config, text: &str) -> Option<Query> {
    enum Item {
        Term(Query),
        Or,
    }
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut items: Vec<Item> = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let mut negate = false;
        let mut j = i;
        if c == '-' {
            negate = true;
            j += 1;
        }
        if j < n && chars[j] == '"' {
            let start = j + 1;
            let mut k = start;
            while k < n && chars[k] != '"' {
                k += 1;
            }
            let phrase: String = chars[start..k].iter().collect();
            i = (k + 1).min(n);
            let q = morph(&phrase, cfg, false, 0, true);
            items.push(Item::Term(if negate { Query::Not(Box::new(q)) } else { q }));
            continue;
        }
        let start = j;
        let mut k = j;
        while k < n && !chars[k].is_whitespace() && chars[k] != '"' {
            k += 1;
        }
        let word: String = chars[start..k].iter().collect();
        i = k.max(i + 1);
        if !negate && word.eq_ignore_ascii_case("or") {
            items.push(Item::Or);
            continue;
        }
        if word.is_empty() {
            continue;
        }
        let q = morph(&word, cfg, false, 0, true);
        if q == Query::Stop && !word.chars().any(word_char) {
            continue;
        }
        items.push(Item::Term(if negate { Query::Not(Box::new(q)) } else { q }));
    }
    // OR binds loosest; an OR with nothing on one side is ignored.
    let mut groups: Vec<Vec<Query>> = vec![Vec::new()];
    for item in items {
        match item {
            Item::Term(q) => groups.last_mut().expect("one group").push(q),
            Item::Or => {
                if !groups.last().expect("one group").is_empty() {
                    groups.push(Vec::new());
                }
            }
        }
    }
    let mut out: Option<Query> = None;
    for g in groups.into_iter().filter(|g| !g.is_empty()) {
        let mut it = g.into_iter();
        let first = it.next().expect("non-empty");
        let and = it.fold(first, |a, b| Query::And(Box::new(a), Box::new(b)));
        out = Some(match out {
            None => and,
            Some(o) => Query::Or(Box::new(o), Box::new(and)),
        });
    }
    match out {
        Some(q) => finish(q),
        None => {
            notice("text-search query doesn't contain lexemes: \"\"");
            None
        }
    }
}

// ------------------------------------------------------------------------
// Matching
// ------------------------------------------------------------------------

/// Positions a subquery matches at (the RIGHT end of a phrase), with the
/// phrase's width, or "everywhere but" for a negation.
struct Hits {
    positions: Vec<u16>,
    width: u16,
    negate: bool,
    /// Matched without position information (a stripped vector).
    lossy: bool,
}

fn weight_ok(mask: u8, w: Weight) -> bool {
    mask == 0 || mask & (1 << w) != 0
}

fn leaf_positions(v: &TsVector, lex: &str, prefix: bool, weight: u8) -> (Vec<u16>, bool) {
    let mut out = Vec::new();
    let mut present = false;
    let mut consider = |positions: &Vec<(u16, Weight)>| {
        if positions.is_empty() {
            if weight == 0 || weight & 1 != 0 {
                present = true;
            }
            return;
        }
        for &(p, w) in positions {
            if weight_ok(weight, w) {
                out.push(p);
                present = true;
            }
        }
    };
    if prefix {
        for (_, positions) in
            v.0.range(lex.to_string()..)
                .take_while(|(k, _)| k.starts_with(lex))
        {
            consider(positions);
        }
    } else if let Some(positions) = v.0.get(lex) {
        consider(positions);
    }
    out.sort_unstable();
    out.dedup();
    (out, present)
}

fn phrase_hits(v: &TsVector, q: &Query) -> Hits {
    match q {
        Query::Val {
            lex,
            prefix,
            weight,
        } => {
            let (positions, present) = leaf_positions(v, lex, *prefix, *weight);
            let lossy = present && positions.is_empty();
            Hits {
                positions,
                width: 0,
                negate: false,
                lossy,
            }
        }
        Query::Stop => Hits {
            positions: Vec::new(),
            width: 0,
            negate: false,
            lossy: false,
        },
        Query::Not(inner) => {
            let h = phrase_hits(v, inner);
            Hits {
                negate: !h.negate,
                ..h
            }
        }
        Query::Or(l, r) => {
            let (a, b) = (phrase_hits(v, l), phrase_hits(v, r));
            let mut positions = if a.negate || b.negate {
                // "not A or B": treat as everywhere, conservatively.
                return Hits {
                    positions: Vec::new(),
                    width: a.width.max(b.width),
                    negate: true,
                    lossy: a.lossy || b.lossy,
                };
            } else {
                a.positions.clone()
            };
            positions.extend(&b.positions);
            positions.sort_unstable();
            positions.dedup();
            Hits {
                positions,
                width: a.width.max(b.width),
                negate: false,
                lossy: a.lossy || b.lossy,
            }
        }
        Query::And(l, r) => {
            let (a, b) = (phrase_hits(v, l), phrase_hits(v, r));
            let positions: Vec<u16> = match (a.negate, b.negate) {
                (false, false) => a
                    .positions
                    .iter()
                    .copied()
                    .filter(|p| b.positions.contains(p))
                    .collect(),
                (false, true) => a
                    .positions
                    .iter()
                    .copied()
                    .filter(|p| !b.positions.contains(p))
                    .collect(),
                (true, false) => b
                    .positions
                    .iter()
                    .copied()
                    .filter(|p| !a.positions.contains(p))
                    .collect(),
                (true, true) => {
                    let mut all = a.positions.clone();
                    all.extend(&b.positions);
                    all.sort_unstable();
                    all.dedup();
                    return Hits {
                        positions: all,
                        width: a.width.max(b.width),
                        negate: true,
                        lossy: a.lossy || b.lossy,
                    };
                }
            };
            Hits {
                positions,
                width: a.width.max(b.width),
                negate: false,
                lossy: a.lossy || b.lossy,
            }
        }
        Query::Phrase(l, r, d) => {
            let (a, b) = (phrase_hits(v, l), phrase_hits(v, r));
            let lossy = a.lossy || b.lossy;
            let width = a.width + d + b.width;
            if lossy {
                return Hits {
                    positions: Vec::new(),
                    width,
                    negate: false,
                    lossy: true,
                };
            }
            let left_ok = |end: i32| -> bool {
                if end < 1 {
                    return a.negate;
                }
                let has = a.positions.contains(&(end as u16));
                has != a.negate
            };
            let mut out = Vec::new();
            if b.negate {
                // Right side absent at r: every r whose left end matches.
                if a.negate {
                    return Hits {
                        positions: Vec::new(),
                        width,
                        negate: true,
                        lossy: false,
                    };
                }
                for &lp in &a.positions {
                    let r = lp as i32 + *d as i32 + b.width as i32;
                    if r > 0 && r <= i32::from(MAX_POS) && !b.positions.contains(&(r as u16)) {
                        out.push(r as u16);
                    }
                }
            } else {
                for &rp in &b.positions {
                    let end = rp as i32 - b.width as i32 - *d as i32;
                    if left_ok(end) {
                        out.push(rp);
                    }
                }
            }
            out.sort_unstable();
            out.dedup();
            Hits {
                positions: out,
                width,
                negate: false,
                lossy: false,
            }
        }
    }
}

fn eval(v: &TsVector, q: &Query) -> bool {
    match q {
        Query::Val {
            lex,
            prefix,
            weight,
        } => leaf_positions(v, lex, *prefix, *weight).1,
        Query::Stop => false,
        Query::Not(inner) => !eval(v, inner),
        Query::And(l, r) => eval(v, l) && eval(v, r),
        Query::Or(l, r) => eval(v, l) || eval(v, r),
        Query::Phrase(..) => {
            let h = phrase_hits(v, q);
            if h.lossy {
                // Without positions a phrase cannot be satisfied: measured,
                // a stripped vector matches no `<->` query.
                return false;
            }
            if h.negate {
                true
            } else {
                !h.positions.is_empty()
            }
        }
    }
}

/// `tsvector @@ tsquery`.
pub fn matches(v: &TsVector, q: &Option<Query>) -> bool {
    match q {
        None => false,
        Some(q) => eval(v, q),
    }
}

// ------------------------------------------------------------------------
// Functions over the types
// ------------------------------------------------------------------------

/// `numnode`.
pub fn numnode(q: &Option<Query>) -> i32 {
    fn count(q: &Query) -> i32 {
        match q {
            Query::Val { .. } | Query::Stop => 1,
            Query::Not(i) => 1 + count(i),
            Query::And(l, r) | Query::Or(l, r) | Query::Phrase(l, r, _) => 1 + count(l) + count(r),
        }
    }
    q.as_ref().map_or(0, count)
}

/// `querytree`: the query with its negated parts removed, `T` when nothing
/// indexable is left.
pub fn querytree(q: &Option<Query>) -> String {
    fn clean(q: &Query) -> Option<Query> {
        match q {
            Query::Val { .. } => Some(q.clone()),
            Query::Stop | Query::Not(_) => None,
            Query::Or(l, r) => Some(Query::Or(Box::new(clean(l)?), Box::new(clean(r)?))),
            Query::And(l, r) | Query::Phrase(l, r, _) => match (clean(l), clean(r)) {
                (None, None) => None,
                (Some(x), None) | (None, Some(x)) => Some(x),
                (Some(a), Some(b)) => Some(match q {
                    Query::And(..) => Query::And(Box::new(a), Box::new(b)),
                    Query::Phrase(_, _, d) => Query::Phrase(Box::new(a), Box::new(b), *d),
                    _ => unreachable!(),
                }),
            },
        }
    }
    match q {
        None => String::new(),
        Some(q) => match clean(q) {
            None => "T".to_string(),
            Some(c) => render_query(&Some(c)),
        },
    }
}

/// `setweight(v, w [, lexemes])`.
pub fn setweight(v: &TsVector, weight: &str, only: Option<&[String]>) -> Result<TsVector> {
    let w = match weight.chars().next().map(|c| c.to_ascii_uppercase()) {
        Some('A') => 3,
        Some('B') => 2,
        Some('C') => 1,
        Some('D') => 0,
        _ => {
            return Err(Error::Sqlstate(
                "XX000",
                format!(
                    "unrecognized weight: {}",
                    weight.chars().next().map_or(0, |c| c as u32)
                ),
            ))
        }
    };
    let mut out = v.clone();
    for (lex, positions) in out.0.iter_mut() {
        if only.is_some_and(|o| !o.iter().any(|x| x == lex)) {
            continue;
        }
        for p in positions.iter_mut() {
            p.1 = w;
        }
    }
    Ok(out)
}

/// `strip`.
pub fn strip(v: &TsVector) -> TsVector {
    TsVector(v.0.keys().map(|k| (k.clone(), Vec::new())).collect())
}

/// `tsvector || tsvector`: the right side's positions shift past the left's
/// largest.
pub fn concat(a: &TsVector, b: &TsVector) -> TsVector {
    let shift =
        a.0.values()
            .flat_map(|p| p.iter().map(|x| x.0))
            .max()
            .unwrap_or(0);
    let mut out = a.clone();
    for (lex, positions) in &b.0 {
        let e = out.0.entry(lex.clone()).or_default();
        if positions.is_empty() {
            continue;
        }
        e.extend(
            positions
                .iter()
                .map(|(p, w)| ((p + shift).min(MAX_POS), *w)),
        );
    }
    out.normalise()
}

/// `tsquery && tsquery` / `tsquery || tsquery` / `!! tsquery`.
pub fn combine(op: &str, a: &Option<Query>, b: &Option<Query>) -> Option<Query> {
    match (a, b) {
        (None, x) | (x, None) => x.clone(),
        (Some(a), Some(b)) => Some(match op {
            "&&" => Query::And(Box::new(a.clone()), Box::new(b.clone())),
            "||" => Query::Or(Box::new(a.clone()), Box::new(b.clone())),
            _ => Query::Phrase(Box::new(a.clone()), Box::new(b.clone()), 1),
        }),
    }
}

pub fn negate(a: &Option<Query>) -> Option<Query> {
    a.as_ref().map(|q| Query::Not(Box::new(q.clone())))
}

/// `ts_headline(config, document, query)` with the default options:
/// `StartSel=<b>, StopSel=</b>, MaxWords=35, MinWords=15`. A document of
/// at most `MaxWords` words is returned whole with each matching word
/// wrapped; a longer one is cut to the first window of `MaxWords` words
/// that holds a match.
pub fn headline(cfg: Config, doc: &str, q: &Option<Query>) -> String {
    let mut wanted: Vec<(String, bool)> = Vec::new();
    fn collect(q: &Query, out: &mut Vec<(String, bool)>) {
        match q {
            Query::Val { lex, prefix, .. } => out.push((lex.clone(), *prefix)),
            Query::Not(_) | Query::Stop => {}
            Query::And(l, r) | Query::Or(l, r) | Query::Phrase(l, r, _) => {
                collect(l, out);
                collect(r, out);
            }
        }
    }
    if let Some(q) = q {
        collect(q, &mut wanted);
    }
    let hit = |word: &str| -> bool {
        let Some(lex) = normalise(word, Kind::Word, cfg) else {
            return false;
        };
        wanted.iter().any(|(w, prefix)| {
            if *prefix {
                lex.starts_with(w.as_str())
            } else {
                &lex == w
            }
        })
    };
    // Split into words and the separators between them, keeping both.
    let mut pieces: Vec<(String, bool)> = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    for c in doc.chars() {
        let w = word_char(c);
        if w != in_word && !cur.is_empty() {
            pieces.push((std::mem::take(&mut cur), in_word));
        }
        in_word = w;
        cur.push(c);
    }
    if !cur.is_empty() {
        pieces.push((cur, in_word));
    }
    let words = pieces.iter().filter(|p| p.1).count();
    let (from, to) = if words <= 35 {
        (0, pieces.len())
    } else {
        let first = pieces.iter().position(|(t, w)| *w && hit(t)).unwrap_or(0);
        let mut end = first;
        let mut count = 0;
        while end < pieces.len() && count < 35 {
            if pieces[end].1 {
                count += 1;
            }
            end += 1;
        }
        (first, end)
    };
    let mut out = String::new();
    for (t, w) in &pieces[from..to] {
        if *w && hit(t) {
            out.push_str("<b>");
            out.push_str(t);
            out.push_str("</b>");
        } else {
            out.push_str(t);
        }
    }
    out
}

// ------------------------------------------------------------------------
// The English (Porter2) stemmer
// ------------------------------------------------------------------------

const VOWELS: &[char] = &['a', 'e', 'i', 'o', 'u', 'y'];

fn is_v(w: &[char], i: usize) -> bool {
    i < w.len() && VOWELS.contains(&w[i])
}

fn ends(w: &[char], s: &str) -> bool {
    let s: Vec<char> = s.chars().collect();
    w.len() >= s.len() && w[w.len() - s.len()..] == s[..]
}

fn has_vowel(w: &[char]) -> bool {
    w.iter().any(|c| VOWELS.contains(c))
}

fn regions(w: &[char]) -> (usize, usize) {
    let s: String = w.iter().collect();
    let mut r1 = w.len();
    let mut special = false;
    for p in ["gener", "commun", "arsen"] {
        if s.starts_with(p) {
            r1 = p.len();
            special = true;
            break;
        }
    }
    if !special {
        for i in 1..w.len() {
            if !VOWELS.contains(&w[i]) && VOWELS.contains(&w[i - 1]) {
                r1 = i + 1;
                break;
            }
        }
    }
    let mut r2 = w.len();
    for i in (r1 + 1)..w.len() {
        if !VOWELS.contains(&w[i]) && VOWELS.contains(&w[i - 1]) {
            r2 = i + 1;
            break;
        }
    }
    (r1, r2)
}

fn ends_short_syllable(w: &[char]) -> bool {
    if w.len() == 2 {
        return is_v(w, 0) && !is_v(w, 1);
    }
    if w.len() < 3 {
        return false;
    }
    let i = w.len() - 1;
    !is_v(w, i) && !matches!(w[i], 'w' | 'x' | 'Y') && is_v(w, i - 1) && !is_v(w, i - 2)
}

fn cut(w: &mut Vec<char>, n: usize) {
    let l = w.len();
    w.truncate(l - n);
}

fn push(w: &mut Vec<char>, s: &str) {
    w.extend(s.chars());
}

/// The Porter2 stem of a lower-cased word -- the algorithm PostgreSQL's
/// `english_stem` dictionary runs.
pub fn stem(word: &str) -> String {
    const SPECIAL: &[(&str, &str)] = &[
        ("skis", "ski"),
        ("skies", "sky"),
        ("dying", "die"),
        ("lying", "lie"),
        ("tying", "tie"),
        ("idly", "idl"),
        ("gently", "gentl"),
        ("ugly", "ugli"),
        ("early", "earli"),
        ("only", "onli"),
        ("singly", "singl"),
        ("sky", "sky"),
        ("news", "news"),
        ("howe", "howe"),
        ("atlas", "atlas"),
        ("cosmos", "cosmos"),
        ("bias", "bias"),
        ("andes", "andes"),
    ];
    const STOP_1A: &[&str] = &[
        "inning", "outing", "canning", "herring", "earring", "proceed", "exceed", "succeed",
    ];
    const STEP2: &[(&str, &str)] = &[
        ("ization", "ize"),
        ("ational", "ate"),
        ("fulness", "ful"),
        ("ousness", "ous"),
        ("iveness", "ive"),
        ("tional", "tion"),
        ("biliti", "ble"),
        ("lessli", "less"),
        ("entli", "ent"),
        ("ation", "ate"),
        ("alism", "al"),
        ("aliti", "al"),
        ("ousli", "ous"),
        ("iviti", "ive"),
        ("fulli", "ful"),
        ("enci", "ence"),
        ("anci", "ance"),
        ("abli", "able"),
        ("izer", "ize"),
        ("ator", "ate"),
        ("alli", "al"),
        ("bli", "ble"),
        ("ogi", "og"),
        ("li", ""),
    ];
    const STEP3: &[(&str, &str)] = &[
        ("ational", "ate"),
        ("tional", "tion"),
        ("alize", "al"),
        ("icate", "ic"),
        ("iciti", "ic"),
        ("ative", ""),
        ("ical", "ic"),
        ("ness", ""),
        ("ful", ""),
    ];
    const STEP4: &[&str] = &[
        "ement", "ance", "ence", "able", "ible", "ment", "ant", "ent", "ism", "ate", "iti", "ous",
        "ive", "ize", "al", "er", "ic",
    ];
    if word.chars().count() <= 2 {
        return word.to_string();
    }
    if let Some((_, s)) = SPECIAL.iter().find(|(w, _)| *w == word) {
        return (*s).to_string();
    }
    let mut w: Vec<char> = word.trim_start_matches('\'').chars().collect();
    if w.is_empty() {
        return word.to_string();
    }
    // Mark consonant y.
    if w[0] == 'y' {
        w[0] = 'Y';
    }
    for i in 1..w.len() {
        if w[i] == 'y' && VOWELS.contains(&w[i - 1]) {
            w[i] = 'Y';
        }
    }
    let (r1, _) = regions(&w);

    // Step 0.
    for s in ["'s'", "'s", "'"] {
        if ends(&w, s) {
            cut(&mut w, s.chars().count());
            break;
        }
    }
    // Step 1a.
    if ends(&w, "sses") {
        cut(&mut w, 2);
    } else if ends(&w, "ied") || ends(&w, "ies") {
        if w.len() > 4 {
            cut(&mut w, 2);
        } else {
            cut(&mut w, 1);
        }
    } else if ends(&w, "us") || ends(&w, "ss") {
    } else if ends(&w, "s") && w.len() >= 2 && has_vowel(&w[..w.len() - 2]) {
        cut(&mut w, 1);
    }
    let lowered: String = w.iter().collect::<String>().replace('Y', "y");
    if STOP_1A.contains(&lowered.as_str()) {
        return lowered;
    }
    // Step 1b.
    if ends(&w, "eedly") || ends(&w, "eed") {
        let n = if ends(&w, "eedly") { 5 } else { 3 };
        if w.len() - n >= r1 {
            cut(&mut w, n);
            push(&mut w, "ee");
        }
    } else {
        for s in ["ingly", "edly", "ing", "ed"] {
            let n = s.len();
            if ends(&w, s) && has_vowel(&w[..w.len() - n]) {
                cut(&mut w, n);
                if ends(&w, "at") || ends(&w, "bl") || ends(&w, "iz") {
                    push(&mut w, "e");
                } else if ["bb", "dd", "ff", "gg", "mm", "nn", "pp", "rr", "tt"]
                    .iter()
                    .any(|d| ends(&w, d))
                {
                    cut(&mut w, 1);
                } else if r1 >= w.len() && ends_short_syllable(&w) {
                    push(&mut w, "e");
                }
                break;
            }
        }
    }
    // Step 1c.
    if w.len() > 2 && matches!(w[w.len() - 1], 'y' | 'Y') && !is_v(&w, w.len() - 2) {
        let l = w.len();
        w[l - 1] = 'i';
    }
    let (r1, r2) = regions(&w);
    // Step 2.
    for (s, r) in STEP2 {
        if ends(&w, s) {
            let n = s.len();
            if w.len() - n >= r1 {
                let before = &w[..w.len() - n];
                if *s == "ogi" && before.last() != Some(&'l') {
                    break;
                }
                if *s == "li" && !before.last().is_some_and(|c| "cdeghkmnrt".contains(*c)) {
                    break;
                }
                cut(&mut w, n);
                push(&mut w, r);
            }
            break;
        }
    }
    // Step 3.
    for (s, r) in STEP3 {
        if ends(&w, s) {
            let n = s.len();
            if w.len() - n >= r1 {
                if *s == "ative" {
                    if w.len() - n >= r2 {
                        cut(&mut w, n);
                    }
                } else {
                    cut(&mut w, n);
                    push(&mut w, r);
                }
            }
            break;
        }
    }
    // Step 4: the longest suffix, its condition applied once.
    if let Some(s) = STEP4.iter().find(|s| ends(&w, s)) {
        if w.len() - s.len() >= r2 {
            cut(&mut w, s.len());
        }
    } else if ends(&w, "ion")
        && w.len() > 3
        && w.len() - 3 >= r2
        && matches!(w[w.len() - 4], 's' | 't')
    {
        cut(&mut w, 3);
    }
    // Step 5.
    let (r1, r2) = regions(&w);
    if ends(&w, "e") {
        if w.len() > r2 || (w.len() > r1 && !ends_short_syllable(&w[..w.len() - 1])) {
            cut(&mut w, 1);
        }
    } else if ends(&w, "ll") && w.len() > r2 {
        cut(&mut w, 1);
    }
    w.iter().collect::<String>().replace('Y', "y")
}

// ------------------------------------------------------------------------
// The SQL-callable surface
// ------------------------------------------------------------------------

const FUNCTIONS: &[&str] = &[
    "to_tsvector",
    "to_tsquery",
    "plainto_tsquery",
    "phraseto_tsquery",
    "websearch_to_tsquery",
    "setweight",
    "strip",
    "tsvector_to_array",
    "array_to_tsvector",
    "tsvector_concat",
    "numnode",
    "querytree",
    "tsquery_and",
    "tsquery_or",
    "tsquery_not",
    "tsquery_phrase",
    "ts_headline",
    "ts_delete",
    "ts_filter",
    "tsvector_length",
    "get_current_ts_config",
    "ts_rank",
    "ts_rank_cd",
];

/// Is `name` one of the full-text functions?
pub fn is_function(name: &str) -> bool {
    FUNCTIONS.contains(&name)
}

/// A full-text function's result type.
pub fn result_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "to_tsvector" | "setweight" | "strip" | "array_to_tsvector" | "tsvector_concat"
        | "ts_delete" | "ts_filter" => "tsvector",
        "to_tsquery"
        | "plainto_tsquery"
        | "phraseto_tsquery"
        | "websearch_to_tsquery"
        | "tsquery_and"
        | "tsquery_or"
        | "tsquery_not"
        | "tsquery_phrase" => "tsquery",
        "tsvector_to_array" => "text[]",
        "numnode" | "tsvector_length" => "int4",
        "querytree" | "ts_headline" => "text",
        "get_current_ts_config" => "regconfig",
        "ts_rank" | "ts_rank_cd" => "float4",
        _ => return None,
    })
}

/// A value's text, reading the Python server's stored shapes too.
pub fn text_of(v: &Bson) -> String {
    match v {
        Bson::String(s) => s.clone(),
        Bson::Document(_) => python_text(v).unwrap_or_else(|| crate::value_text(v)),
        other => crate::value_text(other),
    }
}

/// A value the PYTHON server stored -- `{"tsvector": {lexeme: [pos, ...]}}`
/// or `{"tsquery": <node>}` -- as the canonical text this server carries.
/// Both servers share one on-disk format, so each must read the other's.
pub fn python_text(v: &Bson) -> Option<String> {
    let Bson::Document(d) = v else { return None };
    if let Ok(map) = d.get_document("tsvector") {
        let mut out = BTreeMap::new();
        for (lex, positions) in map {
            let ps = match positions {
                Bson::Array(a) => a
                    .iter()
                    .filter_map(|p| match p {
                        Bson::Int32(i) => u16::try_from(*i).ok(),
                        Bson::Int64(i) => u16::try_from(*i).ok(),
                        _ => None,
                    })
                    .map(|p| (p.min(MAX_POS), 0))
                    .collect(),
                _ => Vec::new(),
            };
            out.insert(lex.clone(), ps);
        }
        return Some(render_vector(&TsVector(out).normalise()));
    }
    if d.contains_key("tsquery") {
        fn node(b: &Bson) -> Option<Query> {
            let d = b.as_document()?;
            if let Ok(l) = d.get_str("lexeme") {
                return Some(Query::Val {
                    lex: l.to_string(),
                    prefix: false,
                    weight: 0,
                });
            }
            if let Ok(l) = d.get_str("prefix") {
                return Some(Query::Val {
                    lex: l.to_string(),
                    prefix: true,
                    weight: 0,
                });
            }
            if let Some(n) = d.get("not") {
                return Some(Query::Not(Box::new(node(n)?)));
            }
            if let Ok(ph) = d.get_document("phrase") {
                let dist = ph.get_i32("distance").unwrap_or(1).max(0) as u16;
                return Some(Query::Phrase(
                    Box::new(node(ph.get("left")?)?),
                    Box::new(node(ph.get("right")?)?),
                    dist,
                ));
            }
            for (key, is_and) in [("and", true), ("or", false)] {
                if let Ok(items) = d.get_array(key) {
                    let mut it = items.iter().filter_map(node);
                    let first = it.next()?;
                    return Some(it.fold(first, |a, b| {
                        if is_and {
                            Query::And(Box::new(a), Box::new(b))
                        } else {
                            Query::Or(Box::new(a), Box::new(b))
                        }
                    }));
                }
            }
            None
        }
        return Some(render_query(&d.get("tsquery").and_then(node)));
    }
    None
}

fn vector_arg(v: &Bson) -> Result<TsVector> {
    parse_vector(&text_of(v))
}

fn query_arg(v: &Bson) -> Result<Option<Query>> {
    parse_query(&text_of(v))
}

fn strings(v: &Bson) -> Result<Vec<String>> {
    match v {
        Bson::Array(items) => items
            .iter()
            .map(|i| match i {
                Bson::Null => Err(Error::Sqlstate(
                    "22004",
                    "lexeme array may not contain nulls".into(),
                )),
                other => Ok(text_of(other)),
            })
            .collect(),
        Bson::String(t) if t.starts_with('{') && t.ends_with('}') => Ok(t[1..t.len() - 1]
            .split(',')
            .map(|x| x.trim().trim_matches('"').to_string())
            .filter(|x| !x.is_empty())
            .collect()),
        other => Ok(vec![text_of(other)]),
    }
}

/// `(config, rest)`: the optional leading configuration argument.
fn split_config(args: &[Bson], plain: usize) -> Result<(Config, &[Bson])> {
    if args.len() > plain {
        Ok((config(&text_of(&args[0]))?, &args[1..]))
    } else {
        Ok((DEFAULT_CONFIG, args))
    }
}

fn wrong(name: &str) -> Error {
    Error::UndefinedFunction(format!("function {name} does not exist"))
}

fn vector_out(v: &TsVector) -> Bson {
    Bson::String(render_vector(v))
}

fn query_out(q: &Option<Query>) -> Bson {
    Bson::String(render_query(q))
}

/// Evaluate a full-text function. `None`: not one of these.
pub fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    if !is_function(name) {
        return None;
    }
    if args.contains(&Bson::Null) && name != "setweight" {
        return Some(Ok(Bson::Null));
    }
    Some(call_inner(name, args))
}

fn call_inner(name: &str, args: &[Bson]) -> Result<Bson> {
    match name {
        "to_tsvector" => {
            let (cfg, rest) = split_config(args, 1)?;
            let [doc] = rest else { return Err(wrong(name)) };
            Ok(vector_out(&to_tsvector(cfg, &text_of(doc))))
        }
        "to_tsquery" | "plainto_tsquery" | "phraseto_tsquery" | "websearch_to_tsquery" => {
            let (cfg, rest) = split_config(args, 1)?;
            let [text] = rest else {
                return Err(wrong(name));
            };
            let text = text_of(text);
            let q = match name {
                "to_tsquery" => to_tsquery(cfg, &text)?,
                "plainto_tsquery" => plainto_tsquery(cfg, &text),
                "phraseto_tsquery" => phraseto_tsquery(cfg, &text),
                _ => websearch_to_tsquery(cfg, &text),
            };
            Ok(query_out(&q))
        }
        "setweight" => {
            if args.first() == Some(&Bson::Null) || args.get(1) == Some(&Bson::Null) {
                return Ok(Bson::Null);
            }
            match args {
                [v, w] => Ok(vector_out(&setweight(&vector_arg(v)?, &text_of(w), None)?)),
                [v, w, only] => {
                    let only = if *only == Bson::Null {
                        Vec::new()
                    } else {
                        strings(only)?
                    };
                    Ok(vector_out(&setweight(
                        &vector_arg(v)?,
                        &text_of(w),
                        Some(&only),
                    )?))
                }
                _ => Err(wrong(name)),
            }
        }
        "strip" => Ok(vector_out(&strip(&vector_arg(&args[0])?))),
        "tsvector_length" => Ok(Bson::Int32(vector_arg(&args[0])?.0.len() as i32)),
        "tsvector_to_array" => Ok(Bson::Array(
            vector_arg(&args[0])?
                .0
                .keys()
                .map(|k| Bson::String(k.clone()))
                .collect(),
        )),
        "array_to_tsvector" => {
            let mut map = BTreeMap::new();
            for s in strings(&args[0])? {
                if s.is_empty() {
                    return Err(Error::Sqlstate(
                        "2200F",
                        "lexeme array may not contain empty strings".into(),
                    ));
                }
                map.insert(s, Vec::new());
            }
            Ok(vector_out(&TsVector(map)))
        }
        "tsvector_concat" => {
            let [a, b] = args else {
                return Err(wrong(name));
            };
            Ok(vector_out(&concat(&vector_arg(a)?, &vector_arg(b)?)))
        }
        "ts_delete" => {
            let [v, lex] = args else {
                return Err(wrong(name));
            };
            let mut v = vector_arg(v)?;
            for l in strings(lex)? {
                v.0.remove(&l);
            }
            Ok(vector_out(&v))
        }
        "ts_filter" => {
            let [v, ws] = args else {
                return Err(wrong(name));
            };
            let mut keep = Vec::new();
            for w in strings(ws)? {
                keep.push(match w.to_ascii_uppercase().as_str() {
                    "A" => 3,
                    "B" => 2,
                    "C" => 1,
                    "D" => 0,
                    _ => {
                        return Err(Error::Sqlstate(
                            "XX000",
                            format!("unrecognized weight: \"{w}\""),
                        ))
                    }
                });
            }
            let mut v = vector_arg(v)?;
            v.0.retain(|_, positions| {
                positions.retain(|(_, w)| keep.contains(w));
                !positions.is_empty()
            });
            Ok(vector_out(&v))
        }
        "numnode" => Ok(Bson::Int32(numnode(&query_arg(&args[0])?))),
        "querytree" => Ok(Bson::String(querytree(&query_arg(&args[0])?))),
        "tsquery_and" | "tsquery_or" => {
            let [a, b] = args else {
                return Err(wrong(name));
            };
            let op = if name == "tsquery_and" { "&&" } else { "||" };
            Ok(query_out(&combine(op, &query_arg(a)?, &query_arg(b)?)))
        }
        "tsquery_not" => Ok(query_out(&negate(&query_arg(&args[0])?))),
        "tsquery_phrase" => {
            let (a, b) = (query_arg(&args[0])?, query_arg(&args[1])?);
            let d = match args.get(2) {
                Some(d) => crate::value_text(d).trim().parse::<u16>().unwrap_or(1),
                None => 1,
            };
            Ok(query_out(&match (a, b) {
                (None, x) | (x, None) => x,
                (Some(a), Some(b)) => Some(Query::Phrase(Box::new(a), Box::new(b), d)),
            }))
        }
        "ts_headline" => {
            // ([config,] document, query [, options])
            let (cfg, rest) = match args.len() {
                4 => (config(&text_of(&args[0]))?, &args[1..3]),
                3 if parse_query(&text_of(&args[2])).is_ok()
                    && config(&text_of(&args[0])).is_ok() =>
                {
                    (config(&text_of(&args[0]))?, &args[1..3])
                }
                _ => (DEFAULT_CONFIG, &args[..2]),
            };
            let [doc, q] = rest else {
                return Err(wrong(name));
            };
            Ok(Bson::String(headline(cfg, &text_of(doc), &query_arg(q)?)))
        }
        "get_current_ts_config" => Ok(Bson::String("english".into())),
        "ts_rank" => rank_call(args, false),
        "ts_rank_cd" => rank_call(args, true),
        _ => Err(wrong(name)),
    }
}

/// `tsvector @@ tsquery` over the values' text.
pub fn match_values(v: &Bson, q: &Bson) -> Result<bool> {
    Ok(matches(&vector_arg(v)?, &query_arg(q)?))
}

/// The binary operators on the two types: `||` of vectors, `&&` / `||` of
/// queries, `<->` of queries.
pub fn operator(op: &str, left_type: &str, a: &Bson, b: &Bson) -> Result<Bson> {
    match (op, left_type) {
        ("||", "tsvector") => Ok(vector_out(&concat(&vector_arg(a)?, &vector_arg(b)?))),
        ("&&" | "||" | "<->", "tsquery") => {
            Ok(query_out(&combine(op, &query_arg(a)?, &query_arg(b)?)))
        }
        _ => Err(Error::UndefinedFunction(format!(
            "operator does not exist: {left_type} {op} {left_type}"
        ))),
    }
}

/// `!! tsquery`.
pub fn not_value(q: &Bson) -> Result<Bson> {
    Ok(query_out(&negate(&query_arg(q)?)))
}

// ------------------------------------------------------------------------
// Ranking: `ts_rank` (tsrank.c's calc_rank, transcribed, in its float4
// arithmetic so the last digits agree)
// ------------------------------------------------------------------------

const DEFAULT_WEIGHTS: [f32; 4] = [0.1, 0.2, 0.4, 1.0];

/// The distinct operands of a query, in PostgreSQL's order (by lexeme
/// bytes; one per lexeme).
fn operands(q: &Query) -> Vec<(String, bool, u8)> {
    fn walk(q: &Query, out: &mut Vec<(String, bool, u8)>) {
        match q {
            Query::Val {
                lex,
                prefix,
                weight,
            } => out.push((lex.clone(), *prefix, *weight)),
            Query::Stop => {}
            Query::Not(i) => walk(i, out),
            Query::And(l, r) | Query::Or(l, r) | Query::Phrase(l, r, _) => {
                walk(l, out);
                walk(r, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(q, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// The vector entries an operand finds: one exact, or every one a prefix
/// covers.
fn found<'a>(v: &'a TsVector, lex: &str, prefix: bool) -> Vec<&'a Vec<(u16, Weight)>> {
    if prefix {
        v.0.range(lex.to_string()..)
            .take_while(|(k, _)| k.starts_with(lex))
            .map(|(_, p)| p)
            .collect()
    } else {
        v.0.get(lex).into_iter().collect()
    }
}

/// An entry's positions, or PostgreSQL's `POSNULL` (one position 0,
/// weight D) for an entry stripped of them.
fn positions_or_null(p: &[(u16, Weight)]) -> Vec<(u16, Weight)> {
    if p.is_empty() {
        vec![(0, 0)]
    } else {
        p.to_vec()
    }
}

fn word_distance(w: i32) -> f32 {
    if w > 100 {
        return 1e-30;
    }
    // `1.0 / (1.005 + 0.05 * exp(((float4) w) / 1.5 - 2))`: the literals are
    // doubles, so everything after the cast runs in double.
    (1.0f64 / (1.005f64 + 0.05f64 * (f64::from(w as f32) / 1.5 - 2.0).exp())) as f32
}

fn rank_or(w: &[f32; 4], v: &TsVector, q: &Query) -> f32 {
    let items = operands(q);
    let mut res: f32 = 0.0;
    for (lex, prefix, _) in &items {
        for entry in found(v, lex, *prefix) {
            let post = positions_or_null(entry);
            let mut resj: f32 = 0.0;
            let mut wjm: f32 = -1.0;
            let mut jm: i32 = 0;
            for (j, (_, wt)) in post.iter().enumerate() {
                let wp = w[*wt as usize];
                let j = j as i32;
                resj += wp / ((j + 1) * (j + 1)) as f32;
                if wp > wjm {
                    wjm = wp;
                    jm = j;
                }
            }
            let term =
                f64::from(wjm + resj - wjm / ((jm + 1) * (jm + 1)) as f32) / 1.64493406685f64;
            res = (f64::from(res) + term) as f32;
        }
    }
    if !items.is_empty() {
        res /= items.len() as f32;
    }
    res
}

#[allow(clippy::type_complexity, clippy::needless_range_loop)]
fn rank_and(w: &[f32; 4], v: &TsVector, q: &Query) -> f32 {
    let items = operands(q);
    if items.len() < 2 {
        return rank_or(w, v, q);
    }
    let mut pos: Vec<Option<(Vec<(u16, Weight)>, bool)>> = vec![None; items.len()];
    let mut res: f32 = -1.0;
    for i in 0..items.len() {
        let (lex, prefix, _) = &items[i];
        for entry in found(v, lex, *prefix) {
            let is_null = entry.is_empty();
            pos[i] = Some((positions_or_null(entry), is_null));
            let (post, inull) = pos[i].clone().expect("just set");
            for k in 0..i {
                let Some((ct, knull)) = &pos[k] else { continue };
                for l in &post {
                    for p in ct {
                        let mut dist = (i32::from(l.0) - i32::from(p.0)).abs();
                        if dist != 0 || inull || *knull {
                            if dist == 0 {
                                dist = 16384;
                            }
                            // float4 product, double sqrt, as in C.
                            let prod: f32 = w[l.1 as usize] * w[p.1 as usize] * word_distance(dist);
                            let curw = f64::from(prod).sqrt() as f32;
                            res = if res < 0.0 {
                                curw
                            } else {
                                (1.0f64 - (1.0f64 - f64::from(res)) * (1.0f64 - f64::from(curw)))
                                    as f32
                            };
                        }
                    }
                }
            }
        }
    }
    res
}

/// `ts_rank([weights,] vector, query [, normalization])`.
pub fn rank(weights: Option<[f32; 4]>, v: &TsVector, q: &Option<Query>, method: i32) -> f32 {
    let w = weights.unwrap_or(DEFAULT_WEIGHTS);
    let Some(q) = q else { return 0.0 };
    if v.0.is_empty() {
        return 0.0;
    }
    let mut res = match q {
        Query::And(..) | Query::Phrase(..) => rank_and(&w, v, q),
        _ => rank_or(&w, v, q),
    };
    if res < 0.0 {
        res = 1e-20;
    }
    let len: usize = v.0.values().map(|p| p.len().max(1)).sum();
    let uniq = v.0.len();
    if method & 1 != 0 {
        res = (f64::from(res) / ((len as f64 + 1.0).ln() / 2f64.ln())) as f32;
    }
    if method & 2 != 0 && len > 0 {
        res /= len as f32;
    }
    if method & 8 != 0 {
        res /= uniq as f32;
    }
    if method & 16 != 0 {
        res = (f64::from(res) / ((uniq as f64 + 1.0).ln() / 2f64.ln())) as f32;
    }
    if method & 32 != 0 {
        res /= res + 1.0;
    }
    res
}

/// A `float4[]` of four rank weights, each in `[0, 1]`.
fn weights_arg(v: &Bson) -> Result<[f32; 4]> {
    let items: Vec<f64> = match v {
        Bson::Array(a) => a
            .iter()
            .map(|x| match x {
                Bson::Double(d) => Ok(*d),
                Bson::Int32(i) => Ok(f64::from(*i)),
                Bson::Int64(i) => Ok(*i as f64),
                Bson::Null => Err(Error::Sqlstate(
                    "22004",
                    "array of weight must not contain nulls".into(),
                )),
                other => crate::value_text(other)
                    .parse::<f64>()
                    .map_err(|_| Error::Sqlstate("22P02", "invalid weight".into())),
            })
            .collect::<Result<_>>()?,
        Bson::String(t) => t
            .trim_matches(['{', '}'])
            .split(',')
            .map(|x| {
                x.trim()
                    .parse::<f64>()
                    .map_err(|_| Error::Sqlstate("22P02", "invalid weight".into()))
            })
            .collect::<Result<_>>()?,
        _ => {
            return Err(Error::Sqlstate(
                "22023",
                "array of weight is too short".into(),
            ))
        }
    };
    if items.len() < 4 {
        return Err(Error::Sqlstate(
            "22023",
            "array of weight is too short".into(),
        ));
    }
    let mut w = [0f32; 4];
    for (i, x) in items.iter().take(4).enumerate() {
        if !(0.0..=1.0).contains(x) {
            return Err(Error::Sqlstate("22023", "weight out of range".into()));
        }
        w[i] = *x as f32;
    }
    Ok(w)
}

/// `ts_rank` over SQL values.
pub fn rank_call(args: &[Bson], cover_density: bool) -> Result<Bson> {
    let (weights, rest) = match args.len() {
        4 => (Some(weights_arg(&args[0])?), &args[1..]),
        3 if matches!(args[0], Bson::Array(_)) || crate::value_text(&args[0]).starts_with('{') => {
            (Some(weights_arg(&args[0])?), &args[1..])
        }
        _ => (None, args),
    };
    let v = vector_arg(&rest[0])?;
    let q = query_arg(&rest[1])?;
    let method = match rest.get(2) {
        Some(m) => crate::value_text(m).trim().parse::<i32>().unwrap_or(0),
        None => 0,
    };
    // A float4 result: the shortest decimal that round-trips the f32, as
    // float4out prints it.
    let r = if cover_density {
        rank_cd(weights, &v, &q, method)
    } else {
        rank(weights, &v, &q, method)
    };
    Ok(Bson::Double(
        r.to_string().parse::<f64>().unwrap_or(f64::from(r)),
    ))
}

/// One document position the query touches (`DocRepresentation`).
struct DocPos {
    pos: u16,
    weight: Weight,
    lexemes: Vec<String>,
}

/// `get_docrep`: every position of every entry a query operand finds, in
/// position order, merged where two entries share a position.
fn docrep(v: &TsVector, q: &Query) -> Vec<DocPos> {
    let mut out: Vec<DocPos> = Vec::new();
    for (lex, prefix, _) in operands(q) {
        let keys: Vec<&String> = if prefix {
            v.0.range(lex.clone()..)
                .take_while(|(k, _)| k.starts_with(lex.as_str()))
                .map(|(k, _)| k)
                .collect()
        } else {
            v.0.get_key_value(&lex)
                .map(|(k, _)| k)
                .into_iter()
                .collect()
        };
        for k in keys {
            // An entry stripped of positions has nothing to cover.
            for &(p, w) in &v.0[k] {
                out.push(DocPos {
                    pos: p,
                    weight: w,
                    lexemes: vec![k.clone()],
                });
            }
        }
    }
    out.sort_by_key(|d| d.pos);
    let mut merged: Vec<DocPos> = Vec::with_capacity(out.len());
    for d in out {
        match merged.last_mut() {
            Some(last) if last.pos == d.pos => {
                for l in d.lexemes {
                    if !last.lexemes.contains(&l) {
                        last.lexemes.push(l);
                    }
                }
            }
            _ => merged.push(d),
        }
    }
    merged
}

/// Does the query hold over the positions `doc[from..=to]` alone?
fn window_holds(doc: &[DocPos], from: usize, to: usize, q: &Query) -> bool {
    let mut map: BTreeMap<String, Vec<(u16, Weight)>> = BTreeMap::new();
    for d in &doc[from..=to] {
        for l in &d.lexemes {
            let e = map.entry(l.clone()).or_default();
            if d.pos > 0 {
                e.push((d.pos, d.weight));
            }
        }
    }
    eval(&TsVector(map).normalise(), q)
}

/// `Cover`: the next minimal extent `(begin, end, p, q)` from `start`.
fn cover(doc: &[DocPos], start: &mut usize, q: &Query) -> Option<(usize, usize, i32, i32)> {
    loop {
        if *start >= doc.len() {
            return None;
        }
        // Upper bound: the first position from `start` at which the query
        // holds over [start, here].
        let mut end = None;
        for i in *start..doc.len() {
            if window_holds(doc, *start, i, q) {
                end = Some(i);
                break;
            }
        }
        let end = end?;
        // Lower bound: moving down from `end`, the first at which it holds.
        let mut begin = None;
        let mut i = end as isize;
        while i >= *start as isize {
            if window_holds(doc, i as usize, end, q) {
                begin = Some(i as usize);
                break;
            }
            i -= 1;
        }
        let begin = begin?;
        let (p, qpos) = (i32::from(doc[begin].pos), i32::from(doc[end].pos));
        if p <= qpos {
            *start = begin + 1;
            return Some((begin, end, p, qpos));
        }
        *start += 1;
    }
}

/// `ts_rank_cd`'s cover density (`calc_rank_cd`).
pub fn rank_cd(weights: Option<[f32; 4]>, v: &TsVector, q: &Option<Query>, method: i32) -> f32 {
    let w = weights.unwrap_or(DEFAULT_WEIGHTS);
    let invws: Vec<f64> = w.iter().map(|x| 1.0 / f64::from(*x)).collect();
    let Some(q) = q else { return 0.0 };
    let doc = docrep(v, q);
    if doc.is_empty() {
        return 0.0;
    }
    let mut wdoc = 0.0f64;
    let mut sum_dist = 0.0f64;
    let mut prev_ext = 0.0f64;
    let mut n_extent = 0;
    let mut start = 0usize;
    while let Some((begin, end, p, qpos)) = cover(&doc, &mut start, q) {
        let inv_sum: f64 = doc[begin..=end]
            .iter()
            .map(|d| invws[d.weight as usize])
            .sum();
        let cpos = (end - begin + 1) as f64 / inv_sum;
        let mut noise = (qpos - p) - (end - begin) as i32;
        if noise < 0 {
            noise = (end - begin) as i32 / 2;
        }
        wdoc += cpos / f64::from(1 + noise);
        let cur = f64::from(qpos + p) / 2.0;
        if n_extent > 0 && cur > prev_ext {
            sum_dist += 1.0 / (cur - prev_ext);
        }
        prev_ext = cur;
        n_extent += 1;
    }
    let len: usize = v.0.values().map(|p| p.len().max(1)).sum();
    let uniq = v.0.len();
    if method & 1 != 0 && uniq > 0 {
        wdoc /= (len as f64 + 1.0).ln();
    }
    if method & 2 != 0 && len > 0 {
        wdoc /= len as f64;
    }
    if method & 4 != 0 && n_extent > 0 && sum_dist > 0.0 {
        wdoc /= f64::from(n_extent) / sum_dist;
    }
    if method & 8 != 0 && uniq > 0 {
        wdoc /= uniq as f64;
    }
    if method & 16 != 0 && uniq > 0 {
        wdoc /= (uniq as f64 + 1.0).ln() / 2f64.ln();
    }
    if method & 32 != 0 {
        wdoc /= wdoc + 1.0;
    }
    wdoc as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every word of the corpus PostgreSQL 14.13's `ts_lexize('english_stem')`
    /// stemmed, matched exactly.
    #[test]
    fn stems_match_postgresql() {
        let data = include_str!("../../../tests/data/english_stems.txt");
        let mut wrong = Vec::new();
        for line in data.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (Some(word), Some(want)) = (parts.next(), parts.next()) else {
                continue;
            };
            if stem(word) != want {
                wrong.push(format!("{word}: {} != {want}", stem(word)));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} wrong: {:?}",
            wrong.len(),
            &wrong[..wrong.len().min(20)]
        );
    }

    #[test]
    fn renders_like_postgresql() {
        let q = to_tsquery(Config::English, "fox <-> the <-> quick").unwrap();
        assert_eq!(render_query(&q), "'fox' <2> 'quick'");
        let q = to_tsquery(Config::English, "(fox | quick) & brown").unwrap();
        assert_eq!(render_query(&q), "( 'fox' | 'quick' ) & 'brown'");
        let v = to_tsvector(Config::English, "foo-bar abc123 42 -7 3.14 don't café 2x");
        assert_eq!(
            render_vector(&v),
            "'-7':6 '2x':11 '3.14':7 '42':5 'abc123':4 'bar':3 'café':10 'foo':2 'foo-bar':1"
        );
    }
}
