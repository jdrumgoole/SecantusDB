//! The subset of PL/pgSQL an inline `DO` block can use here.
//!
//! This is deliberately NOT a PL/pgSQL interpreter. A `DO` body is `begin
//! <statements> end`, and a statement is one of `raise`, `execute`,
//! `perform` or `null`. Every other construct -- a `declare` section,
//! variables, control flow, `into` -- is refused with `0A000`, never
//! approximated. Expressions are not evaluated here at all: the wire layer
//! runs each one as a `select`, so they carry the SQL evaluator's exact
//! semantics rather than a second one.
//!
//! Everything about the messages, SQLSTATEs and context lines below was
//! measured on PostgreSQL 16.15; see `tests/test_rust_pgserver_slice.py`.

/// The severity of a RAISE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Log,
    Info,
    Notice,
    Warning,
    Exception,
}

impl Level {
    fn parse(word: &str) -> Option<Self> {
        Some(match word.to_ascii_lowercase().as_str() {
            "debug" => Level::Debug,
            "log" => Level::Log,
            "info" => Level::Info,
            "notice" => Level::Notice,
            "warning" => Level::Warning,
            "exception" => Level::Exception,
            _ => return None,
        })
    }

    /// The `S` / `V` field of the message a client sees. `Debug` and `Log`
    /// are below the default `client_min_messages` and are never sent.
    pub fn severity(self) -> Option<&'static str> {
        match self {
            Level::Debug | Level::Log => None,
            Level::Info => Some("INFO"),
            Level::Notice => Some("NOTICE"),
            Level::Warning => Some("WARNING"),
            Level::Exception => Some("ERROR"),
        }
    }

    /// The SQLSTATE a RAISE of this level carries when no `errcode` /
    /// condition names one: `00000` for a notice, `01000` for a warning,
    /// `P0001` (raise_exception) for an exception.
    pub fn default_sqlstate(self) -> &'static str {
        match self {
            Level::Warning => "01000",
            Level::Exception => "P0001",
            _ => "00000",
        }
    }
}

/// What a RAISE says: a format string with its `%` arguments (each an SQL
/// expression, unevaluated), a condition name, a literal SQLSTATE, or nothing
/// (`raise using message = ...`, or a bare `raise`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RaiseWhat {
    Format { text: String, args: Vec<String> },
    Condition(String),
    Sqlstate(String),
    Nothing,
}

/// One `RAISE` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raise {
    pub level: Level,
    pub what: RaiseWhat,
    /// `USING` options as (upper-case option name, SQL expression).
    pub options: Vec<(String, String)>,
    /// The 1-based line of the body the statement starts on.
    pub line: usize,
}

/// One statement of the block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Raise(Raise),
    /// `EXECUTE <expr>`: the expression yields the SQL to run.
    Execute {
        query: String,
        line: usize,
    },
    /// `PERFORM <expr>`: `select <expr>`, result discarded.
    Perform {
        expr: String,
        line: usize,
    },
    Null,
}

/// Why a body could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// `42601`, with the 1-based CHARACTER offset into the body of the
    /// token at fault when there is one (the wire layer turns it into a
    /// statement position).
    Syntax {
        message: String,
        offset: Option<usize>,
    },
    /// `42601` found by PL/pgSQL's compile pass, whose context is
    /// `compilation of PL/pgSQL function "inline_code_block" near line N`.
    Compile { message: String, line: usize },
    /// `0A000`: valid PL/pgSQL this server does not interpret.
    Unsupported(String),
}

const RAISE_OPTIONS: &[&str] = &[
    "MESSAGE",
    "DETAIL",
    "HINT",
    "ERRCODE",
    "COLUMN",
    "CONSTRAINT",
    "DATATYPE",
    "TABLE",
    "SCHEMA",
];

/// Compile a body into its statements.
pub fn parse(body: &str) -> Result<Vec<Stmt>, ParseError> {
    let chars: Vec<char> = body.chars().collect();
    let mut pos = skip_space(&chars, 0);
    let (word, after) = word_at(&chars, pos);
    if word.eq_ignore_ascii_case("declare") {
        return Err(ParseError::Unsupported(
            "a DECLARE section in an inline code block".into(),
        ));
    }
    if !word.eq_ignore_ascii_case("begin") {
        return Err(syntax_near(&chars, pos));
    }
    pos = after;
    let mut stmts = Vec::new();
    loop {
        pos = skip_space(&chars, pos);
        let (word, after) = word_at(&chars, pos);
        if word.eq_ignore_ascii_case("end") {
            let tail = skip_space(&chars, after);
            let tail = if chars.get(tail) == Some(&';') {
                skip_space(&chars, tail + 1)
            } else {
                tail
            };
            if tail < chars.len() {
                return Err(syntax_near(&chars, tail));
            }
            return Ok(stmts);
        }
        if word.is_empty() {
            // Ran out of body without an `end`.
            return Err(ParseError::Syntax {
                message: "syntax error at end of input".into(),
                offset: Some(chars.len() + 1),
            });
        }
        let end = statement_end(&chars, pos).ok_or_else(|| {
            // PL/pgSQL swallows the missing `;` and trips over the `end`
            // (or whatever word closes the input).
            let last = last_word_start(&chars, chars.len());
            syntax_near(&chars, last)
        })?;
        let text: String = chars[pos..end].iter().collect();
        let line = 1 + chars[..pos].iter().filter(|c| **c == '\n').count();
        stmts.push(parse_statement(text.trim(), line, pos, &chars)?);
        pos = end + 1;
    }
}

fn parse_statement(
    text: &str,
    line: usize,
    start: usize,
    body: &[char],
) -> Result<Stmt, ParseError> {
    let (head, rest) = split_first_word(text);
    match head.to_ascii_lowercase().as_str() {
        "null" if rest.is_empty() => Ok(Stmt::Null),
        "raise" => parse_raise(
            rest,
            line,
            start + text.chars().count() - rest.chars().count(),
            body,
        ),
        "execute" => {
            if rest.is_empty() {
                return Err(syntax_near(body, start + text.len()));
            }
            if has_top_level_word(rest, "into") || has_top_level_word(rest, "using") {
                return Err(ParseError::Unsupported(
                    "EXECUTE ... INTO / USING in an inline code block".into(),
                ));
            }
            Ok(Stmt::Execute {
                query: rest.to_string(),
                line,
            })
        }
        "perform" => {
            if rest.is_empty() {
                return Err(syntax_near(body, start + text.len()));
            }
            Ok(Stmt::Perform {
                expr: rest.to_string(),
                line,
            })
        }
        other => Err(ParseError::Unsupported(format!(
            "the PL/pgSQL statement \"{other}\" in an inline code block"
        ))),
    }
}

fn parse_raise(rest: &str, line: usize, offset: usize, body: &[char]) -> Result<Stmt, ParseError> {
    // `offset` is where `rest` begins in the body, for error positions.
    let mut s = rest;
    let mut at = offset;
    let advance = |s: &mut &str, at: &mut usize, n: usize| {
        *at += s[..n].chars().count();
        *s = &s[n..];
    };
    let skip_ws = |s: &mut &str, at: &mut usize| {
        let n = s.len() - s.trim_start().len();
        *at += s[..n].chars().count();
        *s = s.trim_start();
    };
    skip_ws(&mut s, &mut at);
    let (first, _) = split_first_word(s);
    let level = match Level::parse(first) {
        Some(l) if !first.is_empty() => {
            advance(&mut s, &mut at, first.len());
            skip_ws(&mut s, &mut at);
            l
        }
        _ => Level::Exception,
    };
    let (what, remaining) = if s.starts_with('\'') {
        let (text, consumed) = string_literal(s).ok_or_else(|| ParseError::Syntax {
            message: "unterminated quoted string".into(),
            offset: Some(at + 1),
        })?;
        advance(&mut s, &mut at, consumed);
        let (args_part, using_part) = split_at_using(s);
        let args: Vec<String> = split_top_level(args_part, ',')
            .into_iter()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect();
        let args_text_len = args_part.len();
        // The leading comma of the argument list, if any.
        if !args_part.trim().is_empty() && !args_part.trim_start().starts_with(',') {
            return Err(syntax_near(body, at + 1));
        }
        advance(&mut s, &mut at, args_text_len);
        let placeholders = count_placeholders(&text);
        if placeholders > args.len() {
            return Err(ParseError::Compile {
                message: "too few parameters specified for RAISE".into(),
                line,
            });
        }
        if placeholders < args.len() {
            return Err(ParseError::Compile {
                message: "too many parameters specified for RAISE".into(),
                line,
            });
        }
        let _ = using_part;
        (RaiseWhat::Format { text, args }, s)
    } else {
        let (word, _) = split_first_word(s);
        if word.is_empty() || word.eq_ignore_ascii_case("using") {
            (RaiseWhat::Nothing, s)
        } else if word.eq_ignore_ascii_case("sqlstate") {
            advance(&mut s, &mut at, word.len());
            skip_ws(&mut s, &mut at);
            let (code, consumed) = string_literal(s).ok_or_else(|| syntax_near(body, at + 1))?;
            advance(&mut s, &mut at, consumed);
            (RaiseWhat::Sqlstate(code), s)
        } else {
            advance(&mut s, &mut at, word.len());
            (RaiseWhat::Condition(word.to_ascii_lowercase()), s)
        }
    };
    let mut s = remaining;
    skip_ws(&mut s, &mut at);
    let mut options = Vec::new();
    if !s.is_empty() {
        let (word, _) = split_first_word(s);
        if !word.eq_ignore_ascii_case("using") {
            return Err(syntax_near(body, at + 1));
        }
        advance(&mut s, &mut at, word.len());
        for item in split_top_level(s, ',') {
            let item_at = at + 1;
            let trimmed = item.trim_start();
            let name_at = item_at + (item.len() - trimmed.len());
            let (name, value) = trimmed
                .split_once('=')
                .ok_or_else(|| syntax_near(body, name_at))?;
            let name = name.trim();
            let upper = name.to_ascii_uppercase();
            if !RAISE_OPTIONS.contains(&upper.as_str()) {
                return Err(ParseError::Syntax {
                    message: format!("unrecognized RAISE statement option at or near \"{name}\""),
                    offset: Some(name_at),
                });
            }
            let value = value.trim();
            if value.is_empty() {
                return Err(syntax_near(body, name_at + name.len()));
            }
            options.push((upper, value.to_string()));
            at += item.chars().count() + 1;
        }
    }
    Ok(Stmt::Raise(Raise {
        level,
        what,
        options,
        line,
    }))
}

/// `%` placeholders in a RAISE format, with `%%` standing for a literal `%`.
fn count_placeholders(text: &str) -> usize {
    let mut n = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            if chars.peek() == Some(&'%') {
                chars.next();
            } else {
                n += 1;
            }
        }
    }
    n
}

/// Render a RAISE format with its evaluated arguments: `%` takes the next
/// argument (a NULL prints as `<NULL>`), `%%` is a literal `%`.
pub fn render_format(text: &str, args: &[Option<String>]) -> String {
    let mut out = String::new();
    let mut next = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'%') {
            chars.next();
            out.push('%');
            continue;
        }
        match args.get(next) {
            Some(Some(v)) => out.push_str(v),
            Some(None) => out.push_str("<NULL>"),
            None => out.push('%'),
        }
        next += 1;
    }
    out
}

/// The SQLSTATE for a condition name, or `None` for an unknown one.
pub fn condition_sqlstate(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    CONDITIONS
        .iter()
        .find(|(n, _)| *n == lower)
        .map(|(_, code)| *code)
}

/// Whether text is a well-formed SQLSTATE: five upper-case alphanumerics.
pub fn is_sqlstate(text: &str) -> bool {
    text.len() == 5
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
}

// ---- lexical helpers -------------------------------------------------------

fn skip_space(chars: &[char], mut pos: usize) -> usize {
    loop {
        while pos < chars.len() && chars[pos].is_whitespace() {
            pos += 1;
        }
        if chars.get(pos) == Some(&'-') && chars.get(pos + 1) == Some(&'-') {
            while pos < chars.len() && chars[pos] != '\n' {
                pos += 1;
            }
            continue;
        }
        if chars.get(pos) == Some(&'/') && chars.get(pos + 1) == Some(&'*') {
            pos += 2;
            while pos < chars.len() && !(chars[pos] == '*' && chars.get(pos + 1) == Some(&'/')) {
                pos += 1;
            }
            pos = (pos + 2).min(chars.len());
            continue;
        }
        return pos;
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The identifier word at `pos` (empty when there is none) and the position
/// just after it.
fn word_at(chars: &[char], pos: usize) -> (String, usize) {
    let mut end = pos;
    while end < chars.len() && is_word_char(chars[end]) {
        end += 1;
    }
    (chars[pos..end].iter().collect(), end)
}

fn last_word_start(chars: &[char], end: usize) -> usize {
    let mut e = end;
    while e > 0 && chars[e - 1].is_whitespace() {
        e -= 1;
    }
    let mut s = e;
    while s > 0 && is_word_char(chars[s - 1]) {
        s -= 1;
    }
    s
}

fn syntax_near(chars: &[char], pos: usize) -> ParseError {
    let pos = pos.min(chars.len());
    let (word, _) = word_at(chars, pos);
    let token = if word.is_empty() {
        chars.get(pos).map(|c| c.to_string()).unwrap_or_default()
    } else {
        word
    };
    if token.is_empty() {
        return ParseError::Syntax {
            message: "syntax error at end of input".into(),
            offset: Some(pos + 1),
        };
    }
    ParseError::Syntax {
        message: format!("syntax error at or near \"{token}\""),
        offset: Some(pos + 1),
    }
}

/// The index of the `;` that ends the statement starting at `pos`, skipping
/// quoted strings and parenthesised groups.
fn statement_end(chars: &[char], pos: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = pos;
    while i < chars.len() {
        match chars[i] {
            '\'' => {
                i += 1;
                loop {
                    if i >= chars.len() {
                        return None;
                    }
                    if chars[i] == '\'' {
                        if chars.get(i + 1) == Some(&'\'') {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            '"' => {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    i += 1;
                }
            }
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ';' if depth <= 0 => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

fn split_first_word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    let end = text
        .char_indices()
        .find(|(_, c)| !is_word_char(*c))
        .map_or(text.len(), |(i, _)| i);
    (&text[..end], text[end..].trim_start())
}

/// A leading `'...'` literal: its unescaped text and the BYTE length consumed.
fn string_literal(text: &str) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut iter = text.char_indices().peekable();
    let (_, q) = iter.next()?;
    if q != '\'' {
        return None;
    }
    while let Some((i, c)) = iter.next() {
        if c == '\'' {
            if iter.peek().map(|(_, c)| *c) == Some('\'') {
                iter.next();
                out.push('\'');
                continue;
            }
            return Some((out, i + 1));
        }
        out.push(c);
    }
    None
}

/// Split at top-level occurrences of `sep` (outside quotes and brackets).
fn split_top_level(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut in_quote: Option<char> = None;
    let mut start = 0;
    let bytes: Vec<(usize, char)> = text.char_indices().collect();
    let mut k = 0;
    while k < bytes.len() {
        let (i, c) = bytes[k];
        if let Some(q) = in_quote {
            if c == q {
                if q == '\'' && bytes.get(k + 1).map(|(_, c)| *c) == Some('\'') {
                    k += 2;
                    continue;
                }
                in_quote = None;
            }
        } else {
            match c {
                '\'' | '"' => in_quote = Some(c),
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                _ if c == sep && depth <= 0 => {
                    parts.push(&text[start..i]);
                    start = i + c.len_utf8();
                }
                _ => {}
            }
        }
        k += 1;
    }
    parts.push(&text[start..]);
    parts
}

/// The byte index of a top-level keyword (outside quotes / brackets, bounded
/// by non-word characters), case-insensitively.
fn find_top_level_word(text: &str, word: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_quote: Option<char> = None;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut k = 0;
    while k < chars.len() {
        let (i, c) = chars[k];
        if let Some(q) = in_quote {
            if c == q {
                if q == '\'' && chars.get(k + 1).map(|(_, c)| *c) == Some('\'') {
                    k += 2;
                    continue;
                }
                in_quote = None;
            }
            k += 1;
            continue;
        }
        match c {
            '\'' | '"' => in_quote = Some(c),
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ if depth <= 0 && is_word_char(c) && (k == 0 || !is_word_char(chars[k - 1].1)) => {
                let (w, _) = split_first_word(&text[i..]);
                if w.eq_ignore_ascii_case(word) {
                    return Some(i);
                }
                k += w.chars().count();
                continue;
            }
            _ => {}
        }
        k += 1;
    }
    None
}

fn has_top_level_word(text: &str, word: &str) -> bool {
    find_top_level_word(text, word).is_some()
}

/// Split `, a, b using x = y` into the argument part and the `using ...`
/// part (the latter starting at the keyword).
fn split_at_using(text: &str) -> (&str, &str) {
    match find_top_level_word(text, "using") {
        Some(i) => (&text[..i], &text[i..]),
        None => (text, ""),
    }
}

/// PL/pgSQL condition names and their SQLSTATEs, from PostgreSQL 16's
/// `errcodes.txt` (`raise exception division_by_zero` is `22012`).
pub const CONDITIONS: &[(&str, &str)] = &[
    ("successful_completion", "00000"),
    ("warning", "01000"),
    ("dynamic_result_sets_returned", "0100C"),
    ("implicit_zero_bit_padding", "01008"),
    ("null_value_eliminated_in_set_function", "01003"),
    ("privilege_not_granted", "01007"),
    ("privilege_not_revoked", "01006"),
    ("string_data_right_truncation", "01004"),
    ("deprecated_feature", "01P01"),
    ("no_data", "02000"),
    ("no_additional_dynamic_result_sets_returned", "02001"),
    ("sql_statement_not_yet_complete", "03000"),
    ("connection_exception", "08000"),
    ("connection_does_not_exist", "08003"),
    ("connection_failure", "08006"),
    ("sqlclient_unable_to_establish_sqlconnection", "08001"),
    ("sqlserver_rejected_establishment_of_sqlconnection", "08004"),
    ("transaction_resolution_unknown", "08007"),
    ("protocol_violation", "08P01"),
    ("triggered_action_exception", "09000"),
    ("feature_not_supported", "0A000"),
    ("invalid_transaction_initiation", "0B000"),
    ("locator_exception", "0F000"),
    ("invalid_locator_specification", "0F001"),
    ("invalid_grantor", "0L000"),
    ("invalid_grant_operation", "0LP01"),
    ("invalid_role_specification", "0P000"),
    ("diagnostics_exception", "0Z000"),
    (
        "stacked_diagnostics_accessed_without_active_handler",
        "0Z002",
    ),
    ("case_not_found", "20000"),
    ("cardinality_violation", "21000"),
    ("data_exception", "22000"),
    ("array_subscript_error", "2202E"),
    ("character_not_in_repertoire", "22021"),
    ("datetime_field_overflow", "22008"),
    ("division_by_zero", "22012"),
    ("error_in_assignment", "22005"),
    ("escape_character_conflict", "2200B"),
    ("indicator_overflow", "22022"),
    ("interval_field_overflow", "22015"),
    ("invalid_argument_for_logarithm", "2201E"),
    ("invalid_argument_for_ntile_function", "22014"),
    ("invalid_argument_for_nth_value_function", "22016"),
    ("invalid_argument_for_power_function", "2201F"),
    ("invalid_argument_for_width_bucket_function", "2201G"),
    ("invalid_character_value_for_cast", "22018"),
    ("invalid_datetime_format", "22007"),
    ("invalid_escape_character", "22019"),
    ("invalid_escape_octet", "2200D"),
    ("invalid_escape_sequence", "22025"),
    ("nonstandard_use_of_escape_character", "22P06"),
    ("invalid_indicator_parameter_value", "22010"),
    ("invalid_parameter_value", "22023"),
    ("invalid_preceding_or_following_size", "22013"),
    ("invalid_regular_expression", "2201B"),
    ("invalid_row_count_in_limit_clause", "2201W"),
    ("invalid_row_count_in_result_offset_clause", "2201X"),
    ("invalid_tablesample_argument", "2202H"),
    ("invalid_tablesample_repeat", "2202G"),
    ("invalid_time_zone_displacement_value", "22009"),
    ("invalid_use_of_escape_character", "2200C"),
    ("most_specific_type_mismatch", "2200G"),
    ("null_value_not_allowed", "22004"),
    ("null_value_no_indicator_parameter", "22002"),
    ("numeric_value_out_of_range", "22003"),
    ("sequence_generator_limit_exceeded", "2200H"),
    ("string_data_length_mismatch", "22026"),
    ("substring_error", "22011"),
    ("trim_error", "22027"),
    ("unterminated_c_string", "22024"),
    ("zero_length_character_string", "2200F"),
    ("floating_point_exception", "22P01"),
    ("invalid_text_representation", "22P02"),
    ("invalid_binary_representation", "22P03"),
    ("bad_copy_file_format", "22P04"),
    ("untranslatable_character", "22P05"),
    ("not_an_xml_document", "2200L"),
    ("invalid_xml_document", "2200M"),
    ("invalid_xml_content", "2200N"),
    ("invalid_xml_comment", "2200S"),
    ("invalid_xml_processing_instruction", "2200T"),
    ("duplicate_json_object_key_value", "22030"),
    ("invalid_argument_for_sql_json_datetime_function", "22031"),
    ("invalid_json_text", "22032"),
    ("invalid_sql_json_subscript", "22033"),
    ("more_than_one_sql_json_item", "22034"),
    ("no_sql_json_item", "22035"),
    ("non_numeric_sql_json_item", "22036"),
    ("non_unique_keys_in_a_json_object", "22037"),
    ("singleton_sql_json_item_required", "22038"),
    ("sql_json_array_not_found", "22039"),
    ("sql_json_member_not_found", "2203A"),
    ("sql_json_number_not_found", "2203B"),
    ("sql_json_object_not_found", "2203C"),
    ("too_many_json_array_elements", "2203D"),
    ("too_many_json_object_members", "2203E"),
    ("sql_json_scalar_required", "2203F"),
    ("sql_json_item_cannot_be_cast_to_target_type", "2203G"),
    ("integrity_constraint_violation", "23000"),
    ("restrict_violation", "23001"),
    ("not_null_violation", "23502"),
    ("foreign_key_violation", "23503"),
    ("unique_violation", "23505"),
    ("check_violation", "23514"),
    ("exclusion_violation", "23P01"),
    ("invalid_cursor_state", "24000"),
    ("invalid_transaction_state", "25000"),
    ("active_sql_transaction", "25001"),
    ("branch_transaction_already_active", "25002"),
    ("held_cursor_requires_same_isolation_level", "25008"),
    ("inappropriate_access_mode_for_branch_transaction", "25003"),
    (
        "inappropriate_isolation_level_for_branch_transaction",
        "25004",
    ),
    ("no_active_sql_transaction_for_branch_transaction", "25005"),
    ("read_only_sql_transaction", "25006"),
    ("schema_and_data_statement_mixing_not_supported", "25007"),
    ("no_active_sql_transaction", "25P01"),
    ("in_failed_sql_transaction", "25P02"),
    ("idle_in_transaction_session_timeout", "25P03"),
    ("invalid_sql_statement_name", "26000"),
    ("triggered_data_change_violation", "27000"),
    ("invalid_authorization_specification", "28000"),
    ("invalid_password", "28P01"),
    ("dependent_privilege_descriptors_still_exist", "2B000"),
    ("dependent_objects_still_exist", "2BP01"),
    ("invalid_transaction_termination", "2D000"),
    ("sql_routine_exception", "2F000"),
    ("function_executed_no_return_statement", "2F005"),
    ("modifying_sql_data_not_permitted", "2F002"),
    ("prohibited_sql_statement_attempted", "2F003"),
    ("reading_sql_data_not_permitted", "2F004"),
    ("invalid_cursor_name", "34000"),
    ("external_routine_exception", "38000"),
    ("containing_sql_not_permitted", "38001"),
    ("external_routine_invocation_exception", "39000"),
    ("invalid_sqlstate_returned", "39001"),
    ("trigger_protocol_violated", "39P01"),
    ("srf_protocol_violated", "39P02"),
    ("event_trigger_protocol_violated", "39P03"),
    ("savepoint_exception", "3B000"),
    ("invalid_savepoint_specification", "3B001"),
    ("invalid_catalog_name", "3D000"),
    ("invalid_schema_name", "3F000"),
    ("transaction_rollback", "40000"),
    ("transaction_integrity_constraint_violation", "40002"),
    ("serialization_failure", "40001"),
    ("statement_completion_unknown", "40003"),
    ("deadlock_detected", "40P01"),
    ("syntax_error_or_access_rule_violation", "42000"),
    ("syntax_error", "42601"),
    ("insufficient_privilege", "42501"),
    ("cannot_coerce", "42846"),
    ("grouping_error", "42803"),
    ("windowing_error", "42P20"),
    ("invalid_recursion", "42P19"),
    ("invalid_foreign_key", "42830"),
    ("invalid_name", "42602"),
    ("name_too_long", "42622"),
    ("reserved_name", "42939"),
    ("datatype_mismatch", "42804"),
    ("indeterminate_datatype", "42P18"),
    ("collation_mismatch", "42P21"),
    ("indeterminate_collation", "42P22"),
    ("wrong_object_type", "42809"),
    ("generated_always", "428C9"),
    ("undefined_column", "42703"),
    ("undefined_function", "42883"),
    ("undefined_table", "42P01"),
    ("undefined_parameter", "42P02"),
    ("undefined_object", "42704"),
    ("duplicate_column", "42701"),
    ("duplicate_cursor", "42P03"),
    ("duplicate_database", "42P04"),
    ("duplicate_function", "42723"),
    ("duplicate_prepared_statement", "42P05"),
    ("duplicate_schema", "42P06"),
    ("duplicate_table", "42P07"),
    ("duplicate_alias", "42712"),
    ("duplicate_object", "42710"),
    ("ambiguous_column", "42702"),
    ("ambiguous_function", "42725"),
    ("ambiguous_parameter", "42P08"),
    ("ambiguous_alias", "42P09"),
    ("invalid_column_reference", "42P10"),
    ("invalid_column_definition", "42611"),
    ("invalid_cursor_definition", "42P11"),
    ("invalid_database_definition", "42P12"),
    ("invalid_function_definition", "42P13"),
    ("invalid_prepared_statement_definition", "42P14"),
    ("invalid_schema_definition", "42P15"),
    ("invalid_table_definition", "42P16"),
    ("invalid_object_definition", "42P17"),
    ("with_check_option_violation", "44000"),
    ("insufficient_resources", "53000"),
    ("disk_full", "53100"),
    ("out_of_memory", "53200"),
    ("too_many_connections", "53300"),
    ("configuration_limit_exceeded", "53400"),
    ("program_limit_exceeded", "54000"),
    ("statement_too_complex", "54001"),
    ("too_many_columns", "54011"),
    ("too_many_arguments", "54023"),
    ("object_not_in_prerequisite_state", "55000"),
    ("object_in_use", "55006"),
    ("cant_change_runtime_param", "55P02"),
    ("lock_not_available", "55P03"),
    ("unsafe_new_enum_value_usage", "55P04"),
    ("operator_intervention", "57000"),
    ("query_canceled", "57014"),
    ("admin_shutdown", "57P01"),
    ("crash_shutdown", "57P02"),
    ("cannot_connect_now", "57P03"),
    ("database_dropped", "57P04"),
    ("idle_session_timeout", "57P05"),
    ("system_error", "58000"),
    ("io_error", "58030"),
    ("undefined_file", "58P01"),
    ("duplicate_file", "58P02"),
    ("snapshot_too_old", "72000"),
    ("config_file_error", "F0000"),
    ("lock_file_exists", "F0001"),
    ("fdw_error", "HV000"),
    ("fdw_column_name_not_found", "HV005"),
    ("fdw_dynamic_parameter_value_needed", "HV002"),
    ("fdw_function_sequence_error", "HV010"),
    ("fdw_inconsistent_descriptor_information", "HV021"),
    ("fdw_invalid_attribute_value", "HV024"),
    ("fdw_invalid_column_name", "HV007"),
    ("fdw_invalid_column_number", "HV008"),
    ("fdw_invalid_data_type", "HV004"),
    ("fdw_invalid_data_type_descriptors", "HV006"),
    ("fdw_invalid_descriptor_field_identifier", "HV091"),
    ("fdw_invalid_handle", "HV00B"),
    ("fdw_invalid_option_index", "HV00C"),
    ("fdw_invalid_option_name", "HV00D"),
    ("fdw_invalid_string_length_or_buffer_length", "HV090"),
    ("fdw_invalid_string_format", "HV00A"),
    ("fdw_invalid_use_of_null_pointer", "HV009"),
    ("fdw_too_many_handles", "HV014"),
    ("fdw_out_of_memory", "HV001"),
    ("fdw_no_schemas", "HV00P"),
    ("fdw_option_name_not_found", "HV00J"),
    ("fdw_reply_handle", "HV00K"),
    ("fdw_schema_not_found", "HV00Q"),
    ("fdw_table_not_found", "HV00R"),
    ("fdw_unable_to_create_execution", "HV00L"),
    ("fdw_unable_to_create_reply", "HV00M"),
    ("fdw_unable_to_establish_connection", "HV00N"),
    ("plpgsql_error", "P0000"),
    ("raise_exception", "P0001"),
    ("no_data_found", "P0002"),
    ("too_many_rows", "P0003"),
    ("assert_failure", "P0004"),
    ("internal_error", "XX000"),
    ("data_corrupted", "XX001"),
    ("index_corrupted", "XX002"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notice_with_args() {
        let stmts = parse("begin raise notice 'hello %', chr(8364); end").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::Raise(Raise {
                level: Level::Notice,
                what: RaiseWhat::Format {
                    text: "hello %".into(),
                    args: vec!["chr(8364)".into()],
                },
                options: vec![],
                line: 1,
            })]
        );
    }

    #[test]
    fn exception_with_using_on_line_two() {
        let stmts =
            parse("begin\nraise exception 'made up code' using errcode = 'PXX99';\nend").unwrap();
        let Stmt::Raise(r) = &stmts[0] else { panic!() };
        assert_eq!(r.level, Level::Exception);
        assert_eq!(r.line, 2);
        assert_eq!(
            r.options,
            vec![("ERRCODE".to_string(), "'PXX99'".to_string())]
        );
    }

    #[test]
    fn execute_and_null() {
        let stmts = parse(
            "begin\n    execute format('insert into \"%s\" values (1)', chr(8364));\n null; end;",
        )
        .unwrap();
        assert_eq!(
            stmts[0],
            Stmt::Execute {
                query: "format('insert into \"%s\" values (1)', chr(8364))".into(),
                line: 2
            }
        );
        assert_eq!(stmts[1], Stmt::Null);
    }

    #[test]
    fn parameter_count_is_a_compile_error() {
        assert_eq!(
            parse("begin raise exception 'x %'; end"),
            Err(ParseError::Compile {
                message: "too few parameters specified for RAISE".into(),
                line: 1
            })
        );
        assert_eq!(
            parse("begin raise exception 'x', 1; end"),
            Err(ParseError::Compile {
                message: "too many parameters specified for RAISE".into(),
                line: 1
            })
        );
        assert!(parse("begin raise exception 'x %% % %', 1, null; end").is_ok());
    }

    #[test]
    fn unknown_option_has_a_position() {
        assert_eq!(
            parse("begin raise exception 'x' using sqlstate='22012'; end"),
            Err(ParseError::Syntax {
                message: "unrecognized RAISE statement option at or near \"sqlstate\"".into(),
                offset: Some(33)
            })
        );
    }

    #[test]
    fn missing_semicolon_trips_on_end() {
        assert_eq!(
            parse("begin raise notice 'a' end"),
            Err(ParseError::Syntax {
                message: "syntax error at or near \"end\"".into(),
                offset: Some(24)
            })
        );
    }

    #[test]
    fn condition_and_sqlstate_forms() {
        let stmts = parse("BEGIN RAISE EXCEPTION division_by_zero; raise sqlstate '22012'; raise exception using message='m'; END").unwrap();
        let Stmt::Raise(a) = &stmts[0] else { panic!() };
        assert_eq!(a.what, RaiseWhat::Condition("division_by_zero".into()));
        let Stmt::Raise(b) = &stmts[1] else { panic!() };
        assert_eq!(b.what, RaiseWhat::Sqlstate("22012".into()));
        let Stmt::Raise(c) = &stmts[2] else { panic!() };
        assert_eq!(c.what, RaiseWhat::Nothing);
        assert_eq!(c.options, vec![("MESSAGE".to_string(), "'m'".to_string())]);
        assert_eq!(condition_sqlstate("division_by_zero"), Some("22012"));
        assert_eq!(condition_sqlstate("no_such_thing"), None);
    }

    #[test]
    fn render() {
        assert_eq!(
            render_format("x %% % %", &[Some("1".into()), None]),
            "x % 1 <NULL>"
        );
    }

    #[test]
    fn unsupported_constructs() {
        assert!(matches!(
            parse("declare x int; begin raise notice 'a'; end"),
            Err(ParseError::Unsupported(_))
        ));
        assert!(matches!(
            parse("begin if true then null; end if; end"),
            Err(ParseError::Unsupported(_))
        ));
    }
}
