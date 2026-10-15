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
    xml_name_escaped(ident, false)
}

/// `map_sql_identifier_to_xml_name(ident, true, escape_period)`: with
/// `escape_period`, a `.` is `_x002E_` too, as each part of a multi-part
/// name has it.
pub(crate) fn xml_name_escaped(ident: &str, escape_period: bool) -> String {
    let chars: Vec<char> = ident.chars().collect();
    let mut out = String::new();
    let lower = ident.to_lowercase();
    for (i, &c) in chars.iter().enumerate() {
        let escape = c == ':'
            || (c == '_' && chars.get(i + 1) == Some(&'x'))
            || (i == 0 && lower.starts_with("xml"))
            || (escape_period && c == '.')
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

fn to_base64(bytes: &[u8]) -> String {
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
fn xml_value(v: &Bson, ty: &str, base64: bool) -> String {
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
            out.push_str(&format!(
                "<element>{}</element>",
                xml_value(i, element, base64)
            ));
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
        // `xmlbinary`: base64, or upper-case hex (measured).
        "bytea" => match bytea::parse(v) {
            Ok(bytes) if base64 => to_base64(&bytes),
            Ok(bytes) => bytes.iter().map(|b| format!("{b:02X}")).collect(),
            Err(_) => escape(&text),
        },
        // An xml value is content already, and goes in unescaped.
        "xml" => text,
        _ => escape(&text),
    }
}

/// `xmldata_root_element_start`: the namespace declarations only at the
/// top level, and the schema's location when the schema is inside.
fn root_start(name: &str, with_schema: bool, ns: &str, top_level: bool) -> String {
    let mut out = format!("<{name}");
    if top_level {
        out.push_str(XSI);
        if !ns.is_empty() {
            out.push_str(&format!(" xmlns=\"{ns}\""));
        }
    }
    if with_schema {
        if ns.is_empty() {
            out.push_str(" xsi:noNamespaceSchemaLocation=\"#\"");
        } else {
            out.push_str(&format!(" xsi:schemaLocation=\"{ns} #\""));
        }
    }
    out.push_str(">\n");
    out
}

/// What `SPI_sql_row_to_xmlelement` writes for each row.
struct RowOpts<'a> {
    nulls: bool,
    forest: bool,
    ns: &'a str,
    top_level: bool,
    /// The forest's row element: the table's name, or `row`.
    row_name: &'a str,
    base64: bool,
}

fn row_to_xml(out: &mut String, cols: &[(String, String)], values: &[Bson], o: &RowOpts) {
    if o.forest {
        out.push_str(&root_start(o.row_name, false, o.ns, o.top_level));
    } else {
        out.push_str("<row>\n");
    }
    for (i, v) in values.iter().enumerate() {
        let (name, ty) = cols
            .get(i)
            .map_or(("", "text"), |(n, t)| (n.as_str(), t.as_str()));
        let name = xml_name(name);
        if *v == Bson::Null {
            if o.nulls {
                out.push_str(&format!("  <{name} xsi:nil=\"true\"/>\n"));
            }
            continue;
        }
        out.push_str(&format!(
            "  <{name}>{}</{name}>\n",
            xml_value(v, ty, o.base64)
        ));
    }
    if o.forest {
        out.push_str(&format!("</{}>\n\n", o.row_name));
    } else {
        out.push_str("</row>\n\n");
    }
}

/// `query_to_xml_internal`: the rows of `sql`, under an element named for
/// `tablename` (or `table`), with `xmlschema` first when given.
pub(crate) fn query_to_xml_internal(
    sql: &str,
    tablename: Option<&str>,
    xmlschema: Option<&str>,
    nulls: bool,
    forest: bool,
    ns: &str,
    top_level: bool,
) -> Result<String> {
    let xmltn = tablename.map_or_else(|| "table".to_string(), xml_name);
    xml_catalog::check_query(sql, false)?;
    let rows = correlated::run_sql(&format!("SELECT q FROM ({sql}) q"))?;
    let mut out = String::new();
    if !forest {
        out.push_str(&root_start(&xmltn, xmlschema.is_some(), ns, top_level));
        out.push('\n');
    }
    if let Some(schema) = xmlschema {
        out.push_str(&format!("{schema}\n\n"));
    }
    let row_name = tablename.map_or_else(|| "row".to_string(), xml_name);
    let opts = RowOpts {
        nulls,
        forest,
        ns,
        top_level,
        row_name: &row_name,
        base64: xml_schema::binary_is_base64(),
    };
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
        let cols: Vec<(String, String)> = names
            .into_iter()
            .enumerate()
            .map(|(i, n)| (n, types.get(i).cloned().unwrap_or_else(|| "text".into())))
            .collect();
        row_to_xml(&mut out, &cols, &values, &opts);
    }
    if !forest {
        out.push_str(&format!("</{xmltn}>\n"));
    }
    Ok(out)
}

/// The type name `xml_value` renders by, from a column's type oid: a
/// domain by its base type (`getBaseType`), an array as `element[]`.
fn value_type(oid: i64) -> String {
    let base = xml_schema::domain_base(oid).map_or(oid, |(b, _)| b);
    if let Some(name) = pgtypes::name_of_oid(base) {
        return name.to_string();
    }
    match pgtypes::BUILTIN_TYPES
        .iter()
        .find(|(_, _, arr)| *arr == base)
    {
        Some((name, _, _)) => format!("{name}[]"),
        None => "text".into(),
    }
}

/// A relation argument: a regclass, or an untyped literal read as one.
fn relation_oid(v: &Bson) -> Result<i64> {
    match regclass_oid(v) {
        Some(oid) => Ok(oid),
        None => resolve_regclass(&render_value_text(v)),
    }
}

/// `table_to_xml_internal`.
fn table_to_xml_internal(
    oid: i64,
    xmlschema: Option<&str>,
    nulls: bool,
    forest: bool,
    ns: &str,
    top_level: bool,
) -> Result<String> {
    let (relname, _) = xml_catalog::relation(oid)?;
    query_to_xml_internal(
        &format!("SELECT * FROM {}", regclass_text(oid)),
        Some(&relname),
        xmlschema,
        nulls,
        forest,
        ns,
        top_level,
    )
}

/// `schema_to_xml_internal`: each of the schema's tables in turn, by name.
fn schema_to_xml_internal(
    nspid: i64,
    xmlschema: Option<&str>,
    nulls: bool,
    forest: bool,
    ns: &str,
    top_level: bool,
) -> Result<String> {
    let xmlsn = xml_name(&xml_catalog::schema_name(nspid)?);
    let mut out = root_start(&xmlsn, xmlschema.is_some(), ns, top_level);
    out.push('\n');
    if let Some(schema) = xmlschema {
        out.push_str(&format!("{schema}\n\n"));
    }
    for rel in xml_catalog::schema_tables(nspid)? {
        out.push_str(&table_to_xml_internal(rel, None, nulls, forest, ns, false)?);
        out.push('\n');
    }
    out.push_str(&format!("</{xmlsn}>\n"));
    Ok(out)
}

/// `database_to_xml_internal`: each visible schema in turn, by name.
fn database_to_xml_internal(
    xmlschema: Option<&str>,
    nulls: bool,
    forest: bool,
    ns: &str,
) -> Result<String> {
    let xmlcn = xml_name(&xml_catalog::database_name()?);
    let mut out = root_start(&xmlcn, xmlschema.is_some(), ns, true);
    out.push('\n');
    if let Some(schema) = xmlschema {
        out.push_str(&format!("{schema}\n\n"));
    }
    for nsp in xml_catalog::database_schemas()? {
        out.push_str(&schema_to_xml_internal(
            nsp, None, nulls, forest, ns, false,
        )?);
        out.push('\n');
    }
    out.push_str(&format!("</{xmlcn}>\n"));
    Ok(out)
}

/// An open cursor's columns; 34000 when no cursor has that name.
fn cursor_columns(name: &str) -> Result<Vec<xml_schema::Col>> {
    let open = correlated::run_sql(&format!(
        "SELECT 1 FROM pg_catalog.pg_cursors WHERE name = '{}'",
        name.replace('\'', "''")
    ))?;
    if open.is_empty() {
        return Err(Error::Sqlstate(
            "34000",
            format!("cursor \"{name}\" does not exist"),
        ));
    }
    xml_schema::columns(&format!(
        "FETCH FORWARD 0 FROM \"{}\"",
        name.replace('"', "\"\"")
    ))
}

/// `cursor_to_xml(cursor, count, nulls, tableforest, targetns)`: the next
/// `count` rows of an open cursor.
fn cursor_to_xml(name: &str, count: i64, nulls: bool, forest: bool, ns: &str) -> Result<String> {
    let mut out = String::new();
    if !forest {
        out.push_str(&root_start("table", false, ns, true));
        out.push('\n');
    }
    let cols: Vec<(String, String)> = cursor_columns(name)?
        .iter()
        .map(|c| (c.name.clone(), value_type(c.oid)))
        .collect();
    let rows = correlated::fetch_cursor(name, count)?;
    let opts = RowOpts {
        nulls,
        forest,
        ns,
        top_level: true,
        row_name: "row",
        base64: xml_schema::binary_is_base64(),
    };
    for row in &rows {
        row_to_xml(&mut out, &cols, row, &opts);
    }
    if !forest {
        out.push_str("</table>\n");
    }
    Ok(out)
}

/// The arguments each mapping takes after its first.
fn arity(name: &str) -> usize {
    match name {
        "cursor_to_xml" => 5,
        "database_to_xml" | "database_to_xmlschema" | "database_to_xml_and_xmlschema" => 3,
        _ => 4,
    }
}

/// The SQL/XML mapping functions: `query_to_xml`, `table_to_xml`,
/// `cursor_to_xml`, `schema_to_xml`, `database_to_xml`, each `...xmlschema`
/// and `..._and_xmlschema` form.
pub(crate) fn call(name: &str, args: &[Bson]) -> Result<Bson> {
    if args.len() != arity(name) {
        return Err(Error::UndefinedFunction(format!(
            "function {name} does not exist"
        )));
    }
    if args.contains(&Bson::Null) {
        return Ok(Bson::Null);
    }
    // The flags and the namespace are the last three arguments.
    let n = args.len();
    let flag = |i: usize| matches!(args[i], Bson::Boolean(true));
    let (nulls, forest) = (flag(n - 3), flag(n - 2));
    let ns = render_value_text(&args[n - 1]);
    let text0 = || render_value_text(&args[0]);
    let out = match name {
        "query_to_xml" => query_to_xml_internal(&text0(), None, None, nulls, forest, &ns, true)?,
        "query_to_xmlschema" => {
            xml_catalog::check_query(&text0(), true)?;
            let cols = xml_schema::columns(&text0())?;
            xml_schema::table_xmlschema(&cols, None, nulls, forest, &ns)?
        }
        "query_to_xml_and_xmlschema" => {
            let sql = text0();
            xml_catalog::check_query(&sql, true)?;
            let cols = xml_schema::columns(&sql)?;
            let schema = xml_schema::table_xmlschema(&cols, None, nulls, forest, &ns)?;
            query_to_xml_internal(&sql, None, Some(&schema), nulls, forest, &ns, true)?
        }
        "table_to_xml" | "table_to_xmlschema" | "table_to_xml_and_xmlschema" => {
            let oid = relation_oid(&args[0])?;
            let schema = || {
                let rel = xml_catalog::relation(oid)?;
                let cols = xml_schema::relation_columns(oid)?;
                xml_schema::table_xmlschema(&cols, Some(&rel), nulls, forest, &ns)
            };
            match name {
                "table_to_xml" => table_to_xml_internal(oid, None, nulls, forest, &ns, true)?,
                "table_to_xmlschema" => schema()?,
                _ => {
                    let schema = schema()?;
                    table_to_xml_internal(oid, Some(&schema), nulls, forest, &ns, true)?
                }
            }
        }
        "cursor_to_xml" => {
            let count = match &args[1] {
                Bson::Int32(c) => i64::from(*c),
                Bson::Int64(c) => *c,
                other => render_value_text(other).parse().unwrap_or(0),
            };
            cursor_to_xml(&text0(), count, nulls, forest, &ns)?
        }
        "cursor_to_xmlschema" => {
            let cols = cursor_columns(&text0())?;
            xml_schema::table_xmlschema(&cols, None, nulls, forest, &ns)?
        }
        "schema_to_xml" => {
            let nspid = xml_catalog::schema_oid(&text0())?;
            schema_to_xml_internal(nspid, None, nulls, forest, &ns, true)?
        }
        "schema_to_xmlschema" => xml_schema::schema_xmlschema(&text0(), forest, &ns)?,
        "schema_to_xml_and_xmlschema" => {
            let nspid = xml_catalog::schema_oid(&text0())?;
            let schema = xml_schema::schema_xmlschema(&text0(), forest, &ns)?;
            schema_to_xml_internal(nspid, Some(&schema), nulls, forest, &ns, true)?
        }
        "database_to_xml" => database_to_xml_internal(None, nulls, forest, &ns)?,
        "database_to_xmlschema" => xml_schema::database_xmlschema(&ns)?,
        _ => {
            let schema = xml_schema::database_xmlschema(&ns)?;
            database_to_xml_internal(Some(&schema), nulls, forest, &ns)?
        }
    };
    Ok(Bson::String(out))
}
