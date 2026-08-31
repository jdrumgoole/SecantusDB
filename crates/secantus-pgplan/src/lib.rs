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
use pg_query::protobuf::{
    a_const, AExpr, AExprKind, BoolExprType, DropBehavior, NullTestType, ObjectType, SortByDir,
    SortByNulls, TransactionStmtKind,
};
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
    /// A bare column beside an aggregate, not in GROUP BY -> 42803.
    Grouping(String),
    /// A `$N` with no bound value -> 42P02.
    Parameter(String),
    /// A value that cannot be read as its target type -> 22P02.
    InvalidText(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Parse(m) => write!(f, "{m}"),
            Error::Unsupported(m) => write!(f, "{m} is not supported yet"),
            Error::UndefinedColumn(c) => write!(f, "column \"{c}\" does not exist"),
            Error::UndefinedTable(t) => write!(f, "relation \"{t}\" does not exist"),
            Error::Grouping(m) => write!(f, "{m}"),
            Error::Parameter(m) => write!(f, "{m}"),
            Error::InvalidText(m) => write!(f, "{m}"),
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
            Error::Grouping(_) => "42803",    // grouping_error
            Error::Parameter(_) => "42P02",   // undefined_parameter
            Error::InvalidText(_) => "22P02", // invalid_text_representation
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
    SelectConstant(SelectConstant),
    Transaction(TransactionControl),
    DropTable(DropTable),
    Aggregate(Aggregate),
    Update(Update),
    Delete(Delete),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Insert {
    pub table: String,
    /// One document per row, already keyed by stored FIELD (PK as `_id`).
    pub rows: Vec<Document>,
}

/// Where SQL puts NULLs in an ORDER BY.
///
/// PostgreSQL's defaults are ASC -> NULLS LAST and DESC -> NULLS FIRST (probed
/// 14 on 2026-08-31). MongoDB sorts null LOW, so pushing an ASC sort into the
/// storage layer would put NULLs first and quietly reorder every nullable
/// column. The sort therefore runs here, in PostgreSQL's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nulls {
    First,
    Last,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderKey {
    pub field: String,
    pub ascending: bool,
    pub nulls: Nulls,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    pub table: String,
    /// Output columns in order, as (output name, stored field).
    pub columns: Vec<(String, String)>,
    pub filter: Document,
    pub order: Vec<OrderKey>,
    /// `None` = no LIMIT. `LIMIT 0` is a real limit, not an absent one.
    pub limit: Option<i64>,
    pub offset: i64,
}

/// The aggregate functions this slice computes exactly.
///
/// `avg` is deliberately absent: PostgreSQL returns `numeric` with its own
/// scale rules (`avg(int4)` over {1,3} is `2.0000000000000000`), and
/// approximating that would be a wrong answer rather than a missing feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFunc {
    /// `count(*)` — counts ROWS, including those whose columns are all NULL.
    CountStar,
    /// `count(col)` — skips NULLs.
    Count,
    Sum,
    Min,
    Max,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AggItem {
    pub func: AggFunc,
    /// Stored field; `None` only for `count(*)`.
    pub field: Option<String>,
    /// Output column name.
    pub out: String,
    /// The declared PostgreSQL type of the source column, for `min`/`max`
    /// which return the input type. `count` and `sum` are always int8.
    pub source_type: Option<String>,
}

/// One output column of an aggregate query, by POSITION.
///
/// Deliberately not a name: `SELECT count(*), count(n)` gives two columns both
/// called `count`, and keying the result row by name silently dropped the
/// first. Position also lets `GROUP BY s ORDER BY s` work when `s` is not
/// projected at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputCol {
    /// Index into `Aggregate::group_by`.
    Group(usize),
    /// Index into `Aggregate::items`.
    Agg(usize),
}

/// ORDER BY over an aggregate query, by index into `Aggregate::group_by`.
#[derive(Debug, Clone, PartialEq)]
pub struct AggOrderKey {
    pub group_index: usize,
    pub ascending: bool,
    pub nulls: Nulls,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    pub table: String,
    /// EVERY GROUP BY column as (name, stored field), in declared order --
    /// including ones the SELECT list does not project, because ORDER BY may
    /// still reference them.
    pub group_by: Vec<(String, String)>,
    pub items: Vec<AggItem>,
    /// The output columns, in order, each pointing at a group or an aggregate.
    pub select: Vec<(String, OutputCol)>,
    pub filter: Document,
    pub order: Vec<AggOrderKey>,
    pub limit: Option<i64>,
    pub offset: i64,
}

/// Transaction control. Savepoints and prepared transactions are deliberately
/// absent -- they need machinery this server does not have, and pretending
/// would silently lose the semantics a client is relying on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionControl {
    Begin,
    Commit,
    Rollback,
}

/// `SELECT <items>` with no FROM: one row, computed without touching storage.
///
/// Clients lean on this constantly -- psycopg, pgjdbc and pgx all probe
/// `version()` and friends during connection setup -- so a server that cannot
/// answer it is unusable by real drivers even if every table query works.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectConstant {
    /// (output name, value, declared PostgreSQL type).
    ///
    /// The type is carried EXPLICITLY rather than inferred from the value.
    /// `Describe` arrives before `Bind` and is planned against NULL
    /// placeholders, so inferring from the value typed `$1::int` as `varchar`
    /// and the client then decoded a perfectly good integer as a string.
    pub columns: Vec<(String, Bson, String)>,
}

/// `DROP TABLE a, b` / `DROP TABLE IF EXISTS a`.
#[derive(Debug, Clone, PartialEq)]
pub struct DropTable {
    pub tables: Vec<String>,
    /// `IF EXISTS`: a missing table is not an error (probed PG 14, which still
    /// answers the `DROP TABLE` tag).
    pub if_exists: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub table: String,
    /// Stored field -> new value.
    pub set: Document,
    pub filter: Document,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Delete {
    pub table: String,
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
    plan_with_params(sql, lookup, &[])
}

/// Plan a statement whose `$N` placeholders are filled from `params`.
///
/// The extended protocol binds values AFTER parsing, so the same SQL is planned
/// once per Bind. Substituting at plan time keeps every NULL rule in one place:
/// a bound NULL then flows through the same `IS NULL` / `<>` / `NOT IN` paths as
/// a literal one, rather than needing its own parallel set.
pub fn plan_with_params(
    sql: &str,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<Statement> {
    match parse_one(sql)? {
        N::CreateStmt(c) => plan_create(&c),
        N::InsertStmt(i) => plan_insert(&i, lookup, params),
        N::SelectStmt(s) => plan_select(&s, lookup, params),
        N::DropStmt(d) => plan_drop(&d),
        N::TransactionStmt(t) => {
            // Named enum, not the wire integer -- twice bitten already.
            match TransactionStmtKind::try_from(t.kind) {
                Ok(TransactionStmtKind::TransStmtBegin | TransactionStmtKind::TransStmtStart) => {
                    Ok(Statement::Transaction(TransactionControl::Begin))
                }
                Ok(TransactionStmtKind::TransStmtCommit) => {
                    Ok(Statement::Transaction(TransactionControl::Commit))
                }
                Ok(TransactionStmtKind::TransStmtRollback) => {
                    Ok(Statement::Transaction(TransactionControl::Rollback))
                }
                Ok(other) => Err(Error::Unsupported(format!("{other:?}"))),
                Err(_) => Err(Error::Unsupported("this transaction statement".into())),
            }
        }
        N::UpdateStmt(u) => plan_update(&u, lookup, params),
        N::DeleteStmt(d) => plan_delete(&d, lookup, params),
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
    params: &[Bson],
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
            d.insert(field, const_value(item, params)?);
        }
        rows.push(d);
    }
    Ok(Statement::Insert(Insert { table, rows }))
}

/// Does this target list contain an aggregate call?
fn has_aggregate(s: &pg_query::protobuf::SelectStmt) -> bool {
    s.target_list.iter().any(|t| {
        matches!(
            t.node.as_ref(),
            Some(N::ResTarget(rt))
                if matches!(rt.val.as_ref().and_then(|v| v.node.as_ref()), Some(N::FuncCall(_)))
        )
    })
}

fn plan_select(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<Statement> {
    if s.from_clause.is_empty() {
        return plan_select_constant(s, params);
    }
    if !s.group_clause.is_empty() || has_aggregate(s) {
        return plan_aggregate(s, lookup, params);
    }
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
        Some(w) => lower_where(w, &def, params)?,
    };

    let mut order = Vec::new();
    for item in &s.sort_clause {
        let Some(N::SortBy(sb)) = item.node.as_ref() else {
            return Err(Error::Unsupported("this ORDER BY item".into()));
        };
        let col = match sb.node.as_ref().and_then(|n| n.node.as_ref()) {
            Some(N::ColumnRef(c)) => c
                .fields
                .first()
                .and_then(|f| f.node.as_ref())
                .and_then(|n| match n {
                    N::String(st) => Some(st.sval.clone()),
                    _ => None,
                })
                .ok_or_else(|| Error::Unsupported("this ORDER BY expression".into()))?,
            // ORDER BY over an expression, or by output position, needs
            // machinery this slice does not have. Refuse, never approximate.
            _ => return Err(Error::Unsupported("ORDER BY over an expression".into())),
        };
        let field = def
            .field_of(&col)
            .ok_or_else(|| Error::UndefinedColumn(col.clone()))?;
        let ascending = match SortByDir::try_from(sb.sortby_dir) {
            Ok(SortByDir::SortbyDesc) => false,
            Ok(SortByDir::SortbyDefault | SortByDir::SortbyAsc) => true,
            _ => return Err(Error::Unsupported("ORDER BY ... USING".into())),
        };
        // The DEFAULT null placement depends on the direction: PostgreSQL 14
        // puts NULLs LAST on ASC and FIRST on DESC.
        let nulls = match SortByNulls::try_from(sb.sortby_nulls) {
            Ok(SortByNulls::SortbyNullsFirst) => Nulls::First,
            Ok(SortByNulls::SortbyNullsLast) => Nulls::Last,
            _ if ascending => Nulls::Last,
            _ => Nulls::First,
        };
        order.push(OrderKey {
            field,
            ascending,
            nulls,
        });
    }

    let limit = match s.limit_count.as_ref() {
        None => None,
        Some(n) => match const_value(n, params)? {
            Bson::Int32(v) => Some(i64::from(v)),
            Bson::Int64(v) => Some(v),
            // `LIMIT NULL` means "no limit" in PostgreSQL, not "limit zero".
            Bson::Null => None,
            _ => return Err(Error::Unsupported("this LIMIT".into())),
        },
    };
    let offset = match s.limit_offset.as_ref() {
        None => 0,
        Some(n) => match const_value(n, params)? {
            Bson::Int32(v) => i64::from(v),
            Bson::Int64(v) => v,
            Bson::Null => 0,
            _ => return Err(Error::Unsupported("this OFFSET".into())),
        },
    };

    Ok(Statement::Select(Select {
        table,
        columns,
        filter,
        order,
        limit,
        offset,
    }))
}

/// `SELECT [group cols,] agg(...) FROM t [WHERE ...] [GROUP BY ...]`.
///
/// PostgreSQL's aggregate NULL rules, all probed on 14 (2026-08-31):
/// `count(*)` counts ROWS (NULL columns included) and is 0 over an empty set,
/// while `count(col)`, `sum`, `min` and `max` all SKIP NULLs and every one of
/// them except `count` yields **NULL, not zero**, when nothing survives the
/// filter. NULL forms its own GROUP BY group.
fn plan_aggregate(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<Statement> {
    if s.from_clause.len() != 1 {
        return Err(Error::Unsupported(
            "an aggregate that is not over one table".into(),
        ));
    }
    if s.having_clause.is_some() {
        return Err(Error::Unsupported("HAVING".into()));
    }
    if !s.distinct_clause.is_empty() {
        return Err(Error::Unsupported("DISTINCT with an aggregate".into()));
    }
    let table = match s.from_clause[0].node.as_ref() {
        Some(N::RangeVar(r)) => r.relname.clone(),
        Some(other) => return Err(Error::Unsupported(disc(other))),
        None => return Err(Error::Parse("empty FROM".into())),
    };
    let def = lookup(&table).ok_or_else(|| Error::UndefinedTable(table.clone()))?;

    // GROUP BY columns, in declared order.
    let mut group_by: Vec<(String, String)> = Vec::new();
    for g in &s.group_clause {
        let name = match g.node.as_ref() {
            Some(N::ColumnRef(c)) => c
                .fields
                .first()
                .and_then(|f| f.node.as_ref())
                .and_then(|n| match n {
                    N::String(st) => Some(st.sval.clone()),
                    _ => None,
                })
                .ok_or_else(|| Error::Unsupported("this GROUP BY expression".into()))?,
            _ => return Err(Error::Unsupported("GROUP BY over an expression".into())),
        };
        let field = def
            .field_of(&name)
            .ok_or_else(|| Error::UndefinedColumn(name.clone()))?;
        group_by.push((name, field));
    }

    let mut items: Vec<AggItem> = Vec::new();
    let mut select: Vec<(String, OutputCol)> = Vec::new();
    for t in &s.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return Err(Error::Unsupported("this target".into()));
        };
        match rt.val.as_ref().and_then(|v| v.node.as_ref()) {
            Some(N::FuncCall(f)) => {
                let name = f
                    .funcname
                    .iter()
                    .filter_map(|n| match n.node.as_ref()? {
                        N::String(st) => Some(st.sval.clone()),
                        _ => None,
                    })
                    .next_back()
                    .unwrap_or_default();
                if f.agg_distinct {
                    return Err(Error::Unsupported("DISTINCT inside an aggregate".into()));
                }
                if f.agg_filter.is_some() {
                    return Err(Error::Unsupported("FILTER on an aggregate".into()));
                }
                let (func, field, source_type) = if name == "count" && f.agg_star {
                    (AggFunc::CountStar, None, None)
                } else {
                    let func = match name.as_str() {
                        "count" => AggFunc::Count,
                        "sum" => AggFunc::Sum,
                        "min" => AggFunc::Min,
                        "max" => AggFunc::Max,
                        // `avg` returns PostgreSQL `numeric` with its own scale
                        // rules; approximating it would be a wrong answer.
                        other => return Err(Error::Unsupported(format!("aggregate {other}()"))),
                    };
                    if f.args.len() != 1 {
                        return Err(Error::Unsupported(
                            "an aggregate with more than one argument".into(),
                        ));
                    }
                    let col = match f.args[0].node.as_ref() {
                        Some(N::ColumnRef(c)) => c
                            .fields
                            .first()
                            .and_then(|x| x.node.as_ref())
                            .and_then(|n| match n {
                                N::String(st) => Some(st.sval.clone()),
                                _ => None,
                            })
                            .ok_or_else(|| Error::Unsupported("this aggregate argument".into()))?,
                        _ => {
                            return Err(Error::Unsupported(
                                "an aggregate over an expression".into(),
                            ))
                        }
                    };
                    let column = def
                        .column(&col)
                        .ok_or_else(|| Error::UndefinedColumn(col.clone()))?;
                    (func, Some(column.field()), Some(column.pg_type.clone()))
                };
                let out = if rt.name.is_empty() {
                    name.clone()
                } else {
                    rt.name.clone()
                };
                select.push((out.clone(), OutputCol::Agg(items.len())));
                items.push(AggItem {
                    func,
                    field,
                    out,
                    source_type,
                });
            }
            Some(N::ColumnRef(c)) => {
                let col = c
                    .fields
                    .first()
                    .and_then(|f| f.node.as_ref())
                    .and_then(|n| match n {
                        N::String(st) => Some(st.sval.clone()),
                        _ => None,
                    })
                    .ok_or_else(|| Error::Unsupported("this target".into()))?;
                // A bare column alongside an aggregate must be grouped by --
                // PostgreSQL errors 42803 otherwise, and so do we.
                let idx = group_by
                    .iter()
                    .position(|(name, _)| *name == col)
                    .ok_or_else(|| {
                        Error::Grouping(format!(
                            "column \"{col}\" must appear in the GROUP BY clause \
                             or be used in an aggregate function"
                        ))
                    })?;
                let out = if rt.name.is_empty() {
                    col
                } else {
                    rt.name.clone()
                };
                select.push((out, OutputCol::Group(idx)));
            }
            Some(other) => return Err(Error::Unsupported(disc(other))),
            None => return Err(Error::Unsupported("an empty target".into())),
        }
    }

    let filter = match s.where_clause.as_ref() {
        None => Document::new(),
        Some(w) => lower_where(w, &def, params)?,
    };

    // ORDER BY is allowed only over GROUP BY columns in this slice; ordering by
    // an aggregate result is a separate piece of work.
    let mut order: Vec<AggOrderKey> = Vec::new();
    for item in &s.sort_clause {
        let Some(N::SortBy(sb)) = item.node.as_ref() else {
            return Err(Error::Unsupported("this ORDER BY item".into()));
        };
        let col = match sb.node.as_ref().and_then(|n| n.node.as_ref()) {
            Some(N::ColumnRef(c)) => c
                .fields
                .first()
                .and_then(|f| f.node.as_ref())
                .and_then(|n| match n {
                    N::String(st) => Some(st.sval.clone()),
                    _ => None,
                })
                .ok_or_else(|| Error::Unsupported("this ORDER BY expression".into()))?,
            _ => return Err(Error::Unsupported("ORDER BY over an expression".into())),
        };
        let group_index = group_by
            .iter()
            .position(|(name, _)| *name == col)
            .ok_or_else(|| Error::Unsupported("ORDER BY over an aggregate result".into()))?;
        let ascending = match SortByDir::try_from(sb.sortby_dir) {
            Ok(SortByDir::SortbyDesc) => false,
            Ok(SortByDir::SortbyDefault | SortByDir::SortbyAsc) => true,
            _ => return Err(Error::Unsupported("ORDER BY ... USING".into())),
        };
        let nulls = match SortByNulls::try_from(sb.sortby_nulls) {
            Ok(SortByNulls::SortbyNullsFirst) => Nulls::First,
            Ok(SortByNulls::SortbyNullsLast) => Nulls::Last,
            _ if ascending => Nulls::Last,
            _ => Nulls::First,
        };
        order.push(AggOrderKey {
            group_index,
            ascending,
            nulls,
        });
    }

    let limit = match s.limit_count.as_ref() {
        None => None,
        Some(n) => match const_value(n, params)? {
            Bson::Int32(v) => Some(i64::from(v)),
            Bson::Int64(v) => Some(v),
            Bson::Null => None,
            _ => return Err(Error::Unsupported("this LIMIT".into())),
        },
    };
    let offset = match s.limit_offset.as_ref() {
        None => 0,
        Some(n) => match const_value(n, params)? {
            Bson::Int32(v) => i64::from(v),
            Bson::Int64(v) => v,
            Bson::Null => 0,
            _ => return Err(Error::Unsupported("this OFFSET".into())),
        },
    };

    Ok(Statement::Aggregate(Aggregate {
        table,
        group_by,
        items,
        select,
        filter,
        order,
        limit,
        offset,
    }))
}

/// The session functions a connecting client asks for.
///
/// The version string mirrors the Python server's shape so both identify as
/// SecantusDB -- the conformance gauges refuse to run against a daemon whose
/// `version()` does not name it, precisely so a stray real PostgreSQL cannot
/// inflate the numbers.
fn session_function(name: &str) -> Option<Bson> {
    Some(match name {
        "version" => Bson::String(format!(
            "PostgreSQL 15.0 (SecantusDB) on {}, compiled by rust",
            std::env::consts::ARCH
        )),
        "current_database" | "current_catalog" => Bson::String("postgres".into()),
        "current_schema" => Bson::String("public".into()),
        "current_user" | "session_user" | "user" => Bson::String("postgres".into()),
        _ => return None,
    })
}

/// The PostgreSQL type a constant value carries when nothing declares one.
fn inferred_type(v: &Bson) -> &'static str {
    match v {
        Bson::Int32(_) => "int4",
        Bson::Int64(_) => "int8",
        Bson::Double(_) => "float8",
        Bson::Boolean(_) => "bool",
        _ => "text",
    }
}

fn plan_select_constant(s: &pg_query::protobuf::SelectStmt, params: &[Bson]) -> Result<Statement> {
    let mut columns: Vec<(String, Bson, String)> = Vec::new();
    for t in &s.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return Err(Error::Unsupported("this target".into()));
        };
        let (default_name, value, pg_type) = match rt.val.as_ref().and_then(|v| v.node.as_ref()) {
            Some(N::FuncCall(f)) => {
                let name = f
                    .funcname
                    .iter()
                    .filter_map(|n| match n.node.as_ref()? {
                        N::String(st) => Some(st.sval.clone()),
                        _ => None,
                    })
                    .next_back()
                    .unwrap_or_default();
                let v = session_function(&name)
                    .ok_or_else(|| Error::Unsupported(format!("function {name}()")))?;
                let t = inferred_type(&v).to_string();
                (name, v, t)
            }
            // `current_user` and friends parse as bare column refs, not calls.
            Some(N::ColumnRef(c)) => {
                let name = c
                    .fields
                    .first()
                    .and_then(|f| f.node.as_ref())
                    .and_then(|n| match n {
                        N::String(st) => Some(st.sval.clone()),
                        _ => None,
                    })
                    .ok_or_else(|| Error::Unsupported("this target".into()))?;
                let v =
                    session_function(&name).ok_or_else(|| Error::UndefinedColumn(name.clone()))?;
                let t = inferred_type(&v).to_string();
                (name, v, t)
            }
            Some(node @ (N::AConst(_) | N::ParamRef(_) | N::TypeCast(_))) => {
                let v = const_value(rt.val.as_ref().expect("checked"), params)?;
                // A cast DECLARES the type; anything else takes the value's.
                // This must not depend on the value, because Describe plans
                // with NULL placeholders.
                let t = match node {
                    N::TypeCast(tc) => tc
                        .type_name
                        .as_ref()
                        .map(|n| type_name(&n.names))
                        .unwrap_or_else(|| inferred_type(&v).to_string()),
                    _ => inferred_type(&v).to_string(),
                };
                ("?column?".to_string(), v, t)
            }
            Some(other) => return Err(Error::Unsupported(disc(other))),
            None => return Err(Error::Unsupported("an empty target".into())),
        };
        let out = if rt.name.is_empty() {
            default_name
        } else {
            rt.name.clone()
        };
        columns.push((out, value, pg_type));
    }
    Ok(Statement::SelectConstant(SelectConstant { columns }))
}

/// `DROP TABLE`. Other DROP targets (index, view, schema) stay unsupported --
/// each needs its own catalog work, and dropping the wrong thing silently
/// would be unrecoverable.
/// Coerce a value to a PostgreSQL type, as `::` does.
///
/// Probed PG 14: `'1'::int` is 1, `1::text` is `"1"`, `'1.5'::float8` is 1.5,
/// `'true'::bool` is true, and **`null::int` stays NULL** rather than becoming
/// a zero. A value that cannot be read as the target type is `22P02
/// invalid_text_representation`, quoting the offending input the way
/// PostgreSQL does.
fn cast_value(value: Bson, target: &str) -> Result<Bson> {
    // A NULL survives every cast; only its declared type changes.
    if value == Bson::Null {
        return Ok(Bson::Null);
    }
    let as_text = |v: &Bson| match v {
        Bson::String(s) => s.clone(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Double(d) => {
            // PostgreSQL renders a whole float8 without a trailing `.0`.
            if d.fract() == 0.0 && d.is_finite() {
                format!("{}", *d as i64)
            } else {
                d.to_string()
            }
        }
        Bson::Boolean(b) => (if *b { "true" } else { "false" }).to_string(),
        other => format!("{other:?}"),
    };
    let bad = |want: &str, v: &Bson| {
        Error::InvalidText(format!(
            "invalid input syntax for type {want}: \"{}\"",
            as_text(v)
        ))
    };

    match target {
        "int4" | "int2" | "integer" | "int" | "smallint" => match &value {
            Bson::Int32(_) => Ok(value),
            Bson::Int64(i) => i32::try_from(*i)
                .map(Bson::Int32)
                .map_err(|_| Error::InvalidText(format!("integer out of range: \"{i}\""))),
            Bson::Double(d) => Ok(Bson::Int32(d.round() as i32)),
            Bson::String(s) => s
                .trim()
                .parse::<i32>()
                .map(Bson::Int32)
                .map_err(|_| bad("integer", &value)),
            _ => Err(bad("integer", &value)),
        },
        "int8" | "bigint" => match &value {
            Bson::Int32(i) => Ok(Bson::Int64(i64::from(*i))),
            Bson::Int64(_) => Ok(value),
            Bson::Double(d) => Ok(Bson::Int64(d.round() as i64)),
            Bson::String(s) => s
                .trim()
                .parse::<i64>()
                .map(Bson::Int64)
                .map_err(|_| bad("bigint", &value)),
            _ => Err(bad("bigint", &value)),
        },
        "float4" | "float8" | "numeric" | "real" | "double" => match &value {
            Bson::Int32(i) => Ok(Bson::Double(f64::from(*i))),
            Bson::Int64(i) => Ok(Bson::Double(*i as f64)),
            Bson::Double(_) => Ok(value),
            Bson::String(s) => s
                .trim()
                .parse::<f64>()
                .map(Bson::Double)
                .map_err(|_| bad("double precision", &value)),
            _ => Err(bad("double precision", &value)),
        },
        "bool" | "boolean" => match &value {
            Bson::Boolean(_) => Ok(value),
            Bson::Int32(i) => Ok(Bson::Boolean(*i != 0)),
            Bson::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "t" | "true" | "y" | "yes" | "on" | "1" => Ok(Bson::Boolean(true)),
                "f" | "false" | "n" | "no" | "off" | "0" => Ok(Bson::Boolean(false)),
                _ => Err(bad("boolean", &value)),
            },
            _ => Err(bad("boolean", &value)),
        },
        "text" | "varchar" | "bpchar" | "char" | "name" => Ok(Bson::String(as_text(&value))),
        other => Err(Error::Unsupported(format!("a cast to {other}"))),
    }
}

fn plan_drop(d: &pg_query::protobuf::DropStmt) -> Result<Statement> {
    if ObjectType::try_from(d.remove_type) != Ok(ObjectType::ObjectTable) {
        return Err(Error::Unsupported(format!(
            "DROP of {:?}",
            ObjectType::try_from(d.remove_type)
        )));
    }
    // CASCADE would have to chase dependants; refuse rather than silently
    // behave as RESTRICT. DropBehavior: Restrict = 1, Cascade = 2.
    if DropBehavior::try_from(d.behavior) == Ok(DropBehavior::DropCascade) {
        return Err(Error::Unsupported("DROP TABLE ... CASCADE".into()));
    }
    let mut tables = Vec::new();
    for obj in &d.objects {
        // Each object is a List of name parts (schema, table).
        let parts = match obj.node.as_ref() {
            Some(N::List(l)) => &l.items,
            _ => return Err(Error::Unsupported("this DROP target".into())),
        };
        let name = parts
            .iter()
            .filter_map(|n| match n.node.as_ref()? {
                N::String(s) => Some(s.sval.clone()),
                _ => None,
            })
            .next_back()
            .ok_or_else(|| Error::Unsupported("this DROP target".into()))?;
        tables.push(name);
    }
    if tables.is_empty() {
        return Err(Error::Parse("DROP TABLE with no table".into()));
    }
    Ok(Statement::DropTable(DropTable {
        tables,
        if_exists: d.missing_ok,
    }))
}

fn plan_update(
    u: &pg_query::protobuf::UpdateStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<Statement> {
    let table = u
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| Error::Parse("UPDATE without a relation".into()))?;
    let def = lookup(&table).ok_or_else(|| Error::UndefinedTable(table.clone()))?;

    let mut set = Document::new();
    for t in &u.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return Err(Error::Unsupported("this SET target".into()));
        };
        let column = def
            .column(&rt.name)
            .ok_or_else(|| Error::UndefinedColumn(rt.name.clone()))?;
        // The PRIMARY KEY is the document's `_id`, which the storage layer
        // treats as immutable. Refuse rather than half-perform the update.
        if column.pk {
            return Err(Error::Unsupported("UPDATE of a PRIMARY KEY column".into()));
        }
        let field = column.field();
        let value = const_value(
            rt.val
                .as_ref()
                .ok_or_else(|| Error::Parse("SET without a value".into()))?,
            params,
        )?;
        set.insert(field, value);
    }
    if set.is_empty() {
        return Err(Error::Parse("UPDATE without a SET list".into()));
    }
    let filter = match u.where_clause.as_ref() {
        None => Document::new(),
        Some(w) => lower_where(w, &def, params)?,
    };
    Ok(Statement::Update(Update { table, set, filter }))
}

fn plan_delete(
    d: &pg_query::protobuf::DeleteStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<Statement> {
    let table = d
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| Error::Parse("DELETE without a relation".into()))?;
    let def = lookup(&table).ok_or_else(|| Error::UndefinedTable(table.clone()))?;
    let filter = match d.where_clause.as_ref() {
        None => Document::new(),
        Some(w) => lower_where(w, &def, params)?,
    };
    Ok(Statement::Delete(Delete { table, filter }))
}

/// A WHERE predicate as a Mongo filter over STORED FIELDS.
pub fn lower_where(
    node: &pg_query::protobuf::Node,
    def: &TableDef,
    params: &[Bson],
) -> Result<Document> {
    match node.node.as_ref() {
        Some(N::AExpr(e)) => lower_aexpr(e, def, params),
        Some(N::NullTest(t)) => {
            let field = column_field(t.arg.as_deref(), def)?;
            // A SQL NULL is either an explicit null or an absent field here,
            // and MQL's `null` / `$ne: null` match exactly that pair.
            match NullTestType::try_from(t.nulltesttype) {
                Ok(NullTestType::IsNull) => Ok(doc! { field: Bson::Null }),
                Ok(NullTestType::IsNotNull) => Ok(doc! { field: { "$ne": Bson::Null } }),
                _ => Err(Error::Unsupported("this IS [NOT] NULL form".into())),
            }
        }
        Some(N::BoolExpr(b)) => {
            // Match on the NAMED enum, never the wire integer: an earlier cut
            // of this used 0/1 and silently turned every AND into an OR, which
            // is a wrong ANSWER rather than an error.
            let key = match BoolExprType::try_from(b.boolop) {
                Ok(BoolExprType::AndExpr) => "$and",
                Ok(BoolExprType::OrExpr) => "$or",
                Ok(BoolExprType::NotExpr) => {
                    let arg = b
                        .args
                        .first()
                        .ok_or_else(|| Error::Parse("NOT with no operand".into()))?;
                    return lower_negated(arg, def, params);
                }
                _ => return Err(Error::Unsupported("this boolean operator".into())),
            };
            let arms = b
                .args
                .iter()
                .map(|a| lower_where(a, def, params))
                .collect::<Result<Vec<_>>>()?;
            Ok(doc! { key: arms })
        }
        Some(other) => Err(Error::Unsupported(disc(other))),
        None => Err(Error::Parse("empty predicate".into())),
    }
}

/// A literal, or a bound `$N` parameter.
///
/// `params` is the extended protocol's Bind values, in order; `$1` is
/// `params[0]`. A statement planned without parameters passes an empty slice,
/// and a `$N` beyond its end is a client error rather than a panic.
fn const_value(node: &pg_query::protobuf::Node, params: &[Bson]) -> Result<Bson> {
    // `'1'::int`, `$1::text`, `null::int`. The cast is applied to whatever the
    // operand evaluates to, so a bound parameter casts exactly like a literal.
    if let Some(N::TypeCast(tc)) = node.node.as_ref() {
        let arg = tc
            .arg
            .as_ref()
            .ok_or_else(|| Error::Parse("cast with no operand".into()))?;
        let value = const_value(arg, params)?;
        let target = tc
            .type_name
            .as_ref()
            .map(|t| type_name(&t.names))
            .unwrap_or_default();
        return cast_value(value, &target);
    }
    if let Some(N::ParamRef(p)) = node.node.as_ref() {
        let idx = usize::try_from(p.number).unwrap_or(0);
        if idx == 0 {
            return Err(Error::Parse("parameter $0 is not valid".into()));
        }
        return params
            .get(idx - 1)
            .cloned()
            .ok_or_else(|| Error::Parameter(format!("there is no parameter ${idx}")));
    }
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

/// `NOT <predicate>`, pushed down rather than wrapped.
///
/// MQL has no operator equal to SQL's NOT: `$nor` matches a document whose
/// field is missing-or-null, where SQL's `NOT (n = 1)` over a NULL `n` yields
/// NULL and excludes the row. Pushing the negation into the leaves keeps every
/// leaf on the NULL-correct forms already built above.
///
/// De Morgan is valid in SQL's three-valued (Kleene) logic, so `NOT (a AND b)`
/// -> `NOT a OR NOT b` is sound, as is the double-negation collapse. Anything
/// not handled here stays an honest 0A000 rather than an approximation.
fn lower_negated(
    node: &pg_query::protobuf::Node,
    def: &TableDef,
    params: &[Bson],
) -> Result<Document> {
    match node.node.as_ref() {
        Some(N::BoolExpr(b)) => match BoolExprType::try_from(b.boolop) {
            Ok(BoolExprType::AndExpr) => {
                let arms = b
                    .args
                    .iter()
                    .map(|a| lower_negated(a, def, params))
                    .collect::<Result<Vec<_>>>()?;
                Ok(doc! { "$or": arms })
            }
            Ok(BoolExprType::OrExpr) => {
                let arms = b
                    .args
                    .iter()
                    .map(|a| lower_negated(a, def, params))
                    .collect::<Result<Vec<_>>>()?;
                Ok(doc! { "$and": arms })
            }
            // NOT NOT x is x.
            Ok(BoolExprType::NotExpr) => {
                let arg = b
                    .args
                    .first()
                    .ok_or_else(|| Error::Parse("NOT with no operand".into()))?;
                lower_where(arg, def, params)
            }
            _ => Err(Error::Unsupported("this boolean operator".into())),
        },
        Some(N::NullTest(t)) => {
            let field = column_field(t.arg.as_deref(), def)?;
            match NullTestType::try_from(t.nulltesttype) {
                Ok(NullTestType::IsNull) => Ok(doc! { field: { "$ne": Bson::Null } }),
                Ok(NullTestType::IsNotNull) => Ok(doc! { field: Bson::Null }),
                _ => Err(Error::Unsupported("this IS [NOT] NULL form".into())),
            }
        }
        Some(N::AExpr(e)) => {
            let mut flipped = e.clone();
            match AExprKind::try_from(e.kind) {
                Ok(AExprKind::AexprBetween) => {
                    flipped.kind = AExprKind::AexprNotBetween as i32;
                    return lower_between(&flipped, def, params);
                }
                Ok(AExprKind::AexprNotBetween) => {
                    flipped.kind = AExprKind::AexprBetween as i32;
                    return lower_between(&flipped, def, params);
                }
                Ok(AExprKind::AexprIn) => {
                    // `IN` and `NOT IN` are distinguished by the operator name
                    // the parser attaches, so flip that.
                    flipped.name = vec![string_node(if in_is_negated(e) { "=" } else { "<>" })];
                    return lower_in(&flipped, def, params);
                }
                Ok(AExprKind::AexprOp) => {}
                _ => return Err(Error::Unsupported("this operator form".into())),
            }
            let op = operator_name(e)?;
            let negated = match op {
                "=" => "<>",
                "<>" | "!=" => "=",
                ">" => "<=",
                ">=" => "<",
                "<" => ">=",
                "<=" => ">",
                other => return Err(Error::Unsupported(format!("NOT over operator {other}"))),
            };
            flipped.name = vec![string_node(negated)];
            lower_aexpr(&flipped, def, params)
        }
        Some(other) => Err(Error::Unsupported(disc(other))),
        None => Err(Error::Parse("NOT with an empty operand".into())),
    }
}

fn string_node(s: &str) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::String(pg_query::protobuf::String {
            sval: s.to_string(),
        })),
    }
}

fn operator_name(e: &AExpr) -> Result<&str> {
    e.name
        .first()
        .and_then(|n| n.node.as_ref())
        .and_then(|n| match n {
            N::String(s) => Some(s.sval.as_str()),
            _ => None,
        })
        .ok_or_else(|| Error::Unsupported("an operator with no name".into()))
}

fn in_is_negated(e: &AExpr) -> bool {
    matches!(operator_name(e), Ok("<>"))
}

/// The stored field behind a bare column reference, or a typed error.
fn column_field(node: Option<&pg_query::protobuf::Node>, def: &TableDef) -> Result<String> {
    match node.and_then(|n| n.node.as_ref()) {
        Some(N::ColumnRef(c)) => {
            let name = c
                .fields
                .first()
                .and_then(|f| f.node.as_ref())
                .and_then(|n| match n {
                    N::String(st) => Some(st.sval.clone()),
                    _ => None,
                })
                .ok_or_else(|| Error::Unsupported("this column reference".into()))?;
            def.field_of(&name).ok_or(Error::UndefinedColumn(name))
        }
        Some(other) => Err(Error::Unsupported(disc(other))),
        None => Err(Error::Parse("missing operand".into())),
    }
}

/// A filter that matches nothing. `{}` matches every document, so NOR of it
/// matches none -- which is what `x NOT IN (.., NULL)` must answer.
fn match_nothing() -> Document {
    doc! { "$nor": [Document::new()] }
}

fn lower_aexpr(e: &AExpr, def: &TableDef, params: &[Bson]) -> Result<Document> {
    // Named enum, never the wire integer. Written against the integers first,
    // this had `Op = 0` (it is 1, so every plain `=` was refused) and
    // `Between = 10` (it is 11, so BETWEEN silently ran the NOT BETWEEN arm and
    // returned the complement). Same mistake as the BoolExpr one below.
    match AExprKind::try_from(e.kind) {
        Ok(AExprKind::AexprIn) => return lower_in(e, def, params),
        Ok(AExprKind::AexprBetween | AExprKind::AexprNotBetween) => {
            return lower_between(e, def, params)
        }
        Ok(AExprKind::AexprOp) => {}
        _ => return Err(Error::Unsupported("this operator form".into())),
    }
    let op = operator_name(e)?;

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
        params,
    )?;

    // A comparison against NULL is never TRUE in SQL -- `n = NULL`, `n <> NULL`
    // and every range operator yield NULL, so no row qualifies. Only `IS NULL`
    // matches. MQL's `{n: null}` would match, so this must short-circuit.
    // Probed PG 14: `select id from t where n = null` returns nothing, even for
    // the row whose `n` IS null. Applies equally to a literal NULL and a bound
    // `$1` -- the parameterised tests found it, but the literal was wrong too.
    if value == Bson::Null {
        return Ok(match_nothing());
    }

    let mongo_op = match op {
        // `=` and the range operators are already NULL-correct: MQL brackets by
        // type, so a null column value matches none of them -- which is what
        // three-valued logic gives.
        "=" => return Ok(doc! { field: value }),
        ">" => "$gt",
        ">=" => "$gte",
        "<" => "$lt",
        "<=" => "$lte",
        // `<>` is the exception, and it is a WRONG-ROWS bug, not a nicety.
        // MQL's `$ne` matches a missing-or-null field, so `n <> 1` returned the
        // row whose `n` is NULL. SQL says `NULL <> 1` is NULL, so PostgreSQL
        // excludes it (probed 14). The explicit not-null guard restores that.
        "<>" | "!=" => {
            return Ok(doc! {
                "$and": [
                    doc! { &field: { "$ne": value } },
                    doc! { &field: { "$ne": Bson::Null } },
                ]
            })
        }
        other => return Err(Error::Unsupported(format!("operator {other}"))),
    };
    Ok(doc! { field: { mongo_op: value } })
}

/// `x IN (a, b)` / `x NOT IN (a, b)`.
///
/// PostgreSQL's three-valued logic drives both edge cases here, probed on 14:
/// `n NOT IN (1)` does NOT return a row whose `n` is NULL (because `NULL <> 1`
/// is NULL, not true), and `n NOT IN (1, NULL)` returns nothing at all. MQL's
/// `$nin` would match a null on both counts, so the guard is explicit.
fn lower_in(e: &AExpr, def: &TableDef, params: &[Bson]) -> Result<Document> {
    let negated = in_is_negated(e);
    let field = column_field(e.lexpr.as_deref(), def)?;
    let items = match e.rexpr.as_ref().and_then(|r| r.node.as_ref()) {
        Some(N::List(l)) => &l.items,
        _ => return Err(Error::Unsupported("this IN list".into())),
    };
    let mut values = Vec::new();
    let mut saw_null = false;
    for item in items {
        let v = const_value(item, params)?;
        if v == Bson::Null {
            saw_null = true;
        } else {
            values.push(v);
        }
    }
    if negated {
        if saw_null {
            // `NOT IN` over a list containing NULL is never true.
            return Ok(match_nothing());
        }
        return Ok(doc! {
            "$and": [
                doc! { &field: { "$nin": values } },
                doc! { &field: { "$ne": Bson::Null } },
            ]
        });
    }
    // A NULL in a positive IN list simply never matches, so dropping it is
    // exactly right.
    Ok(doc! { field: { "$in": values } })
}

/// `x BETWEEN a AND b` / `x NOT BETWEEN a AND b`.
fn lower_between(e: &AExpr, def: &TableDef, params: &[Bson]) -> Result<Document> {
    let field = column_field(e.lexpr.as_deref(), def)?;
    let bounds = match e.rexpr.as_ref().and_then(|r| r.node.as_ref()) {
        Some(N::List(l)) if l.items.len() == 2 => &l.items,
        _ => return Err(Error::Unsupported("this BETWEEN form".into())),
    };
    let lo = const_value(&bounds[0], params)?;
    let hi = const_value(&bounds[1], params)?;
    if lo == Bson::Null || hi == Bson::Null {
        return Ok(match_nothing());
    }
    // Inclusive both ends. A NULL column value matches neither bound in MQL,
    // which is what PostgreSQL's three-valued logic gives too.
    if AExprKind::try_from(e.kind) == Ok(AExprKind::AexprNotBetween) {
        return Ok(doc! {
            "$and": [
                doc! { "$or": [
                    doc! { &field: { "$lt": &lo } },
                    doc! { &field: { "$gt": &hi } },
                ]},
                doc! { &field: { "$ne": Bson::Null } },
            ]
        });
    }
    Ok(doc! { field: { "$gte": lo, "$lte": hi } })
}

#[cfg(test)]
mod tests;
