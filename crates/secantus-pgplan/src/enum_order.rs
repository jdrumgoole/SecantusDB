//! Enum ORDER: an enum value sorts and compares by its label's position in
//! the type's declaration, not by the label's text.
//!
//! This server stores an enum value as its label, so every order-sensitive
//! operation on one used to be text order: `ORDER BY mood`, `mood > 'ok'`,
//! `min(mood)`, a window's `ORDER BY mood` all answered for the labels'
//! spelling rather than the type. A SELECT is rewritten, before it is
//! planned, so each of those reads the label's POSITION instead:
//!
//! * a sort key `e` becomes `array_position(ARRAY[labels], e::text)`;
//! * a comparison `e op x` compares those positions (a literal label on the
//!   other side becomes its position; one that is not a label is 22P02);
//! * `min(e)` / `max(e)` / `greatest(...)` / `least(...)` pick by position and
//!   map back to the label.
//!
//! Equality, `IN` and `DISTINCT` are unaffected: two labels are equal exactly
//! when their positions are.
//!
//! The NETWORK types (`inet`, `cidr`) are stored as their canonical text and
//! had the same defect -- `10.0.0.1` sorted before `9.0.0.1`, and `a <
//! '9.255.0.0'::inet` compared strings. They take the same rewrite with an
//! order KEY (`__net_sortkey`, whose string order is PostgreSQL's
//! `network_cmp`) in place of the position; equality and `IN` go through it
//! too, so an unnormalised literal (`'10.0.0.1'` for `10.0.0.1/32`) matches.

use super::*;

/// The FROM items' tables by the name a column reference would qualify
/// them with (their alias, else their name).
fn scope(
    items: &[pg_query::protobuf::Node],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    out: &mut Vec<(String, TableDef)>,
) {
    for item in items {
        match item.node.as_ref() {
            Some(N::RangeVar(rv)) => {
                if let Some(def) = lookup(&relation_name(rv)) {
                    let name = rv
                        .alias
                        .as_ref()
                        .map(|a| a.aliasname.clone())
                        .unwrap_or_else(|| rv.relname.clone());
                    out.push((name, def));
                }
            }
            // A derived table (a VALUES list, a subquery): its columns, with
            // the collation a `COLLATE` inside it gives each one, which
            // PostgreSQL carries out as the column's implicit collation.
            Some(N::RangeSubselect(rs)) => {
                let (Some(alias), Some(Some(N::SelectStmt(q)))) = (
                    rs.alias.as_ref(),
                    rs.subquery.as_deref().map(|n| n.node.as_ref()),
                ) else {
                    continue;
                };
                out.push((
                    alias.aliasname.clone(),
                    derived_def(&alias.aliasname, q, &alias.colnames),
                ));
            }
            Some(N::JoinExpr(j)) => {
                let sides: Vec<pg_query::protobuf::Node> = [j.larg.as_deref(), j.rarg.as_deref()]
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect();
                scope(&sides, lookup, out);
            }
            _ => {}
        }
    }
}

/// A derived relation's columns: named by `colnames` (an alias list), else
/// by its first leg's targets (`columnN` for VALUES). A column is `text`
/// here -- the rewriter reads only its collation -- carrying the collation
/// an explicit `COLLATE` in any row or leg gives it.
fn derived_def(
    name: &str,
    q: &pg_query::protobuf::SelectStmt,
    colnames: &[pg_query::protobuf::Node],
) -> TableDef {
    fn first_leg(q: &pg_query::protobuf::SelectStmt) -> &pg_query::protobuf::SelectStmt {
        match q.larg.as_deref() {
            Some(l) if q.op != pg_query::protobuf::SetOperation::SetopNone as i32 => first_leg(l),
            _ => q,
        }
    }
    // Every leg's (or row's) expressions, by position.
    fn rows(q: &pg_query::protobuf::SelectStmt, out: &mut Vec<Vec<pg_query::protobuf::Node>>) {
        if q.op != pg_query::protobuf::SetOperation::SetopNone as i32 {
            for side in [q.larg.as_deref(), q.rarg.as_deref()].into_iter().flatten() {
                rows(side, out);
            }
            return;
        }
        if !q.values_lists.is_empty() {
            for vl in &q.values_lists {
                if let Some(N::List(l)) = vl.node.as_ref() {
                    out.push(l.items.clone());
                }
            }
            return;
        }
        out.push(
            q.target_list
                .iter()
                .filter_map(|t| match t.node.as_ref() {
                    Some(N::ResTarget(rt)) => rt.val.as_deref().cloned(),
                    _ => None,
                })
                .collect(),
        );
    }
    let first = first_leg(q);
    let mut names: Vec<String> = if !first.values_lists.is_empty() {
        let width = match first.values_lists.first().and_then(|v| v.node.as_ref()) {
            Some(N::List(l)) => l.items.len(),
            _ => 0,
        };
        (1..=width).map(|i| format!("column{i}")).collect()
    } else {
        first
            .target_list
            .iter()
            .filter_map(|t| match t.node.as_ref() {
                Some(N::ResTarget(rt)) => Some(if rt.name.is_empty() {
                    rt.val
                        .as_deref()
                        .map(expression_column_name)
                        .unwrap_or_default()
                } else {
                    rt.name.clone()
                }),
                _ => None,
            })
            .collect()
    };
    for (i, n) in colnames.iter().enumerate() {
        if let (Some(N::String(s)), Some(slot)) = (n.node.as_ref(), names.get_mut(i)) {
            *slot = s.sval.clone();
        }
    }
    let mut all = Vec::new();
    rows(q, &mut all);
    let columns = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let mut c = Column::new(n, "text", false);
            let collation = all.iter().find_map(|r| match r.get(i)?.node.as_ref() {
                Some(N::CollateClause(cc)) => Some(crate::collation::clause_name(cc)),
                _ => None,
            });
            if let Some(coll) = collation {
                c.extra.insert("collation", coll);
            }
            c
        })
        .collect();
    TableDef::new(name, columns)
}

/// The labels of `ty` when it is an ENUM. The user-type table the planner
/// holds carries composites and other user types beside enums, and treating
/// one of those as an enum with no labels turned every ordering comparison
/// of it into NULL.
fn enum_labels(ty: &str) -> Option<Vec<String>> {
    if user_composite(ty).is_some() || crate::range::is_range_type(ty) {
        return None;
    }
    user_enum(ty).map(|(_, labels)| labels)
}

/// The enum a column's declared type names, if any.
fn column_enum(c: &Column) -> Option<(String, Vec<String>)> {
    let ty = c.extra.get_str("enum_type").unwrap_or(&c.pg_type);
    enum_labels(ty).map(|labels| (ty.to_string(), labels))
}

struct Rewriter<'a> {
    scope: &'a [(String, TableDef)],
    changed: bool,
    /// Where each explicit `COLLATE` seen so far was written, for a
    /// conflict's cursor.
    collate_locations: std::cell::RefCell<Vec<(String, i32)>>,
    /// Expressions this rewrite produced that carry a collation: a grouped
    /// nondeterministic column's representative value.
    derived: std::cell::RefCell<Vec<(pg_query::protobuf::Node, String)>>,
}

impl Rewriter<'_> {
    /// The enum type `node` statically has: a column of an enum type, or a
    /// cast to one.
    fn enum_of(&self, node: &pg_query::protobuf::Node) -> Option<(String, Vec<String>)> {
        match node.node.as_ref() {
            Some(N::ColumnRef(c)) => {
                let parts: Vec<&str> = c
                    .fields
                    .iter()
                    .filter_map(|f| match f.node.as_ref() {
                        Some(N::String(s)) => Some(s.sval.as_str()),
                        _ => None,
                    })
                    .collect();
                match parts.as_slice() {
                    [q, col] | [_, q, col] => self
                        .scope
                        .iter()
                        .find(|(n, _)| n == q)
                        .and_then(|(_, def)| def.column(col))
                        .and_then(column_enum),
                    [col] => {
                        let hits: Vec<&Column> = self
                            .scope
                            .iter()
                            .filter_map(|(_, def)| def.column(col))
                            .collect();
                        match hits.as_slice() {
                            [one] => column_enum(one),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            Some(N::TypeCast(tc)) => {
                let ty = tc.type_name.as_ref().map(type_name_of)?;
                enum_labels(&ty).map(|labels| (ty, labels))
            }
            _ => None,
        }
    }

    /// The network type `node` statically has: an `inet` / `cidr` column in
    /// scope, or a cast to one.
    fn net_of(&self, node: &pg_query::protobuf::Node) -> Option<String> {
        let net = |t: &str| matches!(t, "inet" | "cidr").then(|| t.to_string());
        match node.node.as_ref() {
            Some(N::ColumnRef(c)) => {
                let parts: Vec<&str> = c
                    .fields
                    .iter()
                    .filter_map(|f| match f.node.as_ref() {
                        Some(N::String(s)) => Some(s.sval.as_str()),
                        _ => None,
                    })
                    .collect();
                let column = match parts.as_slice() {
                    [q, col] | [_, q, col] => self
                        .scope
                        .iter()
                        .find(|(n, _)| n == q)
                        .and_then(|(_, def)| def.column(col)),
                    [col] => {
                        let hits: Vec<&Column> = self
                            .scope
                            .iter()
                            .filter_map(|(_, def)| def.column(col))
                            .collect();
                        match hits.as_slice() {
                            [one] => Some(*one),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                column.and_then(|c| net(&c.pg_type))
            }
            Some(N::TypeCast(tc)) => net(&tc.type_name.as_ref().map(type_name_of)?),
            _ => None,
        }
    }

    /// The column `c` names in scope, when it names exactly one.
    fn column_of(&self, c: &pg_query::protobuf::ColumnRef) -> Option<&Column> {
        let parts: Vec<&str> = c
            .fields
            .iter()
            .filter_map(|f| match f.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.as_str()),
                _ => None,
            })
            .collect();
        match parts.as_slice() {
            [q, col] | [_, q, col] => self
                .scope
                .iter()
                .find(|(n, _)| n == q)
                .and_then(|(_, def)| def.column(col)),
            [col] => {
                let hits: Vec<&Column> = self
                    .scope
                    .iter()
                    .filter_map(|(_, def)| def.column(col))
                    .collect();
                match hits.as_slice() {
                    [one] => Some(*one),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The LOCALE collation (one that orders differently from bytes) a
    /// text operand carries: an explicit `COLLATE`, or its column's
    /// declared collation. `(name, explicit)`.
    fn coll_of(&self, node: &pg_query::protobuf::Node) -> Option<(String, bool)> {
        let locale = |name: &str| {
            crate::collation::resolve(name)
                .ok()
                .filter(crate::collation::Resolved::is_locale)
                .map(|_| name.to_string())
        };
        if let Some((_, c)) = self
            .derived
            .borrow()
            .iter()
            .find(|(d, _)| crate::same_expression(d, node))
        {
            return Some((c.clone(), false));
        }
        match node.node.as_ref() {
            Some(N::CollateClause(cc)) => {
                locale(&crate::collation::clause_name(cc)).map(|n| (n, true))
            }
            Some(N::ColumnRef(c)) => self
                .column_of(c)
                .and_then(crate::collation::column_collation)
                .and_then(|n| locale(&n))
                .map(|n| (n, false)),
            Some(N::TypeCast(tc)) if Self::is_citext_cast(tc) => {
                Some((crate::collation::CITEXT.to_string(), false))
            }
            _ => None,
        }
    }

    /// `x::citext`: a citext value, which carries citext's comparison.
    fn is_citext_cast(tc: &pg_query::protobuf::TypeCast) -> bool {
        tc.type_name.as_ref().is_some_and(|t| {
            t.array_bounds.is_empty() && crate::collation::is_citext(&type_name_of(t))
        })
    }

    /// Is `node` statically a `citext[]` -- a cast to one or a column of one?
    fn citext_array(&self, node: &pg_query::protobuf::Node) -> bool {
        let arr = |t: &str| {
            t.trim()
                .strip_suffix("[]")
                .is_some_and(|e| crate::collation::is_citext(e.trim_end()))
        };
        match node.node.as_ref() {
            Some(N::TypeCast(tc)) => tc.type_name.as_ref().is_some_and(|t| {
                !t.array_bounds.is_empty() && crate::collation::is_citext(&type_name_of(t))
                    || arr(&type_name_of(t))
            }),
            Some(N::ColumnRef(c)) => self.column_of(c).is_some_and(|col| arr(&col.pg_type)),
            _ => false,
        }
    }

    /// Is `node` statically a string of a type OTHER than citext -- a
    /// `text` / `varchar` column or cast? PostgreSQL resolves `citext =
    /// text` to text's operator (citext casts implicitly to text), so such
    /// an operand takes citext's comparison away.
    fn plain_string(&self, node: &pg_query::protobuf::Node) -> bool {
        let plain = |t: &str| {
            let t = t.trim();
            !crate::collation::is_citext(t)
                && matches!(
                    t,
                    "text" | "varchar" | "bpchar" | "name" | "character varying" | "character"
                )
                || t.starts_with("varchar(")
                || t.starts_with("character varying(")
        };
        match node.node.as_ref() {
            Some(N::TypeCast(tc)) => tc
                .type_name
                .as_ref()
                .is_some_and(|t| t.array_bounds.is_empty() && plain(&type_name_of(t))),
            Some(N::ColumnRef(c)) => self.column_of(c).is_some_and(|col| plain(&col.pg_type)),
            _ => false,
        }
    }

    /// The collation a comparison or ordering of these operands uses: an
    /// explicit one wins; two different explicit ones are PostgreSQL's
    /// 42P21, two different implicit ones its 42P22.
    fn coll_between(&self, nodes: &[&pg_query::protobuf::Node]) -> Result<Option<String>> {
        // Every collation in play, a `C`-family one included: an explicit
        // `COLLATE "C"` still conflicts with an explicit ICU collation.
        let any = |n: &pg_query::protobuf::Node| -> Option<(String, bool)> {
            match n.node.as_ref() {
                Some(N::CollateClause(cc)) => {
                    let name = crate::collation::clause_name(cc);
                    self.collate_locations
                        .borrow_mut()
                        .push((name.clone(), cc.location));
                    Some((name, true))
                }
                Some(N::ColumnRef(c)) => self
                    .column_of(c)
                    .and_then(crate::collation::column_collation)
                    .map(|name| (name, false)),
                Some(N::TypeCast(tc)) if Self::is_citext_cast(tc) => {
                    Some((crate::collation::CITEXT.to_string(), false))
                }
                _ => None,
            }
        };
        let found: Vec<(String, bool)> = nodes.iter().filter_map(|n| any(n)).collect();
        // citext against a plain string type is text's comparison.
        if found
            .iter()
            .all(|(n, e)| n == crate::collation::CITEXT && !e)
            && nodes.iter().any(|n| self.plain_string(n))
        {
            return Ok(None);
        }
        let winner = self.coll_winner(&found)?;
        Ok(winner.filter(|name| crate::collation::resolve(name).is_ok_and(|r| r.is_locale())))
    }

    fn coll_winner(&self, found: &[(String, bool)]) -> Result<Option<String>> {
        let explicit: Vec<&String> = found.iter().filter(|(_, e)| *e).map(|(n, _)| n).collect();
        if let Some(first) = explicit.first() {
            if let Some(other) = explicit.iter().find(|n| *n != first) {
                // PostgreSQL's cursor is at the conflicting clause.
                if let Some((_, loc)) = self
                    .collate_locations
                    .borrow()
                    .iter()
                    .rev()
                    .find(|(n, _)| n == *other)
                {
                    set_error_location(*loc);
                }
                return Err(Error::Sqlstate(
                    "42P21",
                    format!("collation mismatch between explicit collations \"{first}\" and \"{other}\""),
                ));
            }
            return Ok(Some((*first).clone()));
        }
        let implicit: Vec<&String> = found.iter().map(|(n, _)| n).collect();
        if let Some(first) = implicit.first() {
            if implicit.iter().any(|n| n != first) {
                return Err(Error::Sqlstate(
                    "42P22",
                    "could not determine which collation to use for string comparison".into(),
                ));
            }
            return Ok(Some((*first).clone()));
        }
        Ok(None)
    }

    /// `__coll_key('collation', node)`, the `COLLATE` wrapper stripped.
    fn coll_key(node: &pg_query::protobuf::Node, collation: &str) -> pg_query::protobuf::Node {
        Self::coll_call("__coll_key", node, collation)
    }

    /// `__coll_keyv(...)`: the key with its text, for an extreme.
    fn coll_keyv(node: &pg_query::protobuf::Node, collation: &str) -> pg_query::protobuf::Node {
        Self::coll_call("__coll_keyv", node, collation)
    }

    fn coll_call(
        function: &str,
        node: &pg_query::protobuf::Node,
        collation: &str,
    ) -> pg_query::protobuf::Node {
        let inner = match node.node.as_ref() {
            Some(N::CollateClause(cc)) => {
                cc.arg.as_deref().cloned().unwrap_or_else(|| node.clone())
            }
            _ => node.clone(),
        };
        let name = pg_query::protobuf::Node {
            node: Some(N::AConst(pg_query::protobuf::AConst {
                isnull: false,
                location: -1,
                val: Some(a_const::Val::Sval(pg_query::protobuf::String {
                    sval: collation.to_string(),
                })),
            })),
        };
        pg_query::protobuf::Node {
            node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
                funcname: vec![pg_query::protobuf::Node {
                    node: Some(N::String(pg_query::protobuf::String {
                        sval: function.into(),
                    })),
                }],
                args: vec![name, inner],
                funcformat: pg_query::protobuf::CoercionForm::CoerceExplicitCall as i32,
                location: -1,
                ..Default::default()
            }))),
        }
    }

    /// A call of one of the string functions the citext extension
    /// overloads for a citext first argument, as the extension's SQL body
    /// defines it (citext--1.4.sql): the match is case-insensitive.
    fn citext_function(
        &self,
        f: &pg_query::protobuf::FuncCall,
    ) -> Option<pg_query::protobuf::Node> {
        if f.over.is_some() || f.agg_star || f.agg_distinct || f.funcname.len() > 2 {
            return None;
        }
        let name = func_name(f)?;
        let first = f.args.first()?;
        let citext = crate::collation::CITEXT;
        if !self.coll_of(first).is_some_and(|(c, x)| !x && c == citext) {
            return None;
        }
        // A pattern written as a plain text value picks text's function.
        if f.args.get(1).is_some_and(|a| self.plain_string(a)) {
            return None;
        }
        const QUOTE: &str = "__regex_quote(($2)::text)";
        let template = match (name.as_str(), f.args.len()) {
            ("strpos", 2) => "strpos(lower(($1)::text), lower(($2)::text))".to_string(),
            ("replace", 3) => format!("regexp_replace(($1)::text, {QUOTE}, ($3)::text, 'gi')"),
            ("split_part", 3) => {
                format!("(regexp_split_to_array(($1)::text, {QUOTE}, 'i'))[$3]")
            }
            (
                "regexp_match" | "regexp_matches" | "regexp_split_to_array"
                | "regexp_split_to_table",
                2,
            ) => format!("{name}(($1)::text, '(?i)' || ($2)::text)"),
            ("regexp_replace", 3) => "regexp_replace(($1)::text, ($2)::text, ($3)::text, 'i')".into(),
            ("translate", 3) => {
                "translate(translate(($1)::text, lower(($2)::text), ($3)::text), upper(($2)::text), ($3)::text)"
                    .into()
            }
            _ => return None,
        };
        let mut out = parse_expr(&format!("select {template}"))?;
        fn subst(node: &mut pg_query::protobuf::Node, args: &[pg_query::protobuf::Node]) {
            match node.node.as_mut() {
                Some(N::ParamRef(p)) => {
                    if let Some(a) = usize::try_from(p.number)
                        .ok()
                        .and_then(|n| n.checked_sub(1))
                        .and_then(|n| args.get(n))
                    {
                        *node = a.clone();
                    }
                }
                Some(N::FuncCall(f)) => f.args.iter_mut().for_each(|a| subst(a, args)),
                Some(N::AExpr(e)) => {
                    for side in [e.lexpr.as_deref_mut(), e.rexpr.as_deref_mut()]
                        .into_iter()
                        .flatten()
                    {
                        subst(side, args);
                    }
                }
                Some(N::TypeCast(tc)) => {
                    if let Some(a) = tc.arg.as_deref_mut() {
                        subst(a, args);
                    }
                }
                Some(N::AIndirection(ind)) => {
                    if let Some(a) = ind.arg.as_deref_mut() {
                        subst(a, args);
                    }
                    for i in &mut ind.indirection {
                        if let Some(N::AIndices(x)) = i.node.as_mut() {
                            if let Some(u) = x.uidx.as_deref_mut() {
                                subst(u, args);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        subst(&mut out, &f.args);
        Some(out)
    }

    /// An extreme read back under citext's collation is a citext again.
    fn typed_value(value: pg_query::protobuf::Node, collation: &str) -> pg_query::protobuf::Node {
        if collation != crate::collation::CITEXT {
            return value;
        }
        pg_query::protobuf::Node {
            node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
                arg: Some(Box::new(value)),
                type_name: Some(type_name_node("citext")),
                location: -1,
            }))),
        }
    }

    /// `__coll_value(key)`: an extreme key back to its text.
    fn coll_value(key: pg_query::protobuf::Node) -> pg_query::protobuf::Node {
        pg_query::protobuf::Node {
            node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
                funcname: vec![pg_query::protobuf::Node {
                    node: Some(N::String(pg_query::protobuf::String {
                        sval: "__coll_value".into(),
                    })),
                }],
                args: vec![key],
                funcformat: pg_query::protobuf::CoercionForm::CoerceExplicitCall as i32,
                location: -1,
                ..Default::default()
            }))),
        }
    }

    /// `__net_sortkey(node)`.
    fn net_key(node: pg_query::protobuf::Node) -> pg_query::protobuf::Node {
        pg_query::protobuf::Node {
            node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
                funcname: vec![pg_query::protobuf::Node {
                    node: Some(N::String(pg_query::protobuf::String {
                        sval: "__net_sortkey".into(),
                    })),
                }],
                args: vec![node],
                funcformat: pg_query::protobuf::CoercionForm::CoerceExplicitCall as i32,
                location: -1,
                ..Default::default()
            }))),
        }
    }

    /// One side of a comparison against network type `ty`, as its key: a
    /// value of the type as it is, anything else cast to it first (which
    /// validates and normalises a literal).
    fn net_side(&self, node: &pg_query::protobuf::Node, ty: &str) -> pg_query::protobuf::Node {
        if self.net_of(node).is_some() {
            return Self::net_key(node.clone());
        }
        Self::net_key(pg_query::protobuf::Node {
            node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
                arg: Some(Box::new(node.clone())),
                type_name: Some(type_name_node(ty)),
                location: -1,
            }))),
        })
    }

    /// `split_part(key, '|', 2)::ty`: an extreme key back to its value.
    fn net_value(key: pg_query::protobuf::Node, ty: &str) -> pg_query::protobuf::Node {
        let mut node = parse_expr(&format!("SELECT split_part(NULL::text, '|', 2)::{ty}"))
            .expect("a fixed shape parses");
        if let Some(N::TypeCast(tc)) = node.node.as_mut() {
            if let Some(N::FuncCall(f)) = tc.arg.as_deref_mut().and_then(|a| a.node.as_mut()) {
                f.args[0] = key;
            }
        }
        node
    }

    /// `array_position(ARRAY[labels]::text[], node::text)`.
    fn ord(node: pg_query::protobuf::Node, labels: &[String]) -> pg_query::protobuf::Node {
        let quoted: Vec<String> = labels
            .iter()
            .map(|l| format!("'{}'", l.replace('\'', "''")))
            .collect();
        let sql = format!(
            "SELECT array_position(ARRAY[{}]::text[], NULL::text)",
            quoted.join(", ")
        );
        let mut call = parse_expr(&sql).expect("a fixed shape parses");
        if let Some(N::FuncCall(f)) = call.node.as_mut() {
            f.args[1] = pg_query::protobuf::Node {
                node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
                    arg: Some(Box::new(node)),
                    type_name: Some(type_name_node("text")),
                    location: -1,
                }))),
            };
        }
        call
    }

    /// `CASE position WHEN 1 THEN 'first' ... END::enum` -- a position back
    /// to its label, typed as the enum.
    fn label_of(
        position: pg_query::protobuf::Node,
        ty: &str,
        labels: &[String],
    ) -> pg_query::protobuf::Node {
        let whens: Vec<String> = labels
            .iter()
            .enumerate()
            .map(|(i, l)| format!("WHEN {} THEN '{}'", i + 1, l.replace('\'', "''")))
            .collect();
        let sql = format!("SELECT (CASE NULL::int4 {} END)::{ty}", whens.join(" "));
        let mut node = parse_expr(&sql).expect("a fixed shape parses");
        if let Some(N::TypeCast(tc)) = node.node.as_mut() {
            if let Some(N::CaseExpr(c)) = tc.arg.as_deref_mut().and_then(|a| a.node.as_mut()) {
                c.arg = Some(Box::new(position));
            }
        }
        node
    }

    /// One side of a comparison against enum `labels`: an enum-typed operand
    /// by position, a literal label as its position.
    fn side(
        &self,
        node: &pg_query::protobuf::Node,
        ty: &str,
        labels: &[String],
    ) -> Result<pg_query::protobuf::Node> {
        if self.enum_of(node).is_some() {
            return Ok(Self::ord(node.clone(), labels));
        }
        if let Some(N::AConst(c)) = node.node.as_ref() {
            if let Some(a_const::Val::Sval(sv)) = c.val.as_ref() {
                let Some(i) = labels.iter().position(|l| *l == sv.sval) else {
                    return Err(Error::Sqlstate(
                        "22P02",
                        format!("invalid input value for enum {ty}: \"{}\"", sv.sval),
                    ));
                };
                return Ok(int_const((i + 1) as i64));
            }
        }
        // Anything else -- a parameter, an expression -- is cast to the
        // enum (which checks the label) and ranked.
        Ok(Self::ord(
            pg_query::protobuf::Node {
                node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
                    arg: Some(Box::new(node.clone())),
                    type_name: Some(type_name_node(ty)),
                    location: -1,
                }))),
            },
            labels,
        ))
    }

    fn sort_by(&mut self, item: &mut pg_query::protobuf::Node) {
        if let Some(N::SortBy(sb)) = item.node.as_mut() {
            if let Some(key) = sb.node.as_deref() {
                if let Some((_, labels)) = self.enum_of(key) {
                    sb.node = Some(Box::new(Self::ord(key.clone(), &labels)));
                    self.changed = true;
                } else if self.net_of(key).is_some() {
                    sb.node = Some(Box::new(Self::net_key(key.clone())));
                    self.changed = true;
                } else if let Some((collation, _)) = self.coll_of(key) {
                    sb.node = Some(Box::new(Self::coll_key(key, &collation)));
                    self.changed = true;
                }
            }
        }
    }

    fn window(&mut self, w: &mut pg_query::protobuf::WindowDef) {
        for o in &mut w.order_clause {
            self.sort_by(o);
        }
    }

    /// Rewrite an expression tree in place.
    fn expr(&mut self, node: &mut pg_query::protobuf::Node) -> Result<()> {
        // The comparison itself, before its operands are visited.
        if let Some(N::AExpr(e)) = node.node.as_ref() {
            let kind = AExprKind::try_from(e.kind);
            let op = operator_name(e).ok().map(str::to_string);
            let ordered = matches!(op.as_deref(), Some("<" | "<=" | ">" | ">="));
            let equality = matches!(op.as_deref(), Some("=" | "<>" | "!="));
            // Two citext arrays compare element keys.
            if kind == Ok(AExprKind::AexprOp) && (ordered || equality) {
                if let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) {
                    if self.citext_array(l) || self.citext_array(r) {
                        let citext = crate::collation::CITEXT;
                        let nl = Self::coll_call("__coll_keys", l, citext);
                        let nr = Self::coll_call("__coll_keys", r, citext);
                        if let Some(N::AExpr(e)) = node.node.as_mut() {
                            e.lexpr = Some(Box::new(nl));
                            e.rexpr = Some(Box::new(nr));
                        }
                        self.changed = true;
                        return Ok(());
                    }
                }
            }
            // BETWEEN under a locale collation: as the comparisons it
            // abbreviates, each then compared by key.
            if matches!(
                kind,
                Ok(AExprKind::AexprBetween | AExprKind::AexprNotBetween)
            ) {
                if let (Some(l), Some(N::List(bounds))) = (
                    e.lexpr.as_deref(),
                    e.rexpr.as_deref().and_then(|r| r.node.as_ref()),
                ) {
                    if let [lo, hi] = bounds.items.as_slice() {
                        if self.coll_between(&[l, lo, hi])?.is_some() {
                            let negated = kind == Ok(AExprKind::AexprNotBetween);
                            let cmp =
                                |op: &str,
                                 a: &pg_query::protobuf::Node,
                                 b: &pg_query::protobuf::Node| {
                                    pg_query::protobuf::Node {
                                        node: Some(N::AExpr(Box::new(pg_query::protobuf::AExpr {
                                            kind: AExprKind::AexprOp as i32,
                                            name: vec![pg_query::protobuf::Node {
                                                node: Some(N::String(pg_query::protobuf::String {
                                                    sval: op.to_string(),
                                                })),
                                            }],
                                            lexpr: Some(Box::new(a.clone())),
                                            rexpr: Some(Box::new(b.clone())),
                                            location: -1,
                                        }))),
                                    }
                                };
                            let (a, b) = if negated {
                                (cmp("<", l, lo), cmp(">", l, hi))
                            } else {
                                (cmp(">=", l, lo), cmp("<=", l, hi))
                            };
                            *node = pg_query::protobuf::Node {
                                node: Some(N::BoolExpr(Box::new(pg_query::protobuf::BoolExpr {
                                    xpr: None,
                                    boolop: if negated {
                                        BoolExprType::OrExpr as i32
                                    } else {
                                        BoolExprType::AndExpr as i32
                                    },
                                    args: vec![a, b],
                                    location: -1,
                                }))),
                            };
                            self.changed = true;
                            return self.expr(node);
                        }
                    }
                }
            }
            // A comparison under a LOCALE collation compares sort keys.
            if kind == Ok(AExprKind::AexprOp) && (ordered || equality) {
                if let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) {
                    if let Some(collation) = self.coll_between(&[l, r])? {
                        let (nl, nr) =
                            (Self::coll_key(l, &collation), Self::coll_key(r, &collation));
                        if let Some(N::AExpr(e)) = node.node.as_mut() {
                            e.lexpr = Some(Box::new(nl));
                            e.rexpr = Some(Box::new(nr));
                        }
                        self.changed = true;
                        return Ok(());
                    }
                }
            }
            // citext's pattern operators are case-insensitive: LIKE is
            // ILIKE and `~` is `~*` over a citext left operand, whatever
            // the pattern's type.
            if matches!(kind, Ok(AExprKind::AexprLike))
                || (kind == Ok(AExprKind::AexprOp)
                    && matches!(op.as_deref(), Some("~~" | "!~~" | "~" | "!~")))
            {
                if let Some(l) = e.lexpr.as_deref() {
                    if self
                        .coll_of(l)
                        .is_some_and(|(c, explicit)| !explicit && c == crate::collation::CITEXT)
                    {
                        let to = format!("{}*", op.clone().unwrap_or_default());
                        if let Some(N::AExpr(e)) = node.node.as_mut() {
                            if e.kind == AExprKind::AexprLike as i32 {
                                e.kind = AExprKind::AexprIlike as i32;
                            }
                            e.name = vec![pg_query::protobuf::Node {
                                node: Some(N::String(pg_query::protobuf::String { sval: to })),
                            }];
                        }
                        self.changed = true;
                        // Still visit the operands (a pattern's own subquery).
                        return self.expr(node);
                    }
                }
            }
            // LIKE has no meaning under a nondeterministic collation.
            if matches!(kind, Ok(AExprKind::AexprLike | AExprKind::AexprIlike))
                || (kind == Ok(AExprKind::AexprOp)
                    && matches!(op.as_deref(), Some("~~" | "~~*" | "!~~" | "!~~*")))
            {
                if let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) {
                    if let Some(collation) = self.coll_between(&[l, r])? {
                        if collation != crate::collation::CITEXT
                            && !crate::collation::resolve(&collation)?.deterministic()
                        {
                            return Err(Error::FeatureNotSupported(
                                "nondeterministic collations are not supported for LIKE".into(),
                            ));
                        }
                    }
                }
            }
            // `x = ANY(array)` over citext compares keys.
            if matches!(kind, Ok(AExprKind::AexprOpAny | AExprKind::AexprOpAll))
                && (ordered || equality)
            {
                if let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) {
                    let citext = crate::collation::CITEXT;
                    // `citext = ANY(ARRAY['A'])` is text's: the array is text[].
                    if self.citext_array(r) && !self.plain_string(l) {
                        let nl = Self::coll_key(l, citext);
                        let nr = Self::coll_call("__coll_keys", r, citext);
                        if let Some(N::AExpr(e)) = node.node.as_mut() {
                            e.lexpr = Some(Box::new(nl));
                            e.rexpr = Some(Box::new(nr));
                        }
                        self.changed = true;
                        return Ok(());
                    }
                }
            }
            if matches!(kind, Ok(AExprKind::AexprIn)) {
                if let (Some(l), Some(N::List(items))) = (
                    e.lexpr.as_deref(),
                    e.rexpr.as_deref().and_then(|r| r.node.as_ref()),
                ) {
                    let mut all: Vec<&pg_query::protobuf::Node> = vec![l];
                    all.extend(items.items.iter());
                    if let Some(collation) = self.coll_between(&all)? {
                        let items: Vec<pg_query::protobuf::Node> = items
                            .items
                            .iter()
                            .map(|i| Self::coll_key(i, &collation))
                            .collect();
                        let lkey = Self::coll_key(l, &collation);
                        if let Some(N::AExpr(e)) = node.node.as_mut() {
                            e.lexpr = Some(Box::new(lkey));
                            e.rexpr = Some(Box::new(pg_query::protobuf::Node {
                                node: Some(N::List(pg_query::protobuf::List { items })),
                            }));
                        }
                        self.changed = true;
                        return Ok(());
                    }
                }
            }
            if kind == Ok(AExprKind::AexprOp) && (ordered || equality) {
                if let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) {
                    if let Some(ty) = self.net_of(l).or_else(|| self.net_of(r)) {
                        let (nl, nr) = (self.net_side(l, &ty), self.net_side(r, &ty));
                        if let Some(N::AExpr(e)) = node.node.as_mut() {
                            e.lexpr = Some(Box::new(nl));
                            e.rexpr = Some(Box::new(nr));
                        }
                        self.changed = true;
                        return Ok(());
                    }
                }
            }
            // The containment operators (`<<`, `<<=`, `>>`, `>>=`, `&&`):
            // `__net_op(op, l, r)`, each side cast to the network type.
            if kind == Ok(AExprKind::AexprOp)
                && matches!(op.as_deref(), Some("<<" | "<<=" | ">>" | ">>=" | "&&"))
            {
                if let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) {
                    if let Some(ty) = self.net_of(l).or_else(|| self.net_of(r)) {
                        let cast = |n: &pg_query::protobuf::Node| -> pg_query::protobuf::Node {
                            if self.net_of(n).is_some() {
                                return n.clone();
                            }
                            pg_query::protobuf::Node {
                                node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
                                    arg: Some(Box::new(n.clone())),
                                    type_name: Some(type_name_node(&ty)),
                                    location: -1,
                                }))),
                            }
                        };
                        let op_text = pg_query::protobuf::Node {
                            node: Some(N::AConst(pg_query::protobuf::AConst {
                                isnull: false,
                                location: -1,
                                val: Some(a_const::Val::Sval(pg_query::protobuf::String {
                                    sval: op.clone().unwrap_or_default(),
                                })),
                            })),
                        };
                        *node = pg_query::protobuf::Node {
                            node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
                                funcname: vec![pg_query::protobuf::Node {
                                    node: Some(N::String(pg_query::protobuf::String {
                                        sval: "__net_op".into(),
                                    })),
                                }],
                                args: vec![op_text, cast(l), cast(r)],
                                funcformat: pg_query::protobuf::CoercionForm::CoerceExplicitCall
                                    as i32,
                                location: -1,
                                ..Default::default()
                            }))),
                        };
                        self.changed = true;
                        return Ok(());
                    }
                }
            }
            // Network arithmetic: `inet + n`, `n + inet`, `inet - n`,
            // `inet - inet` (a bigint), `~inet`, `inet & inet`, `inet | inet`.
            if kind == Ok(AExprKind::AexprOp)
                && matches!(op.as_deref(), Some("+" | "-" | "&" | "|" | "~"))
            {
                let (l, r) = (e.lexpr.as_deref(), e.rexpr.as_deref());
                let net_side = l
                    .and_then(|n| self.net_of(n))
                    .or_else(|| r.and_then(|n| self.net_of(n)));
                if let (Some(ty), Some(r)) = (net_side, r) {
                    let op = op.clone().unwrap_or_default();
                    let untyped = |n: &pg_query::protobuf::Node| {
                        matches!(n.node.as_ref(), Some(N::AConst(c))
                            if matches!(c.val, Some(a_const::Val::Sval(_))))
                    };
                    let as_net = |n: &pg_query::protobuf::Node| -> pg_query::protobuf::Node {
                        if self.net_of(n).is_some() {
                            return n.clone();
                        }
                        pg_query::protobuf::Node {
                            node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
                                arg: Some(Box::new(n.clone())),
                                type_name: Some(type_name_node(&ty)),
                                location: -1,
                            }))),
                        }
                    };
                    let both_net = l.is_some_and(|l| self.net_of(l).is_some() || untyped(l))
                        && (self.net_of(r).is_some() || untyped(r));
                    let call = |name: &str, args: Vec<pg_query::protobuf::Node>| {
                        pg_query::protobuf::Node {
                            node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
                                funcname: vec![pg_query::protobuf::Node {
                                    node: Some(N::String(pg_query::protobuf::String {
                                        sval: name.into(),
                                    })),
                                }],
                                args,
                                funcformat: pg_query::protobuf::CoercionForm::CoerceExplicitCall
                                    as i32,
                                location: -1,
                                ..Default::default()
                            }))),
                        }
                    };
                    let text = |v: &str| pg_query::protobuf::Node {
                        node: Some(N::AConst(pg_query::protobuf::AConst {
                            isnull: false,
                            location: -1,
                            val: Some(a_const::Val::Sval(pg_query::protobuf::String {
                                sval: v.into(),
                            })),
                        })),
                    };
                    // The result is an inet: cast, so it is TYPED as one.
                    let inet = |n: pg_query::protobuf::Node| pg_query::protobuf::Node {
                        node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
                            arg: Some(Box::new(n)),
                            type_name: Some(type_name_node("inet")),
                            location: -1,
                        }))),
                    };
                    *node = match (op.as_str(), l) {
                        ("-", Some(l)) if both_net => {
                            call("__net_diff", vec![as_net(l), as_net(r)])
                        }
                        ("&" | "|", Some(l)) => {
                            inet(call("__net_arith", vec![text(&op), as_net(l), as_net(r)]))
                        }
                        // A NULL placeholder would short-circuit the call.
                        ("~", None) => {
                            inet(call("__net_arith", vec![text("~"), text(""), as_net(r)]))
                        }
                        (_, Some(l)) => {
                            inet(call("__net_arith", vec![text(&op), l.clone(), r.clone()]))
                        }
                        _ => return Ok(()),
                    };
                    self.changed = true;
                    return Ok(());
                }
            }
            // `inet_col IN (...)`: each member cast to the type, so a literal
            // is compared in its canonical form.
            if matches!(kind, Ok(AExprKind::AexprIn)) {
                if let (Some(l), Some(N::List(items))) = (
                    e.lexpr.as_deref(),
                    e.rexpr.as_deref().and_then(|r| r.node.as_ref()),
                ) {
                    if let Some(ty) = self.net_of(l) {
                        let items: Vec<pg_query::protobuf::Node> =
                            items.items.iter().map(|i| self.net_side(i, &ty)).collect();
                        let lkey = Self::net_key(l.clone());
                        if let Some(N::AExpr(e)) = node.node.as_mut() {
                            e.lexpr = Some(Box::new(lkey));
                            e.rexpr = Some(Box::new(pg_query::protobuf::Node {
                                node: Some(N::List(pg_query::protobuf::List { items })),
                            }));
                        }
                        self.changed = true;
                        return Ok(());
                    }
                }
            }
            if kind == Ok(AExprKind::AexprOp) && ordered {
                let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) else {
                    return Ok(());
                };
                if let Some((ty, labels)) = self.enum_of(l).or_else(|| self.enum_of(r)) {
                    let (nl, nr) = (self.side(l, &ty, &labels)?, self.side(r, &ty, &labels)?);
                    if let Some(N::AExpr(e)) = node.node.as_mut() {
                        e.lexpr = Some(Box::new(nl));
                        e.rexpr = Some(Box::new(nr));
                    }
                    self.changed = true;
                    return Ok(());
                }
            }
            if matches!(
                kind,
                Ok(AExprKind::AexprBetween | AExprKind::AexprNotBetween)
            ) {
                if let (Some(l), Some(N::List(bounds))) = (
                    e.lexpr.as_deref(),
                    e.rexpr.as_deref().and_then(|r| r.node.as_ref()),
                ) {
                    if let Some((ty, labels)) = self.enum_of(l) {
                        // As the comparisons it abbreviates: the lowering has
                        // no BETWEEN over an expression.
                        let [lo, hi] = bounds.items.as_slice() else {
                            return Ok(());
                        };
                        let negated = kind == Ok(AExprKind::AexprNotBetween);
                        let cmp =
                            |op: &str, a: pg_query::protobuf::Node, b: pg_query::protobuf::Node| {
                                pg_query::protobuf::Node {
                                    node: Some(N::AExpr(Box::new(pg_query::protobuf::AExpr {
                                        kind: AExprKind::AexprOp as i32,
                                        name: vec![pg_query::protobuf::Node {
                                            node: Some(N::String(pg_query::protobuf::String {
                                                sval: op.to_string(),
                                            })),
                                        }],
                                        lexpr: Some(Box::new(a)),
                                        rexpr: Some(Box::new(b)),
                                        location: -1,
                                    }))),
                                }
                            };
                        let pos = self.side(l, &ty, &labels)?;
                        let (lo, hi) = (self.side(lo, &ty, &labels)?, self.side(hi, &ty, &labels)?);
                        let (a, b) = if negated {
                            (cmp("<", pos.clone(), lo), cmp(">", pos, hi))
                        } else {
                            (cmp(">=", pos.clone(), lo), cmp("<=", pos, hi))
                        };
                        *node = pg_query::protobuf::Node {
                            node: Some(N::BoolExpr(Box::new(pg_query::protobuf::BoolExpr {
                                xpr: None,
                                boolop: if negated {
                                    BoolExprType::OrExpr as i32
                                } else {
                                    BoolExprType::AndExpr as i32
                                },
                                args: vec![a, b],
                                location: -1,
                            }))),
                        };
                        self.changed = true;
                        return Ok(());
                    }
                }
            }
        }
        // `pg_collation_for(x)` / `COLLATION FOR (x)`: the collation the
        // operand carries, known here -- an explicit COLLATE, the column's
        // declared one, `"default"` for other text, NULL for a type that
        // takes none.
        if let Some(N::FuncCall(f)) = node.node.as_ref() {
            if func_name(f).as_deref() == Some("pg_collation_for")
                && f.args.len() == 1
                && !matches!(f.funcname.first().and_then(|n| n.node.as_ref()),
                    Some(N::String(s)) if s.sval == crate::COLLATION_FOR_SCHEMA)
            {
                let arg = &f.args[0];
                let rendered: Option<String> = match arg.node.as_ref() {
                    Some(N::CollateClause(cc)) => Some(crate::collation::clause_name(cc)),
                    Some(N::ColumnRef(c)) => match self.column_of(c) {
                        Some(col) => match col.extra.get_str("collation") {
                            Ok(name) => Some(name.to_string()),
                            // `name` is collatable, and always "C".
                            Err(_) if col.pg_type == "name" => Some("C".into()),
                            // Text -- or a VALUES / subquery column whose
                            // type is only settled when the statement runs.
                            Err(_) if crate::collation::collatable(&col.pg_type) => None,
                            // A parse-analysis error in PostgreSQL: raised
                            // whether or not a row is read.
                            Err(_) => {
                                return Err(Error::DatatypeMismatch(format!(
                                    "collations are not supported by type {}",
                                    crate::display_type(&col.pg_type)
                                )))
                            }
                        },
                        None => None,
                    },
                    _ => None,
                };
                // Anything else is decided by the operand's static type when
                // the statement runs (`collation_for_type`): NULL for an
                // untyped literal, "C" for `name`, "default" for other text,
                // 42804 for a type that takes no collation. A collation known
                // here rides as a marked string. Either way the call keeps its
                // name, so the column is still `pg_collation_for`.
                let hidden_arg = match rendered {
                    Some(name) => pg_query::protobuf::Node {
                        node: Some(N::AConst(pg_query::protobuf::AConst {
                            isnull: false,
                            location: -1,
                            val: Some(a_const::Val::Sval(pg_query::protobuf::String {
                                sval: format!("\u{1f}{name}"),
                            })),
                        })),
                    },
                    None => {
                        let typeof_arg = pg_query::protobuf::Node {
                            node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
                                funcname: vec![string_node("pg_typeof")],
                                args: vec![arg.clone()],
                                funcformat: pg_query::protobuf::CoercionForm::CoerceExplicitCall
                                    as i32,
                                location: -1,
                                ..Default::default()
                            }))),
                        };
                        pg_query::protobuf::Node {
                            node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
                                arg: Some(Box::new(typeof_arg)),
                                type_name: Some(type_name_node("text")),
                                location: -1,
                            }))),
                        }
                    }
                };
                *node = pg_query::protobuf::Node {
                    node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
                        funcname: vec![
                            string_node(crate::COLLATION_FOR_SCHEMA),
                            string_node("pg_collation_for"),
                        ],
                        args: vec![hidden_arg],
                        funcformat: pg_query::protobuf::CoercionForm::CoerceExplicitCall as i32,
                        location: f.location,
                        ..Default::default()
                    }))),
                };
                self.changed = true;
                return Ok(());
            }
        }
        // citext's own string functions: case-insensitive matching.
        if let Some(N::FuncCall(f)) = node.node.as_ref() {
            if let Some(rewritten) = self.citext_function(f) {
                *node = rewritten;
                self.changed = true;
                return self.expr(node);
            }
        }
        // `min(e)` / `max(e)`: the extreme POSITION, mapped back.
        if let Some(N::FuncCall(f)) = node.node.as_ref() {
            let name = func_name(f);
            if matches!(name.as_deref(), Some("min" | "max")) && f.over.is_none() {
                if let [arg] = f.args.as_slice() {
                    if let Some((collation, _)) = self.coll_of(arg) {
                        let mut agg = (**f).clone();
                        agg.args = vec![Self::coll_keyv(arg, &collation)];
                        let call = pg_query::protobuf::Node {
                            node: Some(N::FuncCall(Box::new(agg))),
                        };
                        *node = Self::typed_value(Self::coll_value(call), &collation);
                        self.changed = true;
                        return Ok(());
                    }
                    if let Some(ty) = self.net_of(arg) {
                        let mut agg = (**f).clone();
                        agg.args = vec![Self::net_key(arg.clone())];
                        let call = pg_query::protobuf::Node {
                            node: Some(N::FuncCall(Box::new(agg))),
                        };
                        *node = Self::net_value(call, &ty);
                        self.changed = true;
                        return Ok(());
                    }
                    if let Some((ty, labels)) = self.enum_of(arg) {
                        let mut agg = (**f).clone();
                        agg.args = vec![Self::ord(arg.clone(), &labels)];
                        let call = pg_query::protobuf::Node {
                            node: Some(N::FuncCall(Box::new(agg))),
                        };
                        *node = Self::label_of(call, &ty, &labels);
                        self.changed = true;
                        return Ok(());
                    }
                }
            }
        }
        if let Some(N::MinMaxExpr(m)) = node.node.as_ref() {
            let args: Vec<&pg_query::protobuf::Node> = m.args.iter().collect();
            if let Some(collation) = self.coll_between(&args)? {
                let mut mm = (**m).clone();
                mm.args = m
                    .args
                    .iter()
                    .map(|a| Self::coll_keyv(a, &collation))
                    .collect();
                let inner = pg_query::protobuf::Node {
                    node: Some(N::MinMaxExpr(Box::new(mm))),
                };
                *node = Self::typed_value(Self::coll_value(inner), &collation);
                self.changed = true;
                return Ok(());
            }
            if let Some(ty) = m.args.iter().find_map(|a| self.net_of(a)) {
                let mut mm = (**m).clone();
                mm.args = m.args.iter().map(|a| self.net_side(a, &ty)).collect();
                let inner = pg_query::protobuf::Node {
                    node: Some(N::MinMaxExpr(Box::new(mm))),
                };
                *node = Self::net_value(inner, &ty);
                self.changed = true;
                return Ok(());
            }
            if let Some((ty, labels)) = m.args.iter().find_map(|a| self.enum_of(a)) {
                let mut mm = (**m).clone();
                mm.args = m
                    .args
                    .iter()
                    .map(|a| self.side(a, &ty, &labels))
                    .collect::<Result<Vec<_>>>()?;
                let inner = pg_query::protobuf::Node {
                    node: Some(N::MinMaxExpr(Box::new(mm))),
                };
                *node = Self::label_of(inner, &ty, &labels);
                self.changed = true;
                return Ok(());
            }
        }
        // Window and aggregate ORDER BYs inside the expression.
        if let Some(N::FuncCall(f)) = node.node.as_mut() {
            for o in &mut f.agg_order {
                self.sort_by(o);
            }
            if let Some(w) = f.over.as_deref_mut() {
                self.window(w);
            }
        }
        // Then the children.
        let mut kids: Vec<&mut pg_query::protobuf::Node> = Vec::new();
        match node.node.as_mut() {
            Some(N::AExpr(e)) => {
                kids.extend(e.lexpr.as_deref_mut());
                kids.extend(e.rexpr.as_deref_mut());
            }
            Some(N::BoolExpr(b)) => kids.extend(b.args.iter_mut()),
            Some(N::FuncCall(f)) => kids.extend(f.args.iter_mut()),
            Some(N::TypeCast(t)) => kids.extend(t.arg.as_deref_mut()),
            Some(N::NullTest(t)) => kids.extend(t.arg.as_deref_mut()),
            Some(N::CoalesceExpr(c)) => kids.extend(c.args.iter_mut()),
            Some(N::CaseExpr(c)) => {
                kids.extend(c.arg.as_deref_mut());
                for w in &mut c.args {
                    if let Some(N::CaseWhen(cw)) = w.node.as_mut() {
                        kids.extend(cw.expr.as_deref_mut());
                        kids.extend(cw.result.as_deref_mut());
                    }
                }
                kids.extend(c.defresult.as_deref_mut());
            }
            Some(N::ResTarget(rt)) => kids.extend(rt.val.as_deref_mut()),
            Some(N::List(l)) => kids.extend(l.items.iter_mut()),
            _ => {}
        }
        for k in kids {
            self.expr(k)?;
        }
        Ok(())
    }
}

fn int_const(v: i64) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::AConst(pg_query::protobuf::AConst {
            isnull: false,
            location: -1,
            val: Some(a_const::Val::Ival(pg_query::protobuf::Integer {
                ival: v as i32,
            })),
        })),
    }
}

/// The expression of `SELECT <expr>`.
fn parse_expr(sql: &str) -> Option<pg_query::protobuf::Node> {
    let parsed = pg_query::parse(sql).ok()?;
    let stmt = parsed.protobuf.stmts.first()?.stmt.clone()?;
    let Some(N::SelectStmt(sel)) = stmt.node else {
        return None;
    };
    match sel.target_list.first()?.node.clone()? {
        N::ResTarget(rt) => rt.val.map(|v| *v),
        _ => None,
    }
}

/// Replace the grouped columns inside `node` (outside aggregates) with
/// their group's value.
fn nested(
    node: &mut pg_query::protobuf::Node,
    grouped: &[(pg_query::protobuf::Node, String)],
    first: &dyn Fn(&pg_query::protobuf::Node) -> pg_query::protobuf::Node,
) {
    if let Some((col, _)) = grouped
        .iter()
        .find(|(g, _)| crate::same_expression(g, node))
    {
        *node = first(col);
        return;
    }
    match node.node.as_mut() {
        Some(N::TypeCast(tc)) => {
            if let Some(a) = tc.arg.as_deref_mut() {
                nested(a, grouped, first);
            }
        }
        Some(N::AExpr(e)) => {
            for side in [e.lexpr.as_deref_mut(), e.rexpr.as_deref_mut()]
                .into_iter()
                .flatten()
            {
                nested(side, grouped, first);
            }
        }
        Some(N::FuncCall(f)) if !crate::is_aggregate_call(f) && f.over.is_none() => {
            for a in &mut f.args {
                nested(a, grouped, first);
            }
        }
        _ => {}
    }
}

/// Rewrite a SELECT so its enum ordering is the declaration's; `None` when
/// it has nothing enum-ordered in it.
pub(crate) fn rewrite(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Option<pg_query::protobuf::SelectStmt>> {
    // A CTE is a derived relation too, and shadows a table of its name.
    let ctes: Vec<(String, TableDef)> = s
        .with_clause
        .iter()
        .flat_map(|w| &w.ctes)
        .filter_map(|c| match c.node.as_ref() {
            Some(N::CommonTableExpr(cte)) => match cte.ctequery.as_deref()?.node.as_ref() {
                Some(N::SelectStmt(q)) => Some((
                    cte.ctename.clone(),
                    derived_def(&cte.ctename, q, &cte.aliascolnames),
                )),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let with_ctes = |name: &str| {
        ctes.iter()
            .find(|(n, _)| n == name)
            .map(|(_, d)| d.clone())
            .or_else(|| lookup(name))
    };
    let mut defs = Vec::new();
    scope(&s.from_clause, &with_ctes, &mut defs);
    let mut out = s.clone();
    let mut r = Rewriter {
        scope: &defs,
        changed: false,
        collate_locations: std::cell::RefCell::new(Vec::new()),
        derived: std::cell::RefCell::new(Vec::new()),
    };
    // `ORDER BY <position>` / `ORDER BY <output name>` naming a plain enum
    // column: the sort is over the target's expression.
    let targets: Vec<(String, pg_query::protobuf::Node)> = out
        .target_list
        .iter()
        .filter_map(|t| match t.node.as_ref() {
            Some(N::ResTarget(rt)) => Some((rt.name.clone(), rt.val.as_deref()?.clone())),
            _ => None,
        })
        .collect();
    for item in &mut out.sort_clause {
        let Some(N::SortBy(sb)) = item.node.as_mut() else {
            continue;
        };
        let target = match sb.node.as_deref().and_then(|n| n.node.as_ref()) {
            Some(N::AConst(c)) => match c.val.as_ref() {
                Some(a_const::Val::Ival(i)) => usize::try_from(i.ival)
                    .ok()
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| targets.get(i))
                    .map(|(_, v)| v.clone()),
                _ => None,
            },
            // An output column's name, before any input column's: its
            // explicit alias, or the name a cast of a column keeps
            // (`x::text` is `x`).
            Some(N::ColumnRef(c)) if c.fields.len() == 1 => {
                let name = column_ref_name(c).unwrap_or_default();
                let implicit = |v: &pg_query::protobuf::Node| match v.node.as_ref() {
                    Some(N::TypeCast(tc)) => cast_chain_over_column(tc)
                        .ok()
                        .is_some_and(|(col, _)| col == name),
                    _ => false,
                };
                targets
                    .iter()
                    .find(|(n, _)| *n == name)
                    .or_else(|| targets.iter().find(|(n, v)| n.is_empty() && implicit(v)))
                    .map(|(_, v)| v.clone())
            }
            _ => None,
        };
        if let Some(t) = target {
            // An enum target is sorted by position (below); a non-enum one
            // named like an enum column is sorted as itself.
            if r.enum_of(&t).is_some()
                || r.net_of(&t).is_some()
                || r.coll_of(&t).is_some()
                || !matches!(t.node.as_ref(), Some(N::ColumnRef(_)))
            {
                sb.node = Some(Box::new(t));
            }
        }
    }
    // `GROUP BY` a column of a NONDETERMINISTIC collation groups what the
    // collation calls equal: by its key, answering the group's FIRST value in
    // input order (PostgreSQL's hash aggregate keeps the row that opened the
    // group), which still orders by the collation.
    let mut grouped: Vec<(pg_query::protobuf::Node, String)> = Vec::new();
    for g in &mut out.group_clause {
        if let Some((c, _)) = r.coll_of(g) {
            if !crate::collation::resolve(&c)?.deterministic() {
                grouped.push((g.clone(), c.clone()));
                *g = Rewriter::coll_key(g, &c);
                r.changed = true;
            }
        }
    }
    if !grouped.is_empty() {
        let fcall = |name: &str, args: Vec<pg_query::protobuf::Node>| pg_query::protobuf::Node {
            node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
                funcname: vec![pg_query::protobuf::Node {
                    node: Some(N::String(pg_query::protobuf::String { sval: name.into() })),
                }],
                args,
                funcformat: pg_query::protobuf::CoercionForm::CoerceExplicitCall as i32,
                location: -1,
                ..Default::default()
            }))),
        };
        // `(array_agg(col))[1]`: array_agg keeps input order.
        let first_of_group = |col: &pg_query::protobuf::Node| pg_query::protobuf::Node {
            node: Some(N::AIndirection(Box::new(
                pg_query::protobuf::AIndirection {
                    arg: Some(Box::new(fcall(
                        "array_agg",
                        vec![match col.node.as_ref() {
                            Some(N::CollateClause(cc)) => {
                                cc.arg.as_deref().cloned().unwrap_or_else(|| col.clone())
                            }
                            _ => col.clone(),
                        }],
                    ))),
                    indirection: vec![pg_query::protobuf::Node {
                        node: Some(N::AIndices(Box::new(pg_query::protobuf::AIndices {
                            is_slice: false,
                            lidx: None,
                            uidx: Some(Box::new(pg_query::protobuf::Node {
                                node: Some(N::AConst(pg_query::protobuf::AConst {
                                    val: Some(a_const::Val::Ival(pg_query::protobuf::Integer {
                                        ival: 1,
                                    })),
                                    ..Default::default()
                                })),
                            })),
                        }))),
                    }],
                },
            ))),
        };
        for t in &mut out.target_list {
            let Some(N::ResTarget(rt)) = t.node.as_mut() else {
                continue;
            };
            let Some(v) = rt.val.as_deref() else {
                continue;
            };
            if let Some((col, c)) = grouped.iter().find(|(g, _)| crate::same_expression(g, v)) {
                if rt.name.is_empty() {
                    if let Some(N::ColumnRef(cr)) = col.node.as_ref() {
                        rt.name = column_ref_name(cr).unwrap_or_default();
                    }
                }
                let value = first_of_group(col);
                r.derived.borrow_mut().push((value.clone(), c.clone()));
                rt.val = Some(Box::new(value));
            } else if let Some(v) = rt.val.as_deref_mut() {
                // The grouped column INSIDE an expression (`lower(x::text)`)
                // reads the group's value too; an aggregate's argument
                // still reads the rows.
                nested(v, &grouped, &first_of_group);
            }
        }
        // A sort term naming the grouped column orders by the group's key.
        for item in &mut out.sort_clause {
            let Some(N::SortBy(sb)) = item.node.as_mut() else {
                continue;
            };
            let Some(key) = sb.node.as_deref() else {
                continue;
            };
            if let Some((col, c)) = grouped.iter().find(|(g, _)| crate::same_expression(g, key)) {
                // By the group's collation key, which every member shares.
                sb.node = Some(Box::new(fcall("min", vec![Rewriter::coll_key(col, c)])));
            } else if let Some(k) = sb.node.as_deref_mut() {
                nested(k, &grouped, &first_of_group);
            }
        }
    }
    // `SELECT DISTINCT` over a NONDETERMINISTIC collation: rows are the same
    // when the collation says so, so it is DISTINCT ON their keys.
    if matches!(out.distinct_clause.as_slice(), [d] if d.node.is_none()) {
        let mut keys = Vec::new();
        let mut any = false;
        for t in &out.target_list {
            let Some(N::ResTarget(rt)) = t.node.as_ref() else {
                keys.clear();
                break;
            };
            let Some(v) = rt.val.as_deref() else {
                keys.clear();
                break;
            };
            match r.coll_of(v) {
                Some((c, _)) if !crate::collation::resolve(&c)?.deterministic() => {
                    any = true;
                    keys.push(Rewriter::coll_key(v, &c));
                }
                _ => keys.push(v.clone()),
            }
        }
        if any && keys.len() == out.target_list.len() {
            out.distinct_clause = keys;
            r.changed = true;
        }
    }
    // `count(DISTINCT x)` over a NONDETERMINISTIC collation counts what the
    // collation calls distinct: the distinct KEYS.
    for n in out
        .target_list
        .iter_mut()
        .chain(out.having_clause.as_deref_mut())
    {
        let mut failed = None;
        crate::walk_expr(n, &mut |node| {
            let Some(N::FuncCall(f)) = node.node.as_mut() else {
                return Ok(());
            };
            if !f.agg_distinct || crate::func_name(f).as_deref() != Some("count") {
                return Ok(());
            }
            if let [arg] = f.args.as_mut_slice() {
                if let Some((c, _)) = r.coll_of(arg) {
                    match crate::collation::resolve(&c) {
                        Ok(res) if !res.deterministic() => {
                            *arg = Rewriter::coll_key(arg, &c);
                            r.changed = true;
                        }
                        Ok(_) => {}
                        Err(e) => failed = Some(e),
                    }
                }
            }
            Ok(())
        })?;
        if let Some(e) = failed {
            return Err(e);
        }
    }
    for item in &mut out.sort_clause {
        r.sort_by(item);
    }
    // A JOIN's ON condition compares like a WHERE.
    fn join_quals(node: &mut pg_query::protobuf::Node, r: &mut Rewriter) -> Result<()> {
        if let Some(N::JoinExpr(j)) = node.node.as_mut() {
            for side in [j.larg.as_deref_mut(), j.rarg.as_deref_mut()]
                .into_iter()
                .flatten()
            {
                join_quals(side, r)?;
            }
            if let Some(q) = j.quals.as_deref_mut() {
                r.expr(q)?;
            }
        }
        Ok(())
    }
    for item in &mut out.from_clause {
        join_quals(item, &mut r)?;
    }
    for t in &mut out.target_list {
        r.expr(t)?;
    }
    for n in out
        .where_clause
        .as_deref_mut()
        .into_iter()
        .chain(out.having_clause.as_deref_mut())
    {
        r.expr(n)?;
    }
    for w in &mut out.window_clause {
        if let Some(N::WindowDef(def)) = w.node.as_mut() {
            r.window(def);
        }
    }
    Ok(r.changed.then_some(out))
}

/// The comparison rewrite over an UPDATE / DELETE: its WHERE against the
/// target relation (and a DELETE's USING / an UPDATE's FROM). `true` when
/// anything changed.
pub(crate) fn rewrite_dml(
    node: &mut pg_query::protobuf::Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<bool> {
    let (relation, extra, where_clause) = match node.node.as_mut() {
        Some(N::UpdateStmt(u)) => (
            u.relation.clone(),
            u.from_clause.clone(),
            u.where_clause.as_deref_mut(),
        ),
        Some(N::DeleteStmt(d)) => (
            d.relation.clone(),
            d.using_clause.clone(),
            d.where_clause.as_deref_mut(),
        ),
        _ => return Ok(false),
    };
    let Some(where_clause) = where_clause else {
        return Ok(false);
    };
    let mut items: Vec<pg_query::protobuf::Node> = relation
        .map(|rv| pg_query::protobuf::Node {
            node: Some(N::RangeVar(rv)),
        })
        .into_iter()
        .collect();
    items.extend(extra);
    let mut defs = Vec::new();
    scope(&items, lookup, &mut defs);
    let mut r = Rewriter {
        scope: &defs,
        changed: false,
        collate_locations: std::cell::RefCell::new(Vec::new()),
        derived: std::cell::RefCell::new(Vec::new()),
    };
    r.expr(where_clause)?;
    Ok(r.changed)
}
