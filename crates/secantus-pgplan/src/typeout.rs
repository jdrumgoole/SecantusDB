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
//!
//! A `date`, `timestamp` or `timestamptz` prints in the session `DateStyle`
//! (`date_out` is `02.01.2020` under German), whichever flavour. JSON and
//! XML write ISO under every DateStyle, so they take [`iso_output_text`].

use crate::{session_datestyle, session_timezone, Bson};

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
            | "date"
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

/// `ty`'s output function over a non-NULL `v`, in the session's
/// `DateStyle`: a `date` is `02.01.2020` under German, as `date_out` writes
/// it in `concat`, `format`, a record field or `array_out` (RP-894), and in
/// a catalog DEFAULT's text, which `get_const_expr` prints through the
/// output function in the reading session's settings (RP-864).
pub fn output_text(v: &Bson, ty: &str) -> String {
    render(v, ty.trim(), false, true)
}

/// `ty`'s output function over a non-NULL `v` with a date or timestamp in
/// ISO, as JSON and XML write one under every `DateStyle`.
pub fn iso_output_text(v: &Bson, ty: &str) -> String {
    render(v, ty.trim(), false, false)
}

/// `v::text` for a non-NULL `v` of type `ty`.
pub fn cast_text(v: &Bson, ty: &str) -> String {
    render(v, ty.trim(), true, true)
}

/// A `date`, `timestamp` or `timestamptz` in the session `DateStyle`; `None`
/// for any other type.
fn styled_datetime(v: &Bson, ty: &str) -> Option<String> {
    let ds = session_datestyle();
    match (ty, v) {
        ("date", Bson::String(t)) => Some(crate::render_date_styled(t, &ds)),
        ("timestamp" | "timestamp without time zone", _) => {
            crate::timestamp_value_text_styled(v, &ds)
        }
        ("timestamptz" | "timestamp with time zone", _) => {
            crate::timestamptz_value_text_styled(v, &session_timezone(), &ds)
        }
        _ => None,
    }
}

fn render(v: &Bson, ty: &str, cast: bool, styled: bool) -> String {
    if let Some(elem) = ty.strip_suffix("[]") {
        if let Bson::Array(items) = v {
            // `array_out` prints each element by its OUTPUT function, a cast
            // to text included (`array[true]::text` is `{t}`).
            return crate::value_text(&Bson::Array(element_texts(items, elem, styled)));
        }
    }
    if styled {
        if let Some(text) = styled_datetime(v, ty) {
            return text;
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
            other => Some(Some(render(other, elem, false, true))),
        })
        .collect()
}

/// An array's elements as their output text, NULLs kept, nested arrays
/// walked, so the array printer quotes each as `array_out` does.
fn element_texts(items: &[Bson], elem: &str, styled: bool) -> Vec<Bson> {
    items
        .iter()
        .map(|x| match x {
            Bson::Null => Bson::Null,
            Bson::Array(inner) => Bson::Array(element_texts(inner, elem, styled)),
            other => Bson::String(render(other, elem, false, styled)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{set_session_datestyle, DateStyle};

    // A `date` / `timestamp` is written in the session DateStyle, as
    // PostgreSQL 15.19's `date_out` writes it (`02.01.2020` under German) in
    // a catalog DEFAULT (RP-864) and in `concat` / `format` / a record
    // (RP-894); `iso_output_text`, JSON's and XML's, stays ISO.
    #[test]
    fn a_date_and_a_timestamp_output_in_the_session_datestyle() {
        let ts = Bson::DateTime(bson::DateTime::from_millis(1_577_882_096_789));
        let date = Bson::String("2020-01-02".into());
        set_session_datestyle(DateStyle::parse("German"));
        let german = (
            output_text(&date, "date"),
            cast_text(&ts, "timestamp"),
            iso_output_text(&date, "date"),
            iso_output_text(&ts, "timestamp"),
            output_text(&Bson::Array(vec![date.clone(), Bson::Null]), "date[]"),
        );
        set_session_datestyle(DateStyle::default());
        let iso = (output_text(&date, "date"), output_text(&ts, "timestamp"));
        assert_eq!(
            german,
            (
                "02.01.2020".into(),
                "01.01.2020 12:34:56.789".into(),
                "2020-01-02".into(),
                "2020-01-01 12:34:56.789".into(),
                "{02.01.2020,NULL}".into()
            )
        );
        assert_eq!(iso, ("2020-01-02".into(), "2020-01-01 12:34:56.789".into()));
    }
}
