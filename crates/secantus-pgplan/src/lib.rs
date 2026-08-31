//! PostgreSQL parse trees, lowered to the MQL that `secantus-core` evaluates.
//!
//! Parsing is [`pg_query`], which statically links libpg_query -- PostgreSQL's
//! own `gram.y`. That is deliberate: the Python server parses with sqlglot, a
//! generic multi-dialect parser, and carries roughly forty workarounds for its
//! mis-parses (see `tasks/rust-pgserver-plan.md` §4). Parsing exactly what
//! PostgreSQL parses deletes that class rather than re-deriving it.
//!
//! **There is no fallback into Python.** A construct this module cannot lower
//! is an [`Error::Unsupported`], which the server turns into PostgreSQL's
//! `0A000 feature_not_supported`. A wrong answer would be worse than an honest
//! refusal, and the two-server model has no third option.

use bson::{doc, Bson, Document};
use pg_query::protobuf::node::Node as N;
use pg_query::protobuf::{a_const, AExpr, BoolExprType};
use secantus_pgcatalog::{Column, TableDef};

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The statement did not parse. Carries libpg_query's own message, which
    /// is PostgreSQL's.
    Parse(String),
    /// Parsed, but this server cannot lower it yet -> 0A000.
    Unsupported(String),
    /// A column the table does not have -> 42703.
    UndefinedColumn(String),
    /// A table the catalog does not have -> 42P01.
    UndefinedTable(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Parse(m) => write!(f, "{m}"),
            Error::Unsupported(m) => write!(f, "{m} is not supported yet"),
            Error::UndefinedColumn(c) => write!(f, "column \"{c}\" does not exist"),
            Error::UndefinedTable(t) => write!(f, "relation \"{t}\" does not exist"),
        }
    }
}

impl Error {
    /// The SQLSTATE a client should see.
    pub fn sqlstate(&self) -> &'static str {
        match self {
            Error::Parse(_) => "42601",       // syntax_error
            Error::Unsupported(_) => "0A000", // feature_not_supported
            Error::UndefinedColumn(_) => "42703",
            Error::UndefinedTable(_) => "42P01",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// What the server should do with one statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    CreateTable(TableDef),
    Insert(Insert),
    Select(Select),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Insert {
    pub table: String,
    /// One document per row, already keyed by stored FIELD (PK as `_id`).
    pub rows: Vec<Document>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    pub table: String,
    /// Output columns in order, as (output name, stored field).
    pub columns: Vec<(String, String)>,
    pub filter: Document,
}

fn disc(n: &N) -> String {
    format!("{n:?}")
        .split('(')
        .next()
        .unwrap_or("?")
        .to_string()
}

fn parse_one(sql: &str) -> Result<N> {
    let parsed = pg_query::parse(sql).map_err(|e| Error::Parse(e.to_string()))?;
    let mut stmts = parsed.protobuf.stmts;
    if stmts.len() != 1 {
        return Err(Error::Unsupported("multi-statement input".into()));
    }
    stmts
        .remove(0)
        .stmt
        .and_then(|s| s.node)
        .ok_or_else(|| Error::Parse("empty statement".into()))
}

/// Lower one statement. `lookup` resolves a table name to its catalog entry;
/// `CREATE TABLE` does not consult it.
pub fn plan(sql: &str, lookup: &dyn Fn(&str) -> Option<TableDef>) -> Result<Statement> {
    match parse_one(sql)? {
        N::CreateStmt(c) => plan_create(&c),
        N::InsertStmt(i) => plan_insert(&i, lookup),
        N::SelectStmt(s) => plan_select(&s, lookup),
        other => Err(Error::Unsupported(disc(&other))),
    }
}

fn type_name(names: &[pg_query::protobuf::Node]) -> String {
    // libpg_query qualifies built-ins as pg_catalog.<name>; the catalog stores
    // the bare PostgreSQL name (`int4`, `text`), matching the Python server.
    names
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        })
        .next_back()
        .unwrap_or_default()
}

fn plan_create(c: &pg_query::protobuf::CreateStmt) -> Result<Statement> {
    let table = c
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| Error::Parse("CREATE TABLE without a relation".into()))?;

    let mut columns: Vec<Column> = Vec::new();
    for el in &c.table_elts {
        match el.node.as_ref() {
            Some(N::ColumnDef(cd)) => {
                let ty = cd
                    .type_name
                    .as_ref()
                    .map(|t| type_name(&t.names))
                    .unwrap_or_default();
                let pk = cd.constraints.iter().any(|c| {
                    matches!(c.node.as_ref(), Some(N::Constraint(k))
                        if k.contype == pg_query::protobuf::ConstrType::ConstrPrimary as i32)
                });
                columns.push(Column::new(&cd.colname, &ty, pk));
            }
            Some(other) => return Err(Error::Unsupported(disc(other))),
            None => {}
        }
    }
    if columns.iter().filter(|c| c.pk).count() > 1 {
        // The stored form maps the PK onto `_id`, so exactly one column can be
        // it. A composite PK needs a different mapping; refuse rather than
        // silently store only one of them.
        return Err(Error::Unsupported("a composite PRIMARY KEY".into()));
    }
    Ok(Statement::CreateTable(TableDef::new(&table, columns)))
}

fn plan_insert(
    i: &pg_query::protobuf::InsertStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Statement> {
    let table = i
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| Error::Parse("INSERT without a relation".into()))?;
    let def = lookup(&table).ok_or_else(|| Error::UndefinedTable(table.clone()))?;

    // Explicit column list, else every column in declared order.
    let targets: Vec<String> = if i.cols.is_empty() {
        def.columns.iter().map(|c| c.name.clone()).collect()
    } else {
        i.cols
            .iter()
            .filter_map(|c| match c.node.as_ref()? {
                N::ResTarget(rt) => Some(rt.name.clone()),
                _ => None,
            })
            .collect()
    };
    for t in &targets {
        if def.column(t).is_none() {
            return Err(Error::UndefinedColumn(t.clone()));
        }
    }

    let sel = match i.select_stmt.as_ref().and_then(|s| s.node.as_ref()) {
        Some(N::SelectStmt(s)) => s,
        _ => return Err(Error::Unsupported("INSERT without VALUES".into())),
    };
    let mut rows = Vec::new();
    for vl in &sel.values_lists {
        let items = match vl.node.as_ref() {
            Some(N::List(l)) => &l.items,
            _ => return Err(Error::Unsupported("this VALUES form".into())),
        };
        if items.len() != targets.len() {
            return Err(Error::Unsupported(
                "a VALUES row whose width differs from the column list".into(),
            ));
        }
        let mut d = Document::new();
        for (col, item) in targets.iter().zip(items) {
            let field = def.field_of(col).expect("checked above");
            d.insert(field, const_value(item)?);
        }
        rows.push(d);
    }
    Ok(Statement::Insert(Insert { table, rows }))
}

fn plan_select(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Statement> {
    if s.from_clause.len() != 1 {
        return Err(Error::Unsupported(
            "a SELECT that is not from one table".into(),
        ));
    }
    let table = match s.from_clause[0].node.as_ref() {
        Some(N::RangeVar(r)) => r.relname.clone(),
        Some(other) => return Err(Error::Unsupported(disc(other))),
        None => return Err(Error::Parse("empty FROM".into())),
    };
    let def = lookup(&table).ok_or_else(|| Error::UndefinedTable(table.clone()))?;

    let mut columns: Vec<(String, String)> = Vec::new();
    for t in &s.target_list {
        let rt = match t.node.as_ref() {
            Some(N::ResTarget(rt)) => rt,
            Some(other) => return Err(Error::Unsupported(disc(other))),
            None => continue,
        };
        match rt.val.as_ref().and_then(|v| v.node.as_ref()) {
            Some(N::ColumnRef(c)) => {
                let first = c.fields.first().and_then(|f| f.node.as_ref());
                if matches!(first, Some(N::AStar(_))) {
                    for col in &def.columns {
                        columns.push((col.name.clone(), col.field()));
                    }
                    continue;
                }
                let name = match first {
                    Some(N::String(st)) => st.sval.clone(),
                    _ => return Err(Error::Unsupported("this target".into())),
                };
                let field = def
                    .field_of(&name)
                    .ok_or_else(|| Error::UndefinedColumn(name.clone()))?;
                let out = if rt.name.is_empty() {
                    name
                } else {
                    rt.name.clone()
                };
                columns.push((out, field));
            }
            Some(other) => return Err(Error::Unsupported(disc(other))),
            None => return Err(Error::Unsupported("an empty target".into())),
        }
    }

    let filter = match s.where_clause.as_ref() {
        None => Document::new(),
        Some(w) => lower_where(w, &def)?,
    };
    Ok(Statement::Select(Select {
        table,
        columns,
        filter,
    }))
}

/// A WHERE predicate as a Mongo filter over STORED FIELDS.
pub fn lower_where(node: &pg_query::protobuf::Node, def: &TableDef) -> Result<Document> {
    match node.node.as_ref() {
        Some(N::AExpr(e)) => lower_aexpr(e, def),
        Some(N::BoolExpr(b)) => {
            // Match on the NAMED enum, never the wire integer: an earlier cut
            // of this used 0/1 and silently turned every AND into an OR, which
            // is a wrong ANSWER rather than an error.
            let key = match BoolExprType::try_from(b.boolop) {
                Ok(BoolExprType::AndExpr) => "$and",
                Ok(BoolExprType::OrExpr) => "$or",
                // NOT is deliberately absent: `$nor` is NOT the same as SQL NOT
                // once NULLs are involved, and guessing here would be exactly
                // the silent divergence this server refuses to produce.
                _ => return Err(Error::Unsupported("NOT in a predicate".into())),
            };
            let arms = b
                .args
                .iter()
                .map(|a| lower_where(a, def))
                .collect::<Result<Vec<_>>>()?;
            Ok(doc! { key: arms })
        }
        Some(other) => Err(Error::Unsupported(disc(other))),
        None => Err(Error::Parse("empty predicate".into())),
    }
}

fn const_value(node: &pg_query::protobuf::Node) -> Result<Bson> {
    match node.node.as_ref() {
        Some(N::AConst(c)) => {
            if c.isnull {
                return Ok(Bson::Null);
            }
            match c.val.as_ref() {
                Some(a_const::Val::Ival(i)) => Ok(Bson::Int32(i.ival)),
                Some(a_const::Val::Sval(s)) => Ok(Bson::String(s.sval.clone())),
                Some(a_const::Val::Fval(f)) => f
                    .fval
                    .parse::<f64>()
                    .map(Bson::Double)
                    .map_err(|_| Error::Unsupported("this numeric literal".into())),
                Some(a_const::Val::Boolval(b)) => Ok(Bson::Boolean(b.boolval)),
                _ => Err(Error::Unsupported("this constant".into())),
            }
        }
        Some(other) => Err(Error::Unsupported(disc(other))),
        None => Err(Error::Parse("empty constant".into())),
    }
}

fn lower_aexpr(e: &AExpr, def: &TableDef) -> Result<Document> {
    let op = e
        .name
        .first()
        .and_then(|n| n.node.as_ref())
        .and_then(|n| match n {
            N::String(s) => Some(s.sval.as_str()),
            _ => None,
        })
        .ok_or_else(|| Error::Unsupported("an operator with no name".into()))?;

    let col = match e.lexpr.as_ref().and_then(|l| l.node.as_ref()) {
        Some(N::ColumnRef(c)) => c
            .fields
            .first()
            .and_then(|f| f.node.as_ref())
            .and_then(|n| match n {
                N::String(s) => Some(s.sval.clone()),
                _ => None,
            })
            .ok_or_else(|| Error::Unsupported("this column reference".into()))?,
        Some(other) => return Err(Error::Unsupported(disc(other))),
        None => return Err(Error::Parse("no left operand".into())),
    };
    let field = def
        .field_of(&col)
        .ok_or_else(|| Error::UndefinedColumn(col.clone()))?;

    let value = const_value(
        e.rexpr
            .as_ref()
            .ok_or_else(|| Error::Parse("no right operand".into()))?,
    )?;

    let mongo_op = match op {
        "=" => return Ok(doc! { field: value }),
        ">" => "$gt",
        ">=" => "$gte",
        "<" => "$lt",
        "<=" => "$lte",
        "<>" | "!=" => "$ne",
        other => return Err(Error::Unsupported(format!("operator {other}"))),
    };
    Ok(doc! { field: { mongo_op: value } })
}

#[cfg(test)]
mod tests;
