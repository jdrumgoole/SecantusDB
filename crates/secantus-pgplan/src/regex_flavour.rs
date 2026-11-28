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

/// The compile flags a regex starts from, before `prefixes()` reads its
/// director and embedded options: `REG_ADVANCED` for the `~` family (with
/// `REG_ICASE` under `~*`), or what a regex function's options make them
/// (`regexp.c`'s `parse_re_flags`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReFlags {
    /// `REG_EXTENDED`.
    pub extended: bool,
    /// `REG_ADVF`: with `extended`, an ARE.
    pub advf: bool,
    /// `REG_QUOTE`.
    pub quote: bool,
    /// `REG_EXPANDED`.
    pub expanded: bool,
    /// `REG_ICASE`.
    pub icase: bool,
    /// `REG_NLSTOP`.
    pub nlstop: bool,
    /// `REG_NLANCH`.
    pub nlanch: bool,
}

impl ReFlags {
    /// The `~` family's: an ARE, case-insensitive under `~*`.
    pub fn operator(icase: bool) -> Self {
        ReFlags {
            extended: true,
            advf: true,
            quote: false,
            expanded: false,
            icase,
            nlstop: false,
            nlanch: false,
        }
    }

    /// A regex function's options, read left to right as `parse_re_flags`
    /// reads them; the caller has refused a letter that is no option, and
    /// `g` is not a compile flag.
    pub fn options(options: &str) -> Self {
        let mut f = ReFlags::operator(false);
        for c in options.chars() {
            match c {
                'b' => (f.extended, f.advf, f.quote) = (false, false, false),
                'c' => f.icase = false,
                // `REG_EXTENDED` set and then cleared with `REG_ADVANCED`:
                // the option gives a BRE (15.20: `regexp_match('w(0)',
                // 'w(0)', 'e')` is `{w(0)}`), where the embedded `e` gives
                // an ERE.
                'e' => (f.extended, f.advf, f.quote) = (false, false, false),
                'i' => f.icase = true,
                'm' | 'n' => (f.nlstop, f.nlanch) = (true, true),
                'p' => (f.nlstop, f.nlanch) = (true, false),
                'q' => (f.quote, f.extended, f.advf) = (true, false, false),
                's' => (f.nlstop, f.nlanch) = (false, false),
                't' => f.expanded = false,
                'w' => (f.nlstop, f.nlanch) = (false, true),
                'x' => f.expanded = true,
                _ => {}
            }
        }
        f
    }
}

/// A regex (`~`'s flags, `REG_ADVANCED`, and `REG_ICASE` for `~*`) as an
/// ARE of the same meaning, with the flags it compiles under. `None` for a
/// regex `regcomp` refuses (a bad director, an invalid embedded option, a
/// trailing backslash), or where the translation would not be exact.
pub(crate) fn are_text(patt: &str, icase: bool, c_ctype: bool) -> Option<ReText> {
    are_text_with(patt, ReFlags::operator(icase), c_ctype)
}

/// [`are_text`] from `flags`, a regex function's options among them.
pub(crate) fn are_text_with(patt: &str, flags: ReFlags, c_ctype: bool) -> Option<ReText> {
    let chars: Vec<char> = patt.chars().collect();
    let mut pos = 0;
    let ReFlags {
        mut extended,
        mut advf,
        mut quote,
        mut expanded,
        ..
    } = flags;
    let mut re = ReText {
        text: String::new(),
        icase: flags.icase,
        quote: false,
        nlstop: flags.nlstop,
        nlanch: flags.nlanch,
    };
    // A literal string from the options gets no prefixes at all.
    if quote {
        re.quote = true;
        push_literal(&chars, &mut re.text);
        return Some(re);
    }
    // `prefixes()`: an initial `***` is a director.
    if chars.len() >= 4 && chars[..3] == ['*', '*', '*'] {
        match chars[3] {
            // A literal string from here.
            '=' => {
                pos = 4;
                quote = true;
                (extended, advf, expanded) = (false, false, false);
                (re.nlstop, re.nlanch) = (false, false);
            }
            // An ARE from here.
            ':' => {
                pos = 4;
                (extended, advf) = (true, true);
            }
            _ => return None,
        }
    } else if chars.len() >= 3 && chars[..3] == ['*', '*', '*'] {
        return None;
    }
    // Embedded options, in an ARE only.
    if !quote
        && extended
        && advf
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
    // A literal string the options made is neither expanded nor
    // newline-sensitive.
    if quote {
        re.quote = true;
        (re.nlstop, re.nlanch) = (false, false);
        push_literal(&chars[pos..], &mut re.text);
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

/// A PostgreSQL regex compiled from `flags` as the source the `regex`
/// crate and `fancy-regex` read with the same meaning: [`are_text_with`]'s
/// ARE with its classes spelled out (`scalar::pg_regex_source_nl`), behind
/// its flags as inline ones -- `(?s)` unless `REG_NLSTOP`, since a regex is
/// not newline-sensitive by default and its `.` matches a newline; `(?m)`
/// under `REG_NLANCH`; `(?i)` for `REG_ICASE`, and `(?-i)` where an option
/// turned the operator's off -- and, under `REG_NLSTOP`, every negated
/// bracket missing the newline.
///
/// Where `are_text_with` reads no ARE (a regex `regcomp` refuses, or one
/// it cannot translate exactly) the pattern is compiled as written under
/// the starting flags, so the engine raises the error it raises.
pub fn compile_source(patt: &str, flags: ReFlags) -> String {
    let Some(re) = are_text_with(patt, flags, false) else {
        let mut out = String::new();
        let mut inline = String::new();
        for (on, c) in [
            (flags.icase, 'i'),
            (flags.nlanch, 'm'),
            (!flags.nlstop, 's'),
            (flags.expanded, 'x'),
        ] {
            if on {
                inline.push(c);
            }
        }
        if !inline.is_empty() {
            out.push_str(&format!("(?{inline})"));
        }
        out.push_str(&crate::scalar::pg_regex_source(patt));
        return out;
    };
    let mut out = String::new();
    if re.icase {
        out.push_str("(?i)");
    } else if flags.icase {
        out.push_str("(?-i)");
    }
    if re.nlanch {
        out.push_str("(?m)");
    }
    // `s` changes only what a `.` matches.
    if !re.nlstop && re.text.contains('.') {
        out.push_str("(?s)");
    }
    out.push_str(&crate::scalar::pg_regex_source_nl(&re.text, re.nlstop));
    out
}

/// A literal string's characters, each that an ARE (or the `regex` crate)
/// would read otherwise escaped. Only those: the `regex` crate refuses an
/// escaped non-ASCII character and reads `\<` and `\>` as word bounds.
fn push_literal(chars: &[char], out: &mut String) {
    for c in chars {
        push_char(*c, out);
    }
}

/// `c` as a literal character of an ARE the `regex` crate also reads.
fn push_char(c: char, out: &mut String) {
    if regex_syntax_meta(c) {
        out.push('\\');
    }
    out.push(c);
}

/// The characters the `regex` crate gives a meaning (`is_meta_character`).
fn regex_syntax_meta(c: char) -> bool {
    r"\.+*?()|[]{}^$#&-~".contains(c)
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
                // An ERE's escape is the character alone (`next`'s "only AREs
                // have non-trivial escapes"), and so is an ARE's before a
                // character that is no letter or digit.
                if !advf || !e.is_alphanumeric() {
                    push_char(e, out);
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
                    e => push_char(e, out),
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
    use super::{are_text, compile_source, ReFlags};

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
            ("(?x)^w\\ 0", "^w 0"),
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
        // An escaped character the `regex` crate would read otherwise is
        // the character alone: `\<` is a word bound there.
        assert_eq!(text("a\\<b").as_deref(), Some("a<b"));
        assert_eq!(text("(?e)a\\<b").as_deref(), Some("a<b"));
        assert_eq!(text("(?e)a\\.b").as_deref(), Some("a\\.b"));
    }

    #[test]
    fn a_literal_escapes_only_what_the_regex_crate_reads() {
        let r = are_text("***=x<y\u{e9}\u{2014}.", false, true).unwrap();
        assert_eq!(r.text, "x<y\u{e9}\u{2014}\\.");
    }

    fn matches(patt: &str, flags: ReFlags, subject: &str) -> bool {
        let source = compile_source(patt, flags);
        regex::Regex::new(&source)
            .unwrap_or_else(|e| panic!("{patt:?} -> {source:?}: {e}"))
            .is_match(subject)
    }

    /// Each measured on PostgreSQL 15.20.
    #[test]
    fn compiled_sources_match_as_postgresql_does() {
        let op = ReFlags::operator(false);
        let opts = ReFlags::options;
        for (patt, flags, subject, want) in [
            ("a.b", op, "a\nb", true),
            ("(?n)a.b", op, "a\nb", false),
            ("(?n)^ab", op, "x\nab", true),
            ("(?p)^ab", op, "x\nab", false),
            ("(?w)^ab", op, "x\nab", true),
            ("^ab", op, "x\nab", false),
            ("(?n)a[^x]b", op, "a\nb", false),
            ("(?p)a[^]x]b", op, "a\nb", false),
            ("a[^x]b", op, "a\nb", true),
            ("(?n)a\\Db", op, "a\nb", true),
            ("(?n)a\\Wb", op, "a\nb", true),
            ("(?q)a.c", op, "a.c", true),
            ("(?q)a.c", op, "abc", false),
            ("***=^w", op, "^w", true),
            ("***:(?i)W", op, "w", true),
            ("(?b)w0+", op, "w0+", true),
            ("(?b)w0+", op, "w00", false),
            ("(?e)w\\d", op, "wd", true),
            ("(?c)w", ReFlags::operator(true), "W", false),
            ("a\\<b", op, "a<b", true),
            ("a.b", opts("n"), "a\nb", false),
            ("^ab", opts("w"), "x\nab", true),
            ("a b", opts("x"), "ab", true),
            ("(?q)a b", opts("x"), "ab", false),
            ("(?c)w", opts("i"), "W", false),
            ("w", opts("ic"), "W", false),
            ("w", opts("ci"), "W", true),
            ("w0+", opts("e"), "w00", false),
            ("w(0)", opts("e"), "w(0)", true),
            ("a.c", opts("q"), "abc", false),
            ("***:x", opts("b"), "x", true),
        ] {
            assert_eq!(
                matches(patt, flags, subject),
                want,
                "{patt:?} {flags:?} {subject:?}"
            );
        }
    }
}
