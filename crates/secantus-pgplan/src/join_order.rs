//! The order a comma-separated FROM is joined in, and the WHERE equalities
//! each join hashes on.
//!
//! `FROM a, b, c WHERE a.x = b.y AND b.z = c.w` is a cross join filtered by
//! the WHERE. Joined left to right with no keys, every pair of rows is
//! formed before the WHERE drops them: pgjdbc's `getImportedKeys` (nine
//! catalog relations, `pg_attribute` twice) ran for minutes and grew to
//! gigabytes. Here each column-to-column equality between two items becomes
//! a hash key of the join that first brings both sides together, and an
//! item that no equality reaches yet waits until one does. The WHERE is
//! applied to the joined rows exactly as before, so the keys only narrow
//! which pairs are formed -- they never decide a row.
//!
//! Only equalities whose two columns are of one family the join's hash
//! models exactly are taken: integers (`int2`/`int4`/`int8`/`oid`) and
//! strings (`text`/`name`/`varchar`). A pair it cannot hash (a NULL, a
//! value of another type) falls back to comparing every candidate.

use super::*;

/// One WHERE conjunct `lhs = rhs` over two FROM items:
/// `(item, key, item, key)`.
pub(crate) type Equality = (usize, String, usize, String);

fn family(pg_type: &str) -> Option<u8> {
    match pg_type {
        "int2" | "int4" | "int8" | "oid" => Some(0),
        "text" | "name" | "varchar" => Some(1),
        _ => None,
    }
}

/// The column-to-column equalities among `where_clause`'s top-level
/// conjuncts that join two different items. `items[i]` lists item `i`'s
/// columns as `(alias, name, key, type)`; the type is empty for a column
/// not reachable by its bare name.
pub(crate) fn where_equalities(
    where_clause: Option<&pg_query::protobuf::Node>,
    items: &[Vec<(String, String, String, String)>],
) -> Vec<Equality> {
    let Some(w) = where_clause else {
        return Vec::new();
    };
    let mut conjuncts = Vec::new();
    and_terms(w, &mut conjuncts);
    let resolve = |n: Option<&pg_query::protobuf::Node>| -> Option<(usize, String, String)> {
        let Some(N::ColumnRef(c)) = n?.node.as_ref() else {
            return None;
        };
        let parts: Vec<&str> = c
            .fields
            .iter()
            .map(|f| match f.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.as_str()),
                _ => None,
            })
            .collect::<Option<_>>()?;
        let mut hits = Vec::new();
        for (i, cols) in items.iter().enumerate() {
            for (alias, name, key, ty) in cols {
                let matches = match parts.as_slice() {
                    [n] => !ty.is_empty() && name == n,
                    [a, n] | [_, a, n] => alias == a && name == n,
                    _ => false,
                };
                if matches {
                    hits.push((i, key.clone(), ty.clone()));
                }
            }
        }
        // Ambiguous or unknown: the planner reports it; nothing to key on.
        (hits.len() == 1).then(|| hits.remove(0))
    };
    conjuncts
        .into_iter()
        .filter_map(|c| {
            let Some(N::AExpr(e)) = c.node.as_ref() else {
                return None;
            };
            if e.kind != pg_query::protobuf::AExprKind::AexprOp as i32
                || operator_name(e) != Ok("=")
            {
                return None;
            }
            let (li, lk, lt) = resolve(e.lexpr.as_deref())?;
            let (ri, rk, rt) = resolve(e.rexpr.as_deref())?;
            let same = family(&lt).is_some() && family(&lt) == family(&rt);
            (li != ri && same).then_some((li, lk, ri, rk))
        })
        .collect()
}

fn and_terms<'a>(n: &'a pg_query::protobuf::Node, out: &mut Vec<&'a pg_query::protobuf::Node>) {
    match n.node.as_ref() {
        Some(N::BoolExpr(b)) if b.boolop == pg_query::protobuf::BoolExprType::AndExpr as i32 => {
            for a in &b.args {
                and_terms(a, out);
            }
        }
        _ => out.push(n),
    }
}

/// The order to join `n` items in: FROM order, except that an item no
/// equality connects to those already joined waits while a later one is
/// connected.
pub(crate) fn order(n: usize, equalities: &[Equality]) -> Vec<usize> {
    let mut out = vec![0];
    let mut left: Vec<usize> = (1..n).collect();
    while !left.is_empty() {
        let connected = |j: usize| {
            equalities
                .iter()
                .any(|(a, _, b, _)| (*a == j && out.contains(b)) || (*b == j && out.contains(a)))
        };
        let pick = left.iter().position(|&j| connected(j)).unwrap_or(0);
        out.push(left.remove(pick));
    }
    out
}

/// The `(left key, right key)` pairs for joining item `right` onto the
/// items in `joined`.
pub(crate) fn pairs_between(
    equalities: &[Equality],
    joined: &[usize],
    right: usize,
) -> Vec<(String, String)> {
    equalities
        .iter()
        .filter_map(|(a, ak, b, bk)| {
            if *b == right && joined.contains(a) {
                Some((ak.clone(), bk.clone()))
            } else if *a == right && joined.contains(b) {
                Some((bk.clone(), ak.clone()))
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unconnected_item_waits_for_a_connected_one() {
        // a-b, c-d, b-d: a, b, then d (connected via b), then c.
        let eq = vec![
            (0, "a.x".into(), 1, "b.x".into()),
            (2, "c.y".into(), 3, "d.y".into()),
            (1, "b.z".into(), 3, "d.z".into()),
        ];
        assert_eq!(order(4, &eq), vec![0, 1, 3, 2]);
        assert_eq!(
            pairs_between(&eq, &[0, 1, 3], 2),
            vec![("d.y".to_string(), "c.y".to_string())]
        );
    }

    #[test]
    fn nothing_connected_keeps_from_order() {
        assert_eq!(order(3, &[]), vec![0, 1, 2]);
    }
}
