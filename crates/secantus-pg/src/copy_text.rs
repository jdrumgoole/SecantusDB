//! COPY's text and CSV formats under the statement's options (RP-073):
//! the DELIMITER, NULL string, QUOTE, ESCAPE and FORCE_* columns, written
//! as PostgreSQL 15's `CopyAttributeOutText` / `CopyAttributeOutCSV` write
//! them and read as `CopyReadAttributesText` / `CopyReadAttributesCSV` read
//! them (`commands/copyto.c`, `copyfromparse.c`).
//!
//! The two formats keep NULL apart from the empty string differently:
//! TEXT spells NULL with its NULL string (`\N`) and escapes the data, CSV
//! spells it as the unquoted NULL string (empty by default) and quotes any
//! value that would read back as it.

use secantus_pgplan::copy_opts::CopyOptions;

/// Append one field to a row being written. `force` is FORCE_QUOTE for this
/// column, `single` whether the row has one column (a CSV `\.` is quoted
/// then, or it would read back as the end of the data).
pub(crate) fn write_field(
    out: &mut String,
    value: Option<&str>,
    csv: bool,
    opts: &CopyOptions,
    force: bool,
    single: bool,
) {
    let Some(text) = value else {
        out.push_str(&opts.null);
        return;
    };
    if csv {
        let (quote, escape, delim) = (opts.quote, opts.escape, opts.delimiter);
        let use_quote = force
            || text == opts.null
            || (single && text == "\\.")
            || text
                .chars()
                .any(|c| c == delim || c == quote || c == '\n' || c == '\r');
        if !use_quote {
            out.push_str(text);
            return;
        }
        out.push(quote);
        for c in text.chars() {
            if c == quote || c == escape {
                out.push(escape);
            }
            out.push(c);
        }
        out.push(quote);
        return;
    }
    for c in text.chars() {
        match c {
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' if opts.delimiter != '\t' => out.push_str("\\t"),
            '\u{0b}' => out.push_str("\\v"),
            '\\' => out.push_str("\\\\"),
            c if c == opts.delimiter => {
                // The delimiter is backslashed; a control character used as
                // one is too.
                out.push('\\');
                out.push(match c {
                    '\t' => 't',
                    other => other,
                });
            }
            c => out.push(c),
        }
    }
}

/// Parse COPY text or CSV data into rows of fields, `None` for NULL, with
/// FORCE_NOT_NULL / FORCE_NULL applied per column position.
pub(crate) fn parse(
    text: &str,
    csv: bool,
    opts: &CopyOptions,
    force_not_null: &[bool],
    force_null: &[bool],
) -> Vec<Vec<Option<String>>> {
    parse_lines(text, csv, opts, force_not_null, force_null)
        .into_iter()
        .map(|(row, _)| row)
        .collect()
}

/// `parse`, each row with its line as it arrived (without the newline): what
/// a COPY error's CONTEXT quotes.
pub(crate) fn parse_lines(
    text: &str,
    csv: bool,
    opts: &CopyOptions,
    force_not_null: &[bool],
    force_null: &[bool],
) -> Vec<(Vec<Option<String>>, String)> {
    let mut rows = if csv {
        parse_csv(text, opts)
    } else {
        parse_text(text, opts)
    };
    if csv {
        for (row, _) in &mut rows {
            for (i, field) in row.iter_mut().enumerate() {
                if field.is_none() && force_not_null.get(i).copied().unwrap_or(false) {
                    *field = Some(opts.null.clone());
                } else if force_null.get(i).copied().unwrap_or(false)
                    && field.as_deref() == Some(opts.null.as_str())
                {
                    *field = None;
                }
            }
        }
    }
    rows
}

fn parse_text(text: &str, opts: &CopyOptions) -> Vec<(Vec<Option<String>>, String)> {
    let delim = opts.delimiter as u8;
    let mut rows = Vec::new();
    for line in text.split('\n') {
        // A trailing newline leaves an empty final line, and `\.` is the
        // end-of-data marker of the historical protocol.
        if line.is_empty() {
            continue;
        }
        if line == "\\." {
            break;
        }
        let bytes = line.as_bytes();
        let mut fields = Vec::new();
        let mut start = 0;
        let mut i = 0;
        loop {
            if i >= bytes.len() || bytes[i] == delim {
                let raw = &line[start..i];
                // The NULL string is matched against the raw field, before
                // its escapes are read.
                fields.push(if raw == opts.null {
                    None
                } else {
                    Some(crate::unescape_copy_text(raw))
                });
                if i >= bytes.len() {
                    break;
                }
                i += 1;
                start = i;
                continue;
            }
            // A backslash takes the next byte with it, the delimiter
            // included.
            i += if bytes[i] == b'\\' { 2 } else { 1 };
            i = i.min(bytes.len());
        }
        rows.push((fields, line.trim_end_matches('\r').to_string()));
    }
    rows
}

/// CSV, where a newline inside quotes is DATA rather than a row separator --
/// so the rows cannot be found by splitting on newlines first.
fn parse_csv(text: &str, opts: &CopyOptions) -> Vec<(Vec<Option<String>>, String)> {
    let (delim, quote, escape) = (opts.delimiter, opts.quote, opts.escape);
    let mut rows = Vec::new();
    let mut row: Vec<Option<String>> = Vec::new();
    let mut field = String::new();
    let mut in_quote = false;
    let mut saw_quote = false;
    let mut line_start = true;
    let finish = |field: &mut String, saw_quote: &mut bool| -> Option<String> {
        let value = std::mem::take(field);
        let null = !*saw_quote && value == opts.null;
        *saw_quote = false;
        (!null).then_some(value)
    };
    // Where the row being read starts in `text`, for its line.
    let mut row_start = 0usize;
    let line_of = |start: usize, end: usize| text[start..end].trim_end_matches('\r').to_string();
    let mut consumed = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let pos = consumed;
        consumed += c.len_utf8();
        if in_quote {
            if c == escape && escape != quote {
                if let Some(&next) = chars.peek() {
                    if next == quote || next == escape {
                        field.push(next);
                        chars.next();
                        consumed += next.len_utf8();
                        continue;
                    }
                }
            }
            if c == quote {
                // With ESCAPE equal to QUOTE a doubled quote is one quote.
                if escape == quote && chars.peek() == Some(&quote) {
                    field.push(quote);
                    chars.next();
                    consumed += quote.len_utf8();
                } else {
                    in_quote = false;
                }
                continue;
            }
            field.push(c);
            continue;
        }
        // `\.` alone on an unquoted line ends the data.
        if line_start && c == '\\' && chars.peek() == Some(&'.') {
            let mut ahead = chars.clone();
            ahead.next();
            if matches!(ahead.peek(), None | Some('\n') | Some('\r')) {
                break;
            }
        }
        line_start = false;
        if c == quote {
            in_quote = true;
            saw_quote = true;
        } else if c == delim {
            row.push(finish(&mut field, &mut saw_quote));
        } else if c == '\n' {
            row.push(finish(&mut field, &mut saw_quote));
            rows.push((std::mem::take(&mut row), line_of(row_start, pos)));
            row_start = consumed;
            line_start = true;
        } else if c == '\r' {
            // `\r\n` ends a line as `\n` does.
        } else {
            field.push(c);
        }
    }
    if !field.is_empty() || saw_quote || !row.is_empty() {
        row.push(finish(&mut field, &mut saw_quote));
        rows.push((row, line_of(row_start, text.len())));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(csv: bool) -> CopyOptions {
        let mut o = CopyOptions::default();
        if csv {
            o.delimiter = ',';
            o.null = String::new();
        }
        o
    }

    fn row(values: &[Option<&str>], csv: bool, o: &CopyOptions, force: bool) -> String {
        let mut out = String::new();
        for (i, v) in values.iter().enumerate() {
            if i > 0 {
                out.push(o.delimiter);
            }
            write_field(&mut out, *v, csv, o, force, values.len() == 1);
        }
        out
    }

    /// Each line written by PostgreSQL 15.19 (2026-10-22).
    #[test]
    fn writes_as_postgresql() {
        let mut text = opts(false);
        text.delimiter = ',';
        assert_eq!(
            row(&[Some("1"), Some("q\"u,o"), None], false, &text, false),
            "1,q\"u\\,o,\\N"
        );
        let mut csv = opts(true);
        csv.delimiter = ';';
        csv.null = "x".into();
        assert_eq!(
            row(&[Some("4"), Some("NULL"), Some("x")], true, &csv, false),
            "4;NULL;\"x\""
        );
        assert_eq!(row(&[Some("2"), Some(""), None], true, &csv, false), "2;;x");
        let mut q = opts(true);
        q.quote = '|';
        q.escape = '!';
        assert_eq!(
            row(&[Some("q\"u,o"), Some("")], true, &q, false),
            "|q\"u,o|,||"
        );
        assert_eq!(row(&[Some("1")], true, &opts(true), true), "\"1\"");
    }

    #[test]
    fn reads_as_postgresql() {
        let o = opts(true);
        assert_eq!(
            parse("5,,\n", true, &o, &[false, true, false], &[]),
            vec![vec![Some("5".into()), Some(String::new()), None]]
        );
        assert_eq!(
            parse("6,\"\",z\n", true, &o, &[], &[false, true, false]),
            vec![vec![Some("6".into()), None, Some("z".into())]]
        );
        let mut e = opts(true);
        e.escape = '!';
        assert_eq!(
            parse("14,\"a!\"b\",\n", true, &e, &[], &[]),
            vec![vec![Some("14".into()), Some("a\"b".into()), None]]
        );
        let mut t = opts(false);
        t.delimiter = ',';
        assert_eq!(
            parse("8,p\\,q,\\N\n", false, &t, &[], &[]),
            vec![vec![Some("8".into()), Some("p,q".into()), None]]
        );
    }
}
