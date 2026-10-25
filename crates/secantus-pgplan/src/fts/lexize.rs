//! `ts_parse.c`'s `LexizeExec`: a document's tokens through the
//! configuration's dictionaries, one token at a time, or several when a
//! multi-word dictionary (a thesaurus) asks for the next. PostgreSQL 15's
//! state machine, transcribed: `parsetext` (`to_tsvector`, the tsquery
//! builders) and `hlparsetext` (`ts_headline`) both drive it.

use std::collections::VecDeque;

use super::Config;
use crate::ts_dicts::{lexize_oid, DictState, TsLexeme, TSL_FILTER};
use crate::Result;

/// `ParsedLex`: a token and its type (0 at the end of the text).
#[derive(Debug, Clone)]
pub(super) struct ParsedLex {
    pub ty: u8,
    pub lemm: String,
}

/// `LexizeData`.
pub(super) struct Lexizer {
    cfg: Config,
    cur_dict: Option<i64>,
    pos_dict: usize,
    state: DictState,
    /// `towork`, each token with its sequence number.
    towork: VecDeque<(usize, ParsedLex)>,
    next_id: usize,
    cur_sub: Option<usize>,
    waste: Vec<ParsedLex>,
    last_res: Option<usize>,
    tmp_res: Option<Vec<TsLexeme>>,
}

impl Lexizer {
    /// `LexizeInit`.
    pub(super) fn new(cfg: Config) -> Self {
        Lexizer {
            cfg,
            cur_dict: None,
            pos_dict: 0,
            state: DictState::default(),
            towork: VecDeque::new(),
            next_id: 0,
            cur_sub: None,
            waste: Vec::new(),
            last_res: None,
            tmp_res: None,
        }
    }

    fn map(&self, ty: u8) -> Vec<i64> {
        if ty == 0 {
            return Vec::new();
        }
        crate::ts_catalog::dictionaries_for(self.cfg.0, i32::from(ty))
    }

    fn get(&self, id: usize) -> Option<&ParsedLex> {
        let front = self.towork.front()?.0;
        self.towork.get(id.checked_sub(front)?).map(|(_, l)| l)
    }

    /// The token after `id`, as the list stands now.
    fn next(&self, id: usize) -> Option<usize> {
        self.get(id + 1).map(|_| id + 1)
    }

    /// `LexizeAddLemm`.
    pub(super) fn add(&mut self, ty: u8, lemm: String) {
        let id = self.next_id;
        self.next_id += 1;
        self.towork.push_back((id, ParsedLex { ty, lemm }));
        self.cur_sub = Some(id);
    }

    /// `RemoveHead`.
    fn remove_head(&mut self) {
        if let Some((_, l)) = self.towork.pop_front() {
            self.waste.push(l);
        }
        self.pos_dict = 0;
    }

    /// `moveToWaste`: every token up to and including `stop`.
    fn move_to_waste(&mut self, stop: Option<usize>) {
        while let Some(&(id, _)) = self.towork.front() {
            if Some(id) == stop {
                self.cur_sub = self.next(id);
                self.remove_head();
                break;
            }
            self.remove_head();
        }
    }

    /// `setNewTmpRes`.
    fn set_tmp_res(&mut self, lex: usize, res: Vec<TsLexeme>) {
        self.tmp_res = Some(res);
        self.last_res = Some(lex);
    }

    /// `LexizeExec`: the next lexemes the tokens so far give (`None` when
    /// they give none yet), and the tokens they used up (`setCorrLex`).
    pub(super) fn exec(&mut self) -> Result<(Option<Vec<TsLexeme>>, Vec<ParsedLex>)> {
        loop {
            match self.cur_dict {
                None => {
                    // One word at a time, through each dictionary of its
                    // type's map in turn.
                    'tokens: while let Some((id, cur)) = self.towork.front().cloned() {
                        let map = self.map(cur.ty);
                        if map.is_empty() {
                            // A type no dictionary takes.
                            self.remove_head();
                            continue;
                        }
                        let mut lemm = cur.lemm.clone();
                        let mut i = self.pos_dict;
                        while i < map.len() {
                            self.state = DictState::default();
                            let res = lexize_oid(map[i], &lemm, Some(&mut self.state))?;
                            if self.state.getnext {
                                // The dictionary wants the next word: go to
                                // the multi-word mode, keeping where it
                                // started.
                                self.cur_dict = Some(map[i]);
                                self.pos_dict = i + 1;
                                self.cur_sub = self.next(id);
                                if let Some(r) = res {
                                    self.set_tmp_res(id, r);
                                }
                                break 'tokens;
                            }
                            let Some(res) = res else {
                                // It does not know the word.
                                i += 1;
                                continue;
                            };
                            if res.first().is_some_and(|l| l.flags & TSL_FILTER != 0) {
                                lemm = res[0].lexeme.clone();
                                i += 1;
                                continue;
                            }
                            self.remove_head();
                            return Ok((Some(res), std::mem::take(&mut self.waste)));
                        }
                        self.remove_head();
                    }
                    if self.cur_dict.is_none() {
                        return Ok((None, std::mem::take(&mut self.waste)));
                    }
                }
                Some(dict) => {
                    // The dictionary `dict` asks for the following words.
                    let mut redo = false;
                    while let Some(sub) = self.cur_sub {
                        let Some(cur) = self.get(sub).cloned() else {
                            break;
                        };
                        if cur.ty != 0 {
                            let map = self.map(cur.ty);
                            if map.is_empty() {
                                self.cur_sub = self.next(sub);
                                continue;
                            }
                            if !map.contains(&dict) {
                                // It cannot take this type: back to one word
                                // at a time, from the first stored token.
                                self.cur_dict = None;
                                redo = true;
                                break;
                            }
                        }
                        self.state.isend = cur.ty == 0;
                        self.state.getnext = false;
                        let res = lexize_oid(dict, &cur.lemm, Some(&mut self.state))?;
                        if self.state.getnext {
                            // One more.
                            self.cur_sub = self.next(sub);
                            if let Some(r) = res {
                                self.set_tmp_res(sub, r);
                            }
                            continue;
                        }
                        if res.is_some() || self.tmp_res.is_some() {
                            // The words it used are done; back to one word at
                            // a time for the rest.
                            let res = match res {
                                Some(r) => {
                                    self.move_to_waste(Some(sub));
                                    r
                                }
                                None => {
                                    let r = self.tmp_res.take().unwrap_or_default();
                                    let last = self.last_res;
                                    self.move_to_waste(last);
                                    r
                                }
                            };
                            self.cur_dict = None;
                            self.pos_dict = 0;
                            self.last_res = None;
                            self.tmp_res = None;
                            return Ok((Some(res), std::mem::take(&mut self.waste)));
                        }
                        // It wants no more and recognised nothing: redo from
                        // the first stored token.
                        self.cur_dict = None;
                        redo = true;
                        break;
                    }
                    if !redo {
                        return Ok((None, std::mem::take(&mut self.waste)));
                    }
                }
            }
        }
    }
}
