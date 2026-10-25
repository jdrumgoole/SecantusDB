//! `ts_headline`, transcribed from PostgreSQL 15's `ts_parse.c`
//! (`hlparsetext`, `hlfinditem`, `generateHeadline`) and the default
//! parser's `prsd_headline` (`wparser_def.c`: `hlCover`, `mark_hl_words`,
//! `mark_hl_fragments`, `get_next_fragment`, `mark_fragment`).
//!
//! The document is cut into the default parser's tokens; a token whose
//! normalised lexeme a query operand matches carries that operand (and a
//! REPEATED copy of itself for each further operand it matches). A cover is
//! the earliest, shortest run of words that satisfies the query; the
//! headline is chosen around the best cover by the option rules below.

use std::collections::BTreeMap;

use super::*;
use crate::wparser as w;

/// `prsd_headline`'s options, with its defaults.
pub struct HeadlineOptions {
    pub start: String,
    pub stop: String,
    pub delimiter: String,
    pub max_words: i32,
    pub min_words: i32,
    pub short_word: i32,
    pub max_fragments: i32,
    pub highlight_all: bool,
}

impl Default for HeadlineOptions {
    fn default() -> Self {
        HeadlineOptions {
            start: "<b>".into(),
            stop: "</b>".into(),
            delimiter: " ... ".into(),
            max_words: 35,
            min_words: 15,
            short_word: 3,
            max_fragments: 0,
            highlight_all: false,
        }
    }
}

/// `pg_strtoint32` of an option value.
fn int_option(v: &str) -> Result<i32> {
    v.trim().parse::<i32>().map_err(|_| {
        Error::Sqlstate(
            "22P02",
            format!("invalid input syntax for type integer: \"{v}\""),
        )
    })
}

/// `prsd_headline`'s option list and its checks (the checks only outside
/// HighlightAll mode).
pub fn headline_options(text: &str) -> Result<HeadlineOptions> {
    let mut o = HeadlineOptions::default();
    for (name, value) in deserialize_deflist(text)? {
        match name.to_ascii_lowercase().as_str() {
            "maxwords" => o.max_words = int_option(&value)?,
            "minwords" => o.min_words = int_option(&value)?,
            "shortword" => o.short_word = int_option(&value)?,
            "maxfragments" => o.max_fragments = int_option(&value)?,
            "startsel" => o.start = value,
            "stopsel" => o.stop = value,
            "fragmentdelimiter" => o.delimiter = value,
            "highlightall" => {
                o.highlight_all = ["1", "on", "true", "t", "y", "yes"]
                    .iter()
                    .any(|t| value.eq_ignore_ascii_case(t));
            }
            _ => {
                return Err(Error::InvalidParameter(format!(
                    "unrecognized headline parameter: \"{name}\""
                )))
            }
        }
    }
    if !o.highlight_all {
        let bad = |m: &str| Err(Error::InvalidParameter(m.to_string()));
        if o.min_words >= o.max_words {
            return bad("MinWords should be less than MaxWords");
        }
        if o.min_words <= 0 {
            return bad("MinWords should be positive");
        }
        if o.short_word < 0 {
            return bad("ShortWord should be >= 0");
        }
        if o.max_fragments < 0 {
            return bad("MaxFragments should be >= 0");
        }
    }
    Ok(o)
}

/// One `HeadlineWordEntry`.
#[derive(Clone, Default)]
struct Word {
    text: String,
    ty: u8,
    /// The query operand (its index among the operands) the word matches.
    item: Option<usize>,
    repeated: bool,
    pos: u16,
    selected: bool,
    replace: bool,
    skip: bool,
    in_frag: bool,
}

fn nonword(ty: u8) -> bool {
    ty == w::SPACE || ty == w::TAG_T || hlskip(ty)
}

fn hlskip(ty: u8) -> bool {
    matches!(ty, w::URL_T | w::NUMHWORD | w::ASCIIHWORD | w::HWORD)
}

fn noend(ty: u8) -> bool {
    nonword(ty)
        || matches!(
            ty,
            w::SCIENTIFIC
                | w::VERSIONNUMBER
                | w::DECIMAL_T
                | w::SIGNEDINT
                | w::UNSIGNEDINT
                | w::PROTOCOL
                | w::XMLENTITY
        )
}

/// The query's operands in order, and the query with each operand renamed
/// to its index (weights dropped: `checkcondition_HL` matches the operand
/// itself), so a run of words can be checked as a vector of those names.
fn operands(q: &Query, ops: &mut Vec<(String, bool)>) -> Query {
    match q {
        Query::Val { lex, prefix, .. } => {
            ops.push((lex.clone(), *prefix));
            Query::Val {
                lex: format!("{}", ops.len() - 1),
                prefix: false,
                weight: 0,
            }
        }
        Query::Stop => Query::Stop,
        Query::Not(a) => Query::Not(Box::new(operands(a, ops))),
        Query::And(a, b) => Query::And(Box::new(operands(a, ops)), Box::new(operands(b, ops))),
        Query::Or(a, b) => Query::Or(Box::new(operands(a, ops)), Box::new(operands(b, ops))),
        Query::Phrase(a, b, d) => {
            Query::Phrase(Box::new(operands(a, ops)), Box::new(operands(b, ops)), *d)
        }
    }
}

/// `hlfinditem`: each operand `lex` matches; the first marks word `last`,
/// each further one a repeated copy of it.
fn hl_find_item(words: &mut Vec<Word>, last: usize, ops: &[(String, bool)], pos: u32, lex: &str) {
    words[last].pos = u16::try_from(pos.min(u32::from(MAX_POS))).unwrap_or(MAX_POS);
    for (k, (op, prefix)) in ops.iter().enumerate() {
        let hit = if *prefix {
            lex.starts_with(op.as_str())
        } else {
            lex == *op
        };
        if !hit {
            continue;
        }
        if words[last].item.is_some() {
            let mut copy = words[last].clone();
            copy.item = Some(k);
            copy.repeated = true;
            words.push(copy);
        } else {
            words[last].item = Some(k);
        }
    }
}

struct Prs {
    words: Vec<Word>,
    query: Option<Query>,
    short_word: i32,
}

impl Prs {
    /// `hlparsetext`: the tokens through the configuration's dictionaries
    /// (`lexize::Lexizer`), each token a word, every token a dictionary
    /// used for one result matched against the result's lexemes.
    fn parse(cfg: Config, doc: &str, q: &Option<Query>) -> Result<Prs> {
        let mut ops = Vec::new();
        let query = q.as_ref().map(|q| operands(q, &mut ops));
        let mut words: Vec<Word> = Vec::new();
        let mut vectorpos: u32 = 0;
        let mut lx = lexize::Lexizer::new(cfg);
        for (ty, tok) in w::parse(doc).into_iter().chain([(0, String::new())]) {
            if ty > 0 && tok.len() >= MAX_LEXEME {
                notice("word is too long to be indexed");
                continue;
            }
            lx.add(ty, tok);
            loop {
                let (norms, lexs) = lx.exec()?;
                if norms.is_some() {
                    vectorpos += 1;
                }
                // `addHLParsedLex`.
                for l in lexs {
                    if l.ty > 0 {
                        words.push(Word {
                            text: l.lemm,
                            ty: l.ty,
                            ..Default::default()
                        });
                    }
                    let mut savedpos = vectorpos;
                    for lex in norms.iter().flatten() {
                        if lex.flags & crate::ts_dicts::TSL_ADDPOS != 0 {
                            savedpos += 1;
                        }
                        let Some(last) = words.len().checked_sub(1) else {
                            continue;
                        };
                        hl_find_item(&mut words, last, &ops, savedpos, &lex.lexeme);
                    }
                }
                let Some(norms) = norms else {
                    break;
                };
                for lex in &norms {
                    if lex.flags & crate::ts_dicts::TSL_ADDPOS != 0 {
                        vectorpos += 1;
                    }
                }
            }
        }
        Ok(Prs {
            words,
            query,
            short_word: 0,
        })
    }

    fn interesting(&self, j: usize) -> bool {
        self.words[j].item.is_some() && !self.words[j].repeated
    }

    fn bad_endpoint(&self, j: usize) -> bool {
        let wd = &self.words[j];
        (noend(wd.ty) || wd.text.len() as i32 <= self.short_word) && !self.interesting(j)
    }

    fn counts(&self, j: usize) -> bool {
        !nonword(self.words[j].ty)
    }

    fn first_index(&self, pos: usize) -> Option<usize> {
        (pos..self.words.len()).find(|&i| self.words[i].item.is_some())
    }

    /// Does the run `pmin..=pmax` satisfy the query (`checkcondition_HL`)?
    fn satisfies(&self, pmin: usize, pmax: usize) -> bool {
        let Some(q) = &self.query else {
            return false;
        };
        let mut map: BTreeMap<String, Vec<(u16, u8)>> = BTreeMap::new();
        for wd in &self.words[pmin..=pmax] {
            if let Some(k) = wd.item {
                let e = map.entry(k.to_string()).or_default();
                if !e.iter().any(|(p, _)| *p == wd.pos) {
                    e.push((wd.pos, 0));
                }
            }
        }
        for v in map.values_mut() {
            v.sort_unstable();
        }
        eval(&TsVector(map), q)
    }

    /// `hlCover`: the earliest, shortest satisfying run from `*p` on.
    fn cover(&self, max_cover: usize, p: &mut usize, q: &mut usize) -> bool {
        if self.query.is_none() {
            return false;
        }
        let mut pmin = self.first_index(*p);
        while let Some(start) = pmin {
            let mut nextpmin = None;
            let mut pmax = Some(start);
            while let Some(end) = pmax {
                if end - start >= max_cover {
                    break;
                }
                if self.satisfies(start, end) {
                    *p = start;
                    *q = end;
                    return true;
                }
                let next = self.first_index(end + 1);
                if end == start {
                    nextpmin = next;
                }
                pmax = next;
            }
            pmin = nextpmin;
        }
        false
    }

    /// `mark_fragment`.
    fn mark(&mut self, highlight_all: bool, from: usize, to: isize) {
        let mut i = from as isize;
        while i <= to {
            let wd = &mut self.words[i as usize];
            if wd.item.is_some() {
                wd.selected = true;
            }
            if !highlight_all {
                if wd.ty == w::TAG_T {
                    wd.replace = true;
                } else if hlskip(wd.ty) {
                    wd.skip = true;
                }
            } else if hlskip(wd.ty) {
                wd.skip = true;
            }
            wd.in_frag = !wd.repeated;
            i += 1;
        }
    }
}

/// `mark_hl_words`.
fn mark_hl_words(prs: &mut Prs, o: &HeadlineOptions, max_cover: usize) {
    let n = prs.words.len() as isize;
    let (min_words, max_words) = (o.min_words, o.max_words);
    let (mut bestb, mut beste): (isize, isize) = (-1, -1);
    let mut bestlen: i32 = -1;
    let mut bestcover = false;
    if !o.highlight_all {
        let (mut p, mut q) = (0usize, 0usize);
        while prs.cover(max_cover, &mut p, &mut q) {
            let (pi, qi) = (p as isize, q as isize);
            let (mut curlen, mut poslen) = (0i32, 0i32);
            let mut posb = pi;
            let mut pose = pi;
            let mut i = pi;
            while i <= qi && curlen < max_words {
                if prs.counts(i as usize) {
                    curlen += 1;
                }
                if prs.interesting(i as usize) {
                    poslen += 1;
                }
                pose = i;
                i += 1;
            }
            if curlen < max_words {
                i -= 1;
                while i < n && curlen < max_words {
                    let u = i as usize;
                    if i > qi {
                        if prs.counts(u) {
                            curlen += 1;
                        }
                        if prs.interesting(u) {
                            poslen += 1;
                        }
                    }
                    pose = i;
                    if prs.bad_endpoint(u) {
                        i += 1;
                        continue;
                    }
                    if curlen >= min_words {
                        break;
                    }
                    i += 1;
                }
                if curlen < min_words {
                    i = pi - 1;
                    while i >= 0 {
                        let u = i as usize;
                        if prs.counts(u) {
                            curlen += 1;
                        }
                        if prs.interesting(u) {
                            poslen += 1;
                        }
                        if curlen >= max_words {
                            break;
                        }
                        if prs.bad_endpoint(u) {
                            i -= 1;
                            continue;
                        }
                        if curlen >= min_words {
                            break;
                        }
                        i -= 1;
                    }
                    posb = i.max(0);
                }
            } else {
                if i > qi {
                    i = qi;
                }
                while curlen > min_words {
                    let u = i as usize;
                    if !prs.bad_endpoint(u) {
                        break;
                    }
                    if prs.counts(u) {
                        curlen -= 1;
                    }
                    if prs.interesting(u) {
                        poslen -= 1;
                    }
                    pose = i - 1;
                    i -= 1;
                }
            }
            let poscover = posb <= pi && pose >= qi;
            let bad = |j: isize| j < 0 || prs.bad_endpoint(j as usize);
            if (poscover && !bestcover)
                || (poscover == bestcover && poslen > bestlen)
                || (poscover == bestcover && poslen == bestlen && !bad(pose) && bad(beste))
            {
                bestb = posb;
                beste = pose;
                bestlen = poslen;
                bestcover = poscover;
            }
            p += 1;
        }
        if bestlen < 0 {
            let mut curlen = 0;
            let mut pose = -1;
            let mut i = 0;
            while i < n && curlen < min_words {
                if prs.counts(i as usize) {
                    curlen += 1;
                }
                pose = i;
                i += 1;
            }
            bestb = 0;
            beste = pose;
        }
    } else {
        bestb = 0;
        beste = n - 1;
    }
    prs.mark(o.highlight_all, bestb.max(0) as usize, beste);
}

/// One cover (fragment) for `mark_hl_fragments`.
struct CoverPos {
    start: isize,
    end: isize,
    poslen: i32,
    curlen: i32,
    chosen: bool,
    excluded: bool,
}

/// `get_next_fragment`.
fn next_fragment(prs: &Prs, start: &mut isize, end: &mut isize, max_words: i32) -> (i32, i32) {
    let mut i = *start;
    while i <= *end {
        *start = i;
        if prs.interesting(i as usize) {
            break;
        }
        i += 1;
    }
    let (mut curlen, mut poslen) = (0, 0);
    i = *start;
    while i <= *end && curlen < max_words {
        if prs.counts(i as usize) {
            curlen += 1;
        }
        if prs.interesting(i as usize) {
            poslen += 1;
        }
        i += 1;
    }
    if *end > i {
        *end = i;
        let mut j = *end;
        while j >= *start {
            *end = j;
            if prs.interesting(j as usize) {
                break;
            }
            if prs.counts(j as usize) {
                curlen -= 1;
            }
            j -= 1;
        }
    }
    (curlen, poslen)
}

/// `mark_hl_fragments`.
fn mark_hl_fragments(prs: &mut Prs, o: &HeadlineOptions, max_cover: usize) {
    let n = prs.words.len() as isize;
    let max_words = o.max_words;
    let mut covers: Vec<CoverPos> = Vec::new();
    let (mut p, mut q) = (0usize, 0usize);
    while prs.cover(max_cover, &mut p, &mut q) {
        let (mut start, mut end) = (p as isize, q as isize);
        while start <= end {
            let (curlen, poslen) = next_fragment(prs, &mut start, &mut end, max_words);
            covers.push(CoverPos {
                start,
                end,
                poslen,
                curlen,
                chosen: false,
                excluded: false,
            });
            start = end + 1;
            end = q as isize;
        }
        p += 1;
    }
    let mut num_f = 0;
    for _ in 0..o.max_fragments {
        let (mut maxitems, mut minwords, mut min_i) = (0, i32::MAX, None);
        for (i, c) in covers.iter().enumerate() {
            if !c.chosen
                && !c.excluded
                && (maxitems < c.poslen || (maxitems == c.poslen && minwords > c.curlen))
            {
                maxitems = c.poslen;
                minwords = c.curlen;
                min_i = Some(i);
            }
        }
        let Some(m) = min_i else {
            break;
        };
        covers[m].chosen = true;
        let (mut start, mut end, mut curlen) = (covers[m].start, covers[m].end, covers[m].curlen);
        if curlen < max_words {
            let maxstretch = (max_words - curlen) / 2;
            let mut stretch = 0;
            let mut posmarker = start;
            let mut i = start - 1;
            while i >= 0 && stretch < maxstretch && !prs.words[i as usize].in_frag {
                if prs.counts(i as usize) {
                    curlen += 1;
                    stretch += 1;
                }
                posmarker = i;
                i -= 1;
            }
            i = posmarker;
            while i < start && prs.bad_endpoint(i as usize) {
                if prs.counts(i as usize) {
                    curlen -= 1;
                }
                i += 1;
            }
            start = i;
            posmarker = end;
            i = end + 1;
            while i < n && curlen < max_words && !prs.words[i as usize].in_frag {
                if prs.counts(i as usize) {
                    curlen += 1;
                }
                posmarker = i;
                i += 1;
            }
            i = posmarker;
            while i > end && prs.bad_endpoint(i as usize) {
                if prs.counts(i as usize) {
                    curlen -= 1;
                }
                i -= 1;
            }
            end = i;
        }
        covers[m].start = start;
        covers[m].end = end;
        covers[m].curlen = curlen;
        prs.mark(o.highlight_all, start as usize, end);
        num_f += 1;
        for (i, c) in covers.iter_mut().enumerate() {
            if i != m
                && ((c.start >= start && c.start <= end)
                    || (c.end >= start && c.end <= end)
                    || (c.start < start && c.end > end))
            {
                c.excluded = true;
            }
        }
    }
    if num_f == 0 {
        let mut curlen = 0;
        let mut end = -1;
        let mut i = 0;
        while i < n && curlen < o.min_words {
            if prs.counts(i as usize) {
                curlen += 1;
            }
            end = i;
            i += 1;
        }
        prs.mark(o.highlight_all, 0, end);
    }
}

/// `ts_headline(config, document, query, options)`.
pub fn headline(cfg: Config, doc: &str, q: &Option<Query>, o: &HeadlineOptions) -> Result<String> {
    let mut prs = Prs::parse(cfg, doc, q)?;
    prs.short_word = o.short_word;
    let mut max_cover = (o.max_words.saturating_mul(10)).max(100);
    if o.max_fragments > 0 {
        max_cover = max_cover.saturating_mul(o.max_fragments);
    }
    let max_cover = max_cover.max(0) as usize;
    if o.max_fragments == 0 {
        mark_hl_words(&mut prs, o, max_cover);
    } else {
        mark_hl_fragments(&mut prs, o, max_cover);
    }
    // `generateHeadline`.
    let mut out = String::new();
    let (mut infrag, mut fragments) = (false, 0);
    for wd in &prs.words {
        if wd.in_frag && !wd.repeated {
            if !infrag {
                infrag = true;
                fragments += 1;
                if fragments > 1 {
                    out.push_str(&o.delimiter);
                }
            }
            if wd.replace {
                out.push(' ');
            } else if !wd.skip {
                if wd.selected {
                    out.push_str(&o.start);
                }
                out.push_str(&wd.text);
                if wd.selected {
                    out.push_str(&o.stop);
                }
            }
        } else if !wd.repeated {
            infrag = false;
        }
    }
    Ok(out)
}
