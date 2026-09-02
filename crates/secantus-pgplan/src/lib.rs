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
use std::str::FromStr;

use bson::Decimal128;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use pg_query::protobuf::node::Node as N;
use pg_query::protobuf::{
    a_const, AExpr, AExprKind, BoolExprType, DropBehavior, NullTestType, ObjectType, SortByDir,
    SortByNulls, TransactionStmtKind, VariableSetKind,
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
    /// A malformed date/time literal -> 22007.
    InvalidDatetimeFormat(String),
    /// A well-formed date/time naming an impossible value -> 22008.
    DatetimeFieldOverflow(String),
    /// `x / 0` -> 22012.
    DivisionByZero,
    /// Integer overflow -> 22003.
    NumericOutOfRange(String),
    /// More than one command where only one is allowed -> 42601.
    ///
    /// PostgreSQL accepts a multi-command string over the SIMPLE query
    /// protocol and refuses it in a prepared statement, so this is a real
    /// error rather than a gap: the extended protocol has one parameter list
    /// and one row description, which two commands cannot share.
    MultipleCommands,
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
            Error::InvalidDatetimeFormat(m) | Error::DatetimeFieldOverflow(m) => {
                write!(f, "{m}")
            }
            Error::DivisionByZero => write!(f, "division by zero"),
            Error::NumericOutOfRange(m) => write!(f, "{m}"),
            Error::MultipleCommands => {
                write!(
                    f,
                    "cannot insert multiple commands into a prepared statement"
                )
            }
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
            Error::InvalidDatetimeFormat(_) => "22007", // invalid_datetime_format
            Error::DatetimeFieldOverflow(_) => "22008", // datetime_field_overflow
            Error::DivisionByZero => "22012",
            Error::NumericOutOfRange(_) => "22003", // numeric_value_out_of_range
            Error::MultipleCommands => "42601",     // syntax_error, as PostgreSQL reports it
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
    /// `SHOW name` -- one row, one text column named canonically.
    Show(String),
    /// `SET name = value`.
    Set {
        name: String,
        value: String,
    },
    /// `RESET name` / `RESET ALL`.
    Reset(String),
    /// `DEALLOCATE ALL`.
    ///
    /// Only the ALL form. The prepared-statement store belongs to the wire
    /// layer here, not to the planner, so this is a no-op that answers with
    /// PostgreSQL's tag — which is what a client asking to reset its cache
    /// needs. `DEALLOCATE <name>` is still refused rather than treated as a
    /// no-op: PostgreSQL answers 26000 for a name that does not exist, and
    /// silently succeeding there would be a wrong answer.
    DeallocateAll,
    /// `COPY <table> [(cols)] FROM STDIN`.
    CopyFrom(CopyFrom),
    /// `COPY <table> [(cols)] TO STDOUT`.
    CopyTo(CopyFrom),
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
/// One column of a FROM-less SELECT.
///
/// The session-setting variants are resolved at EXECUTION rather than during
/// planning, because the settings live on the connection and the planner is
/// stateless.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstCol {
    Value(Bson),
    /// `current_setting(name [, missing_ok])`.
    CurrentSetting {
        name: String,
        missing_ok: bool,
    },
    /// `set_config(name, value, is_local)` -- sets AND returns.
    SetConfig {
        name: String,
        value: Bson,
        is_local: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectConstant {
    /// (output name, column, declared PostgreSQL type).
    ///
    /// The type is carried EXPLICITLY rather than inferred from the value.
    /// `Describe` arrives before `Bind` and is planned against NULL
    /// placeholders, so inferring from the value typed `$1::int` as `varchar`
    /// and the client then decoded a perfectly good integer as a string.
    pub columns: Vec<(String, ConstCol, String)>,
}

/// `COPY <table> FROM STDIN` or `TO STDOUT`. Text format only.
///
/// Both directions share a shape: a table and an optional column list.
#[derive(Debug, Clone, PartialEq)]
pub struct CopyFrom {
    pub table: String,
    /// Target columns in order; empty means every column in declared order.
    pub columns: Vec<String>,
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
    /// Hidden companion fields to REMOVE. An update to a whole-millisecond
    /// timestamp must clear any remainder the row already carried, or it
    /// reports a time that was never stored.
    pub unset: Vec<String>,
    pub filter: Document,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Delete {
    pub table: String,
    pub filter: Document,
}

/// Name an unsupported node in an error message.
///
/// A bare node kind is the right answer for most of these -- `RowExpr` says
/// what is missing -- but NOT for a function call, where the kind is the same
/// for every function in PostgreSQL's catalog. `FuncCall is not supported yet`
/// was the single most common failure on the psycopg gauge and said nothing
/// about which function to implement; naming it is what makes the remainder
/// rankable.
fn disc(n: &N) -> String {
    if let N::FuncCall(f) = n {
        if let Some(name) = func_name(f) {
            return format!("function {name}()");
        }
    }
    format!("{n:?}")
        .split('(')
        .next()
        .unwrap_or("?")
        .to_string()
}

/// The bare (schema-less) name of a called function, as PostgreSQL prints it.
fn func_name(f: &pg_query::protobuf::FuncCall) -> Option<String> {
    f.funcname
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            N::String(st) => Some(st.sval.clone()),
            _ => None,
        })
        .next_back()
}

/// Split a multi-command string into its individual commands.
///
/// PostgreSQL's SIMPLE query protocol takes any number of commands separated by
/// semicolons and answers with one result per command; only the extended
/// protocol is limited to one. Splitting goes through libpg_query's own parser
/// rather than a scan for `;`, so a semicolon inside a string literal, a dollar-
/// quoted body or a comment does not split the batch.
///
/// Empty commands (a trailing `;`, or `;;`) are dropped: PostgreSQL accepts them
/// and produces no result for them.
pub fn split_statements(sql: &str) -> Result<Vec<String>> {
    let parts = pg_query::split_with_parser(sql).map_err(|e| Error::Parse(e.to_string()))?;
    Ok(parts
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

fn parse_one(sql: &str) -> Result<N> {
    let parsed = pg_query::parse(sql).map_err(|e| Error::Parse(e.to_string()))?;
    let mut stmts = parsed.protobuf.stmts;
    if stmts.len() > 1 {
        return Err(Error::MultipleCommands);
    }
    if stmts.is_empty() {
        return Err(Error::Parse("empty statement".into()));
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
        N::CopyStmt(c) => plan_copy(&c, lookup),
        N::VariableShowStmt(v) => Ok(Statement::Show(v.name.clone())),
        // `DEALLOCATE ALL` carries no name; `DEALLOCATE x` names one.
        N::DeallocateStmt(d) if d.name.is_empty() => Ok(Statement::DeallocateAll),
        N::DeallocateStmt(_) => Err(Error::Unsupported("DEALLOCATE <name>".into())),
        N::VariableSetStmt(v) => plan_set(&v),
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

/// The declared type of a `TypeName`, including its array brackets.
///
/// libpg_query keeps `int[]` as the name `int4` plus a non-empty
/// `array_bounds`, so reading only the name loses the array-ness entirely.
fn type_name_of(t: &pg_query::protobuf::TypeName) -> String {
    let base = type_name(&t.names);
    if t.array_bounds.is_empty() {
        base
    } else {
        format!("{base}[]")
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
                let ty = cd.type_name.as_ref().map(type_name_of).unwrap_or_default();
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
            let column = def.column(col).expect("checked above");
            // PostgreSQL coerces an assigned value to the column's type, so
            // `INSERT INTO t(d) VALUES ('2026-9-1')` STORES `2026-09-01`.
            // Without this the literal went in verbatim and a client reading
            // the column back could not parse it as a date.
            let value = cast_value(const_value(item, params)?, &column.pg_type)?;
            // Resolves the hidden companion (setting or CLEARING it), so a
            // whole-millisecond write cannot inherit stale microseconds.
            let field = column.field();
            let stored = carry_subms(&mut d, &field, value);
            d.insert(field, stored);
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

/// The PostgreSQL type an EXPRESSION declares, read from its shape rather than
/// from the value it happens to produce.
///
/// `Describe` runs before `Bind` and plans against NULL placeholders, so
/// `SELECT $1 + 1` evaluates to NULL at describe time. Typing that column from
/// the value would call it `text`; the operator says `int4`. This is the same
/// trap that made `$1::int` decode as a string.
fn static_type(node: &pg_query::protobuf::Node, value: &Bson) -> String {
    match node.node.as_ref() {
        Some(N::TypeCast(tc)) => tc
            .type_name
            .as_ref()
            .map(type_name_of)
            .unwrap_or_else(|| inferred_type(value).to_string()),
        Some(N::AArrayExpr(_)) => inferred_type(value).to_string(),
        Some(N::AExpr(e)) => {
            let op = operator_name(e).unwrap_or("");
            match op {
                "||" => "text".to_string(),
                "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=" => "bool".to_string(),
                // Arithmetic keeps the value's type when it computed one, and
                // falls back to int4 for the NULL-placeholder case, which is
                // what PostgreSQL reports for `1 + NULL`.
                _ => {
                    if *value == Bson::Null {
                        "int4".to_string()
                    } else {
                        inferred_type(value).to_string()
                    }
                }
            }
        }
        _ => inferred_type(value).to_string(),
    }
}

/// The PostgreSQL type a constant value carries when nothing declares one.
fn inferred_type(v: &Bson) -> &'static str {
    match v {
        Bson::Int32(_) => "int4",
        Bson::Int64(_) => "int8",
        Bson::Double(_) => "float8",
        Bson::Decimal128(_) => "numeric",
        Bson::Array(items) => match items.first() {
            Some(Bson::Int32(_)) | None => "int4[]",
            Some(Bson::Int64(_)) => "int8[]",
            Some(Bson::Double(_)) => "float8[]",
            Some(Bson::Decimal128(_)) => "numeric[]",
            Some(Bson::Boolean(_)) => "bool[]",
            _ => "text[]",
        },
        Bson::Boolean(_) => "bool",
        _ => "text",
    }
}

/// `pg_typeof(x)` — the display name of x's STATIC type.
///
/// Static, not read off the value: `pg_typeof(NULL)` is `unknown`, which no
/// value could report. Lives in one place so the FROM-less target list and the
/// general expression evaluator cannot disagree — `pg_typeof(1)` and
/// `pg_typeof(1)::text` reach it by different routes.
fn pg_typeof(f: &pg_query::protobuf::FuncCall, params: &[Bson]) -> Result<Bson> {
    if f.args.len() != 1 {
        return Err(Error::Parse(
            "function pg_typeof() requires exactly one argument".into(),
        ));
    }
    let arg = &f.args[0];
    let value = const_value(arg, params)?;
    let internal = if value == Bson::Null && matches!(arg.node.as_ref(), Some(N::AConst(_))) {
        "unknown".to_string()
    } else {
        static_type(arg, &value)
    };
    Ok(Bson::String(display_type(&internal)))
}

/// PostgreSQL's DISPLAY name for a type, which is not its internal name.
///
/// `pg_typeof` prints `integer`, not `int4`, and `timestamp without time zone`,
/// not `timestamp` — the spelling a client sees in `\d` and in error messages.
/// Array types print as the element's display name plus `[]`. Measured against
/// PostgreSQL 14 rather than transcribed from memory.
pub fn display_type(internal: &str) -> String {
    if let Some(base) = internal.strip_suffix("[]") {
        return format!("{}[]", display_type(base));
    }
    match internal {
        "int2" | "smallint" => "smallint",
        "int4" | "int" | "integer" => "integer",
        "int8" | "bigint" => "bigint",
        "float4" | "real" => "real",
        "float8" | "double" => "double precision",
        "numeric" | "decimal" => "numeric",
        "bool" | "boolean" => "boolean",
        "varchar" => "character varying",
        "bpchar" | "char" | "character" => "character",
        "time" => "time without time zone",
        "timestamp" => "timestamp without time zone",
        // A bare NULL literal has no type yet: PostgreSQL calls it `unknown`,
        // and resolves it from context when there is any.
        "unknown" => "unknown",
        other => other,
    }
    .to_string()
}

fn plan_select_constant(s: &pg_query::protobuf::SelectStmt, params: &[Bson]) -> Result<Statement> {
    let mut columns: Vec<(String, ConstCol, String)> = Vec::new();
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
                // `pg_typeof(x)` reports the STATIC type of its argument,
                // so it is answered from the same `static_type` the row
                // description uses rather than from the value: `pg_typeof(NULL)`
                // is `unknown`, which no value could tell us.
                if name == "pg_typeof" {
                    columns.push((
                        if rt.name.is_empty() {
                            "pg_typeof".to_string()
                        } else {
                            rt.name.clone()
                        },
                        ConstCol::Value(pg_typeof(f, params)?),
                        "regtype".to_string(),
                    ));
                    continue;
                }
                if let Some(col) = guc_function(&name, f, params)? {
                    let out_name = name.clone();
                    columns.push((
                        if rt.name.is_empty() {
                            out_name
                        } else {
                            rt.name.clone()
                        },
                        col,
                        "text".to_string(),
                    ));
                    continue;
                }
                let v = session_function(&name)
                    .ok_or_else(|| Error::Unsupported(format!("function {name}()")))?;
                let t = inferred_type(&v).to_string();
                (name, ConstCol::Value(v), t)
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
                (name, ConstCol::Value(v), t)
            }
            Some(
                node @ (N::AConst(_)
                | N::ParamRef(_)
                | N::TypeCast(_)
                | N::AExpr(_)
                | N::AArrayExpr(_)),
            ) => {
                let v = const_value(rt.val.as_ref().expect("checked"), params)?;
                let t = static_type(rt.val.as_ref().expect("checked"), &v);
                let _ = node;
                ("?column?".to_string(), ConstCol::Value(v), t)
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
/// Parse a `date` literal and render it canonically.
///
/// PostgreSQL accepts several spellings and always renders `YYYY-MM-DD`
/// (probed 14: `'2026-9-1'` and `'20260901'` both become `2026-09-01`). A
/// malformed value is `22007`; a well-formed one naming a day that does not
/// exist -- `2026-02-30` -- is `22008`, a different code.
fn parse_date(text: &str) -> Result<String> {
    let t = text.trim();
    let parsed = if t.len() == 8 && t.chars().all(|c| c.is_ascii_digit()) {
        NaiveDate::parse_from_str(t, "%Y%m%d")
    } else {
        NaiveDate::parse_from_str(t, "%Y-%m-%d")
    };
    match parsed {
        Ok(d) => Ok(d.format("%Y-%m-%d").to_string()),
        Err(_) => {
            // Distinguish "not a date at all" from "a date that cannot exist".
            let numeric_shape = t
                .split(['-', '/'])
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
            Err(if numeric_shape {
                Error::DatetimeFieldOverflow(format!("date/time field value out of range: \"{t}\""))
            } else {
                Error::InvalidDatetimeFormat(format!("invalid input syntax for type date: \"{t}\""))
            })
        }
    }
}

/// Parse a `time` literal and render it as PostgreSQL does.
///
/// `'12:34'` fills in `:00` seconds; fractional seconds keep only the digits
/// that matter (`12:34:56.5`, not `12:34:56.500000`). An hour past 24 is
/// `22008`, not a parse error.
fn parse_time(text: &str) -> Result<String> {
    let t = text.trim();
    let parsed = NaiveTime::parse_from_str(t, "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(t, "%H:%M"));
    let time = match parsed {
        Ok(v) => v,
        Err(_) => {
            let numeric_shape = t
                .split([':', '.'])
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
            return Err(if numeric_shape {
                Error::DatetimeFieldOverflow(format!("date/time field value out of range: \"{t}\""))
            } else {
                Error::InvalidDatetimeFormat(format!("invalid input syntax for type time: \"{t}\""))
            });
        }
    };
    let micros = time.nanosecond() / 1_000;
    Ok(if micros == 0 {
        time.format("%H:%M:%S").to_string()
    } else {
        // PostgreSQL trims trailing zeros from the fraction.
        let frac = format!("{micros:06}");
        format!("{}.{}", time.format("%H:%M:%S"), frac.trim_end_matches('0'))
    })
}

/// The hidden field carrying a timestamp's sub-millisecond remainder.
///
/// BSON's `Date` is a millisecond count, so a PostgreSQL `timestamp` -- which
/// carries microseconds -- cannot round-trip through one. The Python server
/// keeps the truncated date and stores the lost 0-999 microseconds beside it
/// under this prefix (`src/secantus/sql/subms.py`), so a Mongo client still
/// sees a real BSON date while SQL reads the microseconds back.
///
/// **THE INVARIANT: every write of a timestamp field must SET or CLEAR its
/// companion.** A stale companion is worse than truncation -- it silently
/// reports a time that was never stored. `carry_subms` below makes the clearing
/// explicit so no write path can forget it.
pub const SUBMS_PREFIX: &str = "__us_";

pub fn companion_field(field: &str) -> String {
    format!("{SUBMS_PREFIX}{field}")
}

/// Whether `name` is a hidden remainder field, so `SELECT *` and reflection
/// can skip it.
pub fn is_companion_field(name: &str) -> bool {
    name.starts_with(SUBMS_PREFIX)
}

/// Keys of the composite `cast_value` returns for a timestamp that carries
/// microseconds. The same convention the Python server uses for pipeline
/// accumulators, so a real value cannot be mistaken for one.
pub const COMPOSITE_DATE: &str = "__subms_d";
pub const COMPOSITE_US: &str = "__subms_u";

/// Split a full-precision timestamp into `(millisecond value, 0-999 remainder)`.
fn split_subms(micros_since_epoch: i64) -> (i64, i32) {
    // Rust's `%` truncates toward zero; a pre-epoch timestamp needs the
    // remainder to stay non-negative or the reconstruction moves the time.
    let ms = micros_since_epoch.div_euclid(1000);
    let rem = micros_since_epoch.rem_euclid(1000) as i32;
    (ms, rem)
}

/// Record `value`'s remainder for `field` in `doc`, returning what to store.
///
/// Always resolves the companion -- writing it when there is a remainder and
/// REMOVING it when there is not -- so a field overwritten with a
/// whole-millisecond value cannot keep the previous row's microseconds.
pub fn carry_subms(doc: &mut Document, field: &str, value: Bson) -> Bson {
    let companion = companion_field(field);
    if let Bson::Document(d) = &value {
        if let (Some(date), Some(us)) = (d.get(COMPOSITE_DATE), d.get(COMPOSITE_US)) {
            let rem = us.as_i32().unwrap_or(0);
            if rem != 0 {
                doc.insert(companion, Bson::Int32(rem));
            } else {
                doc.remove(&companion);
            }
            return date.clone();
        }
    }
    doc.remove(&companion);
    value
}

/// Parse a `timestamp` literal to microseconds since the epoch.
///
/// PostgreSQL accepts a bare date (midnight), a `T` separator, and fractional
/// seconds; it renders `YYYY-MM-DD HH:MM:SS` with the fraction only when
/// non-zero (probed 14).
fn parse_timestamp(text: &str) -> Result<i64> {
    let t = text.trim();
    let normalised = t.replacen('T', " ", 1);
    let parsed = NaiveDateTime::parse_from_str(&normalised, "%Y-%m-%d %H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(&normalised, "%Y-%m-%d %H:%M"))
        .or_else(|_| {
            NaiveDate::parse_from_str(&normalised, "%Y-%m-%d")
                .map(|d| d.and_hms_opt(0, 0, 0).expect("midnight is valid"))
        });
    match parsed {
        Ok(dt) => Ok(dt.and_utc().timestamp_micros()),
        Err(_) => {
            let numeric_shape = normalised
                .split([' ', '-', ':', '.'])
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
            Err(if numeric_shape {
                Error::DatetimeFieldOverflow(format!("date/time field value out of range: \"{t}\""))
            } else {
                Error::InvalidDatetimeFormat(format!(
                    "invalid input syntax for type timestamp: \"{t}\""
                ))
            })
        }
    }
}

/// Render stored microseconds as PostgreSQL renders a timestamp.
pub fn render_timestamp(micros: i64) -> String {
    let dt = chrono::DateTime::from_timestamp_micros(micros)
        .map(|d| d.naive_utc())
        .unwrap_or_default();
    let frac = dt.and_utc().timestamp_subsec_micros();
    if frac == 0 {
        dt.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        // The fraction keeps only the digits that matter.
        let s = format!("{frac:06}");
        format!(
            "{}.{}",
            dt.format("%Y-%m-%d %H:%M:%S"),
            s.trim_end_matches('0')
        )
    }
}

/// Parse a `numeric` literal.
///
/// PostgreSQL's `numeric` carries its SCALE as part of the value: `1.50` is not
/// `1.5`, and `SELECT 1.50::numeric::text` answers `'1.50'`. BSON's
/// `Decimal128` preserves scale the same way, so the two agree without any
/// extra bookkeeping (`1.50` -> `1.50`, `2.5000000000000000` round-trips).
///
/// Decimal128 holds 34 significant digits; PostgreSQL's `numeric` is arbitrary
/// precision. A value that will not fit is REFUSED rather than silently
/// rounded -- the same line drawn everywhere else here, because a quietly
/// rounded number is a wrong answer while an error is merely a missing feature.
fn parse_numeric(text: &str) -> Result<Decimal128> {
    let t = text.trim();
    Decimal128::from_str(t).map_err(|_| {
        // Distinguish "too big for us" from "not a number at all": the first is
        // a real PostgreSQL value we cannot represent, the second is the
        // client's mistake.
        let numeric_shape = {
            let body = t.strip_prefix(['+', '-']).unwrap_or(t);
            !body.is_empty()
                && body.chars().all(|c| {
                    c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-'
                })
                && body.chars().any(|c| c.is_ascii_digit())
        };
        if numeric_shape {
            Error::NumericOutOfRange(format!(
                "numeric value out of range: \"{t}\" exceeds the 34 significant \
                 digits this server stores"
            ))
        } else {
            Error::InvalidText(format!("invalid input syntax for type numeric: \"{t}\""))
        }
    })
}

/// Render one array element as PostgreSQL renders it inside `{...}`.
///
/// Quoting is not cosmetic: an element containing a comma, brace, quote,
/// backslash or whitespace -- or one that is empty, or that spells `NULL` --
/// must be quoted, or reading the array back would split it in the wrong place
/// or turn a literal `"NULL"` string into a null.
pub fn render_array_element_text(v: &Bson) -> String {
    render_array_element(v)
}

fn render_array_element(v: &Bson) -> String {
    let raw = match v {
        Bson::Null => return "NULL".to_string(),
        Bson::String(s) => s.clone(),
        Bson::Int32(i) => return i.to_string(),
        Bson::Int64(i) => return i.to_string(),
        Bson::Double(d) => return d.to_string(),
        Bson::Decimal128(d) => return d.to_string(),
        Bson::Boolean(b) => return (if *b { "t" } else { "f" }).to_string(),
        Bson::Array(items) => return render_array(items),
        other => format!("{other:?}"),
    };
    let needs_quotes = raw.is_empty()
        || raw.eq_ignore_ascii_case("null")
        || raw
            .chars()
            .any(|c| matches!(c, ',' | '{' | '}' | '"' | '\\') || c.is_whitespace());
    if needs_quotes {
        let escaped = raw.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        raw
    }
}

/// An array as PostgreSQL's text form: `{1,2,3}`, `{{1,2},{3,4}}`, `{}`.
pub fn render_array(items: &[Bson]) -> String {
    let inner: Vec<String> = items.iter().map(render_array_element).collect();
    format!("{{{}}}", inner.join(","))
}

/// Parse PostgreSQL's array text form into elements, coercing each to
/// `element_type`.
///
/// Handles quoting and nesting; a malformed literal is `22P02`, matching what
/// PostgreSQL answers for text that is not a valid array.
fn parse_array(text: &str, element_type: &str) -> Result<Bson> {
    let t = text.trim();
    if !t.starts_with('{') || !t.ends_with('}') {
        return Err(Error::InvalidText(format!(
            "malformed array literal: \"{t}\""
        )));
    }
    let body = &t[1..t.len() - 1];
    if body.trim().is_empty() {
        return Ok(Bson::Array(Vec::new()));
    }

    let mut items: Vec<Bson> = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut depth = 0usize;
    let mut was_quoted = false;
    for c in body.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => escaped = true,
            '"' => {
                quoted = !quoted;
                was_quoted = true;
            }
            '{' if !quoted => {
                depth += 1;
                cur.push(c);
            }
            '}' if !quoted => {
                depth -= 1;
                cur.push(c);
            }
            ',' if !quoted && depth == 0 => {
                items.push(array_element(&cur, was_quoted, element_type)?);
                cur.clear();
                was_quoted = false;
            }
            _ => cur.push(c),
        }
    }
    if quoted || depth != 0 {
        return Err(Error::InvalidText(format!(
            "malformed array literal: \"{t}\""
        )));
    }
    items.push(array_element(&cur, was_quoted, element_type)?);
    Ok(Bson::Array(items))
}

fn array_element(raw: &str, was_quoted: bool, element_type: &str) -> Result<Bson> {
    let trimmed = raw.trim();
    // An UNQUOTED `NULL` is the null element; a quoted one is the string.
    if !was_quoted && trimmed.eq_ignore_ascii_case("null") {
        return Ok(Bson::Null);
    }
    if trimmed.starts_with('{') {
        return parse_array(trimmed, element_type);
    }
    let text = if was_quoted { raw } else { trimmed };
    cast_value(Bson::String(text.to_string()), element_type)
}

/// A `numeric` rounded to a whole number, as PostgreSQL rounds it.
///
/// PostgreSQL rounds numeric->integer HALF AWAY FROM ZERO (`1.5`->2, `2.5`->3,
/// `-1.5`->-2), which is not what it does for float->integer (that is
/// half-to-even). Measured on PostgreSQL 14.
///
/// Done on the DIGITS rather than through `f64`: a `numeric` carries up to 34
/// significant digits and an f64 has 15, so routing a big one through a float
/// would round twice and silently return a different integer.
fn decimal_to_integer(d: &bson::Decimal128) -> Option<i128> {
    let text = d.to_string();
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1i128, rest),
        None => (1i128, text.as_str()),
    };
    // NaN / Infinity / exponent forms are not whole numbers we can name.
    if digits.chars().any(|c| !c.is_ascii_digit() && c != '.') {
        return None;
    }
    let (int_part, frac) = match digits.split_once('.') {
        Some((i, f)) => (i, f),
        None => (digits, ""),
    };
    let mut magnitude: i128 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().ok()?
    };
    if frac.starts_with(|c: char| ('5'..='9').contains(&c)) {
        magnitude = magnitude.checked_add(1)?;
    }
    Some(sign * magnitude)
}

fn cast_value(value: Bson, target: &str) -> Result<Bson> {
    // A NULL survives every cast; only its declared type changes.
    if value == Bson::Null {
        return Ok(Bson::Null);
    }
    let as_text = |v: &Bson| match v {
        Bson::String(s) => s.clone(),
        // A timestamp renders as PostgreSQL renders it, not as a debug dump.
        // `'...'::timestamp::text` casts through here, and the composite form
        // carries the microseconds the BSON date alone cannot.
        Bson::DateTime(d) => render_timestamp(d.timestamp_millis() * 1000),
        Bson::Document(doc) if doc.contains_key(COMPOSITE_DATE) => {
            let ms = match doc.get(COMPOSITE_DATE) {
                Some(Bson::DateTime(d)) => d.timestamp_millis(),
                _ => 0,
            };
            let us = doc.get(COMPOSITE_US).and_then(|v| v.as_i32()).unwrap_or(0);
            render_timestamp(ms * 1000 + i64::from(us))
        }
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
        // Decimal128's own rendering keeps the scale (`1.50`, not `1.5`).
        Bson::Decimal128(d) => d.to_string(),
        Bson::Array(items) => render_array(items),
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
            // float->integer rounds HALF TO EVEN in PostgreSQL (`2.5` -> 2,
            // `3.5` -> 4), which is NOT the half-away-from-zero rule it uses
            // for numeric->integer. Rust's `round()` is the latter, so using
            // it here answered 3 for `2.5::float8::int`. Measured on PG 14.
            Bson::Double(d) => Ok(Bson::Int32(d.round_ties_even() as i32)),
            Bson::Decimal128(d) => decimal_to_integer(d)
                .and_then(|n| i32::try_from(n).ok())
                .map(Bson::Int32)
                .ok_or_else(|| Error::NumericOutOfRange(format!("integer out of range: \"{d}\""))),
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
            Bson::Double(d) => Ok(Bson::Int64(d.round_ties_even() as i64)),
            Bson::Decimal128(d) => decimal_to_integer(d)
                .and_then(|n| i64::try_from(n).ok())
                .map(Bson::Int64)
                .ok_or_else(|| Error::NumericOutOfRange(format!("bigint out of range: \"{d}\""))),
            Bson::String(s) => s
                .trim()
                .parse::<i64>()
                .map(Bson::Int64)
                .map_err(|_| bad("bigint", &value)),
            _ => Err(bad("bigint", &value)),
        },
        // `numeric` is its own type, not a float: it keeps scale and does not
        // round. Reported as oid 1700, which is what a client reads to decide
        // whether it gets a Decimal or a float.
        t if t.ends_with("[]") => {
            let element = t.trim_end_matches("[]");
            match &value {
                Bson::Array(items) => Ok(Bson::Array(
                    items
                        .iter()
                        .map(|v| cast_value(v.clone(), element))
                        .collect::<Result<Vec<_>>>()?,
                )),
                Bson::String(text) => parse_array(text, element),
                other => Err(Error::InvalidText(format!(
                    "cannot cast {} to {t}",
                    inferred_type(other)
                ))),
            }
        }
        "numeric" | "decimal" => match &value {
            Bson::Decimal128(_) => Ok(value),
            other => Ok(Bson::Decimal128(parse_numeric(&as_text(other))?)),
        },
        "float4" | "float8" | "real" | "double" => match &value {
            Bson::Int32(i) => Ok(Bson::Double(f64::from(*i))),
            Bson::Int64(i) => Ok(Bson::Double(*i as f64)),
            Bson::Double(_) => Ok(value),
            // A decimal literal is `numeric`, so `1.5::float8` arrives here as
            // a Decimal128 rather than a Double. Missing this arm made the
            // cast fail outright once decimal literals stopped being floats.
            Bson::Decimal128(d) => d
                .to_string()
                .parse::<f64>()
                .map(Bson::Double)
                .map_err(|_| bad("double precision", &value)),
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
        // `date` and `time` are stored as their canonical TEXT, matching what
        // the Python server writes -- the two servers share one store, so the
        // representation is a contract, not an implementation choice.
        "date" => Ok(Bson::String(parse_date(&as_text(&value))?)),
        // A timestamp becomes a BSON date plus, when it carries microseconds,
        // a composite the assignment path unwraps into the hidden companion.
        "timestamp" => {
            let micros = parse_timestamp(&as_text(&value))?;
            let (ms, rem) = split_subms(micros);
            let date = Bson::DateTime(bson::DateTime::from_millis(ms));
            Ok(if rem == 0 {
                date
            } else {
                Bson::Document(doc! { COMPOSITE_DATE: date, COMPOSITE_US: rem })
            })
        }
        "time" => Ok(Bson::String(parse_time(&as_text(&value))?)),
        other => Err(Error::Unsupported(format!("a cast to {other}"))),
    }
}

/// Evaluate a constant expression: arithmetic, concatenation, comparison.
///
/// Probed PG 14, and the surprises are all in the corners:
/// `7/2` is **3** (integer division truncates), `5/0` is `22012`, `1+NULL` is
/// NULL, and `'n='||1` coerces the integer to text.
///
/// **Non-integer numeric operands are refused.** PostgreSQL types `1 + 1.5` as
/// `numeric` (oid 1700) with its own scale rules, not `float8`; producing a
/// double would give the right value with the wrong declared type, which is the
/// bug class that made `$1::int` decode as a string. Explicit `::float8` casts
/// work, because then the type IS float8.
fn eval_binary(op: &str, lhs: Bson, rhs: Bson) -> Result<Bson> {
    // NULL propagates through every operator here (PG: `1 + NULL` is NULL).
    if lhs == Bson::Null || rhs == Bson::Null {
        return Ok(Bson::Null);
    }

    if op == "||" {
        let text = |v: &Bson| match v {
            Bson::String(s) => s.clone(),
            Bson::Int32(i) => i.to_string(),
            Bson::Int64(i) => i.to_string(),
            Bson::Boolean(b) => (if *b { "true" } else { "false" }).to_string(),
            Bson::Double(d) => d.to_string(),
            other => format!("{other:?}"),
        };
        return Ok(Bson::String(format!("{}{}", text(&lhs), text(&rhs))));
    }

    if matches!(op, "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=") {
        let ord = compare_constants(&lhs, &rhs)
            .ok_or_else(|| Error::Unsupported(format!("comparing these operands with {op}")))?;
        return Ok(Bson::Boolean(match op {
            "=" => ord == std::cmp::Ordering::Equal,
            "<>" | "!=" => ord != std::cmp::Ordering::Equal,
            "<" => ord == std::cmp::Ordering::Less,
            "<=" => ord != std::cmp::Ordering::Greater,
            ">" => ord == std::cmp::Ordering::Greater,
            _ => ord != std::cmp::Ordering::Less,
        }));
    }

    let ints = |v: &Bson| match v {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        _ => None,
    };
    let (a, b) = match (ints(&lhs), ints(&rhs)) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            // Doubles reach here only from an explicit ::float8 cast, where the
            // declared type really is float8.
            let floats = |v: &Bson| match v {
                Bson::Double(d) => Some(*d),
                Bson::Int32(i) => Some(f64::from(*i)),
                Bson::Int64(i) => Some(*i as f64),
                _ => None,
            };
            let (x, y) = match (floats(&lhs), floats(&rhs)) {
                (Some(x), Some(y)) => (x, y),
                _ => {
                    return Err(Error::Unsupported(format!(
                        "operator {op} on these operands"
                    )))
                }
            };
            return Ok(Bson::Double(match op {
                "+" => x + y,
                "-" => x - y,
                "*" => x * y,
                "/" => {
                    if y == 0.0 {
                        return Err(Error::DivisionByZero);
                    }
                    x / y
                }
                _ => return Err(Error::Unsupported(format!("operator {op}"))),
            }));
        }
    };

    let out = match op {
        "+" => a.checked_add(b),
        "-" => a.checked_sub(b),
        "*" => a.checked_mul(b),
        // Integer division TRUNCATES in PostgreSQL: 7/2 is 3, not 3.5.
        "/" => {
            if b == 0 {
                return Err(Error::DivisionByZero);
            }
            a.checked_div(b)
        }
        "%" => {
            if b == 0 {
                return Err(Error::DivisionByZero);
            }
            a.checked_rem(b)
        }
        other => return Err(Error::Unsupported(format!("operator {other}"))),
    }
    .ok_or_else(|| Error::NumericOutOfRange("integer out of range".into()))?;

    // int4 stays int4 unless it genuinely overflowed into int8 territory.
    Ok(
        if matches!(lhs, Bson::Int64(_))
            || matches!(rhs, Bson::Int64(_))
            || i32::try_from(out).is_err()
        {
            Bson::Int64(out)
        } else {
            Bson::Int32(out as i32)
        },
    )
}

fn compare_constants(a: &Bson, b: &Bson) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Bson::String(x), Bson::String(y)) => Some(x.cmp(y)),
        // PostgreSQL orders arrays element by element, and when one is a
        // prefix of the other the shorter one sorts first.
        (Bson::Array(x), Bson::Array(y)) => {
            for (ex, ey) in x.iter().zip(y.iter()) {
                // Inside an array, PostgreSQL compares NULLs directly rather
                // than through scalar `=`: two NULLs are equal, and a NULL
                // sorts after any non-NULL. (Probed, not assumed - scalar
                // `NULL = NULL` is NULL, so the array rule is the surprise.)
                match (ex == &Bson::Null, ey == &Bson::Null) {
                    (true, true) => continue,
                    (true, false) => return Some(std::cmp::Ordering::Greater),
                    (false, true) => return Some(std::cmp::Ordering::Less),
                    (false, false) => {}
                }
                match compare_constants(ex, ey)? {
                    std::cmp::Ordering::Equal => continue,
                    other => return Some(other),
                }
            }
            Some(x.len().cmp(&y.len()))
        }
        (Bson::Boolean(x), Bson::Boolean(y)) => Some(x.cmp(y)),
        _ => {
            let f = |v: &Bson| match v {
                Bson::Int32(i) => Some(f64::from(*i)),
                Bson::Int64(i) => Some(*i as f64),
                Bson::Double(d) => Some(*d),
                _ => None,
            };
            f(a)?.partial_cmp(&f(b)?)
        }
    }
}

/// `SET` / `RESET`. `SET LOCAL` is treated as `SET`: this server has no
/// statement-scoped settings, and the difference only shows on rollback.
/// `current_setting(...)` / `set_config(...)`, which need connection state and
/// so are resolved at execution rather than here.
fn guc_function(
    name: &str,
    f: &pg_query::protobuf::FuncCall,
    params: &[Bson],
) -> Result<Option<ConstCol>> {
    let text_arg = |i: usize| -> Result<String> {
        match const_value(&f.args[i], params)? {
            Bson::String(s) => Ok(s),
            other => Err(Error::Unsupported(format!(
                "a non-text argument to {name}(): {other:?}"
            ))),
        }
    };
    match name {
        "current_setting" if !f.args.is_empty() && f.args.len() <= 2 => {
            let missing_ok = if f.args.len() == 2 {
                matches!(const_value(&f.args[1], params)?, Bson::Boolean(true))
            } else {
                false
            };
            Ok(Some(ConstCol::CurrentSetting {
                name: text_arg(0)?,
                missing_ok,
            }))
        }
        "set_config" if f.args.len() == 3 => Ok(Some(ConstCol::SetConfig {
            name: text_arg(0)?,
            value: const_value(&f.args[1], params)?,
            is_local: matches!(const_value(&f.args[2], params)?, Bson::Boolean(true)),
        })),
        _ => Ok(None),
    }
}

fn plan_copy(
    c: &pg_query::protobuf::CopyStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Statement> {
    if !c.filename.is_empty() {
        // An empty filename means STDIN/STDOUT, the only endpoints supported:
        // a server-side file would read or write the server's disk.
        return Err(Error::Unsupported(
            "COPY to or from a server-side file".into(),
        ));
    }
    // Only the default text format. A binary or CSV COPY parses differently,
    // and guessing would corrupt the data rather than fail.
    for opt in &c.options {
        if let Some(N::DefElem(d)) = opt.node.as_ref() {
            let name = d.defname.to_ascii_lowercase();
            let value = d
                .arg
                .as_ref()
                .and_then(|a| a.node.as_ref())
                .and_then(|n| match n {
                    N::String(s) => Some(s.sval.to_ascii_lowercase()),
                    _ => None,
                });
            match (name.as_str(), value.as_deref()) {
                ("format", Some("text")) => {}
                ("format", other) => {
                    return Err(Error::Unsupported(format!(
                        "COPY ... FORMAT {}",
                        other.unwrap_or("?")
                    )))
                }
                (other, _) => return Err(Error::Unsupported(format!("COPY option {other}"))),
            }
        }
    }
    let table = c
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| Error::Unsupported("COPY without a table".into()))?;
    let def = lookup(&table).ok_or_else(|| Error::UndefinedTable(table.clone()))?;

    let mut columns = Vec::new();
    for a in &c.attlist {
        let name = match a.node.as_ref() {
            Some(N::String(s)) => s.sval.clone(),
            _ => return Err(Error::Unsupported("this COPY column list".into())),
        };
        if def.column(&name).is_none() {
            return Err(Error::UndefinedColumn(name));
        }
        columns.push(name);
    }
    let spec = CopyFrom { table, columns };
    Ok(if c.is_from {
        Statement::CopyFrom(spec)
    } else {
        Statement::CopyTo(spec)
    })
}

fn plan_set(v: &pg_query::protobuf::VariableSetStmt) -> Result<Statement> {
    // VariableSetKind: Value = 1, Default = 2, Current = 3, Multi = 4, Reset = 5,
    // ResetAll = 6.
    match VariableSetKind::try_from(v.kind) {
        Ok(VariableSetKind::VarReset) => return Ok(Statement::Reset(v.name.clone())),
        Ok(VariableSetKind::VarResetAll) => return Ok(Statement::Reset(String::new())),
        Ok(VariableSetKind::VarSetValue | VariableSetKind::VarSetDefault) => {}
        _ => return Err(Error::Unsupported("this SET form".into())),
    }
    // The value is one or more A_Const / TypeName items; render them as the
    // text PostgreSQL stores, joined by commas (`SET DateStyle = 'ISO','MDY'`).
    let mut parts = Vec::new();
    for a in &v.args {
        let text = match const_value(a, &[])? {
            Bson::String(s) => s,
            Bson::Int32(i) => i.to_string(),
            Bson::Int64(i) => i.to_string(),
            Bson::Double(d) => d.to_string(),
            Bson::Boolean(b) => (if b { "on" } else { "off" }).to_string(),
            Bson::Null => "".to_string(),
            other => format!("{other:?}"),
        };
        parts.push(text);
    }
    Ok(Statement::Set {
        name: v.name.clone(),
        value: parts.join(", "),
    })
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
    let mut unset: Vec<String> = Vec::new();
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
        let value = cast_value(
            const_value(
                rt.val
                    .as_ref()
                    .ok_or_else(|| Error::Parse("SET without a value".into()))?,
                params,
            )?,
            &column.pg_type,
        )?;
        // `carry_subms` writes the companion into a scratch document; a
        // remainder becomes another `$set`, its absence an explicit `$unset`.
        let mut scratch = Document::new();
        let stored = carry_subms(&mut scratch, &field, value);
        let companion = companion_field(&field);
        match scratch.get(&companion) {
            Some(rem) => {
                set.insert(companion, rem.clone());
            }
            None => unset.push(companion),
        }
        set.insert(field, stored);
    }
    if set.is_empty() {
        return Err(Error::Parse("UPDATE without a SET list".into()));
    }
    let filter = match u.where_clause.as_ref() {
        None => Document::new(),
        Some(w) => lower_where(w, &def, params)?,
    };
    Ok(Statement::Update(Update {
        table,
        set,
        unset,
        filter,
    }))
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
        let target = tc.type_name.as_ref().map(type_name_of).unwrap_or_default();
        return cast_value(value, &target);
    }
    if let Some(N::FuncCall(f)) = node.node.as_ref() {
        if func_name(f).as_deref() == Some("pg_typeof") {
            return pg_typeof(f, params);
        }
    }
    if let Some(N::AArrayExpr(a)) = node.node.as_ref() {
        let items = a
            .elements
            .iter()
            .map(|e| const_value(e, params))
            .collect::<Result<Vec<_>>>()?;
        return Ok(Bson::Array(items));
    }
    if let Some(N::AExpr(e)) = node.node.as_ref() {
        if AExprKind::try_from(e.kind) != Ok(AExprKind::AexprOp) {
            return Err(Error::Unsupported("this operator form".into()));
        }
        let op = operator_name(e)?.to_string();
        let rhs = const_value(
            e.rexpr
                .as_ref()
                .ok_or_else(|| Error::Parse("operator with no right operand".into()))?,
            params,
        )?;
        // A missing left operand is unary: `-3`, `+3`.
        let lhs = match e.lexpr.as_ref() {
            Some(l) => const_value(l, params)?,
            None => match op.as_str() {
                "-" => Bson::Int32(0),
                "+" => return Ok(rhs),
                _ => return Err(Error::Unsupported(format!("unary {op}"))),
            },
        };
        return eval_binary(&op, lhs, rhs);
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
                // PostgreSQL types a decimal LITERAL as `numeric`, not
                // float8: `SELECT 1.5` answers oid 1700. Treating it as a
                // double gave the right value under the wrong type.
                Some(a_const::Val::Fval(f)) => Ok(Bson::Decimal128(parse_numeric(&f.fval)?)),
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
