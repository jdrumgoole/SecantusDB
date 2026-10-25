//! The `ispell` text search template: PostgreSQL 15's `spell.c` (with
//! `regis.c`), transcribed. A dictionary file of words and their affix
//! flags, and an affix file in ispell's own format or in MySpell/Hunspell's
//! (`PFX` / `SFX`, `FLAG long` / `num`, `AF` aliases, the `COMPOUND*`
//! options), give a word's normal forms -- and, when the affix file allows
//! compounds, the parts a compound word splits into.
//!
//! Words are bytes, as `spell.c` keeps them: both tries branch on a byte,
//! and a suffix is matched from the word's last byte backwards.

use std::cmp::Ordering;

use crate::pg_qsort::pg_qsort;
use crate::{Error, Result};

const FF_COMPOUNDONLY: u32 = 0x01;
const FF_COMPOUNDBEGIN: u32 = 0x02;
const FF_COMPOUNDMIDDLE: u32 = 0x04;
const FF_COMPOUNDLAST: u32 = 0x08;
const FF_COMPOUNDFLAG: u32 = FF_COMPOUNDBEGIN | FF_COMPOUNDMIDDLE | FF_COMPOUNDLAST;
const FF_COMPOUNDFLAGMASK: u32 = 0x0f;
const FF_COMPOUNDPERMITFLAG: u32 = 0x10;
const FF_COMPOUNDFORBIDFLAG: u32 = 0x20;
const FF_CROSSPRODUCT: u32 = 0x40;

const MAX_NORM: usize = 1024;
const MAXNORMLEN: usize = 256;
/// `FLAGNUM_MAXSIZE`.
const FLAGNUM_MAXSIZE: i64 = 1 << 16;

/// `ERRCODE_CONFIG_FILE_ERROR`.
fn config_error(msg: impl Into<String>) -> Error {
    Error::Sqlstate("F0000", msg.into())
}

// ------------------------------------------------------------------------
// The byte-level helpers `ts_locale.c` gives `spell.c`.
// ------------------------------------------------------------------------

/// `pg_mblen` in UTF-8.
fn mblen(s: &[u8], i: usize) -> usize {
    let n = match s[i] {
        b if b < 0x80 => 1,
        b if b & 0xe0 == 0xc0 => 2,
        b if b & 0xf0 == 0xe0 => 3,
        b if b & 0xf8 == 0xf0 => 4,
        _ => 1,
    };
    n.min(s.len() - i)
}

fn char_at(s: &[u8], i: usize) -> char {
    std::str::from_utf8(&s[i..i + mblen(s, i)])
        .ok()
        .and_then(|c| c.chars().next())
        .unwrap_or('\u{fffd}')
}

/// `t_isspace`: C's `isspace` for a single byte, `iswspace` otherwise.
fn is_space(s: &[u8], i: usize) -> bool {
    if mblen(s, i) == 1 {
        matches!(s[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
    } else {
        char_at(s, i).is_whitespace()
    }
}

fn is_alpha_char(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphabetic()
    } else {
        c.is_alphabetic()
    }
}

/// `t_isalpha`.
fn is_alpha(s: &[u8], i: usize) -> bool {
    is_alpha_char(char_at(s, i))
}

/// `lowerstr`.
fn lower(s: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(s)
        .chars()
        .map(crate::scalar::simple_lower)
        .collect::<String>()
        .into_bytes()
}

fn text(s: &[u8]) -> String {
    String::from_utf8_lossy(s).into_owned()
}

/// A file's lines, each with its newline, as `tsearch_readline` returns
/// them.
fn lines(file: &str) -> impl Iterator<Item = &[u8]> {
    file.as_bytes().split_inclusive(|b| *b == b'\n')
}

/// C's `strtol(s, &end, 10)`: `(value, bytes read, out of range)`, the
/// bytes read 0 when there is no number.
fn strtol(s: &[u8]) -> (i64, usize, bool) {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let neg = match s.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let digits = i;
    let mut v: i64 = 0;
    let mut overflow = false;
    while i < s.len() && s[i].is_ascii_digit() {
        let d = i64::from(s[i] - b'0');
        match v.checked_mul(10).and_then(|x| {
            if neg {
                x.checked_sub(d)
            } else {
                x.checked_add(d)
            }
        }) {
            Some(x) => v = x,
            None => overflow = true,
        }
        i += 1;
    }
    if i == digits {
        return (0, 0, false);
    }
    (v, i, overflow)
}

/// C's `atoi`.
fn atoi(s: &str) -> i64 {
    strtol(s.as_bytes()).0
}

/// glibc's `bsearch`: the first element it lands on that compares equal.
fn bsearch<T>(items: &[T], cmp: impl Fn(&T) -> Ordering) -> Option<&T> {
    let (mut l, mut u) = (0, items.len());
    while l < u {
        let idx = (l + u) / 2;
        match cmp(&items[idx]) {
            Ordering::Greater => u = idx,
            Ordering::Less => l = idx + 1,
            Ordering::Equal => return Some(&items[idx]),
        }
    }
    None
}

// ------------------------------------------------------------------------
// regis.c: the simple affix conditions, a run of letters and [...] sets.
// ------------------------------------------------------------------------

#[derive(Debug)]
struct Regis {
    issuffix: bool,
    /// `(one of?, characters)`: `[^...]` is not one of.
    nodes: Vec<(bool, Vec<char>)>,
}

/// `RS_isRegis`.
fn is_regis(mask: &str) -> bool {
    #[derive(PartialEq)]
    enum S {
        Wait,
        OneOf,
        In,
    }
    let mut state = S::Wait;
    for c in mask.chars() {
        state = match state {
            S::Wait if is_alpha_char(c) => S::Wait,
            S::Wait if c == '[' => S::OneOf,
            S::OneOf if c == '^' || is_alpha_char(c) => S::In,
            S::In if is_alpha_char(c) => S::In,
            S::In if c == ']' => S::Wait,
            _ => return false,
        };
    }
    state == S::Wait
}

impl Regis {
    /// `RS_compile`, of a mask `is_regis` accepted.
    fn compile(issuffix: bool, mask: &str) -> Regis {
        let mut nodes: Vec<(bool, Vec<char>)> = Vec::new();
        let mut in_set = false;
        let mut set_start = false;
        for c in mask.chars() {
            if !in_set {
                if c == '[' {
                    nodes.push((true, Vec::new()));
                    in_set = true;
                    set_start = true;
                } else {
                    nodes.push((true, vec![c]));
                }
            } else if set_start && c == '^' {
                if let Some(n) = nodes.last_mut() {
                    n.0 = false;
                }
                set_start = false;
            } else if c == ']' {
                in_set = false;
            } else {
                set_start = false;
                if let Some(n) = nodes.last_mut() {
                    n.1.push(c);
                }
            }
        }
        Regis { issuffix, nodes }
    }

    /// `RS_execute`.
    fn execute(&self, word: &[u8]) -> bool {
        let chars: Vec<char> = String::from_utf8_lossy(word).chars().collect();
        if chars.len() < self.nodes.len() {
            return false;
        }
        let start = if self.issuffix {
            chars.len() - self.nodes.len()
        } else {
            0
        };
        self.nodes
            .iter()
            .zip(&chars[start..])
            .all(|((oneof, set), c)| set.contains(c) == *oneof)
    }
}

// ------------------------------------------------------------------------
// The dictionary.
// ------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AffType {
    /// `FF_PREFIX` (0): prefixes sort first.
    Prefix,
    /// `FF_SUFFIX` (1).
    Suffix,
}

#[derive(Debug)]
enum Cond {
    Simple,
    Regis(Regis),
    Regex(regex::Regex),
}

#[derive(Debug)]
struct Affix {
    flag: String,
    typ: AffType,
    flagflags: u32,
    cond: Cond,
    find: Vec<u8>,
    repl: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum FlagMode {
    Char,
    Long,
    Num,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum FlagKey {
    Str(String),
    Num(u32),
}

#[derive(Debug)]
struct CompoundFlag {
    flag: FlagKey,
    value: u32,
}

#[derive(Debug, Default)]
struct SpNodeData {
    val: u8,
    isword: bool,
    compoundflag: u32,
    affix: usize,
    node: Option<Box<SpNode>>,
}

#[derive(Debug)]
struct SpNode {
    data: Vec<SpNodeData>,
}

#[derive(Debug, Default)]
struct AffixNodeData {
    val: u8,
    aff: Vec<usize>,
    node: Option<Box<AffixNode>>,
}

#[derive(Debug)]
struct AffixNode {
    isvoid: bool,
    data: Vec<AffixNodeData>,
}

#[derive(Debug)]
struct CmpdAffix {
    affix: Vec<u8>,
    issuffix: bool,
}

/// A word of the dictionary file, while the dictionary is built.
struct Spell {
    word: Vec<u8>,
    flag: String,
    affix: usize,
}

/// `IspellDict`, built.
#[derive(Debug)]
pub struct IspellDict {
    affixes: Vec<Affix>,
    suffix: Option<Box<AffixNode>>,
    prefix: Option<Box<AffixNode>>,
    dictionary: Option<Box<SpNode>>,
    affix_data: Vec<String>,
    use_flag_aliases: bool,
    compound_affix: Option<Vec<CmpdAffix>>,
    usecompound: bool,
    flag_mode: FlagMode,
    compound_flags: Vec<CompoundFlag>,
}

/// An `IspellDict` between `NIStartBuild` and `NISortDictionary`.
pub struct IspellBuilder {
    dict: IspellDict,
    spells: Vec<Spell>,
    /// Interim compound flags, their spelling not yet read for the mode.
    interim_flags: Vec<(String, u32)>,
    /// How many `AF` aliases have been read (`AffixData[i]` is NULL past
    /// it).
    aliases_read: usize,
}

impl Default for IspellBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl IspellBuilder {
    /// `NIStartBuild`.
    pub fn new() -> Self {
        IspellBuilder {
            dict: IspellDict {
                affixes: Vec::new(),
                suffix: None,
                prefix: None,
                dictionary: None,
                affix_data: Vec::new(),
                use_flag_aliases: false,
                compound_affix: None,
                usecompound: false,
                flag_mode: FlagMode::Char,
                compound_flags: Vec::new(),
            },
            spells: Vec::new(),
            interim_flags: Vec::new(),
            aliases_read: 0,
        }
    }

    /// `NIImportDictionary`.
    pub fn import_dictionary(&mut self, file: &str) {
        for line in lines(file) {
            let mut flag = String::new();
            let mut i = 0;
            let mut slash = None;
            while i < line.len() {
                if line[i] == b'/' {
                    slash = Some(i);
                    break;
                }
                i += mblen(line, i);
            }
            let line = match slash {
                Some(s) => {
                    // Only single-byte printable flags, for speed.
                    let mut j = s + 1;
                    while j < line.len() && mblen(line, j) == 1 && (0x21..=0x7e).contains(&line[j])
                    {
                        j += 1;
                    }
                    flag = text(&line[s + 1..j]);
                    &line[..s]
                }
                None => line,
            };
            let mut k = 0;
            while k < line.len() && !is_space(line, k) {
                k += mblen(line, k);
            }
            self.spells.push(Spell {
                word: lower(&line[..k]),
                flag,
                affix: 0,
            });
        }
    }

    /// `addCompoundAffixFlagValue`.
    fn add_compound_flag(&mut self, s: &[u8], val: u32) -> Result<()> {
        let mut i = 0;
        while i < s.len() && is_space(s, i) {
            i += mblen(s, i);
        }
        if i >= s.len() {
            return Err(config_error("syntax error"));
        }
        let start = i;
        while i < s.len() && !is_space(s, i) && s[i] != b'\n' {
            i += mblen(s, i);
        }
        self.interim_flags.push((text(&s[start..i]), val));
        self.dict.usecompound = true;
        Ok(())
    }

    /// `finalizeCompoundAffixFlags`.
    fn finalize_compound_flags(&mut self) -> Result<()> {
        for (s, value) in std::mem::take(&mut self.interim_flags) {
            let flag = if self.dict.flag_mode == FlagMode::Num {
                FlagKey::Num(numeric_flag(&s)?)
            } else {
                FlagKey::Str(s)
            };
            self.dict.compound_flags.push(CompoundFlag { flag, value });
        }
        Ok(())
    }

    /// `NIImportAffixes`: an ispell-format file, or, at its first
    /// new-format command, `NIImportOOAffixes` over the whole file.
    pub fn import_affixes(&mut self, file: &str) -> Result<()> {
        let mut flag = String::new();
        let mut suffixes = false;
        let mut prefixes = false;
        let mut flagflags = 0u32;
        let mut oldformat = false;
        self.dict.usecompound = false;
        self.dict.use_flag_aliases = false;
        self.dict.flag_mode = FlagMode::Char;
        for recoded in lines(file) {
            let pstr = lower(recoded);
            if matches!(pstr.first(), Some(b'#') | Some(b'\n')) {
                continue;
            }
            if pstr.starts_with(b"compoundwords") {
                // The case-insensitive L of "controlled", in the line as
                // written.
                let mut s = 0;
                let mut found = false;
                while s < recoded.len() {
                    if recoded[s] == b'l' || recoded[s] == b'L' {
                        found = true;
                        break;
                    }
                    s += mblen(recoded, s);
                }
                if found {
                    while s < recoded.len() && !is_space(recoded, s) {
                        s += mblen(recoded, s);
                    }
                    while s < recoded.len() && is_space(recoded, s) {
                        s += mblen(recoded, s);
                    }
                    if s < recoded.len() && mblen(recoded, s) == 1 {
                        self.add_compound_flag(&recoded[s..], FF_COMPOUNDFLAG)?;
                        self.dict.usecompound = true;
                    }
                    oldformat = true;
                    continue;
                }
            }
            if pstr.starts_with(b"suffixes") {
                suffixes = true;
                prefixes = false;
                oldformat = true;
                continue;
            }
            if pstr.starts_with(b"prefixes") {
                suffixes = false;
                prefixes = true;
                oldformat = true;
                continue;
            }
            if pstr.starts_with(b"flag") {
                let mut s = 4;
                flagflags = 0;
                while s < recoded.len() && is_space(recoded, s) {
                    s += mblen(recoded, s);
                }
                match recoded.get(s) {
                    Some(b'*') => {
                        flagflags |= FF_CROSSPRODUCT;
                        s += 1;
                    }
                    Some(b'~') => {
                        flagflags |= FF_COMPOUNDONLY;
                        s += 1;
                    }
                    _ => {}
                }
                if recoded.get(s) == Some(&b'\\') {
                    s += 1;
                }
                // An old-format flag is one ASCII character, then the end,
                // a space or ':'; anything else is a new-format command.
                if s < recoded.len() && mblen(recoded, s) == 1 {
                    flag = char::from(recoded[s]).to_string();
                    s += 1;
                    if s >= recoded.len()
                        || matches!(recoded[s], b'#' | b'\n' | b':')
                        || is_space(recoded, s)
                    {
                        oldformat = true;
                        continue;
                    }
                }
                return self.new_format(oldformat, file);
            }
            if recoded.starts_with(b"COMPOUNDFLAG")
                || recoded.starts_with(b"COMPOUNDMIN")
                || recoded.starts_with(b"PFX")
                || recoded.starts_with(b"SFX")
            {
                return self.new_format(oldformat, file);
            }
            if !suffixes && !prefixes {
                continue;
            }
            let Some((mask, find, repl)) = parse_affentry(&pstr)? else {
                continue;
            };
            let typ = if suffixes {
                AffType::Suffix
            } else {
                AffType::Prefix
            };
            self.add_affix(&flag, flagflags, &mask, &find, &repl, typ)?;
        }
        // The old format has no FLAG command: the mode is still FM_CHAR.
        self.finalize_compound_flags()
    }

    fn new_format(&mut self, oldformat: bool, file: &str) -> Result<()> {
        if oldformat {
            return Err(config_error(
                "affix file contains both old-style and new-style commands",
            ));
        }
        self.import_oo_affixes(file)
    }

    /// `NIImportOOAffixes`.
    fn import_oo_affixes(&mut self, file: &str) -> Result<()> {
        self.dict.usecompound = false;
        self.dict.use_flag_aliases = false;
        self.dict.flag_mode = FlagMode::Char;
        const OPTIONS: [(&str, u32); 8] = [
            ("COMPOUNDFLAG", FF_COMPOUNDFLAG),
            ("COMPOUNDBEGIN", FF_COMPOUNDBEGIN),
            ("COMPOUNDLAST", FF_COMPOUNDLAST),
            // COMPOUNDLAST and COMPOUNDEND are synonyms.
            ("COMPOUNDEND", FF_COMPOUNDLAST),
            ("COMPOUNDMIDDLE", FF_COMPOUNDMIDDLE),
            ("ONLYINCOMPOUND", FF_COMPOUNDONLY),
            ("COMPOUNDPERMITFLAG", FF_COMPOUNDPERMITFLAG),
            ("COMPOUNDFORBIDFLAG", FF_COMPOUNDFORBIDFLAG),
        ];
        let skip = |l: &[u8]| l.is_empty() || is_space(l, 0) || l[0] == b'#';
        // The first pass: the options, and the flag mode.
        for recoded in lines(file) {
            if skip(recoded) {
                continue;
            }
            if let Some((kw, val)) = OPTIONS
                .iter()
                .find(|(kw, _)| recoded.starts_with(kw.as_bytes()))
            {
                self.add_compound_flag(&recoded[kw.len()..], *val)?;
            } else if recoded.starts_with(b"FLAG") {
                let mut s = 4;
                while s < recoded.len() && is_space(recoded, s) {
                    s += mblen(recoded, s);
                }
                if s < recoded.len() {
                    let rest = &recoded[s..];
                    if rest.starts_with(b"long") {
                        self.dict.flag_mode = FlagMode::Long;
                    } else if rest.starts_with(b"num") {
                        self.dict.flag_mode = FlagMode::Num;
                    } else if !rest.starts_with(b"default") {
                        return Err(config_error(
                            "Ispell dictionary supports only \"default\", \"long\", and \"num\" flag values",
                        ));
                    }
                }
            }
        }
        self.finalize_compound_flags()?;
        if self.dict.compound_flags.len() > 1 {
            pg_qsort(&mut self.dict.compound_flags, &mut |a, b| {
                a.flag.cmp(&b.flag)
            });
        }
        // The second pass: the aliases and the affixes.
        let mut naffix: usize = 0;
        let mut is_suffix = false;
        let mut flagflags = 0u32;
        for recoded in lines(file) {
            if skip(recoded) {
                continue;
            }
            let (fields_read, [typ, sflag, find, repl, mask]) = parse_ooaffentry(recoded);
            let ptype = text(&lower(typ.as_bytes()));
            if ptype.starts_with("af") {
                if !self.dict.use_flag_aliases {
                    // The first line is the number of aliases.
                    self.dict.use_flag_aliases = true;
                    let n = atoi(&sflag);
                    if n <= 0 {
                        return Err(config_error("invalid number of flag vector aliases"));
                    }
                    // A place for the empty flag set too.
                    naffix = usize::try_from(n).unwrap_or(usize::MAX - 1) + 1;
                    self.dict.affix_data = vec![String::new(); naffix];
                    self.aliases_read = 1;
                } else if self.aliases_read < naffix {
                    self.dict.affix_data[self.aliases_read] = sflag;
                    self.aliases_read += 1;
                } else {
                    return Err(config_error(format!(
                        "number of aliases exceeds specified number {}",
                        naffix - 1
                    )));
                }
                continue;
            }
            if fields_read < 4 || (!ptype.starts_with("sfx") && !ptype.starts_with("pfx")) {
                continue;
            }
            let sflaglen = sflag.len();
            if sflaglen == 0
                || (sflaglen > 1 && self.dict.flag_mode == FlagMode::Char)
                || (sflaglen > 2 && self.dict.flag_mode == FlagMode::Long)
            {
                continue;
            }
            if fields_read == 4 {
                // A header: SFX \ N 1.
                is_suffix = ptype.starts_with("sfx");
                flagflags = if find.starts_with(['y', 'Y']) {
                    FF_CROSSPRODUCT
                } else {
                    0
                };
            } else {
                // An entry: SFX \   0 Y/L [^Y].
                let mut aflg = 0;
                if let Some(p) = repl.find('/') {
                    let set = self.affix_flag_set(&repl[p + 1..])?;
                    aflg |= self.dict.compound_flag_value(&set)?;
                }
                let mut prepl = lower(repl.as_bytes());
                if let Some(p) = prepl.iter().position(|b| *b == b'/') {
                    prepl.truncate(p);
                }
                let mut pfind = lower(find.as_bytes());
                let pmask = text(&lower(mask.as_bytes()));
                if find.starts_with('0') {
                    pfind.clear();
                }
                if repl.starts_with('0') {
                    prepl.clear();
                }
                let typ = if is_suffix {
                    AffType::Suffix
                } else {
                    AffType::Prefix
                };
                self.add_affix(&sflag, flagflags | aflg, &pmask, &pfind, &prepl, typ)?;
            }
        }
        if self.dict.use_flag_aliases && self.aliases_read != naffix {
            return Err(config_error(format!(
                "number of aliases is less than specified number {}",
                naffix - 1
            )));
        }
        Ok(())
    }

    /// `getAffixFlagSet`.
    fn affix_flag_set(&self, s: &str) -> Result<String> {
        if !self.dict.use_flag_aliases || s.is_empty() {
            return Ok(s.to_string());
        }
        let invalid = || config_error(format!("invalid affix alias \"{s}\""));
        let (v, used, overflow) = strtol(s.as_bytes());
        if used == 0 || overflow {
            return Err(invalid());
        }
        let n = self.dict.affix_data.len() as i64;
        if v > 0 && v < n {
            if v as usize >= self.aliases_read {
                return Err(invalid());
            }
            // Not v - 1: the empty set is at 0.
            return Ok(self.dict.affix_data[v as usize].clone());
        } else if v > n {
            return Err(invalid());
        }
        Ok(String::new())
    }

    /// `NIAddAffix`.
    fn add_affix(
        &mut self,
        flag: &str,
        flagflags: u32,
        mask: &str,
        find: &[u8],
        repl: &[u8],
        typ: AffType,
    ) -> Result<()> {
        let cond = if mask == "." || mask.is_empty() {
            Cond::Simple
        } else if is_regis(mask) {
            Cond::Regis(Regis::compile(typ == AffType::Suffix, mask))
        } else {
            let anchored = match typ {
                AffType::Suffix => format!("{mask}$"),
                AffType::Prefix => format!("^{mask}"),
            };
            Cond::Regex(
                regex::Regex::new(&crate::scalar::pg_regex_source(&anchored))
                    .map_err(|e| Error::InvalidRegex(format!("invalid regular expression: {e}")))?,
            )
        };
        // `flagflags` is a seven-bit field.
        let mut flagflags = flagflags & 0x7f;
        if flagflags & (FF_COMPOUNDONLY | FF_COMPOUNDPERMITFLAG) != 0
            && flagflags & FF_COMPOUNDFLAG == 0
        {
            flagflags |= FF_COMPOUNDFLAG;
        }
        self.dict.affixes.push(Affix {
            flag: flag.to_string(),
            typ,
            flagflags,
            cond,
            find: find.to_vec(),
            repl: repl.to_vec(),
        });
        Ok(())
    }

    /// `NISortDictionary` then `NISortAffixes`: the tries built.
    pub fn finish(mut self) -> Result<IspellDict> {
        let mut spells = std::mem::take(&mut self.spells);
        let mut d = self.dict;
        if d.use_flag_aliases {
            let n = d.affix_data.len() as i64;
            for sp in &mut spells {
                sp.affix = if sp.flag.is_empty() {
                    0
                } else {
                    let invalid = || config_error(format!("invalid affix alias \"{}\"", sp.flag));
                    let f = sp.flag.as_bytes();
                    let (v, used, overflow) = strtol(f);
                    if used == 0 || overflow || v < 0 || v >= n {
                        return Err(invalid());
                    }
                    if let Some(&e) = f.get(used) {
                        if !e.is_ascii_digit() && !is_space(f, used) {
                            return Err(invalid());
                        }
                    }
                    v as usize
                };
            }
        } else {
            pg_qsort(&mut spells, &mut |a, b| {
                a.flag.as_bytes().cmp(b.flag.as_bytes())
            });
            d.affix_data.clear();
            for sp in &mut spells {
                if d.affix_data.last() != Some(&sp.flag) || d.affix_data.is_empty() {
                    d.affix_data.push(sp.flag.clone());
                }
                sp.affix = d.affix_data.len() - 1;
            }
        }
        pg_qsort(&mut spells, &mut |a, b| a.word.cmp(&b.word));
        d.dictionary = d.mk_sp_node(&spells, 0, spells.len(), 0)?;
        d.sort_affixes()?;
        Ok(d)
    }
}

/// `parseNumericAffixFlag`.
fn numeric_flag(s: &str) -> Result<u32> {
    let (v, used, overflow) = strtol(s.as_bytes());
    if used == 0 || overflow {
        return Err(config_error(format!("invalid affix flag \"{s}\"")));
    }
    if !(0..=FLAGNUM_MAXSIZE).contains(&v) {
        return Err(config_error(format!("affix flag \"{s}\" is out of range")));
    }
    Ok(v as u32)
}

/// `get_nextfield`: the next space-separated field, `None` at the end of
/// the line or a comment.
fn next_field(s: &[u8], at: &mut usize) -> Option<String> {
    let mut start = None;
    while *at < s.len() {
        let n = mblen(s, *at);
        match start {
            None => {
                if s[*at] == b'#' {
                    return None;
                }
                if !is_space(s, *at) {
                    start = Some(*at);
                }
            }
            Some(b) => {
                if is_space(s, *at) {
                    return Some(text(&s[b..*at]));
                }
            }
        }
        *at += n;
    }
    start.map(|b| text(&s[b..]))
}

/// `parse_ooaffentry`: `(fields read, [type, flag, find, repl, mask])`.
fn parse_ooaffentry(s: &[u8]) -> (usize, [String; 5]) {
    let mut fields: [String; 5] = Default::default();
    let mut at = 0;
    let mut n = 0;
    while at < s.len() && n < 5 {
        match next_field(s, &mut at) {
            Some(f) => {
                fields[n] = f;
                n += 1;
            }
            None => break,
        }
    }
    (n, fields)
}

/// An ispell-format entry: `(mask, find, repl)`.
type AffEntry = (String, Vec<u8>, Vec<u8>);

/// `parse_affentry`: an ispell-format entry, `mask > [-find,]repl`, of a
/// lower-cased line.
fn parse_affentry(s: &[u8]) -> Result<Option<AffEntry>> {
    #[derive(PartialEq)]
    enum S {
        WaitMask,
        InMask,
        WaitFind,
        InFind,
        WaitRepl,
        InRepl,
    }
    let syntax = || config_error("syntax error");
    let (mut mask, mut find, mut repl) = (Vec::new(), Vec::new(), Vec::new());
    let mut state = S::WaitMask;
    let mut i = 0;
    while i < s.len() {
        let n = mblen(s, i);
        let ch = &s[i..i + n];
        match state {
            S::WaitMask => {
                if s[i] == b'#' {
                    return Ok(None);
                } else if !is_space(s, i) {
                    mask.extend_from_slice(ch);
                    state = S::InMask;
                }
            }
            S::InMask => {
                if s[i] == b'>' {
                    state = S::WaitFind;
                } else if !is_space(s, i) {
                    mask.extend_from_slice(ch);
                }
            }
            S::WaitFind => {
                if s[i] == b'-' {
                    state = S::InFind;
                } else if is_alpha(s, i) || s[i] == b'\'' {
                    repl.extend_from_slice(ch);
                    state = S::InRepl;
                } else if !is_space(s, i) {
                    return Err(syntax());
                }
            }
            S::InFind => {
                if s[i] == b',' {
                    state = S::WaitRepl;
                } else if is_alpha(s, i) {
                    find.extend_from_slice(ch);
                } else if !is_space(s, i) {
                    return Err(syntax());
                }
            }
            S::WaitRepl => {
                if s[i] == b'-' {
                    // An empty replacement.
                    break;
                } else if is_alpha(s, i) {
                    repl.extend_from_slice(ch);
                    state = S::InRepl;
                } else if !is_space(s, i) {
                    return Err(syntax());
                }
            }
            S::InRepl => {
                if s[i] == b'#' {
                    break;
                } else if is_alpha(s, i) {
                    repl.extend_from_slice(ch);
                } else if !is_space(s, i) {
                    return Err(syntax());
                }
            }
        }
        i += n;
    }
    if mask.is_empty() || (find.is_empty() && repl.is_empty()) {
        return Ok(None);
    }
    Ok(Some((text(&mask), find, repl)))
}

/// `strbncmp`: the last `count` bytes compared backwards.
fn strbncmp(s1: &[u8], s2: &[u8], count: usize) -> Ordering {
    let (mut l1, mut l2) = (s1.len() as isize - 1, s2.len() as isize - 1);
    let mut l = count;
    while l1 >= 0 && l2 >= 0 && l > 0 {
        match s1[l1 as usize].cmp(&s2[l2 as usize]) {
            Ordering::Equal => {}
            o => return o,
        }
        l1 -= 1;
        l2 -= 1;
        l -= 1;
    }
    if l == 0 {
        return Ordering::Equal;
    }
    l1.cmp(&l2)
}

/// `strbcmp`.
fn strbcmp(s1: &[u8], s2: &[u8]) -> Ordering {
    s1.iter().rev().cmp(s2.iter().rev())
}

/// `GETWCHAR`: a prefix reads forwards, a suffix from the end.
fn getwchar(w: &[u8], n: usize, typ: AffType) -> u8 {
    match typ {
        AffType::Prefix => w[n],
        AffType::Suffix => w[w.len() - 1 - n],
    }
}

impl IspellDict {
    /// `getNextFlagFromString`: the flag at `*at` in a flag set.
    fn next_flag(&self, s: &[u8], at: &mut usize) -> Result<String> {
        let start = *at;
        let mut out: Vec<u8> = Vec::new();
        let mut maxstep = if self.flag_mode == FlagMode::Long {
            2
        } else {
            1
        };
        while *at < s.len() {
            match self.flag_mode {
                FlagMode::Char | FlagMode::Long => {
                    let n = mblen(s, *at);
                    out.extend_from_slice(&s[*at..*at + n]);
                    *at += n;
                    maxstep -= 1;
                    if maxstep == 0 {
                        break;
                    }
                }
                FlagMode::Num => {
                    let rest = text(&s[*at..]);
                    let (v, used, overflow) = strtol(&s[*at..]);
                    if used == 0 || overflow {
                        return Err(config_error(format!("invalid affix flag \"{rest}\"")));
                    }
                    if !(0..=FLAGNUM_MAXSIZE).contains(&v) {
                        return Err(config_error(format!(
                            "affix flag \"{rest}\" is out of range"
                        )));
                    }
                    out.extend_from_slice(v.to_string().as_bytes());
                    *at += used;
                    let mut met_comma = false;
                    while *at < s.len() {
                        let rest = || text(&s[*at..]);
                        if s[*at].is_ascii_digit() {
                            if !met_comma {
                                return Err(config_error(format!(
                                    "invalid affix flag \"{}\"",
                                    rest()
                                )));
                            }
                            break;
                        } else if s[*at] == b',' {
                            if met_comma {
                                return Err(config_error(format!(
                                    "invalid affix flag \"{}\"",
                                    rest()
                                )));
                            }
                            met_comma = true;
                        } else if !is_space(s, *at) {
                            return Err(config_error(format!(
                                "invalid character in affix flag \"{}\"",
                                rest()
                            )));
                        }
                        *at += mblen(s, *at);
                    }
                    break;
                }
            }
        }
        if self.flag_mode == FlagMode::Long && maxstep > 0 {
            return Err(config_error(format!(
                "invalid affix flag \"{}\" with \"long\" flag value",
                text(&s[start..])
            )));
        }
        Ok(text(&out))
    }

    /// `IsAffixFlagInUse`: is `flag` in the flag set `affix`?
    fn affix_flag_in_use(&self, affix: usize, flag: &str) -> Result<bool> {
        if flag.is_empty() {
            return Ok(true);
        }
        let set = self.affix_data[affix].as_bytes();
        let mut at = 0;
        while at < set.len() {
            if self.next_flag(set, &mut at)? == flag {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `getCompoundAffixFlagValue`: the compound options a flag set names.
    fn compound_flag_value(&self, s: &str) -> Result<u32> {
        if self.compound_flags.is_empty() {
            return Ok(0);
        }
        let mut value = 0;
        let s = s.as_bytes();
        let mut at = 0;
        while at < s.len() {
            let f = self.next_flag(s, &mut at)?;
            let key = if self.flag_mode == FlagMode::Num {
                FlagKey::Num(numeric_flag(&f)?)
            } else {
                FlagKey::Str(f)
            };
            if let Some(found) = bsearch(&self.compound_flags, |c| c.flag.cmp(&key)) {
                value |= found.value;
            }
        }
        Ok(value)
    }

    /// `makeCompoundFlags`.
    fn make_compound_flags(&self, affix: usize) -> Result<u32> {
        Ok(self.compound_flag_value(&self.affix_data[affix])? & FF_COMPOUNDFLAGMASK)
    }

    /// `MergeAffix`.
    fn merge_affix(&mut self, a1: usize, a2: usize) -> usize {
        if self.affix_data[a1].is_empty() {
            return a2;
        } else if self.affix_data[a2].is_empty() {
            return a1;
        }
        let merged = if self.flag_mode == FlagMode::Num {
            format!("{},{}", self.affix_data[a1], self.affix_data[a2])
        } else {
            format!("{}{}", self.affix_data[a1], self.affix_data[a2])
        };
        self.affix_data.push(merged);
        self.affix_data.len() - 1
    }

    /// `mkSPNode`: the word trie's level `level` over `spells[low..high]`.
    fn mk_sp_node(
        &mut self,
        spells: &[Spell],
        low: usize,
        high: usize,
        level: usize,
    ) -> Result<Option<Box<SpNode>>> {
        let mut nchar = 0;
        let mut last = 0u8;
        for sp in &spells[low..high] {
            if sp.word.len() > level && last != sp.word[level] {
                nchar += 1;
                last = sp.word[level];
            }
        }
        if nchar == 0 {
            return Ok(None);
        }
        let mut data: Vec<SpNodeData> = Vec::with_capacity(nchar);
        data.push(SpNodeData::default());
        let mut cur = 0;
        let mut last = 0u8;
        let mut lownew = low;
        for i in low..high {
            let sp = &spells[i];
            if sp.word.len() <= level {
                continue;
            }
            let c = sp.word[level];
            if last != c {
                if last != 0 {
                    data[cur].node = self.mk_sp_node(spells, lownew, i, level + 1)?;
                    lownew = i;
                    data.push(SpNodeData::default());
                    cur += 1;
                }
                last = c;
            }
            data[cur].val = c;
            if sp.word.len() == level + 1 {
                let mut clear_compound_only = false;
                if data[cur].isword && data[cur].affix != sp.affix {
                    // When one of two equal words may be in a compound and
                    // the other may not, the merged one is not
                    // compound-only.
                    let other = self.make_compound_flags(sp.affix)?;
                    clear_compound_only = FF_COMPOUNDONLY & data[cur].compoundflag & other == 0;
                    data[cur].affix = self.merge_affix(data[cur].affix, sp.affix);
                } else {
                    data[cur].affix = sp.affix;
                }
                data[cur].isword = true;
                let mut cf = self.make_compound_flags(data[cur].affix)?;
                if cf & FF_COMPOUNDONLY != 0 && cf & FF_COMPOUNDFLAG == 0 {
                    cf |= FF_COMPOUNDFLAG;
                }
                if clear_compound_only {
                    cf &= !FF_COMPOUNDONLY;
                }
                data[cur].compoundflag = cf & FF_COMPOUNDFLAGMASK;
            }
        }
        data[cur].node = self.mk_sp_node(spells, lownew, high, level + 1)?;
        Ok(Some(Box::new(SpNode { data })))
    }

    /// `mkANode`: the affix trie's level `level` over `affixes[low..high]`,
    /// on the replacement strings.
    fn mk_a_node(
        &self,
        low: usize,
        high: usize,
        level: usize,
        typ: AffType,
    ) -> Option<Box<AffixNode>> {
        let getc = |i: usize| getwchar(&self.affixes[i].repl, level, typ);
        let mut nchar = 0;
        let mut last = 0u8;
        for i in low..high {
            if self.affixes[i].repl.len() > level && last != getc(i) {
                nchar += 1;
                last = getc(i);
            }
        }
        if nchar == 0 {
            return None;
        }
        let mut aff: Vec<usize> = Vec::new();
        let mut data: Vec<AffixNodeData> = Vec::with_capacity(nchar);
        data.push(AffixNodeData::default());
        let mut cur = 0;
        let mut last = 0u8;
        let mut lownew = low;
        for i in low..high {
            if self.affixes[i].repl.len() <= level {
                continue;
            }
            let c = getc(i);
            if last != c {
                if last != 0 {
                    data[cur].node = self.mk_a_node(lownew, i, level + 1, typ);
                    data[cur].aff = std::mem::take(&mut aff);
                    data.push(AffixNodeData::default());
                    cur += 1;
                    lownew = i;
                }
                last = c;
            }
            data[cur].val = c;
            if self.affixes[i].repl.len() == level + 1 {
                aff.push(i);
            }
        }
        data[cur].node = self.mk_a_node(lownew, high, level + 1, typ);
        data[cur].aff = aff;
        Some(Box::new(AffixNode {
            isvoid: false,
            data,
        }))
    }

    /// `mkVoidAffix`: a root above the trie that holds the affixes with an
    /// empty replacement.
    fn mk_void_affix(&mut self, issuffix: bool, startsuffix: usize) {
        let range = if issuffix {
            startsuffix..self.affixes.len()
        } else {
            0..startsuffix
        };
        let aff: Vec<usize> = range.filter(|&i| self.affixes[i].repl.is_empty()).collect();
        let slot = if issuffix {
            &mut self.suffix
        } else {
            &mut self.prefix
        };
        let node = slot.take();
        *slot = Some(Box::new(AffixNode {
            isvoid: true,
            data: vec![AffixNodeData { val: 0, aff, node }],
        }));
    }

    /// `isAffixInUse`.
    fn affix_in_use(&self, flag: &str) -> Result<bool> {
        for i in 0..self.affix_data.len() {
            if self.affix_flag_in_use(i, flag)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `NISortAffixes`.
    fn sort_affixes(&mut self) -> Result<()> {
        if self.affixes.is_empty() {
            return Ok(());
        }
        if self.affixes.len() > 1 {
            pg_qsort(&mut self.affixes, &mut |a, b| {
                a.typ.cmp(&b.typ).then_with(|| match a.typ {
                    AffType::Prefix => a.repl.cmp(&b.repl),
                    AffType::Suffix => strbcmp(&a.repl, &b.repl),
                })
            });
        }
        let mut firstsuffix = self.affixes.len();
        let mut compound: Vec<CmpdAffix> = Vec::new();
        for i in 0..self.affixes.len() {
            let a = &self.affixes[i];
            if a.typ == AffType::Suffix && i < firstsuffix {
                firstsuffix = i;
            }
            if a.flagflags & FF_COMPOUNDFLAG != 0
                && !a.repl.is_empty()
                && self.affix_in_use(&a.flag)?
            {
                let issuffix = a.typ == AffType::Suffix;
                // Only the unique and shortest ones.
                let new = match compound.last() {
                    None => true,
                    Some(p) => {
                        issuffix != p.issuffix
                            || strbncmp(&p.affix, &a.repl, p.affix.len()) != Ordering::Equal
                    }
                };
                if new {
                    compound.push(CmpdAffix {
                        affix: a.repl.clone(),
                        issuffix,
                    });
                }
            }
        }
        self.compound_affix = Some(compound);
        self.prefix = self.mk_a_node(0, firstsuffix, 0, AffType::Prefix);
        self.suffix = self.mk_a_node(firstsuffix, self.affixes.len(), 0, AffType::Suffix);
        self.mk_void_affix(true, firstsuffix);
        self.mk_void_affix(false, firstsuffix);
        Ok(())
    }

    /// `FindWord`: is `word` in the dictionary with `affixflag` in its flag
    /// set, and allowed where `flag` (a compound position, or 0) puts it?
    fn find_word(&self, word: &[u8], affixflag: &str, flag: u32) -> Result<bool> {
        let flag = flag & FF_COMPOUNDFLAGMASK;
        let mut node = self.dictionary.as_deref();
        let mut p = 0;
        while let Some(n) = node {
            if p >= word.len() {
                break;
            }
            let Some(m) = bsearch(&n.data, |d| d.val.cmp(&word[p])) else {
                break;
            };
            if p + 1 == word.len() && m.isword {
                if flag == 0 {
                    // A word only another word can form, outside a
                    // compound.
                    if m.compoundflag & FF_COMPOUNDONLY != 0 {
                        return Ok(false);
                    }
                } else if flag & m.compoundflag == 0 {
                    return Ok(false);
                }
                if self.affix_flag_in_use(m.affix, affixflag)? {
                    return Ok(true);
                }
            }
            node = m.node.as_deref();
            p += 1;
        }
        Ok(false)
    }

    /// `FindAffixes`: the next trie node down `word` that ends affixes.
    fn find_affixes<'a>(
        node: &'a AffixNode,
        word: &[u8],
        level: &mut usize,
        typ: AffType,
    ) -> Option<&'a AffixNodeData> {
        let mut node = Some(node);
        if let Some(n) = node {
            if n.isvoid {
                if !n.data[0].aff.is_empty() {
                    return Some(&n.data[0]);
                }
                node = n.data[0].node.as_deref();
            }
        }
        while let Some(n) = node {
            if *level >= word.len() {
                break;
            }
            let symbol = getwchar(word, *level, typ);
            let m = bsearch(&n.data, |d| d.val.cmp(&symbol))?;
            *level += 1;
            if !m.aff.is_empty() {
                return Some(m);
            }
            node = m.node.as_deref();
        }
        None
    }

    /// `CheckAffix`: `word` with the affix taken off, when it applies.
    fn check_affix(
        &self,
        word: &[u8],
        affix: &Affix,
        flagflags: u32,
        baselen: Option<&mut usize>,
    ) -> Option<Vec<u8>> {
        // The compound flags allow it?
        if flagflags == 0 {
            if affix.flagflags & FF_COMPOUNDONLY != 0 {
                return None;
            }
        } else if flagflags & FF_COMPOUNDBEGIN != 0 {
            if affix.flagflags & FF_COMPOUNDFORBIDFLAG != 0 {
                return None;
            }
            if affix.flagflags & FF_COMPOUNDBEGIN == 0 && affix.typ == AffType::Suffix {
                return None;
            }
        } else if flagflags & FF_COMPOUNDMIDDLE != 0 {
            if affix.flagflags & FF_COMPOUNDMIDDLE == 0
                || affix.flagflags & FF_COMPOUNDFORBIDFLAG != 0
            {
                return None;
            }
        } else if flagflags & FF_COMPOUNDLAST != 0 {
            if affix.flagflags & FF_COMPOUNDFORBIDFLAG != 0 {
                return None;
            }
            if affix.flagflags & FF_COMPOUNDLAST == 0 && affix.typ == AffType::Prefix {
                return None;
            }
        }
        if word.len() < affix.repl.len() {
            return None;
        }
        let keeplen = word.len() - affix.repl.len();
        if keeplen + affix.find.len() >= 2 * MAXNORMLEN {
            return None;
        }
        let newword = match affix.typ {
            AffType::Suffix => {
                if let Some(b) = baselen {
                    *b = keeplen;
                }
                [&word[..keeplen], &affix.find[..]].concat()
            }
            AffType::Prefix => {
                // A prefix as long as the part a suffix left unchanged: the
                // word was only a prefix and a suffix.
                if let Some(b) = baselen {
                    if *b + affix.find.len() <= affix.repl.len() {
                        return None;
                    }
                }
                [&affix.find[..], &word[affix.repl.len()..]].concat()
            }
        };
        let ok = match &affix.cond {
            Cond::Simple => true,
            Cond::Regis(r) => r.execute(&newword),
            Cond::Regex(re) => re.is_match(&String::from_utf8_lossy(&newword)),
        };
        ok.then_some(newword)
    }

    /// `NormalizeSubWord`: `word`'s normal forms where `flag` puts it.
    fn normalize_sub_word(&self, word: &[u8], flag: u32) -> Result<Option<Vec<Vec<u8>>>> {
        if word.len() > MAXNORMLEN {
            return Ok(None);
        }
        let mut forms: Vec<Vec<u8>> = Vec::new();
        // `addToResult`.
        let add = |forms: &mut Vec<Vec<u8>>, w: Vec<u8>| {
            if forms.len() < MAX_NORM - 1 && forms.last() != Some(&w) {
                forms.push(w);
            }
        };
        // The word itself is a normal form?
        if self.find_word(word, "", flag)? {
            forms.push(word.to_vec());
        }
        // Its other normal forms: a prefix off.
        let mut pnode = self.prefix.as_deref();
        let mut plevel = 0;
        while let Some(pn) = pnode {
            let Some(prefix) = Self::find_affixes(pn, word, &mut plevel, AffType::Prefix) else {
                break;
            };
            for &j in &prefix.aff {
                let a = &self.affixes[j];
                if let Some(nw) = self.check_affix(word, a, flag, None) {
                    if self.find_word(&nw, &a.flag, flag)? {
                        add(&mut forms, nw);
                    }
                }
            }
            pnode = prefix.node.as_deref();
        }
        // A suffix off, then a prefix too.
        let mut snode = self.suffix.as_deref();
        let mut slevel = 0;
        while let Some(sn) = snode {
            let mut baselen = 0;
            let Some(suffix) = Self::find_affixes(sn, word, &mut slevel, AffType::Suffix) else {
                break;
            };
            for &i in &suffix.aff {
                let s = &self.affixes[i];
                let Some(nw) = self.check_affix(word, s, flag, Some(&mut baselen)) else {
                    continue;
                };
                if self.find_word(&nw, &s.flag, flag)? {
                    add(&mut forms, nw.clone());
                }
                let mut pnode = self.prefix.as_deref();
                let mut plevel = 0;
                while let Some(pn) = pnode {
                    let Some(prefix) = Self::find_affixes(pn, &nw, &mut plevel, AffType::Prefix)
                    else {
                        break;
                    };
                    for &j in &prefix.aff {
                        let p = &self.affixes[j];
                        if let Some(pnw) = self.check_affix(&nw, p, flag, Some(&mut baselen)) {
                            let ff = if p.flagflags & s.flagflags & FF_CROSSPRODUCT != 0 {
                                ""
                            } else {
                                p.flag.as_str()
                            };
                            if self.find_word(&pnw, ff, flag)? {
                                add(&mut forms, pnw);
                            }
                        }
                    }
                    pnode = prefix.node.as_deref();
                }
            }
            snode = suffix.node.as_deref();
        }
        Ok((!forms.is_empty()).then_some(forms))
    }

    /// `CheckCompoundAffixes`: the length to the next compound affix in
    /// `word` from `*cursor` on, -1 when there is none.
    fn check_compound_affixes(&self, cursor: &mut usize, word: &[u8], in_place: bool) -> isize {
        let Some(ca) = &self.compound_affix else {
            return -1;
        };
        let len = word.len();
        while *cursor < ca.len() {
            let a = &ca[*cursor];
            *cursor += 1;
            if len <= a.affix.len() {
                continue;
            }
            let at = if in_place {
                word.starts_with(&a.affix).then_some(0)
            } else {
                word.windows(a.affix.len())
                    .position(|w| w == a.affix.as_slice())
            };
            if let Some(off) = at {
                return if a.issuffix {
                    (a.affix.len() + off) as isize
                } else {
                    0
                };
            }
        }
        -1
    }

    /// `SplitToVariants`: the ways `word` splits into dictionary words from
    /// `startpos`, each a list of stems, `orig`'s first.
    fn split_to_variants(
        &self,
        snode: Option<&SpNode>,
        orig: &[Vec<u8>],
        word: &[u8],
        mut startpos: usize,
        minpos: isize,
    ) -> Result<Vec<Vec<Vec<u8>>>> {
        let wordlen = word.len();
        let mut node = snode.or(self.dictionary.as_deref());
        let mut level = if snode.is_some() {
            minpos as usize
        } else {
            startpos
        };
        let mut notprobed = vec![true; wordlen];
        let mut var: Vec<Vec<u8>> = orig.to_vec();
        let mut rest: Vec<Vec<Vec<u8>>> = Vec::new();
        let done = |var: Vec<Vec<u8>>, rest: Vec<Vec<Vec<u8>>>| {
            let mut out = Vec::with_capacity(rest.len() + 1);
            out.push(var);
            out.extend(rest);
            out
        };
        while level < wordlen {
            // A word with an epenthetic or compound affix.
            let mut caff = 0;
            if level > startpos {
                loop {
                    let found =
                        self.check_compound_affixes(&mut caff, &word[level..], node.is_some());
                    if found < 0 {
                        break;
                    }
                    let lenaff = level - startpos + found as usize;
                    if !notprobed[startpos + lenaff - 1] {
                        continue;
                    }
                    if (level + lenaff) as isize - 1 <= minpos {
                        continue;
                    }
                    if lenaff >= MAXNORMLEN {
                        continue;
                    }
                    let buf = &word[startpos..startpos + lenaff];
                    let compoundflag = if level == 0 {
                        FF_COMPOUNDBEGIN
                    } else if level == wordlen - 1 {
                        FF_COMPOUNDLAST
                    } else {
                        FF_COMPOUNDMIDDLE
                    };
                    if let Some(subres) = self.normalize_sub_word(buf, compoundflag)? {
                        // It was a dictionary word.
                        let mut new = var.clone();
                        notprobed[startpos + lenaff - 1] = false;
                        new.extend(subres);
                        rest.extend(self.split_to_variants(
                            None,
                            &new,
                            word,
                            startpos + lenaff,
                            (startpos + lenaff) as isize,
                        )?);
                    }
                }
            }
            let Some(n) = node else {
                break;
            };
            match bsearch(&n.data, |d| d.val.cmp(&word[level])) {
                Some(m) => {
                    let compoundflag = if startpos == 0 {
                        FF_COMPOUNDBEGIN
                    } else if level == wordlen - 1 {
                        FF_COMPOUNDLAST
                    } else {
                        FF_COMPOUNDMIDDLE
                    };
                    // A whole word allowed in a compound here, longer than
                    // the least?
                    if m.isword
                        && m.compoundflag & compoundflag != 0
                        && notprobed[level]
                        && level as isize > minpos
                    {
                        if wordlen == level + 1 {
                            // The last word.
                            var.push(word[startpos..].to_vec());
                            return Ok(done(var, rest));
                        }
                        // A longer word from the same place, then the
                        // next word.
                        rest.extend(self.split_to_variants(
                            Some(n),
                            &var,
                            word,
                            startpos,
                            level as isize,
                        )?);
                        level += 1;
                        var.push(word[startpos..level].to_vec());
                        node = self.dictionary.as_deref();
                        startpos = level;
                        continue;
                    }
                    node = m.node.as_deref();
                }
                None => node = None,
            }
            level += 1;
        }
        var.push(word[startpos..].to_vec());
        Ok(done(var, rest))
    }

    /// `NINormalizeWord`: `(lexeme, variant)` for each normal form, a
    /// compound's parts sharing their variant; `None` for a word the
    /// dictionary does not know.
    pub fn normalize_word(&self, word: &[u8]) -> Result<Option<Vec<(String, u16)>>> {
        let mut out: Vec<(String, u16)> = Vec::new();
        let add = |out: &mut Vec<(String, u16)>, w: &[u8], v: u16| {
            if out.len() < MAX_NORM - 1 {
                out.push((text(w), v));
            }
        };
        let mut variant: u16 = 1;
        if let Some(res) = self.normalize_sub_word(word, 0)? {
            for f in res {
                add(&mut out, &f, variant);
                variant = variant.wrapping_add(1);
            }
        }
        if self.usecompound {
            for v in self.split_to_variants(None, &[], word, 0, -1)? {
                if v.len() <= 1 {
                    continue;
                }
                let Some(subres) = self.normalize_sub_word(&v[v.len() - 1], FF_COMPOUNDLAST)?
                else {
                    continue;
                };
                for s in subres {
                    for stem in &v[..v.len() - 1] {
                        add(&mut out, stem, variant);
                    }
                    add(&mut out, &s, variant);
                    variant = variant.wrapping_add(1);
                }
            }
        }
        Ok((!out.is_empty()).then_some(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(dict: &str, affix: &str) -> Result<IspellDict> {
        let mut b = IspellBuilder::new();
        b.import_dictionary(dict);
        b.import_affixes(affix)?;
        b.finish()
    }

    fn forms(d: &IspellDict, w: &str) -> Option<Vec<String>> {
        d.normalize_word(w.as_bytes())
            .unwrap()
            .map(|v| v.into_iter().map(|(l, _)| l).collect())
    }

    const ISPELL_DICT: &str = include_str!("tsearch_data/ispell_sample.dict");
    const ISPELL_AFFIX: &str = include_str!("tsearch_data/ispell_sample.affix");

    /// PostgreSQL 15's `tsdicts` regression answers.
    #[test]
    fn ispell_sample() {
        let d = build(ISPELL_DICT, ISPELL_AFFIX).unwrap();
        for (w, want) in [
            ("skies", Some(vec!["sky"])),
            ("bookings", Some(vec!["booking", "book"])),
            ("foots", Some(vec!["foot"])),
            ("rebook", None),
            ("unbookings", Some(vec!["book"])),
            ("footklubber", Some(vec!["foot", "klubber"])),
            (
                "footballklubber",
                Some(vec![
                    "footballklubber",
                    "foot",
                    "ball",
                    "klubber",
                    "football",
                    "klubber",
                ]),
            ),
            ("ballyklubber", Some(vec!["ball", "klubber"])),
            ("footballyklubber", Some(vec!["foot", "ball", "klubber"])),
        ] {
            let want = want.map(|v| v.into_iter().map(String::from).collect::<Vec<_>>());
            assert_eq!(forms(&d, w), want, "{w}");
        }
    }

    #[test]
    fn hunspell_long_and_num() {
        let d = build(
            include_str!("tsearch_data/hunspell_sample_long.dict"),
            include_str!("tsearch_data/hunspell_sample_long.affix"),
        )
        .unwrap();
        assert_eq!(forms(&d, "booked"), Some(vec!["book".to_string()]));
        assert_eq!(
            forms(&d, "ex-machina"),
            Some(vec!["ex-".to_string(), "machina".to_string()])
        );
        let d = build(
            include_str!("tsearch_data/hunspell_sample_num.dict"),
            include_str!("tsearch_data/hunspell_sample_num.affix"),
        )
        .unwrap();
        assert_eq!(forms(&d, "sk"), Some(vec!["sky".to_string()]));
        let e = build(
            ISPELL_DICT,
            include_str!("tsearch_data/hunspell_sample_num.affix"),
        )
        .unwrap_err();
        assert!(e.to_string().contains("invalid affix flag \"SZ\\\""), "{e}");
    }
}
