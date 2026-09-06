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

pub mod json;
pub mod pgtypes;
pub mod range;
pub mod scalar;

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
    /// A value that is well-formed but not allowed -> 22000. PostgreSQL puts
    /// a crossed range bound here rather than in the invalid-text class, which
    /// is where a malformed LITERAL goes.
    DataException(String),
    /// An ORDER BY position with no such output column -> 42P10.
    InvalidColumnReference(String),
    /// A parameter that is the wrong VALUE rather than the wrong shape ->
    /// 22023. PostgreSQL distinguishes this from the generic data class.
    InvalidParameter(String),
    /// A function call whose ARGUMENT TYPES match no overload -> 42883.
    ///
    /// Distinct from `Unsupported`: PostgreSQL has no such function either, so
    /// this is the answer a real server gives rather than a gap in this one.
    UndefinedFunction(String),
    /// More than one command where only one is allowed -> 42601.
    ///
    /// PostgreSQL accepts a multi-command string over the SIMPLE query
    /// protocol and refuses it in a prepared statement, so this is a real
    /// error rather than a gap: the extended protocol has one parameter list
    /// and one row description, which two commands cannot share.
    MultipleCommands,
    /// A parameter whose type the client did not declare and context cannot
    /// resolve -> 42P18.
    IndeterminateDatatype(String),
    /// A named object (a type, mostly) that does not exist -> 42704.
    UndefinedObject(String),
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
            Error::DataException(m) | Error::InvalidParameter(m) => write!(f, "{m}"),
            Error::InvalidColumnReference(m)
            | Error::UndefinedFunction(m)
            | Error::IndeterminateDatatype(m)
            | Error::UndefinedObject(m) => write!(f, "{m}"),
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
            Error::DataException(_) => "22000",     // data_exception
            Error::InvalidParameter(_) => "22023",  // invalid_parameter_value
            Error::InvalidColumnReference(_) => "42P10", // invalid_column_reference
            Error::UndefinedFunction(_) => "42883", // undefined_function
            Error::MultipleCommands => "42601",     // syntax_error, as PostgreSQL reports it
            Error::IndeterminateDatatype(_) => "42P18", // indeterminate_datatype
            Error::UndefinedObject(_) => "42704",   // undefined_object
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// What the server should do with one statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    /// `CREATE TABLE`, and whether it was written `IF NOT EXISTS` -- which is
    /// a NO-OP on an existing table rather than the `42P07` a bare one gets.
    CreateTable(TableDef, bool),
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
    /// `DECLARE <name> CURSOR FOR <query>`.
    ///
    /// The inner query is planned here and executed at DECLARE time, because a
    /// cursor over a materialised result is scrollable in both directions --
    /// which PostgreSQL's cursors are, and a forward-only stream would not be.
    DeclareCursor {
        name: String,
        query: Box<Statement>,
    },
    /// `FETCH`/`MOVE`. `is_move` discards the rows and reports only the count,
    /// which is the only difference between the two statements.
    Fetch {
        name: String,
        direction: FetchDirection,
        count: i64,
        is_move: bool,
    },
    /// `CLOSE <name>`.
    CloseCursor(String),
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

/// Which way a `FETCH` or `MOVE` runs, and from where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchDirection {
    Forward,
    Backward,
    /// From the start of the result, one-based.
    Absolute,
    /// From the current position.
    Relative,
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
    /// A set-returning function standing in for a table, as in
    /// `FROM generate_series(1, 5)`. The rows are generated rather than read,
    /// and everything after the source -- ORDER BY, LIMIT, aggregates -- works
    /// on them unchanged, which is why this is a SOURCE on the existing
    /// statement rather than a statement of its own.
    pub series: Option<Series>,
    /// Output columns in order, as (output name, stored field).
    pub columns: Vec<(String, String)>,
    /// A cast to apply to each output column, parallel to `columns`. `None`
    /// almost everywhere; carries `col::regtype::text` and friends, which is
    /// how a client's type-discovery query reads the catalog.
    pub casts: Vec<Option<String>>,
    pub filter: Document,
    pub order: Vec<OrderKey>,
    /// `None` = no LIMIT. `LIMIT 0` is a real limit, not an absent one.
    pub limit: Option<i64>,
    pub offset: i64,
}

/// `generate_series(start, stop [, step])`, the only set-returning function
/// this server has. `step` defaults to 1, may be negative to count down, and
/// may not be zero -- PostgreSQL answers 22023 for that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Series {
    pub start: i64,
    pub stop: i64,
    pub step: i64,
    /// The output column's name: `generate_series` unless the FROM item was
    /// given an alias.
    pub column: String,
}

impl Series {
    pub fn values(&self) -> Vec<i64> {
        let mut out = Vec::new();
        if self.step == 0 {
            return out;
        }
        let mut v = self.start;
        while (self.step > 0 && v <= self.stop) || (self.step < 0 && v >= self.stop) {
            out.push(v);
            match v.checked_add(self.step) {
                Some(next) => v = next,
                None => break,
            }
        }
        out
    }
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
    /// A generated source in place of a table, as for `Select`.
    pub series: Option<Series>,
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

/// Transaction control. Prepared transactions (two-phase commit) are
/// deliberately absent -- they need machinery this server does not have, and
/// pretending would silently lose the semantics a client is relying on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionControl {
    Begin,
    /// `START TRANSACTION`, which does exactly what `BEGIN` does and differs
    /// only in the command tag it answers with.
    Start,
    /// `chain` is `AND CHAIN`: the block ends and another opens immediately,
    /// so the connection is still in a transaction afterwards.
    Commit {
        chain: bool,
    },
    Rollback {
        chain: bool,
    },
    /// `SAVEPOINT <name>`: a point inside the block to come back to.
    Savepoint(String),
    /// `RELEASE [SAVEPOINT] <name>`: destroy it, KEEPING its writes.
    Release(String),
    /// `ROLLBACK TO [SAVEPOINT] <name>`: undo everything written since it, and
    /// leave the savepoint itself open.
    RollbackTo(String),
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

/// The three wire formats a COPY can use. They are not interchangeable: text
/// escapes with backslashes and writes `\N` for NULL, CSV quotes with `"` and
/// writes NULL as an EMPTY unquoted field (an empty string being `""`), and
/// binary is length-prefixed values behind a fixed signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CopyFormat {
    #[default]
    Text,
    Csv,
    Binary,
}

/// `COPY <table> FROM STDIN` or `TO STDOUT`.
///
/// Both directions share a shape: a source and an optional column list. The
/// source is a table, or -- for `TO STDOUT` only -- a query, which PostgreSQL
/// allows and `FROM STDIN` does not.
#[derive(Debug, Clone, PartialEq)]
pub struct CopyFrom {
    pub table: String,
    /// Target columns in order; empty means every column in declared order.
    pub columns: Vec<String>,
    pub format: CopyFormat,
    /// `COPY (SELECT ...) TO STDOUT`. Mutually exclusive with `table`.
    pub query: Option<Box<Statement>>,
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
        N::CopyStmt(c) => plan_copy(&c, lookup, params),
        N::VariableShowStmt(v) => Ok(Statement::Show(v.name.clone())),
        N::DeclareCursorStmt(d) => {
            let inner = match d.query.as_ref().and_then(|q| q.node.as_ref()) {
                Some(N::SelectStmt(sel)) => plan_select(sel, lookup, params)?,
                Some(other) => return Err(Error::Unsupported(disc(other))),
                None => return Err(Error::Parse("DECLARE CURSOR without a query".into())),
            };
            Ok(Statement::DeclareCursor {
                name: d.portalname.clone(),
                query: Box::new(inner),
            })
        }
        N::FetchStmt(f) => {
            use pg_query::protobuf::FetchDirection as Fd;
            // Named enum, not the raw integer: writing these as numbers is how
            // this server once turned every AND into an OR.
            let direction = match Fd::try_from(f.direction) {
                Ok(Fd::FetchForward) => FetchDirection::Forward,
                Ok(Fd::FetchBackward) => FetchDirection::Backward,
                Ok(Fd::FetchAbsolute) => FetchDirection::Absolute,
                Ok(Fd::FetchRelative) => FetchDirection::Relative,
                _ => return Err(Error::Unsupported("this FETCH direction".into())),
            };
            Ok(Statement::Fetch {
                name: f.portalname.clone(),
                direction,
                count: f.how_many,
                is_move: f.ismove,
            })
        }
        N::ClosePortalStmt(c) => Ok(Statement::CloseCursor(c.portalname.clone())),
        // `DEALLOCATE ALL` carries no name; `DEALLOCATE x` names one.
        N::DeallocateStmt(d) if d.name.is_empty() => Ok(Statement::DeallocateAll),
        N::DeallocateStmt(_) => Err(Error::Unsupported("DEALLOCATE <name>".into())),
        N::VariableSetStmt(v) => plan_set(&v),
        N::TransactionStmt(t) => {
            // Named enum, not the wire integer -- twice bitten already.
            match TransactionStmtKind::try_from(t.kind) {
                Ok(TransactionStmtKind::TransStmtBegin) => {
                    Ok(Statement::Transaction(TransactionControl::Begin))
                }
                Ok(TransactionStmtKind::TransStmtStart) => {
                    Ok(Statement::Transaction(TransactionControl::Start))
                }
                Ok(TransactionStmtKind::TransStmtCommit) => {
                    Ok(Statement::Transaction(TransactionControl::Commit {
                        chain: t.chain,
                    }))
                }
                Ok(TransactionStmtKind::TransStmtRollback) => {
                    Ok(Statement::Transaction(TransactionControl::Rollback {
                        chain: t.chain,
                    }))
                }
                // A nested `conn.transaction()` block in any client becomes a
                // savepoint, so these are not an exotic corner.
                Ok(TransactionStmtKind::TransStmtSavepoint) => Ok(Statement::Transaction(
                    TransactionControl::Savepoint(t.savepoint_name.clone()),
                )),
                Ok(TransactionStmtKind::TransStmtRelease) => Ok(Statement::Transaction(
                    TransactionControl::Release(t.savepoint_name.clone()),
                )),
                Ok(TransactionStmtKind::TransStmtRollbackTo) => Ok(Statement::Transaction(
                    TransactionControl::RollbackTo(t.savepoint_name.clone()),
                )),
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
                // `timestamptz` / `timetz` work as casts, literals and bound
                // values, but NOT as a column: they are stored as canonical
                // text, and a timestamptz renders in the SESSION zone, so the
                // stored text is right only for the session that wrote it. A
                // row written under UTC then read under Europe/Rome came back
                // with UTC's wall clock and UTC's offset -- a wrong answer no
                // client could detect. Refuse until the stored form is an
                // instant rather than a rendering of one.
                if matches!(ty.as_str(), "timestamptz" | "timetz") {
                    return Err(Error::Unsupported(format!("a {ty} column")));
                }
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
    Ok(Statement::CreateTable(
        TableDef::new(&table, columns),
        c.if_not_exists,
    ))
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

/// A `FROM generate_series(...)` item, if that is what this FROM clause is.
///
/// The alias renames the column: `AS g` makes it `g`, and `AS g(x)` makes it
/// `x` -- the column alias wins over the table one.
fn series_from_clause(from: &pg_query::protobuf::Node, params: &[Bson]) -> Result<Option<Series>> {
    let Some(N::RangeFunction(rf)) = from.node.as_ref() else {
        return Ok(None);
    };
    // The nesting is a list of lists; the call is the first leaf.
    let call = rf
        .functions
        .iter()
        .flat_map(|f| match f.node.as_ref() {
            Some(N::List(l)) => l.items.clone(),
            _ => vec![f.clone()],
        })
        .find_map(|n| match n.node.as_ref() {
            Some(N::FuncCall(f)) => Some(f.clone()),
            _ => None,
        });
    let Some(call) = call else {
        return Ok(None);
    };
    if func_name(&call).as_deref() != Some("generate_series") {
        return Ok(None);
    }
    let series = series_from_args(&call, params)?;
    // `AS g(x)` -- the column alias, then the table alias, then the default.
    let column = rf
        .alias
        .as_ref()
        .map(|a| {
            a.colnames
                .iter()
                .find_map(|c| match c.node.as_ref() {
                    Some(N::String(st)) => Some(st.sval.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| a.aliasname.clone())
        })
        .unwrap_or_else(|| "generate_series".to_string());
    Ok(Some(Series { column, ..series }))
}

/// `generate_series(start, stop [, step])` from its arguments.
fn series_from_args(f: &pg_query::protobuf::FuncCall, params: &[Bson]) -> Result<Series> {
    if f.args.len() < 2 || f.args.len() > 3 {
        return Err(Error::Parse(
            "function generate_series does not exist with that argument list".into(),
        ));
    }
    // `None` is a NULL argument, which is not an error: PostgreSQL answers a
    // series with any NULL bound with ZERO ROWS.
    let int_at = |i: usize| -> Result<Option<i64>> {
        match const_value(&f.args[i], params)? {
            Bson::Null => Ok(None),
            Bson::Int32(v) => Ok(Some(i64::from(v))),
            Bson::Int64(v) => Ok(Some(v)),
            // A bound parameter sent with no type arrives as TEXT, and
            // PostgreSQL resolves it against the function's own signature --
            // `generate_series(1, $1)` reads `$1` as an integer. Refusing it
            // made every parameterised series fail, which is how clients
            // overwhelmingly write one.
            Bson::String(text) => text.trim().parse::<i64>().map(Some).map_err(|_| {
                Error::InvalidText(format!("invalid input syntax for type integer: \"{text}\""))
            }),
            // There is no `generate_series(int, float8)` in PostgreSQL, and
            // this server used to truncate one instead -- a wrong answer where
            // a real server refuses. (The `numeric` overload DOES exist; that
            // one is a gap, and says so.)
            Bson::Double(_) => Err(Error::UndefinedFunction(
                "function generate_series(integer, double precision) does not exist".into(),
            )),
            other => Err(Error::Unsupported(format!(
                "generate_series over {}",
                inferred_type(&other)
            ))),
        }
    };
    let step = if f.args.len() == 3 {
        int_at(2)?
    } else {
        Some(1)
    };
    if step == Some(0) {
        // 22023 invalid_parameter_value: the argument is a number of the right
        // shape whose VALUE cannot work, which PostgreSQL separates from the
        // generic data class.
        return Err(Error::InvalidParameter(
            "step size cannot equal zero".into(),
        ));
    }
    let (start, stop) = (int_at(0)?, int_at(1)?);
    match (start, stop, step) {
        (Some(start), Some(stop), Some(step)) => Ok(Series {
            start,
            stop,
            step,
            column: "generate_series".to_string(),
        }),
        // A NULL bound: an EMPTY series rather than an error, spelled as a
        // range that generates nothing.
        _ => Ok(Series {
            start: 1,
            stop: 0,
            step: 1,
            column: "generate_series".to_string(),
        }),
    }
}

/// A `SELECT` whose source is a generated series.
///
/// Only the source differs from an ordinary select, so ORDER BY, LIMIT and
/// OFFSET are read exactly as they are elsewhere. A WHERE clause is refused
/// rather than ignored: the filter language here is built against stored
/// columns, and quietly dropping a predicate would answer with rows the client
/// asked to exclude.
fn plan_series_select(
    s: &pg_query::protobuf::SelectStmt,
    series: Series,
    params: &[Bson],
) -> Result<Statement> {
    if s.where_clause.is_some() {
        return Err(Error::Unsupported(
            "a WHERE clause over generate_series".into(),
        ));
    }
    let mut columns: Vec<(String, String)> = Vec::new();
    for t in &s.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            continue;
        };
        match rt.val.as_ref().and_then(|v| v.node.as_ref()) {
            // `*`, or the series column by name -- both mean the one column
            // there is.
            Some(N::ColumnRef(_)) | None => {
                let out = if rt.name.is_empty() {
                    series.column.clone()
                } else {
                    rt.name.clone()
                };
                columns.push((out, series.column.clone()));
            }
            Some(other) => return Err(Error::Unsupported(disc(other))),
        }
    }
    if columns.is_empty() {
        columns.push((series.column.clone(), series.column.clone()));
    }
    // ORDER BY over the one column there is, by name or by position.
    let mut order = Vec::new();
    for item in &s.sort_clause {
        let Some(N::SortBy(sb)) = item.node.as_ref() else {
            return Err(Error::Unsupported("this ORDER BY item".into()));
        };
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
        order.push(OrderKey {
            field: series.column.clone(),
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
    Ok(Statement::Select(Select {
        table: String::new(),
        series: Some(series),
        columns,
        casts: Vec::new(),
        filter: Document::new(),
        order,
        limit,
        offset,
    }))
}

/// The column NAME in a ColumnRef, with any table qualification stripped.
///
/// `t.oid` arrives as two fields; only one table can be in FROM here, so any
/// qualifier names it (or its alias) and the trailing part is the column.
fn column_ref_name(c: &pg_query::protobuf::ColumnRef) -> Option<String> {
    let last = c.fields.last().and_then(|f| f.node.as_ref());
    match last {
        Some(N::String(st)) => Some(st.sval.clone()),
        _ => None,
    }
}

/// A chain of casts over a single column: `oid::regtype::text` is the column
/// `oid` with `["regtype", "text"]` applied outward. Anything that is not a
/// cast-of-(cast-of-...)-column is refused here and handled by the caller.
fn cast_chain_over_column(tc: &pg_query::protobuf::TypeCast) -> Result<(String, Vec<String>)> {
    let ty = tc
        .type_name
        .as_ref()
        .map(type_name_of)
        .ok_or_else(|| Error::Parse("cast with no type".into()))?;
    match tc.arg.as_ref().and_then(|a| a.node.as_ref()) {
        Some(N::ColumnRef(c)) => {
            let name =
                column_ref_name(c).ok_or_else(|| Error::Unsupported("this cast target".into()))?;
            Ok((name, vec![ty]))
        }
        Some(N::TypeCast(inner)) => {
            let (name, mut chain) = cast_chain_over_column(inner)?;
            chain.push(ty);
            Ok((name, chain))
        }
        _ => Err(Error::Unsupported("a cast over this expression".into())),
    }
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
    // A set-returning function stands in for the table.
    if let Some(series) = series_from_clause(&s.from_clause[0], params)? {
        return plan_series_select(s, series, params);
    }
    let table = match s.from_clause[0].node.as_ref() {
        Some(N::RangeVar(r)) => r.relname.clone(),
        Some(other) => return Err(Error::Unsupported(disc(other))),
        None => return Err(Error::Parse("empty FROM".into())),
    };
    let def = lookup(&table).ok_or_else(|| Error::UndefinedTable(table.clone()))?;

    let mut columns: Vec<(String, String)> = Vec::new();
    let mut casts: Vec<Option<String>> = Vec::new();
    for t in &s.target_list {
        let rt = match t.node.as_ref() {
            Some(N::ResTarget(rt)) => rt,
            Some(other) => return Err(Error::Unsupported(disc(other))),
            None => continue,
        };
        match rt.val.as_ref().and_then(|v| v.node.as_ref()) {
            // `col::type [AS out]` -- a cast of a column, which is how a
            // client's type-discovery query reads the catalog
            // (`oid::regtype::text AS regtype`). Chained casts flatten into
            // the last one applied to the innermost column.
            Some(N::TypeCast(tc)) => {
                let (col_name, chain) = cast_chain_over_column(tc)?;
                let field = def
                    .field_of(&col_name)
                    .ok_or_else(|| Error::UndefinedColumn(col_name.clone()))?;
                let out = if rt.name.is_empty() {
                    chain.last().cloned().unwrap_or_else(|| col_name.clone())
                } else {
                    rt.name.clone()
                };
                columns.push((out, field));
                casts.push(Some(chain.join("::")));
                continue;
            }
            Some(N::ColumnRef(c)) => {
                let first = c.fields.first().and_then(|f| f.node.as_ref());
                if matches!(first, Some(N::AStar(_))) {
                    for col in &def.columns {
                        columns.push((col.name.clone(), col.field()));
                        casts.push(None);
                    }
                    continue;
                }
                let name =
                    column_ref_name(c).ok_or_else(|| Error::Unsupported("this target".into()))?;
                let field = def
                    .field_of(&name)
                    .ok_or_else(|| Error::UndefinedColumn(name.clone()))?;
                let out = if rt.name.is_empty() {
                    name
                } else {
                    rt.name.clone()
                };
                columns.push((out, field));
                casts.push(None);
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
        // `ORDER BY 1` is the FIRST OUTPUT COLUMN, not the constant 1 -- an
        // ordinal into the select list, which is why it has to be resolved
        // against `columns` rather than against the table.
        let field = match sb.node.as_ref().and_then(|n| n.node.as_ref()) {
            Some(N::AConst(c)) => {
                let Some(pg_query::protobuf::a_const::Val::Ival(v)) = c.val.as_ref() else {
                    return Err(Error::Unsupported("ORDER BY over an expression".into()));
                };
                let pos = v.ival;
                let idx = usize::try_from(pos)
                    .ok()
                    .filter(|n| *n >= 1 && *n <= columns.len())
                    .ok_or_else(|| {
                        Error::InvalidColumnReference(format!(
                            "ORDER BY position {pos} is not in select list"
                        ))
                    })?;
                columns[idx - 1].1.clone()
            }
            Some(N::ColumnRef(c)) => {
                let col = column_ref_name(c)
                    .ok_or_else(|| Error::Unsupported("this ORDER BY expression".into()))?;
                def.field_of(&col)
                    .ok_or_else(|| Error::UndefinedColumn(col.clone()))?
            }
            // ORDER BY over a computed expression still needs machinery this
            // slice does not have. Refuse, never approximate.
            _ => return Err(Error::Unsupported("ORDER BY over an expression".into())),
        };
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
        series: None,
        table,
        columns,
        casts,
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
    // An aggregate over a generated source. Only the ungrouped forms are
    // supported: there is one column, so grouping by it would make each row its
    // own group, which nothing in the corpus asks for and would be easy to get
    // subtly wrong.
    if let Some(series) = series_from_clause(&s.from_clause[0], params)? {
        if !s.group_clause.is_empty() {
            return Err(Error::Unsupported("GROUP BY over generate_series".into()));
        }
        let mut items = Vec::new();
        let mut select = Vec::new();
        for t in &s.target_list {
            let Some(N::ResTarget(rt)) = t.node.as_ref() else {
                continue;
            };
            let Some(N::FuncCall(f)) = rt.val.as_ref().and_then(|v| v.node.as_ref()) else {
                return Err(Error::Unsupported(
                    "a bare column beside an aggregate over generate_series".into(),
                ));
            };
            let name = func_name(f).unwrap_or_default();
            let func = match name.as_str() {
                "count" => {
                    if f.agg_star {
                        AggFunc::CountStar
                    } else {
                        AggFunc::Count
                    }
                }
                "sum" => AggFunc::Sum,
                "min" => AggFunc::Min,
                "max" => AggFunc::Max,
                other => {
                    return Err(Error::Unsupported(format!("aggregate {other}()")));
                }
            };
            let field = if func == AggFunc::CountStar {
                None
            } else {
                Some(series.column.clone())
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
                // `min`/`max` return the input type, which here is always int4.
                source_type: Some("int4".to_string()),
            });
        }
        return Ok(Statement::Aggregate(Aggregate {
            table: String::new(),
            series: Some(series),
            group_by: Vec::new(),
            items,
            select,
            filter: Document::new(),
            order: Vec::new(),
            limit: None,
            offset: 0,
        }));
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
        series: None,
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
        // An array's type comes from its ELEMENTS' static types, not from the
        // values it happens to hold. The describe path plans with no values at
        // all -- every parameter is NULL there -- so an array typed from its
        // values described `array[$1::float4]` as `text[]`, and the client
        // decoded floats as text because the row description is what it
        // believes.
        Some(N::AArrayExpr(a)) => {
            let items = match value {
                Bson::Array(items) => items.as_slice(),
                _ => &[],
            };
            let mut common: Option<String> = None;
            for (i, element) in a.elements.iter().enumerate() {
                let value = items.get(i).unwrap_or(&Bson::Null);
                // An UNTYPED parameter contributes nothing: PostgreSQL takes
                // the type from the elements that have one.
                if matches!(element.node.as_ref(), Some(N::ParamRef(p))
                    if declared_param_type(usize::try_from(p.number).unwrap_or(0)).is_none())
                {
                    continue;
                }
                // A bare NULL literal contributes nothing either: `array[null,
                // 1]` is `int4[]` on PostgreSQL.
                if matches!(element.node.as_ref(), Some(N::AConst(c)) if c.isnull) {
                    continue;
                }
                let t = static_type(element, value);
                match &common {
                    None => common = Some(t),
                    Some(existing) if *existing == t => {}
                    Some(existing) => match wider_numeric(existing, &t) {
                        Some(wider) => common = Some(wider),
                        // A mix this server has no rule for. The value-derived
                        // answer is what it had before, and PostgreSQL would
                        // coerce the unknown side rather than widen.
                        None => return inferred_type(value).to_string(),
                    },
                }
            }
            match common {
                Some(t) => format!("{t}[]"),
                None => inferred_type(value).to_string(),
            }
        }
        // A PARAMETER's type is the one the client declared, not the one its
        // decoded value suggests: psycopg sends a small integer as `int2`, and
        // `pg_typeof` answers `smallint` where the value alone says `integer`.
        Some(N::ParamRef(p)) => declared_param_type(usize::try_from(p.number).unwrap_or(0))
            .unwrap_or_else(|| inferred_type(value).to_string()),
        // A LITERAL carries its own type in its node. Reading it from the
        // value works until the value is NULL -- `nullif(1,1)` is NULL, and a
        // NULL types as `text`, so the column came back as oid 25 where
        // PostgreSQL says 23. A NULL still has a type; it just cannot report
        // one itself.
        Some(N::AConst(c)) if !c.isnull => match c.val.as_ref() {
            Some(pg_query::protobuf::a_const::Val::Ival(_)) => "int4".to_string(),
            Some(pg_query::protobuf::a_const::Val::Fval(_)) => "numeric".to_string(),
            Some(pg_query::protobuf::a_const::Val::Boolval(_)) => "bool".to_string(),
            _ => inferred_type(value).to_string(),
        },
        Some(N::FuncCall(f)) if func_name(f).as_deref() == Some("to_regtype") => {
            "regtype".to_string()
        }
        // `int4range(1,5)` is an `int4range`, not the text it renders as.
        Some(N::FuncCall(f)) if func_name(f).as_deref().is_some_and(range::is_range_type) => {
            func_name(f).unwrap_or_default()
        }
        // These pick one of their arguments, so they report its type.
        Some(N::CoalesceExpr(_)) | Some(N::MinMaxExpr(_)) => inferred_type(value).to_string(),
        Some(N::AExpr(e)) => {
            // `NULLIF` is an operator node whose operator is `=`, but it
            // answers its LEFT operand, not a boolean. Typing it from the
            // operator made `select nullif(1,2)` report `false` under oid 16.
            if AExprKind::try_from(e.kind) == Ok(AExprKind::AexprNullif) {
                // From the LEFT OPERAND's node, not from the value: when the
                // two are equal the value is NULL, and typing a NULL gives
                // `text` -- so `nullif(1,1)` reported oid 25 where PostgreSQL
                // reports 23. A NULL still has a type; it just cannot tell you
                // what it is.
                return match e.lexpr.as_ref() {
                    Some(l) => static_type(l, value),
                    None => inferred_type(value).to_string(),
                };
            }
            let op = operator_name(e).unwrap_or("");
            // A json operator's result type comes from the operator and the
            // LEFT operand: `->` keeps the json flavour, `->>` is text, `?` is
            // a boolean.
            if matches!(
                op,
                "->" | "->>" | "#>" | "#>>" | "?" | "?|" | "?&" | "@>" | "<@"
            ) {
                if let Some(target) = static_json_type(e.lexpr.as_deref(), &Bson::Null) {
                    return match op {
                        "->" | "#>" => target,
                        "->>" | "#>>" => "text".to_string(),
                        _ => "bool".to_string(),
                    };
                }
            }
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
/// The wider of two NUMERIC types, in PostgreSQL's own order.
///
/// Measured, not assumed: `array[1, 1.5]` is `numeric[]`, `array[1::float4,
/// 1.5]` is `float4[]` (the float wins over the numeric), and `array[1::float4,
/// 1::float8]` is `float8[]`. `None` for anything that is not two numerics,
/// which is a mix this server does not resolve.
fn wider_numeric(a: &str, b: &str) -> Option<String> {
    const LADDER: [&str; 6] = ["int2", "int4", "int8", "numeric", "float4", "float8"];
    let rank = |t: &str| LADDER.iter().position(|x| *x == t);
    let (ra, rb) = (rank(a)?, rank(b)?);
    Some(LADDER[ra.max(rb)].to_string())
}

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
    // A parameter the client left untyped has no type to report: PostgreSQL
    // answers `42P18`, not a guess. `pg_typeof(%s)` with a plain string is the
    // shape that reaches this.
    if let Some(N::ParamRef(p)) = arg.node.as_ref() {
        let n = usize::try_from(p.number).unwrap_or(0);
        if declared_param_type(n).is_none() {
            return Err(Error::IndeterminateDatatype(format!(
                "could not determine data type of parameter ${n}"
            )));
        }
    }
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
        "timestamptz" => "timestamp with time zone",
        "timetz" => "time with time zone",
        "interval" => "interval",
        "json" => "json",
        "jsonb" => "jsonb",
        t if range::is_range_type(t) || range::is_multirange_type(t) => return t.to_string(),
        // A bare NULL literal has no type yet: PostgreSQL calls it `unknown`,
        // and resolves it from context when there is any.
        "unknown" => "unknown",
        other => other,
    }
    .to_string()
}

/// A FROM-less select whose target list is a set-returning function.
///
/// Only the single-target form: `select 1, generate_series(1,3)` repeats the
/// constant across the generated rows, which needs the constants carried into
/// each row, and nothing in the corpus asks for it. Refusing is better than a
/// shape that silently drops a column.
fn plan_select_srf(
    s: &pg_query::protobuf::SelectStmt,
    params: &[Bson],
) -> Result<Option<Statement>> {
    let srf_targets = s
        .target_list
        .iter()
        .filter(|t| match t.node.as_ref() {
            Some(N::ResTarget(rt)) => matches!(
                rt.val.as_ref().and_then(|v| v.node.as_ref()),
                Some(N::FuncCall(f)) if func_name(f).as_deref() == Some("generate_series")
            ),
            _ => false,
        })
        .count();
    if srf_targets == 0 {
        return Ok(None);
    }
    if srf_targets > 1 || s.target_list.len() > 1 {
        return Err(Error::Unsupported(
            "a set-returning function beside another output column".into(),
        ));
    }
    let Some(N::ResTarget(rt)) = s.target_list[0].node.as_ref() else {
        return Ok(None);
    };
    let Some(N::FuncCall(f)) = rt.val.as_ref().and_then(|v| v.node.as_ref()) else {
        return Ok(None);
    };
    let series = series_from_args(f, params)?;
    let column = if rt.name.is_empty() {
        series.column.clone()
    } else {
        rt.name.clone()
    };
    let series = Series {
        column: column.clone(),
        ..series
    };
    let mut order = Vec::new();
    for item in &s.sort_clause {
        let Some(N::SortBy(sb)) = item.node.as_ref() else {
            return Err(Error::Unsupported("this ORDER BY item".into()));
        };
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
        order.push(OrderKey {
            field: column.clone(),
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
    Ok(Some(Statement::Select(Select {
        table: String::new(),
        series: Some(series),
        columns: vec![(column.clone(), column)],
        casts: Vec::new(),
        filter: Document::new(),
        order,
        limit,
        offset,
    })))
}

fn plan_select_constant(s: &pg_query::protobuf::SelectStmt, params: &[Bson]) -> Result<Statement> {
    // A SET-RETURNING function in the target list of a FROM-less select is not
    // a constant at all: `select generate_series(1,3)` is three ROWS. It is
    // planned as an ordinary select over a generated source, which is what the
    // `FROM generate_series(...)` form already produces -- so ORDER BY, LIMIT
    // and OFFSET keep working without a second implementation.
    if let Some(stmt) = plan_select_srf(s, params)? {
        return Ok(stmt);
    }
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
                // `to_regtype` in a bare target list, same shape as above; the
                // value itself is computed by `const_value`, which is also
                // what evaluates it inside a WHERE clause.
                if name == "to_regtype" {
                    columns.push((
                        if rt.name.is_empty() {
                            "to_regtype".to_string()
                        } else {
                            rt.name.clone()
                        },
                        ConstCol::Value(const_value(rt.val.as_ref().expect("checked"), params)?),
                        "regtype".to_string(),
                    ));
                    continue;
                }
                // A scalar built-in in the target list. Without this a BARE
                // `select upper('a')` failed while `select upper('a')::text`
                // worked, because only the cast route goes through
                // `const_value` -- and a probe whose every case carried a cast
                // would never notice.
                if range::is_multirange_type(&name) {
                    let args = f
                        .args
                        .iter()
                        .map(|a| const_value(a, params))
                        .collect::<Result<Vec<_>>>()?;
                    let value =
                        range::render_multirange(&range::multirange_from_args(&args, &name)?);
                    columns.push((
                        if rt.name.is_empty() {
                            name.clone()
                        } else {
                            rt.name.clone()
                        },
                        ConstCol::Value(Bson::String(value)),
                        name.clone(),
                    ));
                    continue;
                }
                if range::is_range_type(&name) {
                    let args = f
                        .args
                        .iter()
                        .map(|a| const_value(a, params))
                        .collect::<Result<Vec<_>>>()?;
                    let literal_flags = !matches!(
                        f.args.get(2).and_then(|a| a.node.as_ref()),
                        Some(N::ParamRef(_))
                    );
                    let value = range::render(&range::from_args(&args, &name, literal_flags)?);
                    columns.push((
                        if rt.name.is_empty() {
                            name.clone()
                        } else {
                            rt.name.clone()
                        },
                        ConstCol::Value(Bson::String(value)),
                        name.clone(),
                    ));
                    continue;
                }
                if scalar::is_scalar(&name) {
                    let args = f
                        .args
                        .iter()
                        .map(|a| const_value(a, params))
                        .collect::<Result<Vec<_>>>()?;
                    if let Some(result) = scalar::call(&name, &args) {
                        let value = result?;
                        let t = inferred_type(&value).to_string();
                        columns.push((
                            if rt.name.is_empty() {
                                name.clone()
                            } else {
                                rt.name.clone()
                            },
                            ConstCol::Value(value),
                            t,
                        ));
                        continue;
                    }
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
                | N::AArrayExpr(_)
                | N::CoalesceExpr(_)
                | N::MinMaxExpr(_)),
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

/// The key of the one-field document a `regtype` VALUE is carried as.
///
/// A regtype is an oid that RENDERS as the type's display name -- two facts no
/// single scalar holds. `select to_regtype('text')` prints `text` under oid
/// 2206, while `where t.oid = to_regtype('text')` compares 25. The document
/// keeps the oid; the render helpers below produce the name.
pub const REGTYPE_KEY: &str = "__regtype_oid";

pub(crate) fn regtype_value(oid: i64) -> Bson {
    let mut d = Document::new();
    d.insert(REGTYPE_KEY, Bson::Int64(oid));
    Bson::Document(d)
}

/// The oid inside a regtype value, or `None` for any other value.
pub fn regtype_oid(v: &Bson) -> Option<i64> {
    match v {
        Bson::Document(d) if d.len() == 1 => d.get_i64(REGTYPE_KEY).ok(),
        _ => None,
    }
}

/// The display rendering of a regtype value: `integer`, not `int4`, exactly as
/// `::regtype::text` prints on PostgreSQL. An ARRAY type renders as its
/// element's display name plus `[]`.
pub fn regtype_text(oid: i64) -> String {
    if let Some(name) = pgtypes::name_of_oid(oid) {
        return display_type(name);
    }
    if let Some((name, _, _)) = pgtypes::BUILTIN_TYPES
        .iter()
        .find(|(_, _, arr)| *arr == oid)
    {
        return format!("{}[]", display_type(name));
    }
    oid.to_string()
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
pub(crate) fn parse_timestamp(text: &str) -> Result<i64> {
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

/// The session's `TimeZone`, resolved to something that can date arithmetic.
///
/// PostgreSQL accepts both a fixed offset (`SET TimeZone TO '+02:00'`) and a
/// named IANA zone (`'Europe/Rome'`), and the two behave differently: a fixed
/// offset is the same all year, a named zone carries a DST rule, so
/// `2026-01-01 12:00` and `2026-07-01 12:00` resolve to different offsets in
/// `Europe/Rome` and to the same one under `'+02:00'`.
///
/// Note PostgreSQL's POSIX sign convention: in `SET TimeZone`, `'+02:00'` means
/// two hours WEST of Greenwich, i.e. UTC-02. Probed, because it is the reverse
/// of the sign in a timestamp literal like `'12:00+02'`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TimeZoneSetting {
    #[default]
    Utc,
    Fixed(chrono::FixedOffset),
    Named(chrono_tz::Tz),
}

impl TimeZoneSetting {
    /// Parse a `TimeZone` GUC value. Unknown names fall back to UTC rather than
    /// failing: the setting is applied when it is SET, and this server has no
    /// business refusing a query because it does not know a zone name.
    pub fn parse(value: &str) -> Self {
        let v = value.trim().trim_matches('\'');
        if v.is_empty() || v.eq_ignore_ascii_case("utc") || v.eq_ignore_ascii_case("gmt") {
            return TimeZoneSetting::Utc;
        }
        if let Some(off) = parse_utc_offset_posix(v) {
            return TimeZoneSetting::Fixed(off);
        }
        match v.parse::<chrono_tz::Tz>() {
            Ok(tz) => TimeZoneSetting::Named(tz),
            Err(_) => TimeZoneSetting::Utc,
        }
    }

    /// The offset in effect at a given instant.
    pub fn offset_at(&self, micros: i64) -> chrono::FixedOffset {
        use chrono::{Offset, TimeZone};
        match self {
            TimeZoneSetting::Utc => chrono::FixedOffset::east_opt(0).expect("zero is valid"),
            TimeZoneSetting::Fixed(off) => *off,
            TimeZoneSetting::Named(tz) => {
                let instant = chrono::DateTime::from_timestamp_micros(micros).unwrap_or_default();
                tz.from_utc_datetime(&instant.naive_utc()).offset().fix()
            }
        }
    }

    /// The offset this zone gives a LOCAL wall-clock reading, which is what a
    /// zone-less literal needs: the instant is not known until the offset is.
    pub fn offset_for_local(&self, naive_micros: i64) -> chrono::FixedOffset {
        use chrono::{Offset, TimeZone};
        match self {
            TimeZoneSetting::Utc => chrono::FixedOffset::east_opt(0).expect("zero is valid"),
            TimeZoneSetting::Fixed(off) => *off,
            TimeZoneSetting::Named(tz) => {
                let naive = chrono::DateTime::from_timestamp_micros(naive_micros)
                    .unwrap_or_default()
                    .naive_utc();
                // A local time can be ambiguous (the hour DST repeats) or absent
                // (the hour it skips). PostgreSQL takes the EARLIER offset for an
                // ambiguous reading, which is what `earliest()` gives.
                tz.from_local_datetime(&naive)
                    .earliest()
                    .or_else(|| tz.from_local_datetime(&naive).latest())
                    .map(|d| d.offset().fix())
                    .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("zero is valid"))
            }
        }
    }
}

/// `SET TimeZone TO '+02:00'` uses the POSIX sign: positive is WEST of
/// Greenwich, so `'+02:00'` is UTC-02. A bare `'02:00'` is the same as `'+02:00'`.
/// This is the OPPOSITE of the sign in `'2026-01-01 12:00+02'`, and was probed
/// rather than assumed.
fn parse_utc_offset_posix(v: &str) -> Option<chrono::FixedOffset> {
    let (sign, rest) = match v.strip_prefix('-') {
        Some(r) => (1i32, r),
        None => (-1i32, v.strip_prefix('+').unwrap_or(v)),
    };
    if rest.is_empty() || !rest.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let (h, m) = match rest.split_once(':') {
        Some((h, m)) => (h.parse::<i32>().ok()?, m.parse::<i32>().ok()?),
        None => (rest.parse::<i32>().ok()?, 0),
    };
    if !(0..=15).contains(&h) || !(0..60).contains(&m) {
        return None;
    }
    chrono::FixedOffset::east_opt(sign * (h * 3600 + m * 60))
}

thread_local! {
    /// The session `TimeZone` in force for the statement being planned.
    ///
    /// `timestamptz` needs it in two places that are deep inside the lowering
    /// code — interpreting a literal that carries no offset, and rendering one
    /// back — and threading a session argument through every intermediate
    /// signature to reach two leaves buys nothing.
    ///
    /// Safe because it is set around a SYNCHRONOUS call: `plan_with_session`
    /// installs it, calls the planner, and restores it, with no `await` in
    /// between, so no other task can observe or inherit it. The Python server
    /// arms its `maxTimeMS` deadline the same way.
    /// The type each `$n` was declared as, when the client declared one.
    static PLAN_PARAM_TYPES: std::cell::RefCell<Vec<Option<String>>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static PLAN_TIMEZONE: std::cell::RefCell<TimeZoneSetting> =
        const { std::cell::RefCell::new(TimeZoneSetting::Utc) };
}

/// Plan a statement with the session's `TimeZone` in force.
pub fn plan_with_session(
    sql: &str,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
    timezone: &TimeZoneSetting,
) -> Result<Statement> {
    plan_with_session_types(sql, lookup, params, &[], timezone)
}

/// As `plan_with_session`, and told what type the client DECLARED for each
/// parameter.
///
/// A parameter's declared type is not recoverable from its decoded value:
/// psycopg sends a small integer as `int2`, and `pg_typeof` has to answer
/// `smallint` rather than the `integer` the value alone suggests. The types
/// ride a thread-local for the same reason the session zone does -- they are
/// needed deep inside the expression walk, and threading them through every
/// signature would touch every planner function to reach two of them.
pub fn plan_with_session_types(
    sql: &str,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
    param_types: &[Option<String>],
    timezone: &TimeZoneSetting,
) -> Result<Statement> {
    let previous = PLAN_TIMEZONE.with(|t| t.replace(timezone.clone()));
    let previous_types = PLAN_PARAM_TYPES.with(|t| t.replace(param_types.to_vec()));
    let out = plan_with_params(sql, lookup, params);
    PLAN_TIMEZONE.with(|t| *t.borrow_mut() = previous);
    PLAN_PARAM_TYPES.with(|t| *t.borrow_mut() = previous_types);
    out
}

/// The declared type of `$n`, when the client gave one.
fn declared_param_type(n: usize) -> Option<String> {
    PLAN_PARAM_TYPES.with(|t| t.borrow().get(n.checked_sub(1)?).cloned().flatten())
}

/// Cast a TEXT representation to a declared type, with the session zone in
/// force. The public door onto `cast_value` for the wire layer, which has text
/// from a client and a declared oid and needs the same value a literal of that
/// type would produce.
/// As `cast_text_to`, for a VALUE that is already typed -- the wire layer's
/// door onto per-column cast chains (`oid::regtype::text`).
pub fn cast_value_with_tz(value: Bson, target: &str, tz: &TimeZoneSetting) -> Result<Bson> {
    let previous = PLAN_TIMEZONE.with(|t| t.replace(tz.clone()));
    let out = cast_value(value, target);
    PLAN_TIMEZONE.with(|t| *t.borrow_mut() = previous);
    out
}

pub fn cast_text_to(text: &str, target: &str, tz: &TimeZoneSetting) -> Result<Bson> {
    let previous = PLAN_TIMEZONE.with(|t| t.replace(tz.clone()));
    let out = cast_value(Bson::String(text.to_string()), target);
    PLAN_TIMEZONE.with(|t| *t.borrow_mut() = previous);
    out
}

fn session_timezone() -> TimeZoneSetting {
    PLAN_TIMEZONE.with(|t| t.borrow().clone())
}

/// Split a trailing UTC offset off a timestamp literal.
///
/// Returns the body and the offset in seconds when one is present. Note the
/// sign here is the ORDINARY one — `'12:00+02'` is two hours EAST — which is
/// the reverse of `SET TimeZone TO '+02:00'`.
fn split_trailing_offset(text: &str) -> (String, Option<i32>) {
    let t = text.trim();
    if let Some(body) = t.strip_suffix(['Z', 'z']) {
        return (body.trim().to_string(), Some(0));
    }
    // Scan from the right for a sign that starts an offset, but not the `-`
    // inside a date: an offset only appears after a time, so require a `:` or a
    // space before it.
    let bytes = t.as_bytes();
    for i in (1..bytes.len()).rev() {
        let c = bytes[i] as char;
        if c != '+' && c != '-' {
            continue;
        }
        let tail = &t[i + 1..];
        if tail.is_empty() || !tail.chars().all(|c| c.is_ascii_digit() || c == ':') {
            continue;
        }
        let head = &t[..i];
        // A date's `-` never follows a `:` or a space-separated time.
        if !head.contains(':') {
            continue;
        }
        // An offset can carry SECONDS -- `+01:02:03` is a real PostgreSQL
        // offset, and several historical zones used one before the hour-based
        // convention settled. An earlier comment here asserted no zone in use
        // carried seconds; the psycopg corpus contains them.
        let mut parts = tail.split(':');
        let h = parts.next().and_then(|v| v.parse::<i32>().ok());
        let m = parts.next().map_or(Some(0), |v| v.parse::<i32>().ok());
        let sec = parts.next().map_or(Some(0), |v| v.parse::<i32>().ok());
        if parts.next().is_some() {
            continue;
        }
        if let (Some(h), Some(m), Some(sec)) = (h, m, sec) {
            if (0..=15).contains(&h) && (0..60).contains(&m) && (0..60).contains(&sec) {
                let sign = if c == '-' { -1 } else { 1 };
                return (
                    head.trim().to_string(),
                    Some(sign * (h * 3600 + m * 60 + sec)),
                );
            }
        }
    }
    (t.to_string(), None)
}

/// A `timestamptz` literal as an absolute instant, in microseconds since the
/// Unix epoch.
///
/// A literal that carries an offset names an instant outright. One that does
/// not is a WALL-CLOCK reading in the session zone, so the offset — and with it
/// the instant — depends on the zone's rule at that local time.
fn parse_timestamptz(text: &str, tz: &TimeZoneSetting) -> Result<i64> {
    let (body, offset) = split_trailing_offset(text);
    let naive = parse_timestamp(&body).map_err(|e| match e {
        Error::InvalidDatetimeFormat(_) => Error::InvalidDatetimeFormat(format!(
            "invalid input syntax for type timestamp with time zone: \"{}\"",
            text.trim()
        )),
        other => other,
    })?;
    let seconds = match offset {
        Some(s) => s,
        None => tz.offset_for_local(naive).local_minus_utc(),
    };
    Ok(naive - i64::from(seconds) * 1_000_000)
}

/// An instant as PostgreSQL renders a `timestamptz`: the wall clock in the
/// session zone, then the offset that zone had at that instant.
pub fn render_timestamptz(micros: i64, tz: &TimeZoneSetting) -> String {
    let offset = tz.offset_at(micros);
    let seconds = offset.local_minus_utc();
    let local = micros + i64::from(seconds) * 1_000_000;
    format!("{}{}", render_timestamp(local), render_offset(seconds))
}

/// PostgreSQL prints an offset as `+02`, widening to `+02:30` for minutes and
/// `+01:02:03` for seconds -- second-precision offsets are real, and appear in
/// the psycopg corpus.
fn render_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let a = seconds.abs();
    let (h, m, s) = (a / 3600, (a % 3600) / 60, a % 60);
    if s != 0 {
        format!("{sign}{h:02}:{m:02}:{s:02}")
    } else if m != 0 {
        format!("{sign}{h:02}:{m:02}")
    } else {
        format!("{sign}{h:02}")
    }
}

/// A `timetz` from its parts: microseconds since midnight, and the offset in
/// seconds EAST of UTC.
pub fn render_timetz(micros: i64, east_seconds: i32) -> String {
    format!(
        "{}{}",
        render_time_from_micros(micros),
        render_offset(east_seconds)
    )
}

/// A `timetz` literal as canonical text: a time plus a fixed offset.
///
/// `timetz` is not an instant — it is a clock reading that remembers which
/// offset it was read in, which is why PostgreSQL itself discourages the type.
/// A literal with no offset takes the session zone's CURRENT offset, so the
/// same literal can mean different things on either side of a DST change.
fn parse_timetz(text: &str, tz: &TimeZoneSetting) -> Result<String> {
    let (body, offset) = split_trailing_offset(text);
    let time = parse_time(&body)?;
    let seconds = match offset {
        Some(s) => s,
        None => {
            // `chrono`'s clock feature is off here on purpose (the planner is
            // otherwise deterministic), so the wall clock comes from std.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_micros() as i64)
                .unwrap_or(0);
            tz.offset_at(now).local_minus_utc()
        }
    };
    Ok(format!("{time}{}", render_offset(seconds)))
}

/// PostgreSQL's `interval`: three INDEPENDENT components.
///
/// Months, days and microseconds are stored separately because they are not
/// convertible without a calendar. A month is 28-31 days depending on where you
/// start, and a day is 23, 24 or 25 hours across a DST boundary — so
/// `'1 mon'` added to January 31st lands on February 28th, and no fixed number
/// of microseconds expresses that.
///
/// COMPARISON, on the other hand, does flatten them: PostgreSQL answers true
/// for `'1 day' = '24:00:00'` and for `'1 mon' = '30 days'`, using 30-day
/// months and 24-hour days. So ordering and equality go through
/// `comparable_micros` while arithmetic keeps the parts apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Interval {
    pub months: i32,
    pub days: i32,
    pub micros: i64,
}

/// Marker keys for an interval carried as a BSON value. The composite shape
/// follows the one a sub-millisecond timestamp already uses.
pub const INTERVAL_MONTHS: &str = "__ivl_mon";
pub const INTERVAL_DAYS: &str = "__ivl_day";
pub const INTERVAL_MICROS: &str = "__ivl_us";

impl Interval {
    /// The value ordering and equality use: 30-day months, 24-hour days.
    /// Probed — `'1 mon'::interval = '30 days'::interval` is true.
    pub fn comparable_micros(&self) -> i128 {
        const DAY: i128 = 86_400 * 1_000_000;
        i128::from(self.months) * 30 * DAY + i128::from(self.days) * DAY + i128::from(self.micros)
    }

    pub fn to_bson(self) -> Bson {
        Bson::Document(bson::doc! {
            INTERVAL_MONTHS: self.months,
            INTERVAL_DAYS: self.days,
            INTERVAL_MICROS: self.micros,
        })
    }

    pub fn from_bson(v: &Bson) -> Option<Interval> {
        let Bson::Document(d) = v else { return None };
        if !d.contains_key(INTERVAL_MONTHS) {
            return None;
        }
        Some(Interval {
            months: d.get(INTERVAL_MONTHS).and_then(|x| x.as_i32())?,
            days: d.get(INTERVAL_DAYS).and_then(|x| x.as_i32())?,
            micros: d.get(INTERVAL_MICROS).and_then(|x| match x {
                Bson::Int64(v) => Some(*v),
                Bson::Int32(v) => Some(i64::from(*v)),
                _ => None,
            })?,
        })
    }
}

/// Render an interval the way PostgreSQL's default `IntervalStyle` does.
///
/// Months split into years and months; each part is pluralised when its value
/// is not exactly 1 — so `-1 day` prints as `-1 days`, which looks like a typo
/// and is what PostgreSQL emits. The time part is `HH:MM:SS`, zero-padded, with
/// hours allowed past 24 (`'25:00:00'` is a valid interval), a trimmed
/// fraction, and its own sign. A wholly zero interval is `00:00:00`.
pub fn render_interval(iv: &Interval) -> String {
    let mut parts: Vec<String> = Vec::new();
    let (years, months) = (iv.months / 12, iv.months % 12);
    let unit = |n: i32, singular: &str| {
        if n == 1 {
            format!("{n} {singular}")
        } else {
            format!("{n} {singular}s")
        }
    };
    if years != 0 {
        parts.push(unit(years, "year"));
    }
    if months != 0 {
        parts.push(unit(months, "mon"));
    }
    if iv.days != 0 {
        parts.push(unit(iv.days, "day"));
    }
    if iv.micros != 0 || parts.is_empty() {
        let neg = iv.micros < 0;
        let a = iv.micros.unsigned_abs();
        let (h, m, sec, frac) = (
            a / 3_600_000_000,
            (a % 3_600_000_000) / 60_000_000,
            (a % 60_000_000) / 1_000_000,
            a % 1_000_000,
        );
        let mut t = format!("{}{h:02}:{m:02}:{sec:02}", if neg { "-" } else { "" });
        if frac != 0 {
            t.push('.');
            t.push_str(format!("{frac:06}").trim_end_matches('0'));
        }
        parts.push(t);
    }
    parts.join(" ")
}

/// An interval VALUE as its canonical text, when the value is one.
pub fn interval_value_text(v: &Bson) -> Option<String> {
    Interval::from_bson(v).map(|iv| render_interval(&iv))
}

/// Parse a PostgreSQL interval literal.
///
/// Three input shapes all reach here: the verbose form (`1 year 2 months`,
/// with the abbreviations `y` / `mon` / `d` / `h` / `m` / `s` and a `week` that
/// becomes 7 days), a bare time (`02:03:04.5`, which may carry its own sign and
/// may exceed 24 hours), and ISO 8601 (`P1Y2M3D`, `PT1H2M3S`). They can be
/// combined — `1 day -02:03:04` is a positive day and a negative time, which is
/// why the components keep independent signs.
fn parse_interval(text: &str) -> Result<Interval> {
    let t = text.trim();
    if t.is_empty() {
        return Err(bad_interval(text));
    }
    if let Some(rest) = t.strip_prefix(['P', 'p']) {
        return parse_iso_interval(rest, text);
    }
    let mut iv = Interval::default();
    let mut pending: Option<f64> = None;
    let mut saw_any = false;

    for token in t.split_whitespace() {
        // A `HH:MM:SS` chunk, possibly signed.
        if token.contains(':') {
            let (sign, body) = match token.strip_prefix('-') {
                Some(b) => (-1i64, b),
                None => (1i64, token.strip_prefix('+').unwrap_or(token)),
            };
            let mut it = body.split(':');
            let h: i64 = it
                .next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| bad_interval(text))?;
            let m: i64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let secs: f64 = it
                .next()
                .map_or(Ok(0.0), |v| v.parse::<f64>())
                .map_err(|_| bad_interval(text))?;
            if it.next().is_some() {
                return Err(bad_interval(text));
            }
            iv.micros +=
                sign * (h * 3_600_000_000 + m * 60_000_000 + (secs * 1_000_000.0).round() as i64);
            saw_any = true;
            continue;
        }
        // A number, or a number glued to its unit (`1d`, `3h`).
        let split = token
            .char_indices()
            .find(|(_, c)| c.is_ascii_alphabetic())
            .map(|(i, _)| i);
        match split {
            Some(0) => {
                // A bare unit, applying to the number before it.
                let n = pending.take().ok_or_else(|| bad_interval(text))?;
                apply_interval_unit(&mut iv, n, token, text)?;
                saw_any = true;
            }
            Some(i) => {
                let n: f64 = token[..i].parse().map_err(|_| bad_interval(text))?;
                if pending.is_some() {
                    return Err(bad_interval(text));
                }
                apply_interval_unit(&mut iv, n, &token[i..], text)?;
                saw_any = true;
            }
            None => {
                if pending.is_some() {
                    return Err(bad_interval(text));
                }
                pending = Some(token.parse().map_err(|_| bad_interval(text))?);
            }
        }
    }
    // A trailing bare number is seconds: `'1'::interval` is one second.
    if let Some(n) = pending {
        iv.micros += (n * 1_000_000.0).round() as i64;
        saw_any = true;
    }
    if !saw_any {
        return Err(bad_interval(text));
    }
    Ok(iv)
}

/// The unit spellings an interval literal may use, singular form.
fn is_interval_unit(u: &str) -> bool {
    matches!(
        u,
        "y" | "yr"
            | "year"
            | "mon"
            | "month"
            | "d"
            | "day"
            | "w"
            | "week"
            | "h"
            | "hr"
            | "hour"
            | "m"
            | "min"
            | "minute"
            | "s"
            | "sec"
            | "second"
            | "ms"
            | "msec"
            | "millisecond"
            | "us"
            | "usec"
            | "microsecond"
    )
}

/// Add `n` of a named unit. A FRACTIONAL month or year spills into days and
/// time the way PostgreSQL does (`1.5 days` is `1 day 12:00:00`), using 30-day
/// months, because a fraction of a month has no calendar meaning.
fn apply_interval_unit(iv: &mut Interval, n: f64, unit: &str, text: &str) -> Result<()> {
    // Strip a plural `s` only when what remains is still a unit. Stripping it
    // unconditionally destroyed `s` (seconds) itself, and turned `ms`
    // (milliseconds) into `m` (minutes) -- a factor of 60,000.
    let lower = unit.to_ascii_lowercase();
    let u = if is_interval_unit(&lower) {
        lower
    } else {
        let singular = lower.trim_end_matches('s').to_string();
        if is_interval_unit(&singular) {
            singular
        } else {
            lower
        }
    };
    let months_per = match u.as_str() {
        "y" | "yr" | "year" => Some(12.0),
        "mon" | "month" => Some(1.0),
        _ => None,
    };
    if let Some(per) = months_per {
        let total = n * per;
        iv.months += total.trunc() as i32;
        let rest_months = total.fract();
        // A leftover fraction of a month becomes days at 30 days per month.
        let days = rest_months * 30.0;
        iv.days += days.trunc() as i32;
        iv.micros += (days.fract() * 86_400_000_000.0).round() as i64;
        return Ok(());
    }
    let micros_per: f64 = match u.as_str() {
        "d" | "day" => {
            iv.days += n.trunc() as i32;
            iv.micros += (n.fract() * 86_400_000_000.0).round() as i64;
            return Ok(());
        }
        "w" | "week" => {
            let days = n * 7.0;
            iv.days += days.trunc() as i32;
            iv.micros += (days.fract() * 86_400_000_000.0).round() as i64;
            return Ok(());
        }
        "h" | "hr" | "hour" => 3_600_000_000.0,
        "m" | "min" | "minute" => 60_000_000.0,
        "s" | "sec" | "second" => 1_000_000.0,
        "ms" | "msec" | "millisecond" => 1_000.0,
        "us" | "usec" | "microsecond" => 1.0,
        _ => return Err(bad_interval(text)),
    };
    iv.micros += (n * micros_per).round() as i64;
    Ok(())
}

/// ISO 8601 durations: `P1Y2M3D`, `PT1H2M3S`, `P1DT2H`. `M` before the `T` is
/// months and after it is minutes, which is the whole reason the `T` is there.
fn parse_iso_interval(rest: &str, text: &str) -> Result<Interval> {
    let mut iv = Interval::default();
    let mut in_time = false;
    let mut number = String::new();
    for c in rest.chars() {
        if c == 'T' || c == 't' {
            in_time = true;
            continue;
        }
        if c.is_ascii_digit() || c == '.' || c == '-' || c == '+' {
            number.push(c);
            continue;
        }
        let n: f64 = number.parse().map_err(|_| bad_interval(text))?;
        number.clear();
        let unit = match (c.to_ascii_uppercase(), in_time) {
            ('Y', _) => "year",
            ('M', false) => "mon",
            ('M', true) => "min",
            ('W', _) => "week",
            ('D', _) => "day",
            ('H', _) => "hour",
            ('S', _) => "sec",
            _ => return Err(bad_interval(text)),
        };
        apply_interval_unit(&mut iv, n, unit, text)?;
    }
    if !number.is_empty() {
        return Err(bad_interval(text));
    }
    Ok(iv)
}

fn bad_interval(text: &str) -> Error {
    Error::InvalidDatetimeFormat(format!(
        "invalid input syntax for type interval: \"{}\"",
        text.trim()
    ))
}

/// Add an interval to an instant, in PostgreSQL's order: months first (with
/// end-of-month CLAMPING, so January 31st plus one month is February 28th),
/// then whole days, then the time.
///
/// The order matters and the clamping is not arithmetic: `2026-01-31 + 1 mon`
/// cannot be February 31st, so PostgreSQL takes the last day of the target
/// month. Probed.
pub fn add_interval_to_micros(micros: i64, iv: &Interval, sign: i64) -> Option<i64> {
    use chrono::Datelike;
    let base = chrono::DateTime::from_timestamp_micros(micros)?.naive_utc();
    let months = i64::from(iv.months) * sign;
    let shifted = if months == 0 {
        base
    } else {
        let total = i64::from(base.year()) * 12 + i64::from(base.month0()) + months;
        let (y, m0) = (total.div_euclid(12), total.rem_euclid(12));
        let year = i32::try_from(y).ok()?;
        let month = u32::try_from(m0).ok()? + 1;
        let last = last_day_of_month(year, month);
        let day = base.day().min(last);
        chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_time(base.time())
    };
    let out = shifted.and_utc().timestamp_micros()
        + sign * (i64::from(iv.days) * 86_400_000_000 + iv.micros);
    Some(out)
}

fn last_day_of_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    chrono::NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| chrono::Datelike::day(&d))
        .unwrap_or(28)
}

/// Days since 2000-01-01 as PostgreSQL's `date` text.
///
/// PostgreSQL's binary `date` is a day count from 2000-01-01, not from the Unix
/// epoch. Rendering it back to canonical text lets a binary parameter take the
/// exact same path through the planner as a text one.
pub fn render_date_from_pg_days(days: i32) -> String {
    // 2000-01-01 is 10957 days after 1970-01-01.
    let unix_days = i64::from(days) + 10_957;
    render_timestamp(unix_days * 86_400 * 1_000_000)
        .split(' ')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Microseconds since midnight as PostgreSQL's `time` text.
pub fn render_time_from_micros(micros: i64) -> String {
    let total_us = micros.rem_euclid(86_400 * 1_000_000);
    let us = total_us % 1_000_000;
    let secs = total_us / 1_000_000;
    let (h, m, sec) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if us == 0 {
        format!("{h:02}:{m:02}:{sec:02}")
    } else {
        format!("{h:02}:{m:02}:{sec:02}.{:06}", us)
            .trim_end_matches('0')
            .to_string()
    }
}

/// Microseconds since 2000-01-01 as PostgreSQL's `timestamp` text.
pub fn render_timestamp_from_pg_micros(micros: i64) -> String {
    // 2000-01-01T00:00:00Z is 946684800 seconds after the Unix epoch.
    render_timestamp(micros + 946_684_800 * 1_000_000)
}

/// A timestamp VALUE as PostgreSQL's text, whether it arrived as a BSON date
/// or as the composite that carries sub-millisecond digits.
///
/// A stored timestamp is reassembled from its column plus a hidden companion
/// field, which the row path already did. A timestamp that is a CONSTANT never
/// touches a row, so it reached the wire as a composite document that the
/// encoder had no arm for — and `select '2026-01-01 12:00'::timestamp`
/// answered NULL while the same value through a column answered correctly.
pub fn timestamp_value_text(v: &Bson) -> Option<String> {
    match v {
        Bson::DateTime(d) => Some(render_timestamp(d.timestamp_millis() * 1000)),
        Bson::Document(doc) if doc.contains_key(COMPOSITE_DATE) => {
            let ms = match doc.get(COMPOSITE_DATE) {
                Some(Bson::DateTime(d)) => d.timestamp_millis(),
                _ => return None,
            };
            let us = doc.get(COMPOSITE_US).and_then(|v| v.as_i32()).unwrap_or(0);
            Some(render_timestamp(ms * 1000 + i64::from(us)))
        }
        _ => None,
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
pub fn parse_numeric(text: &str) -> Result<Decimal128> {
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
/// A `numeric` rendered the way PostgreSQL renders one: PLAIN, never in
/// exponent notation.
///
/// `Decimal128`'s own rendering keeps the scale (`1.50`, not `1.5`), which is
/// part of a numeric's value and must survive -- but it also falls back to
/// `E` notation for large and small magnitudes, and PostgreSQL never does:
/// `1.5e20::numeric` is `150000000000000000000` there and was `1.5E+20` here,
/// in the row, in a `::text` cast and inside an array. A value the client
/// cannot tell from the right one only by luck of `Decimal` comparing equal.
///
/// The non-finite renderings (`NaN`, `Infinity`) carry no exponent and pass
/// through untouched.
pub fn plain_numeric_text(text: &str) -> String {
    let Some(epos) = text.find(['e', 'E']) else {
        return text.to_string();
    };
    let (mantissa, exp) = text.split_at(epos);
    let Ok(exp) = exp[1..].parse::<i32>() else {
        return text.to_string();
    };
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa.strip_prefix('+').unwrap_or(mantissa)),
    };
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i.to_string(), f.to_string()),
        None => (mantissa.to_string(), String::new()),
    };
    if !int_part
        .bytes()
        .chain(frac_part.bytes())
        .all(|b| b.is_ascii_digit())
    {
        return text.to_string();
    }
    let mut digits = format!("{int_part}{frac_part}");
    // Where the point sits, counted from the left of the digit string.
    let mut point = int_part.len() as i32 + exp;
    if point <= 0 {
        // Padding on the left puts the point just after the digits added,
        // which is position 1 whatever it was before.
        digits = format!("{}{digits}", "0".repeat((1 - point) as usize));
        point = 1;
    }
    while (digits.len() as i32) < point {
        digits.push('0');
    }
    let (i, f) = digits.split_at(point as usize);
    if f.is_empty() {
        format!("{sign}{i}")
    } else {
        format!("{sign}{i}.{f}")
    }
}

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
        Bson::Decimal128(d) => return plain_numeric_text(&d.to_string()),
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
    // ASCII whitespace only. Rust's `trim` also strips U+0085 and U+00A0,
    // which PostgreSQL keeps -- so a text array carrying either of them (any
    // corpus that walks the byte range does) round-tripped them to the EMPTY
    // STRING. Silent data loss, and invisible in any test whose alphabet is
    // ASCII.
    let trimmed = raw.trim_matches(|c: char| c.is_ascii_whitespace());
    // An UNQUOTED `NULL` is the null element; a quoted one is the string.
    if !was_quoted && trimmed.eq_ignore_ascii_case("null") {
        return Ok(Bson::Null);
    }
    // Only an UNQUOTED `{` opens a nested array. A quoted one is the string
    // `{`, and treating it as a sub-array made `'{"{"}'::text[]` -- an ordinary
    // element in any corpus that walks the ASCII range -- answer "malformed
    // array literal" for the element rather than returning it.
    if !was_quoted && trimmed.starts_with('{') {
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

/// A value as its PostgreSQL text, which is what `::text` would produce.
pub fn value_text(v: &Bson) -> String {
    render_value_text(v)
}

pub(crate) fn render_value_text(v: &Bson) -> String {
    match cast_value(v.clone(), "text") {
        Ok(Bson::String(s)) => s,
        _ => String::new(),
    }
}

pub(crate) fn cast_value(value: Bson, target: &str) -> Result<Bson> {
    // A NULL survives every cast; only its declared type changes.
    if value == Bson::Null {
        return Ok(Bson::Null);
    }
    // A regtype value casts onward by its two natures: to text as its display
    // NAME, to any integer type as its OID.
    if let Some(oid) = regtype_oid(&value) {
        return match target {
            "regtype" => Ok(value),
            "text" | "varchar" | "name" | "bpchar" => Ok(Bson::String(regtype_text(oid))),
            "int4" | "int8" | "oid" | "integer" | "int" | "bigint" => Ok(Bson::Int64(oid)),
            _ => Err(Error::Unsupported(format!("a regtype cast to {target}"))),
        };
    }
    // `oid` is an UNSIGNED 32-bit integer: a negative literal wraps
    // (`(-1)::oid` is 4294967295), a value past 2^32-1 is out of range, and a
    // non-numeric string is invalid text. All measured on PG 14.
    if target == "oid" {
        let out_of_range = || Error::NumericOutOfRange("OID out of range".into());
        let from_i64 = |v: i64| -> Result<Bson> {
            if !(-(1i64 << 31)..(1i64 << 32)).contains(&v) {
                return Err(out_of_range());
            }
            Ok(Bson::Int64(v.rem_euclid(1i64 << 32)))
        };
        return match value {
            Bson::Int32(v) => from_i64(i64::from(v)),
            Bson::Int64(v) => from_i64(v),
            // A literal past i32 arrives as a decimal; whole ones are still
            // oids (`4294967295::oid`), and anything past 2^32-1 is the same
            // out-of-range PostgreSQL reports.
            Bson::Double(v) if v.fract() == 0.0 => from_i64(v as i64),
            Bson::Decimal128(d) => match d.to_string().parse::<i64>() {
                Ok(v) => from_i64(v),
                Err(_) => Err(out_of_range()),
            },
            Bson::String(text) => match text.trim().parse::<i64>() {
                Ok(v) if (0..(1i64 << 32)).contains(&v) => Ok(Bson::Int64(v)),
                Ok(_) => Err(out_of_range()),
                Err(_) => Err(Error::InvalidText(format!(
                    "invalid input syntax for type oid: \"{}\"",
                    text.trim()
                ))),
            },
            other => Err(Error::Unsupported(format!(
                "a cast of {} to oid",
                bson_kind(&other)
            ))),
        };
    }
    if target == "regtype" {
        return match value {
            Bson::Int32(oid) => Ok(regtype_value(i64::from(oid))),
            Bson::Int64(oid) => Ok(regtype_value(oid)),
            // `'text'::regtype` -- unlike `to_regtype`, an unknown NAME is an
            // error here, which is why psycopg prefers the function.
            Bson::String(name) => match pgtypes::oid_of_name(&name) {
                Some(oid) => Ok(regtype_value(oid)),
                None => Err(Error::UndefinedObject(format!(
                    "type \"{}\" does not exist",
                    name.trim()
                ))),
            },
            other => Err(Error::Unsupported(format!(
                "a cast of {} to regtype",
                bson_kind(&other)
            ))),
        };
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
        // Decimal128's own rendering keeps the scale (`1.50`, not `1.5`), and
        // the expansion drops its exponent notation, which PostgreSQL's
        // numeric output never uses.
        Bson::Decimal128(d) => plain_numeric_text(&d.to_string()),
        Bson::Document(_) if Interval::from_bson(v).is_some() => {
            render_interval(&Interval::from_bson(v).expect("checked"))
        }
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
        // `json` VALIDATES and keeps the text it was given -- whitespace, key
        // order and duplicate keys all survive. `jsonb` parses and stores a
        // structure, so it comes back normalised.
        t if range::is_range_type(t) => {
            let text = as_text(&value);
            Ok(Bson::String(range::render(&range::from_text(&text, t)?)))
        }
        t if range::is_multirange_type(t) => {
            let text = as_text(&value);
            Ok(Bson::String(range::render_multirange(
                &range::multirange_from_text(&text, t)?,
            )))
        }
        "json" | "jsonb" => {
            let text = as_text(&value);
            let parsed = json::parse(&text).map_err(|_| {
                Error::InvalidText(format!(
                    "invalid input syntax for type {target}: \"{}\"",
                    text.trim()
                ))
            })?;
            Ok(Bson::String(if target == "json" {
                text
            } else {
                json::render_jsonb(&parsed)
            }))
        }
        "interval" => match Interval::from_bson(&value) {
            Some(iv) => Ok(iv.to_bson()),
            None => Ok(parse_interval(&as_text(&value))?.to_bson()),
        },
        // `date` and `time` are stored as their canonical TEXT, matching what
        // the Python server writes -- the two servers share one store, so the
        // representation is a contract, not an implementation choice.
        "date" => Ok(Bson::String(parse_date(&as_text(&value))?)),
        // `timestamptz` and `timetz` are stored as their canonical TEXT, the
        // same choice `date` and `time` already make here. A `timestamptz`
        // renders in the SESSION zone, so the text is only canonical for the
        // session that produced it -- fine for an expression or a bound value,
        // which is all this server accepts (a timestamptz COLUMN is refused:
        // storing session-relative text in a row would be a wrong answer for
        // every other session that read it).
        "timestamptz" | "timestamp with time zone" => {
            let tz = session_timezone();
            Ok(Bson::String(render_timestamptz(
                parse_timestamptz(&as_text(&value), &tz)?,
                &tz,
            )))
        }
        "timetz" | "time with time zone" => Ok(Bson::String(parse_timetz(
            &as_text(&value),
            &session_timezone(),
        )?)),
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
/// The instant behind a value that names one: a BSON date, the sub-millisecond
/// composite, or the canonical text a date / timestamp / timestamptz is stored
/// as. Returns `None` for anything that is not a moment in time.
fn instant_micros(v: &Bson) -> Option<i64> {
    match v {
        Bson::DateTime(d) => Some(d.timestamp_millis() * 1000),
        Bson::Document(doc) if doc.contains_key(COMPOSITE_DATE) => {
            let ms = match doc.get(COMPOSITE_DATE) {
                Some(Bson::DateTime(d)) => d.timestamp_millis(),
                _ => return None,
            };
            let us = doc.get(COMPOSITE_US).and_then(|x| x.as_i32()).unwrap_or(0);
            Some(ms * 1000 + i64::from(us))
        }
        Bson::String(t) => {
            let (body, offset) = split_trailing_offset(t);
            let naive = parse_timestamp(&body).ok()?;
            Some(naive - i64::from(offset.unwrap_or(0)) * 1_000_000)
        }
        _ => None,
    }
}

/// Coerce a bare unknown literal to the type of the operand beside it.
///
/// PostgreSQL resolves an `unknown` literal to the OTHER operand's type before
/// it chooses an operator, so that type decides both the parse and the error.
/// It applies to comparison as much as to arithmetic — `interval '1 day' =
/// '1 day'` is true, and `interval '1 day' = '2020-01-01'` is `22007` rather
/// than false.
///
/// The decision is made on the AST NODE, not the value: by this point a
/// `::date` cast is a string too, so only a bare string `AConst` marks a
/// literal whose type is still unresolved.
///
/// `*` and `/` are excluded on purpose — there PostgreSQL resolves the unknown
/// to a NUMBER instead, which is why `interval '1 day' * '2'` is two days.
/// The RANGE type an operand is STATICALLY known to have.
///
/// A range value is carried as its rendered text, so by the time two operands
/// are values there is nothing to tell `'[10,21)'` from any other string. The
/// EXPRESSION still says it: a range constructor names its type, and so does a
/// cast. That is enough to resolve an unknown parameter beside it, which is
/// what PostgreSQL does at analysis time.
fn static_range_type(n: Option<&pg_query::protobuf::Node>) -> Option<String> {
    let named = |name: String| {
        (range::is_range_type(&name) || range::is_multirange_type(&name)).then_some(name)
    };
    match n.and_then(|x| x.node.as_ref()) {
        Some(N::FuncCall(f)) => named(func_name(f)?),
        Some(N::TypeCast(tc)) => named(type_name_of(tc.type_name.as_ref()?)),
        _ => None,
    }
}

/// The JSON operators: `->`, `->>`, `#>`, `#>>` and `?`.
///
/// A json value is carried as its TEXT, so by the time two operands are values
/// there is nothing to tell `{"a": 1}` from any other string -- the left
/// operand's static type is what says this is a json operator at all, exactly
/// as it does for ranges.
///
/// Every lookup that does not apply answers SQL NULL rather than an error: a
/// missing key, an index past the end, a name against an array. That is
/// PostgreSQL's rule and the reason these operators are usable at all.
fn json_operator(op: &str, target: &str, lhs: &Bson, rhs: &Bson) -> Result<Bson> {
    let text = value_text(lhs);
    let parsed = json::parse(&text).map_err(|_| {
        Error::InvalidText(format!(
            "invalid input syntax for type {target}: \"{}\"",
            text.trim()
        ))
    })?;
    // `#>` and `#>>` take a PATH; the others take one key.
    let steps: Vec<String> = if op.starts_with('#') && op != "#" {
        match rhs {
            Bson::Array(items) => items.iter().map(value_text).collect(),
            // A path given as its text form, which is how an unknown-typed
            // parameter arrives.
            other => match cast_value(other.clone(), "text[]")? {
                Bson::Array(items) => items.iter().map(value_text).collect(),
                _ => return Err(Error::Unsupported(format!("a {op} path of this shape"))),
            },
        }
    } else {
        vec![value_text(rhs)]
    };

    if op == "?" {
        return Ok(Bson::Boolean(json::contains_key(&parsed, &steps[0])));
    }
    // `?|` is ANY of the keys, `?&` is ALL of them.
    if op == "?|" || op == "?&" {
        let keys: Vec<String> = match rhs {
            Bson::Array(items) => items.iter().map(value_text).collect(),
            other => match cast_value(other.clone(), "text[]")? {
                Bson::Array(items) => items.iter().map(value_text).collect(),
                _ => return Err(Error::Unsupported(format!("a {op} key list of this shape"))),
            },
        };
        let mut hits = keys.iter().map(|k| json::contains_key(&parsed, k));
        return Ok(Bson::Boolean(if op == "?|" {
            hits.any(|x| x)
        } else {
            hits.all(|x| x)
        }));
    }
    // Containment compares by VALUE, so key order and whitespace do not count.
    if op == "@>" || op == "<@" {
        let other = value_text(rhs);
        let other = json::parse(&other).map_err(|_| {
            Error::InvalidText(format!(
                "invalid input syntax for type {target}: \"{}\"",
                other.trim()
            ))
        })?;
        return Ok(Bson::Boolean(if op == "@>" {
            json::contains(&parsed, &other)
        } else {
            json::contains(&other, &parsed)
        }));
    }
    let mut current = Some(&parsed);
    for step in &steps {
        current = current.and_then(|v| json::member(v, step));
    }
    let Some(found) = current else {
        return Ok(Bson::Null);
    };
    // `->` and `#>` answer a json DOCUMENT; `->>` and `#>>` answer text, in
    // which a json string loses its quotes and a json null becomes SQL NULL.
    if op == "->" || op == "#>" {
        return Ok(Bson::String(if target == "jsonb" {
            json::render_jsonb(found)
        } else {
            json::render_json(found)
        }));
    }
    Ok(match json::as_sql_text(found) {
        Some(text) => Bson::String(text),
        None => Bson::Null,
    })
}

/// The json type an operand is statically known to have, for the operators
/// above. `None` means this is not a json operand and `->` is something else
/// (or nothing this server knows).
fn static_json_type(n: Option<&pg_query::protobuf::Node>, value: &Bson) -> Option<String> {
    let n = n?;
    let t = static_type(n, value);
    (t == "json" || t == "jsonb").then_some(t)
}

fn coerce_unknown_operand(
    e: &pg_query::protobuf::AExpr,
    lhs: Bson,
    rhs: Bson,
    op: &str,
) -> Result<(Bson, Bson)> {
    if !matches!(op, "+" | "-" | "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=") {
        return Ok((lhs, rhs));
    }
    // A bare string literal, or a PARAMETER whose type the client left
    // unspecified -- PostgreSQL resolves both from context. psycopg sends
    // lists and datetimes with an unspecified oid and lets the server infer,
    // so without the ParamRef arm `array[...] = %s` compared an array to the
    // string the parameter decoded to.
    let unresolved =
        |n: Option<&Box<pg_query::protobuf::Node>>| match n.and_then(|x| x.node.as_ref()) {
            Some(N::AConst(c)) => matches!(
                c.val.as_ref(),
                Some(pg_query::protobuf::a_const::Val::Sval(_))
            ),
            Some(N::ParamRef(_)) => true,
            _ => false,
        };
    let bare_string = unresolved;
    let l_bare = bare_string(e.lexpr.as_ref());
    let r_bare = bare_string(e.rexpr.as_ref());
    if l_bare == r_bare {
        // Both unresolved, or neither: nothing to resolve against.
        return Ok((lhs, rhs));
    }
    let (typed, unknown) = if r_bare { (&lhs, &rhs) } else { (&rhs, &lhs) };
    let Bson::String(text) = unknown else {
        return Ok((lhs, rhs));
    };
    let coerced = match typed {
        v if Interval::from_bson(v).is_some() => Some(parse_interval(text)?.to_bson()),
        // A timestamp, as the sub-millisecond composite or a BSON date.
        Bson::Document(d) if d.contains_key(COMPOSITE_DATE) => {
            Some(cast_value(Bson::String(text.clone()), "timestamp")?)
        }
        Bson::DateTime(_) => Some(cast_value(Bson::String(text.clone()), "timestamp")?),
        // An array literal takes the element type from the array beside it.
        Bson::Array(items) => {
            let element = items.first().map(inferred_type).unwrap_or("text");
            Some(cast_value(
                Bson::String(text.clone()),
                &format!("{element}[]"),
            )?)
        }
        _ => None,
    };
    // A range beside an unknown parameter: `int4range(10, 20, '[]') = $1`.
    // Both sides are strings by now, so the type has to come from the
    // expression -- and without it the parameter kept the client's spelling
    // while the constructor had been canonicalised, so two spellings of one
    // range compared UNEQUAL while printing identically.
    let coerced = coerced.or_else(|| {
        let typed_node = if r_bare {
            e.lexpr.as_deref()
        } else {
            e.rexpr.as_deref()
        };
        let name = static_range_type(typed_node)?;
        cast_value(Bson::String(text.clone()), &name).ok()
    });
    let Some(coerced) = coerced else {
        return Ok((lhs, rhs));
    };
    Ok(if r_bare {
        (lhs, coerced)
    } else {
        (coerced, rhs)
    })
}

/// A decimal as an exact (unscaled value, scale) pair.
///
/// PostgreSQL's `numeric` arithmetic is EXACT and carries a defined result
/// scale, so it cannot go through an `f64`: `0.1 + 0.2` is `0.3`, not
/// `0.30000000000000004`, and a 34-digit operand has more digits than a float
/// can hold. `i128` covers the 34 significant digits Decimal128 stores, and an
/// operation that would exceed them is an error rather than a rounding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Dec {
    unscaled: i128,
    scale: u32,
}

fn parse_dec(text: &str) -> Option<Dec> {
    let t = text.trim();
    let (neg, body) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    let digits: String = format!("{int}{frac}");
    let unscaled: i128 = digits.parse().ok()?;
    Some(Dec {
        unscaled: if neg { -unscaled } else { unscaled },
        scale: u32::try_from(frac.len()).ok()?,
    })
}

fn render_dec(d: Dec) -> String {
    if d.scale == 0 {
        return d.unscaled.to_string();
    }
    let neg = d.unscaled < 0;
    let digits = d.unscaled.unsigned_abs().to_string();
    let scale = d.scale as usize;
    let padded = if digits.len() <= scale {
        format!("{}{}", "0".repeat(scale - digits.len() + 1), digits)
    } else {
        digits
    };
    let split = padded.len() - scale;
    format!(
        "{}{}.{}",
        if neg { "-" } else { "" },
        &padded[..split],
        &padded[split..]
    )
}

/// Line two decimals up on the greater scale, exactly.
fn align(a: Dec, b: Dec) -> Option<(i128, i128, u32)> {
    let scale = a.scale.max(b.scale);
    let lift = |d: Dec| -> Option<i128> {
        let steps = scale - d.scale;
        d.unscaled.checked_mul(10i128.checked_pow(steps)?)
    };
    Some((lift(a)?, lift(b)?, scale))
}

/// Exact `+`, `-` and `*` on decimals, with PostgreSQL's result scales:
/// addition and subtraction take `max(s1, s2)`, multiplication takes
/// `s1 + s2`. Both were measured — `1.50 + 1.5` is `3.00` and `1.50 * 1.50` is
/// `2.2500`, so the scale is part of the answer rather than formatting.
///
/// Division is deliberately absent: its result scale depends on the operands'
/// weights in a way that has not been measured here, and guessing it would
/// produce a plausible number of decimal places that is not PostgreSQL's.
pub(crate) fn decimal_arith(op: &str, a: &str, b: &str) -> Option<Result<Bson>> {
    let (x, y) = (parse_dec(a)?, parse_dec(b)?);
    let overflow = || {
        Err(Error::NumericOutOfRange(
            "numeric value out of range: the result exceeds the 34 significant \
             digits this server stores"
                .to_string(),
        ))
    };
    let out = match op {
        "+" | "-" => {
            let Some((xa, ya, scale)) = align(x, y) else {
                return Some(overflow());
            };
            let sum = if op == "+" {
                xa.checked_add(ya)
            } else {
                xa.checked_sub(ya)
            };
            match sum {
                Some(unscaled) => Dec { unscaled, scale },
                None => return Some(overflow()),
            }
        }
        "*" => match x.unscaled.checked_mul(y.unscaled) {
            Some(unscaled) => Dec {
                unscaled,
                scale: x.scale + y.scale,
            },
            None => return Some(overflow()),
        },
        _ => return None,
    };
    let text = render_dec(out);
    Some(match parse_numeric(&text) {
        Ok(d) => Ok(Bson::Decimal128(d)),
        Err(e) => Err(e),
    })
}

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

    // Interval arithmetic, before the numeric paths: an interval is a
    // Document, so it would otherwise fall through to "operator on these
    // operands". Months are added FIRST and clamp to the end of the month --
    // `2026-01-31 + '1 mon'` is `2026-02-28`, which no amount of microseconds
    // could express, and is why an interval keeps three parts.
    if matches!(op, "+" | "-") {
        let sign = if op == "-" { -1 } else { 1 };
        match (Interval::from_bson(&lhs), Interval::from_bson(&rhs)) {
            (Some(a), Some(b)) => {
                return Ok(Interval {
                    months: a.months + sign as i32 * b.months,
                    days: a.days + sign as i32 * b.days,
                    micros: a.micros + sign * b.micros,
                }
                .to_bson());
            }
            (None, Some(iv)) => {
                // <instant or date or timestamp text> +/- interval.
                if let Some(micros) = instant_micros(&lhs) {
                    let out = add_interval_to_micros(micros, &iv, sign).ok_or_else(|| {
                        Error::DatetimeFieldOverflow("timestamp out of range".to_string())
                    })?;
                    return Ok(Bson::String(render_timestamp(out)));
                }
            }
            _ => {}
        }
    }

    // Scaling an interval by a number, in either order. A fractional result
    // SPILLS DOWNWARD -- `'1 mon' * 1.5` is `1 mon 15 days`, not 1.5 months --
    // using 30-day months and 24-hour days, because a fraction of a month has
    // no calendar meaning even though a whole one does.
    if matches!(op, "*" | "/") {
        let numeric = |v: &Bson| match v {
            Bson::Int32(i) => Some(f64::from(*i)),
            Bson::Int64(i) => Some(*i as f64),
            Bson::Double(d) => Some(*d),
            Bson::Decimal128(d) => d.to_string().parse::<f64>().ok(),
            _ => None,
        };
        let scaled = match (Interval::from_bson(&lhs), Interval::from_bson(&rhs)) {
            (Some(iv), None) => numeric(&rhs).map(|f| (iv, f)),
            // `2 * interval '1 day'` is the same interval; division is not
            // commutative, so a number DIVIDED BY an interval is not defined.
            (None, Some(iv)) if op == "*" => numeric(&lhs).map(|f| (iv, f)),
            _ => None,
        };
        if let Some((iv, factor)) = scaled {
            if op == "/" && factor == 0.0 {
                return Err(Error::DivisionByZero);
            }
            let f = if op == "/" { 1.0 / factor } else { factor };
            let months = f64::from(iv.months) * f;
            let whole_months = months.trunc();
            let days = f64::from(iv.days) * f + (months - whole_months) * 30.0;
            let whole_days = days.trunc();
            let micros = iv.micros as f64 * f + (days - whole_days) * 86_400_000_000.0;
            return Ok(Interval {
                months: whole_months as i32,
                days: whole_days as i32,
                micros: micros.round() as i64,
            }
            .to_bson());
        }
    }

    if matches!(op, "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=") {
        let ord = compare_constants(&lhs, &rhs).ok_or_else(|| {
            // Name the OPERAND TYPES. "comparing these operands" was the second
            // largest failure signature on the psycopg gauge and said nothing
            // about which pair to implement -- the same shape as the
            // unnamed `FuncCall` error before it.
            Error::Unsupported(format!(
                "comparing {} with {} using {op}",
                bson_kind(&lhs),
                bson_kind(&rhs)
            ))
        })?;
        return Ok(Bson::Boolean(match op {
            "=" => ord == std::cmp::Ordering::Equal,
            "<>" | "!=" => ord != std::cmp::Ordering::Equal,
            "<" => ord == std::cmp::Ordering::Less,
            "<=" => ord != std::cmp::Ordering::Greater,
            ">" => ord == std::cmp::Ordering::Greater,
            _ => ord != std::cmp::Ordering::Less,
        }));
    }

    // Decimal arithmetic, before the integer and float paths. A decimal
    // literal is `numeric`, so `1.5 + 1.5` arrives here as two Decimal128s --
    // and once decimal literals stopped being floats, every one of these
    // operators refused outright until this arm existed.
    if matches!(op, "+" | "-" | "*")
        && (matches!(lhs, Bson::Decimal128(_)) || matches!(rhs, Bson::Decimal128(_)))
        && !matches!(lhs, Bson::Double(_))
        && !matches!(rhs, Bson::Double(_))
    {
        let text = |v: &Bson| match v {
            Bson::Decimal128(d) => Some(d.to_string()),
            Bson::Int32(i) => Some(i.to_string()),
            Bson::Int64(i) => Some(i.to_string()),
            _ => None,
        };
        if let (Some(a), Some(b)) = (text(&lhs), text(&rhs)) {
            if let Some(result) = decimal_arith(op, &a, &b) {
                return result;
            }
        }
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

/// The BSON shape of a value, for diagnostics. Distinct from `inferred_type`,
/// which answers a PostgreSQL type name and collapses several BSON kinds onto
/// `text` -- which is exactly what hid three different comparison gaps behind
/// one "text vs text" message.
fn bson_kind(v: &Bson) -> &'static str {
    match v {
        Bson::String(_) => "string",
        Bson::Int32(_) => "int32",
        Bson::Int64(_) => "int64",
        Bson::Double(_) => "double",
        Bson::Decimal128(_) => "decimal128",
        Bson::Boolean(_) => "boolean",
        Bson::Array(_) => "array",
        Bson::Binary(_) => "binary",
        Bson::DateTime(_) => "datetime",
        Bson::Null => "null",
        Bson::Document(d) if d.contains_key(INTERVAL_MONTHS) => "interval",
        Bson::Document(_) => "document",
        _ => "other",
    }
}

/// Compare two decimal texts EXACTLY, digit by digit.
///
/// A `numeric` carries up to 34 significant digits and an `f64` holds 15, so
/// routing a comparison through a float can report two different numbers as
/// equal. Scale is not part of equality — `1.50 = 1.5` is true — so trailing
/// zeros are trimmed before comparing.
///
/// PostgreSQL gives NaN a place in a TOTAL order, unlike IEEE: NaN equals
/// itself and sorts ABOVE every number, infinity included. Probed on PG 14.
pub(crate) fn compare_decimal_text(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    let rank = |t: &str| -> Option<i32> {
        let u = t.trim().to_ascii_lowercase();
        match u.as_str() {
            "nan" => Some(2),
            "infinity" | "inf" | "+infinity" | "+inf" => Some(1),
            "-infinity" | "-inf" => Some(-1),
            _ => None,
        }
    };
    match (rank(a), rank(b)) {
        (Some(x), Some(y)) => return Some(x.cmp(&y)),
        (Some(x), None) => {
            return Some(if x > 0 {
                Ordering::Greater
            } else {
                Ordering::Less
            })
        }
        (None, Some(y)) => {
            return Some(if y > 0 {
                Ordering::Less
            } else {
                Ordering::Greater
            })
        }
        (None, None) => {}
    }
    let split = |t: &str| -> Option<(bool, String, String)> {
        let t = t.trim();
        let (neg, body) = match t.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, t.strip_prefix('+').unwrap_or(t)),
        };
        if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return None;
        }
        let (i, f) = body.split_once('.').unwrap_or((body, ""));
        // Leading zeros in the integer part and trailing zeros in the fraction
        // change neither the value nor the ordering.
        let int = i.trim_start_matches('0').to_string();
        let frac = f.trim_end_matches('0').to_string();
        Some((neg, int, frac))
    };
    let (an, ai, af) = split(a)?;
    let (bn, bi, bf) = split(b)?;
    let a_zero = ai.is_empty() && af.is_empty();
    let b_zero = bi.is_empty() && bf.is_empty();
    // Negative zero is zero.
    let an = an && !a_zero;
    let bn = bn && !b_zero;
    if an != bn {
        return Some(if an {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    let magnitude = ai
        .len()
        .cmp(&bi.len())
        .then_with(|| ai.cmp(&bi))
        .then_with(|| {
            let n = af.len().max(bf.len());
            let pad = |f: &str| format!("{f:0<width$}", width = n);
            pad(&af).cmp(&pad(&bf))
        });
    Some(if an { magnitude.reverse() } else { magnitude })
}

pub(crate) fn compare_constants(a: &Bson, b: &Bson) -> Option<std::cmp::Ordering> {
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
        // A regtype compares as its OID: `where t.oid = to_regtype('text')`.
        (a2, b2) if regtype_oid(a2).is_some() || regtype_oid(b2).is_some() => {
            let num = |v: &Bson| -> Option<i64> {
                regtype_oid(v).or(match v {
                    Bson::Int32(x) => Some(i64::from(*x)),
                    Bson::Int64(x) => Some(*x),
                    _ => None,
                })
            };
            Some(num(a2)?.cmp(&num(b2)?))
        }
        // Two instants. Without this a timestamp compared to a timestamp fell
        // through to the numeric path, which has no arm for a BSON date.
        //
        // The composite form counts too: a timestamp carrying sub-millisecond
        // digits is a Document, and comparing two of those found no arm at all
        // -- which surfaced as "comparing timestamp range bounds", because a
        // `tsrange` has to order its own bounds to canonicalise.
        (a2, b2) if instant_micros(a2).is_some() && instant_micros(b2).is_some() => {
            Some(instant_micros(a2)?.cmp(&instant_micros(b2)?))
        }
        // Intervals compare FLATTENED -- 30-day months, 24-hour days -- even
        // though they are stored as three independent parts for arithmetic.
        (Bson::Document(_), Bson::Document(_))
            if Interval::from_bson(a).is_some() && Interval::from_bson(b).is_some() =>
        {
            Some(
                Interval::from_bson(a)?
                    .comparable_micros()
                    .cmp(&Interval::from_bson(b)?.comparable_micros()),
            )
        }
        _ => {
            // Decimals compare on their DIGITS: an f64 holds 15 significant
            // digits where a numeric holds 34, so a float comparison can call
            // two different numbers equal.
            let dec = |v: &Bson| match v {
                Bson::Decimal128(d) => Some(d.to_string()),
                Bson::Int32(i) => Some(i.to_string()),
                Bson::Int64(i) => Some(i.to_string()),
                _ => None,
            };
            // A decimal beside a FLOAT compares as floats: PostgreSQL widens
            // the numeric to float8 for that operator, so the float's own
            // precision governs and the exact path would be the wrong answer.
            let mixed_float = matches!(a, Bson::Double(_)) || matches!(b, Bson::Double(_));
            if (matches!(a, Bson::Decimal128(_)) || matches!(b, Bson::Decimal128(_)))
                && !mixed_float
            {
                return compare_decimal_text(&dec(a)?, &dec(b)?);
            }
            let f = |v: &Bson| match v {
                Bson::Int32(i) => Some(f64::from(*i)),
                Bson::Int64(i) => Some(*i as f64),
                Bson::Double(d) => Some(*d),
                Bson::Decimal128(d) => d.to_string().parse::<f64>().ok(),
                _ => None,
            };
            let (x, y) = (f(a)?, f(b)?);
            // PostgreSQL orders floats TOTALLY: NaN equals itself and sorts
            // above every number, infinity included. IEEE says every NaN
            // comparison is false, which `partial_cmp` faithfully reports as
            // `None` -- and that became "cannot compare" rather than an answer.
            if x.is_nan() || y.is_nan() {
                return Some(match (x.is_nan(), y.is_nan()) {
                    (true, true) => std::cmp::Ordering::Equal,
                    (true, false) => std::cmp::Ordering::Greater,
                    (false, true) => std::cmp::Ordering::Less,
                    (false, false) => unreachable!("one of them is NaN"),
                });
            }
            x.partial_cmp(&y)
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
    params: &[Bson],
) -> Result<Statement> {
    if !c.filename.is_empty() {
        // An empty filename means STDIN/STDOUT, the only endpoints supported:
        // a server-side file would read or write the server's disk.
        return Err(Error::Unsupported(
            "COPY to or from a server-side file".into(),
        ));
    }
    let mut format = CopyFormat::Text;
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
                ("format", Some("text")) => format = CopyFormat::Text,
                ("format", Some("csv")) => format = CopyFormat::Csv,
                ("format", Some("binary")) => format = CopyFormat::Binary,
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
    // `COPY (SELECT ...) TO STDOUT`. PostgreSQL allows a query only when
    // copying OUT -- there is nowhere to put rows copied INTO one.
    if c.relation.is_none() {
        let Some(N::SelectStmt(sel)) = c.query.as_ref().and_then(|q| q.node.as_ref()) else {
            return Err(Error::Unsupported("COPY without a table".into()));
        };
        if c.is_from {
            return Err(Error::Parse(
                "COPY FROM not supported with a query source".into(),
            ));
        }
        let inner = plan_select(sel, lookup, params)?;
        return Ok(Statement::CopyTo(CopyFrom {
            table: String::new(),
            columns: Vec::new(),
            format,
            query: Some(Box::new(inner)),
        }));
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
    let spec = CopyFrom {
        table,
        columns,
        format,
        query: None,
    };
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
        // `to_regtype(name)` resolves a type name to its oid, and NULL --
        // rather than an error -- for a name it does not know. That NULL is
        // the whole reason clients use it over the `::regtype` cast.
        if func_name(f).as_deref() == Some("to_regtype") {
            if f.args.len() != 1 {
                return Err(Error::Parse(
                    "function to_regtype() requires exactly one argument".into(),
                ));
            }
            return Ok(match const_value(&f.args[0], params)? {
                Bson::String(name) => match pgtypes::oid_of_name(&name) {
                    Some(oid) => regtype_value(oid),
                    None => Bson::Null,
                },
                _ => Bson::Null,
            });
        }
        if let Some(name) = func_name(f) {
            // `int4multirange(int4range(1,5), ...)`: each argument is a range.
            if range::is_multirange_type(&name) {
                let args = f
                    .args
                    .iter()
                    .map(|a| const_value(a, params))
                    .collect::<Result<Vec<_>>>()?;
                return Ok(Bson::String(range::render_multirange(
                    &range::multirange_from_args(&args, &name)?,
                )));
            }
            // `int4range(1,5)` and friends: a constructor named for its type.
            if range::is_range_type(&name) {
                let args = f
                    .args
                    .iter()
                    .map(|a| const_value(a, params))
                    .collect::<Result<Vec<_>>>()?;
                // A literal `null` for the flags is an error; the same NULL
                // from a not-yet-bound parameter is not, since Describe runs
                // before Bind.
                let literal_flags = !matches!(
                    f.args.get(2).and_then(|a| a.node.as_ref()),
                    Some(N::ParamRef(_))
                );
                return Ok(Bson::String(range::render(&range::from_args(
                    &args,
                    &name,
                    literal_flags,
                )?)));
            }
            if scalar::is_scalar(&name) {
                let args = f
                    .args
                    .iter()
                    .map(|a| const_value(a, params))
                    .collect::<Result<Vec<_>>>()?;
                if let Some(result) = scalar::call(&name, &args) {
                    return result;
                }
            }
        }
    }
    // `COALESCE`, `NULLIF` and `GREATEST` / `LEAST` are their own AST nodes
    // rather than function calls, so they arrive here separately even though a
    // user writes them like functions.
    if let Some(N::CoalesceExpr(c)) = node.node.as_ref() {
        for a in &c.args {
            let v = const_value(a, params)?;
            if v != Bson::Null {
                return Ok(v);
            }
        }
        return Ok(Bson::Null);
    }
    if let Some(N::MinMaxExpr(m)) = node.node.as_ref() {
        let args = m
            .args
            .iter()
            .map(|a| const_value(a, params))
            .collect::<Result<Vec<_>>>()?;
        let name = if m.op == pg_query::protobuf::MinMaxOp::IsGreatest as i32 {
            "greatest"
        } else {
            "least"
        };
        return scalar::call(name, &args).expect("greatest/least are scalars");
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
        // `NULLIF(a, b)` is an operator node, not a function call: it is `a`
        // unless the two are equal, in which case it is NULL.
        if AExprKind::try_from(e.kind) == Ok(AExprKind::AexprNullif) {
            let lhs = match e.lexpr.as_ref() {
                Some(l) => const_value(l, params)?,
                None => return Err(Error::Parse("NULLIF without a left operand".into())),
            };
            let rhs = match e.rexpr.as_ref() {
                Some(r) => const_value(r, params)?,
                None => return Err(Error::Parse("NULLIF without a right operand".into())),
            };
            return Ok(
                if lhs != Bson::Null
                    && rhs != Bson::Null
                    && compare_constants(&lhs, &rhs) == Some(std::cmp::Ordering::Equal)
                {
                    Bson::Null
                } else {
                    lhs
                },
            );
        }
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
        // The JSON operators need the left operand's STATIC type, which the
        // values no longer carry.
        if matches!(
            op.as_str(),
            "->" | "->>" | "#>" | "#>>" | "?" | "?|" | "?&" | "@>" | "<@"
        ) {
            if let Some(target) = static_json_type(e.lexpr.as_deref(), &lhs) {
                if lhs == Bson::Null || rhs == Bson::Null {
                    return Ok(Bson::Null);
                }
                return json_operator(&op, &target, &lhs, &rhs);
            }
        }
        // A bare UNKNOWN literal takes the type of the operand beside it,
        // which decides both the parse and the error.
        let (lhs, rhs) = coerce_unknown_operand(e, lhs, rhs, &op)?;
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
        Some(N::ColumnRef(c)) => {
            column_ref_name(c).ok_or_else(|| Error::Unsupported("this column reference".into()))?
        }
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
    // A regtype value filters by its OID -- the stored column is a number.
    let value = match regtype_oid(&value) {
        Some(oid) => Bson::Int64(oid),
        None => value,
    };

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
