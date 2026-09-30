//! The rule system: an INSERT / UPDATE / DELETE on a relation with rules is
//! REWRITTEN into the statements PostgreSQL's `rewriteHandler.c` produces,
//! which the executor then runs in order.
//!
//! Each rule action is joined to the original statement's source rows
//! (`rewriteRuleAction`): a derived relation `__rule_src` holding, per
//! affected row, OLD's columns as `o_<col>` and NEW's as `n_<col>` (for an
//! UPDATE, the SET expressions already applied). `old.x` / `new.x` in the
//! action and its WHERE become `__rule_src.o_x` / `__rule_src.n_x`. The
//! source is joined only when PostgreSQL would join it: an INSERT's always,
//! an UPDATE's / DELETE's when the action, the rule's WHERE or the
//! statement's own WHERE refers to the target. So an action naming neither
//! runs ONCE, and an aggregating action sees the whole join.
//!
//! Order and effect, as PostgreSQL has them:
//! * an INSERT runs first and its rule actions after; an UPDATE's or
//!   DELETE's actions run before it;
//! * an unconditional INSTEAD rule removes the original statement, and a
//!   conditional one adds `(qual) IS NOT TRUE` to it;
//! * the command tag is the original's, or -- when an unconditional INSTEAD
//!   removed it -- the last INSTEAD action of the same command type's, or
//!   that type with zero rows.

use pg_query::protobuf::node::Node as N;

use crate::{Error, Result, TableDef};

/// One enabled rule, as the catalog holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleDef {
    pub table: String,
    pub name: String,
    /// `INSERT` / `UPDATE` / `DELETE`.
    pub event: String,
    pub instead: bool,
    pub condition: Option<String>,
    pub actions: Vec<String>,
}

/// A rewritten statement: SQL steps to plan and run in order.
#[derive(Debug, Clone, PartialEq)]
pub struct RulePlan {
    pub table: String,
    /// `INSERT` / `UPDATE` / `DELETE`: the original's command type.
    pub kind: String,
    /// `(sql, is_the_original_statement)`.
    pub steps: Vec<(String, bool)>,
    /// Which step's result is the statement's (`None`: the kind with 0 rows).
    pub tag_step: Option<usize>,
    /// The tables the steps write, for a savepoint's capture.
    pub targets: Vec<String>,
    /// The original statement's bound parameters, which its steps keep.
    pub params: Vec<bson::Bson>,
    /// Their declared types, when the client declared them.
    pub param_types: Vec<Option<String>>,
}

thread_local! {
    static RULES: std::cell::RefCell<Vec<RuleDef>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Relations whose rules are NOT applied: the original statement of a
    /// rewrite, planned as itself.
    static SUPPRESSED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Install the database's enabled rules for the statements that follow.
pub fn set_rules(rules: Vec<RuleDef>) {
    RULES.with(|r| *r.borrow_mut() = rules);
}

/// Run `f` with `table`'s rules not applied.
pub fn with_rules_suppressed<R>(table: &str, f: impl FnOnce() -> R) -> R {
    SUPPRESSED.with(|s| s.borrow_mut().push(table.to_string()));
    let out = f();
    SUPPRESSED.with(|s| {
        s.borrow_mut().pop();
    });
    out
}

fn deparse(n: &pg_query::protobuf::Node) -> Result<String> {
    n.deparse().map_err(|e| Error::Parse(e.to_string()))
}

fn q(ident: &str) -> String {
    crate::scalar::quote_identifier(ident)
}

/// Replace every `new.<col>` / `old.<col>` in `sql` (token-aware: never
/// inside a literal) by what `map` answers for `(which, col)`.
fn substitute(sql: &str, map: &dyn Fn(&str, &str) -> Option<String>) -> Result<String> {
    let scanned = pg_query::scan(sql).map_err(|e| Error::Parse(e.to_string()))?;
    let toks: Vec<(usize, usize)> = scanned
        .tokens
        .iter()
        .map(|t| (t.start as usize, t.end as usize))
        .collect();
    let text = |i: usize| sql.get(toks[i].0..toks[i].1).unwrap_or("");
    let unquote = |t: &str| -> String {
        match t.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
            Some(x) => x.replace("\"\"", "\""),
            None => t.to_ascii_lowercase(),
        }
    };
    let mut out = String::new();
    let mut cursor = 0;
    let mut i = 0;
    while i < toks.len() {
        let w = text(i).to_ascii_lowercase();
        if (w == "new" || w == "old")
            && i + 2 < toks.len()
            && text(i + 1) == "."
            && toks[i + 1].0 == toks[i].1
            // Not itself qualified (`x.new.y` is something else).
            && (i == 0 || text(i - 1) != ".")
        {
            let col = unquote(text(i + 2));
            if let Some(rep) = map(&w, &col) {
                out.push_str(&sql[cursor..toks[i].0]);
                out.push_str(&rep);
                cursor = toks[i + 2].1;
                i += 3;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&sql[cursor..]);
    Ok(out)
}

/// Does `sql` mention `new.` / `old.` at all?
fn mentions_pseudo(sql: &str) -> bool {
    substitute(sql, &|_, _| Some(String::new()))
        .map(|s| s != sql)
        .unwrap_or(false)
}

/// A column's default as SQL, for NEW's unlisted columns.
fn default_sql(c: &secantus_pgcatalog::Column) -> String {
    if let Some(seq) = c.sequence.as_deref() {
        return format!("nextval('{}')", seq.replace('\'', "''"));
    }
    if let Some(e) = c.default_expr() {
        return e.to_string();
    }
    if let (Some(sql), Some(_)) = (c.default_sql(), c.default.as_ref()) {
        return sql.to_string();
    }
    let ty = crate::display_type(&c.pg_type);
    match c.default.as_ref() {
        None | Some(bson::Bson::Null) => format!("NULL::{ty}"),
        Some(bson::Bson::String(s)) => format!("{}::{ty}", crate::scalar::quote_literal(s)),
        Some(bson::Bson::Boolean(b)) => format!("{b}::{ty}"),
        Some(v) => match crate::numeric::numeric_text(v) {
            Some(t) => format!("({t})::{ty}"),
            None => format!("NULL::{ty}"),
        },
    }
}

/// The original statement's facts a rewrite needs.
struct Source {
    /// The `__rule_src` relation, as a FROM item.
    from: String,
    /// Whether an UPDATE / DELETE's own WHERE names the target (so the
    /// source must be joined even for an action that names neither).
    has_where: bool,
    /// An UPDATE's SET expressions by column (deparsed), for a conditional
    /// INSTEAD's qual on the original.
    set: Vec<(String, String)>,
    /// The name the original statement calls the target by.
    target_ref: String,
    /// An INSERT's columns NOT in its column list, with their defaults:
    /// `new.<col>` is the default itself, evaluated where it is used.
    defaults: Vec<(String, String)>,
    /// An INSERT's listed columns, which `__rule_src` carries.
    provided: Vec<String>,
}

impl Source {
    /// What `new.<col>` / `old.<col>` reads as, against `__rule_src`.
    fn pseudo(&self, which: &str, col: &str) -> Option<String> {
        if which == "new" {
            if let Some((_, d)) = self.defaults.iter().find(|(c, _)| c == col) {
                return Some(format!("({d})"));
            }
        }
        Some(format!(
            "__rule_src.{}",
            q(&format!("{}_{col}", if which == "old" { "o" } else { "n" }))
        ))
    }
}

fn select_list(def: &TableDef, old: Option<&str>, new: &dyn Fn(&str) -> String) -> String {
    let mut cols = Vec::new();
    for c in &def.columns {
        if let Some(r) = old {
            cols.push(format!(
                "{r}.{} AS {}",
                q(&c.name),
                q(&format!("o_{}", c.name))
            ));
        }
        cols.push(format!(
            "({})::{} AS {}",
            new(&c.name),
            crate::display_type(&c.pg_type),
            q(&format!("n_{}", c.name))
        ));
    }
    cols.join(", ")
}

fn source_of(node: &N, def: &TableDef) -> Result<Option<Source>> {
    Ok(Some(match node {
        N::InsertStmt(ins) => {
            let rel = ins
                .relation
                .as_ref()
                .ok_or_else(|| Error::Parse("INSERT without a table".into()))?;
            let target_ref = rel
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .unwrap_or_else(|| rel.relname.clone());
            let listed: Vec<String> = ins
                .cols
                .iter()
                .filter_map(|c| match c.node.as_ref() {
                    Some(N::ResTarget(rt)) if rt.indirection.is_empty() => Some(rt.name.clone()),
                    _ => None,
                })
                .collect();
            if listed.len() != ins.cols.len() {
                return Ok(None);
            }
            let names: Vec<String> = if listed.is_empty() {
                def.columns.iter().map(|c| c.name.clone()).collect()
            } else {
                listed
            };
            let defaults_for = |provided: &[String]| -> Vec<(String, String)> {
                def.columns
                    .iter()
                    .filter(|c| !provided.contains(&c.name))
                    .map(|c| (c.name.clone(), default_sql(c)))
                    .collect()
            };
            let (from, provided) = match ins.select_stmt.as_deref() {
                None => {
                    // DEFAULT VALUES: one row, every column its default.
                    (
                        "(SELECT 1 AS __rule_one) AS __rule_src".to_string(),
                        Vec::new(),
                    )
                }
                Some(sel) => {
                    // A VALUES `DEFAULT` is the column's default expression
                    // here, which a derived table can hold.
                    let mut sel = sel.clone();
                    if let Some(N::SelectStmt(s)) = sel.node.as_mut() {
                        fill_values_defaults(s, &names, def)?;
                    }
                    let Some(N::SelectStmt(s)) = sel.node.as_ref() else {
                        return Ok(None);
                    };
                    let body = deparse(&sel)?;
                    let width = if !s.values_lists.is_empty() {
                        match s.values_lists.first().and_then(|r| r.node.as_ref()) {
                            Some(N::List(l)) => l.items.len(),
                            _ => return Ok(None),
                        }
                    } else {
                        names.len()
                    };
                    let aliases: Vec<String> = (1..=width).map(|i| format!("__v{i}")).collect();
                    let provided: Vec<String> = names.iter().take(width).cloned().collect();
                    let list: Vec<String> = provided
                        .iter()
                        .enumerate()
                        .map(|(i, c)| {
                            let ty = def
                                .column(c)
                                .map(|c| crate::display_type(&c.pg_type))
                                .unwrap_or_else(|| "text".into());
                            format!("(__rule_in.__v{})::{ty} AS {}", i + 1, q(&format!("n_{c}")))
                        })
                        .collect();
                    (
                        format!(
                            "(SELECT {} FROM ({body}) AS __rule_in({})) AS __rule_src",
                            list.join(", "),
                            aliases.join(", ")
                        ),
                        provided,
                    )
                }
            };
            Source {
                from,
                has_where: false,
                set: Vec::new(),
                target_ref,
                defaults: defaults_for(&provided),
                provided,
            }
        }
        N::UpdateStmt(u) => {
            let rel = u
                .relation
                .as_ref()
                .ok_or_else(|| Error::Parse("UPDATE without a table".into()))?;
            let target_ref = rel
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .unwrap_or_else(|| rel.relname.clone());
            let mut set = Vec::new();
            for t in &u.target_list {
                let Some(N::ResTarget(rt)) = t.node.as_ref() else {
                    return Ok(None);
                };
                if !rt.indirection.is_empty() {
                    return Ok(None);
                }
                let v = rt
                    .val
                    .as_deref()
                    .ok_or_else(|| Error::Parse("SET without a value".into()))?;
                if matches!(
                    v.node.as_ref(),
                    Some(N::MultiAssignRef(_) | N::SetToDefault(_))
                ) {
                    return Ok(None);
                }
                set.push((rt.name.clone(), deparse_expr(v)?));
            }
            let r = q(&target_ref);
            let list = select_list(def, Some(&r), &|c| {
                set.iter()
                    .find(|(n, _)| n == c)
                    .map(|(_, e)| e.clone())
                    .unwrap_or_else(|| format!("{r}.{}", q(c)))
            });
            let mut from = vec![format!(
                "{}{}",
                q(&rel.relname),
                rel.alias
                    .as_ref()
                    .map(|a| format!(" AS {}", q(&a.aliasname)))
                    .unwrap_or_default()
            )];
            for f in &u.from_clause {
                from.push(deparse_from_item(f)?);
            }
            let wh = match u.where_clause.as_deref() {
                Some(w) => format!(" WHERE {}", deparse_expr(w)?),
                None => String::new(),
            };
            Source {
                from: format!("(SELECT {list} FROM {}{wh}) AS __rule_src", from.join(", ")),
                has_where: u.where_clause.is_some(),
                set,
                target_ref,
                defaults: Vec::new(),
                provided: Vec::new(),
            }
        }
        N::DeleteStmt(d) => {
            let rel = d
                .relation
                .as_ref()
                .ok_or_else(|| Error::Parse("DELETE without a table".into()))?;
            let target_ref = rel
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .unwrap_or_else(|| rel.relname.clone());
            let r = q(&target_ref);
            let list: Vec<String> = def
                .columns
                .iter()
                .map(|c| format!("{r}.{} AS {}", q(&c.name), q(&format!("o_{}", c.name))))
                .collect();
            let mut from = vec![format!(
                "{}{}",
                q(&rel.relname),
                rel.alias
                    .as_ref()
                    .map(|a| format!(" AS {}", q(&a.aliasname)))
                    .unwrap_or_default()
            )];
            for f in &d.using_clause {
                from.push(deparse_from_item(f)?);
            }
            let wh = match d.where_clause.as_deref() {
                Some(w) => format!(" WHERE {}", deparse_expr(w)?),
                None => String::new(),
            };
            Source {
                from: format!(
                    "(SELECT {} FROM {}{wh}) AS __rule_src",
                    list.join(", "),
                    from.join(", ")
                ),
                has_where: d.where_clause.is_some(),
                set: Vec::new(),
                target_ref,
                defaults: Vec::new(),
                provided: Vec::new(),
            }
        }
        _ => return Ok(None),
    }))
}

fn deparse_expr(n: &pg_query::protobuf::Node) -> Result<String> {
    crate::deparse_expr(n)
}

fn deparse_from_item(n: &pg_query::protobuf::Node) -> Result<String> {
    // A FROM item deparses inside a SELECT.
    let sel = pg_query::protobuf::SelectStmt {
        target_list: vec![pg_query::protobuf::Node {
            node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                val: Some(Box::new(pg_query::protobuf::Node {
                    node: Some(N::AConst(pg_query::protobuf::AConst {
                        val: Some(pg_query::protobuf::a_const::Val::Ival(
                            pg_query::protobuf::Integer { ival: 1 },
                        )),
                        ..Default::default()
                    })),
                })),
                ..Default::default()
            }))),
        }],
        from_clause: vec![n.clone()],
        // Enum fields left at 0 are `Undefined`, which libpg_query's
        // deparser asserts on.
        op: pg_query::protobuf::SetOperation::SetopNone as i32,
        limit_option: pg_query::protobuf::LimitOption::Default as i32,
        ..Default::default()
    };
    let text = deparse(&pg_query::protobuf::Node {
        node: Some(N::SelectStmt(Box::new(sel))),
    })?;
    text.strip_prefix("SELECT 1 FROM ")
        .map(str::to_string)
        .ok_or_else(|| Error::Parse("a FROM item".into()))
}

fn parse_one(sql: &str) -> Result<pg_query::protobuf::Node> {
    let parsed = pg_query::parse(sql).map_err(|e| Error::Parse(e.to_string()))?;
    parsed
        .protobuf
        .stmts
        .first()
        .and_then(|s| s.stmt.as_deref().cloned())
        .ok_or_else(|| Error::Parse("empty rule action".into()))
}

fn parse_expr(sql: &str) -> Result<pg_query::protobuf::Node> {
    let n = parse_one(&format!("SELECT {sql}"))?;
    match n.node {
        Some(N::SelectStmt(s)) => match s.target_list.into_iter().next().and_then(|t| t.node) {
            Some(N::ResTarget(rt)) => rt
                .val
                .map(|v| *v)
                .ok_or_else(|| Error::Parse("qual".into())),
            _ => Err(Error::Parse("qual".into())),
        },
        _ => Err(Error::Parse("qual".into())),
    }
}

fn and(
    a: Option<pg_query::protobuf::Node>,
    b: pg_query::protobuf::Node,
) -> pg_query::protobuf::Node {
    match a {
        None => b,
        Some(a) => pg_query::protobuf::Node {
            node: Some(N::BoolExpr(Box::new(pg_query::protobuf::BoolExpr {
                boolop: pg_query::protobuf::BoolExprType::AndExpr as i32,
                args: vec![a, b],
                ..Default::default()
            }))),
        },
    }
}

/// Replace each `DEFAULT` in a VALUES list with the column's default
/// expression (NULL for a column that has none), as PostgreSQL's rewriter
/// does before a rule sees the statement. `cols` names the VALUES columns in
/// order.
fn fill_values_defaults(
    s: &mut pg_query::protobuf::SelectStmt,
    cols: &[String],
    def: &TableDef,
) -> Result<()> {
    for row in &mut s.values_lists {
        let Some(N::List(l)) = row.node.as_mut() else {
            continue;
        };
        for (i, item) in l.items.iter_mut().enumerate() {
            if matches!(item.node.as_ref(), Some(N::SetToDefault(_))) {
                let sql = cols
                    .get(i)
                    .and_then(|c| def.column(c))
                    .map_or_else(|| "NULL".to_string(), default_sql);
                *item = parse_expr(&sql)?;
            }
        }
    }
    Ok(())
}

fn from_item(sql: &str) -> Result<pg_query::protobuf::Node> {
    let n = parse_one(&format!("SELECT 1 FROM {sql}"))?;
    match n.node {
        Some(N::SelectStmt(s)) => s
            .from_clause
            .into_iter()
            .next()
            .ok_or_else(|| Error::Parse("rule source".into())),
        _ => Err(Error::Parse("rule source".into())),
    }
}

/// One rule action, joined to the source: `None` for a shape this cannot
/// rewrite.
fn rewrite_action(
    action: &str,
    rule_qual: Option<&str>,
    src: &Source,
    join_source: bool,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Option<(String, String)>> {
    let mut node = parse_one(action)?;
    let qual = rule_qual.map(parse_expr).transpose()?;
    let target;
    match node.node.as_mut() {
        Some(N::InsertStmt(ins)) => {
            target = ins
                .relation
                .as_ref()
                .map(|r| r.relname.clone())
                .unwrap_or_default();
            if ins.returning_list.len() + usize::from(ins.with_clause.is_some()) > 0 {
                return Ok(None);
            }
            // The action's own `VALUES (..., DEFAULT)`: its target's default.
            if let Some(def) = lookup(&target) {
                let listed: Vec<String> = ins
                    .cols
                    .iter()
                    .filter_map(|c| match c.node.as_ref() {
                        Some(N::ResTarget(rt)) => Some(rt.name.clone()),
                        _ => None,
                    })
                    .collect();
                let cols: Vec<String> = if listed.is_empty() {
                    def.columns.iter().map(|c| c.name.clone()).collect()
                } else {
                    listed
                };
                if let Some(N::SelectStmt(s)) =
                    ins.select_stmt.as_deref_mut().and_then(|n| n.node.as_mut())
                {
                    fill_values_defaults(s, &cols, &def)?;
                }
            }
            if join_source {
                let Some(sel) = ins.select_stmt.as_deref_mut() else {
                    return Ok(None);
                };
                let Some(N::SelectStmt(s)) = sel.node.as_mut() else {
                    return Ok(None);
                };
                if s.op != pg_query::protobuf::SetOperation::SetopNone as i32 {
                    return Ok(None);
                }
                if s.values_lists.len() > 1 {
                    // Several rows: one `SELECT row FROM source [WHERE qual]`
                    // per row, joined by UNION ALL -- each row once per
                    // source row, as the rewritten VALUES yields them.
                    let from = &src.from;
                    let cond = qual
                        .as_ref()
                        .map(crate::deparse_expr)
                        .transpose()?
                        .map(|q| format!(" WHERE {q}"))
                        .unwrap_or_default();
                    let mut members = Vec::new();
                    for row in &s.values_lists {
                        let Some(N::List(l)) = row.node.as_ref() else {
                            return Ok(None);
                        };
                        let items = l
                            .items
                            .iter()
                            .map(crate::deparse_expr)
                            .collect::<Result<Vec<_>>>()?;
                        members.push(format!("SELECT {} FROM {from}{cond}", items.join(", ")));
                    }
                    *sel = parse_one(&members.join(" UNION ALL "))?;
                    let text = deparse(&node)?;
                    let text = substitute(&text, &|which, col| src.pseudo(which, col))?;
                    return Ok(Some((text, target)));
                } else if !s.values_lists.is_empty() {
                    // `VALUES (row)` becomes `SELECT row FROM source`.
                    let [row] = s.values_lists.as_slice() else {
                        return Ok(None);
                    };
                    let Some(N::List(l)) = row.node.as_ref() else {
                        return Ok(None);
                    };
                    let targets = l
                        .items
                        .iter()
                        .map(|v| pg_query::protobuf::Node {
                            node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                                val: Some(Box::new(v.clone())),
                                ..Default::default()
                            }))),
                        })
                        .collect();
                    s.values_lists.clear();
                    s.target_list = targets;
                    s.from_clause.push(from_item(&src.from)?);
                } else {
                    s.from_clause.push(from_item(&src.from)?);
                }
                if let Some(qn) = qual {
                    s.where_clause = Some(Box::new(and(s.where_clause.take().map(|b| *b), qn)));
                }
            }
        }
        Some(N::UpdateStmt(u)) => {
            target = u
                .relation
                .as_ref()
                .map(|r| r.relname.clone())
                .unwrap_or_default();
            if !u.returning_list.is_empty() || u.with_clause.is_some() {
                return Ok(None);
            }
            if join_source {
                u.from_clause.push(from_item(&src.from)?);
                if let Some(qn) = qual {
                    u.where_clause = Some(Box::new(and(u.where_clause.take().map(|b| *b), qn)));
                }
            }
        }
        Some(N::DeleteStmt(d)) => {
            target = d
                .relation
                .as_ref()
                .map(|r| r.relname.clone())
                .unwrap_or_default();
            if !d.returning_list.is_empty() || d.with_clause.is_some() {
                return Ok(None);
            }
            if join_source {
                d.using_clause.push(from_item(&src.from)?);
                if let Some(qn) = qual {
                    d.where_clause = Some(Box::new(and(d.where_clause.take().map(|b| *b), qn)));
                }
            }
        }
        Some(N::SelectStmt(_)) | Some(N::NotifyStmt(_)) => {
            if join_source {
                return Ok(None);
            }
            target = String::new();
        }
        _ => return Ok(None),
    }
    let text = deparse(&node)?;
    let text = substitute(&text, &|which, col| src.pseudo(which, col))?;
    Ok(Some((text, target)))
}

/// The original statement with a conditional INSTEAD rule's qual negated
/// into it: `(qual) IS NOT TRUE`, with NEW / OLD as the original sees them.
fn negate_into_original(
    original: &pg_query::protobuf::Node,
    quals: &[&str],
    src: &Source,
    def: &TableDef,
) -> Result<Option<String>> {
    if quals.is_empty() {
        return Ok(Some(deparse(original)?));
    }
    let mut node = original.clone();
    let r = q(&src.target_ref);
    let negated: Vec<String> = quals
        .iter()
        .map(|qual| format!("(({qual})) IS NOT TRUE"))
        .collect();
    match node.node.as_mut() {
        Some(N::UpdateStmt(u)) => {
            let text = negated.join(" AND ");
            let text = substitute(&text, &|which, col| {
                if which == "new" {
                    if let Some((_, e)) = src.set.iter().find(|(n, _)| n == col) {
                        return Some(format!("({e})"));
                    }
                }
                Some(format!("{r}.{}", q(col)))
            })?;
            u.where_clause = Some(Box::new(and(
                u.where_clause.take().map(|b| *b),
                parse_expr(&text)?,
            )));
            Ok(Some(deparse(&node)?))
        }
        Some(N::DeleteStmt(d)) => {
            let text = negated.join(" AND ");
            let text = substitute(&text, &|_, col| Some(format!("{r}.{}", q(col))))?;
            d.where_clause = Some(Box::new(and(
                d.where_clause.take().map(|b| *b),
                parse_expr(&text)?,
            )));
            Ok(Some(deparse(&node)?))
        }
        Some(N::InsertStmt(ins)) => {
            if !ins.returning_list.is_empty() || ins.on_conflict_clause.is_some() {
                return Ok(None);
            }
            // The INSERT becomes an INSERT ... SELECT from the source, the
            // negated quals over NEW.
            let text = negated.join(" AND ");
            let text = substitute(&text, &|which, col| src.pseudo(which, col))?;
            let rel = ins
                .relation
                .as_ref()
                .ok_or_else(|| Error::Parse("INSERT".into()))?;
            if src.provided.is_empty() {
                // DEFAULT VALUES: the row of defaults, kept only when the
                // quals allow it. Identity and generated columns fill
                // themselves.
                let plain: Vec<&secantus_pgcatalog::Column> = def
                    .columns
                    .iter()
                    .filter(|c| c.identity.is_none() && c.extra.get_str("generated").is_err())
                    .collect();
                let Some(first) = plain.first() else {
                    return Ok(None);
                };
                return Ok(Some(format!(
                    "INSERT INTO {} ({}) SELECT {} WHERE {text}",
                    q(&rel.relname),
                    q(&first.name),
                    default_sql(first)
                )));
            }
            let cols: Vec<String> = src.provided.iter().map(|c| q(c)).collect();
            let vals: Vec<String> = src
                .provided
                .iter()
                .map(|c| format!("__rule_src.{}", q(&format!("n_{c}"))))
                .collect();
            Ok(Some(format!(
                "INSERT INTO {} ({}) SELECT {} FROM {} WHERE {text}",
                q(&rel.relname),
                cols.join(", "),
                vals.join(", "),
                src.from
            )))
        }
        _ => Ok(None),
    }
}

/// The rewrite for `node`, when its target has rules for its event.
pub(crate) fn plan(
    node: &pg_query::protobuf::Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[bson::Bson],
) -> Result<Option<RulePlan>> {
    let (table, kind) = match node.node.as_ref() {
        Some(N::InsertStmt(i)) => (i.relation.as_ref().map(|r| r.relname.clone()), "INSERT"),
        Some(N::UpdateStmt(u)) => (u.relation.as_ref().map(|r| r.relname.clone()), "UPDATE"),
        Some(N::DeleteStmt(d)) => (d.relation.as_ref().map(|r| r.relname.clone()), "DELETE"),
        _ => return Ok(None),
    };
    let Some(table) = table else {
        return Ok(None);
    };
    if SUPPRESSED.with(|s| s.borrow().contains(&table)) {
        return Ok(None);
    }
    let mut rules: Vec<RuleDef> = RULES.with(|r| {
        r.borrow()
            .iter()
            .filter(|r| r.table == table && r.event == kind)
            .cloned()
            .collect()
    });
    if rules.is_empty() {
        return Ok(None);
    }
    rules.sort_by(|a, b| a.name.cmp(&b.name));
    let unsupported =
        || Error::FeatureNotSupported(format!("this {kind} on a relation with rules"));
    let def = match lookup(&table) {
        Some(d) => d,
        None => view_output_def(&table, lookup).ok_or_else(unsupported)?,
    };
    let returning = match node.node.as_ref() {
        Some(N::InsertStmt(i)) => !i.returning_list.is_empty() || i.with_clause.is_some(),
        Some(N::UpdateStmt(u)) => !u.returning_list.is_empty() || u.with_clause.is_some(),
        Some(N::DeleteStmt(d)) => !d.returning_list.is_empty() || d.with_clause.is_some(),
        _ => false,
    };
    let src = source_of(node.node.as_ref().expect("checked"), &def)?.ok_or_else(unsupported)?;
    let unconditional_instead = rules.iter().any(|r| r.instead && r.condition.is_none());
    if returning && rules.iter().any(|r| r.instead) {
        return Err(Error::FeatureNotSupported(format!(
            "cannot perform {kind} RETURNING on relation \"{table}\""
        )));
    }
    if recurses(&table, kind) {
        return Err(Error::Sqlstate(
            "42P17",
            format!("infinite recursion detected in rules for relation \"{table}\""),
        ));
    }
    let mut actions: Vec<(String, bool)> = Vec::new();
    let mut targets = vec![table.clone()];
    let mut last_same_kind_instead: Option<usize> = None;
    for r in &rules {
        for a in &r.actions {
            let join =
                kind == "INSERT" || src.has_where || r.condition.is_some() || mentions_pseudo(a);
            let (sql, target) = rewrite_action(a, r.condition.as_deref(), &src, join, lookup)?
                .ok_or_else(unsupported)?;
            let action_kind = sql
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            if r.instead && action_kind == kind {
                last_same_kind_instead = Some(actions.len());
            }
            if !target.is_empty() && !targets.contains(&target) {
                targets.push(target);
            }
            actions.push((sql, false));
        }
    }
    let mut steps: Vec<(String, bool)> = Vec::new();
    let mut tag_step = None;
    let original = if unconditional_instead {
        None
    } else {
        let quals: Vec<&str> = rules
            .iter()
            .filter(|r| r.instead)
            .filter_map(|r| r.condition.as_deref())
            .collect();
        Some(negate_into_original(node, &quals, &src, &def)?.ok_or_else(unsupported)?)
    };
    if kind == "INSERT" {
        if let Some(o) = original.clone() {
            tag_step = Some(0);
            steps.push((o, true));
        }
        let base = steps.len();
        if tag_step.is_none() {
            tag_step = last_same_kind_instead.map(|i| base + i);
        }
        steps.extend(actions);
    } else {
        let n = actions.len();
        if original.is_none() {
            tag_step = last_same_kind_instead;
        }
        steps.extend(actions);
        if let Some(o) = original {
            tag_step = Some(n);
            steps.push((o, true));
        }
    }
    Ok(Some(RulePlan {
        table,
        kind: kind.to_string(),
        steps,
        tag_step,
        targets,
        params: params.to_vec(),
        param_types: crate::PLAN_PARAM_TYPES.with(|t| t.borrow().clone()),
    }))
}

/// A view's output columns, as its SELECT plans.
fn view_output_def(name: &str, lookup: &dyn Fn(&str) -> Option<TableDef>) -> Option<TableDef> {
    let st =
        crate::plan_with_params(&format!("SELECT * FROM {} LIMIT 0", q(name)), lookup, &[]).ok()?;
    let mut def = crate::sub_plan_def(&st, lookup).ok()?;
    def.name = name.to_string();
    Some(def)
}

/// The `(target, command)` a rule action writes.
fn action_target(action: &str) -> Option<(String, &'static str)> {
    let node = parse_one(action).ok()?;
    match node.node? {
        N::InsertStmt(i) => Some((i.relation?.relname, "INSERT")),
        N::UpdateStmt(u) => Some((u.relation?.relname, "UPDATE")),
        N::DeleteStmt(d) => Some((d.relation?.relname, "DELETE")),
        _ => None,
    }
}

/// Would rewriting `(table, kind)` reach `(table, kind)` again through
/// rule actions? PostgreSQL refuses that while rewriting (42P17), whatever
/// the rules' conditions.
fn recurses(table: &str, kind: &str) -> bool {
    RULES.with(|r| {
        let rules = r.borrow();
        let mut seen: Vec<(String, String)> = Vec::new();
        let mut stack = vec![(table.to_string(), kind.to_string())];
        while let Some((t, k)) = stack.pop() {
            for rule in rules.iter().filter(|x| x.table == t && x.event == k) {
                for a in &rule.actions {
                    let Some((nt, nk)) = action_target(a) else {
                        continue;
                    };
                    if nt == table && nk == kind {
                        return true;
                    }
                    let edge = (nt, nk.to_string());
                    if !seen.contains(&edge) {
                        seen.push(edge.clone());
                        stack.push(edge);
                    }
                }
            }
        }
        false
    })
}
