//! A value's text by its TYPE, as PostgreSQL's output function prints it.
//!
//! Several types are carried in a form that is not their text: a `pg_lsn`
//! and a `money` as a numeric, a `tid` as one integer, a `float4` as an
//! f64, a `timestamptz` as an instant (printed in the session zone), an
//! `inet` with the mask `inet_out` drops, an `int2vector` as an array, a
//! reg* / interval / geometric value as a document. The wire layer prints
//! each by its column type, but a value that becomes TEXT inside an
//! expression -- `x || 'a'`, `concat(x)`, `format('%s', x)`, a `ROW(...)`
//! field, `to_json(x)` -- used to print the carried form (`22x` for
//! `'0/16'::pg_lsn || 'x'`, `Document({"__regclass_oid": ...})` for a
//! regclass in `concat`). Measured on PostgreSQL 15.19.
//!
//! Two flavours, because PostgreSQL has two: the OUTPUT function (`concat`,
//! `format`, a record field, `array_out`) and the `::text` CAST (`||` is
//! `$1::text || $2`, `quote_literal(anyelement)` is
//! `quote_literal($1::text)`). They differ only for a boolean, which
//! outputs as `t` and casts to `true`.

use crate::{session_timezone, Bson};

/// Is `ty` a type whose value is carried in a form other than its text,
/// so that printing it needs the type? A string, an integer or a numeric
/// prints as itself.
pub fn needs_type(ty: &str) -> bool {
    let ty = ty.trim();
    if let Some(elem) = ty.strip_suffix("[]") {
        return needs_type(elem);
    }
    matches!(
        ty,
        "tid"
            | "pg_lsn"
            | "money"
            | "float4"
            | "real"
            | "timestamptz"
            | "timestamp with time zone"
            | "timestamp"
            | "timestamp without time zone"
            | "inet"
            | "cidr"
            | "int2vector"
            | "oidvector"
            | "interval"
            | "bytea"
            | "regclass"
            | "regtype"
            | "regproc"
            | "regprocedure"
            | "regnamespace"
            | "regrole"
            | "regoper"
            | "regoperator"
            | "regconfig"
            | "regdictionary"
            | "regcollation"
            | "point"
            | "box"
            | "lseg"
            | "line"
            | "circle"
            | "path"
            | "polygon"
    )
}

/// `ty`'s output function over a non-NULL `v`.
pub fn output_text(v: &Bson, ty: &str) -> String {
    render(v, ty.trim(), false)
}

/// `v::text` for a non-NULL `v` of type `ty`.
pub fn cast_text(v: &Bson, ty: &str) -> String {
    render(v, ty.trim(), true)
}

fn render(v: &Bson, ty: &str, cast: bool) -> String {
    if let Some(elem) = ty.strip_suffix("[]") {
        if let Bson::Array(items) = v {
            // `array_out` prints each element by its OUTPUT function, a cast
            // to text included (`array[true]::text` is `{t}`).
            return crate::value_text(&Bson::Array(element_texts(items, elem)));
        }
    }
    match (ty, v) {
        (_, Bson::Boolean(b)) => match (cast, *b) {
            (true, true) => "true".into(),
            (true, false) => "false".into(),
            (false, true) => "t".into(),
            (false, false) => "f".into(),
        },
        ("tid", _) => crate::tid::render_tid_value(v),
        ("pg_lsn", _) => crate::systypes::render_lsn_value(v),
        ("money", _) => crate::money::render_value(v),
        ("float4" | "real", Bson::Double(d)) => crate::geo::float4_text(*d as f32),
        ("timestamptz" | "timestamp with time zone", _) => {
            crate::timestamptz_value_text(v, &session_timezone())
                .unwrap_or_else(|| crate::value_text(v))
        }
        // `inet_out` drops a full host mask; `text(inet)` keeps it.
        ("inet", Bson::String(s)) if cast => s.clone(),
        ("inet" | "cidr", Bson::String(s)) => crate::net::text_out(s, ty == "cidr"),
        // An int2vector / oidvector is space-separated, not `{1,2}`.
        ("int2vector" | "oidvector", Bson::Array(items)) => items
            .iter()
            .map(crate::value_text)
            .collect::<Vec<_>>()
            .join(" "),
        _ => crate::value_text(v),
    }
}

/// The elements of a one-dimensional array of type `ty` (`money[]`) as
/// their output text, NULLs as `None`; `None` for anything else.
pub fn array_elements(v: &Bson, ty: &str) -> Option<Vec<Option<String>>> {
    let elem = ty.trim().strip_suffix("[]")?;
    let Bson::Array(items) = v else {
        return None;
    };
    items
        .iter()
        .map(|x| match x {
            Bson::Null => Some(None),
            Bson::Array(_) => None,
            other => Some(Some(render(other, elem, false))),
        })
        .collect()
}

/// An array's elements as their output text, NULLs kept, nested arrays
/// walked, so the array printer quotes each as `array_out` does.
fn element_texts(items: &[Bson], elem: &str) -> Vec<Bson> {
    items
        .iter()
        .map(|x| match x {
            Bson::Null => Bson::Null,
            Bson::Array(inner) => Bson::Array(element_texts(inner, elem)),
            other => Bson::String(render(other, elem, false)),
        })
        .collect()
}
