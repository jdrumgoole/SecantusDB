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
        // `min(e)` / `max(e)`: the extreme POSITION, mapped back.
        if let Some(N::FuncCall(f)) = node.node.as_ref() {
            let name = func_name(f);
            if matches!(name.as_deref(), Some("min" | "max")) && f.over.is_none() {
                if let [arg] = f.args.as_slice() {
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

/// Rewrite a SELECT so its enum ordering is the declaration's; `None` when
/// it has nothing enum-ordered in it.
pub(crate) fn rewrite(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Option<pg_query::protobuf::SelectStmt>> {
    let mut defs = Vec::new();
    scope(&s.from_clause, lookup, &mut defs);
    let mut out = s.clone();
    let mut r = Rewriter {
        scope: &defs,
        changed: false,
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
            if r.enum_of(&t).is_some() || !matches!(t.node.as_ref(), Some(N::ColumnRef(_))) {
                sb.node = Some(Box::new(t));
            }
        }
    }
    for item in &mut out.sort_clause {
        r.sort_by(item);
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
    };
    r.expr(where_clause)?;
    Ok(r.changed)
}
