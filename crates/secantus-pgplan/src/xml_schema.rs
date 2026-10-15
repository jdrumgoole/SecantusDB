//! The SQL/XML schema mappings: `query_to_xmlschema`, `table_to_xmlschema`,
//! `cursor_to_xmlschema`, the schema and database forms, and the
//! `..._and_xmlschema` forms that put the schema inside the data.
//! Transcribed from PostgreSQL 15's xml.c (`map_sql_table_to_xmlschema`,
//! `map_sql_type_to_xml_name`, `map_sql_type_to_xmlschema_type`,
//! `map_sql_typecoll_to_xmlschema_types`, `map_sql_schema_to_xmlschema_types`,
//! `map_sql_catalog_to_xmlschema_types`) and measured on 15.19.
//!
//! The catalog reads PostgreSQL does through SPI and the syscache go through
//! the executor here as SQL (`correlated::run_sql`), and a query's columns
//! come from its description (`correlated::describe_sql`), which exists
//! whether or not the query returns rows.

use super::*;
use crate::xml_catalog::{
    database_name, database_schemas, database_tables, relation, schema_name, schema_oid,
    schema_tables, Rel,
};
use crate::xml_export::xml_name_escaped;

/// A result column: its name and its type's oid -- the domain's own, for a
/// column declared over one, as a tuple descriptor has it.
pub(crate) struct Col {
    pub name: String,
    pub oid: i64,
}

/// The columns of `sql`'s result.
pub(crate) fn columns(sql: &str) -> Result<Vec<Col>> {
    correlated::describe_sql(sql)?
        .into_iter()
        .map(|row| {
            let name = row.first().map(render_value_text).unwrap_or_default();
            let wire = match row.get(1) {
                Some(Bson::Int64(o)) => *o,
                Some(Bson::Int32(o)) => i64::from(*o),
                _ => 0,
            };
            let oid = match row.get(2) {
                // Only a type of the database's own: a built-in's declared
                // name can differ from what the wire reports (a `char(n)`
                // stored as `text`), and the wire is right about those.
                Some(Bson::String(t)) => domains::user_domain(t)
                    .map(|d| d.oid)
                    .or_else(|| user_casts::type_oid(t))
                    .filter(|&o| o >= 16384)
                    .unwrap_or(wire),
                _ => wire,
            };
            Ok(Col { name, oid })
        })
        .collect()
}

/// What `map_sql_type_to_xml_name` reads from a type's `pg_type` row.
struct TypeRow {
    name: String,
    schema: String,
    domain: bool,
    base: i64,
    base_typmod: i32,
}

pub(crate) fn int_of(v: Option<&Bson>) -> i64 {
    match v {
        Some(Bson::Int64(n)) => *n,
        Some(Bson::Int32(n)) => i64::from(*n),
        Some(Bson::Double(n)) => *n as i64,
        Some(other) => render_value_text(other).parse().unwrap_or(0),
        None => 0,
    }
}

fn type_row(oid: i64) -> Result<TypeRow> {
    let rows = correlated::run_sql(&format!(
        "SELECT t.typname::text, n.nspname::text, t.typtype::text, \
         t.typbasetype::int8, t.typtypmod::int8 FROM pg_catalog.pg_type t \
         JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace WHERE t.oid = {oid}"
    ))?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| Error::Internal(format!("cache lookup failed for type {oid}")))?;
    let text = |i: usize| row.get(i).map(render_value_text).unwrap_or_default();
    let mut out = TypeRow {
        name: text(0),
        schema: text(1),
        domain: text(2) == "d",
        base: int_of(row.get(3)),
        base_typmod: i32::try_from(int_of(row.get(4))).unwrap_or(-1),
    };
    if out.domain {
        if let Some((base, typmod)) = domain_base(oid) {
            out.base = base;
            out.base_typmod = typmod;
        }
    }
    Ok(out)
}

/// `getBaseTypeAndTypmod`: through every domain to the type beneath, with
/// the modifier of the outermost domain that declares one; `None` for a
/// type that is not a domain. From the planner's own record of each domain.
pub(crate) fn domain_base(oid: i64) -> Option<(i64, i32)> {
    let mut d = domains::domain_name_of_oid(oid).and_then(|n| domains::user_domain(&n))?;
    let mut typmod = -1;
    loop {
        if typmod == -1 {
            typmod = d.typmod;
        }
        let base = user_casts::type_oid(&d.base)?;
        match domains::domain_name_of_oid(base).and_then(|n| domains::user_domain(&n)) {
            Some(next) => d = next,
            None => return Some((base, typmod)),
        }
    }
}

/// `map_multipart_sql_identifier_to_xml_name`: each part fully escaped,
/// a period inside a part as well, joined by periods.
fn multipart(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|p| xml_name_escaped(p, true))
        .collect::<Vec<_>>()
        .join(".")
}

/// `map_sql_type_to_xml_name(typeoid, typmod)`.
fn type_xml_name(oid: i64, typmod: i32, db: &str) -> Result<String> {
    let with = |bare: &str, n: Option<i32>| match n {
        Some(n) if typmod != -1 => format!("{bare}_{n}"),
        _ => bare.to_string(),
    };
    Ok(match oid {
        1042 => with("CHAR", Some(typmod - 4)),
        1043 => with("VARCHAR", Some(typmod - 4)),
        1700 if typmod != -1 => format!(
            "NUMERIC_{}_{}",
            ((typmod - 4) >> 16) & 0xffff,
            (typmod - 4) & 0xffff
        ),
        1700 => "NUMERIC".into(),
        23 => "INTEGER".into(),
        21 => "SMALLINT".into(),
        20 => "BIGINT".into(),
        700 => "REAL".into(),
        701 => "DOUBLE".into(),
        16 => "BOOLEAN".into(),
        1083 => with("TIME", Some(typmod)),
        1266 => with("TIME_WTZ", Some(typmod)),
        1114 => with("TIMESTAMP", Some(typmod)),
        1184 => with("TIMESTAMP_WTZ", Some(typmod)),
        1082 => "DATE".into(),
        142 => "XML".into(),
        _ => {
            let t = type_row(oid)?;
            let kind = if t.domain { "Domain" } else { "UDT" };
            multipart(&[kind, db, &t.schema, &t.name])
        }
    })
}

/// `map_sql_type_to_xmlschema_type(typeoid, -1)`: the named simple type a
/// column's values take (the XML type's is an anonymous complex type).
fn type_xmlschema(oid: i64, db: &str, base64: bool) -> Result<String> {
    if oid == 142 {
        return Ok("<xsd:complexType mixed=\"true\">\n  <xsd:sequence>\n    \
                   <xsd:any name=\"element\" minOccurs=\"0\" maxOccurs=\"unbounded\" \
                   processContents=\"skip\"/>\n  </xsd:sequence>\n</xsd:complexType>\n"
            .into());
    }
    let mut out = format!(
        "<xsd:simpleType name=\"{}\">\n",
        type_xml_name(oid, -1, db)?
    );
    let restriction = |base: &str, facets: &str| {
        format!("  <xsd:restriction base=\"xsd:{base}\">\n{facets}  </xsd:restriction>\n")
    };
    let range = |base: &str, max: &str, min: &str| {
        restriction(
            base,
            &format!(
                "    <xsd:maxInclusive value=\"{max}\"/>\n    \
                 <xsd:minInclusive value=\"{min}\"/>\n"
            ),
        )
    };
    let pattern =
        |base: &str, p: &str| restriction(base, &format!("    <xsd:pattern value=\"{p}\"/>\n"));
    const TZ: &str = "(\\+|-)\\p{Nd}{2}:\\p{Nd}{2}";
    const TIME: &str = "\\p{Nd}{2}:\\p{Nd}{2}:\\p{Nd}{2}(.\\p{Nd}+)?";
    const DATE: &str = "\\p{Nd}{4}-\\p{Nd}{2}-\\p{Nd}{2}";
    out.push_str(&match oid {
        1042 | 1043 | 25 => restriction("string", ""),
        17 => restriction(if base64 { "base64Binary" } else { "hexBinary" }, ""),
        // A numeric with no declared precision has no restriction at all.
        1700 => String::new(),
        21 => range("short", "32767", "-32768"),
        23 => range("int", "2147483647", "-2147483648"),
        20 => range("long", "9223372036854775807", "-9223372036854775808"),
        700 => "  <xsd:restriction base=\"xsd:float\"></xsd:restriction>\n".into(),
        701 => "  <xsd:restriction base=\"xsd:double\"></xsd:restriction>\n".into(),
        16 => "  <xsd:restriction base=\"xsd:boolean\"></xsd:restriction>\n".into(),
        1083 => pattern("time", TIME),
        1266 => pattern("time", &format!("{TIME}{TZ}")),
        1114 => pattern("dateTime", &format!("{DATE}T{TIME}")),
        1184 => pattern("dateTime", &format!("{DATE}T{TIME}{TZ}")),
        1082 => pattern("date", DATE),
        _ => {
            let t = type_row(oid)?;
            if t.domain {
                format!(
                    "  <xsd:restriction base=\"{}\"/>\n",
                    type_xml_name(t.base, t.base_typmod, db)?
                )
            } else {
                String::new()
            }
        }
    });
    out.push_str("</xsd:simpleType>\n");
    Ok(out)
}

/// `xmlbinary`: `base64` unless set to `hex`.
pub(crate) fn binary_is_base64() -> bool {
    correlated::run_sql("SELECT current_setting('xmlbinary', true)")
        .ok()
        .and_then(|r| r.first().and_then(|r| r.first()).map(render_value_text))
        .is_none_or(|v| !v.eq_ignore_ascii_case("hex"))
}

/// `map_sql_typecoll_to_xmlschema_types`: every column type of every
/// descriptor once, in first-use order, then each domain's base type.
fn typecoll(descs: &[&[Col]], db: &str) -> Result<String> {
    let mut unique: Vec<i64> = Vec::new();
    for c in descs.iter().flat_map(|d| d.iter()) {
        if !unique.contains(&c.oid) {
            unique.push(c.oid);
        }
    }
    // `foreach` over the list it appends to also visits what it appended.
    let mut i = 0;
    while i < unique.len() {
        let oid = unique[i];
        if pgtypes::name_of_oid(oid).is_none() {
            let t = type_row(oid)?;
            if t.domain && !unique.contains(&t.base) {
                unique.push(t.base);
            }
        }
        i += 1;
    }
    let base64 = binary_is_base64();
    let mut out = String::new();
    for oid in unique {
        out.push_str(&type_xmlschema(oid, db, base64)?);
        out.push('\n');
    }
    Ok(out)
}

/// `xsd_schema_element_start`.
fn xsd_start(ns: &str) -> String {
    let mut out = "<xsd:schema\n    xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\"".to_string();
    if !ns.is_empty() {
        out.push_str(&format!(
            "\n    targetNamespace=\"{ns}\"\n    elementFormDefault=\"qualified\""
        ));
    }
    out.push_str(">\n\n");
    out
}

/// `map_sql_table_to_xmlschema(tupdesc, relid, ...)`; `rel` is `None` for
/// a query's or a cursor's descriptor (InvalidOid).
pub(crate) fn table_xmlschema(
    cols: &[Col],
    rel: Option<&Rel>,
    nulls: bool,
    forest: bool,
    ns: &str,
) -> Result<String> {
    let db = database_name()?;
    let (xmltn, table_type, row_type) = match rel {
        Some((name, schema)) => (
            xml_name_escaped(name, false),
            multipart(&["TableType", &db, schema, name]),
            multipart(&["RowType", &db, schema, name]),
        ),
        None => (
            (if forest { "row" } else { "table" }).to_string(),
            "TableType".to_string(),
            "RowType".to_string(),
        ),
    };
    let mut out = xsd_start(ns);
    out.push_str(&typecoll(&[cols], &db)?);
    out.push_str(&format!(
        "<xsd:complexType name=\"{row_type}\">\n  <xsd:sequence>\n"
    ));
    for c in cols {
        out.push_str(&format!(
            "    <xsd:element name=\"{}\" type=\"{}\"{}></xsd:element>\n",
            xml_name_escaped(&c.name, false),
            type_xml_name(c.oid, -1, &db)?,
            if nulls {
                " nillable=\"true\""
            } else {
                " minOccurs=\"0\""
            }
        ));
    }
    out.push_str("  </xsd:sequence>\n</xsd:complexType>\n\n");
    if forest {
        out.push_str(&format!(
            "<xsd:element name=\"{xmltn}\" type=\"{row_type}\"/>\n\n"
        ));
    } else {
        out.push_str(&format!(
            "<xsd:complexType name=\"{table_type}\">\n  <xsd:sequence>\n    \
             <xsd:element name=\"row\" type=\"{row_type}\" minOccurs=\"0\" \
             maxOccurs=\"unbounded\"/>\n  </xsd:sequence>\n</xsd:complexType>\n\n\
             <xsd:element name=\"{xmltn}\" type=\"{table_type}\"/>\n\n"
        ));
    }
    out.push_str("</xsd:schema>");
    Ok(out)
}

/// A relation's columns, as its tuple descriptor holds them.
pub(crate) fn relation_columns(oid: i64) -> Result<Vec<Col>> {
    columns(&format!("SELECT * FROM {}", regclass_text(oid)))
}

/// `schema_to_xmlschema_internal`.
pub(crate) fn schema_xmlschema(schema: &str, forest: bool, ns: &str) -> Result<String> {
    let nspid = schema_oid(schema)?;
    let db = database_name()?;
    let mut out = xsd_start(ns);
    let rels = schema_tables(nspid)?;
    let descs = rels
        .iter()
        .map(|&r| relation_columns(r))
        .collect::<Result<Vec<_>>>()?;
    let descs: Vec<&[Col]> = descs.iter().map(Vec::as_slice).collect();
    out.push_str(&typecoll(&descs, &db)?);
    // map_sql_schema_to_xmlschema_types
    let nspname = schema_name(nspid)?;
    let schema_type = multipart(&["SchemaType", &db, &nspname]);
    out.push_str(&format!("<xsd:complexType name=\"{schema_type}\">\n"));
    out.push_str(if forest {
        "  <xsd:sequence>\n"
    } else {
        "  <xsd:all>\n"
    });
    for &r in &rels {
        let (relname, _) = relation(r)?;
        let xmltn = xml_name_escaped(&relname, false);
        let ty = multipart(&[
            if forest { "RowType" } else { "TableType" },
            &db,
            &nspname,
            &relname,
        ]);
        out.push_str(&if forest {
            format!(
                "    <xsd:element name=\"{xmltn}\" type=\"{ty}\" minOccurs=\"0\" \
                 maxOccurs=\"unbounded\"/>\n"
            )
        } else {
            format!("    <xsd:element name=\"{xmltn}\" type=\"{ty}\"/>\n")
        });
    }
    out.push_str(if forest {
        "  </xsd:sequence>\n"
    } else {
        "  </xsd:all>\n"
    });
    out.push_str("</xsd:complexType>\n\n");
    out.push_str(&format!(
        "<xsd:element name=\"{}\" type=\"{schema_type}\"/>\n\n",
        xml_name_escaped(&nspname, false)
    ));
    out.push_str("</xsd:schema>");
    Ok(out)
}

/// `database_to_xmlschema_internal`.
pub(crate) fn database_xmlschema(ns: &str) -> Result<String> {
    let db = database_name()?;
    let mut out = xsd_start(ns);
    let descs = database_tables()?
        .iter()
        .map(|&r| relation_columns(r))
        .collect::<Result<Vec<_>>>()?;
    let descs: Vec<&[Col]> = descs.iter().map(Vec::as_slice).collect();
    out.push_str(&typecoll(&descs, &db)?);
    // map_sql_catalog_to_xmlschema_types
    let catalog_type = multipart(&["CatalogType", &db]);
    out.push_str(&format!(
        "<xsd:complexType name=\"{catalog_type}\">\n  <xsd:all>\n"
    ));
    for nsp in database_schemas()? {
        let nspname = schema_name(nsp)?;
        out.push_str(&format!(
            "    <xsd:element name=\"{}\" type=\"{}\"/>\n",
            xml_name_escaped(&nspname, false),
            multipart(&["SchemaType", &db, &nspname])
        ));
    }
    out.push_str("  </xsd:all>\n</xsd:complexType>\n\n");
    out.push_str(&format!(
        "<xsd:element name=\"{}\" type=\"{catalog_type}\"/>\n\n",
        xml_name_escaped(&db, false)
    ));
    out.push_str("</xsd:schema>");
    Ok(out)
}
