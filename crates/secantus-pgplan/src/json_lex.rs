//! PostgreSQL's JSON input error: the DETAIL and the `JSON data, line N:`
//! CONTEXT line `json_in` / `jsonb_in` raise for text that is not JSON.
//!
//! A transcription of PostgreSQL 15's recursive-descent parser and lexer
//! (`src/common/jsonapi.c`: `pg_parse_json`, `json_lex`, `json_lex_string`,
//! `json_lex_number`, `report_parse_error`, `json_errdetail`) and of
//! `report_json_context` / `json_ereport_error` in
//! `src/backend/utils/adt/jsonfuncs.c`. It runs only once [`crate::json`]'s
//! parser has rejected the text, to say WHERE and WHY in PostgreSQL's words:
//! which token the error names and where the excerpt ends follow the
//! lexer's own cursor (`token_start` / `token_terminator`), so the parser
//! is ported as written rather than reconstructed from the messages.

/// `JsonTokenType`. `Invalid` is the type a fresh lexer starts with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Token {
    Invalid,
    Str,
    Number,
    ObjectStart,
    ObjectEnd,
    ArrayStart,
    ArrayEnd,
    Comma,
    Colon,
    True,
    False,
    Null,
    End,
}

/// `JsonParseContext`: what the parser expected when the token did not fit.
#[derive(Clone, Copy)]
enum Ctx {
    Value,
    Str,
    ArrayStart,
    ArrayNext,
    ObjectStart,
    ObjectLabel,
    ObjectNext,
    End,
}

/// `JsonParseErrorType` (less `JSON_UNICODE_HIGH_ESCAPE`, which only a
/// frontend build or a non-UTF8 server reaches).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    EscapingInvalid,
    EscapingRequired,
    ExpectedArrayFirst,
    ExpectedArrayNext,
    ExpectedColon,
    ExpectedEnd,
    ExpectedJson,
    ExpectedMore,
    ExpectedObjectFirst,
    ExpectedObjectNext,
    ExpectedString,
    InvalidToken,
    UnicodeCodePointZero,
    UnicodeEscapeFormat,
    UnicodeHighSurrogate,
    UnicodeLowSurrogate,
}

/// The error PostgreSQL raises for JSON input that does not parse.
#[derive(Debug, PartialEq, Eq)]
pub struct InputError {
    /// 22P05 `unsupported Unicode escape sequence` (`\u0000` into jsonb)
    /// rather than 22P02 `invalid input syntax for type json`.
    pub untranslatable: bool,
    pub detail: String,
    /// `JSON data, line N: <excerpt>`.
    pub context: String,
}

/// The error PostgreSQL's parser raises for `text`, or `None` when it
/// accepts it. `need_escapes` is `jsonb_in`'s mode: escapes are decoded, so
/// a lone surrogate or `\u0000` is an error; `json_in` only validates.
pub fn input_error(text: &str, need_escapes: bool) -> Option<InputError> {
    let mut lex = Lexer {
        input: text.as_bytes(),
        need_escapes,
        token_start: Some(0),
        token_terminator: 0,
        token_type: Token::Invalid,
        line_number: 1,
        line_start: 0,
    };
    let fault = lex.parse().err()?;
    Some(InputError {
        untranslatable: fault == Fault::UnicodeCodePointZero,
        detail: lex.errdetail(fault),
        context: lex.context(),
    })
}

struct Lexer<'a> {
    input: &'a [u8],
    need_escapes: bool,
    /// `None` once the input is exhausted.
    token_start: Option<usize>,
    token_terminator: usize,
    token_type: Token,
    line_number: usize,
    line_start: usize,
}

/// `JSON_ALPHANUMERIC_CHAR`: a byte that continues a bare word.
fn alphanumeric(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c & 0x80 != 0
}

/// `pg_utf_mblen`: the length of the UTF-8 character starting with `c`.
fn utf8_len(c: u8) -> usize {
    if c & 0x80 == 0 {
        1
    } else if c & 0xe0 == 0xc0 {
        2
    } else if c & 0xf0 == 0xe0 {
        3
    } else if c & 0xf8 == 0xf0 {
        4
    } else {
        1
    }
}

type Step = Result<(), Fault>;

impl Lexer<'_> {
    /// `pg_parse_json`.
    fn parse(&mut self) -> Step {
        self.lex()?;
        self.value()?;
        self.expect(Ctx::End, Token::End)
    }

    /// The `switch (tok)` each caller runs on a value's first token.
    fn value(&mut self) -> Step {
        match self.token_type {
            Token::ObjectStart => self.object(),
            Token::ArrayStart => self.array(),
            _ => self.scalar(),
        }
    }

    /// `lex_expect`.
    fn expect(&mut self, ctx: Ctx, token: Token) -> Step {
        if self.token_type == token {
            self.lex()
        } else {
            Err(self.report(ctx))
        }
    }

    /// `report_parse_error`.
    fn report(&self, ctx: Ctx) -> Fault {
        if self.token_start.is_none() || self.token_type == Token::End {
            return Fault::ExpectedMore;
        }
        match ctx {
            Ctx::End => Fault::ExpectedEnd,
            Ctx::Value => Fault::ExpectedJson,
            Ctx::Str => Fault::ExpectedString,
            Ctx::ArrayStart => Fault::ExpectedArrayFirst,
            Ctx::ArrayNext => Fault::ExpectedArrayNext,
            Ctx::ObjectStart => Fault::ExpectedObjectFirst,
            Ctx::ObjectLabel => Fault::ExpectedColon,
            Ctx::ObjectNext => Fault::ExpectedObjectNext,
        }
    }

    /// `parse_scalar`.
    fn scalar(&mut self) -> Step {
        if !matches!(
            self.token_type,
            Token::Str | Token::Number | Token::True | Token::False | Token::Null
        ) {
            return Err(self.report(Ctx::Value));
        }
        self.lex()
    }

    /// `parse_object_field`.
    fn object_field(&mut self) -> Step {
        if self.token_type != Token::Str {
            return Err(self.report(Ctx::Str));
        }
        self.lex()?;
        self.expect(Ctx::ObjectLabel, Token::Colon)?;
        self.value()
    }

    /// `parse_object`.
    fn object(&mut self) -> Step {
        self.lex()?;
        match self.token_type {
            Token::Str => {
                self.object_field()?;
                while self.token_type == Token::Comma {
                    self.lex()?;
                    self.object_field()?;
                }
            }
            Token::ObjectEnd => {}
            _ => return Err(self.report(Ctx::ObjectStart)),
        }
        self.expect(Ctx::ObjectNext, Token::ObjectEnd)
    }

    /// `parse_array`.
    fn array(&mut self) -> Step {
        self.expect(Ctx::ArrayStart, Token::ArrayStart)?;
        if self.token_type != Token::ArrayEnd {
            self.value()?;
            while self.token_type == Token::Comma {
                self.lex()?;
                self.value()?;
            }
        }
        self.expect(Ctx::ArrayNext, Token::ArrayEnd)
    }

    /// `json_lex`: one token, from the end of the last. A failure leaves
    /// `token_type` as the previous token's, as PostgreSQL's does.
    fn lex(&mut self) -> Step {
        let b = self.input;
        let mut s = self.token_terminator;
        while s < b.len() && matches!(b[s], b' ' | b'\t' | b'\n' | b'\r') {
            s += 1;
            if b[s - 1] == b'\n' {
                self.line_number += 1;
                self.line_start = s;
            }
        }
        self.token_start = Some(s);
        if s >= b.len() {
            self.token_start = None;
            self.token_terminator = s;
            self.token_type = Token::End;
            return Ok(());
        }
        let punct = match b[s] {
            b'{' => Some(Token::ObjectStart),
            b'}' => Some(Token::ObjectEnd),
            b'[' => Some(Token::ArrayStart),
            b']' => Some(Token::ArrayEnd),
            b',' => Some(Token::Comma),
            b':' => Some(Token::Colon),
            _ => None,
        };
        if let Some(token) = punct {
            self.token_terminator = s + 1;
            self.token_type = token;
            return Ok(());
        }
        self.token_type = match b[s] {
            b'"' => {
                self.lex_string(s)?;
                Token::Str
            }
            b'-' => {
                self.lex_number(s + 1)?;
                Token::Number
            }
            b'0'..=b'9' => {
                self.lex_number(s)?;
                Token::Number
            }
            _ => {
                // Scan the whole word, so the error names it rather than
                // some prefix of it.
                let mut p = s;
                while p < b.len() && alphanumeric(b[p]) {
                    p += 1;
                }
                if p == s {
                    self.token_terminator = s + 1;
                    return Err(Fault::InvalidToken);
                }
                self.token_terminator = p;
                match &b[s..p] {
                    b"true" => Token::True,
                    b"null" => Token::Null,
                    b"false" => Token::False,
                    _ => return Err(Fault::InvalidToken),
                }
            }
        };
        Ok(())
    }

    /// `json_lex_string`, from the opening quote at `start`.
    fn lex_string(&mut self, start: usize) -> Step {
        let b = self.input;
        let end = b.len();
        // FAIL_AT_CHAR_START / FAIL_AT_CHAR_END.
        let at_start = |lex: &mut Self, s: usize, f: Fault| {
            lex.token_terminator = s;
            Err(f)
        };
        let at_end = |lex: &mut Self, s: usize, f: Fault| {
            lex.token_terminator = (s + utf8_len(b[s])).min(end);
            Err(f)
        };
        let mut s = start;
        let mut hi_surrogate: Option<u32> = None;
        loop {
            s += 1;
            if s >= end {
                return at_start(self, s, Fault::InvalidToken);
            }
            let c = b[s];
            if c == b'"' {
                break;
            } else if c < 32 {
                // Not printable, so left out of the context excerpt.
                return at_start(self, s, Fault::EscapingRequired);
            } else if c == b'\\' {
                s += 1;
                if s >= end {
                    return at_start(self, s, Fault::InvalidToken);
                }
                if b[s] == b'u' {
                    let mut ch: u32 = 0;
                    for _ in 0..4 {
                        s += 1;
                        if s >= end {
                            return at_start(self, s, Fault::InvalidToken);
                        }
                        match (b[s] as char).to_digit(16) {
                            Some(d) => ch = ch * 16 + d,
                            None => return at_end(self, s, Fault::UnicodeEscapeFormat),
                        }
                    }
                    if self.need_escapes {
                        if (0xD800..=0xDBFF).contains(&ch) {
                            if hi_surrogate.is_some() {
                                return at_end(self, s, Fault::UnicodeHighSurrogate);
                            }
                            hi_surrogate = Some(ch);
                            continue;
                        } else if (0xDC00..=0xDFFF).contains(&ch) {
                            if hi_surrogate.is_none() {
                                return at_end(self, s, Fault::UnicodeLowSurrogate);
                            }
                            hi_surrogate = None;
                            // A pair is one character, never zero.
                            ch = 0x10000;
                        }
                        if hi_surrogate.is_some() {
                            return at_end(self, s, Fault::UnicodeLowSurrogate);
                        }
                        if ch == 0 {
                            return at_end(self, s, Fault::UnicodeCodePointZero);
                        }
                    }
                } else if self.need_escapes {
                    if hi_surrogate.is_some() {
                        return at_end(self, s, Fault::UnicodeLowSurrogate);
                    }
                    if !b"\"\\/bfnrt".contains(&b[s]) {
                        // Only the escape is reported, not the whole string.
                        self.token_start = Some(s);
                        return at_end(self, s, Fault::EscapingInvalid);
                    }
                } else if !b"\"\\/bfnrt".contains(&b[s]) {
                    self.token_start = Some(s);
                    return at_end(self, s, Fault::EscapingInvalid);
                }
            } else if self.need_escapes && hi_surrogate.is_some() {
                return at_end(self, s, Fault::UnicodeLowSurrogate);
            }
        }
        self.token_terminator = s + 1;
        if hi_surrogate.is_some() {
            return Err(Fault::UnicodeLowSurrogate);
        }
        Ok(())
    }

    /// `json_lex_number`, from just after any minus sign.
    fn lex_number(&mut self, mut s: usize) -> Step {
        let b = self.input;
        let n = b.len();
        let digit = |s: usize| s < n && b[s].is_ascii_digit();
        let mut error = false;
        if s < n && b[s] == b'0' {
            s += 1;
        } else if s < n && (b'1'..=b'9').contains(&b[s]) {
            while digit(s) {
                s += 1;
            }
        } else {
            error = true;
        }
        if s < n && b[s] == b'.' {
            s += 1;
            if !digit(s) {
                error = true;
            } else {
                while digit(s) {
                    s += 1;
                }
            }
        }
        if s < n && matches!(b[s], b'e' | b'E') {
            s += 1;
            if s < n && matches!(b[s], b'+' | b'-') {
                s += 1;
            }
            if !digit(s) {
                error = true;
            } else {
                while digit(s) {
                    s += 1;
                }
            }
        }
        // Trailing alphanumerics belong to the token, for the message.
        while s < n && alphanumeric(b[s]) {
            s += 1;
            error = true;
        }
        self.token_terminator = s;
        if error {
            return Err(Fault::InvalidToken);
        }
        Ok(())
    }

    /// `extract_token`.
    fn token(&self) -> String {
        let start = self.token_start.unwrap_or(self.token_terminator);
        String::from_utf8_lossy(&self.input[start..self.token_terminator]).into_owned()
    }

    /// `json_errdetail`.
    fn errdetail(&self, fault: Fault) -> String {
        match fault {
            Fault::EscapingInvalid => format!("Escape sequence \"\\{}\" is invalid.", self.token()),
            Fault::EscapingRequired => format!(
                "Character with value 0x{:02x} must be escaped.",
                self.input[self.token_terminator]
            ),
            Fault::ExpectedEnd => {
                format!("Expected end of input, but found \"{}\".", self.token())
            }
            Fault::ExpectedArrayFirst => format!(
                "Expected array element or \"]\", but found \"{}\".",
                self.token()
            ),
            Fault::ExpectedArrayNext => {
                format!("Expected \",\" or \"]\", but found \"{}\".", self.token())
            }
            Fault::ExpectedColon => format!("Expected \":\", but found \"{}\".", self.token()),
            Fault::ExpectedJson => {
                format!("Expected JSON value, but found \"{}\".", self.token())
            }
            Fault::ExpectedMore => "The input string ended unexpectedly.".into(),
            Fault::ExpectedObjectFirst => {
                format!("Expected string or \"}}\", but found \"{}\".", self.token())
            }
            Fault::ExpectedObjectNext => {
                format!("Expected \",\" or \"}}\", but found \"{}\".", self.token())
            }
            Fault::ExpectedString => format!("Expected string, but found \"{}\".", self.token()),
            Fault::InvalidToken => format!("Token \"{}\" is invalid.", self.token()),
            Fault::UnicodeCodePointZero => "\\u0000 cannot be converted to text.".into(),
            Fault::UnicodeEscapeFormat => {
                "\"\\u\" must be followed by four hexadecimal digits.".into()
            }
            Fault::UnicodeHighSurrogate => {
                "Unicode high surrogate must not follow a high surrogate.".into()
            }
            Fault::UnicodeLowSurrogate => {
                "Unicode low surrogate must follow a high surrogate.".into()
            }
        }
    }

    /// `report_json_context`: the error's line up to where the lexer
    /// stopped, cut to under 50 bytes from the left, `...` marking either
    /// cut.
    fn context(&self) -> String {
        let b = self.input;
        let line_start = self.line_start;
        let end = self.token_terminator;
        let mut start = line_start;
        while end - start >= 50 {
            start += if b[start] & 0x80 != 0 {
                utf8_len(b[start])
            } else {
                1
            };
        }
        // Within 3 bytes of the line's start, the whole line is shown.
        if start - line_start <= 3 {
            start = line_start;
        }
        let prefix = if start > line_start { "..." } else { "" };
        let suffix =
            if self.token_type != Token::End && end < b.len() && b[end] != b'\n' && b[end] != b'\r'
            {
                "..."
            } else {
                ""
            };
        format!(
            "JSON data, line {}: {prefix}{}{suffix}",
            self.line_number,
            String::from_utf8_lossy(&b[start..end])
        )
    }
}

#[cfg(test)]
mod tests {
    use super::input_error;

    fn answer(text: &str, jsonb: bool) -> Option<(String, String)> {
        input_error(text, jsonb).map(|e| (e.detail, e.context))
    }

    #[test]
    fn valid_input_has_no_error() {
        for text in [
            "1",
            " {\"a\": [1, -2.5e3, true, null]} ",
            "\"\\ud83d\\ude00\"",
        ] {
            assert_eq!(answer(text, true), None, "{text}");
        }
        // json only validates escapes, so a lone surrogate is fine there.
        assert_eq!(answer("\"\\uD800\"", false), None);
    }

    /// Measured on PostgreSQL 15.19.
    #[test]
    fn errors_name_the_token_and_the_line_up_to_it() {
        let cases = [
            ("t", "Token \"t\" is invalid.", "JSON data, line 1: t"),
            (
                "[1,\n2,\nx]",
                "Token \"x\" is invalid.",
                "JSON data, line 3: x...",
            ),
            (
                "{\"a\" 1}",
                "Expected \":\", but found \"1\".",
                "JSON data, line 1: {\"a\" 1...",
            ),
            (
                "",
                "The input string ended unexpectedly.",
                "JSON data, line 1: ",
            ),
            (
                "[1",
                "The input string ended unexpectedly.",
                "JSON data, line 1: [1",
            ),
            (
                "\"\\uD800\"",
                "Unicode low surrogate must follow a high surrogate.",
                "JSON data, line 1: \"\\uD800\"",
            ),
        ];
        for (text, detail, context) in cases {
            assert_eq!(
                answer(text, true),
                Some((detail.to_string(), context.to_string())),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_long_line_is_cut_to_the_last_49_bytes() {
        let word = "abcdefghijklmnopqrstuvwxyz".repeat(3);
        let (_, context) = answer(&format!("{word} x"), true).unwrap();
        assert_eq!(
            context,
            "JSON data, line 1: ...defghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz..."
        );
    }

    #[test]
    fn nul_escape_is_untranslatable_only_for_jsonb() {
        let e = input_error("\"a\\u0000b\"", true).unwrap();
        assert!(e.untranslatable);
        assert_eq!(e.context, "JSON data, line 1: \"a\\u0000...");
        assert_eq!(input_error("\"a\\u0000b\"", false), None);
    }
}
