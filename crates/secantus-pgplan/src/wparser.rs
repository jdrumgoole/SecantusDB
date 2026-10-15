//! PostgreSQL's default text-search parser, `wparser_def.c`, transcribed:
//! the same states, the same action tables in the same order, and the same
//! driver loop (`TParserGet`), so `ts_parse` and `ts_debug` split a document
//! into exactly the tokens PostgreSQL 15 does and give each the same type.
//!
//! The parser works on characters, as the C does under a multibyte encoding
//! (`usewide`); a character's byte length only matters where the C asks for
//! a one-byte character (`p_iseq`, `p_isascii`, `p_isurlchar`).

use std::sync::OnceLock;

mod states;
use states::*;

/// Token types, `wparser_def.c`'s numbering.
pub const ASCIIWORD: u8 = 1;
pub const WORD_T: u8 = 2;
pub const NUMWORD: u8 = 3;
pub const EMAIL: u8 = 4;
pub const URL_T: u8 = 5;
pub const HOST: u8 = 6;
pub const SCIENTIFIC: u8 = 7;
pub const VERSIONNUMBER: u8 = 8;
pub const NUMPARTHWORD: u8 = 9;
pub const PARTHWORD: u8 = 10;
pub const ASCIIPARTHWORD: u8 = 11;
pub const SPACE: u8 = 12;
pub const TAG_T: u8 = 13;
pub const PROTOCOL: u8 = 14;
pub const NUMHWORD: u8 = 15;
pub const ASCIIHWORD: u8 = 16;
pub const HWORD: u8 = 17;
pub const URLPATH: u8 = 18;
pub const FILEPATH: u8 = 19;
pub const DECIMAL_T: u8 = 20;
pub const SIGNEDINT: u8 = 21;
pub const UNSIGNEDINT: u8 = 22;
pub const XMLENTITY: u8 = 23;

/// `tok_alias` / `lex_descr`, indexed by token type - 1: what
/// `ts_token_type` lists.
pub const TOKEN_TYPES: [(&str, &str); 23] = [
    ("asciiword", "Word, all ASCII"),
    ("word", "Word, all letters"),
    ("numword", "Word, letters and digits"),
    ("email", "Email address"),
    ("url", "URL"),
    ("host", "Host"),
    ("sfloat", "Scientific notation"),
    ("version", "Version number"),
    ("hword_numpart", "Hyphenated word part, letters and digits"),
    ("hword_part", "Hyphenated word part, all letters"),
    ("hword_asciipart", "Hyphenated word part, all ASCII"),
    ("blank", "Space symbols"),
    ("tag", "XML tag"),
    ("protocol", "Protocol head"),
    ("numhword", "Hyphenated word, letters and digits"),
    ("asciihword", "Hyphenated word, all ASCII"),
    ("hword", "Hyphenated word, all letters"),
    ("url_path", "URL path"),
    ("file", "File or path name"),
    ("float", "Decimal notation"),
    ("int", "Signed integer"),
    ("uint", "Unsigned integer"),
    ("entity", "XML entity"),
];

/// The default parser's oid, `pg_ts_parser.oid` for `default`.
pub const DEFAULT_PARSER_OID: i64 = 3722;

/// Every token of `text`, in order, with its type -- what
/// `ts_parse('default', text)` returns.
pub fn parse(text: &str) -> Vec<(u8, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut p = Parser::new(&chars);
    let mut out = Vec::new();
    while p.get() {
        let tok: String = chars[p.token..p.token + p.lentoken].iter().collect();
        out.push((p.ty, tok));
    }
    out
}

// ------------------------------------------------------------------------
// Character classes
// ------------------------------------------------------------------------

/// A table of inclusive code point ranges, one `XXXX YYYY` per line.
fn ranges(text: &'static str) -> Vec<(u32, u32)> {
    text.lines()
        .filter_map(|l| {
            let (a, b) = l.split_once(' ')?;
            Some((
                u32::from_str_radix(a, 16).ok()?,
                u32::from_str_radix(b, 16).ok()?,
            ))
        })
        .collect()
}

fn in_ranges(table: &[(u32, u32)], cp: u32) -> bool {
    table
        .binary_search_by(|(lo, hi)| {
            if *hi < cp {
                std::cmp::Ordering::Less
            } else if *lo > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `iswalpha` of a non-ASCII character in the reference server's UTF-8
/// locale: the Unicode 13 letters (general category L*), measured by
/// running every BMP code point through `ts_parse` in several contexts.
/// Not Unicode's wider Alphabetic property: a combining mark, a letter
/// number (`ᛮ`) and a circled letter (`Ⓐ`) are not letters to it, nor is a
/// letter added after Unicode 13. ASCII goes through the C `is*` classes.
fn is_alpha(c: char) -> bool {
    static LETTERS: OnceLock<Vec<(u32, u32)>> = OnceLock::new();
    if c.is_ascii() {
        return c.is_ascii_alphabetic();
    }
    in_ranges(
        LETTERS.get_or_init(|| ranges(include_str!("tsearch_data/letters.txt"))),
        c as u32,
    )
}

/// `iswdigit`: ASCII digits only.
fn is_digit(c: char) -> bool {
    c.is_ascii_digit()
}

/// `iswalnum`: a letter, or a decimal digit of any script (general
/// category Nd -- `٠` is alnum but not a digit, measured), which is why a
/// non-ASCII digit ends a run of blanks without starting a number.
fn is_alnum(c: char) -> bool {
    static DIGITS: OnceLock<Vec<(u32, u32)>> = OnceLock::new();
    is_alpha(c)
        || is_digit(c)
        || (!c.is_ascii()
            && in_ranges(
                DIGITS.get_or_init(|| ranges(include_str!("tsearch_data/digits.txt"))),
                c as u32,
            ))
}

fn is_space(c: char) -> bool {
    if c.is_ascii() {
        matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')
    } else {
        c.is_whitespace()
    }
}

/// `p_isspecial`: a character of zero display width (`pg_dsplen` is 0 --
/// NUL or one of `ucs_wcwidth`'s nonspacing ranges, PostgreSQL 15's
/// `unicode_combining_table.h`), or one of the spacing combining marks
/// `wparser_def.c` lists as `strange_letter`. Neither breaks a word.
fn is_special(c: char) -> bool {
    static NONSPACING: OnceLock<Vec<(u32, u32)>> = OnceLock::new();
    static STRANGE: OnceLock<Vec<u32>> = OnceLock::new();
    let cp = c as u32;
    if cp == 0 {
        return true;
    }
    // `ucs_wcwidth` answers -1 for a control character before it looks.
    let control = cp < 0x20 || (0x7f..0xa0).contains(&cp);
    if !control
        && in_ranges(
            NONSPACING.get_or_init(|| ranges(include_str!("tsearch_data/nonspacing.txt"))),
            cp,
        )
    {
        return true;
    }
    STRANGE
        .get_or_init(|| {
            include_str!("tsearch_data/strange_letter.txt")
                .lines()
                .filter_map(|l| u32::from_str_radix(l.trim(), 16).ok())
                .collect()
        })
        .binary_search(&cp)
        .is_ok()
}

/// `p_isurlchar`: a printable ASCII character RFC 3986 allows.
fn is_urlchar(c: char) -> bool {
    c.is_ascii()
        && (c as u32) > 0x20
        && (c as u32) < 0x7f
        && !matches!(c, '"' | '<' | '>' | '\\' | '^' | '`' | '{' | '|' | '}')
}

// ------------------------------------------------------------------------
// The driver
// ------------------------------------------------------------------------

/// `TParserPosition`, in characters.
#[derive(Debug, Clone)]
struct Pos {
    pos: usize,
    /// The current character's length in bytes; 0 at the end.
    charlen: usize,
    lentoken: usize,
    state: S,
    /// The action a PUSH was taken at, to resume after it on a POP.
    pushed_at: Option<usize>,
}

struct Parser<'a> {
    s: &'a [char],
    /// The position stack; the last is the current one.
    stack: Vec<Pos>,
    ignore: bool,
    wanthost: bool,
    /// Where the token being read starts, and (on a BINGO) its length/type.
    token: usize,
    lentoken: usize,
    ty: u8,
}

impl<'a> Parser<'a> {
    fn new(s: &'a [char]) -> Self {
        Parser {
            s,
            stack: vec![Pos {
                pos: 0,
                charlen: 0,
                lentoken: 0,
                state: S::Base,
                pushed_at: None,
            }],
            ignore: false,
            wanthost: false,
            token: 0,
            lentoken: 0,
            ty: 0,
        }
    }

    fn top(&mut self) -> &mut Pos {
        self.stack.last_mut().expect("parser state")
    }

    fn cur(&self) -> Option<char> {
        let p = self.stack.last().expect("parser state");
        (p.charlen > 0).then(|| self.s[p.pos])
    }

    /// `TParserCopyInit` + one `TParserGet` from the current position, in
    /// `state` (pushed over `Base`) -- `p_ishost` / `p_isURLPath`. On a
    /// token of type `want` the current position takes it in.
    fn sub_token(&mut self, state: Option<S>, wanthost: bool, want: u8) -> bool {
        let at = self.stack.last().expect("parser state").pos;
        let mut sub = Parser::new(&self.s[at..]);
        sub.wanthost = wanthost;
        if let Some(st) = state {
            let mut p = sub.stack[0].clone();
            p.state = st;
            sub.stack.push(p);
        }
        if sub.get() && sub.ty == want {
            let n = sub.lentoken;
            let charlen = sub.stack.last().expect("parser state").charlen;
            let top = self.top();
            top.pos += n;
            top.lentoken += n;
            top.charlen = charlen;
            return true;
        }
        false
    }

    fn test(&mut self, t: T) -> bool {
        let c = self.cur();
        let one_byte = self.stack.last().expect("parser state").charlen == 1;
        match t {
            T::Eof => c.is_none(),
            T::Eq(x) => one_byte && c == Some(x as char),
            T::Ignore => self.ignore,
            T::Asclet => c.is_some_and(|c| c.is_ascii_alphabetic()),
            T::Alpha => c.is_some_and(is_alpha),
            T::Digit => c.is_some_and(is_digit),
            T::Alnum => c.is_some_and(is_alnum),
            // At the end the C reads the terminating NUL, which is not alnum.
            T::NotAlnum => !c.is_some_and(is_alnum),
            T::Space => c.is_some_and(is_space),
            T::Xdigit => c.is_some_and(|c| c.is_ascii_hexdigit()),
            // `pg_dsplen` of the terminating NUL is 0.
            T::Special => c.is_none_or(is_special),
            T::StopHost => std::mem::take(&mut self.wanthost),
            T::Host => self.sub_token(None, true, HOST),
            T::UrlPath => self.sub_token(Some(S::InURLPathFirst), false, URLPATH),
            T::UrlChar => c.is_some_and(is_urlchar),
            T::Any => true,
        }
    }

    /// `SpecialTags`: `<script>` / `<style>` start ignoring, their close
    /// tags stop it.
    fn special_tags(&mut self) {
        let n = self.stack.last().expect("parser state").lentoken;
        let tok: String = self.s[self.token..self.token + n]
            .iter()
            .collect::<String>()
            .to_ascii_lowercase();
        match n {
            8 if tok == "</script" => self.ignore = false,
            7 if tok == "</style" => self.ignore = false,
            7 if tok == "<script" => self.ignore = true,
            6 if tok == "<style" => self.ignore = true,
            _ => {}
        }
    }

    /// `TParserGet`: the next token, or `false` at the end.
    fn get(&mut self) -> bool {
        let len = self.s.len();
        if self.top().pos >= len {
            return false;
        }
        self.token = self.top().pos;
        self.top().pushed_at = None;
        let mut last: Option<Item> = None;
        while self.top().pos <= len {
            let pos = self.top().pos;
            self.top().charlen = if pos == len {
                0
            } else {
                self.s[pos].len_utf8()
            };
            let state = self.top().state;
            let list = actions(state);
            let mut idx = match self.top().pushed_at.take() {
                Some(at) => at + 1,
                None => 0,
            };
            while list[idx].0 != T::Any {
                if self.test(list[idx].0) {
                    break;
                }
                idx += 1;
            }
            let item = list[idx];
            let (_, flags, to, ty, special) = item;
            last = Some(item);
            match special {
                Sp::No => {}
                Sp::Tags => self.special_tags(),
                Sp::Furl => {
                    self.wanthost = true;
                    let top = self.top();
                    top.pos -= top.lentoken;
                }
                Sp::Hyphen => {
                    let top = self.top();
                    top.pos -= top.lentoken;
                }
                Sp::VerVersion => {
                    let top = self.top();
                    top.pos -= top.lentoken;
                    top.lentoken = 0;
                }
            }
            if flags & A_BINGO != 0 {
                self.lentoken = self.top().lentoken;
                self.top().lentoken = 0;
                self.ty = ty;
            }
            if flags & A_POP != 0 {
                self.stack.pop();
            } else if flags & A_PUSH != 0 {
                self.top().pushed_at = Some(idx);
                let mut p = self.top().clone();
                p.pushed_at = None;
                self.stack.push(p);
            } else if flags & A_CLEAR != 0 {
                let n = self.stack.len();
                self.stack.remove(n - 2);
            } else if flags & A_CLRALL != 0 {
                let top = self.stack.pop().expect("parser state");
                self.stack.clear();
                self.stack.push(top);
            } else if flags & A_MERGE != 0 {
                let ptr = self.stack.pop().expect("parser state");
                let top = self.top();
                top.pos = ptr.pos;
                top.charlen = ptr.charlen;
                top.lentoken = ptr.lentoken;
            }
            if to != S::Null {
                self.top().state = to;
            }
            if flags & A_BINGO != 0 || (self.top().pos >= len && flags & A_RERUN == 0) {
                break;
            }
            if flags & (A_RERUN | A_POP) != 0 {
                continue;
            }
            let top = self.top();
            if top.charlen > 0 {
                top.pos += 1;
                top.lentoken += 1;
            }
        }
        last.is_some_and(|(_, flags, ..)| flags & A_BINGO != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(text: &str) -> Vec<(u8, String)> {
        parse(text)
    }

    /// Answers measured on PostgreSQL 15.19.
    #[test]
    fn matches_postgres() {
        let t = |v: &[(u8, &str)]| {
            v.iter()
                .map(|(a, b)| (*a, b.to_string()))
                .collect::<Vec<_>>()
        };
        assert_eq!(types("foo bar"), t(&[(1, "foo"), (12, " "), (1, "bar")]));
        assert_eq!(
            types("foo-bar"),
            t(&[(16, "foo-bar"), (11, "foo"), (12, "-"), (11, "bar")])
        );
        assert_eq!(
            types("http://x.com/a"),
            t(&[(14, "http://"), (5, "x.com/a"), (6, "x.com"), (18, "/a")])
        );
        assert_eq!(types("1.2.3"), t(&[(8, "1.2.3")]));
        assert_eq!(types("<b>"), t(&[(13, "<b>")]));
        assert_eq!(types(""), t(&[]));
    }
}
