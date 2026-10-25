//! The `thesaurus` text search template: PostgreSQL 15's
//! `dict_thesaurus.c`, transcribed. A thesaurus file of phrases, each with
//! the phrase that replaces it, normalised through a subdictionary; the
//! dictionary is a multi-word one, asking for the next token
//! (`DictSubState.getnext`) while a phrase may still be matching.

use crate::ts_dicts::{DictState, TsLexeme};
use crate::{Error, Result};

/// `DT_USEASIS`: a substitute word written `*word`, not lexized.
const DT_USEASIS: u16 = 0x1000;

fn config_error(msg: impl Into<String>) -> Error {
    Error::Sqlstate("F0000", msg.into())
}

/// `LexemeInfo`: where a sample word stands in a rule.
#[derive(Debug, Clone)]
struct LexemeInfo {
    idsubst: u32,
    posinsubst: u16,
    /// How many lexemes the subdictionary gave this word's variant.
    tnvariant: u16,
    nextentry: Option<usize>,
}

/// `TheLexeme`: a sample word's lexeme (`None` for the `?` stop-word
/// marker) and the head of its entries.
#[derive(Debug, Clone)]
struct TheLexeme {
    lexeme: Option<String>,
    entries: usize,
}

/// `TheSubstitute`.
#[derive(Debug, Clone)]
struct TheSubstitute {
    lastlexeme: u16,
    res: Vec<TsLexeme>,
}

/// `DictThesaurus`.
#[derive(Debug)]
pub struct Thesaurus {
    subdict: i64,
    wrds: Vec<TheLexeme>,
    infos: Vec<LexemeInfo>,
    subst: Vec<TheSubstitute>,
}

/// `cmpLexeme`: the stop-word marker last.
fn cmp_lexeme(a: &Option<String>, b: &Option<String>) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (None, None) => Equal,
        (None, Some(_)) => Greater,
        (Some(_), None) => Less,
        (Some(a), Some(b)) => a.as_bytes().cmp(b.as_bytes()),
    }
}

impl Thesaurus {
    fn info_cmp(&self, a: usize, b: usize) -> std::cmp::Ordering {
        let (a, b) = (&self.infos[a], &self.infos[b]);
        (a.idsubst, a.posinsubst, a.tnvariant).cmp(&(b.idsubst, b.posinsubst, b.tnvariant))
    }

    /// `thesaurus_init`, after the options are read: `file` read
    /// (`thesaurusRead`), its words normalised through `subdict`.
    pub fn build(file: &str, subdict: i64) -> Result<Thesaurus> {
        // `thesaurusRead`: (word, idsubst, posinsubst) per sample word, and
        // each rule's substitute words.
        let mut samples: Vec<(String, u32, u16)> = Vec::new();
        let mut subst: Vec<TheSubstitute> = Vec::new();
        let space = |c: char| c.is_whitespace();
        let mut idsubst: u32 = 0;
        for line in file.split_inclusive('\n') {
            let trimmed = line.trim_start_matches(space);
            if trimmed.is_empty() || trimmed.starts_with(['#', '\n', '\r']) {
                continue;
            }
            #[derive(PartialEq)]
            enum S {
                WaitLex,
                InLex,
                WaitSubs,
                InSubs,
            }
            let mut state = S::WaitLex;
            let mut begin = 0;
            let mut posinsubst: u32 = 0;
            let mut nwrd: u32 = 0;
            let mut useasis = false;
            let mut words: Vec<TsLexeme> = Vec::new();
            for (i, c) in line.char_indices() {
                match state {
                    S::WaitLex => {
                        if c == ':' {
                            if posinsubst == 0 {
                                return Err(config_error("unexpected delimiter"));
                            }
                            state = S::WaitSubs;
                        } else if !space(c) {
                            begin = i;
                            state = S::InLex;
                        }
                    }
                    S::InLex => {
                        if c == ':' || space(c) {
                            samples.push((line[begin..i].to_string(), idsubst, posinsubst as u16));
                            posinsubst += 1;
                            state = if c == ':' { S::WaitSubs } else { S::WaitLex };
                        }
                    }
                    S::WaitSubs => {
                        if c == '*' || c == '\\' {
                            useasis = c == '*';
                            state = S::InSubs;
                            begin = i + c.len_utf8();
                        } else if !space(c) {
                            useasis = false;
                            begin = i;
                            state = S::InSubs;
                        }
                    }
                    S::InSubs => {
                        if space(c) {
                            if i == begin {
                                return Err(config_error("unexpected end of line or lexeme"));
                            }
                            words.push(TsLexeme {
                                lexeme: line[begin..i].to_string(),
                                nvariant: nwrd as u16,
                                flags: if useasis { DT_USEASIS } else { 0 },
                            });
                            nwrd += 1;
                            state = S::WaitSubs;
                        }
                    }
                }
            }
            if state == S::InSubs {
                if begin == line.len() {
                    return Err(config_error("unexpected end of line or lexeme"));
                }
                words.push(TsLexeme {
                    lexeme: line[begin..].to_string(),
                    nvariant: nwrd as u16,
                    flags: if useasis { DT_USEASIS } else { 0 },
                });
                nwrd += 1;
            }
            idsubst += 1;
            if nwrd == 0 || posinsubst == 0 {
                return Err(config_error("unexpected end of line"));
            }
            if nwrd > u32::from(u16::MAX) || posinsubst > u32::from(u16::MAX) {
                return Err(config_error("too many lexemes in thesaurus entry"));
            }
            subst.push(TheSubstitute {
                lastlexeme: (posinsubst - 1) as u16,
                res: words,
            });
        }
        let mut t = Thesaurus {
            subdict,
            wrds: Vec::new(),
            infos: Vec::new(),
            subst,
        };
        t.compile_lexemes(samples)?;
        t.compile_substitutes()?;
        Ok(t)
    }

    /// `compileTheLexeme`: every sample word through the subdictionary,
    /// sorted, equal lexemes' entries chained.
    fn compile_lexemes(&mut self, samples: Vec<(String, u32, u16)>) -> Result<()> {
        let add = |t: &mut Thesaurus, lexeme: Option<String>, idsubst, posinsubst, tnvariant| {
            t.infos.push(LexemeInfo {
                idsubst,
                posinsubst,
                tnvariant,
                nextentry: None,
            });
            t.wrds.push(TheLexeme {
                lexeme,
                entries: t.infos.len() - 1,
            });
        };
        for (word, idsubst, posinsubst) in samples {
            if word == "?" {
                // The stop-word marker.
                add(self, None, idsubst, posinsubst, 1);
                continue;
            }
            let rule = idsubst + 1;
            match crate::ts_dicts::lexize_oid(self.subdict, &word, None)? {
                None => {
                    return Err(config_error(format!(
                        "thesaurus sample word \"{word}\" isn't recognized by subdictionary (rule {rule})"
                    )))
                }
                Some(l) if l.is_empty() => {
                    return Err(config_error(format!(
                        "thesaurus sample word \"{word}\" is a stop word (rule {rule})"
                    )))
                }
                Some(l) => {
                    let mut k = 0;
                    while k < l.len() {
                        let cur = l[k].nvariant;
                        let mut end = k + 1;
                        while end < l.len() && l[end].nvariant == l[end - 1].nvariant {
                            end += 1;
                        }
                        let tnvar = (end - k) as u16;
                        while k < l.len() && l[k].nvariant == cur {
                            add(self, Some(l[k].lexeme.clone()), idsubst, posinsubst, tnvar);
                            k += 1;
                        }
                    }
                }
            }
        }
        if self.wrds.len() > 1 {
            let mut wrds = std::mem::take(&mut self.wrds);
            // `cmpTheLexeme`: by lexeme, then by entry, descending.
            crate::pg_qsort::pg_qsort(&mut wrds, &mut |a, b| {
                cmp_lexeme(&a.lexeme, &b.lexeme).then_with(|| self.info_cmp(b.entries, a.entries))
            });
            let mut out: Vec<TheLexeme> = Vec::with_capacity(wrds.len());
            for w in wrds {
                match out.last_mut() {
                    Some(last) if cmp_lexeme(&w.lexeme, &last.lexeme).is_eq() => {
                        if self.info_cmp(w.entries, last.entries).is_ne() {
                            self.infos[w.entries].nextentry = Some(last.entries);
                            last.entries = w.entries;
                        }
                    }
                    _ => out.push(w),
                }
            }
            self.wrds = out;
        }
        Ok(())
    }

    /// `compileTheSubstitute`: each rule's substitute words through the
    /// subdictionary (but a `*word`), a word after the first starting a new
    /// position.
    fn compile_substitutes(&mut self) -> Result<()> {
        for (i, s) in self.subst.iter_mut().enumerate() {
            let rule = i + 1;
            let mut out: Vec<TsLexeme> = Vec::new();
            for w in std::mem::take(&mut s.res) {
                let lexized = if w.flags & DT_USEASIS != 0 {
                    Some(vec![TsLexeme {
                        flags: 0,
                        ..w.clone()
                    }])
                } else {
                    crate::ts_dicts::lexize_oid(self.subdict, &w.lexeme, None)?
                };
                match lexized {
                    Some(l) if !l.is_empty() => {
                        let toset = (!out.is_empty()).then_some(out.len());
                        out.extend(l);
                        if let Some(t) = toset {
                            out[t].flags |= crate::ts_dicts::TSL_ADDPOS;
                        }
                    }
                    Some(_) => {
                        return Err(config_error(format!(
                            "thesaurus substitute word \"{}\" is a stop word (rule {rule})",
                            w.lexeme
                        )))
                    }
                    None => {
                        return Err(config_error(format!(
                            "thesaurus substitute word \"{}\" isn't recognized by subdictionary (rule {rule})",
                            w.lexeme
                        )))
                    }
                }
            }
            if out.is_empty() {
                return Err(config_error(format!(
                    "thesaurus substitute phrase is empty (rule {rule})"
                )));
            }
            s.res = out;
        }
        Ok(())
    }

    /// `findTheLexeme`: the head of a lexeme's entries.
    fn find_lexeme(&self, lexeme: Option<&str>) -> Option<usize> {
        let key = lexeme.map(str::to_string);
        self.wrds
            .binary_search_by(|w| cmp_lexeme(&w.lexeme, &key))
            .ok()
            .map(|i| self.wrds[i].entries)
    }

    /// `matchIdSubst`: an empty chain matches every rule.
    fn match_id(&self, chain: &[usize], idsubst: u32) -> bool {
        chain.is_empty() || chain.iter().any(|&i| self.infos[i].idsubst == idsubst)
    }

    /// `findVariant`: the rules in which every lexeme of a variant stands
    /// at `curpos`, prepended to `found`.
    fn find_variant(
        &self,
        mut found: Vec<usize>,
        stored: &[usize],
        curpos: u16,
        newin: &mut [Option<usize>],
    ) -> Vec<usize> {
        let newn = newin.len();
        let next = |i: usize| self.infos[i].nextentry;
        loop {
            let Some(mut ptr) = newin[0] else {
                return found;
            };
            let mut i = 0;
            while i < newn {
                while let Some(e) = newin[i] {
                    if self.infos[e].idsubst < self.infos[ptr].idsubst {
                        newin[i] = next(e);
                    } else {
                        break;
                    }
                }
                let Some(mut e) = newin[i] else {
                    return found;
                };
                if self.infos[e].idsubst > self.infos[ptr].idsubst {
                    ptr = e;
                    i = 0;
                    continue;
                }
                while self.infos[e].idsubst == self.infos[ptr].idsubst {
                    if self.infos[e].posinsubst == curpos
                        && usize::from(self.infos[e].tnvariant) == newn
                    {
                        ptr = e;
                        break;
                    }
                    match next(e) {
                        Some(n) => {
                            e = n;
                            newin[i] = Some(n);
                        }
                        None => return found,
                    }
                }
                if self.infos[e].idsubst != self.infos[ptr].idsubst {
                    ptr = e;
                    i = 0;
                    continue;
                }
                i += 1;
            }
            let id = self.infos[ptr].idsubst;
            if self.match_id(stored, id) && (found.is_empty() || !self.match_id(&found, id)) {
                found.insert(0, ptr);
            }
            // Step forward.
            for slot in newin.iter_mut() {
                *slot = slot.and_then(next);
            }
        }
    }

    /// `thesaurus_lexize`.
    pub fn lexize(&self, token: &str, state: &mut DictState) -> Result<Option<Vec<TsLexeme>>> {
        if state.isend {
            return Ok(None);
        }
        let stored = std::mem::take(&mut state.private);
        let curpos = stored
            .first()
            .map_or(0, |&s| self.infos[s].posinsubst.wrapping_add(1));
        let res = crate::ts_dicts::lexize_oid(self.subdict, token, None)?;
        let mut info: Vec<usize> = Vec::new();
        match res {
            Some(l) if !l.is_empty() => {
                let mut k = 0;
                while k < l.len() {
                    let nv = l[k].nvariant;
                    let base = k;
                    while k < l.len() && l[k].nvariant == nv {
                        k += 1;
                    }
                    let mut infos: Vec<Option<usize>> = Vec::with_capacity(k - base);
                    let mut all = true;
                    for x in &l[base..k] {
                        match self.find_lexeme(Some(&x.lexeme)) {
                            Some(e) => infos.push(Some(e)),
                            None => {
                                all = false;
                                break;
                            }
                        }
                    }
                    if all {
                        info = self.find_variant(info, &stored, curpos, &mut infos);
                    }
                }
            }
            Some(_) => {
                // A stop word.
                let mut infos = [self.find_lexeme(None)];
                info = self.find_variant(Vec::new(), &stored, curpos, &mut infos);
            }
            None => {}
        }
        if info.is_empty() {
            state.getnext = false;
            return Ok(None);
        }
        // `checkMatch`.
        let mut moreres = false;
        let mut matched = None;
        for (n, &e) in info.iter().enumerate() {
            if n + 1 < info.len() {
                moreres = true;
            }
            let s = &self.subst[self.infos[e].idsubst as usize];
            if s.lastlexeme == curpos {
                matched = Some(s.res.clone());
                break;
            }
        }
        state.private = info;
        match matched {
            Some(res) => {
                state.getnext = moreres;
                Ok(Some(res))
            }
            None => {
                state.getnext = true;
                Ok(None)
            }
        }
    }
}
