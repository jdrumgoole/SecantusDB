//! `MERGE` (PostgreSQL 15): planned as two READS over the statement's
//! starting snapshot, whose rows the executor then turns into writes.
//!
//! * The MATCHED query joins the target to the source on the join condition
//!   and, per joined pair, names the first `WHEN MATCHED` clause whose
//!   condition holds (a `CASE`), the target row's identity, and that
//!   clause's SET values -- all evaluated over the pair, as PostgreSQL does.
//! * The NOT MATCHED query reads the source rows no target row joins
//!   (`NOT EXISTS`), naming the first `WHEN NOT MATCHED` clause that holds
//!   and its INSERT values.
//!
//! Both are read before anything is written, so no action sees another's
//! effect -- which is PostgreSQL's snapshot. The executor then runs each
//! action as an ordinary UPDATE / DELETE / INSERT by row identity, so
//! constraints, defaults, triggers and row security apply as they would to
//! the statement written out by hand.

use super::*;

/// One `WHEN` clause's action.
#[derive(Debug, Clone, PartialEq)]
pub enum MergeAction {
    /// The SET columns, whose values the MATCHED query computes in order.
    Update(Vec<String>),
    Delete,
    /// The INSERT columns (empty: every column, in order) and, per value,
    /// whether it is `DEFAULT` rather than a value the query computes.
    Insert {
        columns: Vec<String>,
        defaults: Vec<bool>,
    },
    Nothing,
}

/// A planned MERGE.
#[derive(Debug, Clone, PartialEq)]
pub struct Merge {
    pub target: String,
    /// The target's identity columns: its primary key, or every column.
    pub key: Vec<String>,
    /// Whether `key` is the primary key (else a row is found by every
    /// column, NULLs matching NULLs).
    pub key_is_pk: bool,
    /// The MATCHED query: `key...`, the chosen clause (-1: none), then each
    /// UPDATE clause's SET values in clause order.
    pub matched_sql: String,
    /// The NOT MATCHED query: the chosen clause (-1: none), then each INSERT
    /// clause's non-DEFAULT values in clause order.
    pub not_matched_sql: String,
    /// `(matched, action)` per WHEN clause, in order.
    pub clauses: Vec<(bool, MergeAction)>,
    /// The statement's parameters, which both queries read.
    pub params: Vec<Bson>,
}

fn res_target(val: pg_query::protobuf::Node, name: &str) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
            name: name.to_string(),
            val: Some(Box::new(val)),
            location: -1,
            ..Default::default()
        }))),
    }
}

fn int_const(i: i64) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::AConst(pg_query::protobuf::AConst {
            isnull: false,
            location: -1,
            val: Some(pg_query::protobuf::a_const::Val::Ival(
                pg_query::protobuf::Integer { ival: i as i32 },
            )),
        })),
    }
}

fn true_const() -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::AConst(pg_query::protobuf::AConst {
            isnull: false,
            location: -1,
            val: Some(pg_query::protobuf::a_const::Val::Boolval(
                pg_query::protobuf::Boolean { boolval: true },
            )),
        })),
    }
}

fn qualified(alias: &str, column: &str) -> pg_query::protobuf::Node {
    let s = |v: &str| pg_query::protobuf::Node {
        node: Some(N::String(pg_query::protobuf::String { sval: v.to_string() })),
    };
    pg_query::protobuf::Node {
        node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
            fields: vec![s(alias), s(column)],
            location: -1,
        })),
    }
}

/// `CASE WHEN c1 THEN i1 WHEN c2 THEN i2 ... ELSE -1 END`.
fn choose(arms: &[(usize, Option<pg_query::protobuf::Node>)]) -> pg_query::protobuf::Node {
    if arms.is_empty() {
        return int_const(-1);
    }
    pg_query::protobuf::Node {
        node: Some(N::CaseExpr(Box::new(pg_query::protobuf::CaseExpr {
            args: arms
                .iter()
                .map(|(i, cond)| pg_query::protobuf::Node {
                    node: Some(N::CaseWhen(Box::new(pg_query::protobuf::CaseWhen {
                        expr: Some(Box::new(cond.clone().unwrap_or_else(true_const))),
                        result: Some(Box::new(int_const(*i as i64))),
                        location: -1,
                        ..Default::default()
                    }))),
                })
                .collect(),
            defresult: Some(Box::new(int_const(-1))),
            location: -1,
            ..Default::default()
        }))),
    }
}

fn select_sql(s: pg_query::protobuf::SelectStmt) -> Result<String> {
    pg_query::protobuf::Node {
        node: Some(N::SelectStmt(Box::new(s))),
    }
    .deparse()
    .map_err(|e| Error::Parse(e.to_string()))
}

pub(crate) fn plan(
    m: &pg_query::protobuf::MergeStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<Statement> {
    use pg_query::protobuf::{CmdType, MergeMatchKind};
    if !m.returning_list.is_empty() {
        return Err(Error::Parse("syntax error at or near \"RETURNING\"".into()));
    }
    if m.with_clause.is_some() {
        return Err(Error::Unsupported("WITH on MERGE".into()));
    }
    let rel = m
        .relation
        .as_ref()
        .ok_or_else(|| Error::Parse("MERGE with no target".into()))?;
    let target = rel.relname.clone();
    let def = lookup(&target)
        .ok_or_else(|| Error::UndefinedTable(target.clone()))?;
    let alias = rel
        .alias
        .as_ref()
        .map(|a| a.aliasname.clone())
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| target.clone());
    let source = m
        .source_relation
        .as_deref()
        .cloned()
        .ok_or_else(|| Error::Parse("MERGE with no source".into()))?;
    let on = m
        .join_condition
        .as_deref()
        .cloned()
        .ok_or_else(|| Error::Parse("MERGE with no join condition".into()))?;
    let pk: Vec<String> = def.columns.iter().filter(|c| c.pk).map(|c| c.name.clone()).collect();
    let key_is_pk = !pk.is_empty();
    let key = if key_is_pk {
        pk
    } else {
        def.columns.iter().map(|c| c.name.clone()).collect()
    };

    let mut clauses = Vec::new();
    let mut matched_arms = Vec::new();
    let mut not_matched_arms = Vec::new();
    let mut set_values = Vec::new();
    let mut insert_values = Vec::new();
    for (i, w) in m.merge_when_clauses.iter().enumerate() {
        let Some(N::MergeWhenClause(w)) = w.node.as_ref() else {
            return Err(Error::Parse("a MERGE WHEN clause".into()));
        };
        let matched = match MergeMatchKind::try_from(w.match_kind) {
            Ok(MergeMatchKind::MergeWhenMatched) => true,
            Ok(MergeMatchKind::MergeWhenNotMatchedByTarget) => false,
            // PostgreSQL 15 has no `NOT MATCHED BY SOURCE` (17 added it).
            _ => return Err(Error::Parse("syntax error at or near \"BY\"".into())),
        };
        let cond = w.condition.as_deref().cloned();
        if matched {
            matched_arms.push((i, cond));
        } else {
            not_matched_arms.push((i, cond));
        }
        let action = match CmdType::try_from(w.command_type) {
            Ok(CmdType::CmdUpdate) => {
                let mut columns = Vec::new();
                for t in &w.target_list {
                    let Some(N::ResTarget(rt)) = t.node.as_ref() else { continue };
                    if !rt.indirection.is_empty() {
                        return Err(Error::Unsupported("a subscripted SET in MERGE".into()));
                    }
                    if def.column(&rt.name).is_none() {
                        return Err(Error::UndefinedColumn(format!(
                            "column \"{}\" of relation \"{target}\" does not exist",
                            rt.name
                        )));
                    }
                    let val = rt
                        .val
                        .as_deref()
                        .cloned()
                        .ok_or_else(|| Error::Parse("SET with no value".into()))?;
                    if matches!(val.node.as_ref(), Some(N::SetToDefault(_))) {
                        return Err(Error::Unsupported("SET ... = DEFAULT in MERGE".into()));
                    }
                    set_values.push(val);
                    columns.push(rt.name.clone());
                }
                MergeAction::Update(columns)
            }
            Ok(CmdType::CmdDelete) => MergeAction::Delete,
            Ok(CmdType::CmdInsert) => {
                let columns: Vec<String> = w
                    .target_list
                    .iter()
                    .filter_map(|t| match t.node.as_ref() {
                        Some(N::ResTarget(rt)) => Some(rt.name.clone()),
                        _ => None,
                    })
                    .collect();
                let mut defaults = Vec::new();
                for v in &w.values {
                    let is_default = matches!(v.node.as_ref(), Some(N::SetToDefault(_)));
                    defaults.push(is_default);
                    if !is_default {
                        insert_values.push(v.clone());
                    }
                }
                let width = if columns.is_empty() { def.columns.len() } else { columns.len() };
                if defaults.len() > width {
                    return Err(Error::Sqlstate(
                        "42601",
                        "INSERT has more expressions than target columns".into(),
                    ));
                }
                MergeAction::Insert { columns, defaults }
            }
            Ok(CmdType::CmdNothing) => MergeAction::Nothing,
            _ => return Err(Error::Unsupported("this MERGE action".into())),
        };
        clauses.push((matched, action));
    }

    let target_item = pg_query::protobuf::Node {
        node: Some(N::RangeVar(pg_query::protobuf::RangeVar {
            relname: target.clone(),
            inh: true,
            relpersistence: "p".into(),
            alias: Some(pg_query::protobuf::Alias {
                aliasname: alias.clone(),
                colnames: Vec::new(),
            }),
            location: -1,
            ..Default::default()
        })),
    };
    // MATCHED: the identity, the chosen clause, every SET value.
    let mut targets: Vec<pg_query::protobuf::Node> = key
        .iter()
        .enumerate()
        .map(|(i, k)| res_target(qualified(&alias, k), &format!("__k{i}")))
        .collect();
    targets.push(res_target(choose(&matched_arms), "__clause"));
    for (i, v) in set_values.into_iter().enumerate() {
        targets.push(res_target(v, &format!("__u{i}")));
    }
    let join = pg_query::protobuf::Node {
        node: Some(N::JoinExpr(Box::new(pg_query::protobuf::JoinExpr {
            jointype: pg_query::protobuf::JoinType::JoinInner as i32,
            larg: Some(Box::new(target_item.clone())),
            rarg: Some(Box::new(source.clone())),
            quals: Some(Box::new(on.clone())),
            ..Default::default()
        }))),
    };
    let matched_sql = select_sql(pg_query::protobuf::SelectStmt {
        target_list: targets,
        from_clause: vec![join],
        op: pg_query::protobuf::SetOperation::SetopNone as i32,
        limit_option: pg_query::protobuf::LimitOption::Default as i32,
        ..Default::default()
    })?;
    // NOT MATCHED: the source rows no target row joins.
    let exists = pg_query::protobuf::SelectStmt {
        target_list: vec![res_target(int_const(1), "")],
        from_clause: vec![target_item],
        where_clause: Some(Box::new(on)),
        op: pg_query::protobuf::SetOperation::SetopNone as i32,
        limit_option: pg_query::protobuf::LimitOption::Default as i32,
        ..Default::default()
    };
    let not_exists = pg_query::protobuf::Node {
        node: Some(N::BoolExpr(Box::new(pg_query::protobuf::BoolExpr {
            boolop: BoolExprType::NotExpr as i32,
            args: vec![pg_query::protobuf::Node {
                node: Some(N::SubLink(Box::new(pg_query::protobuf::SubLink {
                    sub_link_type: SubLinkType::ExistsSublink as i32,
                    subselect: Some(Box::new(pg_query::protobuf::Node {
                        node: Some(N::SelectStmt(Box::new(exists))),
                    })),
                    location: -1,
                    ..Default::default()
                }))),
            }],
            location: -1,
            ..Default::default()
        }))),
    };
    let mut targets = vec![res_target(choose(&not_matched_arms), "__clause")];
    for (i, v) in insert_values.into_iter().enumerate() {
        targets.push(res_target(v, &format!("__i{i}")));
    }
    let not_matched_sql = select_sql(pg_query::protobuf::SelectStmt {
        target_list: targets,
        from_clause: vec![source],
        where_clause: Some(Box::new(not_exists)),
        op: pg_query::protobuf::SetOperation::SetopNone as i32,
        limit_option: pg_query::protobuf::LimitOption::Default as i32,
        ..Default::default()
    })?;
    Ok(Statement::Merge(Merge {
        target,
        key,
        key_is_pk,
        matched_sql,
        not_matched_sql,
        clauses,
        params: params.to_vec(),
    }))
}
