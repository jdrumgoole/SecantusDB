//! `query_to_xml` and `table_to_xml`: a query's rows as XML, in PostgreSQL
//! 15's SQL/XML mapping (xml.c's `SPI_sql_row_to_xmlelement`,
//! `map_sql_identifier_to_xml_name` and `map_sql_value_to_xml_value`,
//! measured on 15.19).
//!
//! The query runs through the executor as `SELECT q FROM (<query>) q`, so
//! each row comes back as one record carrying its fields' names and types.

use super::*;

const XSI: &str = " xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"";

/// `map_sql_identifier_to_xml_name(ident, fully_escaped = true, false)`: a
/// character no XML name may hold is `_xHHHH_`, as is `:`, the `_` of a
/// `_x`, and the first letter of a name starting `xml` in any case.
fn xml_name(ident: &str) -> String {
    let chars: Vec<char> = ident.chars().collect();
    let mut out = String::new();
    let lower = ident.to_lowercase();
    for (i, &c) in chars.iter().enumerate() {
        let escape = c == ':'
            || (c == '_' && chars.get(i + 1) == Some(&'x'))
            || (i == 0 && lower.starts_with("xml"))
            || (i == 0 && !(c.is_alphabetic() || c == '_'))
            || (i > 0 && !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.')));
        if escape {
            out.push_str(&format!("_x{:04X}_", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

/// Text for XML content: `&`, `<`, `>` and a carriage return escaped.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#x0d;"),
            _ => out.push(c),
        }
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `map_sql_value_to_xml_value`: a value as XML content, by its type.
fn xml_value(v: &Bson, ty: &str) -> String {
    if let Some(element) = ty.strip_suffix("[]") {
        fn flat(v: &Bson, out: &mut Vec<Bson>) {
            match v {
                Bson::Array(items) => items.iter().for_each(|i| flat(i, out)),
                other => out.push(other.clone()),
            }
        }
        let mut items = Vec::new();
        flat(&arrays::strip(v), &mut items);
        let mut out = String::new();
        for i in items.iter().filter(|i| **i != Bson::Null) {
            out.push_str(&format!("<element>{}</element>", xml_value(i, element)));
        }
        return out;
    }
    let text = if typeout::needs_type(ty) {
        typeout::output_text(v, ty)
    } else {
        render_value_text(v)
    };
    match ty {
        "bool" => (if matches!(v, Bson::Boolean(true)) {
            "true"
        } else {
            "false"
        })
        .into(),
        // xsd:dateTime: a `T` between the date and the time, and a zone
        // offset in full (`+00:00`).
        "timestamp" => text.replacen(' ', "T", 1),
        "timestamptz" => {
            let t = text.replacen(' ', "T", 1);
            match t.rfind(['+', '-']).filter(|&i| i > 10) {
                Some(i) if t[i..].len() == 3 => format!("{t}:00"),
                _ => t,
            }
        }
        "bytea" => match bytea::parse(v) {
            Ok(bytes) => base64(&bytes),
            Err(_) => escape(&text),
        },
        _ => escape(&text),
    }
}

/// The rows of `sql` as XML, the table element named `table` and each row
/// `row` (or, as a forest, each row named `forest_row`).
fn rows_to_xml(
    sql: &str,
    nulls: bool,
    forest: bool,
    ns: &str,
    table: &str,
    forest_row: &str,
) -> Result<Bson> {
    let rows = correlated::run_sql(&format!("SELECT q FROM ({sql}) q"))?;
    let attrs = if ns.is_empty() {
        XSI.to_string()
    } else {
        format!("{XSI} xmlns=\"{ns}\"")
    };
    let mut out = String::new();
    if !forest {
        out.push_str(&format!("<{table}{attrs}>\n\n"));
    }
    for row in &rows {
        let record = row.first().cloned().unwrap_or(Bson::Null);
        let values = record_values(&record).cloned().unwrap_or_default();
        let types = record_field_types(&record).unwrap_or_default();
        let names: Vec<String> = match &record {
            Bson::Document(d) => d
                .get_array(RECORD_NAMES_KEY)
                .map(|a| {
                    a.iter()
                        .map(|n| n.as_str().unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let open = if forest {
            format!("<{forest_row}{attrs}>\n")
        } else {
            "<row>\n".to_string()
        };
        out.push_str(&open);
        for (i, v) in values.iter().enumerate() {
            let name = xml_name(names.get(i).map_or("", String::as_str));
            if *v == Bson::Null {
                if nulls {
                    out.push_str(&format!("  <{name} xsi:nil=\"true\"/>\n"));
                }
                continue;
            }
            let ty = types.get(i).map_or("text", String::as_str);
            out.push_str(&format!("  <{name}>{}</{name}>\n", xml_value(v, ty)));
        }
        out.push_str(&format!(
            "</{}>\n\n",
            if forest { forest_row } else { "row" }
        ));
    }
    if !forest {
        out.push_str(&format!("</{table}>\n"));
    }
    Ok(Bson::String(out))
}

/// `query_to_xml(query, nulls, tableforest, targetns)` and
/// `table_to_xml(tbl, nulls, tableforest, targetns)`.
pub(crate) fn call(name: &str, args: &[Bson]) -> Result<Bson> {
    if args.len() != 4 {
        return Err(Error::UndefinedFunction(format!(
            "function {name} does not exist"
        )));
    }
    if args.contains(&Bson::Null) {
        return Ok(Bson::Null);
    }
    let flag = |i: usize| matches!(args[i], Bson::Boolean(true));
    let ns = render_value_text(&args[3]);
    match name {
        "query_to_xml" => rows_to_xml(
            &render_value_text(&args[0]),
            flag(1),
            flag(2),
            &ns,
            "table",
            "row",
        ),
        _ => {
            // An untyped literal is a regclass's input text.
            let oid = match regclass_oid(&args[0]) {
                Some(oid) => oid,
                None => resolve_regclass(&render_value_text(&args[0]))?,
            };
            let relation = regclass_text(oid);
            let element = xml_name(
                relation
                    .rsplit('.')
                    .next()
                    .unwrap_or(&relation)
                    .trim_matches('"'),
            );
            rows_to_xml(
                &format!("SELECT * FROM {relation}"),
                flag(1),
                flag(2),
                &ns,
                &element,
                &element,
            )
        }
    }
}
