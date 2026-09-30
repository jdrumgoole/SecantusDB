//! Statements that only RECORD something, with no effect on what a query
//! answers here: extended statistics (a planner hint), tablespaces (where
//! files live), publications (what logical replication would send) and
//! security labels (which need a label provider PostgreSQL does not ship).
//! They are planned into `CatalogOp`s the executor stores and the catalogs
//! (`pg_statistic_ext`, `pg_tablespace`, `pg_publication*`) report, with
//! PostgreSQL's validation and errors.

use super::*;

/// One publication member: a table, with PostgreSQL 15's column list and
/// row filter as written.
#[derive(Debug, Clone, PartialEq)]
pub struct PubTable {
    pub table: String,
    pub columns: Vec<String>,
    pub row_filter: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PubAction {
    Add,
    Drop,
    Set,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CatalogOp {
    CreateStatistics {
        name: String,
        table: String,
        columns: Vec<String>,
        exprs: Vec<String>,
        /// `d` / `f` / `m` (and `e` for expressions), PostgreSQL's order.
        kinds: Vec<String>,
        if_not_exists: bool,
    },
    AlterStatistics {
        name: String,
        rename: Option<String>,
        missing_ok: bool,
    },
    DropStatistics {
        names: Vec<String>,
        if_exists: bool,
    },
    CreateTablespace {
        name: String,
        location: String,
        owner: Option<String>,
    },
    DropTablespace {
        name: String,
        if_exists: bool,
    },
    CreatePublication {
        name: String,
        all_tables: bool,
        tables: Vec<PubTable>,
        publish: Option<String>,
        via_root: Option<bool>,
    },
    AlterPublication {
        name: String,
        action: PubAction,
        tables: Vec<PubTable>,
        publish: Option<String>,
        via_root: Option<bool>,
    },
    DropPublication {
        names: Vec<String>,
        if_exists: bool,
    },
}

impl CatalogOp {
    pub fn tag(&self) -> &'static str {
        match self {
            CatalogOp::CreateStatistics { .. } => "CREATE STATISTICS",
            CatalogOp::AlterStatistics { .. } => "ALTER STATISTICS",
            CatalogOp::DropStatistics { .. } => "DROP STATISTICS",
            CatalogOp::CreateTablespace { .. } => "CREATE TABLESPACE",
            CatalogOp::DropTablespace { .. } => "DROP TABLESPACE",
            CatalogOp::CreatePublication { .. } => "CREATE PUBLICATION",
            CatalogOp::AlterPublication { .. } => "ALTER PUBLICATION",
            CatalogOp::DropPublication { .. } => "DROP PUBLICATION",
        }
    }
}

thread_local! {
    static TABLESPACES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Install the database's user tablespaces for the statements that follow.
pub fn set_tablespaces(names: Vec<String>) {
    TABLESPACES.with(|t| *t.borrow_mut() = names);
}

/// 42704 for a `TABLESPACE` clause naming none that exists.
pub(crate) fn check_tablespace(name: &str) -> Result<()> {
    if name.is_empty()
        || matches!(name, "pg_default" | "pg_global")
        || TABLESPACES.with(|t| t.borrow().iter().any(|n| n == name))
    {
        return Ok(());
    }
    Err(Error::UndefinedObject(format!(
        "tablespace \"{name}\" does not exist"
    )))
}

fn last_name(names: &[pg_query::protobuf::Node]) -> String {
    names
        .iter()
        .rev()
        .find_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

pub(crate) fn plan_create_statistics(
    s: &pg_query::protobuf::CreateStatsStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Statement> {
    let table = match s.relations.first().and_then(|r| r.node.as_ref()) {
        Some(N::RangeVar(r)) if s.relations.len() == 1 => r.relname.clone(),
        _ => {
            return Err(Error::FeatureNotSupported(
                "only a single relation is allowed in CREATE STATISTICS".into(),
            ))
        }
    };
    let def = lookup(&table).ok_or_else(|| Error::UndefinedTable(table.clone()))?;
    let mut columns = Vec::new();
    let mut exprs = Vec::new();
    for e in &s.exprs {
        let Some(N::StatsElem(el)) = e.node.as_ref() else {
            continue;
        };
        match el.expr.as_deref() {
            None => {
                if def.column(&el.name).is_none() {
                    return Err(Error::UndefinedColumn(el.name.clone()));
                }
                columns.push(el.name.clone());
            }
            // `(a)` in parentheses is still a plain column reference.
            Some(expr) => match expr.node.as_ref() {
                Some(N::ColumnRef(c)) => {
                    let name = column_ref_name(c).unwrap_or_default();
                    if def.column(&name).is_none() {
                        return Err(Error::UndefinedColumn(name));
                    }
                    columns.push(name);
                }
                _ => {
                    plan_check_expression(&deparse_expr(expr)?, &def)?;
                    exprs.push(deparse_expr(expr)?);
                }
            },
        }
    }
    if columns.len() + exprs.len() < 2 && exprs.is_empty() {
        return Err(Error::Sqlstate(
            "42P17",
            "extended statistics require at least 2 columns".into(),
        ));
    }
    let mut kinds = Vec::new();
    for t in &s.stat_types {
        let Some(N::String(k)) = t.node.as_ref() else {
            continue;
        };
        kinds.push(match k.sval.as_str() {
            "ndistinct" => "d",
            "dependencies" => "f",
            "mcv" => "m",
            other => {
                return Err(Error::Parse(format!(
                    "unrecognized statistics kind \"{other}\""
                )))
            }
        });
    }
    // A single expression gathers only expression statistics.
    let single_expr = columns.is_empty() && exprs.len() == 1;
    if kinds.is_empty() && !single_expr {
        kinds = vec!["d", "f", "m"];
    }
    kinds.sort_by_key(|k| ["d", "f", "m"].iter().position(|x| x == k));
    let mut kinds: Vec<String> = kinds.into_iter().map(str::to_string).collect();
    if !exprs.is_empty() {
        kinds.push("e".into());
    }
    let name = if s.defnames.is_empty() {
        let mut parts: Vec<String> = columns.clone();
        if !exprs.is_empty() {
            parts.push("expr".into());
        }
        format!("{table}_{}_stat", parts.join("_"))
    } else {
        last_name(&s.defnames)
    };
    Ok(Statement::Catalog(CatalogOp::CreateStatistics {
        name,
        table,
        columns,
        exprs,
        kinds,
        if_not_exists: s.if_not_exists,
    }))
}

pub(crate) fn plan_alter_statistics(s: &pg_query::protobuf::AlterStatsStmt) -> Statement {
    Statement::Catalog(CatalogOp::AlterStatistics {
        name: last_name(&s.defnames),
        rename: None,
        missing_ok: s.missing_ok,
    })
}

fn publish_options(options: &[pg_query::protobuf::Node]) -> Result<(Option<String>, Option<bool>)> {
    let mut publish = None;
    let mut via_root = None;
    for o in options {
        let Some(N::DefElem(e)) = o.node.as_ref() else {
            continue;
        };
        let value = match e.arg.as_ref().and_then(|a| a.node.as_ref()) {
            Some(N::String(s)) => s.sval.clone(),
            Some(N::Boolean(b)) => b.boolval.to_string(),
            Some(N::Integer(i)) => i.ival.to_string(),
            None => "true".into(),
            _ => String::new(),
        };
        match e.defname.as_str() {
            "publish" => {
                for part in value.split(',') {
                    let p = part.trim();
                    if !matches!(p, "insert" | "update" | "delete" | "truncate") {
                        return Err(Error::Sqlstate(
                            "42601",
                            format!(
                                "unrecognized value for publication option \"publish\": \"{p}\""
                            ),
                        ));
                    }
                }
                publish = Some(value);
            }
            "publish_via_partition_root" => {
                via_root = Some(matches!(
                    value.to_ascii_lowercase().as_str(),
                    "true" | "on" | "1" | "yes"
                ));
            }
            other => {
                return Err(Error::Sqlstate(
                    "42601",
                    format!("unrecognized publication parameter: \"{other}\""),
                ))
            }
        }
    }
    Ok((publish, via_root))
}

fn publication_tables(
    objects: &[pg_query::protobuf::Node],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Vec<PubTable>> {
    let mut out = Vec::new();
    for o in objects {
        let Some(N::PublicationObjSpec(spec)) = o.node.as_ref() else {
            continue;
        };
        let Some(t) = spec.pubtable.as_deref() else {
            return Err(Error::FeatureNotSupported(
                "a publication of the tables in a schema".into(),
            ));
        };
        let Some(rel) = t.relation.as_ref() else {
            continue;
        };
        let def = lookup(&rel.relname).ok_or_else(|| Error::UndefinedTable(rel.relname.clone()))?;
        let mut columns = Vec::new();
        for c in &t.columns {
            if let Some(N::String(s)) = c.node.as_ref() {
                if def.column(&s.sval).is_none() {
                    return Err(Error::UndefinedColumn(format!(
                        "{}\" of relation \"{}",
                        s.sval, rel.relname
                    )));
                }
                columns.push(s.sval.clone());
            }
        }
        let row_filter = t.where_clause.as_deref().map(deparse_expr).transpose()?;
        if let Some(f) = &row_filter {
            plan_check_expression(f, &def)?;
        }
        out.push(PubTable {
            table: rel.relname.clone(),
            columns,
            row_filter,
        });
    }
    Ok(out)
}

pub(crate) fn plan_create_publication(
    p: &pg_query::protobuf::CreatePublicationStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Statement> {
    let (publish, via_root) = publish_options(&p.options)?;
    Ok(Statement::Catalog(CatalogOp::CreatePublication {
        name: p.pubname.clone(),
        all_tables: p.for_all_tables,
        tables: publication_tables(&p.pubobjects, lookup)?,
        publish,
        via_root,
    }))
}

pub(crate) fn plan_alter_publication(
    p: &pg_query::protobuf::AlterPublicationStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Statement> {
    use pg_query::protobuf::AlterPublicationAction as A;
    let (publish, via_root) = publish_options(&p.options)?;
    let action = match A::try_from(p.action) {
        Ok(A::ApDropObjects) => PubAction::Drop,
        Ok(A::ApSetObjects) => PubAction::Set,
        _ => PubAction::Add,
    };
    Ok(Statement::Catalog(CatalogOp::AlterPublication {
        name: p.pubname.clone(),
        action,
        tables: publication_tables(&p.pubobjects, lookup)?,
        publish,
        via_root,
    }))
}

/// `SECURITY LABEL`: PostgreSQL ships no label provider, so without one
/// loaded every form is refused -- which is this server's state too.
pub(crate) fn plan_security_label(s: &pg_query::protobuf::SecLabelStmt) -> Error {
    if s.provider.is_empty() {
        Error::Sqlstate(
            "22023",
            "no security label providers have been loaded".into(),
        )
    } else {
        Error::Sqlstate(
            "22023",
            format!("security label provider \"{}\" is not loaded", s.provider),
        )
    }
}
