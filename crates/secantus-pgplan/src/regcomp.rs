//! `regcomp`'s refusals: PostgreSQL's regex parser (`regcomp.c`'s
//! `parse`, `parsebranch`, `parseqatom` and `brackpart`, over
//! `regc_lex.c`'s lexer and `prefixes()`), run in front of whichever engine
//! matches, so a regex PostgreSQL refuses is refused here with its error
//! code and `pg_regerror`'s text (RP-1123).
//!
//! The port keeps the parser's order of checks and drops everything that
//! builds the NFA: a regex is checked, not compiled. The engines
//! (`pg_regex`, `secantus-core`'s `$regex`) still run it; before this, a
//! regex they could not compile was 22P02 with the `regex` crate's message,
//! and one they read differently (`a{2,1}`) matched nothing.

use crate::{Error, Result};

/// `REG_EXTENDED`.
pub(crate) const EXTENDED: u32 = 0o1;
/// `REG_ADVF`: the ARE extensions.
const ADVF: u32 = 0o2;
/// `REG_ADVANCED`, an ARE: `REG_EXTENDED | REG_ADVF`.
pub(crate) const ADVANCED: u32 = EXTENDED | ADVF;
/// `REG_QUOTE`: a literal string.
const QUOTE: u32 = 0o4;
/// `REG_ICASE`.
pub(crate) const ICASE: u32 = 0o10;
/// `REG_NLSTOP`.
const NLSTOP: u32 = 0o100;
/// `REG_NLANCH`.
const NLANCH: u32 = 0o200;
/// `REG_NEWLINE`: `REG_NLSTOP | REG_NLANCH`.
const NEWLINE: u32 = NLSTOP | NLANCH;
/// `REG_EXPANDED`.
const EXPANDED: u32 = 0o40;

/// `_POSIX2_RE_DUP_MAX`, the largest bound.
const DUPMAX: u32 = 255;
/// `CHR_MAX`, the largest character an escape may name.
const CHR_MAX: u64 = 0x7fff_fffe;

/// A `regcomp` failure, as `regerrs.h` names and spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegError {
    BadPat,
    ECollate,
    ECtype,
    EEscape,
    ESubreg,
    EBrack,
    EParen,
    EBrace,
    BadBr,
    ERange,
    BadRpt,
    Assert,
    InvArg,
    BadOpt,
}

impl RegError {
    /// `pg_regerror`'s text.
    pub(crate) fn text(self) -> &'static str {
        match self {
            RegError::BadPat => "invalid regexp (reg version 0.8)",
            RegError::ECollate => "invalid collating element",
            RegError::ECtype => "invalid character class",
            RegError::EEscape => "invalid escape \\ sequence",
            RegError::ESubreg => "invalid backreference number",
            RegError::EBrack => "brackets [] not balanced",
            RegError::EParen => "parentheses () not balanced",
            RegError::EBrace => "braces {} not balanced",
            RegError::BadBr => "invalid repetition count(s)",
            RegError::ERange => "invalid character range",
            RegError::BadRpt => "quantifier operand invalid",
            RegError::Assert => "\"can't happen\" -- you found a bug",
            RegError::InvArg => "invalid argument to regex function",
            RegError::BadOpt => "invalid embedded option",
        }
    }
}

/// `RE_compile_and_cache`'s error: 2201B, "invalid regular expression: "
/// and `pg_regerror`'s text.
pub(crate) fn check(pattern: &str, cflags: u32) -> Result<()> {
    regcomp(pattern, cflags)
        .map_err(|e| Error::InvalidRegex(format!("invalid regular expression: {}", e.text())))
}

/// The compile flags `parse_re_flags` reads from a regex function's
/// options, letter by letter, as PostgreSQL does: `e` clears
/// `REG_ADVANCED`, which holds `REG_EXTENDED`, so it leaves a BRE
/// (measured on 15.20: `regexp_replace('a+', 'a+', 'b', 'e')` is `b`).
/// A letter that is no option is the caller's to refuse.
pub(crate) fn options_cflags(options: &str) -> u32 {
    let mut cflags = ADVANCED;
    for c in options.chars() {
        match c {
            'b' => cflags &= !(ADVANCED | QUOTE),
            'c' => cflags &= !ICASE,
            'e' => {
                cflags |= EXTENDED;
                cflags &= !(ADVANCED | QUOTE);
            }
            'i' => cflags |= ICASE,
            'm' | 'n' => cflags |= NEWLINE,
            'p' => {
                cflags |= NLSTOP;
                cflags &= !NLANCH;
            }
            'q' => {
                cflags |= QUOTE;
                cflags &= !ADVANCED;
            }
            's' => cflags &= !NEWLINE,
            't' => cflags &= !EXPANDED,
            'w' => {
                cflags &= !NLSTOP;
                cflags |= NLANCH;
            }
            'x' => cflags |= EXPANDED,
            _ => {}
        }
    }
    cflags
}

/// `jspConvertRegexFlags`: a jsonpath `like_regex` is an ARE, or with `q`
/// a literal string. Its other flags move only the newline handling, which
/// refuses nothing.
pub(crate) fn jsonpath_cflags(flags: &str) -> u32 {
    if flags.contains('q') {
        QUOTE
    } else {
        ADVANCED
    }
}

/// `pg_regcomp`'s answer for `pattern` under `cflags`: `Ok` for a regex it
/// compiles, its first error otherwise.
pub(crate) fn regcomp(pattern: &str, cflags: u32) -> std::result::Result<(), RegError> {
    if cflags & QUOTE != 0 && cflags & (ADVANCED | EXPANDED | NEWLINE) != 0 {
        return Err(RegError::InvArg);
    }
    if cflags & EXTENDED == 0 && cflags & ADVF != 0 {
        return Err(RegError::InvArg);
    }
    let mut v = Vars {
        chars: pattern.chars().collect(),
        now: 0,
        cflags,
        lexcon: Con::Ere,
        nexttype: Tok::Empty,
        lasttype: Tok::Empty,
        err: None,
        nsubexp: 0,
        closed: Vec::new(),
    };
    v.lexstart();
    if v.err.is_none() {
        v.parse(Stop::Eos, false);
    }
    match v.err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// A token, as `regc_lex.c` returns it (the type, and the value where the
/// checks read one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Empty,
    Eos,
    Plain(char),
    Digit(u32),
    Comma,
    Pipe,
    Star,
    Plus,
    Quest,
    LBrace,
    RBrace,
    /// `(`, capturing or not.
    LParen(bool),
    RParen,
    LBracket,
    RBracket,
    Range(char),
    Dot,
    Caret,
    Dollar,
    Collel,
    Eclass,
    Cclass,
    End,
    CclassS,
    CclassC,
    Backref(u32),
    SBegin,
    SEnd,
    WBdry,
    NWBdry,
    Lt,
    Gt,
    Lacon,
}

impl Tok {
    /// The type alone, as `SEE` and `LASTTYPE` compare tokens.
    fn is(self, other: Tok) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }
}

/// The lexer's context (`L_ERE`, `L_BRE`, `L_Q`, `L_EBND`, `L_BBND`,
/// `L_BRACK`, `L_CEL`, `L_ECL`, `L_CCL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Con {
    Ere,
    Bre,
    Q,
    Ebnd,
    Bbnd,
    Brack,
    Cel,
    Ecl,
    Ccl,
}

/// `parse`'s stopper: the end of the regex, or a `)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Eos,
    Paren,
}

struct Vars {
    chars: Vec<char>,
    now: usize,
    cflags: u32,
    lexcon: Con,
    nexttype: Tok,
    lasttype: Tok,
    err: Option<RegError>,
    /// Capturing groups opened so far (`v->nsubexp`).
    nsubexp: u32,
    /// Which groups have closed (`v->subs[n] != NULL`), by number.
    closed: Vec<bool>,
}

/// `pg_wc_isalpha`, `pg_wc_isspace` under the server's UTF-8 locale
/// (measured on 15.20 under `en_US.UTF-8`: `(?é)a` is an invalid embedded
/// option, and `(?x)` skips a no-break space).
fn isalpha(c: char) -> bool {
    c.is_alphabetic()
}

fn isspace(c: char) -> bool {
    c.is_whitespace()
}

/// `regc_locale.c`'s `cnames`: the collating elements a bracket may name.
const CNAMES: &[(&str, char)] = &[
    ("NUL", '\0'),
    ("SOH", '\u{1}'),
    ("STX", '\u{2}'),
    ("ETX", '\u{3}'),
    ("EOT", '\u{4}'),
    ("ENQ", '\u{5}'),
    ("ACK", '\u{6}'),
    ("BEL", '\u{7}'),
    ("alert", '\u{7}'),
    ("BS", '\u{8}'),
    ("backspace", '\u{8}'),
    ("HT", '\t'),
    ("tab", '\t'),
    ("LF", '\n'),
    ("newline", '\n'),
    ("VT", '\u{b}'),
    ("vertical-tab", '\u{b}'),
    ("FF", '\u{c}'),
    ("form-feed", '\u{c}'),
    ("CR", '\r'),
    ("carriage-return", '\r'),
    ("SO", '\u{e}'),
    ("SI", '\u{f}'),
    ("DLE", '\u{10}'),
    ("DC1", '\u{11}'),
    ("DC2", '\u{12}'),
    ("DC3", '\u{13}'),
    ("DC4", '\u{14}'),
    ("NAK", '\u{15}'),
    ("SYN", '\u{16}'),
    ("ETB", '\u{17}'),
    ("CAN", '\u{18}'),
    ("EM", '\u{19}'),
    ("SUB", '\u{1a}'),
    ("ESC", '\u{1b}'),
    ("IS4", '\u{1c}'),
    ("FS", '\u{1c}'),
    ("IS3", '\u{1d}'),
    ("GS", '\u{1d}'),
    ("IS2", '\u{1e}'),
    ("RS", '\u{1e}'),
    ("IS1", '\u{1f}'),
    ("US", '\u{1f}'),
    ("space", ' '),
    ("exclamation-mark", '!'),
    ("quotation-mark", '"'),
    ("number-sign", '#'),
    ("dollar-sign", '$'),
    ("percent-sign", '%'),
    ("ampersand", '&'),
    ("apostrophe", '\''),
    ("left-parenthesis", '('),
    ("right-parenthesis", ')'),
    ("asterisk", '*'),
    ("plus-sign", '+'),
    ("comma", ','),
    ("hyphen", '-'),
    ("hyphen-minus", '-'),
    ("period", '.'),
    ("full-stop", '.'),
    ("slash", '/'),
    ("solidus", '/'),
    ("zero", '0'),
    ("one", '1'),
    ("two", '2'),
    ("three", '3'),
    ("four", '4'),
    ("five", '5'),
    ("six", '6'),
    ("seven", '7'),
    ("eight", '8'),
    ("nine", '9'),
    ("colon", ':'),
    ("semicolon", ';'),
    ("less-than-sign", '<'),
    ("equals-sign", '='),
    ("greater-than-sign", '>'),
    ("question-mark", '?'),
    ("commercial-at", '@'),
    ("left-square-bracket", '['),
    ("backslash", '\\'),
    ("reverse-solidus", '\\'),
    ("right-square-bracket", ']'),
    ("circumflex", '^'),
    ("circumflex-accent", '^'),
    ("underscore", '_'),
    ("low-line", '_'),
    ("grave-accent", '`'),
    ("left-brace", '{'),
    ("left-curly-bracket", '{'),
    ("vertical-line", '|'),
    ("right-brace", '}'),
    ("right-curly-bracket", '}'),
    ("tilde", '~'),
    ("DEL", '\u{7f}'),
];

/// `regc_locale.c`'s `classNames`.
const CLASS_NAMES: &[&str] = &[
    "alnum", "alpha", "ascii", "blank", "cntrl", "digit", "graph", "lower", "print", "punct",
    "space", "upper", "xdigit", "word",
];

impl Vars {
    fn ateos(&self) -> bool {
        self.now >= self.chars.len()
    }

    fn have(&self, n: usize) -> bool {
        self.chars.len() - self.now.min(self.chars.len()) >= n
    }

    fn at(&self, off: usize) -> Option<char> {
        self.chars.get(self.now + off).copied()
    }

    fn next1(&self, c: char) -> bool {
        self.at(0) == Some(c)
    }

    /// `ERR`: the first error stands, and the token stream ends.
    fn error(&mut self, e: RegError) {
        self.nexttype = Tok::Eos;
        if self.err.is_none() {
            self.err = Some(e);
        }
    }

    fn see(&self, t: Tok) -> bool {
        self.nexttype.is(t)
    }

    fn see_stop(&self, stop: Stop) -> bool {
        match stop {
            Stop::Eos => self.see(Tok::Eos),
            Stop::Paren => self.see(Tok::RParen),
        }
    }

    fn eat(&mut self, t: Tok) -> bool {
        if self.see(t) {
            self.next();
            true
        } else {
            false
        }
    }

    /// `RETV`: the token, and success.
    fn ret(&mut self, t: Tok) -> bool {
        self.nexttype = t;
        true
    }

    /// `FAILW`.
    fn fail(&mut self, e: RegError) -> bool {
        self.error(e);
        false
    }

    // ---- regc_lex.c ----

    /// `lexstart`: the director and embedded options, then the first
    /// token.
    fn lexstart(&mut self) {
        self.prefixes();
        if self.err.is_some() {
            return;
        }
        self.lexcon = if self.cflags & QUOTE != 0 {
            Con::Q
        } else if self.cflags & EXTENDED != 0 {
            Con::Ere
        } else {
            Con::Bre
        };
        self.nexttype = Tok::Empty;
        self.next();
    }

    /// `prefixes`.
    fn prefixes(&mut self) {
        if self.cflags & QUOTE != 0 {
            return;
        }
        if self.have(4)
            && self.at(0) == Some('*')
            && self.at(1) == Some('*')
            && self.at(2) == Some('*')
        {
            match self.at(3) {
                Some('?') => {
                    self.error(RegError::BadPat);
                    return;
                }
                Some('=') => {
                    self.cflags |= QUOTE;
                    self.cflags &= !(ADVANCED | EXPANDED | NEWLINE);
                    self.now += 4;
                    return;
                }
                Some(':') => {
                    self.cflags |= ADVANCED;
                    self.now += 4;
                }
                _ => {
                    self.error(RegError::BadRpt);
                    return;
                }
            }
        }
        if self.cflags & ADVANCED != ADVANCED {
            return;
        }
        if self.have(3)
            && self.at(0) == Some('(')
            && self.at(1) == Some('?')
            && self.at(2).is_some_and(isalpha)
        {
            self.now += 2;
            while let Some(c) = self.at(0).filter(|c| isalpha(*c)) {
                match c {
                    'b' => self.cflags &= !(ADVANCED | QUOTE),
                    'c' => self.cflags &= !ICASE,
                    'e' => {
                        self.cflags |= EXTENDED;
                        self.cflags &= !(ADVF | QUOTE);
                    }
                    'i' => self.cflags |= ICASE,
                    'm' | 'n' => self.cflags |= NEWLINE,
                    'p' => {
                        self.cflags |= NLSTOP;
                        self.cflags &= !NLANCH;
                    }
                    'q' => {
                        self.cflags |= QUOTE;
                        self.cflags &= !ADVANCED;
                    }
                    's' => self.cflags &= !NEWLINE,
                    't' => self.cflags &= !EXPANDED,
                    'w' => {
                        self.cflags &= !NLSTOP;
                        self.cflags |= NLANCH;
                    }
                    'x' => self.cflags |= EXPANDED,
                    _ => {
                        self.error(RegError::BadOpt);
                        return;
                    }
                }
                self.now += 1;
            }
            if !self.next1(')') {
                self.error(RegError::BadOpt);
                return;
            }
            self.now += 1;
            if self.cflags & QUOTE != 0 {
                self.cflags &= !(EXPANDED | NEWLINE);
            }
        }
    }

    /// `skip`: an expanded regex's white space and `#` comments.
    fn skip(&mut self) {
        loop {
            while self.at(0).is_some_and(isspace) {
                self.now += 1;
            }
            if !self.next1('#') {
                break;
            }
            while self.at(0).is_some_and(|c| c != '\n') {
                self.now += 1;
            }
        }
    }

    /// `next`: lex one token into `nexttype`.
    fn next(&mut self) -> bool {
        loop {
            if self.err.is_some() {
                return false;
            }
            self.lasttype = self.nexttype;
            if self.cflags & EXPANDED != 0
                && matches!(self.lexcon, Con::Ere | Con::Bre | Con::Ebnd | Con::Bbnd)
            {
                self.skip();
            }
            if self.ateos() {
                return match self.lexcon {
                    Con::Ere | Con::Bre | Con::Q => self.ret(Tok::Eos),
                    Con::Ebnd | Con::Bbnd => self.fail(RegError::EBrace),
                    Con::Brack | Con::Cel | Con::Ecl | Con::Ccl => self.fail(RegError::EBrack),
                };
            }
            let c = self.chars[self.now];
            self.now += 1;
            match self.lexcon {
                Con::Bre => return self.brenext(c),
                Con::Ere => {}
                Con::Q => return self.ret(Tok::Plain(c)),
                Con::Ebnd | Con::Bbnd => {
                    return match c {
                        '0'..='9' => self.ret(Tok::Digit(c as u32 - '0' as u32)),
                        ',' => self.ret(Tok::Comma),
                        '}' if self.lexcon == Con::Ebnd => {
                            self.lexcon = Con::Ere;
                            if self.cflags & ADVF != 0 && self.next1('?') {
                                self.now += 1;
                            }
                            self.ret(Tok::RBrace)
                        }
                        '\\' if self.lexcon == Con::Bbnd && self.next1('}') => {
                            self.now += 1;
                            self.lexcon = Con::Bre;
                            self.ret(Tok::RBrace)
                        }
                        _ => self.fail(RegError::BadBr),
                    };
                }
                Con::Brack => return self.brack_next(c),
                Con::Cel | Con::Ecl | Con::Ccl => {
                    let close = match self.lexcon {
                        Con::Cel => '.',
                        Con::Ecl => '=',
                        _ => ':',
                    };
                    if c == close && self.next1(']') {
                        self.now += 1;
                        self.lexcon = Con::Brack;
                        return self.ret(Tok::End);
                    }
                    return self.ret(Tok::Plain(c));
                }
            }
            // An ERE or an ARE.
            let advf = self.cflags & ADVF != 0;
            return match c {
                '|' => self.ret(Tok::Pipe),
                '*' | '+' | '?' => {
                    if advf && self.next1('?') {
                        self.now += 1;
                    }
                    self.ret(match c {
                        '*' => Tok::Star,
                        '+' => Tok::Plus,
                        _ => Tok::Quest,
                    })
                }
                '{' => {
                    if self.cflags & EXPANDED != 0 {
                        self.skip();
                    }
                    if self.at(0).is_some_and(|d| d.is_ascii_digit()) {
                        self.lexcon = Con::Ebnd;
                        self.ret(Tok::LBrace)
                    } else {
                        self.ret(Tok::Plain(c))
                    }
                }
                '(' => {
                    if advf && self.next1('?') {
                        self.now += 1;
                        let Some(k) = self.at(0) else {
                            return self.fail(RegError::BadRpt);
                        };
                        self.now += 1;
                        match k {
                            ':' => self.ret(Tok::LParen(false)),
                            '#' => {
                                while self.at(0).is_some_and(|c| c != ')') {
                                    self.now += 1;
                                }
                                if !self.ateos() {
                                    self.now += 1;
                                }
                                // `goto next_restart`, with the last type
                                // kept.
                                self.nexttype = self.lasttype;
                                continue;
                            }
                            '=' | '!' => self.ret(Tok::Lacon),
                            '<' => {
                                let Some(k) = self.at(0) else {
                                    return self.fail(RegError::BadRpt);
                                };
                                self.now += 1;
                                match k {
                                    '=' | '!' => self.ret(Tok::Lacon),
                                    _ => self.fail(RegError::BadRpt),
                                }
                            }
                            _ => self.fail(RegError::BadRpt),
                        }
                    } else {
                        self.ret(Tok::LParen(true))
                    }
                }
                ')' => self.ret(Tok::RParen),
                '[' => {
                    if self.word_bracket() {
                        return true;
                    }
                    self.open_bracket()
                }
                '.' => self.ret(Tok::Dot),
                '^' => self.ret(Tok::Caret),
                '$' => self.ret(Tok::Dollar),
                '\\' => {
                    if self.ateos() {
                        return self.fail(RegError::EEscape);
                    }
                    if !advf {
                        let p = self.chars[self.now];
                        self.now += 1;
                        return self.ret(Tok::Plain(p));
                    }
                    self.lexescape()
                }
                _ => self.ret(Tok::Plain(c)),
            };
        }
    }

    /// `[[:<:]]` and `[[:>:]]`, the word boundaries, after a `[`.
    fn word_bracket(&mut self) -> bool {
        if self.have(6)
            && self.at(0) == Some('[')
            && self.at(1) == Some(':')
            && matches!(self.at(2), Some('<' | '>'))
            && self.at(3) == Some(':')
            && self.at(4) == Some(']')
            && self.at(5) == Some(']')
        {
            let lt = self.at(2) == Some('<');
            self.now += 6;
            return self.ret(if lt { Tok::Lt } else { Tok::Gt });
        }
        false
    }

    /// A bracket expression opens.
    fn open_bracket(&mut self) -> bool {
        self.lexcon = Con::Brack;
        if self.next1('^') {
            self.now += 1;
        }
        self.ret(Tok::LBracket)
    }

    /// The `L_BRACK` context.
    fn brack_next(&mut self, c: char) -> bool {
        match c {
            ']' => {
                if self.lasttype.is(Tok::LBracket) {
                    self.ret(Tok::Plain(c))
                } else {
                    self.lexcon = if self.cflags & EXTENDED != 0 {
                        Con::Ere
                    } else {
                        Con::Bre
                    };
                    self.ret(Tok::RBracket)
                }
            }
            '\\' => {
                if self.cflags & ADVF == 0 {
                    return self.ret(Tok::Plain(c));
                }
                if self.ateos() {
                    return self.fail(RegError::EEscape);
                }
                if !self.lexescape() {
                    return false;
                }
                match self.nexttype {
                    Tok::Plain(_) | Tok::CclassS | Tok::CclassC => true,
                    _ => self.fail(RegError::EEscape),
                }
            }
            '-' => {
                if self.lasttype.is(Tok::LBracket) || self.next1(']') {
                    self.ret(Tok::Plain(c))
                } else {
                    self.ret(Tok::Range(c))
                }
            }
            '[' => {
                let Some(k) = self.at(0) else {
                    return self.fail(RegError::EBrack);
                };
                self.now += 1;
                match k {
                    '.' => {
                        self.lexcon = Con::Cel;
                        self.ret(Tok::Collel)
                    }
                    '=' => {
                        self.lexcon = Con::Ecl;
                        self.ret(Tok::Eclass)
                    }
                    ':' => {
                        self.lexcon = Con::Ccl;
                        self.ret(Tok::Cclass)
                    }
                    _ => {
                        self.now -= 1;
                        self.ret(Tok::Plain(c))
                    }
                }
            }
            _ => self.ret(Tok::Plain(c)),
        }
    }

    /// `lexescape`: an ARE's backslash sequence, the backslash eaten.
    fn lexescape(&mut self) -> bool {
        let c = self.chars[self.now];
        self.now += 1;
        if !c.is_ascii_alphanumeric() {
            return self.ret(Tok::Plain(c));
        }
        match c {
            'a' => self.ret(Tok::Plain('\u{7}')),
            'A' => self.ret(Tok::SBegin),
            'b' => self.ret(Tok::Plain('\u{8}')),
            'B' => self.ret(Tok::Plain('\\')),
            'c' => {
                let Some(n) = self.at(0) else {
                    return self.fail(RegError::EEscape);
                };
                self.now += 1;
                self.ret(Tok::Plain(char::from_u32(n as u32 & 0o37).unwrap_or('\0')))
            }
            'd' | 's' | 'w' => self.ret(Tok::CclassS),
            'D' | 'S' | 'W' => self.ret(Tok::CclassC),
            'e' => self.ret(Tok::Plain('\u{1b}')),
            'f' => self.ret(Tok::Plain('\u{c}')),
            'm' => self.ret(Tok::Lt),
            'M' => self.ret(Tok::Gt),
            'n' => self.ret(Tok::Plain('\n')),
            'r' => self.ret(Tok::Plain('\r')),
            't' => self.ret(Tok::Plain('\t')),
            'v' => self.ret(Tok::Plain('\u{b}')),
            'u' | 'U' | 'x' => {
                let (min, max) = match c {
                    'u' => (4, 4),
                    'U' => (8, 8),
                    _ => (1, 255),
                };
                match self.lexdigits(16, min, max) {
                    Some(n) if n <= CHR_MAX => self.ret(Tok::Plain(
                        char::from_u32(n as u32).unwrap_or(char::REPLACEMENT_CHARACTER),
                    )),
                    _ => self.fail(RegError::EEscape),
                }
            }
            'y' => self.ret(Tok::WBdry),
            'Y' => self.ret(Tok::NWBdry),
            'Z' => self.ret(Tok::SEnd),
            '1'..='9' => {
                let save = self.now;
                self.now -= 1;
                let Some(n) = self.lexdigits(10, 1, 255) else {
                    return self.fail(RegError::EEscape);
                };
                if self.now == save || (n > 0 && n <= u64::from(self.nsubexp)) {
                    return self.ret(Tok::Backref(u32::try_from(n).unwrap_or(u32::MAX)));
                }
                // Not a back reference after all: an octal escape.
                self.now = save;
                self.octal()
            }
            '0' => self.octal(),
            // An unknown ASCII letter is reserved.
            _ => self.fail(RegError::EEscape),
        }
    }

    /// The octal escape whose first digit was just read.
    fn octal(&mut self) -> bool {
        self.now -= 1;
        let Some(mut n) = self.lexdigits(8, 1, 3) else {
            return self.fail(RegError::EEscape);
        };
        if n > 0xff {
            self.now -= 1;
            n >>= 3;
        }
        self.ret(Tok::Plain(char::from_u32(n as u32).unwrap_or('\0')))
    }

    /// `lexdigits`: `None` (`REG_EESCAPE`) for fewer than `minlen` digits.
    /// The value wraps as `uchr` does.
    fn lexdigits(&mut self, base: u32, minlen: usize, maxlen: usize) -> Option<u64> {
        let mut n: u32 = 0;
        let mut len = 0;
        while len < maxlen && !self.ateos() {
            let c = self.chars[self.now];
            self.now += 1;
            let d = match c {
                '0'..='9' => c as u32 - '0' as u32,
                'a'..='f' => c as u32 - 'a' as u32 + 10,
                'A'..='F' => c as u32 - 'A' as u32 + 10,
                _ => u32::MAX,
            };
            if d >= base {
                self.now -= 1;
                break;
            }
            n = n.wrapping_mul(base).wrapping_add(d);
            len += 1;
        }
        (len >= minlen).then_some(u64::from(n))
    }

    /// `brenext`: a BRE's token.
    fn brenext(&mut self, c: char) -> bool {
        match c {
            '*' => {
                if self.lasttype.is(Tok::Empty)
                    || self.lasttype.is(Tok::LParen(true))
                    || self.lasttype.is(Tok::Caret)
                {
                    return self.ret(Tok::Plain(c));
                }
                return self.ret(Tok::Star);
            }
            '[' => {
                if self.word_bracket() {
                    return true;
                }
                return self.open_bracket();
            }
            '.' => return self.ret(Tok::Dot),
            '^' => {
                if self.lasttype.is(Tok::Empty) || self.lasttype.is(Tok::LParen(true)) {
                    return self.ret(Tok::Caret);
                }
                return self.ret(Tok::Plain(c));
            }
            '$' => {
                if self.cflags & EXPANDED != 0 {
                    self.skip();
                }
                if self.ateos() || (self.at(0) == Some('\\') && self.at(1) == Some(')')) {
                    return self.ret(Tok::Dollar);
                }
                return self.ret(Tok::Plain(c));
            }
            '\\' => {}
            _ => return self.ret(Tok::Plain(c)),
        }
        let Some(c) = self.at(0) else {
            return self.fail(RegError::EEscape);
        };
        self.now += 1;
        match c {
            '{' => {
                self.lexcon = Con::Bbnd;
                self.ret(Tok::LBrace)
            }
            '(' => self.ret(Tok::LParen(true)),
            ')' => self.ret(Tok::RParen),
            '<' => self.ret(Tok::Lt),
            '>' => self.ret(Tok::Gt),
            '1'..='9' => self.ret(Tok::Backref(c as u32 - '0' as u32)),
            _ => self.ret(Tok::Plain(c)),
        }
    }

    // ---- regcomp.c ----

    /// `parse`: alternatives up to `stopper`.
    fn parse(&mut self, stopper: Stop, lacon: bool) {
        loop {
            self.parsebranch(stopper, lacon);
            if self.err.is_some() {
                return;
            }
            if !self.eat(Tok::Pipe) {
                break;
            }
        }
        if !self.see_stop(stopper) {
            // A `)` the regex never reaches.
            self.error(RegError::EParen);
        }
    }

    /// `parsebranch`: one alternative.
    fn parsebranch(&mut self, stopper: Stop, lacon: bool) {
        while !self.see(Tok::Pipe) && !self.see_stop(stopper) && !self.see(Tok::Eos) {
            self.parseqatom(lacon);
            if self.err.is_some() {
                return;
            }
        }
    }

    /// `parseqatom`: an atom or constraint, and its quantifier.
    fn parseqatom(&mut self, lacon: bool) {
        match self.nexttype {
            // Constraints take no quantifier.
            Tok::Caret
            | Tok::Dollar
            | Tok::SBegin
            | Tok::SEnd
            | Tok::Lt
            | Tok::Gt
            | Tok::WBdry
            | Tok::NWBdry => {
                self.next();
                return;
            }
            Tok::Lacon => {
                self.next();
                self.parse(Stop::Paren, true);
                if self.err.is_some() {
                    return;
                }
                self.next();
                return;
            }
            Tok::Star | Tok::Plus | Tok::Quest | Tok::LBrace => {
                self.error(RegError::BadRpt);
                return;
            }
            Tok::RParen => {
                // Legal in an ERE, "due to specification botch".
                if self.cflags & ADVANCED != EXTENDED {
                    self.error(RegError::EParen);
                    return;
                }
                self.next();
            }
            Tok::Plain(_) | Tok::CclassS | Tok::CclassC | Tok::Dot => {
                self.next();
            }
            Tok::LBracket => {
                self.bracket();
                if self.err.is_some() {
                    return;
                }
                self.next();
            }
            Tok::LParen(cap) => {
                // A lookaround's groups do not capture.
                let cap = cap && !lacon;
                let subno = if cap {
                    self.nsubexp += 1;
                    Some(self.nsubexp as usize)
                } else {
                    None
                };
                self.next();
                self.parse(Stop::Paren, lacon);
                if self.err.is_some() {
                    return;
                }
                self.next();
                if self.err.is_some() {
                    return;
                }
                if let Some(n) = subno {
                    if self.closed.len() <= n {
                        self.closed.resize(n + 1, false);
                    }
                    self.closed[n] = true;
                }
            }
            Tok::Backref(n) => {
                if lacon || !self.closed.get(n as usize).copied().unwrap_or(false) {
                    self.error(RegError::ESubreg);
                    return;
                }
                self.next();
            }
            _ => {
                self.error(RegError::Assert);
                return;
            }
        }
        if self.err.is_some() {
            return;
        }
        // The quantifier.
        match self.nexttype {
            Tok::Star | Tok::Plus | Tok::Quest => {
                self.next();
            }
            Tok::LBrace => {
                self.next();
                let m = self.scannum();
                if self.eat(Tok::Comma) {
                    let n = if self.see(Tok::Digit(0)) {
                        self.scannum()
                    } else {
                        u32::MAX
                    };
                    if m > n {
                        self.error(RegError::BadBr);
                        return;
                    }
                }
                if !self.see(Tok::RBrace) {
                    self.error(RegError::BadBr);
                    return;
                }
                self.next();
            }
            _ => {}
        }
    }

    /// `scannum`: a bound, at most `DUPMAX`.
    fn scannum(&mut self) -> u32 {
        let mut n = 0;
        while let Tok::Digit(d) = self.nexttype {
            if n >= DUPMAX {
                break;
            }
            n = n * 10 + d;
            self.next();
        }
        if self.see(Tok::Digit(0)) || n > DUPMAX {
            self.error(RegError::BadBr);
            return 0;
        }
        n
    }

    /// `bracket`: the parts of a bracket expression, up to its `]`.
    fn bracket(&mut self) {
        self.next();
        while !self.see(Tok::RBracket) && !self.see(Tok::Eos) {
            self.brackpart();
        }
    }

    /// `brackpart`: one character, range, class or element.
    fn brackpart(&mut self) {
        let startc = match self.nexttype {
            Tok::Range(_) => {
                self.error(RegError::ERange);
                return;
            }
            Tok::Plain(c) => {
                self.next();
                if !self.see(Tok::Range('-')) {
                    return;
                }
                c
            }
            Tok::Collel => match self.scanplain(RegError::ECollate) {
                Some(name) => match element(&name) {
                    Some(c) => c,
                    None => {
                        self.error(RegError::ECollate);
                        return;
                    }
                },
                None => return,
            },
            Tok::Eclass => {
                if let Some(name) = self.scanplain(RegError::ECollate) {
                    if element(&name).is_none() {
                        self.error(RegError::ECollate);
                    }
                }
                return;
            }
            Tok::Cclass => {
                if let Some(name) = self.scanplain(RegError::ECtype) {
                    if !CLASS_NAMES.contains(&name.as_str()) {
                        self.error(RegError::ECtype);
                    }
                }
                return;
            }
            Tok::CclassS | Tok::CclassC => {
                self.next();
                return;
            }
            _ => {
                self.error(RegError::Assert);
                return;
            }
        };
        let endc = if self.eat(Tok::Range('-')) {
            match self.nexttype {
                Tok::Plain(c) | Tok::Range(c) => {
                    self.next();
                    c
                }
                Tok::Collel => match self.scanplain(RegError::ECollate) {
                    Some(name) => match element(&name) {
                        Some(c) => c,
                        None => {
                            self.error(RegError::ECollate);
                            return;
                        }
                    },
                    None => return,
                },
                _ => {
                    self.error(RegError::ERange);
                    return;
                }
            }
        } else {
            startc
        };
        if startc > endc {
            self.error(RegError::ERange);
        }
    }

    /// `scanplain`: the name inside `[. .]`, `[= =]` or `[: :]`; `None`
    /// after an error (`empty` for an empty name).
    fn scanplain(&mut self, empty: RegError) -> Option<String> {
        self.next();
        let mut name = String::new();
        while let Tok::Plain(c) = self.nexttype {
            name.push(c);
            self.next();
        }
        if self.err.is_some() {
            return None;
        }
        self.next();
        if self.err.is_some() {
            return None;
        }
        if name.is_empty() {
            self.error(empty);
            return None;
        }
        Some(name)
    }
}

/// `element`: a collating element's character, a single character or a
/// name from `cnames`.
fn element(name: &str) -> Option<char> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(c);
    }
    CNAMES.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn are(p: &str) -> std::result::Result<(), RegError> {
        regcomp(p, ADVANCED)
    }

    /// Each answer measured on PostgreSQL 15.20 (`select 'a' ~ p`).
    #[test]
    fn refusals_match_postgres() {
        use RegError::*;
        let cases: &[(&str, Option<RegError>)] = &[
            ("(", Some(EParen)),
            (")", Some(EParen)),
            ("a)", Some(EParen)),
            ("(?z)a", Some(BadOpt)),
            ("a{2,1}", Some(BadBr)),
            ("a{256}", Some(BadBr)),
            ("a{255}", None),
            ("a{1", Some(EBrace)),
            ("a{1x}", Some(BadBr)),
            ("a{1,2,3}", Some(BadBr)),
            ("a{,2}", None),
            ("*a", Some(BadRpt)),
            ("a**", Some(BadRpt)),
            ("^*", Some(BadRpt)),
            ("a|*", Some(BadRpt)),
            ("(*)", Some(BadRpt)),
            ("(?=a)*", Some(BadRpt)),
            ("$*", Some(BadRpt)),
            (r"\y*", Some(BadRpt)),
            ("[", Some(EBrack)),
            ("[a", Some(EBrack)),
            ("[]", Some(EBrack)),
            ("[]a]", None),
            ("[^]", Some(EBrack)),
            ("[z-a]", Some(ERange)),
            (r"[a-\d]", Some(ERange)),
            (r"[\d-a]", Some(ERange)),
            ("[[:foo:]]", Some(ECtype)),
            ("[[:alpha:]]", None),
            ("[[:word:]]", None),
            ("[[::]]", Some(ECtype)),
            ("[[.a.]]", None),
            ("[[.ab.]]", Some(ECollate)),
            ("[[.space.]]", None),
            ("[[.hyphen.]-z]", None),
            ("[[=a=]]", None),
            ("[[=ab=]]", Some(ECollate)),
            (r"\1", Some(ESubreg)),
            (r"(a)\1", None),
            (r"(a\1)", Some(ESubreg)),
            (r"(?=\1)", Some(ESubreg)),
            (r"(a)(?=\1)", Some(ESubreg)),
            (r"\12", None),
            (r"[\1]", Some(EEscape)),
            (r"[\A]", Some(EEscape)),
            (r"[\d]", None),
            (r"\q", Some(EEscape)),
            (r"\z", Some(EEscape)),
            ("\\", Some(EEscape)),
            (r"a\\", None),
            (r"\x", Some(EEscape)),
            (r"\xg", Some(EEscape)),
            (r"\u12", Some(EEscape)),
            (r"\c", Some(EEscape)),
            ("***?", Some(BadPat)),
            ("***x", Some(BadRpt)),
            ("***", Some(BadRpt)),
            ("***=(", None),
            ("***:(", Some(EParen)),
            ("(?", Some(BadRpt)),
            ("(?x", Some(BadOpt)),
            ("(?i", Some(BadOpt)),
            ("(?<", Some(BadRpt)),
            ("(?<x)", Some(BadRpt)),
            ("(?y)", Some(BadOpt)),
            ("(?é)a", Some(BadOpt)),
            ("(?:", Some(EParen)),
            ("(?#abc", None),
            ("(?#abc)(", Some(EParen)),
            ("a{1}{2}", Some(BadRpt)),
            ("a{1}?", None),
            ("a*?", None),
            ("a+?+", Some(BadRpt)),
            ("(?e)a)", None),
            (r"(?e)\d", None),
            ("(?e)(?:a)", Some(BadRpt)),
            ("(?e)(", Some(EParen)),
            ("(?e)a{1", Some(EBrace)),
            (r"(?b)\(", Some(EParen)),
            (r"(?b)a\{1", Some(EBrace)),
            ("(?b)*a", None),
            (r"(?b)\(*a\)", None),
            (r"(?b)a\{,2\}", None),
            (r"(?b)\{1\}", Some(BadRpt)),
            (r"(?b)a\)", Some(EParen)),
            (r"(?b)\1", Some(ESubreg)),
            (r"(?b)\(a\)\1", None),
            ("(?q)(", None),
            ("(?x)a{ 1 }", None),
            ("(?x)a{1 }", None),
            ("(?x)a\u{a0}", None),
            ("[[:<:]]a", None),
            ("[[:<:]]*", Some(BadRpt)),
            (r"\m*", Some(BadRpt)),
            (r"a\Z*", Some(BadRpt)),
            (r"\A*", Some(BadRpt)),
            ("(?x)a #(", None),
            ("[a-]", None),
            ("[a-a]", None),
            ("[--a]", None),
            ("[a--]", Some(ERange)),
            ("[%--]", None),
            (r"\x110000", None),
            (r"\U00110000", None),
            (r"\UFFFFFFFF", Some(EEscape)),
            (r"[\x]", Some(EEscape)),
            ("[[.]", Some(EBrack)),
            ("[[=", Some(EBrack)),
            ("[[:alpha:", Some(EBrack)),
            ("(a|)", None),
            ("|", None),
            ("()", None),
        ];
        for (p, want) in cases {
            assert_eq!(are(p).err(), *want, "{p}");
        }
    }

    /// `parse_re_flags`' options (15.20, `regexp_replace`).
    #[test]
    fn function_options() {
        let with = |p: &str, o: &str| regcomp(p, options_cflags(o)).err();
        assert_eq!(with("a", "qx"), Some(RegError::InvArg));
        assert_eq!(with("a", "nq"), Some(RegError::InvArg));
        assert_eq!(with("(", "q"), None);
        // `e` leaves a BRE, where `(` is itself.
        assert_eq!(with("(", "e"), None);
        assert_eq!(with(r"\(", "e"), Some(RegError::EParen));
        assert_eq!(with(r"a\{1", "e"), Some(RegError::EBrace));
        assert_eq!(with("(?z)", "e"), None);
        assert_eq!(with("(", "qe"), None);
        assert_eq!(with("***:(", "e"), Some(RegError::EParen));
        assert_eq!(with("***=(", "e"), None);
        assert_eq!(with("(?z)", ""), Some(RegError::BadOpt));
    }
}
