//! The flags a PostgreSQL regex compiles under and its text as an ARE of
//! the same meaning: `regcomp.c`'s `prefixes()` (a `***` director and an
//! ARE's embedded options), then `regc_lex.c`'s lexing of the flavour they
//! leave -- an ARE, an ERE, a BRE or a literal string, expanded or not
//! (RP-423).
//!
//! The planner's regex reading (`colstats`'s prefix walk, and the
//! histogram matcher beside it) reads only AREs, as written; this gives it
//! every other flavour translated into one, so `(?x)^w 0 0` is read as
//! `^w00`, `(?b)^w0+` as `^w0\+` and `***=^w` as `\^w`.

/// What [`are_text`] reads from a regex.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReText {
    /// The regex as an ARE of the same meaning: the director and options
    /// gone, an expanded regex's white space and comments and an ARE's
    /// `(?#...)` comments dropped, a BRE's, an ERE's or a literal string's
    /// characters escaped where an ARE would read them otherwise.
    pub text: String,
    /// `REG_ICASE`: the operator's case-insensitivity, as the `i` and `c`
    /// options leave it.
    pub icase: bool,
    /// `REG_QUOTE`: the regex is a literal string (`***=`, the `q`
    /// option), so it has no anchor.
    pub quote: bool,
    /// `REG_NLSTOP`: `.` and a negated bracket do not match a newline
    /// (the `n`, `m` and `p` options).
    pub nlstop: bool,
    /// `REG_NLANCH`: `^` and `$` also match after and before a newline
    /// (the `n`, `m` and `w` options).
    pub nlanch: bool,
}

/// `pg_wc_isspace` of the regex's collation: `isspace` in `"C"`, the
/// Unicode white space otherwise.
fn is_space(c: char, c_ctype: bool) -> bool {
    if c_ctype {
        matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
    } else {
        c.is_whitespace()
    }
}

/// `regc_lex.c`'s `skip`: an expanded regex's white space, and its
/// comments from a `#` to the end of the line.
fn skip(chars: &[char], pos: &mut usize, c_ctype: bool) {
    loop {
        while chars.get(*pos).is_some_and(|c| is_space(*c, c_ctype)) {
            *pos += 1;
        }
        if chars.get(*pos) != Some(&'#') {
            break;
        }
        while chars.get(*pos).is_some_and(|c| *c != '\n') {
            *pos += 1;
        }
    }
}

/// A regex (`~`'s flags, `REG_ADVANCED`, and `REG_ICASE` for `~*`) as an
/// ARE of the same meaning, with the flags it compiles under. `None` for a
/// regex `regcomp` refuses (a bad director, an invalid embedded option, a
/// trailing backslash), or where the translation would not be exact.
pub(crate) fn are_text(patt: &str, icase: bool, c_ctype: bool) -> Option<ReText> {
    let chars: Vec<char> = patt.chars().collect();
    let mut pos = 0;
    // `REG_EXTENDED`, `REG_ADVF` (together `REG_ADVANCED`), `REG_QUOTE`,
    // `REG_EXPANDED`.
    let (mut extended, mut advf, mut quote, mut expanded) = (true, true, false, false);
    let mut re = ReText {
        text: String::new(),
        icase,
        quote: false,
        nlstop: false,
        nlanch: false,
    };
    // `prefixes()`: an initial `***` is a director.
    if chars.len() >= 4 && chars[..3] == ['*', '*', '*'] {
        match chars[3] {
            // A literal string from here.
            '=' => {
                pos = 4;
                quote = true;
                (extended, advf) = (false, false);
            }
            // An ARE from here.
            ':' => pos = 4,
            _ => return None,
        }
    } else if chars.len() >= 3 && chars[..3] == ['*', '*', '*'] {
        return None;
    }
    // Embedded options, in an ARE only.
    if !quote
        && chars.len() >= pos + 3
        && chars[pos] == '('
        && chars[pos + 1] == '?'
        && chars[pos + 2].is_alphabetic()
    {
        pos += 2;
        while let Some(c) = chars.get(pos).filter(|c| c.is_alphabetic()) {
            match c {
                'b' => (extended, advf, quote) = (false, false, false),
                'c' => re.icase = false,
                'e' => (extended, advf, quote) = (true, false, false),
                'i' => re.icase = true,
                'm' | 'n' => (re.nlstop, re.nlanch) = (true, true),
                'p' => (re.nlstop, re.nlanch) = (true, false),
                'q' => (quote, extended, advf) = (true, false, false),
                's' => (re.nlstop, re.nlanch) = (false, false),
                't' => expanded = false,
                'w' => (re.nlstop, re.nlanch) = (false, true),
                'x' => expanded = true,
                _ => return None,
            }
            pos += 1;
        }
        if chars.get(pos) != Some(&')') {
            return None;
        }
        pos += 1;
    }
    // A literal string is neither expanded nor newline-sensitive.
    if quote {
        re.quote = true;
        (re.nlstop, re.nlanch) = (false, false);
        for c in &chars[pos..] {
            if !c.is_alphanumeric() {
                re.text.push('\\');
            }
            re.text.push(*c);
        }
        return Some(re);
    }
    let rest = &chars[pos..];
    let ok = if !extended {
        bre(rest, expanded, c_ctype, &mut re.text)
    } else {
        ere_or_are(rest, advf, expanded, c_ctype, &mut re.text)
    };
    ok.then_some(re)
}

/// A bracket expression at `*pos` copied into `out`, `*pos` moved past
/// it. In an ARE a backslash escapes inside it (`lexescape` in
/// `L_BRACK`); in an ERE or a BRE it is a backslash.
fn bracket(chars: &[char], pos: &mut usize, are: bool, out: &mut String) -> bool {
    out.push('[');
    *pos += 1;
    if chars.get(*pos) == Some(&'^') {
        out.push('^');
        *pos += 1;
    }
    if chars.get(*pos) == Some(&']') {
        out.push(']');
        *pos += 1;
    }
    loop {
        let Some(&c) = chars.get(*pos) else {
            return false;
        };
        match (c, chars.get(*pos + 1)) {
            (']', _) => {
                out.push(']');
                *pos += 1;
                return true;
            }
            // A class, collating element or equivalence class, to its
            // closing `:]`, `.]` or `=]`.
            ('[', Some(&k @ (':' | '.' | '='))) => {
                let Some(end) = chars[*pos + 2..].windows(2).position(|w| w == [k, ']']) else {
                    return false;
                };
                let end = *pos + 2 + end + 2;
                out.extend(&chars[*pos..end]);
                *pos = end;
            }
            ('\\', Some(&e)) if are => {
                out.push('\\');
                out.push(e);
                *pos += 2;
            }
            ('\\', _) if !are => {
                out.push_str("\\\\");
                *pos += 1;
            }
            _ => {
                out.push(c);
                *pos += 1;
            }
        }
    }
}

/// An ARE's text (`advf`) or an ERE's, as an ARE, into `out`: an expanded
/// regex's white space and comments dropped, `(?#...)` comments dropped,
/// and an ERE's backslash before an alphanumeric (`next`'s "only AREs have
/// non-trivial escapes") the character alone.
fn ere_or_are(chars: &[char], advf: bool, expanded: bool, c_ctype: bool, out: &mut String) -> bool {
    let mut pos = 0;
    loop {
        if expanded {
            skip(chars, &mut pos, c_ctype);
        }
        let Some(&c) = chars.get(pos) else {
            return true;
        };
        match c {
            '[' => {
                if !bracket(chars, &mut pos, advf, out) {
                    return false;
                }
            }
            '\\' => {
                let Some(&e) = chars.get(pos + 1) else {
                    return false;
                };
                pos += 2;
                if !advf {
                    if !e.is_alphanumeric() {
                        out.push('\\');
                    }
                    out.push(e);
                    continue;
                }
                out.push('\\');
                out.push(e);
                match e {
                    // A control character: the next character, as it is.
                    'c' => {
                        let Some(&k) = chars.get(pos) else {
                            return false;
                        };
                        out.push(k);
                        pos += 1;
                    }
                    // Digits read straight on (`lexdigits`): white space
                    // dropped after them could join the next digit to them.
                    'x' | 'u' | 'U' | '0'..='9' if expanded => {
                        while chars.get(pos).is_some_and(|d| d.is_ascii_hexdigit()) {
                            out.push(chars[pos]);
                            pos += 1;
                        }
                        let before = pos;
                        skip(chars, &mut pos, c_ctype);
                        if pos != before && chars.get(pos).is_some_and(|d| d.is_ascii_hexdigit()) {
                            return false;
                        }
                    }
                    _ => {}
                }
            }
            '(' if chars.get(pos + 1) == Some(&'?') => {
                // An ERE has no `(?`: a quantifier with no operand.
                if !advf {
                    return false;
                }
                match chars.get(pos + 2) {
                    // A comment, to its `)`.
                    Some('#') => {
                        pos += 3;
                        while chars.get(pos).is_some_and(|c| *c != ')') {
                            pos += 1;
                        }
                        pos = (pos + 1).min(chars.len());
                    }
                    // `(?:`, `(?=`, `(?!`, `(?<=`, `(?<!`: read as a unit.
                    Some(&k) => {
                        let len = if k == '<' { 4 } else { 3 };
                        let end = (pos + len).min(chars.len());
                        out.extend(&chars[pos..end]);
                        pos = end;
                    }
                    None => return false,
                }
            }
            c => {
                out.push(c);
                pos += 1;
            }
        }
    }
}

/// A BRE's text as an ARE, into `out` (`regc_lex.c`'s `brenext`): `\(`,
/// `\)`, `\{`, `\}` its groups and bounds, `\<` and `\>` word bounds, `*`
/// a character at the start or after `\(` or a leading `^`, `^` an anchor
/// only there, `$` only at the end or before `\)`, and `+`, `?`, `|`, `(`,
/// `)`, `{`, `}` characters.
fn bre(chars: &[char], expanded: bool, c_ctype: bool, out: &mut String) -> bool {
    #[derive(PartialEq)]
    enum Last {
        Empty,
        Open,
        Caret,
        Other,
    }
    let mut last = Last::Empty;
    let mut pos = 0;
    let mut in_bound = false;
    loop {
        if expanded {
            skip(chars, &mut pos, c_ctype);
        }
        let Some(&c) = chars.get(pos) else {
            return !in_bound;
        };
        pos += 1;
        if in_bound {
            match c {
                '\\' if chars.get(pos) == Some(&'}') => {
                    pos += 1;
                    out.push('}');
                    in_bound = false;
                    last = Last::Other;
                }
                '0'..='9' | ',' => out.push(c),
                _ => return false,
            }
            continue;
        }
        let mut next = Last::Other;
        match c {
            '*' if last != Last::Other => out.push_str("\\*"),
            '*' => out.push('*'),
            '^' if matches!(last, Last::Empty | Last::Open) => {
                out.push('^');
                next = Last::Caret;
            }
            '$' => {
                let mut at = pos;
                if expanded {
                    skip(chars, &mut at, c_ctype);
                }
                let anchor = at == chars.len()
                    || chars.get(at) == Some(&'\\') && chars.get(at + 1) == Some(&')');
                out.push_str(if anchor { "$" } else { "\\$" });
            }
            '[' => {
                pos -= 1;
                if !bracket(chars, &mut pos, false, out) {
                    return false;
                }
            }
            '.' => out.push('.'),
            '\\' => {
                let Some(&e) = chars.get(pos) else {
                    return false;
                };
                pos += 1;
                match e {
                    '(' => {
                        out.push('(');
                        next = Last::Open;
                    }
                    ')' => out.push(')'),
                    '{' => {
                        out.push('{');
                        in_bound = true;
                        continue;
                    }
                    '<' => out.push_str("\\m"),
                    '>' => out.push_str("\\M"),
                    '1'..='9' => {
                        out.push('\\');
                        out.push(e);
                    }
                    e if e.is_alphanumeric() => out.push(e),
                    e => {
                        out.push('\\');
                        out.push(e);
                    }
                }
            }
            '^' | '+' | '?' | '|' | '(' | ')' | '{' | '}' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
        last = next;
    }
}

#[cfg(test)]
mod tests {
    use super::are_text;

    fn text(patt: &str) -> Option<String> {
        are_text(patt, false, true).map(|r| r.text)
    }

    #[test]
    fn an_are_without_options_is_its_own_text() {
        for p in ["^w00", "^(?:w|^x)0", "^w[ ]0", "^w\\d", "(?=x)w", "^a{2,3}"] {
            assert_eq!(text(p).as_deref(), Some(p), "{p}");
        }
    }

    #[test]
    fn options_and_directors_are_read() {
        let r = are_text("(?ic)^w00", false, true).unwrap();
        assert!(!r.icase);
        let r = are_text("(?ci)^w00", false, true).unwrap();
        assert!(r.icase);
        let r = are_text("(?c)^w", true, true).unwrap();
        assert!(!r.icase);
        let r = are_text("(?n)^w", false, true).unwrap();
        assert!(r.nlstop && r.nlanch);
        let r = are_text("(?p)^w", false, true).unwrap();
        assert!(r.nlstop && !r.nlanch);
        let r = are_text("(?w)^w", false, true).unwrap();
        assert!(!r.nlstop && r.nlanch);
        let r = are_text("(?q)^w.0", false, true).unwrap();
        assert!(r.quote);
        assert_eq!(r.text, "\\^w\\.0");
        let r = are_text("***=^w", false, true).unwrap();
        assert!(r.quote);
        assert_eq!(text("***:^w00").as_deref(), Some("^w00"));
        assert_eq!(text("***:(?x)^w 0").as_deref(), Some("^w0"));
        // `regcomp` refuses these.
        for p in ["(?z)^w", "(?x", "***?", "***", "***x", "^w\\"] {
            assert_eq!(text(p), None, "{p}");
        }
        // Not options: an ARE's `(?` followed by no letter.
        assert_eq!(text("(?:w)0").as_deref(), Some("(?:w)0"));
    }

    #[test]
    fn expanded_syntax_drops_white_space_and_comments() {
        for (p, want) in [
            ("(?x)^w 0 0", "^w00"),
            ("(?x)^w0 # c", "^w0"),
            ("(?x)^w0 #c\n1", "^w01"),
            ("(?x)^w\\ 0", "^w\\ 0"),
            ("(?x)^w[ ]0", "^w[ ]0"),
            ("(?x)^w0 * 1", "^w0*1"),
            ("(?x)^w0{ 2}", "^w0{2}"),
            ("(?x)^w0\t0", "^w00"),
            ("(?x) ^ (?:w 0) 1", "^(?:w0)1"),
            ("^w(?#c)00", "^w00"),
            ("(?x)^w(?#c) 00", "^w00"),
        ] {
            assert_eq!(text(p).as_deref(), Some(want), "{p:?}");
        }
        // White space between a hex escape and a hex digit.
        assert_eq!(text("(?x)^\\x4 1"), None);
    }

    #[test]
    fn eres_and_bres_are_translated() {
        for (p, want) in [
            ("(?e)^w0{2}", "^w0{2}"),
            ("(?e)^w\\d", "^wd"),
            ("(?e)^w(0)\\1", "^w(0)1"),
            ("(?e)^w[\\]0", "^w[\\\\]0"),
            ("(?b)^w0+", "^w0\\+"),
            ("(?b)^w0\\{2\\}", "^w0{2}"),
            ("(?b)\\(^w\\)0", "(^w)0"),
            ("(?b)^w\\(0\\)\\1", "^w(0)\\1"),
            ("(?b)*w", "\\*w"),
            ("(?b)^*w", "^\\*w"),
            ("(?b)w^0", "w\\^0"),
            ("(?b)^w0$", "^w0$"),
            ("(?b)^w0$x", "^w0\\$x"),
            ("(?b)^w0\\$", "^w0\\$"),
            ("(?bx)^w 0 0", "^w00"),
        ] {
            assert_eq!(text(p).as_deref(), Some(want), "{p:?}");
        }
        assert_eq!(text("(?e)(?:w)"), None);
    }
}
