//! Tests for `builtin_alias`: what a call is rewritten into, printed back as SQL.

use super::*;

/// The call `sql`, rewritten and printed back as a statement. The tree
/// printed is one libpg_query parsed, with only the target replaced:
/// its deparser asserts on a statement built from defaults.
fn rewritten(sql: &str) -> Option<String> {
    let mut parsed = pg_query::parse(&format!("SELECT {sql}"))
        .expect("parses")
        .protobuf;
    let stmt = parsed.stmts[0].stmt.as_mut()?.node.as_mut()?;
    let N::SelectStmt(select) = stmt else {
        return None;
    };
    let Some(N::ResTarget(target)) = select.target_list[0].node.as_mut() else {
        return None;
    };
    let node = replacement(target.val.as_deref()?)?;
    target.val = Some(Box::new(node));
    Some(pg_query::deparse(&parsed).expect("deparses"))
}

#[test]
fn an_operator_function_is_its_operator() {
    assert_eq!(rewritten("int4pl(a, b)").as_deref(), Some("SELECT a + b"));
    assert_eq!(
        rewritten("pg_catalog.int4eq(a, 2)").as_deref(),
        Some("SELECT a = 2::int")
    );
    assert_eq!(
        rewritten("textlike(a, 'x%')").as_deref(),
        Some("SELECT a LIKE 'x%'::pg_catalog.text")
    );
}

#[test]
fn a_literal_takes_the_declared_type_where_postgresql_would_coerce_it() {
    // An integer literal goes to any numeric parameter.
    assert_eq!(
        rewritten("int8pl(1, 2)").as_deref(),
        Some("SELECT 1::bigint + 2::bigint")
    );
    // A numeric literal has no implicit cast to an integer, so `int4pl` does
    // not exist for it: the call is left for the planner to refuse (42883),
    // where it used to run as `1.5 + a`.
    assert_eq!(rewritten("int4pl(1.5, a)"), None);
    assert_eq!(rewritten("textlen(1.5)"), None);
    assert_eq!(rewritten("dexp(true)"), None);
    // A whole number past int4 is a bigint: int8pl takes it, int4pl not.
    assert!(rewritten("int8pl(5000000000, 1)").is_some());
    assert_eq!(rewritten("int4pl(5000000000, 1)"), None);
    // A column's type is not known here, so a column argument is rewritten.
    assert_eq!(
        rewritten("int4pl(a, 2)").as_deref(),
        Some("SELECT a + 2::int")
    );
}

#[test]
fn a_template_substitutes_its_arguments() {
    assert_eq!(
        rewritten("textlen(a)").as_deref(),
        Some("SELECT length(a::text)")
    );
    assert_eq!(
        rewritten("int4larger(a, b)").as_deref(),
        Some("SELECT CASE WHEN a >= b THEN a WHEN a < b THEN b END")
    );
}

#[test]
fn what_is_not_an_alias_is_left_alone() {
    assert_eq!(rewritten("upper(a)"), None);
    // An aggregate or window decoration is not a plain call.
    assert_eq!(rewritten("int4pl(DISTINCT a, b)"), None);
    // A schema other than pg_catalog is somebody's own function.
    assert_eq!(rewritten("app.int4pl(a, b)"), None);
    // A function with a VARIADIC overload is not its operator: this is
    // the path's first step.
    assert_eq!(rewritten("jsonb_extract_path(j, 'a')"), None);
    assert_eq!(rewritten("json_extract_path_text(j, 'a', 'b')"), None);
    // The comparison whose answer depends on the C library is refused.
    assert_eq!(rewritten("bttextcmp(a, b)"), None);
}

#[test]
fn family_types_are_read_off_the_name() {
    assert_eq!(family_types("int4larger"), ("int4", "int4"));
    assert_eq!(family_types("btint48cmp"), ("int4", "int8"));
    assert_eq!(family_types("btfloat84cmp"), ("float8", "float4"));
    assert_eq!(family_types("text_smaller"), ("text", "text"));
    assert_eq!(family_types("numeric_cmp"), ("numeric", "numeric"));
    assert_eq!(
        family_types("timestamptz_larger"),
        ("timestamptz", "timestamptz")
    );
}

#[test]
fn every_template_parses() {
    assert_eq!(templates().len(), TEMPLATES.len());
    for (name, _) in FAMILIES {
        assert!(family_template(name, 2).is_some(), "{name}");
    }
    assert!(operators().len() > 700);
}
