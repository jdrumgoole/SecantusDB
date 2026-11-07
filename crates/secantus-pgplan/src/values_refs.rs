//! A column reference in an INSERT's VALUES list (RP-415).
//!
//! PostgreSQL 15.19's `transformInsertStmt` analyses a VALUES row in the
//! INSERT's own query level, where the target relation is in the range
//! table but not in the namespace: no column reference there can bind. So
//! `transformColumnRef` fails on the first one it meets, row by row, before
//! that row's length and assignment types are checked and before anything
//! is evaluated:
//!
//! - a bare name (`bb`, `(zz).f`, `zz[1]`) is 42703 `column "bb" does not
//!   exist`, whose HINT `errorMissingColumn` finds in the range table that
//!   holds the target (the server adds it from the error's position,
//!   `colhint`);
//! - a qualified one (`e.a`, `public.e.a`, `x.*`) goes to `errorMissingRTE`:
//!   42P01 `invalid reference to FROM-clause entry for table "e"` when the
//!   qualifier names the target (by its alias, or as the same relation),
//!   with the hint that the entry cannot be referenced from there, and
//!   `missing FROM-clause entry for table "x"` otherwise;
//! - four names are `db.schema.table.column`, 0A000 `cross-database
//!   references are not implemented` unless `db` is this database; more are
//!   42601 `improper qualified name (too many dotted names)`.
//!
//! This server used to evaluate the row as constants, where a column
//! reference was refused as a gap (`0A000 ColumnRef is not supported yet`),
//! and only after evaluating the items before it (`values (1/0, bb)` was
//! 22012).

use super::*;
use pg_query::protobuf as pb;

/// The error PostgreSQL raises for the first column reference in one
/// VALUES row of `INSERT INTO target`, or `Ok` when the row has none. The
/// row is walked as `transformExpr` walks it, outermost first; a subquery's
/// body is its own query level and is not walked here.
pub(crate) fn check_row(
    items: &[pb::Node],
    target: &pb::RangeVar,
    target_def: &TableDef,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<()> {
    for item in items {
        let mut item = item.clone();
        let mut found: Option<pb::ColumnRef> = None;
        let walked = walk_expr(&mut item, &mut |n| {
            if let Some(N::ColumnRef(c)) = n.node.as_ref() {
                found = Some(c.clone());
                // Stop at the first; the error is built below.
                return Err(Error::Internal(String::new()));
            }
            Ok(())
        });
        if let Some(c) = found {
            return Err(reference_error(&c, target, target_def, lookup));
        }
        walked?;
    }
    Ok(())
}

/// `transformColumnRef`'s failure for `c` where nothing is in the
/// namespace.
fn reference_error(
    c: &pb::ColumnRef,
    target: &pb::RangeVar,
    target_def: &TableDef,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Error {
    let parts: Vec<Option<&str>> = c
        .fields
        .iter()
        .map(|f| match f.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.as_str()),
            _ => None,
        })
        .collect();
    // `NameListToString`: a `*` prints as itself.
    let joined = || {
        parts
            .iter()
            .map(|p| p.unwrap_or("*"))
            .collect::<Vec<_>>()
            .join(".")
    };
    set_error_location(c.location);
    match parts.as_slice() {
        [Some(name)] => Error::UndefinedColumn((*name).to_string()),
        [Some(rel), _] => missing_rte(None, rel, target, target_def, lookup),
        [Some(schema), Some(rel), _] => missing_rte(Some(*schema), rel, target, target_def, lookup),
        [Some(db), Some(schema), Some(rel), _] => {
            if *db == session_database() {
                missing_rte(Some(*schema), rel, target, target_def, lookup)
            } else {
                Error::FeatureNotSupported(format!(
                    "cross-database references are not implemented: {}",
                    joined()
                ))
            }
        }
        [_, _, _, _, _, ..] => Error::Sqlstate(
            "42601",
            format!(
                "improper qualified name (too many dotted names): {}",
                joined()
            ),
        ),
        // A bare `*` the grammar refuses before this; anything else keeps
        // the refusal it had.
        _ => Error::Unsupported("ColumnRef".into()),
    }
}

/// `errorMissingRTE` for the relation `schema.rel` written as a qualifier.
/// `searchRangeTableForRel` finds the INSERT's target when the qualifier is
/// its alias (`eref->aliasname`, the table's name when it has none) or
/// names the same relation. The namespace is empty, so the "Perhaps you
/// meant to reference the table alias" hint never applies.
fn missing_rte(
    schema: Option<&str>,
    rel: &str,
    target: &pb::RangeVar,
    target_def: &TableDef,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Error {
    let alias = target
        .alias
        .as_ref()
        .map_or(target.relname.as_str(), |a| a.aliasname.as_str());
    let qualified = match schema {
        Some(s) => format!("{s}.{rel}"),
        None => rel.to_string(),
    };
    let same_relation = || lookup(&qualified).is_some_and(|d| d.name == target_def.name);
    if rel == alias || same_relation() {
        Error::Sqlstate(
            "42P01",
            format!(
                "invalid reference to FROM-clause entry for table \"{rel}\"\nHint: There is an entry for table \"{alias}\", but it cannot be referenced from this part of the query."
            ),
        )
    } else {
        Error::Sqlstate(
            "42P01",
            format!("missing FROM-clause entry for table \"{rel}\""),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `plan_insert`'s error for `sql` over `e(a int, b text)`, as its
    /// SQLSTATE and message, and the position it set.
    fn refused(sql: &str) -> (String, String, Option<i32>) {
        let def = TableDef::new(
            "e",
            vec![
                secantus_pgcatalog::Column::new("a", "int4", false),
                secantus_pgcatalog::Column::new("b", "text", false),
            ],
        );
        let lookup = |n: &str| (n == "e" || n == "public.e").then(|| def.clone());
        let tree = pg_query::parse(sql).unwrap();
        let Some(N::InsertStmt(i)) = tree.protobuf.stmts[0]
            .stmt
            .as_ref()
            .and_then(|n| n.node.clone())
        else {
            panic!("not an insert");
        };
        let _ = take_error_location();
        let e = plan_insert(&i, &lookup, &[]).expect_err("refused");
        (
            e.sqlstate().to_string(),
            e.to_string(),
            take_error_location(),
        )
    }

    #[test]
    fn a_bare_name_is_a_missing_column_at_its_token() {
        assert_eq!(
            refused("insert into e values (1, 'x'), (bb, 'y')"),
            (
                "42703".into(),
                "column \"bb\" does not exist".into(),
                Some(32)
            ),
        );
    }

    #[test]
    fn a_qualifier_naming_the_target_is_an_invalid_reference() {
        let (code, message, _) = refused("insert into e as x values (e.a)");
        assert_eq!(code, "42P01");
        assert_eq!(
            message,
            "invalid reference to FROM-clause entry for table \"e\"\nHint: There is an entry for table \"x\", but it cannot be referenced from this part of the query."
        );
        let (code, message, _) = refused("insert into e values (s.a)");
        assert_eq!(code, "42P01");
        assert_eq!(message, "missing FROM-clause entry for table \"s\"");
    }

    #[test]
    fn a_row_is_analysed_before_anything_is_evaluated() {
        assert_eq!(refused("insert into e values (1/0, bb)").0, "42703");
        // ... and row by row, its assignment types after its references.
        assert_eq!(
            refused("insert into e values (true, 'x'), (bb, 'y')").0,
            "42804"
        );
        assert_eq!(
            refused("insert into e values (bb, 'y'), (true, 'x')").0,
            "42703"
        );
    }
}
