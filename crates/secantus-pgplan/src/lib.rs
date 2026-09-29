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

pub mod acl;
pub mod arrays;
pub mod bytea;
pub mod correlated;
pub mod datetime;
pub mod escape_strings;
pub mod formatting;
pub mod fts;
pub mod geo;
pub mod geometry;
pub mod hstore;
pub mod joins;
pub use correlated::set_user_functions;
pub use correlated::{
    with_correlated_runner, with_function_hook, with_sequence_hook, FnResult, UserFn,
};
pub mod json;
pub mod jsonpath;
pub mod net;
pub mod numeric;
pub mod pgtypes;
pub mod range;
pub mod scalar;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use pg_query::protobuf::node::Node as N;
use pg_query::protobuf::{
    a_const, AExpr, AExprKind, BoolExprType, DropBehavior, NullTestType, ObjectType,
    OverridingKind, SortByDir, SortByNulls, SubLinkType, TransactionStmtKind, VariableSetKind,
};
use secantus_pgcatalog::{CheckConstraint, Column, ForeignKey, TableDef, UniqueConstraint};

pub use numeric::{
    canonical_numeric_text, compare_decimal_text, is_numeric, is_wide_numeric, numeric_bson,
    numeric_text, parse_numeric, plain_numeric_text, WIDE_NUMERIC_KEY, WIDE_NUMERIC_SORT_KEY,
};
use numeric::{decimal_arith, negate_numeric_text};

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The statement did not parse. Carries libpg_query's own message, which
    /// is PostgreSQL's.
    Parse(String),
    /// Parsed, but this server cannot lower it yet -> 0A000.
    Unsupported(String),
    /// A column the table does not have -> 42703.
    UndefinedColumn(String),
    /// A field a composite or record value does not have -> 42703. Carries
    /// the whole message, because PostgreSQL words it by the SOURCE type:
    /// `column "x" not found in data type t` for a named composite and
    /// `could not identify column "x" in record data type` for a bare record.
    UndefinedField(String),
    /// A table the catalog does not have -> 42P01.
    UndefinedTable(String),
    /// A name that cannot be an identifier at all (`'a b'::regclass`, an
    /// empty name) -> 42602.
    InvalidName(String),
    /// A bare column beside an aggregate, not in GROUP BY -> 42803.
    Grouping(String),
    /// An `ON CONFLICT` target matching no unique constraint -> 42P10.
    /// Carries the whole message: PostgreSQL words it differently for a
    /// column list and for a named constraint that does not exist.
    NoArbiter(String),
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
    /// A NULL where the operation cannot take one -> 22004
    /// (`format('%I', NULL)`).
    NullValueNotAllowed(String),
    /// A character the target representation cannot hold -> 22P05
    /// (`'"\u0000"'::jsonb`: jsonb stores decoded text and text cannot
    /// carry a NUL).
    UntranslatableCharacter(String),
    /// A function call whose ARGUMENT TYPES match no overload -> 42883.
    ///
    /// Distinct from `Unsupported`: PostgreSQL has no such function either, so
    /// this is the answer a real server gives rather than a gap in this one.
    UndefinedFunction(String),
    /// An error PostgreSQL raises with its own SQLSTATE and message, where no
    /// dedicated variant exists: `(sqlstate, message)`.
    Sqlstate(&'static str, String),
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
    /// A substring / overlay offset outside what the operation allows -> 22011
    /// (substring_error). Its own class, not 22P02: PostgreSQL does not treat
    /// `overlay('abc' placing 'X' from 0)` as a malformed VALUE, and a client
    /// matching on the class would mis-route it.
    SubstringError(String),
    /// A malformed regular expression -> 2201B. Its own class, not 22P02: the
    /// PATTERN is broken, not the value being matched.
    InvalidRegex(String),
    /// A byte/array index out of range -> 2202E (array_subscript_error).
    ArraySubscript(String),
    /// An operand of the wrong type where the SYNTAX is fine -> 42804
    /// (datatype_mismatch): `1 AND 2`.
    DatatypeMismatch(String),
    /// A cast between two types PostgreSQL has no cast for -> 42846
    /// (cannot_coerce): `5::box`, `'(1,2),(3,4)'::box::float8`.
    CannotCoerce(String),
    /// A FOREIGN KEY whose referenced columns carry no unique constraint ->
    /// 42830 (invalid_foreign_key).
    InvalidForeignKey(String),
    /// Something PostgreSQL itself refuses as unsupported -> 0A000, worded
    /// as it words it. Distinct from `Unsupported`, which is a gap in THIS
    /// server and says so.
    FeatureNotSupported(String),
    /// A subquery used as a value that returned more than one row -> 21000
    /// (cardinality_violation).
    CardinalityViolation(String),
    /// Two columns of one name in a CREATE TABLE -> 42701 (duplicate_column).
    DuplicateColumn(String),
    /// A window function used where one is not allowed, or a frame clause
    /// PostgreSQL itself rejects -> 42P20 (windowing_error). Its own class,
    /// not 0A000: PostgreSQL refuses these too, so they are the answer a real
    /// server gives rather than a gap in this one.
    Windowing(String),
    /// `ntile(0)` -> 22014 (invalid_argument_for_ntile_function).
    /// PostgreSQL gives the ntile and nth_value argument checks their OWN
    /// class rather than the generic 22023 an invalid parameter gets.
    InvalidNtileArgument(String),
    /// An error PostgreSQL reports under the internal class -> XX000. The
    /// PostGIS parsers do this for malformed geometry text and GeoJSON,
    /// so a client matching on `InternalError` sees the same class.
    Internal(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Parse(m) => write!(f, "{m}"),
            Error::Unsupported(m) => write!(f, "{m} is not supported yet"),
            Error::FeatureNotSupported(m) => write!(f, "{m}"),
            Error::CardinalityViolation(m) => write!(f, "{m}"),
            Error::InvalidNtileArgument(m) => write!(f, "{m}"),
            Error::Windowing(m) => write!(f, "{m}"),
            Error::DuplicateColumn(m) => write!(f, "{m}"),
            Error::UndefinedColumn(c) => write!(f, "column \"{c}\" does not exist"),
            Error::UndefinedField(m) => write!(f, "{m}"),
            Error::UndefinedTable(t) => write!(f, "relation \"{t}\" does not exist"),
            Error::InvalidName(m) => write!(f, "{m}"),
            Error::Grouping(m) => write!(f, "{m}"),
            Error::NoArbiter(m) => write!(f, "{m}"),
            Error::Parameter(m) => write!(f, "{m}"),
            Error::InvalidText(m) => write!(f, "{m}"),
            Error::InvalidDatetimeFormat(m) | Error::DatetimeFieldOverflow(m) => {
                write!(f, "{m}")
            }
            Error::DivisionByZero => write!(f, "division by zero"),
            Error::NumericOutOfRange(m) => write!(f, "{m}"),
            Error::DataException(m) | Error::InvalidParameter(m) | Error::SubstringError(m) => {
                write!(f, "{m}")
            }
            Error::NullValueNotAllowed(m) => write!(f, "{m}"),
            Error::UntranslatableCharacter(m) => write!(f, "{m}"),
            Error::InvalidColumnReference(m)
            | Error::UndefinedFunction(m)
            | Error::IndeterminateDatatype(m)
            | Error::UndefinedObject(m)
            | Error::InvalidRegex(m)
            | Error::ArraySubscript(m)
            | Error::DatatypeMismatch(m)
            | Error::CannotCoerce(m)
            | Error::InvalidForeignKey(m)
            | Error::Internal(m) => write!(f, "{m}"),
            Error::Sqlstate(_, m) => write!(f, "{m}"),
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
    /// The `H` (hint) field PostgreSQL sends with this error, where it sends
    /// one: measured on 16, `unrecognized format() type specifier "x"`
    /// carries `For a single "%" use "%%".`.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Error::InvalidParameter(m) if m.starts_with("unrecognized format() type specifier") => {
                Some("For a single \"%\" use \"%%\".")
            }
            // An assignment with no assignment cast (measured on 16: `insert
            // into t(j) values ($1)` with a `text`-declared `$1` into `jsonb`).
            Error::UndefinedFunction(m)
                if m.starts_with("function ") && m.ends_with(") does not exist") =>
            {
                Some("No function matches the given name and argument types. You might need to add explicit type casts.")
            }
            Error::DatatypeMismatch(m) if m.contains(" but expression is of type ") => {
                Some("You will need to rewrite or cast the expression.")
            }
            _ => None,
        }
    }

    /// The SQLSTATE a client should see.
    pub fn sqlstate(&self) -> &'static str {
        match self {
            Error::Parse(_) => "42601", // syntax_error
            Error::Unsupported(_) | Error::FeatureNotSupported(_) => "0A000", // feature_not_supported
            Error::CardinalityViolation(_) => "21000", // cardinality_violation
            Error::InvalidNtileArgument(_) => "22014", // invalid_argument_for_ntile_function
            Error::Windowing(_) => "42P20",            // windowing_error
            Error::DuplicateColumn(_) => "42701",      // duplicate_column
            Error::UndefinedColumn(_) | Error::UndefinedField(_) => "42703",
            Error::UndefinedTable(_) => "42P01",
            Error::InvalidName(_) => "42602",    // invalid_name
            Error::Grouping(_) => "42803",       // grouping_error
            Error::NoArbiter(_) => "42P10",      // invalid_column_reference
            Error::Parameter(_) => "42P02",      // undefined_parameter
            Error::SubstringError(_) => "22011", // substring_error
            Error::InvalidText(_) => "22P02",    // invalid_text_representation
            Error::InvalidDatetimeFormat(_) => "22007", // invalid_datetime_format
            Error::DatetimeFieldOverflow(_) => "22008", // datetime_field_overflow
            Error::DivisionByZero => "22012",
            Error::NumericOutOfRange(_) => "22003", // numeric_value_out_of_range
            Error::DataException(_) => "22000",     // data_exception
            Error::InvalidParameter(_) => "22023",  // invalid_parameter_value
            Error::NullValueNotAllowed(_) => "22004", // null_value_not_allowed
            Error::UntranslatableCharacter(_) => "22P05", // untranslatable_character
            Error::InvalidColumnReference(_) => "42P10", // invalid_column_reference
            Error::UndefinedFunction(_) => "42883", // undefined_function
            Error::MultipleCommands => "42601",     // syntax_error, as PostgreSQL reports it
            Error::IndeterminateDatatype(_) => "42P18", // indeterminate_datatype
            Error::UndefinedObject(_) => "42704",   // undefined_object
            Error::InvalidRegex(_) => "2201B",      // invalid_regular_expression
            Error::ArraySubscript(_) => "2202E",    // array_subscript_error
            Error::DatatypeMismatch(_) => "42804",  // datatype_mismatch
            Error::CannotCoerce(_) => "42846",      // cannot_coerce
            Error::InvalidForeignKey(_) => "42830", // invalid_foreign_key
            Error::Internal(_) => "XX000",          // internal_error
            Error::Sqlstate(code, _) => code,
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
    /// `ALTER TABLE <t> <action>, ...`. PostgreSQL applies the actions in
    /// order and the whole statement is one transaction, so a later one
    /// failing undoes the earlier ones.
    AlterTable {
        table: String,
        missing_ok: bool,
        actions: Vec<AlterTableAction>,
    },
    /// `CREATE SEQUENCE`.
    CreateSequence {
        name: String,
        options: SequenceOptions,
        if_not_exists: bool,
        temp: bool,
    },
    /// `DROP SEQUENCE`.
    DropSequence {
        names: Vec<String>,
        if_exists: bool,
    },
    /// `ALTER SEQUENCE`, which applies only the options it names.
    AlterSequence {
        name: String,
        options: SequenceOptions,
        missing_ok: bool,
    },
    /// `ALTER TABLE <t> RENAME TO <u>`. Its own statement because pg_query
    /// parses it as a `RenameStmt` rather than an `AlterTableCmd`.
    RenameTable {
        table: String,
        to: String,
        missing_ok: bool,
    },
    /// `ALTER TABLE <t> RENAME COLUMN <c> TO <d>`.
    RenameColumn {
        table: String,
        column: String,
        to: String,
        missing_ok: bool,
    },
    Select(Select),
    /// `UNION` / `INTERSECT` / `EXCEPT`.
    SetOp(SetOpSelect),
    SelectConstant(SelectConstant),
    /// A bare `VALUES (...), (...)` query -- a fixed set of literal rows with no
    /// FROM. `SelectConstant` is its single-row cousin; this is what a
    /// multi-row `VALUES` list (`copy (values ...) to stdout`, or `VALUES`
    /// executed directly) becomes.
    ValuesConstant(ValuesConstant),
    Transaction(TransactionControl),
    DropTable(DropTable),
    /// The rows of a general JOIN, planned as a source: only ever the `plan`
    /// of a `SubSource`, never executed on its own.
    JoinRows(joins::JoinRows),
    /// `EXPLAIN [ANALYZE] [(options)] <statement>`.
    Explain {
        inner: Box<Statement>,
        options: ExplainOptions,
    },
    /// `CREATE [UNIQUE] INDEX`.
    CreateIndex(CreateIndex),
    /// `DROP INDEX <name>, ...`.
    DropIndex {
        names: Vec<String>,
        if_exists: bool,
    },
    /// `CREATE [OR REPLACE] VIEW <name> [(cols)] AS <select>`.
    CreateView(CreateView),
    /// `DROP VIEW <name>, ...`.
    DropView {
        names: Vec<String>,
        if_exists: bool,
        cascade: bool,
    },
    /// `CREATE TYPE <name> AS ENUM (<labels>)`.
    CreateEnum {
        name: String,
        /// The schema qualifier (`CREATE TYPE s.t AS ENUM (...)`), or `None` for
        /// an unqualified name, which lands in `public`. Two enums with the same
        /// bare name in different schemas are distinct types.
        schema: Option<String>,
        labels: Vec<String>,
    },
    /// `DROP TYPE [IF EXISTS] <names> [CASCADE]`.
    DropType {
        names: Vec<String>,
        if_exists: bool,
        /// `CASCADE`: drop the functions that depend on the type too (a base
        /// type's I/O functions). RESTRICT -- the default -- refuses with
        /// 2BP01 while any exist.
        cascade: bool,
    },
    /// `CREATE TYPE <name>` with nothing after the name -- a SHELL type: a
    /// placeholder that exists only so I/O functions can name it before the
    /// full `CREATE TYPE <name> (input = ..., output = ...)` completes it.
    /// (pg_query parses both forms as a `DefineStmt` of kind `OBJECT_TYPE`.)
    CreateShellType {
        name: String,
        schema: Option<String>,
    },
    /// `CREATE TYPE <name> (input = f, output = g [, like = t])` -- the full
    /// form that turns a shell into a base type. Only `input`, `output` and
    /// `like` are accepted; any other option is refused at plan time.
    CreateBaseType {
        name: String,
        schema: Option<String>,
        input: Option<String>,
        output: Option<String>,
    },
    /// `CREATE [OR REPLACE] FUNCTION name(args) RETURNS t LANGUAGE internal AS
    /// '<builtin>'` -- a catalog registration of an internal-language wrapper,
    /// which is how a base type's I/O functions are declared. Other languages
    /// are refused at plan time.
    CreateFunction {
        name: String,
        replace: bool,
        /// Declared argument types, in order (`cstring`, `a-b`).
        arg_types: Vec<String>,
        return_type: String,
        /// The built-in the wrapper names (`textin`).
        body: String,
        volatility: String,
    },
    /// `CREATE [OR REPLACE] FUNCTION` in `LANGUAGE sql` or `plpgsql`.
    CreateUserFunction(UserFunctionDef),
    /// `CREATE [OR REPLACE] TRIGGER`.
    CreateTrigger(TriggerDef),
    /// `DROP TRIGGER [IF EXISTS] name ON table`.
    DropTrigger {
        name: String,
        table: String,
        if_exists: bool,
    },
    /// `DROP FUNCTION [IF EXISTS] name[(args)] [CASCADE]`.
    DropFunction {
        name: String,
        /// `None` when the signature was left off (`DROP FUNCTION f`).
        arg_types: Option<Vec<String>>,
        if_exists: bool,
        cascade: bool,
    },
    /// `CREATE SCHEMA [IF NOT EXISTS] <name>`.
    CreateSchema {
        name: String,
        if_not_exists: bool,
    },
    /// `CREATE TYPE <name> AS (<field type>, ...)` -- a composite type.
    CreateComposite {
        name: String,
        /// The schema qualifier (`CREATE TYPE s.t AS (...)`), or `None` for an
        /// unqualified name, which lands in `public`. Two composites with the
        /// same bare name in different schemas are distinct types.
        schema: Option<String>,
        /// (field name, field type name), in declared order.
        fields: Vec<(String, String)>,
    },
    /// `CREATE TYPE <name> AS RANGE (subtype = <type>)` -- a custom range type.
    CreateRange {
        name: String,
        /// The schema qualifier (`CREATE TYPE s.t AS RANGE (...)`), or `None` for
        /// an unqualified name, which lands in `public`. Two ranges with the same
        /// bare name in different schemas are distinct types.
        schema: Option<String>,
        subtype: String,
    },
    /// `DROP SCHEMA [IF EXISTS] <names> [CASCADE]`.
    DropSchema {
        names: Vec<String>,
        if_exists: bool,
        cascade: bool,
    },
    /// `CREATE DATABASE <name>` -- the options are accepted and ignored.
    CreateDatabase {
        name: String,
    },
    /// `DROP DATABASE [IF EXISTS] <name>`.
    DropDatabase {
        name: String,
        if_exists: bool,
    },
    /// `SHOW name` -- one row, one text column named canonically.
    Show(String),
    /// `CREATE ROLE / USER / GROUP name [WITH options]`. `CREATE USER`
    /// differs from `CREATE ROLE` only in defaulting `LOGIN` to true.
    CreateRole {
        name: String,
        options: RoleOptions,
    },
    /// `ALTER ROLE / USER name [WITH options]` -- the role it names and the
    /// attributes to change. Options not given are left as they are.
    AlterRole {
        name: String,
        options: RoleOptions,
    },
    /// `DROP ROLE / USER / GROUP [IF EXISTS] names`.
    DropRole {
        names: Vec<String>,
        if_exists: bool,
    },
    /// `CREATE EXTENSION [IF NOT EXISTS] name [...]`. The options (`SCHEMA`,
    /// `VERSION`, `CASCADE`) are accepted and ignored: the two extensions
    /// this server carries have one version each and live in `public`.
    CreateExtension {
        name: String,
        if_not_exists: bool,
    },
    /// `DROP EXTENSION [IF EXISTS] names [CASCADE]`.
    DropExtension {
        names: Vec<String>,
        if_exists: bool,
        cascade: bool,
    },
    /// `SET name = value`.
    Set {
        name: String,
        value: String,
    },
    /// `RESET name` / `RESET ALL`.
    Reset(String),
    /// `SET TRANSACTION <modes>` -- the characteristics of the CURRENT
    /// transaction (isolation level / read-write mode / deferrable). Reflected
    /// in `transaction_isolation` / `transaction_read_only` /
    /// `transaction_deferrable` for the life of the block.
    SetTransaction(TransactionModes),
    /// `SET SESSION CHARACTERISTICS AS TRANSACTION <modes>` -- the session
    /// DEFAULT for new transactions, reflected in the `default_transaction_*`
    /// GUCs (and, when not inside an explicit block, the `transaction_*` GUCs
    /// too, since the next implicit statement inherits the new default).
    SetSessionCharacteristics(TransactionModes),
    /// `DECLARE <name> CURSOR FOR <query>`.
    ///
    /// The inner query is planned here and executed at DECLARE time, because a
    /// cursor over a materialised result is scrollable in both directions --
    /// which PostgreSQL's cursors are, and a forward-only stream would not be.
    DeclareCursor {
        name: String,
        query: Box<Statement>,
        /// The deparsed inner query, for the `pg_cursors` catalog view's
        /// `statement` column. Not re-executed -- only reported.
        statement: String,
        /// Declared cursor options, surfaced by `pg_cursors`. A cursor declared
        /// `NO SCROLL` reports `is_scrollable = false`; anything else is
        /// scrollable here, since every cursor is materialised.
        scrollable: bool,
        holdable: bool,
        binary: bool,
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
    /// needs.
    DeallocateAll,
    /// `DEALLOCATE <name>`: the wire layer drops that one prepared statement,
    /// answering 26000 when no statement of the name exists.
    Deallocate(String),
    /// `NOTIFY channel [, payload]`: queued for the transaction, delivered
    /// to every backend LISTENing on the channel when it commits.
    Notify {
        channel: String,
        payload: String,
    },
    /// `LISTEN channel`: takes effect at commit, like the NOTIFY it pairs with.
    Listen(String),
    /// `UNLISTEN channel` (`Some`) or `UNLISTEN *` (`None`).
    Unlisten(Option<String>),
    /// `DO [LANGUAGE lang] 'body'`: an inline code block. The planner only
    /// carries the body and the language (default `plpgsql`); the wire layer
    /// interprets the small RAISE / EXECUTE subset it supports.
    Do {
        language: String,
        body: String,
    },
    /// `COPY <table> [(cols)] FROM STDIN`.
    CopyFrom(CopyFrom),
    /// `COPY <table> [(cols)] TO STDOUT`.
    CopyTo(CopyFrom),
    Aggregate(Aggregate),
    Update(Update),
    Delete(Delete),
    /// `TRUNCATE [TABLE] name [, ...] [RESTART IDENTITY] [CASCADE]`: every
    /// row of every named table goes. `cascade` widens the list to the
    /// tables whose foreign keys reference one being truncated.
    Truncate {
        tables: Vec<String>,
        restart_identity: bool,
        cascade: bool,
    },
    /// `CREATE [TEMP] TABLE name [(cols)] AS query [WITH [NO] DATA]`.
    ///
    /// The table's columns are the query's output columns -- names and
    /// types as the query DESCRIBES them -- so they are settled by the
    /// executor, which is the only place the query's description exists.
    /// `column_names` is the optional explicit list that renames them.
    CreateTableAs {
        table: String,
        if_not_exists: bool,
        temp: bool,
        column_names: Vec<String>,
        query: Box<Statement>,
        /// `WITH NO DATA` creates the table empty and tags `CREATE TABLE AS`;
        /// the default fills it and tags `SELECT n`.
        with_data: bool,
    },
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

/// Which unique constraint an `ON CONFLICT` clause arbitrates on.
///
/// PostgreSQL calls this the *arbiter*. A bare `ON CONFLICT DO NOTHING` names
/// none and takes any unique violation; a target names columns or a constraint,
/// and a violation of a DIFFERENT constraint is still an error.
#[derive(Debug, Clone, PartialEq)]
pub enum ConflictTarget {
    /// `ON CONFLICT (a, b)` — matched against the PK and each UNIQUE
    /// constraint by COLUMN SET, because PostgreSQL infers the index rather
    /// than requiring the declared order.
    Columns(Vec<String>),
    /// `ON CONFLICT ON CONSTRAINT name`.
    Constraint(String),
}

/// What to do with a row that violates the arbiter.
#[derive(Debug, Clone, PartialEq)]
pub enum ConflictAction {
    /// `DO NOTHING` — the row is skipped, and is not counted in the tag.
    Nothing,
    /// `DO UPDATE SET ...` over the existing row.
    ///
    /// Every assignment is a row expression, with no constant fast path: a
    /// literal may sit beside an `excluded.` reference in the same statement,
    /// and the row an expression reads is assembled per conflict anyway, so a
    /// second representation would buy nothing and could disagree with this one.
    Update {
        /// `(stored field, column type, expression)`, planned over the
        /// target's columns PLUS the proposed row's under `EXCLUDED_PREFIX`.
        set_exprs: Vec<(String, String, ColumnExpr)>,
        /// `WHERE` on the DO UPDATE: the update is skipped when it is false.
        filter: Option<ColumnExpr>,
    },
}

/// `INSERT ... ON CONFLICT ...`.
#[derive(Debug, Clone, PartialEq)]
pub struct OnConflict {
    pub target: Option<ConflictTarget>,
    pub action: ConflictAction,
}

/// Field prefix under which a `DO UPDATE` expression sees the PROPOSED row.
///
/// `excluded.v` and `t.v` both resolve to `"v"` through `column_ref_name`,
/// which takes the LAST name of a qualified reference — so without a rename the
/// two are indistinguishable, and `set v = excluded.v` would read the EXISTING
/// row and make the update a silent no-op. The clause's refs are renamed to
/// this prefix before resolution; it contains a dot, so no real column can
/// collide with it.
pub const EXCLUDED_PREFIX: &str = "__excluded__.";

#[derive(Debug, Clone, PartialEq)]
pub struct Insert {
    pub table: String,
    /// One document per row, already keyed by stored FIELD (PK as `_id`).
    pub rows: Vec<Document>,
    /// `RETURNING ...`: the output columns and their expressions, planned
    /// over the table exactly as a SELECT's target list is. `None` when the
    /// statement returns no rows.
    pub returning: Option<Returning>,
    /// `INSERT ... SELECT ...`: the query whose rows are written, planned as
    /// the same SELECT would be on its own. The executor evaluates it, shapes
    /// each row through `insert_row` over `targets`, and appends to `rows`.
    /// `None` for a `VALUES` insert, whose rows are already in `rows`.
    pub source: Option<Box<Statement>>,
    /// The columns being written, in the order the source's columns map onto
    /// them -- the explicit column list, or every column in declared order.
    pub targets: Vec<String>,
    /// Whether the statement named its columns. Without a list a short row
    /// leaves the trailing columns to their defaults (`insert into t(a, b)
    /// select 1` is an error; `insert into t select 1` writes `b` NULL).
    pub explicit_columns: bool,
    /// `ON CONFLICT ...`, or `None` when the statement has no such clause,
    /// which leaves the plain-INSERT path untouched.
    pub on_conflict: Option<OnConflict>,
    /// `OVERRIDING SYSTEM VALUE`, which is the only way to write a
    /// `GENERATED ALWAYS AS IDENTITY` column by hand.
    pub overriding_system: bool,
    /// `OVERRIDING USER VALUE`: the opposite -- the value the statement gives
    /// is DISCARDED and the sequence supplies one, for either identity kind.
    pub overriding_user: bool,
}

/// The projection a `RETURNING` clause applies to each written row.
#[derive(Debug, Clone, PartialEq)]
pub struct Returning {
    /// Output columns in order, as (output name, stored field).
    pub columns: Vec<(String, String)>,
    /// A computed expression per output column, parallel to `columns`.
    pub casts: Vec<Option<ColumnExpr>>,
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
    /// `ORDER BY` over a COMPUTED expression (`order by n * -1`,
    /// `order by upper(a)`). The executor evaluates it per row into `field` —
    /// a synthetic name — just before sorting, so the sort itself stays the
    /// one comparison routine rather than growing a second path.
    ///
    /// `None` is the ordinary case: `field` already names a stored column.
    pub expr: Option<ColumnExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Distinct {
    /// No DISTINCT: every row is its own.
    #[default]
    None,
    /// `SELECT DISTINCT` -- dedup on the whole output row.
    All,
    /// `SELECT DISTINCT ON (a, b)` -- one row per key, the first in ORDER BY
    /// order, which is why it is applied AFTER the sort.
    On(Vec<String>),
}

/// `UNION` / `INTERSECT` / `EXCEPT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOpKind {
    Union,
    Intersect,
    Except,
}

/// `<query> UNION|INTERSECT|EXCEPT [ALL] <query>`, with the ORDER BY / LIMIT
/// that belong to the whole thing.
///
/// Planned as a statement of its own because the two sides are separate
/// queries: each is planned and run on its own, and the rows are combined
/// afterwards. Before 2026-09-20 a set operation was not recognised at all --
/// the outer statement has an empty FROM, so it fell through to the constant
/// planner and answered one empty row.
#[derive(Debug, Clone, PartialEq)]
pub struct SetOpSelect {
    pub left: Box<Statement>,
    pub right: Box<Statement>,
    pub kind: SetOpKind,
    /// `ALL` keeps duplicates (and multiplicities, for INTERSECT / EXCEPT).
    pub all: bool,
    /// ORDER BY over the OUTPUT columns, by position in the select list.
    pub order: Vec<SetOpOrder>,
    pub limit: Option<i64>,
    pub offset: i64,
}

impl SetOpSelect {
    /// The output column names -- the left side's, as PostgreSQL has it.
    pub fn output_names(&self) -> Vec<String> {
        match self.left.as_ref() {
            Statement::Select(sel) => sel.columns.iter().map(|(out, _)| out.clone()).collect(),
            Statement::SelectConstant(sc) => sc.columns.iter().map(|(n, ..)| n.clone()).collect(),
            Statement::ValuesConstant(vc) => vc.names.clone(),
            Statement::SetOp(inner) => inner.output_names(),
            _ => Vec::new(),
        }
    }
}

/// One ORDER BY term of a set operation, as an index into the output row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetOpOrder {
    pub index: usize,
    pub ascending: bool,
    pub nulls: Nulls,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    pub table: String,
    /// Window functions in the select list, each computing into its own
    /// synthetic `__winN` field. Evaluated after the WHERE and before
    /// `DISTINCT` / `ORDER BY` / `LIMIT`, which is PostgreSQL's order.
    pub windows: Vec<WindowItem>,
    /// A FROM-subquery (`FROM (SELECT ...) s`) or an inlined CTE reference
    /// standing in for a table. The executor materialises the inner plan's
    /// rows first and the outer query runs over them, which is why this is a
    /// SOURCE beside `series` and `join` rather than a statement of its own.
    pub sub: Option<Box<SubSource>>,
    /// A set-returning function standing in for a table, as in
    /// `FROM generate_series(1, 5)`. The rows are generated rather than read,
    /// and everything after the source -- ORDER BY, LIMIT, aggregates -- works
    /// on them unchanged, which is why this is a SOURCE on the existing
    /// statement rather than a statement of its own.
    pub series: Option<Series>,
    /// A top-level two-table JOIN standing in for a table, as in
    /// `SELECT ... FROM pg_type t JOIN pg_range r ON ...` -- the plain-select
    /// form of the source `Aggregate` also carries. `RangeInfo.fetch` sends
    /// exactly this. Columns then reference the join's OUTPUT names.
    pub join: Option<Box<JoinSelect>>,
    /// Output columns in order, as (output name, stored field).
    pub columns: Vec<(String, String)>,
    /// A computed expression per output column, parallel to `columns`. `None`
    /// almost everywhere; carries `col::regtype::text` and
    /// `regexp_replace(col, ...)` -- the shapes catalog-reading clients put in
    /// a select list.
    pub casts: Vec<Option<ColumnExpr>>,
    pub filter: Document,
    /// A predicate that does not lower to an MQL filter -- `where (case ...
    /// end)`, say. Evaluated per row AFTER the storage filter, so the result
    /// is correct at the cost of a scan. `None` whenever the whole WHERE
    /// lowered, which is the ordinary case and is untouched.
    ///
    /// SQL's three-valued logic falls out of it: only TRUE keeps a row, so a
    /// NULL result excludes it exactly as PostgreSQL does.
    pub residual: Option<ColumnExpr>,
    pub order: Vec<OrderKey>,
    /// `None` = no LIMIT. `LIMIT 0` is a real limit, not an absent one.
    pub limit: Option<i64>,
    pub offset: i64,
    /// `SELECT DISTINCT` / `DISTINCT ON (...)`. Dropped on the floor before
    /// 2026-09-20: the clause was read in one place (the aggregate planner,
    /// which refuses it) and nowhere else, so a plain `SELECT DISTINCT`
    /// returned its duplicates.
    pub distinct: Distinct,
}

/// One action of an `ALTER TABLE`.
///
/// Only the forms whose effect on the stored rows is well defined are here;
/// anything else is refused by name at plan time. A DDL statement that
/// silently did nothing would leave the catalog describing a table the rows
/// do not match, which is the worst failure this server can have.
#[derive(Debug, Clone, PartialEq)]
pub enum AlterTableAction {
    AddColumn {
        column: Column,
        if_not_exists: bool,
    },
    DropColumn {
        name: String,
        if_exists: bool,
    },
    /// `SET DEFAULT <literal>`, or `DROP DEFAULT` as `None`.
    SetDefault {
        column: String,
        value: Option<Bson>,
        /// An expression default, kept as SQL (`SET DEFAULT now()`).
        expr: Option<String>,
    },
    /// `SET NOT NULL` / `DROP NOT NULL`.
    SetNotNull {
        column: String,
        not_null: bool,
    },
    /// `ALTER COLUMN <c> TYPE <t>`. Existing values are cast, so a value the
    /// new type cannot hold fails the statement rather than being dropped.
    AlterType {
        column: String,
        pg_type: String,
        typmod: i32,
    },
    AddCheck(CheckConstraint),
    DropConstraint {
        name: String,
        if_exists: bool,
    },
}

/// One window function in the select list -- `sum(v) OVER (PARTITION BY g
/// ORDER BY id)`.
///
/// Computed into `field` (a synthetic `__winN`) over the materialised rows,
/// which the select list then projects like any stored column -- the same
/// trick `ORDER BY upper(a)` uses for its `__orderN`.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowItem {
    /// The synthetic field the value lands in.
    pub field: String,
    pub func: WindowFunc,
    /// The function's argument, evaluated per row. `None` for the
    /// argument-less ranking functions and for `count(*)`.
    pub arg: Option<ColumnExpr>,
    /// Extra LITERAL arguments: `lag`/`lead`'s offset and default,
    /// `nth_value`'s N, `ntile`'s bucket count.
    pub args: Vec<Bson>,
    /// `PARTITION BY`: rows are grouped by these before anything else. Reuses
    /// `OrderKey` so one comparator serves partitioning and ordering both;
    /// the direction and null placement are unused here.
    pub partition_by: Vec<OrderKey>,
    pub order_by: Vec<OrderKey>,
    pub frame: WindowFrame,
    /// Fixed at plan time, for the DESCRIBE pass that never sees a row.
    pub result_type: String,
    /// The ARGUMENT's declared type, which `sum` and `avg` need to promote
    /// the way PostgreSQL does (int4 sums as int8, numeric sums exactly).
    pub source_type: Option<String>,
    /// `FILTER (WHERE ...)`, evaluated per row; a row that fails it is not
    /// AGGREGATED but still gets an output value.
    pub filter: Option<ColumnExpr>,
}

/// The window functions this server computes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowFunc {
    RowNumber,
    Rank,
    DenseRank,
    PercentRank,
    CumeDist,
    Ntile,
    Lag,
    Lead,
    FirstValue,
    LastValue,
    NthValue,
    Sum,
    Count,
    CountStar,
    Avg,
    Min,
    Max,
    StringAgg,
    ArrayAgg,
    BoolAnd,
    BoolOr,
}

impl WindowFunc {
    /// True for the functions whose value depends only on the row's POSITION
    /// in the window ordering, not on the frame. PostgreSQL ignores the frame
    /// clause for these, so a frame written beside one must not be applied.
    pub fn ignores_frame(self) -> bool {
        matches!(
            self,
            WindowFunc::RowNumber
                | WindowFunc::Rank
                | WindowFunc::DenseRank
                | WindowFunc::PercentRank
                | WindowFunc::CumeDist
                | WindowFunc::Ntile
                | WindowFunc::Lag
                | WindowFunc::Lead
        )
    }
}

/// A window frame: which rows of the partition the function sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowFrame {
    pub mode: FrameMode,
    pub start: FrameBound,
    pub end: FrameBound,
    pub exclude: FrameExclude,
}

/// `EXCLUDE` removes rows from the frame AFTER its bounds are found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameExclude {
    /// The default: nothing is removed.
    NoOthers,
    /// The current row only.
    CurrentRow,
    /// The current row and every row that ties with it on the ORDER BY.
    Group,
    /// The ties but NOT the current row itself.
    Ties,
}

impl WindowFrame {
    /// PostgreSQL's DEFAULT frame, which is the same whether or not the window
    /// has an ORDER BY: `RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW`
    /// (measured -- `over ()`, `over (order by id)` and `over (partition by g)`
    /// all parse to the identical `frame_options`).
    ///
    /// The familiar difference between a RUNNING total and a whole-partition
    /// one therefore needs no special case: under RANGE the frame ends at the
    /// last PEER of the current row, and with no ORDER BY every row in the
    /// partition is a peer of every other, so the frame is the partition.
    pub const DEFAULT: Self = Self {
        mode: FrameMode::Range,
        start: FrameBound::UnboundedPreceding,
        end: FrameBound::CurrentRow,
        exclude: FrameExclude::NoOthers,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameMode {
    Rows,
    Range,
    Groups,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameBound {
    UnboundedPreceding,
    Preceding(i64),
    CurrentRow,
    Following(i64),
    UnboundedFollowing,
}

/// A subquery standing in for a table in FROM.
///
/// Its rows are keyed by the inner plan's OUTPUT NAMES -- every column of
/// `def` is built `pk: false`, so `Column::field()` is the name itself and the
/// outer query reads `s.c` from the field `c`. That is the same convention a
/// JOIN's `left_sub` / `right_sub` side already uses.
#[derive(Debug, Clone, PartialEq)]
pub struct SubSource {
    /// The alias the outer query refers to it by -- `s` in `FROM (...) s`, or
    /// the CTE's name for an inlined `WITH`. PostgreSQL REQUIRES an alias on a
    /// FROM-subquery, so this is never empty for the subquery form.
    pub alias: String,
    /// The planned inner query.
    pub plan: Box<Statement>,
    /// The inner query's output columns.
    pub def: TableDef,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AggFunc {
    /// `count(*)` — counts ROWS, including those whose columns are all NULL.
    #[default]
    CountStar,
    /// `count(col)` — skips NULLs.
    Count,
    Sum,
    Min,
    Max,
    /// `array_agg(col)` -- every value in group order, NULLs INCLUDED, which
    /// is how a LEFT-JOIN miss surfaces as `[None]` rather than `[]`.
    ArrayAgg,
    /// `bool_and(cond)` / `bool_or(cond)` -- min and max over booleans, NULLs
    /// skipped, NULL over an empty input.
    BoolAnd,
    BoolOr,
    /// `avg(col)` -- the exact sum divided by the count. A float input
    /// averages as a float; everything else answers `numeric`, at the scale
    /// PostgreSQL's division picks.
    Avg,
    /// `string_agg(col, sep)` -- the non-NULL values joined, in group order.
    /// The separator is the item's `sep`.
    StringAgg,
    /// `variance` / `var_samp`, `var_pop`, `stddev` / `stddev_samp`,
    /// `stddev_pop`: exact numeric over integers and numerics, float8 over
    /// floats (PostgreSQL's two accumulator families).
    VarSamp,
    VarPop,
    StddevSamp,
    StddevPop,
    /// `json_agg` / `jsonb_agg`: every value, NULLs as JSON null.
    JsonAgg,
    JsonbAgg,
    /// `json_object_agg(k, v)` / `jsonb_object_agg`.
    JsonObjectAgg,
    JsonbObjectAgg,
    /// The float8 regression family over `(y, x)` pairs, rows with either
    /// NULL skipped (`float8_regr_accum`).
    Corr,
    CovarPop,
    CovarSamp,
    RegrCount,
    RegrAvgX,
    RegrAvgY,
    RegrSxx,
    RegrSyy,
    RegrSxy,
    RegrSlope,
    RegrIntercept,
    RegrR2,
    /// Ordered-set aggregates: the argument is the WITHIN GROUP expression,
    /// the fraction a direct argument.
    PercentileCont,
    PercentileDisc,
    Mode,
    /// Hypothetical-set aggregates: where the direct argument would rank
    /// among the WITHIN GROUP values.
    HypRank,
    HypDenseRank,
    HypPercentRank,
    HypCumeDist,
    /// `bit_and` / `bit_or` over integers.
    BitAnd,
    BitOr,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AggItem {
    pub func: AggFunc,
    /// Stored field; `None` only for `count(*)`.
    pub field: Option<String>,
    /// Output column name.
    pub out: String,
    /// The declared PostgreSQL type of the source column, for `min`/`max`
    /// which return the input type. `count` and `sum` are always int8.
    pub source_type: Option<String>,
    /// An aggregate over an EXPRESSION -- `max(length(data))`,
    /// `sum(n * 2)` -- evaluated per row into `field` (a hidden `__aggN`
    /// slot) before the group is computed. `None` for a bare column.
    pub expr: Option<ColumnExpr>,
    /// `count(DISTINCT col)` and friends: the group's values are deduped
    /// before the function runs. `count(DISTINCT)` is by far the common one,
    /// but PostgreSQL allows it on any aggregate.
    pub distinct: bool,
    /// `agg(...) FILTER (WHERE ...)`: only the group's rows matching this
    /// contribute. A group where NONE match is the empty input -- `count` is
    /// 0 and everything else NULL -- which falls out of feeding the
    /// aggregate an empty row set.
    pub filter: Option<Document>,
    /// `string_agg`'s separator, already evaluated. `Some(Bson::Null)` is a
    /// NULL separator, which PostgreSQL joins with nothing between the
    /// values rather than answering NULL.
    pub sep: Option<Bson>,
    /// `agg(x ORDER BY y)` -- the group's rows are sorted by these before the
    /// values are collected, which is what makes `string_agg` and `array_agg`
    /// answer in a defined order. Empty for every other aggregate, where the
    /// order cannot be observed.
    pub order: Vec<OrderKey>,
    /// The source column's `atttypmod`, which only a blank-padded `char(n)`
    /// uses: `array_agg`, `min` and `max` see the PADDED value, because they
    /// take the column's own type. `string_agg` does not -- its argument is
    /// coerced to `text`, which strips (all measured on 14.24).
    pub source_typmod: i32,
    /// A two-argument aggregate's second argument (`corr(y, x)`'s `x`,
    /// `json_object_agg(k, v)`'s `v`): its field, and the expression that
    /// fills it per row when it is not a bare column.
    pub field2: Option<String>,
    pub expr2: Option<ColumnExpr>,
    pub source_type2: Option<String>,
    /// An ordered-set or hypothetical-set aggregate's DIRECT arguments, already
    /// evaluated: the fraction, or the hypothetical row.
    pub direct: Vec<Bson>,
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
    /// An expression OVER the grouped values -- `count(*) + 1`,
    /// `coalesce(sum(n), 0)` -- by index into `Aggregate::exprs`. Each
    /// aggregate inside it is an ordinary item, computed once and read back
    /// by its slot name.
    Expr(usize),
}

/// A `HAVING` predicate over the grouped rows.
///
/// Deliberately a small shape rather than a general expression: a comparison
/// or NULL test on one grouped value against a constant, combined with
/// AND / OR / NOT. Anything else is refused while planning, because HAVING
/// decides which ROWS come back and approximating it would answer wrongly.
#[derive(Debug, Clone, PartialEq)]
pub enum Having {
    /// `<subject> <op> <constant>`, with the constant already evaluated.
    Compare {
        subject: OutputCol,
        op: String,
        value: Bson,
    },
    /// `<subject> IS [NOT] NULL`.
    IsNull {
        subject: OutputCol,
        negated: bool,
    },
    And(Vec<Having>),
    Or(Vec<Having>),
    Not(Box<Having>),
}

/// ORDER BY over an aggregate query, by index into `Aggregate::group_by`.
#[derive(Debug, Clone, PartialEq)]
pub struct AggOrderKey {
    pub group_index: usize,
    pub ascending: bool,
    pub nulls: Nulls,
}

/// One GROUP BY key: a bare column, or an expression over the row.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupKey {
    /// The name PostgreSQL gives the key when it is projected.
    pub name: String,
    /// The stored field a bare column key reads.
    pub field: String,
    /// An expression key -- `GROUP BY length(data)`, or `GROUP BY 2` naming
    /// such a target -- evaluated per row; `None` for a bare column.
    pub expr: Option<ColumnExpr>,
    /// The key's declared PostgreSQL type, for the row description.
    pub pg_type: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    pub table: String,
    /// A generated source in place of a table, as for `Select`.
    pub series: Option<Series>,
    /// A FROM-subquery or inlined CTE in place of a table, as for `Select` --
    /// `select max(c) from (select count(*) as c from t group by k) s`.
    pub sub: Option<Box<SubSource>>,
    /// A JOINED source in place of a table: `FROM (SELECT ... FROM a JOIN b
    /// ON ...) x`, which is how psycopg's type-registration queries read the
    /// catalog. The rows are materialised by the executor and then grouped
    /// exactly as a table's would be.
    pub join: Option<Box<JoinSelect>>,
    /// EVERY GROUP BY key, in declared order -- including ones the SELECT
    /// list does not project, because ORDER BY may still reference them.
    pub group_by: Vec<GroupKey>,
    /// `GROUPING SETS` / `ROLLUP` / `CUBE`, expanded to one entry per set,
    /// each naming the INDICES into `group_by` that the set groups on.
    ///
    /// `None` is a plain `GROUP BY`, which is the same thing as a single set
    /// naming every key -- but kept distinct so the ordinary path allocates
    /// and branches exactly as it did before.
    pub grouping_sets: Option<Vec<Vec<usize>>>,
    pub items: Vec<AggItem>,
    /// The output columns, in order, each pointing at a group or an aggregate.
    pub select: Vec<(String, OutputCol)>,
    pub filter: Document,
    pub order: Vec<AggOrderKey>,
    pub limit: Option<i64>,
    pub offset: i64,
    /// `HAVING ...`, applied to the grouped rows before ORDER BY and LIMIT.
    pub having: Option<Having>,
    /// Expressions over the grouped values, referenced by `OutputCol::Expr`.
    pub exprs: Vec<ColumnExpr>,
    /// `SELECT DISTINCT count(*) ...` -- dedup on the aggregate's OUTPUT rows,
    /// after grouping and before the ORDER BY. `DISTINCT ON` over an aggregate
    /// is still refused rather than approximated.
    pub distinct: bool,
}

/// A column reference as `(alias, column)`.
type QualifiedColumn = (String, Vec<String>);

/// A two-table join, projected: the subset every measured catalog query uses.
///
/// One equality in ON, an optional single-column equality filter, an optional
/// single-column ORDER BY. Anything else in a JOIN is still refused -- a JOIN
/// half-supported quietly returns wrong rows, which is worse.
/// A comparison operator usable in a JOIN's WHERE predicate list.
#[derive(Debug, Clone, PartialEq)]
pub enum JoinOp {
    Eq,
    Gt,
    Ge,
    Lt,
    Le,
    /// `NOT <boolcol>` -- keep rows whose boolean column is NOT true (false or
    /// NULL), which is what `NOT a.attisdropped` means. Carries no value.
    NotTrue,
}

/// One WHERE predicate `alias.col <op> value` against a JOIN side.
#[derive(Debug, Clone, PartialEq)]
pub struct JoinPred {
    pub alias: String,
    pub col: String,
    pub op: JoinOp,
    pub value: Bson,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JoinSelect {
    /// (table, alias) for each side.
    pub left: (String, String),
    pub right: (String, String),
    /// LEFT OUTER when true, INNER when false.
    pub left_join: bool,
    /// The ON equality: (alias, column) on each side, either order. `None`
    /// is a CROSS join (`FROM a, b`): every left row against every right.
    pub on: Option<((String, String), (String, String))>,
    /// Output columns: (output name, side alias, column).
    pub columns: Vec<(String, String, String)>,
    /// A computed expression per output column (cast chains, mostly:
    /// `t.oid::regtype::text AS regtype`), parallel to `columns`.
    pub exprs: Vec<Option<ColumnExpr>>,
    /// WHERE predicates against one side's column, already evaluated. A plain
    /// single equality is one `Eq` entry (byte-identical to the former
    /// `Option` form); a subquery's `WHERE a=1 AND b>0 AND NOT c` is several.
    pub filter: Vec<JoinPred>,
    /// ORDER BY one column: (alias, column, ascending). PostgreSQL sorts NULLS
    /// LAST ascending, which a LEFT JOIN's misses rely on.
    pub order: Option<(String, String, bool)>,
    /// When a side is a SUBQUERY rather than a table (`... JOIN (SELECT ...) a`),
    /// its planned sub-statement; the executor materialises its rows. `None`
    /// for a plain table side, whose rows come from `table_docs(left.0)`. A Sub
    /// side carries `""` as its table name and the alias as `.1`.
    pub left_sub: Option<Box<Statement>>,
    pub right_sub: Option<Box<Statement>>,
}

/// Transaction control, two-phase commit included: `PREPARE TRANSACTION`
/// carries the block's write set into the storage so `COMMIT PREPARED` /
/// `ROLLBACK PREPARED` can resolve it from any connection, restart included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionControl {
    /// `modes` are the transaction characteristics tacked onto the statement
    /// (`BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY DEFERRABLE`), which set
    /// the `transaction_*` GUCs for the life of the block.
    Begin(TransactionModes),
    /// `START TRANSACTION`, which does exactly what `BEGIN` does and differs
    /// only in the command tag it answers with. Carries the same modes.
    Start(TransactionModes),
    /// `chain` is `AND CHAIN`: the block ends and another opens immediately,
    /// so the connection is still in a transaction afterwards.
    Commit {
        chain: bool,
    },
    Rollback {
        chain: bool,
    },
    /// `PREPARE TRANSACTION '<gid>'`: end the block with its work durably
    /// parked under `gid`, neither committed nor discarded.
    Prepare(String),
    /// `COMMIT PREPARED '<gid>'`.
    CommitPrepared(String),
    /// `ROLLBACK PREPARED '<gid>'`.
    RollbackPrepared(String),
    /// `SAVEPOINT <name>`: a point inside the block to come back to.
    Savepoint(String),
    /// `RELEASE [SAVEPOINT] <name>`: destroy it, KEEPING its writes.
    Release(String),
    /// `ROLLBACK TO [SAVEPOINT] <name>`: undo everything written since it, and
    /// leave the savepoint itself open.
    RollbackTo(String),
}

/// The transaction characteristics on a `BEGIN` / `START TRANSACTION` /
/// `SET TRANSACTION` / `SET SESSION CHARACTERISTICS AS TRANSACTION` statement.
///
/// Each field is `None` when the statement did not name it, so an omitted mode
/// inherits the session default rather than forcing a value. This server is
/// single-node and does not truly enforce isolation levels -- it accepts them
/// and reflects them in the `transaction_*` / `default_transaction_*` GUCs so a
/// client (e.g. psycopg reading `current_setting('transaction_isolation')`)
/// sees exactly what it set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransactionModes {
    /// The canonical GUC spelling of the isolation level -- `"serializable"`,
    /// `"repeatable read"`, `"read committed"`, `"read uncommitted"`.
    pub isolation: Option<String>,
    /// `READ ONLY` -> `Some(true)`, `READ WRITE` -> `Some(false)`.
    pub read_only: Option<bool>,
    /// `DEFERRABLE` -> `Some(true)`, `NOT DEFERRABLE` -> `Some(false)`.
    pub deferrable: Option<bool>,
}

/// Parse a list of `DefElem` nodes (the `options` of a `TransactionStmt` or the
/// `args` of a `SET TRANSACTION` / `SET SESSION CHARACTERISTICS` statement) into
/// `TransactionModes`. Unknown DefElems (e.g. `TRANSACTION SNAPSHOT`) are
/// ignored -- they carry no isolation/read-only/deferrable characteristic this
/// server reflects.
fn parse_transaction_modes(nodes: &[pg_query::protobuf::Node]) -> TransactionModes {
    let mut modes = TransactionModes::default();
    for node in nodes {
        let Some(N::DefElem(d)) = node.node.as_ref() else {
            continue;
        };
        match d.defname.as_str() {
            "transaction_isolation" => {
                if let Some(arg) = d.arg.as_ref() {
                    if let Some(N::AConst(c)) = arg.node.as_ref() {
                        if let Some(pg_query::protobuf::a_const::Val::Sval(s)) = c.val.as_ref() {
                            modes.isolation = Some(s.sval.to_ascii_lowercase());
                        }
                    }
                }
            }
            "transaction_read_only" => modes.read_only = Some(def_elem_bool(d)),
            "transaction_deferrable" => modes.deferrable = Some(def_elem_bool(d)),
            _ => {}
        }
    }
    modes
}

/// The attributes `CREATE ROLE` / `ALTER ROLE` can set. `None` means the
/// statement did not mention the attribute. `password` is `Some(None)` for
/// `PASSWORD NULL`, which clears a stored one.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RoleOptions {
    pub login: Option<bool>,
    pub superuser: Option<bool>,
    pub createdb: Option<bool>,
    pub createrole: Option<bool>,
    pub inherit: Option<bool>,
    pub replication: Option<bool>,
    pub bypassrls: Option<bool>,
    pub connection_limit: Option<i64>,
    pub password: Option<Option<String>>,
    pub valid_until: Option<String>,
}

/// The role option list of a CREATE / ALTER ROLE: `DefElem`s named by
/// gram.y (`canlogin`, `superuser`, `password`, ...). `PASSWORD NULL`
/// arrives as a `password` element with no argument.
fn role_options(options: &[pg_query::protobuf::Node]) -> Result<RoleOptions> {
    let mut out = RoleOptions::default();
    for node in options {
        let Some(N::DefElem(d)) = node.node.as_ref() else {
            continue;
        };
        let flag = || -> bool {
            match d.arg.as_ref().and_then(|a| a.node.as_ref()) {
                Some(N::Boolean(b)) => b.boolval,
                Some(N::Integer(i)) => i.ival != 0,
                Some(N::AConst(c)) => {
                    matches!(
                        c.val.as_ref(),
                        Some(pg_query::protobuf::a_const::Val::Boolval(b)) if b.boolval
                    ) || matches!(
                        c.val.as_ref(),
                        Some(pg_query::protobuf::a_const::Val::Ival(v)) if v.ival != 0
                    )
                }
                _ => true,
            }
        };
        let text = || -> Option<String> {
            match d.arg.as_ref().and_then(|a| a.node.as_ref()) {
                Some(N::String(sv)) => Some(sv.sval.clone()),
                Some(N::AConst(c)) => match c.val.as_ref() {
                    Some(pg_query::protobuf::a_const::Val::Sval(sv)) => Some(sv.sval.clone()),
                    _ => None,
                },
                _ => None,
            }
        };
        match d.defname.as_str() {
            "canlogin" => out.login = Some(flag()),
            "superuser" => out.superuser = Some(flag()),
            "createdb" => out.createdb = Some(flag()),
            "createrole" => out.createrole = Some(flag()),
            "inherit" => out.inherit = Some(flag()),
            "isreplication" => out.replication = Some(flag()),
            "bypassrls" => out.bypassrls = Some(flag()),
            // `PASSWORD $1` parses here but is a syntax error on PostgreSQL
            // (gram.y takes only `PASSWORD Sconst` / `PASSWORD NULL`; probed
            // 16): a client that wants a bound password must inline it, as
            // libpq's PQchangePassword does.
            "password" => {
                if let Some(N::ParamRef(p)) = d.arg.as_ref().and_then(|a| a.node.as_ref()) {
                    return Err(Error::Parse(format!(
                        "syntax error at or near \"${}\"",
                        p.number
                    )));
                }
                out.password = Some(text())
            }
            "validUntil" => out.valid_until = text(),
            "connectionlimit" => {
                out.connection_limit = match d.arg.as_ref().and_then(|a| a.node.as_ref()) {
                    Some(N::Integer(i)) => Some(i64::from(i.ival)),
                    _ => Some(-1),
                }
            }
            // Membership and SYSID options: parsed by PostgreSQL, but this
            // server has no role graph to record them in.
            "addroleto" | "rolemembers" | "adminmembers" | "sysid" | "encrypted"
            | "unencrypted" => {
                return Err(Error::Unsupported(format!("the {} role option", d.defname)))
            }
            other => {
                return Err(Error::Parse(format!(
                    "unrecognized role option \"{other}\""
                )))
            }
        }
    }
    Ok(out)
}

/// A `DefElem` whose arg is an `A_Const` integer used as a boolean
/// (`transaction_read_only` / `transaction_deferrable`): PostgreSQL renders
/// `READ ONLY` / `DEFERRABLE` as integer `1` and their negations as `0`.
fn def_elem_bool(d: &pg_query::protobuf::DefElem) -> bool {
    d.arg
        .as_ref()
        .and_then(|arg| match arg.node.as_ref()? {
            N::AConst(c) => match c.val.as_ref()? {
                pg_query::protobuf::a_const::Val::Ival(v) => Some(v.ival != 0),
                pg_query::protobuf::a_const::Val::Boolval(b) => Some(b.boolval),
                _ => None,
            },
            _ => None,
        })
        .unwrap_or(false)
}

/// `SELECT <items>` with no FROM: one row, computed without touching storage.
///
/// Clients lean on this constantly -- psycopg, pgjdbc and pgx all probe
/// `version()` and friends during connection setup -- so a server that cannot
/// answer it is unusable by real drivers even if every table query works.
/// A computed output column of a TABLE select: the stored value, transformed.
#[derive(Debug, Clone, PartialEq)]
pub enum ColumnExpr {
    /// A chain of casts, innermost first: `oid::regtype::text` is
    /// `["regtype", "text"]`. `source` is the column's DECLARED type when the
    /// planner knows it: a stored `timestamptz` renders `::text` in the
    /// session zone, which the value alone (a UTC instant, the same carrier
    /// as `timestamp`) cannot tell the executor.
    Casts {
        source: Option<String>,
        chain: Vec<String>,
    },
    /// A scalar call with the COLUMN somewhere among constant arguments --
    /// `regexp_replace(statement, 'pat', '', 'i')`. `None` marks the column's
    /// position; `result_type` is fixed at plan time so the DESCRIBE pass,
    /// which never sees a row, can still name the column's type.
    Call {
        name: String,
        args: Vec<Option<Bson>>,
        result_type: String,
    },
    /// `coalesce(col, <fallback>...)` -- its own SQL node, not a scalar call.
    /// `None` marks the column's argument position; the first non-NULL argument
    /// wins. Used for a join target like `coalesce(a.fnames, '{}')`, where a
    /// LEFT-JOIN miss makes the column NULL and the fallback stands in.
    Coalesce { args: Vec<Option<Bson>> },
    /// A literal in the select list -- `SELECT 1 FROM t`. The value is the same
    /// for every row and ignores the row entirely; `result_type` fixes the
    /// column's type for the DESCRIBE pass. `SELECT 1 FROM pg_cursors WHERE
    /// name = ...` is how psycopg's server cursor probes for a cursor's
    /// existence, and `SELECT 1 FROM generate_series(...)` is how the suite
    /// counts rows.
    Const { value: Bson, result_type: String },
    /// An arbitrary expression over the row -- `'2021-01-01'::date + i`,
    /// `i::int4`, `i * 2`. Every column reference in `expr` was rewritten at
    /// plan time into a parameter numbered PAST the statement's own, so the
    /// constant evaluator runs it unchanged over `params ++ row values`;
    /// `fields` lists the row values in that order as (column name, stored
    /// field, type): the field is what the row is read by, and the type is
    /// declared as the parameter's when the expression runs, so `pg_typeof(i)`
    /// answers the column's declared type. `result_type` is fixed at plan
    /// time for the DESCRIBE pass.
    Row {
        expr: Box<pg_query::protobuf::Node>,
        fields: Vec<RowField>,
        params: Vec<Bson>,
        result_type: String,
    },
}

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
    /// `pg_backend_pid()` -- the connection's own backend PID, which only the
    /// server knows (pgwire assigns it during startup).
    BackendPid,
    /// `nextval(seq)` -- draws and CONSUMES a value, so it is resolved at
    /// execution and never at DESCRIBE: a describe that advanced the sequence
    /// would hand the next caller a number PostgreSQL never skipped.
    NextVal(Box<ConstCol>),
    /// `currval(seq)` -- what THIS SESSION last drew from it.
    CurrVal(Box<ConstCol>),
    /// `setval(seq, value [, is_called])`.
    SetVal {
        sequence: Box<ConstCol>,
        value: Box<ConstCol>,
        is_called: Box<ConstCol>,
    },
    /// `pg_get_serial_sequence(table, column)` -- the sequence name a serial
    /// column draws from, which only the catalog knows.
    SerialSequence {
        table: Box<ConstCol>,
        column: Box<ConstCol>,
    },
    /// `pg_terminate_backend(pid)` -- terminate the backend with that PID. The
    /// argument is itself a `ConstCol` because it may be a literal, a bound
    /// parameter, or a nested `pg_backend_pid()` (the common `SELECT
    /// pg_terminate_backend(pg_backend_pid())`), all of which the server
    /// resolves at execution.
    TerminateBackend(Box<ConstCol>),
    /// `pg_cancel_backend(pid)` -- cancel that backend's running statement.
    /// Same argument shapes as `TerminateBackend`.
    CancelBackend(Box<ConstCol>),
    /// `current_user` / `session_user` / `user` / `current_role` -- the role
    /// the client connected as, which only the server's session knows.
    SessionUser,
    /// `current_database()` / `current_catalog` -- the database the client
    /// connected to, which only the server's session knows.
    CurrentDatabase,
    /// `pg_sleep(seconds)` -- the argument is already cast to `float8` (or
    /// NULL). The sleep happens at execution, on the connection's own thread,
    /// so it costs the caller exactly the wait PostgreSQL would.
    Sleep(Bson),
    /// `pg_notify(channel, payload)` -- a NOTIFY as a function, queued for
    /// the transaction like the statement. Either argument may be NULL; the
    /// server applies PostgreSQL's rules (an empty channel is 22023, a NULL
    /// payload is the empty string).
    PgNotify {
        channel: Bson,
        payload: Bson,
    },
    /// `pg_listening_channels()` -- one row per channel this session
    /// LISTENs on, which only the server's session knows.
    ListeningChannels,
    /// The output column of a `FROM function(...)` source -- `select * from
    /// pg_sleep(1)`, `select x from pg_listening_channels() x` -- read from
    /// `SelectConstant::source`'s row rather than resolved on its own.
    FromColumn,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValuesConstant {
    /// Output column names. A bare `VALUES` names them `column1`, `column2`,
    /// ... in PostgreSQL, which is what a client sees for
    /// `copy (values ...) to stdout` too.
    pub names: Vec<String>,
    /// The declared PostgreSQL type of each column, carried explicitly for the
    /// same reason `SelectConstant` does: the describe pass names the columns
    /// before any row is seen.
    pub types: Vec<String>,
    /// The already-resolved literal rows, each with one value per column.
    pub rows: Vec<Vec<Bson>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectConstant {
    /// (output name, column, declared PostgreSQL type, type modifier).
    ///
    /// The type is carried EXPLICITLY rather than inferred from the value.
    /// `Describe` arrives before `Bind` and is planned against NULL
    /// placeholders, so inferring from the value typed `$1::int` as `varchar`
    /// and the client then decoded a perfectly good integer as a string.
    ///
    /// The fourth element is the wire type-modifier (`atttypmod`) a declared
    /// cast carries -- `varchar(10)` -> 14, `numeric(10,2)` -> 655366 -- or -1
    /// for no modifier. It is DESCRIPTION metadata that clients turn into
    /// `precision` / `scale` / `display_size`; it never affects the value.
    pub columns: Vec<(String, ConstCol, String, i32)>,
    /// Whether the (constant) WHERE clause admits the one row. `select 1
    /// where false` is ZERO rows in PostgreSQL; before this was carried, the
    /// predicate was ignored and the row answered anyway.
    pub where_true: bool,
    /// `FROM function(...)`: the function stands in for a table. It is
    /// evaluated once per statement (so `select 'ok' from pg_sleep(0.5)`
    /// still waits), a set-returning one yields one row per result, and
    /// every `ConstCol::FromColumn` in `columns` reads that row's value.
    pub source: Option<Box<ConstCol>>,
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

/// `CREATE [UNIQUE] INDEX [IF NOT EXISTS] [name] ON t (cols) [INCLUDE (...)]
/// [WHERE pred]`.
///
/// An index maps onto a STORAGE index over the table's collection -- the shape
/// the Python server writes too, so either server reads the other's. It never
/// changes an answer, except that a UNIQUE one enforces.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateIndex {
    /// `None` when the statement named no index: the server picks
    /// PostgreSQL's `<table>_<cols>_idx`, numbered on a collision.
    pub name: Option<String>,
    pub table: String,
    /// `(column, descending)`, in key order.
    pub columns: Vec<(String, bool)>,
    /// `INCLUDE (...)` columns: metadata only, reported by the catalog.
    pub include: Vec<String>,
    pub unique: bool,
    pub if_not_exists: bool,
    /// `WHERE pred` lowered to a filter over stored fields.
    pub predicate: Option<Document>,
    /// The predicate as PostgreSQL renders it in `pg_indexes.indexdef`.
    pub predicate_sql: Option<String>,
    /// `btree` or `hash`.
    pub method: String,
}

/// A user-defined function as CREATE FUNCTION declares it.
#[derive(Debug, Clone, PartialEq)]
pub struct UserFunctionDef {
    pub name: String,
    pub replace: bool,
    /// `(name, type)` of each INPUT parameter, in order; a name may be empty.
    pub params: Vec<(String, String)>,
    pub return_type: String,
    pub returns_set: bool,
    /// `RETURNS TABLE (...)` / OUT columns.
    pub columns: Vec<(String, String)>,
    pub body: String,
    pub language: String,
    pub volatility: String,
}

/// A trigger as CREATE TRIGGER declares it.
#[derive(Debug, Clone, PartialEq)]
pub struct TriggerDef {
    pub name: String,
    pub table: String,
    pub replace: bool,
    /// `BEFORE` / `AFTER`.
    pub timing: String,
    /// `INSERT` / `UPDATE` / `DELETE` / `TRUNCATE`, in PostgreSQL's order.
    pub events: Vec<String>,
    /// `UPDATE OF col, ...`: fire only when one of these is a SET target.
    pub update_columns: Vec<String>,
    /// `ROW` / `STATEMENT`.
    pub level: String,
    pub function: String,
    /// `EXECUTE FUNCTION f('a', 'b')`: the literal arguments, as `TG_ARGV`.
    pub args: Vec<String>,
    /// The `WHEN (...)` condition, deparsed.
    pub when: Option<String>,
}

/// `EXPLAIN`'s options, as PostgreSQL defaults them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainOptions {
    pub analyze: bool,
    pub verbose: bool,
    pub costs: bool,
    /// `text` or `json`.
    pub format: String,
}

/// `CREATE [OR REPLACE] VIEW`.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateView {
    pub name: String,
    /// The deparsed SELECT alone, before any column list is applied.
    pub body: String,
    /// The stored definition: the deparsed SELECT, wrapped so a declared
    /// column list renames its outputs.
    pub definition: String,
    /// The declared column list, empty when none was written.
    pub columns: Vec<String>,
    pub replace: bool,
    /// `LOCAL` / `CASCADED` for `WITH CHECK OPTION`.
    pub check_option: Option<String>,
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
    /// SET values that read the row -- `num = num * 2`, `s = upper(s)`.
    /// Each is (stored field, declared column type, expression); the server
    /// evaluates them per matched row with `update_row_sets` and writes the
    /// result by `_id`, because a constant `$set` cannot express them.
    pub set_exprs: Vec<(String, String, ColumnExpr)>,
    /// `SET a[i] = v` / `SET a[lo:hi] = v` -- assignments INTO an array the
    /// row already holds, which rewrite it rather than replace it. Kept apart
    /// from `set_exprs` because the new value is a function of the OLD one and
    /// of the subscripts, none of which a plain expression over the row can
    /// express.
    pub set_subscripts: Vec<SubscriptAssign>,
    pub filter: Document,
    /// As `Delete::residual`.
    pub residual: Option<ColumnExpr>,
    /// `UPDATE ... RETURNING`, over the rows AFTER the update -- which is
    /// what PostgreSQL returns. Dropped on the floor before 2026-09-20: the
    /// rows were updated and the client got no rowset at all.
    pub returning: Option<Returning>,
}

/// One `SET a[...] = v` assignment.
#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptAssign {
    /// The array column's stored field.
    pub field: String,
    /// The column's declared type, which the rewritten ARRAY is cast to.
    pub pg_type: String,
    /// The type the assigned value is cast to: the element type for an
    /// element assignment, the array type itself for a slice.
    pub value_type: String,
    pub subs: Vec<SubscriptTarget>,
    pub value: ColumnExpr,
}

/// One subscript of an assignment target. A bound is an expression over the
/// row, because `SET a[n] = 1` is legal and `n` may be a column.
#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptTarget {
    Index(ColumnExpr),
    Slice(Option<ColumnExpr>, Option<ColumnExpr>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Delete {
    pub table: String,
    pub filter: Document,
    /// A WHERE (or the part of one) that does not lower to a filter,
    /// evaluated per candidate row by the executor.
    pub residual: Option<ColumnExpr>,
    /// `DELETE ... RETURNING`, over the rows as they were before the delete.
    pub returning: Option<Returning>,
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

/// `a[i]`, `a[lo:hi]` and their multidimensional forms.
///
/// Two rules decide the shape of the answer, and both are PostgreSQL's rather
/// than this server's (measured on 14.13):
///
/// - If ANY subscript in the chain is a slice, EVERY one is — a bare `[1]`
///   beside a slice means `[1:1]`, so `m[1:2][1]` is `{{1},{3}}` and not a
///   1-D array. The result is then an array; otherwise it is one element.
/// - A subscript chain SHORTER than the array's dimensionality selects
///   nothing: `(ARRAY[[1,2],[3,4]])[1]` is NULL, not `{1,2}`.
fn array_subscript(
    arg: &pg_query::protobuf::Node,
    indirection: &[pg_query::protobuf::Node],
    params: &[Bson],
) -> Result<Bson> {
    let value = const_value(arg, params)?;
    if value == Bson::Null {
        return Ok(Bson::Null);
    }
    if !matches!(value, Bson::Array(_)) {
        return Err(Error::DatatypeMismatch(format!(
            "cannot subscript type {} because it does not support subscripting",
            static_type(arg, &value)
        )));
    }
    let bound = |n: Option<&pg_query::protobuf::Node>| -> Result<Option<i64>> {
        let Some(n) = n else { return Ok(None) };
        Ok(match const_value(n, params)? {
            Bson::Null => None,
            v => Some(arrays::subscript_index(&v)?),
        })
    };
    let mut subs = Vec::with_capacity(indirection.len());
    let mut any_slice = false;
    for ind in indirection {
        let Some(N::AIndices(idx)) = ind.node.as_ref() else {
            return Err(Error::Unsupported("this field selection".into()));
        };
        any_slice |= idx.is_slice;
        subs.push((
            idx.is_slice,
            bound(idx.lidx.as_deref())?,
            bound(idx.uidx.as_deref())?,
        ));
    }
    if !any_slice {
        let ndims = arrays::dim_lengths(&value).len();
        let plain: Vec<Option<i64>> = subs.iter().map(|(_, _, u)| *u).collect();
        return Ok(arrays::element(&value, &plain, ndims));
    }
    // Under a slice an omitted bound is the array's own edge, and a bare
    // index `i` stands for `i:i`.
    let bounds: Vec<(Option<i64>, Option<i64>)> = subs
        .iter()
        .enumerate()
        .map(|(dim, (is_slice, lo, hi))| {
            if *is_slice {
                let len = arrays::dim_lengths(&value)
                    .get(dim)
                    .map(|n| *n as i64)
                    .unwrap_or(0);
                (Some(lo.unwrap_or(1)), Some(hi.unwrap_or(len)))
            } else {
                // PostgreSQL: "any subscript written as a single number is
                // treated as being from 1 to the number specified". So
                // `m[1:2][2]` is `m[1:2][1:2]` -- the WHOLE second dimension,
                // not its second element. Reading it as `n:n` happened to
                // agree whenever n was 1, which is why the first probe of
                // `m[1:2][1]` did not catch it.
                (Some(1), *hi)
            }
        })
        .collect();
    // A NULL bound anywhere makes the whole slice NULL.
    if bounds.iter().any(|(l, u)| l.is_none() || u.is_none()) {
        return Ok(Bson::Null);
    }
    Ok(arrays::slice(&value, &bounds).unwrap_or(Bson::Null))
}

/// The range or multirange type a CONSTRUCTOR call names, when it names one:
/// `int4range(1,5)`, `testrange('a','b')`, `testschema.testrange(1.5,2.5)`.
///
/// Schema-qualified as the call is, because `testschema.testrange` and a bare
/// `testrange` are two types with two subtypes; `func_name` keeps only the
/// last part and would resolve both to the public one.
fn range_constructor_type(f: &pg_query::protobuf::FuncCall) -> Option<String> {
    let parts: Vec<&str> = f
        .funcname
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            N::String(st) => Some(st.sval.as_str()),
            _ => None,
        })
        .collect();
    let name = match parts.as_slice() {
        [n] | ["pg_catalog", n] => (*n).to_string(),
        [schema, n] => canonical_type_ref(&format!("{schema}.{n}")),
        _ => return None,
    };
    (range::is_range_type(&name) || range::is_multirange_type(&name)).then_some(name)
}

/// `int4range('[1,3)')` / `int4multirange('{[1,3)}')`: a constructor called
/// with ONE string literal is the function-style cast of that literal, not a
/// lower bound -- PostgreSQL parses it as a range literal and reports a bad
/// one as `malformed range literal`. Only a LITERAL: a typed argument goes
/// through the constructor.
fn sole_literal_string_arg(f: &pg_query::protobuf::FuncCall) -> Option<String> {
    let [only] = f.args.as_slice() else {
        return None;
    };
    match only.node.as_ref()? {
        N::AConst(c) => match c.val.as_ref()? {
            a_const::Val::Sval(sv) => Some(sv.sval.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// A range bound accessor over an operand that is STATICALLY a range or a
/// multirange: `lower('[1,5)'::int4range)`, `upper($1)` with `$1` declared
/// `testrange`, `isempty(int4range(1,1))`.
///
/// A range is carried as its rendered text, and `lower` / `upper` are ALSO
/// the string case functions -- so without the expression's type,
/// `lower('[a,b)'::testrange)` lower-cased the text instead of answering the
/// bound. This is the same PostgreSQL rule the constructor path follows: the
/// operand's type picks the overload. Answers `(element type, range type,
/// is multirange)`, or `None` when the call is not an accessor over a typed
/// range operand, in which case the scalar path takes it.
fn range_accessor(f: &pg_query::protobuf::FuncCall) -> Option<(String, String, bool)> {
    let name = func_name(f)?;
    if !range::is_accessor(&name) || f.args.len() != 1 {
        return None;
    }
    let type_name = static_range_type(f.args.first())?;
    if let Some(member) = range::multirange_member(&type_name) {
        let (element, _) = range::range_element(&member)?;
        return Some((element, type_name, true));
    }
    let (element, _) = range::range_element(&type_name)?;
    Some((element, type_name, false))
}

/// Evaluate a range accessor call (see `range_accessor`) to its value and
/// its result type.
fn range_accessor_value(
    f: &pg_query::protobuf::FuncCall,
    params: &[Bson],
) -> Option<Result<(Bson, String)>> {
    let (element, type_name, multi) = range_accessor(f)?;
    let name = func_name(f)?;
    let result_type = range::accessor_result_type(&name, &element);
    let arg = f.args.first()?;
    Some((|| {
        let value = const_value(arg, params)?;
        if value == Bson::Null {
            return Ok((Bson::Null, result_type));
        }
        let text = render_value_text(&value);
        let out = if multi {
            let members = range::multirange_from_text(&text, &type_name)?;
            range::multirange_accessor(&name, &members, &element)?
        } else {
            range::accessor(&name, &range::from_text(&text, &type_name)?, &element)?
        };
        Ok((out, result_type))
    })())
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
/// The 1-based CHARACTER position of the first identifier token spelling
/// `name` in `sql`, for an error's `P` field. A quoted identifier matches on
/// its unquoted text; an unquoted one on its case-folded text.
pub fn identifier_position(sql: &str, name: &str) -> Option<usize> {
    let scanned = pg_query::scan(sql).ok()?;
    let ident = pg_query::protobuf::Token::Ident as i32;
    for tok in &scanned.tokens {
        if tok.token != ident {
            continue;
        }
        let (start, end) = (tok.start as usize, tok.end as usize);
        let text = sql.get(start..end)?;
        let spelled = if let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"'))
        {
            inner.replace("\"\"", "\"")
        } else {
            text.to_ascii_lowercase()
        };
        if spelled == name {
            return Some(sql[..start].chars().count() + 1);
        }
    }
    None
}

/// libpg_query's own message, without the `Error splitting: ` / `Invalid
/// statement: ` label the Rust binding prefixes it with: the message IS
/// PostgreSQL's (`syntax error at or near "selct"`), and the label reached
/// the client's `message_primary`.
fn parse_error(e: pg_query::Error) -> Error {
    Error::Parse(match e {
        pg_query::Error::Parse(m) | pg_query::Error::Split(m) | pg_query::Error::Scan(m) => m,
        other => other.to_string(),
    })
}

pub fn split_statements(sql: &str) -> Result<Vec<String>> {
    let parts = pg_query::split_with_parser(sql).map_err(parse_error)?;
    Ok(parts
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

/// The parse tree of `sql`, from a process-wide memo. One statement is
/// parsed several times on its way through the server -- at Describe, at
/// Execute, and once more each for parameter typing -- and `pg_query` parses
/// through C and a protobuf round trip, which was the largest single cost of
/// an `executemany` once the catalog stopped being re-read (2026-09-09). The
/// text is the whole input to the parser, so the tree is a pure function of
/// it. Bounded by wholesale clearing: statement text in a loop repeats, and
/// a rebuilt memo costs one parse per distinct text.
fn parse_tree(sql: &str) -> Result<std::sync::Arc<pg_query::protobuf::ParseResult>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    const MAX_ENTRIES: usize = 4096;
    static MEMO: OnceLock<Mutex<HashMap<String, Arc<pg_query::protobuf::ParseResult>>>> =
        OnceLock::new();
    let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(tree) = memo.lock().unwrap_or_else(|e| e.into_inner()).get(sql) {
        return Ok(Arc::clone(tree));
    }
    let tree = Arc::new(pg_query::parse(sql).map_err(parse_error)?.protobuf);
    let mut guard = memo.lock().unwrap_or_else(|e| e.into_inner());
    if guard.len() >= MAX_ENTRIES {
        guard.clear();
    }
    guard.insert(sql.to_string(), Arc::clone(&tree));
    Ok(tree)
}

fn parse_one(sql: &str) -> Result<N> {
    let parsed = parse_tree(sql)?;
    let stmts = &parsed.stmts;
    if stmts.len() > 1 {
        return Err(Error::MultipleCommands);
    }
    if stmts.is_empty() {
        return Err(Error::Parse("empty statement".into()));
    }
    stmts[0]
        .stmt
        .as_ref()
        .and_then(|s| s.node.clone())
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
    joins::clear_planned_joins();
    plan_node(parse_one(sql)?, lookup, params)
}

/// `plan_with_params`, with a way to RUN an uncorrelated subquery.
///
/// Without the runner a `SubLink` reaches the lowering and is refused; with
/// it, every uncorrelated subquery is evaluated first and replaced by the
/// values it returned, so the rest of the planner is unchanged. Callers that
/// have no executor to hand (a CHECK constraint, a `DO` block's own parser)
/// keep using `plan_with_params` and keep the refusal.
pub fn plan_with_subqueries(
    sql: &str,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
    run: SubqueryRunner<'_>,
) -> Result<Statement> {
    let mut node = pg_query::protobuf::Node {
        node: Some(parse_one(sql)?),
    };
    // The resolved values are appended to the bound parameters as `$N`, so
    // the list the statement is finally planned with is longer than the one
    // the client bound.
    let mut params = params.to_vec();
    resolve_sublinks(&mut node, lookup, &mut params, run)?;
    plan_node(
        node.node
            .ok_or_else(|| Error::Parse("empty statement".into()))?,
        lookup,
        &params,
    )
}

fn plan_node(
    node: N,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<Statement> {
    match node {
        N::CreateStmt(c) => plan_create(&c),
        N::AlterTableStmt(a) => plan_alter_table(&a, lookup, params),
        N::CreateSeqStmt(c) => plan_create_sequence(&c),
        N::IndexStmt(i) => plan_create_index(&i, lookup, params),
        N::ViewStmt(v) => plan_create_view(&v),
        N::ExplainStmt(e) => plan_explain(&e, lookup, params),
        N::AlterSeqStmt(a) => plan_alter_sequence(&a),
        N::RenameStmt(r) => plan_rename(&r),
        N::CreateTableAsStmt(c) => plan_create_table_as(&c, lookup, params),
        N::InsertStmt(i) => plan_insert(&i, lookup, params),
        N::SelectStmt(s) => plan_select(&s, lookup, params),
        N::DropStmt(d) => plan_drop(&d),
        N::CreateSchemaStmt(c) => Ok(Statement::CreateSchema {
            name: c.schemaname.clone(),
            if_not_exists: c.if_not_exists,
        }),
        N::CreatedbStmt(c) => Ok(Statement::CreateDatabase {
            name: c.dbname.clone(),
        }),
        N::DropdbStmt(d) => Ok(Statement::DropDatabase {
            name: d.dbname.clone(),
            if_exists: d.missing_ok,
        }),
        // `CREATE TYPE name AS (field type, ...)` -- a composite type.
        N::CompositeTypeStmt(ct) => {
            let typevar = ct
                .typevar
                .as_ref()
                .ok_or_else(|| Error::Parse("CREATE TYPE without a name".into()))?;
            let name = typevar.relname.clone();
            let schema = if typevar.schemaname.is_empty() {
                None
            } else {
                Some(typevar.schemaname.clone())
            };
            let mut fields = Vec::new();
            for col in &ct.coldeflist {
                let Some(N::ColumnDef(cd)) = col.node.as_ref() else {
                    return Err(Error::Unsupported("this composite field".into()));
                };
                let ty = cd
                    .type_name
                    .as_ref()
                    .map(type_name_of)
                    .ok_or_else(|| Error::Parse("composite field without a type".into()))?;
                fields.push((cd.colname.clone(), ty));
            }
            Ok(Statement::CreateComposite {
                name,
                schema,
                fields,
            })
        }
        // `CREATE TYPE ... AS ENUM`. The name may be schema-qualified; with no
        // schema support the last part is the name, same rule as columns.
        N::CreateEnumStmt(e) => {
            // Keep the schema qualifier: `CREATE TYPE s.t AS ENUM` is a distinct
            // type from a bare `t`. Dropping it (a bare `.next_back()`) collided
            // the two.
            let (schema, name) = split_qualified_type_name(&e.type_name)?;
            let labels = e
                .vals
                .iter()
                .filter_map(|n| match n.node.as_ref()? {
                    N::String(s) => Some(s.sval.clone()),
                    _ => None,
                })
                .collect();
            Ok(Statement::CreateEnum {
                name,
                schema,
                labels,
            })
        }
        // `CREATE TYPE name AS RANGE (subtype = T)` -- a custom range type.
        N::CreateRangeStmt(r) => {
            // Keep the schema qualifier: `CREATE TYPE s.t AS RANGE` is a distinct
            // type from a bare `t`. Dropping it (a bare `.next_back()`) collided
            // the two.
            let (schema, name) = split_qualified_type_name(&r.type_name)?;
            // The subtype is a DefElem `subtype = <type>`; its arg is a TypeName.
            let subtype = r
                .params
                .iter()
                .find_map(|p| match p.node.as_ref()? {
                    N::DefElem(d) if d.defname.eq_ignore_ascii_case("subtype") => {
                        d.arg.as_ref().map(|a| type_name_of_node(a))
                    }
                    _ => None,
                })
                .flatten()
                .ok_or_else(|| Error::Unsupported("a RANGE type without a subtype".into()))?;
            Ok(Statement::CreateRange {
                name,
                schema,
                subtype,
            })
        }
        // `CREATE TYPE name` (shell) and `CREATE TYPE name (input = ..., output
        // = ...)` (base type) are both a DefineStmt of kind OBJECT_TYPE; the
        // other kinds (aggregate, operator, collation, ...) stay unsupported.
        N::DefineStmt(d) if ObjectType::try_from(d.kind) == Ok(ObjectType::ObjectType) => {
            plan_define_type(&d)
        }
        N::CreateFunctionStmt(f) => plan_create_function(&f),
        N::CreateTrigStmt(t) => plan_create_trigger(&t),
        N::CopyStmt(c) => plan_copy(&c, lookup, params),
        N::VariableShowStmt(v) => Ok(Statement::Show(v.name.clone())),
        N::AlterRoleStmt(a) => Ok(Statement::AlterRole {
            name: a
                .role
                .as_ref()
                .map(|r| r.rolename.clone())
                .unwrap_or_default(),
            options: role_options(&a.options)?,
        }),
        N::CreateRoleStmt(c) => {
            let mut options = role_options(&c.options)?;
            // CREATE USER is CREATE ROLE with LOGIN on by default.
            if options.login.is_none()
                && pg_query::protobuf::RoleStmtType::try_from(c.stmt_type)
                    == Ok(pg_query::protobuf::RoleStmtType::RolestmtUser)
            {
                options.login = Some(true);
            }
            Ok(Statement::CreateRole {
                name: c.role.clone(),
                options,
            })
        }
        N::DropRoleStmt(d) => Ok(Statement::DropRole {
            names: d
                .roles
                .iter()
                .filter_map(|n| match n.node.as_ref()? {
                    N::RoleSpec(r) => Some(r.rolename.clone()),
                    _ => None,
                })
                .collect(),
            if_exists: d.missing_ok,
        }),
        N::CreateExtensionStmt(c) => Ok(Statement::CreateExtension {
            name: c.extname.clone(),
            if_not_exists: c.if_not_exists,
        }),
        N::DeclareCursorStmt(d) => {
            let inner_node = d.query.as_ref().and_then(|q| q.node.as_ref());
            let inner = match inner_node {
                Some(N::SelectStmt(sel)) => plan_select(sel, lookup, params)?,
                Some(other) => return Err(Error::Unsupported(disc(other))),
                None => return Err(Error::Parse("DECLARE CURSOR without a query".into())),
            };
            // PostgreSQL's cursor-option bitmask (`nodes/parsenodes.h`).
            const CURSOR_OPT_BINARY: i32 = 0x0001;
            const CURSOR_OPT_NO_SCROLL: i32 = 0x0004;
            const CURSOR_OPT_HOLD: i32 = 0x0020;
            // Deparse the inner query for `pg_cursors.statement`. Best-effort:
            // an un-deparsable node leaves the column empty rather than failing
            // the DECLARE, which no client reads that column to check.
            let statement = inner_node
                .and_then(|n| n.deparse().ok())
                .unwrap_or_default();
            Ok(Statement::DeclareCursor {
                name: d.portalname.clone(),
                query: Box::new(inner),
                statement,
                scrollable: (d.options & CURSOR_OPT_NO_SCROLL) == 0,
                holdable: (d.options & CURSOR_OPT_HOLD) != 0,
                binary: (d.options & CURSOR_OPT_BINARY) != 0,
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
        N::DeallocateStmt(d) => Ok(Statement::Deallocate(d.name.clone())),
        // LISTEN / UNLISTEN / NOTIFY: the parser has already folded the
        // channel to lower case unless it was quoted, and `UNLISTEN *` comes
        // through as the literal name `*`.
        N::NotifyStmt(n) => Ok(Statement::Notify {
            channel: n.conditionname.clone(),
            payload: n.payload.clone(),
        }),
        N::ListenStmt(l) => Ok(Statement::Listen(l.conditionname.clone())),
        // `UNLISTEN *` carries no name at all in the parse tree.
        N::UnlistenStmt(u) if u.conditionname.is_empty() || u.conditionname == "*" => {
            Ok(Statement::Unlisten(None))
        }
        N::UnlistenStmt(u) => Ok(Statement::Unlisten(Some(u.conditionname.clone()))),
        N::DoStmt(d) => {
            let mut language = "plpgsql".to_string();
            let mut body = None;
            for node in &d.args {
                let Some(N::DefElem(e)) = node.node.as_ref() else {
                    continue;
                };
                let value = match e.arg.as_ref().and_then(|a| a.node.as_ref()) {
                    Some(N::String(sv)) => sv.sval.clone(),
                    _ => continue,
                };
                match e.defname.as_str() {
                    "as" => body = Some(value),
                    "language" => language = value,
                    _ => {}
                }
            }
            Ok(Statement::Do {
                language,
                body: body.unwrap_or_default(),
            })
        }
        N::VariableSetStmt(v) => plan_set(&v),
        N::TransactionStmt(t) => {
            // Named enum, not the wire integer -- twice bitten already.
            match TransactionStmtKind::try_from(t.kind) {
                Ok(TransactionStmtKind::TransStmtBegin) => Ok(Statement::Transaction(
                    TransactionControl::Begin(parse_transaction_modes(&t.options)),
                )),
                Ok(TransactionStmtKind::TransStmtStart) => Ok(Statement::Transaction(
                    TransactionControl::Start(parse_transaction_modes(&t.options)),
                )),
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
                Ok(TransactionStmtKind::TransStmtPrepare) => Ok(Statement::Transaction(
                    TransactionControl::Prepare(t.gid.clone()),
                )),
                Ok(TransactionStmtKind::TransStmtCommitPrepared) => Ok(Statement::Transaction(
                    TransactionControl::CommitPrepared(t.gid.clone()),
                )),
                Ok(TransactionStmtKind::TransStmtRollbackPrepared) => Ok(Statement::Transaction(
                    TransactionControl::RollbackPrepared(t.gid.clone()),
                )),
                Ok(other) => Err(Error::Unsupported(format!("{other:?}"))),
                Err(_) => Err(Error::Unsupported("this transaction statement".into())),
            }
        }
        N::UpdateStmt(u) => plan_update(&u, lookup, params),
        N::DeleteStmt(d) => plan_delete(&d, lookup, params),
        N::TruncateStmt(t) => plan_truncate(&t, lookup),
        other => Err(Error::Unsupported(disc(&other))),
    }
}

/// Split a possibly schema-qualified `CREATE TYPE` name (the statement's
/// `type_name` node list) into `(schema, bare_name)`. An unqualified name -- or
/// one explicitly in `public` -- yields `None`, so it registers under its bare
/// name (public is on the default search_path); `schema.name` keeps the schema
/// so it is a distinct type. A three-part `catalog.schema.name` keeps the last
/// two parts (the catalog is the current database).
fn split_qualified_type_name(
    names: &[pg_query::protobuf::Node],
) -> Result<(Option<String>, String)> {
    let parts: Vec<String> = names
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        })
        .collect();
    match parts.as_slice() {
        [] => Err(Error::Parse("CREATE TYPE without a name".into())),
        [name] => Ok((None, name.clone())),
        [schema, name] if schema == "public" => Ok((None, name.clone())),
        [schema, name] => Ok((Some(schema.clone()), name.clone())),
        _ => {
            let name = parts.last().expect("non-empty").clone();
            let schema = parts[parts.len() - 2].clone();
            Ok(((schema != "public").then_some(schema), name))
        }
    }
}

/// `CREATE TYPE name` (a shell) or `CREATE TYPE name (input = f, output = g,
/// like = t)` (a base type over its shell). Both arrive as a `DefineStmt` of
/// kind OBJECT_TYPE; an empty option list is the shell form.
fn plan_define_type(d: &pg_query::protobuf::DefineStmt) -> Result<Statement> {
    let (schema, name) = split_qualified_type_name(&d.defnames)?;
    if d.definition.is_empty() {
        return Ok(Statement::CreateShellType { name, schema });
    }
    let mut input = None;
    let mut output = None;
    for opt in &d.definition {
        let Some(N::DefElem(e)) = opt.node.as_ref() else {
            return Err(Error::Parse("malformed CREATE TYPE option".into()));
        };
        let value = e.arg.as_ref().and_then(|a| type_name_of_node(a));
        match e.defname.to_ascii_lowercase().as_str() {
            "input" => input = value,
            "output" => output = value,
            // `like = text` copies the representation (typlen / alignment /
            // storage), none of which this server surfaces: a base type is
            // carried as its text form whatever it is `like`.
            "like" => {}
            other => {
                return Err(Error::Unsupported(format!(
                    "the CREATE TYPE option \"{other}\""
                )))
            }
        }
    }
    Ok(Statement::CreateBaseType {
        name,
        schema,
        input,
        output,
    })
}

/// `CREATE FUNCTION name(args) RETURNS t LANGUAGE internal AS '<builtin>'`.
/// Only the internal language is planned -- the catalog registration is what a
/// base type's `input = ` / `output = ` options resolve against. A function
/// in any other language is refused, since nothing here could run it.
/// `CREATE TRIGGER`. `timing` and `events` are PostgreSQL's `TRIGGER_TYPE_*`
/// bits: BEFORE 2, INSERT 4, DELETE 8, UPDATE 16, TRUNCATE 32, INSTEAD 64.
fn plan_create_trigger(t: &pg_query::protobuf::CreateTrigStmt) -> Result<Statement> {
    if t.isconstraint {
        return Err(Error::Unsupported("CREATE CONSTRAINT TRIGGER".into()));
    }
    if !t.transition_rels.is_empty() {
        return Err(Error::Unsupported(
            "a trigger's REFERENCING transition tables".into(),
        ));
    }
    let timing = match t.timing {
        2 => "BEFORE",
        64 => "INSTEAD OF",
        _ => "AFTER",
    };
    if timing == "INSTEAD OF" {
        return Err(Error::Unsupported("INSTEAD OF triggers".into()));
    }
    let mut events = Vec::new();
    for (bit, name) in [
        (4, "INSERT"),
        (8, "DELETE"),
        (16, "UPDATE"),
        (32, "TRUNCATE"),
    ] {
        if t.events & bit != 0 {
            events.push(name.to_string());
        }
    }
    if t.row && events.iter().any(|e| e == "TRUNCATE") {
        return Err(Error::FeatureNotSupported(
            "TRUNCATE FOR EACH ROW triggers are not supported".into(),
        ));
    }
    let table = t
        .relation
        .as_ref()
        .map(relation_name)
        .ok_or_else(|| Error::Parse("CREATE TRIGGER without a table".into()))?;
    let function = t
        .funcname
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        })
        .next_back()
        .ok_or_else(|| Error::Parse("CREATE TRIGGER without a function".into()))?;
    let strings = |nodes: &[pg_query::protobuf::Node]| -> Vec<String> {
        nodes
            .iter()
            .filter_map(|n| match n.node.as_ref()? {
                N::String(s) => Some(s.sval.clone()),
                _ => None,
            })
            .collect()
    };
    let when = match t.when_clause.as_deref() {
        Some(w) => {
            if !t.row && references_columns(w) {
                return Err(Error::Sqlstate(
                    "42P17",
                    "statement trigger's WHEN condition cannot reference column values".into(),
                ));
            }
            Some(deparse_expr(w)?)
        }
        None => None,
    };
    Ok(Statement::CreateTrigger(TriggerDef {
        name: t.trigname.clone(),
        table,
        replace: t.replace,
        timing: timing.to_string(),
        events,
        update_columns: strings(&t.columns),
        level: if t.row { "ROW" } else { "STATEMENT" }.to_string(),
        function,
        args: strings(&t.args),
        when,
    }))
}

fn plan_create_function(f: &pg_query::protobuf::CreateFunctionStmt) -> Result<Statement> {
    if f.is_procedure {
        return Err(Error::Unsupported("CREATE PROCEDURE".into()));
    }
    let name = f
        .funcname
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        })
        .next_back()
        .ok_or_else(|| Error::Parse("CREATE FUNCTION without a name".into()))?;
    let mut arg_types = Vec::new();
    for p in &f.parameters {
        let Some(N::FunctionParameter(fp)) = p.node.as_ref() else {
            return Err(Error::Parse("malformed CREATE FUNCTION parameter".into()));
        };
        let ty = fp
            .arg_type
            .as_ref()
            .map(type_name_of)
            .ok_or_else(|| Error::Parse("CREATE FUNCTION parameter without a type".into()))?;
        arg_types.push(ty);
    }
    let return_type = f
        .return_type
        .as_ref()
        .map(type_name_of)
        .ok_or_else(|| Error::Unsupported("CREATE FUNCTION without RETURNS".into()))?;
    let mut language = None;
    let mut body = None;
    let mut volatility = "volatile".to_string();
    for opt in &f.options {
        let Some(N::DefElem(e)) = opt.node.as_ref() else {
            continue;
        };
        let text = e.arg.as_ref().and_then(|a| type_name_of_node(a));
        match e.defname.to_ascii_lowercase().as_str() {
            "language" => language = text,
            "as" => body = text,
            "volatility" => volatility = text.unwrap_or(volatility),
            _ => {}
        }
    }
    let language = language.unwrap_or_default().to_ascii_lowercase();
    if matches!(language.as_str(), "sql" | "plpgsql") {
        if f.sql_body.is_some() {
            return Err(Error::Unsupported(
                "a SQL-standard function body (BEGIN ATOMIC / RETURN)".into(),
            ));
        }
        let mut params = Vec::new();
        let mut columns = Vec::new();
        for p in &f.parameters {
            let Some(N::FunctionParameter(fp)) = p.node.as_ref() else {
                continue;
            };
            let ty = fp.arg_type.as_ref().map(type_name_of).unwrap_or_default();
            use pg_query::protobuf::FunctionParameterMode as M;
            match M::try_from(fp.mode) {
                Ok(M::FuncParamOut | M::FuncParamTable) => columns.push((fp.name.clone(), ty)),
                Ok(M::FuncParamInout) => {
                    params.push((fp.name.clone(), ty.clone()));
                    columns.push((fp.name.clone(), ty));
                }
                Ok(M::FuncParamVariadic) => {
                    return Err(Error::Unsupported("a VARIADIC parameter".into()))
                }
                _ => params.push((fp.name.clone(), ty)),
            }
        }
        let returns_set = f.return_type.as_ref().is_some_and(|t| t.setof);
        let body = body.ok_or_else(|| Error::Parse("no function body specified".into()))?;
        return Ok(Statement::CreateUserFunction(UserFunctionDef {
            name,
            replace: f.replace,
            params,
            return_type,
            returns_set,
            columns,
            body,
            language,
            volatility,
        }));
    }
    if !language.eq_ignore_ascii_case("internal") {
        return Err(Error::Unsupported(format!(
            "CREATE FUNCTION in language \"{language}\""
        )));
    }
    let body = body.ok_or_else(|| Error::Parse("no function body specified".into()))?;
    Ok(Statement::CreateFunction {
        name,
        replace: f.replace,
        arg_types,
        return_type,
        body,
        volatility,
    })
}

/// Resolve a `serial` pseudo-type to its underlying integer type.
///
/// PostgreSQL's `smallserial`/`serial`/`bigserial` (and their `serial2`/
/// `serial4`/`serial8` aliases) are not real types: the parser rewrites them
/// to `int2`/`int4`/`int8` with an attached sequence default. We keep the
/// integer type so the column behaves as an integer everywhere; the implicit
/// sequence default is a separate feature. Any other name passes through.
fn normalize_serial(ty: &str) -> String {
    match ty.to_ascii_lowercase().as_str() {
        "smallserial" | "serial2" => "int2".to_string(),
        "serial" | "serial4" => "int4".to_string(),
        "bigserial" | "serial8" => "int8".to_string(),
        _ => ty.to_string(),
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

/// The integer value of a `typmods` node. libpg_query renders a declared
/// modifier -- including a negative scale like `numeric(2,-3)` -- as an
/// `A_Const` integer literal directly, so no sign reconstruction is needed.
fn typmod_ival(node: &pg_query::protobuf::Node) -> Option<i32> {
    match node.node.as_ref()? {
        N::AConst(c) => match c.val.as_ref()? {
            pg_query::protobuf::a_const::Val::Ival(v) => Some(v.ival),
            _ => None,
        },
        _ => None,
    }
}

/// PostgreSQL's wire type-modifier (`atttypmod`) for a declared cast type such
/// as `varchar(10)` (-> 14) or `numeric(10,2)` (-> 655366), or `-1` for a bare
/// type or one whose modifier this server does not encode.
///
/// DESCRIPTION metadata only: it feeds the `RowDescription`, from which clients
/// derive `precision` / `scale` / `display_size`. It never changes how a value
/// is decoded.
fn cast_typmod(node: &pg_query::protobuf::Node) -> i32 {
    let Some(N::TypeCast(tc)) = node.node.as_ref() else {
        return -1;
    };
    let Some(tn) = tc.type_name.as_ref() else {
        return -1;
    };
    declared_typmod(tn)
}

/// The `atttypmod` a declared type carries, or -1 for an unmodified type.
fn declared_typmod(tn: &pg_query::protobuf::TypeName) -> i32 {
    let mods: Vec<i32> = tn.typmods.iter().filter_map(typmod_ival).collect();
    // A modifier we could not read as an integer means no faithful typmod.
    if mods.len() != tn.typmods.len() {
        return -1;
    }
    match type_name(&tn.names).to_ascii_lowercase().as_str() {
        // numeric(p,s): ((p << 16) | (s & 0x7FF)) + VARHDRSZ; bare precision
        // implies scale 0. The low 11 bits hold a signed scale (PG15+).
        "numeric" | "decimal" => match mods.as_slice() {
            [p] => (p << 16) + 4,
            [p, s] => ((p << 16) | (s & 0x7FF)) + 4,
            _ => -1,
        },
        // varchar(n)/char(n): declared length plus the varlena header.
        "varchar" | "character varying" | "bpchar" | "char" | "character" => {
            match mods.as_slice() {
                [n] => n + 4,
                _ => -1,
            }
        }
        // bit(n)/varbit(n): the length itself, no header.
        "bit" | "varbit" => match mods.as_slice() {
            [n] => *n,
            _ => -1,
        },
        // time/timestamp precision is the whole modifier.
        "time" | "timetz" | "timestamp" | "timestamptz" => match mods.as_slice() {
            [p] => *p,
            _ => -1,
        },
        // interval carries its typmod as [range-field mask, precision]:
        // `interval(6)` is [32767, 6] packed as (range << 16) | precision. A
        // lone precision (no field list) takes the full-range mask.
        "interval" => match mods.as_slice() {
            [range, prec] => (range << 16) | prec,
            [prec] => (0x7FFF << 16) | prec,
            _ => -1,
        },
        _ => -1,
    }
}

/// The type name inside a DefElem arg (`subtype = int4`): a `TypeName` node,
/// or a bare String/TypeName-list. Returns the bare PostgreSQL name.
fn type_name_of_node(node: &pg_query::protobuf::Node) -> Option<String> {
    match node.node.as_ref()? {
        N::TypeName(tn) => Some(type_name(&tn.names)),
        N::String(s) => Some(s.sval.clone()),
        N::List(l) => l.items.iter().rev().find_map(|n| match n.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        }),
        _ => None,
    }
}

fn type_name(names: &[pg_query::protobuf::Node]) -> String {
    // libpg_query qualifies built-ins as pg_catalog.<name>; the catalog stores
    // the bare PostgreSQL name (`int4`, `text`). A USER schema stays on the
    // name -- `testschema.testrange` is a different type from `testrange`,
    // and the registries key it in `canonical_type_ref` form.
    let parts: Vec<&str> = names
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            N::String(s) => Some(s.sval.as_str()),
            _ => None,
        })
        .collect();
    match parts.as_slice() {
        [] => String::new(),
        [n] | ["pg_catalog" | "public", n] => (*n).to_string(),
        [schema, n] => canonical_type_ref(&format!("{schema}.{n}")),
        other => other.join("."),
    }
}

/// `CREATE TABLE ... AS <query>`. Only a table target: a materialized view
/// (`objtype` OBJECT_MATVIEW) is a refreshable relation this server has no
/// catalog for, and `SELECT ... INTO` is the same statement in older clothes.
fn plan_create_table_as(
    c: &pg_query::protobuf::CreateTableAsStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<Statement> {
    use pg_query::protobuf::ObjectType;
    if c.objtype != ObjectType::ObjectTable as i32 {
        return Err(Error::Unsupported("CREATE MATERIALIZED VIEW".into()));
    }
    let into = c
        .into
        .as_ref()
        .ok_or_else(|| Error::Parse("CREATE TABLE AS without a target".into()))?;
    let rel = into
        .rel
        .as_ref()
        .ok_or_else(|| Error::Parse("CREATE TABLE AS without a relation".into()))?;
    let query = match c.query.as_ref().and_then(|q| q.node.as_ref()) {
        Some(N::SelectStmt(sel)) => plan_select(sel, lookup, params)?,
        Some(other) => return Err(Error::Unsupported(disc(other))),
        None => return Err(Error::Parse("CREATE TABLE AS without a query".into())),
    };
    Ok(Statement::CreateTableAs {
        table: rel.relname.clone(),
        if_not_exists: c.if_not_exists,
        temp: rel.relpersistence == "t",
        column_names: string_list(&into.col_names),
        query: Box::new(query),
        with_data: !into.skip_data,
    })
}

/// `ALTER TABLE <t> <action>, ...`.
///
/// Planned against the table's CURRENT def, and each action against the def
/// the ones before it produced -- `add column x int, alter column x set
/// default 0` is legal in PostgreSQL and needs the second action to see the
/// first one's column.
fn plan_alter_table(
    a: &pg_query::protobuf::AlterTableStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<Statement> {
    let relation = a
        .relation
        .as_ref()
        .ok_or_else(|| Error::Parse("ALTER TABLE without a relation".into()))?;
    let table = relation.relname.clone();
    // Only tables. `ALTER INDEX` / `ALTER VIEW` / `ALTER SEQUENCE` parse to
    // the same node with a different `objtype`, and answering them as though
    // they were table alterations would be worse than refusing them.
    if a.objtype != ObjectType::ObjectTable as i32 {
        return Err(Error::Unsupported(format!(
            "ALTER {} ",
            object_type_word(a.objtype)
        )));
    }
    let Some(mut def) = lookup(&table) else {
        if a.missing_ok {
            return Ok(Statement::AlterTable {
                table,
                missing_ok: true,
                actions: Vec::new(),
            });
        }
        return Err(Error::UndefinedTable(table));
    };

    let mut actions = Vec::new();
    for cmd in &a.cmds {
        let Some(N::AlterTableCmd(cmd)) = cmd.node.as_ref() else {
            return Err(Error::Unsupported("this ALTER TABLE action".into()));
        };
        let action = plan_alter_action(cmd, &table, &def, params)?;
        // Apply it to the working def so the NEXT action sees it.
        apply_alter_to_def(&mut def, &action);
        actions.push(action);
    }
    Ok(Statement::AlterTable {
        table,
        missing_ok: a.missing_ok,
        actions,
    })
}

/// Whether `ALTER COLUMN ... TYPE` converts from `from` to `to` without a
/// `USING` clause.
///
/// PostgreSQL allows it exactly where an ASSIGNMENT cast exists, which is not
/// the same as "the values happen to convert". Measured on 14.24 across 31
/// type pairs, the rule is:
///
/// * to a STRING type -- always (everything has an assignment cast to text,
///   so `json`, `bytea`, `date` and `int` all convert);
/// * within the NUMERIC family, both directions;
/// * within the DATE/TIME family, both directions;
/// * `json` and `jsonb`, both directions;
/// * a type to itself.
///
/// Everything else needs `USING`, and PostgreSQL answers 42804 -- notably
/// FROM a string to anything but a string (`text -> int`, `text -> date`,
/// `varchar -> int`), `bool` to or from `int`, and a scalar to an array.
fn alter_type_is_automatic(from: &str, to: &str) -> bool {
    const STRINGS: &[&str] = &["text", "varchar", "bpchar", "name", "char"];
    const NUMBERS: &[&str] = &["int2", "int4", "int8", "numeric", "float4", "float8"];
    const DATETIMES: &[&str] = &[
        "date",
        "timestamp",
        "timestamptz",
        "time",
        "timetz",
        "interval",
    ];
    if from == to || STRINGS.contains(&to) {
        return true;
    }
    let both = |set: &[&str]| set.contains(&from) && set.contains(&to);
    both(NUMBERS) || both(DATETIMES) || both(&["json", "jsonb"])
}

/// The word PostgreSQL uses for an object type in `ALTER <word>`, for a
/// refusal that names what was actually asked for.
fn object_type_word(objtype: i32) -> &'static str {
    match ObjectType::try_from(objtype) {
        Ok(ObjectType::ObjectIndex) => "INDEX",
        Ok(ObjectType::ObjectView) => "VIEW",
        Ok(ObjectType::ObjectMatview) => "MATERIALIZED VIEW",
        Ok(ObjectType::ObjectSequence) => "SEQUENCE",
        Ok(ObjectType::ObjectForeignTable) => "FOREIGN TABLE",
        _ => "that object",
    }
}

fn plan_alter_action(
    cmd: &pg_query::protobuf::AlterTableCmd,
    table: &str,
    def: &TableDef,
    params: &[Bson],
) -> Result<AlterTableAction> {
    use pg_query::protobuf::AlterTableType as AT;
    use pg_query::protobuf::ConstrType as CT;
    match AT::try_from(cmd.subtype) {
        Ok(AT::AtAddColumn) => {
            let Some(N::ColumnDef(cd)) = cmd.def.as_ref().and_then(|d| d.node.as_ref()) else {
                return Err(Error::Parse("ADD COLUMN without a column".into()));
            };
            Ok(AlterTableAction::AddColumn {
                column: plan_added_column(cd, params)?,
                if_not_exists: cmd.missing_ok,
            })
        }
        Ok(AT::AtDropColumn) => Ok(AlterTableAction::DropColumn {
            name: cmd.name.clone(),
            if_exists: cmd.missing_ok,
        }),
        Ok(AT::AtColumnDefault) => {
            // `DROP DEFAULT` is the same node with no expression.
            let value = match cmd.def.as_ref() {
                None => None,
                Some(raw) => {
                    let column = def
                        .column(&cmd.name)
                        .ok_or_else(|| Error::UndefinedColumn(cmd.name.clone()))?;
                    // SET DEFAULT rewrites no rows, so an expression default
                    // needs nothing evaluated now.
                    Some(default_value_or_expr(raw, &column.pg_type, params)?)
                }
            };
            let (value, expr) = match value {
                None => (None, None),
                Some(DefaultSpec::Value(v)) => (Some(v), None),
                Some(DefaultSpec::Expr(e)) => (None, Some(e)),
            };
            Ok(AlterTableAction::SetDefault {
                column: cmd.name.clone(),
                value,
                expr,
            })
        }
        Ok(AT::AtSetNotNull) => Ok(AlterTableAction::SetNotNull {
            column: cmd.name.clone(),
            not_null: true,
        }),
        Ok(AT::AtDropNotNull) => Ok(AlterTableAction::SetNotNull {
            column: cmd.name.clone(),
            not_null: false,
        }),
        Ok(AT::AtAlterColumnType) => {
            let Some(N::ColumnDef(cd)) = cmd.def.as_ref().and_then(|d| d.node.as_ref()) else {
                return Err(Error::Parse("ALTER COLUMN TYPE without a type".into()));
            };
            // `USING <expr>` rewrites the value rather than casting it, which
            // is a different conversion; refused rather than silently cast.
            if cd.raw_default.is_some() {
                return Err(Error::Unsupported("ALTER COLUMN TYPE ... USING".into()));
            }
            let ty = cd
                .type_name
                .as_ref()
                .map(type_name_of)
                .ok_or_else(|| Error::Parse("ALTER COLUMN TYPE without a type".into()))?;
            let ty = normalize_serial(&ty);
            let current = def
                .column(&cmd.name)
                .ok_or_else(|| Error::UndefinedColumn(cmd.name.clone()))?;
            // PostgreSQL decides this from the TYPES alone, before looking at
            // a single row: the conversion is allowed only where an
            // assignment cast exists, and otherwise the statement is refused
            // whatever the data happens to be. Casting per row instead made
            // `text -> int` succeed on a table whose values were all digits
            // and answer `22P02` on one whose values were not -- neither of
            // which is what PostgreSQL does.
            if !alter_type_is_automatic(&current.pg_type, &ty) {
                return Err(Error::DatatypeMismatch(format!(
                    "column \"{}\" cannot be cast automatically to type {}",
                    cmd.name,
                    display_type(&ty)
                )));
            }
            Ok(AlterTableAction::AlterType {
                column: cmd.name.clone(),
                pg_type: ty,
                typmod: cd.type_name.as_ref().map(declared_typmod).unwrap_or(-1),
            })
        }
        Ok(AT::AtAddConstraint) => {
            let Some(N::Constraint(k)) = cmd.def.as_ref().and_then(|d| d.node.as_ref()) else {
                return Err(Error::Parse("ADD CONSTRAINT without a constraint".into()));
            };
            if CT::try_from(k.contype) != Ok(CT::ConstrCheck) {
                // UNIQUE / PRIMARY KEY / FOREIGN KEY added after the fact each
                // need an index built over the rows already there, which is
                // the `CREATE INDEX` work rather than this.
                return Err(Error::Unsupported(
                    "ALTER TABLE ADD CONSTRAINT of this kind".into(),
                ));
            }
            let raw = k
                .raw_expr
                .as_ref()
                .ok_or_else(|| Error::Parse("CHECK without an expression".into()))?;
            let expression = check_expression_text(raw)?;
            // Planned now, so an unknown column is 42703 at ALTER rather than
            // on the next INSERT.
            plan_check_expression(&expression, def)?;
            let name = if k.conname.is_empty() {
                let named = columns_referenced(raw, def);
                let base = match named.as_slice() {
                    [one] => format!("{table}_{one}_check"),
                    _ => format!("{table}_check"),
                };
                let mut candidate = base.clone();
                let mut n = 1;
                while def.check_constraints.iter().any(|c| c.name == candidate) {
                    candidate = format!("{base}{n}");
                    n += 1;
                }
                candidate
            } else {
                k.conname.clone()
            };
            Ok(AlterTableAction::AddCheck(CheckConstraint {
                name,
                expression,
            }))
        }
        Ok(AT::AtDropConstraint) => Ok(AlterTableAction::DropConstraint {
            name: cmd.name.clone(),
            if_exists: cmd.missing_ok,
        }),
        // Everything else -- OWNER, SET STATISTICS, CLUSTER, inheritance,
        // partitioning -- is named rather than lumped under one refusal, so
        // the message says which action was not understood.
        Ok(other) => Err(Error::Unsupported(format!(
            "ALTER TABLE {}",
            alter_action_word(other)
        ))),
        Err(_) => Err(Error::Unsupported("this ALTER TABLE action".into())),
    }
}

fn alter_action_word(t: pg_query::protobuf::AlterTableType) -> &'static str {
    use pg_query::protobuf::AlterTableType as AT;
    match t {
        AT::AtChangeOwner => "OWNER TO",
        AT::AtSetStatistics => "ALTER COLUMN ... SET STATISTICS",
        AT::AtSetStorage => "ALTER COLUMN ... SET STORAGE",
        AT::AtClusterOn => "CLUSTER ON",
        AT::AtAddIndex | AT::AtAddIndexConstraint => "ADD INDEX",
        AT::AtValidateConstraint => "VALIDATE CONSTRAINT",
        AT::AtAlterConstraint => "ALTER CONSTRAINT",
        AT::AtAddInherit | AT::AtDropInherit => "INHERIT",
        AT::AtAttachPartition | AT::AtDetachPartition => "PARTITION",
        AT::AtEnableTrig | AT::AtDisableTrig => "TRIGGER",
        AT::AtEnableRowSecurity | AT::AtDisableRowSecurity => "ROW LEVEL SECURITY",
        AT::AtSetExpression | AT::AtDropExpression => "ALTER COLUMN ... EXPRESSION",
        _ => "this action",
    }
}

/// A column added by `ALTER TABLE ADD COLUMN`.
///
/// Deliberately narrower than `CREATE TABLE`'s column: a PRIMARY KEY, UNIQUE,
/// REFERENCES or `serial` added after the fact each need an index or a
/// sequence built over rows that already exist, and answering the statement
/// without building it would leave the catalog claiming a constraint nothing
/// enforces.
fn plan_added_column(cd: &pg_query::protobuf::ColumnDef, params: &[Bson]) -> Result<Column> {
    use pg_query::protobuf::ConstrType as CT;
    let ty = cd.type_name.as_ref().map(type_name_of).unwrap_or_default();
    if normalize_serial(&ty) != ty {
        return Err(Error::Unsupported(
            "ALTER TABLE ADD COLUMN of a serial column".into(),
        ));
    }
    let mut column = Column::new(&cd.colname, &ty, false);
    column.typmod = cd.type_name.as_ref().map(declared_typmod).unwrap_or(-1);
    for con in &cd.constraints {
        let Some(N::Constraint(k)) = con.node.as_ref() else {
            continue;
        };
        match CT::try_from(k.contype) {
            Ok(CT::ConstrNotnull) => column.nullable = false,
            Ok(CT::ConstrNull) => column.nullable = true,
            Ok(CT::ConstrDefault) => {
                let raw = k
                    .raw_expr
                    .as_ref()
                    .ok_or_else(|| Error::Parse("DEFAULT without an expression".into()))?;
                // An expression default is evaluated per EXISTING row by the
                // executor's rewrite -- `gen_random_uuid()` gives each row its
                // own, as PostgreSQL does -- and per inserted row after.
                match default_value_or_expr(raw, &ty, params)? {
                    DefaultSpec::Value(v) => column.default = Some(v),
                    DefaultSpec::Expr(e) => column.set_default_expr(Some(e)),
                }
            }
            Ok(CT::ConstrPrimary) => {
                return Err(Error::Unsupported(
                    "ALTER TABLE ADD COLUMN ... PRIMARY KEY".into(),
                ))
            }
            Ok(CT::ConstrUnique) => {
                return Err(Error::Unsupported(
                    "ALTER TABLE ADD COLUMN ... UNIQUE".into(),
                ))
            }
            Ok(CT::ConstrForeign) => {
                return Err(Error::Unsupported(
                    "ALTER TABLE ADD COLUMN ... REFERENCES".into(),
                ))
            }
            Ok(CT::ConstrCheck) => {
                return Err(Error::Unsupported(
                    "ALTER TABLE ADD COLUMN ... CHECK".into(),
                ))
            }
            _ => {}
        }
    }
    Ok(column)
}

/// An EXPRESSION as SQL text. pg_query deparses only whole statements, so the
/// expression rides a `SELECT` whose prefix is cut off again.
pub(crate) fn deparse_expr(node: &pg_query::protobuf::Node) -> Result<String> {
    let select = pg_query::protobuf::Node {
        node: Some(N::SelectStmt(Box::new(pg_query::protobuf::SelectStmt {
            target_list: vec![pg_query::protobuf::Node {
                node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                    val: Some(Box::new(node.clone())),
                    location: -1,
                    ..Default::default()
                }))),
            }],
            limit_option: pg_query::protobuf::LimitOption::Default as i32,
            op: pg_query::protobuf::SetOperation::SetopNone as i32,
            ..Default::default()
        }))),
    };
    let text = select.deparse().map_err(|e| Error::Parse(e.to_string()))?;
    Ok(text
        .strip_prefix("SELECT ")
        .map(str::to_string)
        .unwrap_or(text))
}

/// A column DEFAULT as planned: a value folded now, or an expression kept as
/// SQL text for the executor to evaluate per row.
pub enum DefaultSpec {
    Value(Bson),
    Expr(String),
}

/// Fold a DEFAULT to a value when it is a constant; keep it as SQL when it is
/// volatile (`now()`, `nextval`, `gen_random_uuid()`) or does not fold, so it
/// is evaluated per row -- where PostgreSQL evaluates it -- rather than frozen
/// at CREATE time.
fn default_value_or_expr(
    raw: &pg_query::protobuf::Node,
    pg_type: &str,
    params: &[Bson],
) -> Result<DefaultSpec> {
    let as_expr = || -> Result<DefaultSpec> { Ok(DefaultSpec::Expr(deparse_expr(raw)?)) };
    if default_is_volatile(raw) {
        return as_expr();
    }
    match const_value(raw, params) {
        Ok(v) => Ok(DefaultSpec::Value(cast_value(v, pg_type)?)),
        Err(Error::Unsupported(_)) => as_expr(),
        Err(e) => Err(e),
    }
}

/// Apply one action to a def, so the next action in the same statement -- and
/// the executor -- see the same shape.
pub fn apply_alter_to_def(def: &mut TableDef, action: &AlterTableAction) {
    match action {
        AlterTableAction::AddColumn { column, .. } => {
            if def.column(&column.name).is_none() {
                def.columns.push(column.clone());
            }
        }
        AlterTableAction::DropColumn { name, .. } => {
            def.columns.retain(|c| c.name != *name);
            // A constraint over the dropped column goes with it, which is what
            // PostgreSQL does for a CHECK naming only that column.
            def.check_constraints
                .retain(|c| !constraint_mentions(&c.expression, name));
            def.unique_constraints
                .retain(|u| !u.columns.iter().any(|c| c == name));
            def.foreign_keys
                .retain(|f| !f.columns.iter().any(|c| c == name));
        }
        AlterTableAction::SetDefault {
            column,
            value,
            expr,
        } => {
            if let Some(c) = def.columns.iter_mut().find(|c| c.name == *column) {
                c.default = value.clone();
                c.set_default_expr(expr.clone());
            }
        }
        AlterTableAction::SetNotNull { column, not_null } => {
            if let Some(c) = def.columns.iter_mut().find(|c| c.name == *column) {
                c.nullable = !not_null;
            }
        }
        AlterTableAction::AlterType {
            column,
            pg_type,
            typmod,
        } => {
            if let Some(c) = def.columns.iter_mut().find(|c| c.name == *column) {
                c.pg_type = pg_type.clone();
                c.typmod = *typmod;
            }
        }
        AlterTableAction::AddCheck(check) => {
            def.check_constraints.push(check.clone());
            def.check_constraints.sort_by(|a, b| a.name.cmp(&b.name));
        }
        AlterTableAction::DropConstraint { name, .. } => {
            def.check_constraints.retain(|c| c.name != *name);
            def.unique_constraints.retain(|u| u.name != *name);
            def.foreign_keys.retain(|f| f.name != *name);
        }
    }
}

/// Whether a CHECK's SQL text names `column`, as a whole identifier.
///
/// A substring test would drop `check (nn > 0)` when column `n` is dropped,
/// so the match is bounded by non-identifier characters on both sides.
fn constraint_mentions(expression: &str, column: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let bytes: Vec<char> = expression.chars().collect();
    let target: Vec<char> = column.chars().collect();
    if target.is_empty() {
        return false;
    }
    for i in 0..bytes.len() {
        if bytes[i..].starts_with(target.as_slice()) {
            let before_ok = i == 0 || !ident(bytes[i - 1]);
            let after = i + target.len();
            let after_ok = after >= bytes.len() || !ident(bytes[after]);
            if before_ok && after_ok {
                return true;
            }
        }
    }
    false
}

/// `ALTER TABLE ... RENAME TO` and `... RENAME COLUMN ... TO`.
fn plan_rename(r: &pg_query::protobuf::RenameStmt) -> Result<Statement> {
    let relation = r
        .relation
        .as_ref()
        .ok_or_else(|| Error::Parse("RENAME without a relation".into()))?;
    let table = relation.relname.clone();
    match ObjectType::try_from(r.rename_type) {
        Ok(ObjectType::ObjectTable) => Ok(Statement::RenameTable {
            table,
            to: r.newname.clone(),
            missing_ok: r.missing_ok,
        }),
        Ok(ObjectType::ObjectColumn) => Ok(Statement::RenameColumn {
            table,
            column: r.subname.clone(),
            to: r.newname.clone(),
            missing_ok: r.missing_ok,
        }),
        Ok(ObjectType::ObjectTabconstraint) => Err(Error::Unsupported(
            "ALTER TABLE ... RENAME CONSTRAINT".into(),
        )),
        _ => Err(Error::Unsupported("this RENAME".into())),
    }
}

/// The options a `CREATE`/`ALTER SEQUENCE` carries, each `None` when the
/// statement did not name it.
///
/// `ALTER` applies only what it names, so every field has to be optional --
/// `alter sequence s increment by 10` must not reset the start or the
/// current value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SequenceOptions {
    pub start: Option<i64>,
    pub increment: Option<i64>,
    pub min_value: Option<i64>,
    pub max_value: Option<i64>,
    pub cycle: Option<bool>,
    /// `RESTART` / `RESTART WITH n`: sets the current value and un-calls the
    /// sequence, so the NEXT `nextval` returns it rather than one past it.
    pub restart: Option<Option<i64>>,
    /// `OWNED BY t.c`, or `OWNED BY NONE` as an empty string.
    pub owned_by: Option<String>,
}

/// `CREATE SEQUENCE` / `ALTER SEQUENCE` options, read off the parser's
/// `DefElem` list.
fn plan_sequence_options(options: &[pg_query::protobuf::Node]) -> Result<SequenceOptions> {
    let mut out = SequenceOptions::default();
    for opt in options {
        let Some(N::DefElem(d)) = opt.node.as_ref() else {
            continue;
        };
        // A sequence option's argument is a BARE `Integer` / `Float` node,
        // not the `A_Const` an expression would be -- `const_value` answered
        // `0A000 Integer is not supported yet` for every `START 10` and
        // `RESTART WITH 5`.
        let value = || -> Result<Option<i64>> {
            let Some(node) = d.arg.as_deref() else {
                return Ok(None);
            };
            match node.node.as_ref() {
                Some(N::Integer(i)) => Ok(Some(i64::from(i.ival))),
                Some(N::Float(f)) => f.fval.parse::<i64>().map(Some).map_err(|_| {
                    Error::Unsupported(format!("a non-integer {} for a sequence", d.defname))
                }),
                _ => match const_value(node, &[])? {
                    Bson::Int32(v) => Ok(Some(i64::from(v))),
                    Bson::Int64(v) => Ok(Some(v)),
                    Bson::Null => Ok(None),
                    _ => Err(Error::Unsupported(format!(
                        "a non-integer {} for a sequence",
                        d.defname
                    ))),
                },
            }
        };
        match d.defname.as_str() {
            "start" => out.start = value()?,
            "increment" => out.increment = value()?,
            // `NO MINVALUE` / `NO MAXVALUE` arrive as the same option with no
            // argument, which is what `None` from `value()` means -- so the
            // bound falls back to the type's limit rather than being left as
            // it was.
            "minvalue" => out.min_value = value()?,
            "maxvalue" => out.max_value = value()?,
            "cycle" => {
                out.cycle = Some(match d.arg.as_ref().and_then(|n| n.node.as_ref()) {
                    None => true,
                    Some(N::Integer(i)) => i.ival != 0,
                    Some(N::Boolean(b)) => b.boolval,
                    _ => true,
                })
            }
            "restart" => out.restart = Some(value()?),
            "owned_by" => {
                // A list of name parts: `t.c`, or the single word `none`.
                let parts = match d.arg.as_deref().and_then(|n| n.node.as_ref()) {
                    Some(N::List(l)) => l
                        .items
                        .iter()
                        .filter_map(|i| match i.node.as_ref() {
                            Some(N::String(s)) => Some(s.sval.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                out.owned_by = Some(
                    if parts.len() == 1 && parts[0].eq_ignore_ascii_case("none") {
                        String::new()
                    } else {
                        parts.join(".")
                    },
                );
            }
            // `AS bigint` fixes the bounds, and the sequence's own MIN/MAX
            // options override it; `CACHE` is a performance hint with no
            // effect on the values this server hands out.
            "as" | "cache" => {}
            other => return Err(Error::Unsupported(format!("the sequence option {other}"))),
        }
    }
    Ok(out)
}

fn plan_create_sequence(c: &pg_query::protobuf::CreateSeqStmt) -> Result<Statement> {
    let name = c
        .sequence
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| Error::Parse("CREATE SEQUENCE without a name".into()))?;
    Ok(Statement::CreateSequence {
        name,
        options: plan_sequence_options(&c.options)?,
        if_not_exists: c.if_not_exists,
        temp: c.sequence.as_ref().is_some_and(|r| r.relpersistence == "t"),
    })
}

fn plan_alter_sequence(a: &pg_query::protobuf::AlterSeqStmt) -> Result<Statement> {
    let name = a
        .sequence
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| Error::Parse("ALTER SEQUENCE without a name".into()))?;
    Ok(Statement::AlterSequence {
        name,
        options: plan_sequence_options(&a.options)?,
        missing_ok: a.missing_ok,
    })
}

/// The name a FROM item resolves to, keeping the schema only where it
/// DISAMBIGUATES.
///
/// `information_schema`'s views are called `tables`, `columns`, `sequences` --
/// names a user table may perfectly well have. The virtual-relation lookup
/// wins over the catalog, so registering them bare would make a user's own
/// `columns` table unreachable. Keeping the qualifier for that one schema
/// separates them; `pg_catalog.pg_type` still resolves to `pg_type`, because
/// nothing else is called that, and `public.t` to `t`.
pub fn relation_name(r: &pg_query::protobuf::RangeVar) -> String {
    if r.schemaname.eq_ignore_ascii_case("information_schema") {
        format!("information_schema.{}", r.relname)
    } else {
        r.relname.clone()
    }
}

/// `CREATE [UNIQUE] INDEX`.
///
/// What cannot be mapped onto a storage index is refused BY NAME rather than
/// approximated: an expression key, an operator class, a collation, a
/// non-default NULLS ordering and the non-btree access methods all change
/// either what the index enforces or what the catalog must report, and an
/// index silently built as something else is the wrong index.
fn plan_create_index(
    i: &pg_query::protobuf::IndexStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<Statement> {
    let relation = i
        .relation
        .as_ref()
        .ok_or_else(|| Error::Parse("CREATE INDEX without a relation".into()))?;
    let table = relation.relname.clone();
    let def = lookup(&table).ok_or_else(|| write_target_missing(&table, "an index on"))?;
    let method = if i.access_method.is_empty() {
        "btree".to_string()
    } else {
        i.access_method.to_ascii_lowercase()
    };
    match method.as_str() {
        "btree" => {}
        "hash" => {
            if i.unique {
                return Err(Error::FeatureNotSupported(
                    "access method \"hash\" does not support unique indexes".into(),
                ));
            }
            if i.index_params.len() > 1 {
                return Err(Error::FeatureNotSupported(
                    "access method \"hash\" does not support multicolumn indexes".into(),
                ));
            }
        }
        other => {
            return Err(Error::Unsupported(format!(
                "an index using access method \"{other}\""
            )))
        }
    }
    if i.nulls_not_distinct {
        return Err(Error::Unsupported("UNIQUE NULLS NOT DISTINCT".into()));
    }
    let mut columns = Vec::new();
    for p in &i.index_params {
        let Some(N::IndexElem(e)) = p.node.as_ref() else {
            return Err(Error::Unsupported("this index key".into()));
        };
        if e.expr.is_some() || e.name.is_empty() {
            return Err(Error::Unsupported("an index over an expression".into()));
        }
        if !e.opclass.is_empty() {
            return Err(Error::Unsupported("an index operator class".into()));
        }
        if !e.collation.is_empty() {
            return Err(Error::Unsupported("an index key COLLATE".into()));
        }
        if def.column(&e.name).is_none() {
            return Err(Error::UndefinedColumn(e.name.clone()));
        }
        let desc = SortByDir::try_from(e.ordering) == Ok(SortByDir::SortbyDesc);
        // The default is NULLS LAST ascending and NULLS FIRST descending; the
        // storage order is fixed, so only the default can be honoured.
        let nulls = SortByNulls::try_from(e.nulls_ordering);
        let non_default = match nulls {
            Ok(SortByNulls::SortbyNullsFirst) => !desc,
            Ok(SortByNulls::SortbyNullsLast) => desc,
            _ => false,
        };
        if non_default {
            return Err(Error::Unsupported(
                "a non-default NULLS ordering in an index".into(),
            ));
        }
        columns.push((e.name.clone(), desc));
    }
    let mut include = Vec::new();
    for p in &i.index_including_params {
        let Some(N::IndexElem(e)) = p.node.as_ref() else {
            return Err(Error::Unsupported("this INCLUDE column".into()));
        };
        if e.expr.is_some() || e.name.is_empty() {
            return Err(Error::FeatureNotSupported(
                "expressions are not supported in included columns".into(),
            ));
        }
        if def.column(&e.name).is_none() {
            return Err(Error::UndefinedColumn(e.name.clone()));
        }
        include.push(e.name.clone());
    }
    let (predicate, predicate_sql) = match i.where_clause.as_deref() {
        None => (None, None),
        Some(w) => {
            let filter = lower_where(w, &def, params)?;
            let sql = render_index_predicate(w, &def)
                .or_else(|| deparse_expr(w).ok().map(|s| format!("({s})")))
                .unwrap_or_default();
            (Some(filter), Some(sql))
        }
    };
    let name = (!i.idxname.is_empty()).then(|| i.idxname.clone());
    Ok(Statement::CreateIndex(CreateIndex {
        name,
        table,
        columns,
        include,
        unique: i.unique,
        if_not_exists: i.if_not_exists,
        predicate,
        predicate_sql,
        method,
    }))
}

/// An index predicate as PostgreSQL's ruleutils prints it in
/// `pg_indexes.indexdef` -- every comparison parenthesised, a string constant
/// carrying its column's cast. `None` for a shape this does not reproduce
/// exactly; the caller then falls back to the parenthesised deparse.
fn render_index_predicate(node: &pg_query::protobuf::Node, def: &TableDef) -> Option<String> {
    use pg_query::protobuf::{a_const::Val, BoolExprType, NullTestType};
    let column = |n: &pg_query::protobuf::Node| -> Option<String> {
        let N::ColumnRef(c) = n.node.as_ref()? else {
            return None;
        };
        let name = match c.fields.last()?.node.as_ref()? {
            N::String(s) => s.sval.clone(),
            _ => return None,
        };
        def.column(&name).map(|_| name)
    };
    match node.node.as_ref()? {
        N::BoolExpr(b) => {
            let parts: Option<Vec<String>> = b
                .args
                .iter()
                .map(|a| render_index_predicate(a, def))
                .collect();
            let parts = parts?;
            match BoolExprType::try_from(b.boolop).ok()? {
                BoolExprType::AndExpr => Some(format!("({})", parts.join(" AND "))),
                BoolExprType::OrExpr => Some(format!("({})", parts.join(" OR "))),
                BoolExprType::NotExpr => Some(format!("(NOT {})", parts.first()?)),
                _ => None,
            }
        }
        N::NullTest(t) => {
            let col = column(t.arg.as_deref()?)?;
            match NullTestType::try_from(t.nulltesttype).ok()? {
                NullTestType::IsNull => Some(format!("({col} IS NULL)")),
                NullTestType::IsNotNull => Some(format!("({col} IS NOT NULL)")),
                _ => None,
            }
        }
        N::ColumnRef(_) => column(node),
        N::AExpr(a) if a.kind == pg_query::protobuf::AExprKind::AexprOp as i32 => {
            let op = match a.name.first()?.node.as_ref()? {
                N::String(s) => s.sval.clone(),
                _ => return None,
            };
            if !matches!(op.as_str(), "=" | "<>" | "<" | "<=" | ">" | ">=") {
                return None;
            }
            let col = column(a.lexpr.as_deref()?)?;
            let N::AConst(c) = a.rexpr.as_deref()?.node.as_ref()? else {
                return None;
            };
            let ty = def.column(&col)?.pg_type.clone();
            let lit = match c.val.as_ref()? {
                Val::Ival(v) => v.ival.to_string(),
                Val::Fval(v) if ty == "numeric" => v.fval.clone(),
                Val::Boolval(v) => v.boolval.to_string(),
                Val::Sval(v) => {
                    let cast = match ty.as_str() {
                        "text" => "text",
                        "varchar" => "character varying",
                        _ => return None,
                    };
                    format!("'{}'::{cast}", v.sval.replace('\'', "''"))
                }
                _ => return None,
            };
            Some(format!("({col} {op} {lit})"))
        }
        _ => None,
    }
}

/// `EXPLAIN`. The statement is planned as it would run; the server renders
/// the plan's SHAPE -- this server has no cost model, so it never pretends to
/// PostgreSQL's numbers.
fn plan_explain(
    e: &pg_query::protobuf::ExplainStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<Statement> {
    let mut options = ExplainOptions {
        analyze: false,
        verbose: false,
        costs: true,
        format: "text".into(),
    };
    for o in &e.options {
        let Some(N::DefElem(d)) = o.node.as_ref() else {
            continue;
        };
        let text = d.arg.as_deref().and_then(|a| match a.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.to_ascii_lowercase()),
            Some(N::Boolean(b)) => Some(b.boolval.to_string()),
            Some(N::Integer(i)) => Some(i.ival.to_string()),
            _ => None,
        });
        let flag = || -> Result<bool> {
            match text.as_deref() {
                None | Some("true" | "on" | "1") => Ok(true),
                Some("false" | "off" | "0") => Ok(false),
                Some(_) => Err(Error::Sqlstate(
                    "22023",
                    format!("{} requires a Boolean value", d.defname),
                )),
            }
        };
        match d.defname.as_str() {
            "analyze" => options.analyze = flag()?,
            "verbose" => options.verbose = flag()?,
            "costs" => options.costs = flag()?,
            "buffers" | "timing" | "summary" | "settings" | "wal" => {
                flag()?;
            }
            "format" => match text.as_deref() {
                Some(f @ ("text" | "json")) => options.format = f.to_string(),
                Some(f @ ("yaml" | "xml")) => {
                    return Err(Error::Unsupported(format!(
                        "EXPLAIN (FORMAT {})",
                        f.to_ascii_uppercase()
                    )))
                }
                other => {
                    return Err(Error::Sqlstate(
                        "22023",
                        format!(
                            "unrecognized value for EXPLAIN option \"format\": \"{}\"",
                            other.unwrap_or_default()
                        ),
                    ))
                }
            },
            other => {
                return Err(Error::Sqlstate(
                    "42601",
                    format!("unrecognized EXPLAIN option \"{other}\""),
                ))
            }
        }
    }
    let query = e
        .query
        .as_deref()
        .and_then(|q| q.node.clone())
        .ok_or_else(|| Error::Parse("EXPLAIN without a statement".into()))?;
    let inner = plan_node(query, lookup, params)?;
    if !matches!(
        inner,
        Statement::Select(_)
            | Statement::Aggregate(_)
            | Statement::SetOp(_)
            | Statement::SelectConstant(_)
            | Statement::ValuesConstant(_)
            | Statement::Insert(_)
            | Statement::Update(_)
            | Statement::Delete(_)
    ) {
        return Err(Error::Unsupported("EXPLAIN of this statement".into()));
    }
    Ok(Statement::Explain {
        inner: Box::new(inner),
        options,
    })
}

/// `CREATE [OR REPLACE] VIEW`. The body is kept as SQL text, which is what the
/// Python server stores too, and expanded as a FROM-subquery wherever the
/// view is read (`expand_views`).
fn plan_create_view(v: &pg_query::protobuf::ViewStmt) -> Result<Statement> {
    let relation = v
        .view
        .as_ref()
        .ok_or_else(|| Error::Parse("CREATE VIEW without a name".into()))?;
    if relation.relpersistence == "t" {
        return Err(Error::Unsupported("CREATE TEMP VIEW".into()));
    }
    let query = v
        .query
        .as_deref()
        .ok_or_else(|| Error::Parse("CREATE VIEW without a query".into()))?;
    let Some(N::SelectStmt(select)) = query.node.as_ref() else {
        return Err(Error::Unsupported("a view over this statement".into()));
    };
    if select.with_clause.as_ref().is_some_and(|w| w.recursive) {
        return Err(Error::Unsupported("CREATE RECURSIVE VIEW".into()));
    }
    let body = query.deparse().map_err(|e| Error::Parse(e.to_string()))?;
    let columns: Vec<String> = v
        .aliases
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        })
        .collect();
    let name = relation.relname.clone();
    // A declared column list renames the outputs. Written into the select
    // list's own aliases where it can be -- that is the form the Python
    // server stores and reads -- and as a column-aliased subquery only where
    // it cannot (a `*` target or a set operation has no per-column alias).
    let renamed = (!columns.is_empty()
        && select.op == pg_query::protobuf::SetOperation::SetopNone as i32
        && select.target_list.len() >= columns.len()
        && select.target_list.iter().all(|t| {
            matches!(t.node.as_ref(), Some(N::ResTarget(r))
                if !matches!(r.val.as_deref().and_then(|v| v.node.as_ref()),
                    Some(N::ColumnRef(c)) if c.fields.iter().any(|f| matches!(f.node, Some(N::AStar(_))))))
        }))
    .then(|| {
        let mut sel = (**select).clone();
        for (target, name) in sel.target_list.iter_mut().zip(&columns) {
            if let Some(N::ResTarget(r)) = target.node.as_mut() {
                r.name = name.clone();
            }
        }
        pg_query::protobuf::Node {
            node: Some(N::SelectStmt(Box::new(sel))),
        }
        .deparse()
        .ok()
    })
    .flatten();
    let definition = if columns.is_empty() {
        body.clone()
    } else if let Some(renamed) = renamed {
        renamed
    } else {
        let quoted: Vec<String> = columns
            .iter()
            .map(|c| crate::scalar::quote_identifier(c))
            .collect();
        format!(
            "SELECT * FROM ({body}) AS {}({})",
            crate::scalar::quote_identifier(&name),
            quoted.join(", ")
        )
    };
    let check_option = match pg_query::protobuf::ViewCheckOption::try_from(v.with_check_option) {
        Ok(pg_query::protobuf::ViewCheckOption::LocalCheckOption) => Some("LOCAL".to_string()),
        Ok(pg_query::protobuf::ViewCheckOption::CascadedCheckOption) => {
            Some("CASCADED".to_string())
        }
        _ => None,
    };
    Ok(Statement::CreateView(CreateView {
        name,
        body,
        definition,
        columns,
        replace: v.replace,
        check_option,
    }))
}

thread_local! {
    /// The views this connection can read: `(name, stored definition)`,
    /// installed per statement by the wire layer beside the user types.
    static PLAN_VIEWS: std::cell::RefCell<Vec<(String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Install the views for the statements that follow on this thread.
pub fn set_views(views: Vec<(String, String)>) {
    PLAN_VIEWS.with(|v| *v.borrow_mut() = views);
}

fn view_definition(name: &str) -> Option<String> {
    PLAN_VIEWS.with(|v| {
        v.borrow()
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, d)| d.clone())
    })
}

/// Is `name` a view this connection can read?
pub fn is_view(name: &str) -> bool {
    view_definition(name).is_some()
}

/// Replace every FROM reference to a VIEW with the subquery its definition
/// is -- which is what PostgreSQL's rewriter does with a view, too. Only the
/// top level of this SELECT is rewritten: a FROM-subquery, a subquery in an
/// expression and each side of a set operation are all planned through
/// `plan_select` in their turn, which expands their own references.
fn expand_views(s: &pg_query::protobuf::SelectStmt) -> Result<pg_query::protobuf::SelectStmt> {
    if PLAN_VIEWS.with(|v| v.borrow().is_empty()) {
        return Ok(s.clone());
    }
    let mut out = s.clone();
    for item in &mut out.from_clause {
        expand_views_in_from(item, 0)?;
    }
    Ok(out)
}

fn expand_views_in_from(item: &mut pg_query::protobuf::Node, depth: usize) -> Result<()> {
    match item.node.as_mut() {
        Some(N::RangeVar(r)) => {
            if !(r.schemaname.is_empty() || r.schemaname == "public") || !r.catalogname.is_empty() {
                return Ok(());
            }
            let Some(definition) = view_definition(&r.relname) else {
                return Ok(());
            };
            // A view whose definition reaches itself (only possible through
            // OR REPLACE) is PostgreSQL's 42P17 at query time.
            if depth > 32 {
                return Err(Error::Unsupported(format!(
                    "infinite recursion detected in rules for relation \"{}\"",
                    r.relname
                )));
            }
            let N::SelectStmt(body) = parse_one(&definition)? else {
                return Err(Error::Internal(format!(
                    "the stored definition of view \"{}\" is not a SELECT",
                    r.relname
                )));
            };
            let mut body = *body;
            for inner in &mut body.from_clause {
                expand_views_in_from(inner, depth + 1)?;
            }
            let alias = r
                .alias
                .clone()
                .filter(|a| !a.aliasname.is_empty())
                .unwrap_or(pg_query::protobuf::Alias {
                    aliasname: r.relname.clone(),
                    colnames: Vec::new(),
                });
            item.node = Some(N::RangeSubselect(Box::new(
                pg_query::protobuf::RangeSubselect {
                    lateral: false,
                    subquery: Some(Box::new(pg_query::protobuf::Node {
                        node: Some(N::SelectStmt(Box::new(body))),
                    })),
                    alias: Some(alias),
                },
            )));
            Ok(())
        }
        Some(N::JoinExpr(j)) => {
            for side in [j.larg.as_mut(), j.rarg.as_mut()].into_iter().flatten() {
                expand_views_in_from(side, depth)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The error for a statement whose target relation is not a table: a VIEW is
/// refused by name (this server does not write through one), anything else
/// is the ordinary 42P01.
fn write_target_missing(table: &str, what: &str) -> Error {
    if is_view(table) {
        Error::Unsupported(format!("{what} a view"))
    } else {
        Error::UndefinedTable(table.to_string())
    }
}

/// Every view whose definition reads `table`, for DROP's dependency check.
pub fn views_reading(table: &str) -> Vec<String> {
    PLAN_VIEWS.with(|v| {
        v.borrow()
            .iter()
            .filter(|(_, d)| {
                pg_query::parse(d)
                    .map(|p| {
                        p.tables()
                            .iter()
                            .any(|t| t == table || t == &format!("public.{table}"))
                    })
                    .unwrap_or(false)
            })
            .map(|(n, _)| n.clone())
            .collect()
    })
}

fn plan_create(c: &pg_query::protobuf::CreateStmt) -> Result<Statement> {
    use pg_query::protobuf::ConstrType as CT;
    let relation = c
        .relation
        .as_ref()
        .ok_or_else(|| Error::Parse("CREATE TABLE without a relation".into()))?;
    let table = relation.relname.clone();
    // `CREATE TEMP TABLE`: `relpersistence` is `t` (RELPERSISTENCE_TEMP).
    let temp = relation.relpersistence == "t";

    let mut columns: Vec<Column> = Vec::new();
    // (constraint name if given, expression node, the columns it names) --
    // resolved to CheckConstraints once every column is known, because a
    // column-level CHECK may read a column declared after it.
    let mut checks: Vec<(String, pg_query::protobuf::Node)> = Vec::new();
    let mut fks: Vec<ForeignKey> = Vec::new();
    let mut uniques: Vec<UniqueConstraint> = Vec::new();
    // DEFERRABLE / INITIALLY DEFERRED arrive as separate attribute constraints
    // AFTER the constraint they qualify, so they have to be applied to whichever
    // one was pushed last. Before UNIQUE existed here that was always the FK,
    // and the attribute handlers below reached straight for `fks.last_mut()`;
    // with two kinds in play that would silently attach `UNIQUE ... DEFERRABLE`
    // to an unrelated foreign key earlier in the same table.
    let mut last_deferrable: Option<DeferTarget> = None;
    let mut table_pk: Vec<String> = Vec::new();
    for el in &c.table_elts {
        match el.node.as_ref() {
            Some(N::ColumnDef(cd)) => {
                let ty = cd.type_name.as_ref().map(type_name_of).unwrap_or_default();
                // `serial` family are pseudo-types: PostgreSQL resolves them to
                // the underlying integer type (plus an implicit sequence
                // default). We store the integer type so the column reads and
                // writes as `int4`/`int8`/`int2` on the wire and through COPY.
                let underlying = normalize_serial(&ty);
                let pk = cd.constraints.iter().any(|c| {
                    matches!(c.node.as_ref(), Some(N::Constraint(k))
                        if k.contype == CT::ConstrPrimary as i32)
                });
                let mut column = Column::new(&cd.colname, &underlying, pk);
                // The declared width / precision, so `char(4)` describes as
                // 4 rather than as an unsized `text` -- and so the Python
                // server, which shares this catalog, reads the same type.
                column.typmod = cd.type_name.as_ref().map(declared_typmod).unwrap_or(-1);
                // A serial column draws its default from a sequence named
                // `<table>_<column>_seq`, as PostgreSQL names it -- and is
                // NOT NULL, as PostgreSQL declares it.
                if underlying != ty {
                    column.sequence = Some(format!("{table}_{}_seq", cd.colname));
                    column.nullable = false;
                }
                for k in &cd.constraints {
                    let Some(N::Constraint(k)) = k.node.as_ref() else {
                        continue;
                    };
                    match CT::try_from(k.contype) {
                        Ok(CT::ConstrPrimary) => {}
                        Ok(CT::ConstrNotnull) => column.nullable = false,
                        Ok(CT::ConstrNull) => column.nullable = !pk,
                        // A literal DEFAULT is cast to the column's type now
                        // (a bad literal is an error at CREATE, as on
                        // PostgreSQL) and stored in the catalog. An
                        // expression default -- `now()`, arithmetic -- is
                        // refused rather than dropped: before this, every
                        // DEFAULT was silently ignored and the omitted column
                        // read NULL.
                        Ok(CT::ConstrDefault) => {
                            let Some(raw) = k.raw_expr.as_ref() else {
                                continue;
                            };
                            // A DEFAULT is evaluated ONCE here and stored as a
                            // value, so a volatile function -- `now()` -- would
                            // be frozen at CREATE time and every later row
                            // would carry the table's creation instant where
                            // PostgreSQL stamps each INSERT. Refuse it rather
                            // than store the wrong value silently.
                            // So a volatile one -- or one the constant
                            // evaluator cannot fold -- is kept as its SQL and
                            // evaluated per INSERTed row by the executor.
                            match default_value_or_expr(raw, &underlying, &[])? {
                                DefaultSpec::Value(v) => column.default = Some(v),
                                DefaultSpec::Expr(e) => column.set_default_expr(Some(e)),
                            }
                        }
                        Ok(CT::ConstrCheck) => {
                            let Some(raw) = k.raw_expr.as_ref() else {
                                continue;
                            };
                            checks.push((k.conname.clone(), (**raw).clone()));
                        }
                        Ok(CT::ConstrForeign) => {
                            fks.push(foreign_key_of(k, &table, vec![cd.colname.clone()])?);
                            last_deferrable = Some(DeferTarget::ForeignKey);
                        }
                        // A column constraint's DEFERRABLE / INITIALLY
                        // DEFERRED arrive as separate attribute constraints
                        // after the constraint they qualify.
                        Ok(CT::ConstrAttrDeferrable) => {
                            apply_defer(&mut fks, &mut uniques, last_deferrable, true, None);
                        }
                        Ok(CT::ConstrAttrNotDeferrable) => {
                            apply_defer(
                                &mut fks,
                                &mut uniques,
                                last_deferrable,
                                false,
                                Some(false),
                            );
                        }
                        Ok(CT::ConstrAttrDeferred) => {
                            apply_defer(&mut fks, &mut uniques, last_deferrable, true, Some(true));
                        }
                        Ok(CT::ConstrAttrImmediate) => {
                            apply_defer_initial(&mut fks, &mut uniques, last_deferrable, false);
                        }
                        Ok(CT::ConstrUnique) => {
                            // PostgreSQL names an unnamed one
                            // `<table>_<column>_key` (probed 14.13).
                            let name = if k.conname.is_empty() {
                                format!("{table}_{}_key", cd.colname)
                            } else {
                                k.conname.clone()
                            };
                            uniques.push(UniqueConstraint::new(&name, vec![cd.colname.clone()]));
                            last_deferrable = Some(DeferTarget::Unique);
                        }
                        // `GENERATED ALWAYS AS IDENTITY` / `GENERATED BY
                        // DEFAULT AS IDENTITY`: a sequence-backed column with
                        // stricter rules than a serial. `generated_when` is
                        // `"a"` for ALWAYS and `"d"` for BY DEFAULT; stored
                        // under the PYTHON server's spelling, because the two
                        // share this catalog.
                        Ok(CT::ConstrIdentity) => {
                            column.identity = Some(
                                if k.generated_when == "a" {
                                    "always"
                                } else {
                                    "by_default"
                                }
                                .to_string(),
                            );
                            column.sequence = Some(format!("{table}_{}_seq", cd.colname));
                            // An identity column is NOT NULL by definition.
                            column.nullable = false;
                        }
                        _ => {
                            return Err(Error::Unsupported(format!(
                                "constraint kind {} on column \"{}\"",
                                k.contype, cd.colname
                            )));
                        }
                    }
                }
                columns.push(column);
            }
            Some(N::Constraint(k)) => match CT::try_from(k.contype) {
                Ok(CT::ConstrCheck) => {
                    let Some(raw) = k.raw_expr.as_ref() else {
                        continue;
                    };
                    checks.push((k.conname.clone(), (**raw).clone()));
                }
                Ok(CT::ConstrForeign) => {
                    let cols = string_list(&k.fk_attrs);
                    fks.push(foreign_key_of(k, &table, cols)?);
                }
                Ok(CT::ConstrUnique) => {
                    let cols = string_list(&k.keys);
                    // `<table>_<col>_<col>_key` for a multi-column one
                    // (probed 14.13: `UNIQUE (a,b)` -> `u3_a_b_key`).
                    let name = if k.conname.is_empty() {
                        format!("{table}_{}_key", cols.join("_"))
                    } else {
                        k.conname.clone()
                    };
                    let mut uq = UniqueConstraint::new(&name, cols);
                    uq.deferrable = k.deferrable;
                    uq.initially_deferred = k.initdeferred;
                    uniques.push(uq);
                }
                Ok(CT::ConstrPrimary) => table_pk = string_list(&k.keys),
                _ => return Err(Error::Unsupported(disc(el.node.as_ref().unwrap()))),
            },
            Some(other) => return Err(Error::Unsupported(disc(other))),
            None => {}
        }
    }
    // ONE primary key per table: two column-level ones, or a column-level
    // one beside a table-level one, is PostgreSQL's 42P16. Only a single
    // constraint naming several columns is a composite key.
    let column_level = columns.iter().filter(|c| c.pk).count();
    if column_level + usize::from(!table_pk.is_empty()) > 1 {
        return Err(Error::Sqlstate(
            "42P16",
            format!("multiple primary keys for table \"{table}\" are not allowed"),
        ));
    }
    for name in &table_pk {
        let col = columns
            .iter_mut()
            .find(|c| &c.name == name)
            .ok_or_else(|| Error::UndefinedColumn(name.clone()))?;
        col.pk = true;
        col.nullable = false;
    }
    // A COMPOSITE primary key is a subdocument `_id` whose fields are the key
    // columns, in TABLE-column order -- the Python server's layout, so the
    // two share a store, and a fixed order so the same key is always the same
    // `_id` (a document's equality depends on its key order).
    if columns.iter().filter(|c| c.pk).count() > 1 {
        for c in columns.iter_mut().filter(|c| c.pk) {
            c.field_override = Some(format!("_id.{}", c.name));
        }
    }
    let mut def = TableDef::new(&table, columns);
    def.temp = temp;
    // A self-referencing FOREIGN KEY reads this table's own PK, which is only
    // settled now; a FK to another table is checked against the catalog by
    // the server, which has it.
    for fk in &mut fks {
        if fk.ref_table == table {
            resolve_fk_target(fk, &def)?;
        }
    }
    // Two columns of the same name: PostgreSQL's 42701, which this accepted
    // silently -- producing a table whose second column was unreachable,
    // since every lookup resolves a name to the FIRST match.
    for (i, col) in def.columns.iter().enumerate() {
        if def.columns[..i].iter().any(|c| c.name == col.name) {
            return Err(Error::DuplicateColumn(format!(
                "column \"{}\" specified more than once",
                col.name
            )));
        }
    }
    let mut check_names: Vec<String> = fks.iter().map(|f| f.name.clone()).collect();
    if def.columns.iter().any(|c| c.pk) {
        check_names.push(format!("{table}_pkey"));
    }
    for (conname, raw) in checks {
        // The predicate's SQL text is the catalog record (`(a > 0)`);
        // planning it now surfaces an unknown column at CREATE.
        let expression = check_expression_text(&raw)?;
        plan_check_expression(&expression, &def)?;
        let name = if conname.is_empty() {
            // `<table>_<col>_check` when the predicate names exactly one
            // column, `<table>_check` otherwise, with a counter to keep the
            // name unique within the table (`ChooseConstraintName`).
            let named = columns_referenced(&raw, &def);
            let base = match named.as_slice() {
                [one] => format!("{table}_{one}_check"),
                _ => format!("{table}_check"),
            };
            let mut candidate = base.clone();
            let mut n = 1;
            while check_names.contains(&candidate) {
                candidate = format!("{base}{n}");
                n += 1;
            }
            candidate
        } else {
            conname
        };
        check_names.push(name.clone());
        def.check_constraints
            .push(CheckConstraint { name, expression });
    }
    // PostgreSQL evaluates CHECK constraints in name order.
    def.check_constraints.sort_by(|a, b| a.name.cmp(&b.name));
    def.foreign_keys = fks;
    // A UNIQUE over the PRIMARY KEY column is already enforced by the `_id`
    // index, and backing it with a second storage index would report the
    // wrong constraint name on a duplicate. PostgreSQL keeps both constraints
    // but only one index; dropping ours is the same observable behaviour.
    let pk_col = def.columns.iter().find(|c| c.pk).map(|c| c.name.clone());
    uniques.retain(|u| match (&pk_col, u.columns.as_slice()) {
        (Some(pk), [only]) => only != pk,
        _ => true,
    });
    for uq in &uniques {
        for col in &uq.columns {
            if def.column(col).is_none() {
                return Err(Error::UndefinedColumn(col.clone()));
            }
        }
    }
    def.unique_constraints = uniques;
    Ok(Statement::CreateTable(def, c.if_not_exists))
}

/// Which constraint a trailing DEFERRABLE / INITIALLY DEFERRED qualifies.
///
/// PostgreSQL emits these as separate attribute constraints following the one
/// they modify, so the parser has to remember what it last pushed.
#[derive(Debug, Clone, Copy, PartialEq)]
enum DeferTarget {
    ForeignKey,
    Unique,
}

/// Apply a DEFERRABLE-family attribute to whichever constraint was pushed last.
/// `initial` of `None` leaves `initially_deferred` alone (plain `DEFERRABLE`).
fn apply_defer(
    fks: &mut [ForeignKey],
    uniques: &mut [UniqueConstraint],
    target: Option<DeferTarget>,
    deferrable: bool,
    initial: Option<bool>,
) {
    match target {
        Some(DeferTarget::ForeignKey) => {
            if let Some(fk) = fks.last_mut() {
                fk.deferrable = deferrable;
                if let Some(v) = initial {
                    fk.initially_deferred = v;
                }
            }
        }
        Some(DeferTarget::Unique) => {
            if let Some(uq) = uniques.last_mut() {
                uq.deferrable = deferrable;
                if let Some(v) = initial {
                    uq.initially_deferred = v;
                }
            }
        }
        None => {}
    }
}

/// `INITIALLY IMMEDIATE`: only the initial-deferred flag moves.
fn apply_defer_initial(
    fks: &mut [ForeignKey],
    uniques: &mut [UniqueConstraint],
    target: Option<DeferTarget>,
    initially_deferred: bool,
) {
    match target {
        Some(DeferTarget::ForeignKey) => {
            if let Some(fk) = fks.last_mut() {
                fk.initially_deferred = initially_deferred;
            }
        }
        Some(DeferTarget::Unique) => {
            if let Some(uq) = uniques.last_mut() {
                uq.initially_deferred = initially_deferred;
            }
        }
        None => {}
    }
}

/// The SQL text of a CHECK predicate, as `pg_get_constraintdef` renders it:
/// an operator expression is parenthesised (`(c > 0)`), a bare constant or
/// call is not (`true`). pg_query deparses only whole statements, so the
/// expression rides in a SELECT list and the keyword is stripped.
fn check_expression_text(raw: &pg_query::protobuf::Node) -> Result<String> {
    let select = N::SelectStmt(Box::new(pg_query::protobuf::SelectStmt {
        target_list: vec![pg_query::protobuf::Node {
            node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                name: String::new(),
                indirection: vec![],
                val: Some(Box::new(raw.clone())),
                location: 0,
            }))),
        }],
        limit_option: pg_query::protobuf::LimitOption::Default as i32,
        op: pg_query::protobuf::SetOperation::SetopNone as i32,
        ..Default::default()
    }));
    let text = select.deparse().map_err(|e| Error::Parse(e.to_string()))?;
    let text = text.strip_prefix("SELECT ").unwrap_or(&text).to_string();
    Ok(match raw.node.as_ref() {
        Some(N::AExpr(_) | N::BoolExpr(_) | N::NullTest(_) | N::BooleanTest(_)) => {
            format!("({text})")
        }
        _ => text,
    })
}

/// The `String` nodes of a name list, as strings.
fn string_list(nodes: &[pg_query::protobuf::Node]) -> Vec<String> {
    nodes
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// Which of `def`'s columns `node` references, in column order. Each column
/// is probed the way `references_columns` probes for any: rewriting against
/// every field but that one fails on exactly that column iff it is named.
fn columns_referenced(node: &pg_query::protobuf::Node, def: &TableDef) -> Vec<String> {
    let all: Vec<RowField> = def
        .columns
        .iter()
        .map(|c| (c.name.clone(), c.field(), c.pg_type.clone()))
        .collect();
    def.columns
        .iter()
        .filter(|c| {
            let others: Vec<RowField> = all.iter().filter(|f| f.0 != c.name).cloned().collect();
            let mut probe = node.clone();
            matches!(rewrite_column_refs(&mut probe, &others, 0),
                Err(Error::UndefinedColumn(n)) if n == c.name)
        })
        .map(|c| c.name.clone())
        .collect()
}

/// A FOREIGN KEY constraint from its parse node. `columns` are the
/// referencing columns (the column itself for a column constraint, the
/// `FOREIGN KEY (...)` list for a table constraint). The referenced columns
/// are left empty when the user named none -- they default to the referenced
/// table's PRIMARY KEY, which the server resolves against its catalog.
fn foreign_key_of(
    k: &pg_query::protobuf::Constraint,
    table: &str,
    columns: Vec<String>,
) -> Result<ForeignKey> {
    let ref_table = k
        .pktable
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| Error::Parse("REFERENCES without a table".into()))?;
    let ref_columns = string_list(&k.pk_attrs);
    if !ref_columns.is_empty() && ref_columns.len() != columns.len() {
        return Err(Error::InvalidForeignKey(
            "number of referencing and referenced columns for foreign key disagree".into(),
        ));
    }
    // PostgreSQL's one-letter action codes (`parsenodes.h`).
    let action = |code: &str| -> Result<Option<String>> {
        Ok(match code {
            "a" | "" => None,
            "r" => Some("RESTRICT".to_string()),
            "c" => Some("CASCADE".to_string()),
            "n" => Some("SET NULL".to_string()),
            "d" => Some("SET DEFAULT".to_string()),
            other => {
                return Err(Error::Parse(format!(
                    "unknown referential action {other:?}"
                )))
            }
        })
    };
    let on_delete = action(&k.fk_del_action)?;
    let on_update = action(&k.fk_upd_action)?;
    let name = if k.conname.is_empty() {
        format!("{table}_{}_fkey", columns.join("_"))
    } else {
        k.conname.clone()
    };
    Ok(ForeignKey {
        name,
        columns,
        ref_table,
        ref_columns,
        on_delete,
        on_update,
        deferrable: k.deferrable,
        initially_deferred: k.initdeferred,
    })
}

/// Settle a FOREIGN KEY's referenced columns against the referenced table's
/// definition: an empty list defaults to its PRIMARY KEY, and the named
/// column must BE that key (this server has no other unique constraint to
/// reference) -- PostgreSQL's 42830 otherwise.
pub fn resolve_fk_target(fk: &mut ForeignKey, target: &TableDef) -> Result<()> {
    let no_match = || {
        Error::InvalidForeignKey(format!(
            "there is no unique constraint matching given keys for referenced table \"{}\"",
            target.name
        ))
    };
    let pk: Vec<String> = target
        .columns
        .iter()
        .filter(|c| c.pk)
        .map(|c| c.name.clone())
        .collect();
    if fk.ref_columns.is_empty() {
        if pk.is_empty() {
            return Err(Error::InvalidForeignKey(format!(
                "there is no primary key for referenced table \"{}\"",
                target.name
            )));
        }
        if pk.len() != fk.columns.len() {
            return Err(Error::InvalidForeignKey(
                "number of referencing and referenced columns for foreign key disagree".into(),
            ));
        }
        fk.ref_columns = pk;
        return Ok(());
    }
    for col in &fk.ref_columns {
        if target.column(col).is_none() {
            return Err(Error::UndefinedColumn(col.clone()));
        }
    }
    // The referenced columns must be exactly a unique key -- the PRIMARY KEY
    // or a UNIQUE constraint -- as a SET: PostgreSQL pairs the columns
    // positionally but matches the key regardless of the order it is named.
    let same_set = |key: &[String]| {
        key.len() == fk.ref_columns.len() && fk.ref_columns.iter().all(|c| key.contains(c))
    };
    if same_set(&pk)
        || target
            .unique_constraints
            .iter()
            .any(|u| !u.deferrable && same_set(&u.columns))
    {
        Ok(())
    } else {
        Err(no_match())
    }
}

/// Plan a CHECK constraint's predicate text over `def`'s columns as a row
/// expression. `apply_row_expr` then evaluates it per row: `false` is a
/// violation; `true` and NULL pass (SQL's CHECK rule).
pub fn plan_check_expression(expression: &str, def: &TableDef) -> Result<ColumnExpr> {
    let N::SelectStmt(sel) = parse_one(&format!("SELECT {expression}"))? else {
        return Err(Error::Parse("CHECK expression is not an expression".into()));
    };
    let val = sel
        .target_list
        .first()
        .and_then(|t| match t.node.as_ref() {
            Some(N::ResTarget(rt)) => rt.val.as_deref(),
            _ => None,
        })
        .ok_or_else(|| Error::Parse("CHECK expression is not an expression".into()))?;
    let fields: Vec<RowField> = def
        .columns
        .iter()
        .map(|c| (c.name.clone(), c.field(), c.pg_type.clone()))
        .collect();
    let mut sample = Document::new();
    for c in &def.columns {
        sample.insert(c.field(), sample_value_for_type(&c.pg_type));
    }
    row_column_expr(val, &fields, &[], &sample)
}

/// The unique constraints a table has, as (name, column set), PK included.
///
/// The PK is not in `unique_constraints` — it is a `Column.pk` flag — so it is
/// synthesised here under PostgreSQL's default name, which is what
/// `ON CONFLICT ON CONSTRAINT t_pkey` refers to.
fn arbiters(def: &TableDef) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let pk: Vec<String> = def
        .columns
        .iter()
        .filter(|c| c.pk)
        .map(|c| c.name.clone())
        .collect();
    if !pk.is_empty() {
        out.push((format!("{}_pkey", def.name), pk));
    }
    for u in &def.unique_constraints {
        if !u.exclusion {
            out.push((u.name.clone(), u.columns.clone()));
        }
    }
    out
}

/// Rename `excluded.x` to a single name under `EXCLUDED_PREFIX`.
///
/// Runs BEFORE resolution, so the combined field list can carry the proposed
/// row's columns alongside the target's. See `EXCLUDED_PREFIX` for why the
/// rename is needed at all.
fn rename_excluded_refs(node: &mut pg_query::protobuf::Node) -> Result<()> {
    walk_column_refs(node, &mut |inner, c| {
        if c.fields.len() != 2 {
            return Ok(());
        }
        let qualifier = match c.fields[0].node.as_ref() {
            Some(N::String(st)) => st.sval.clone(),
            _ => return Ok(()),
        };
        if !qualifier.eq_ignore_ascii_case("excluded") {
            return Ok(());
        }
        let column = match c.fields[1].node.as_ref() {
            Some(N::String(st)) => st.sval.clone(),
            _ => return Ok(()),
        };
        let renamed = pg_query::protobuf::String {
            sval: format!("{EXCLUDED_PREFIX}{column}"),
        };
        *inner = N::ColumnRef(pg_query::protobuf::ColumnRef {
            fields: vec![pg_query::protobuf::Node {
                node: Some(N::String(renamed)),
            }],
            location: c.location,
        });
        Ok(())
    })
}

/// The field list a `DO UPDATE` expression resolves against: every column of
/// the target, then every column again under `EXCLUDED_PREFIX`.
fn on_conflict_fields(def: &TableDef) -> Vec<RowField> {
    let mut fields: Vec<RowField> = def
        .columns
        .iter()
        .map(|c| (c.name.clone(), c.field(), c.pg_type.clone()))
        .collect();
    for c in &def.columns {
        fields.push((
            format!("{EXCLUDED_PREFIX}{}", c.name),
            format!("{EXCLUDED_PREFIX}{}", c.field()),
            c.pg_type.clone(),
        ));
    }
    fields
}

fn plan_on_conflict(
    clause: &pg_query::protobuf::OnConflictClause,
    def: &TableDef,
    params: &[Bson],
) -> Result<OnConflict> {
    use pg_query::protobuf::OnConflictAction as A;

    let target = match clause.infer.as_deref() {
        None => None,
        Some(infer) => {
            // A partial-index arbiter (`ON CONFLICT (a) WHERE b`) needs index
            // inference this server does not have. REFUSED rather than
            // silently widened to the unconditional index, which would take a
            // conflict the user's predicate excludes.
            if infer.where_clause.is_some() {
                return Err(Error::Unsupported(
                    "ON CONFLICT with a WHERE on the conflict target".into(),
                ));
            }
            if !infer.conname.is_empty() {
                Some(ConflictTarget::Constraint(infer.conname.clone()))
            } else {
                let mut cols = Vec::new();
                for e in &infer.index_elems {
                    match e.node.as_ref() {
                        Some(N::IndexElem(ie)) if !ie.name.is_empty() => {
                            cols.push(ie.name.clone());
                        }
                        // An expression index (`ON CONFLICT (lower(a))`) is
                        // inference this server cannot do.
                        _ => return Err(Error::Unsupported("ON CONFLICT on an expression".into())),
                    }
                }
                Some(ConflictTarget::Columns(cols))
            }
        }
    };

    // PostgreSQL resolves the arbiter at PLAN time: a target matching no
    // unique constraint is 42P10 before any row is touched, not a dup-key
    // error when one happens to collide.
    if let Some(t) = &target {
        let known = arbiters(def);
        let matched = match t {
            ConflictTarget::Constraint(name) => known.iter().any(|(n, _)| n == name),
            ConflictTarget::Columns(cols) => known
                .iter()
                .any(|(_, c)| c.len() == cols.len() && cols.iter().all(|x| c.contains(x))),
        };
        if !matched {
            for col in match t {
                ConflictTarget::Columns(cols) => cols.clone(),
                ConflictTarget::Constraint(_) => Vec::new(),
            } {
                if def.column(&col).is_none() {
                    return Err(Error::UndefinedColumn(col));
                }
            }
            // The two halves are DIFFERENT errors in PostgreSQL 14, measured
            // rather than assumed: a named constraint that does not exist is
            // `42704 undefined_object`, while a column list matching no unique
            // index is `42P10 invalid_column_reference`.
            return Err(match t {
                ConflictTarget::Constraint(name) => Error::UndefinedObject(format!(
                    "constraint \"{name}\" for table \"{}\" does not exist",
                    def.name
                )),
                ConflictTarget::Columns(_) => Error::NoArbiter(
                    "there is no unique or exclusion constraint matching the \
                     ON CONFLICT specification"
                        .to_string(),
                ),
            });
        }
    }

    let action = match A::try_from(clause.action) {
        Ok(A::OnconflictNothing) => ConflictAction::Nothing,
        Ok(A::OnconflictUpdate) => {
            let fields = on_conflict_fields(def);
            let mut sample = Document::new();
            for (_, field, ty) in &fields {
                sample.insert(field.clone(), sample_value_for_type(ty));
            }
            let mut set_exprs: Vec<(String, String, ColumnExpr)> = Vec::new();
            for t in &clause.target_list {
                let Some(N::ResTarget(rt)) = t.node.as_ref() else {
                    return Err(Error::Unsupported("this ON CONFLICT SET target".into()));
                };
                let column = def
                    .column(&rt.name)
                    .ok_or_else(|| Error::UndefinedColumn(rt.name.clone()))?;
                // The PK is the document `_id`, which storage treats as
                // immutable — the same refusal `UPDATE` makes.
                if column.pk {
                    return Err(Error::Unsupported(
                        "ON CONFLICT DO UPDATE of a PRIMARY KEY column".into(),
                    ));
                }
                let field = column.field();
                let val = rt
                    .val
                    .as_ref()
                    .ok_or_else(|| Error::Parse("SET without a value".into()))?;
                let mut val = (**val).clone();
                rename_excluded_refs(&mut val)?;
                // Always planned as a row expression: even a constant may sit
                // beside an `excluded.` reference, and the row it reads is
                // assembled per conflict.
                let expr = row_column_expr(&val, &fields, params, &sample)?;
                set_exprs.push((field, column.pg_type.clone(), expr));
            }
            if set_exprs.is_empty() {
                return Err(Error::Parse(
                    "ON CONFLICT DO UPDATE without a SET list".into(),
                ));
            }
            let filter = match clause.where_clause.as_deref() {
                None => None,
                Some(w) => {
                    let mut w = w.clone();
                    rename_excluded_refs(&mut w)?;
                    Some(row_column_expr(&w, &fields, params, &sample)?)
                }
            };
            ConflictAction::Update { set_exprs, filter }
        }
        _ => return Err(Error::Unsupported("this ON CONFLICT action".into())),
    };

    Ok(OnConflict { target, action })
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
    let def = lookup(&table).ok_or_else(|| write_target_missing(&table, "INSERT into"))?;

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

    // `INSERT ... DEFAULT VALUES`: one row, every column left to its default.
    let default_values = pg_query::protobuf::SelectStmt {
        values_lists: vec![pg_query::protobuf::Node {
            node: Some(N::List(pg_query::protobuf::List { items: Vec::new() })),
        }],
        ..Default::default()
    };
    let sel = match i.select_stmt.as_ref().and_then(|s| s.node.as_ref()) {
        Some(N::SelectStmt(s)) => s,
        None if i.select_stmt.is_none() => &default_values,
        _ => return Err(Error::Unsupported("this INSERT source".into())),
    };
    let mut rows = Vec::new();
    // `INSERT ... SELECT`: the query is planned whole and evaluated by the
    // executor, which has the storage. Before this it was silently treated
    // as an empty VALUES list -- `INSERT 0 0`, nothing written, no error.
    let source = if sel.values_lists.is_empty() {
        Some(Box::new(plan_select(sel, lookup, params)?))
    } else {
        None
    };
    for vl in &sel.values_lists {
        let items = match vl.node.as_ref() {
            Some(N::List(l)) => &l.items,
            _ => return Err(Error::Unsupported("this VALUES form".into())),
        };
        for (item, target) in items.iter().zip(&targets) {
            if let Some(column) = def.column(target) {
                check_assignment_type(column, item)?;
            }
        }
        // A `DEFAULT` item leaves its column out of the row, so the column's
        // default fills it exactly as it would an omitted one.
        let defaulted: Vec<&String> = items
            .iter()
            .zip(&targets)
            .filter(|(item, _)| matches!(item.node.as_ref(), Some(N::SetToDefault(_))))
            .map(|(_, t)| t)
            .collect();
        let values = items
            .iter()
            .map(|item| match item.node.as_ref() {
                Some(N::SetToDefault(_)) => Ok(Bson::Null),
                _ => const_value(item, params),
            })
            .collect::<Result<Vec<_>>>()?;
        let explicit = !i.cols.is_empty() && !items.is_empty();
        let mut row = insert_row(&def, &targets, explicit, values)?;
        for target in defaulted {
            if let Some(column) = def.column(target) {
                let field = column.field();
                row.remove(companion_field(&field));
                row.remove(&field);
            }
        }
        rows.push(row);
    }
    let returning = if i.returning_list.is_empty() {
        None
    } else {
        let (columns, casts) = plan_table_targets(&i.returning_list, &def, params)?;
        Some(Returning { columns, casts })
    };
    let on_conflict = match i.on_conflict_clause.as_deref() {
        None => None,
        Some(c) => Some(plan_on_conflict(c, &def, params)?),
    };
    Ok(Statement::Insert(Insert {
        table,
        rows,
        returning,
        source,
        targets,
        explicit_columns: !i.cols.is_empty(),
        on_conflict,
        overriding_system: OverridingKind::try_from(i.r#override)
            == Ok(OverridingKind::OverridingSystemValue),
        overriding_user: OverridingKind::try_from(i.r#override)
            == Ok(OverridingKind::OverridingUserValue),
    }))
}

/// The type an assigned expression DECLARES, when it declares one: an
/// explicit cast, or a parameter the client typed. Anything else -- a bare
/// literal (`unknown`), a function call, an operator -- is left to the
/// coercion `cast_value` already performs, so this never invents a type
/// from a value.
fn declared_expression_type(node: &pg_query::protobuf::Node) -> Option<String> {
    match node.node.as_ref()? {
        N::TypeCast(tc) => tc.type_name.as_ref().map(type_name_of),
        N::ParamRef(p) => declared_param_type(usize::try_from(p.number).ok()?),
        // A bare `true` / `false` is a `boolean` constant, not an unknown
        // literal: `set n = true` on an integer column is 42804.
        N::AConst(c) if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Boolval(_))) => {
            Some("bool".to_string())
        }
        _ => None,
    }
}

fn is_string_type(t: &str) -> bool {
    matches!(
        t,
        "text" | "varchar" | "character varying" | "bpchar" | "char" | "character" | "name"
    )
}

fn is_integer_type(t: &str) -> bool {
    matches!(
        t,
        "int2" | "smallint" | "int4" | "int" | "integer" | "int8" | "bigint"
    )
}

fn is_boolean_type(t: &str) -> bool {
    matches!(t, "bool" | "boolean")
}

/// PostgreSQL's assignment-cast rule for the part of it a typed value can
/// hit: an assignment needs an ASSIGNMENT cast, and a string type has one
/// only to another string type (every other cast out of `text` is explicit),
/// while any type has an I/O cast INTO a string type. `boolean` and the
/// integers have only explicit casts between them.
///
/// Measured on 16 -- `insert into t(data) values ($1)` with a text-declared
/// `$1` is `42804 column "data" is of type jsonb but expression is of type
/// text` for jsonb / json / integer / numeric / real / date / timestamp /
/// interval / boolean / uuid / bytea / text[] targets; the same statement
/// into `text`, `varchar` or `char(3)` stores the value, as does an
/// integer-declared `$1` into `text` or `bigint`, and json into jsonb.
/// Before this the server coerced the text through the column's parser, so
/// the psycopg binary-format string that PostgreSQL rejects was stored.
fn check_assignment_type(column: &Column, node: &pg_query::protobuf::Node) -> Result<()> {
    let Some(from) = declared_expression_type(node) else {
        return Ok(());
    };
    let to = column.pg_type.as_str();
    let (from_s, to_s) = (display_type(&from), display_type(to));
    let allowed = from_s == to_s
        || from == "unknown"
        || is_string_type(to)
        || !(is_string_type(&from)
            || (is_boolean_type(&from) && is_integer_type(to))
            || (is_integer_type(&from) && is_boolean_type(to)));
    if allowed {
        return Ok(());
    }
    Err(Error::DatatypeMismatch(format!(
        "column \"{}\" is of type {to_s} but expression is of type {from_s}",
        column.name
    )))
}

/// Shape one row of values into the document an INSERT stores, `values`
/// mapping positionally onto `targets` (columns of `def`).
///
/// A width mismatch is PostgreSQL's 42601 (probed PG 16: `insert into t
/// select 1, 2, 3` over a two-column table is "INSERT has more expressions
/// than target columns"; a short row is the mirror message only when the
/// columns were named -- an unnamed list leaves the trailing columns to
/// their defaults, so `insert into t select 1` writes the rest NULL).
pub fn insert_row(
    def: &TableDef,
    targets: &[String],
    explicit_columns: bool,
    values: Vec<Bson>,
) -> Result<Document> {
    if values.len() > targets.len() {
        return Err(Error::Parse(
            "INSERT has more expressions than target columns".into(),
        ));
    }
    if values.len() < targets.len() && explicit_columns {
        return Err(Error::Parse(
            "INSERT has more target columns than expressions".into(),
        ));
    }
    let mut d = Document::new();
    for (col, value) in targets.iter().zip(values) {
        let column = def
            .column(col)
            .ok_or_else(|| Error::UndefinedColumn(col.clone()))?;
        // PostgreSQL coerces an assigned value to the column's type, so
        // `INSERT INTO t(d) VALUES ('2026-9-1')` STORES `2026-09-01`.
        // Without this the literal went in verbatim and a client reading
        // the column back could not parse it as a date.
        let value = cast_value(value, &column.pg_type)?;
        // Resolves the hidden companion (setting or CLEARING it), so a
        // whole-millisecond write cannot inherit stale microseconds.
        let field = column.field();
        let stored = carry_subms(&mut d, &field, value);
        d.insert(field, stored);
    }
    Ok(canonical_key_order(def, d))
}

/// A composite primary key's `_id.<name>` fields in TABLE-column order, so the
/// `_id` subdocument they become on write is the same document however the
/// INSERT listed its columns.
pub fn canonical_key_order(def: &TableDef, mut d: Document) -> Document {
    let key_fields: Vec<String> = def
        .columns
        .iter()
        .filter(|c| c.pk && c.field_override.is_some())
        .map(|c| c.field())
        .collect();
    for f in key_fields {
        if let Some(v) = d.remove(&f) {
            d.insert(f, v);
        }
    }
    d
}

/// Does this target list contain an aggregate call?
/// The type `sum()` returns for an input type, as PostgreSQL 14.24 answers it
/// (measured 2026-09-20, not inferred -- the widening is not uniform):
///
/// | input | `sum` |
/// | --- | --- |
/// | `int2`, `int4` | `int8` |
/// | `int8` | `numeric` |
/// | `float4` | `float4` |
/// | `float8` | `float8` |
/// | `numeric` | `numeric` |
/// | `money` | `money` |
/// | `interval` | `interval` |
///
/// Everything used to answer `int8` unless it was a numeric, so `sum(f)` over
/// a `float8` column was DESCRIBED as an integer while the value `1.5` went
/// out on the wire -- and the client raised `invalid literal for int()`
/// rather than getting a number. A wrong type here is not cosmetic.
/// The type `avg()` returns for an input type (PostgreSQL 14.24, measured
/// 2026-09-22): a float averages as `float8`, an interval as `interval`, and
/// everything else -- the integers and `numeric` -- as `numeric`.
pub fn avg_result_type(source: Option<&str>) -> &'static str {
    match source {
        Some("float4" | "real" | "float8" | "double precision") => "float8",
        Some("interval") => "interval",
        _ => "numeric",
    }
}

pub fn sum_result_type(source: Option<&str>) -> &'static str {
    match source {
        Some("float4" | "real") => "float4",
        Some("float8" | "double precision") => "float8",
        Some("numeric" | "decimal" | "int8" | "bigint") => "numeric",
        Some("money") => "money",
        Some("interval") => "interval",
        _ => "int8",
    }
}

/// Whether an aggregate call appears BELOW the top level of a target -- the
/// `sum(n)` in `sum(n) + 0`, `count(*) + 1`, or `coalesce(sum(n), -1)`.
///
/// `has_aggregate` deliberately matches only a top-level call, so these were
/// planned as a plain SELECT with a computed column and reached the per-row
/// scalar evaluator, which has no `sum`. That answered
/// `0A000 function sum() is not supported yet` -- and over an EMPTY table it
/// answered NO ROWS AT ALL, because nothing was evaluated and so nothing
/// refused, where PostgreSQL returns one row. Refusing here makes the answer
/// the same either way, and names what is actually missing.
fn contains_nested_aggregate(node: &pg_query::protobuf::Node) -> bool {
    fn walk(node: Option<&pg_query::protobuf::Node>, depth: usize) -> bool {
        let Some(node) = node else { return false };
        match node.node.as_ref() {
            Some(N::FuncCall(f)) => {
                // `sum(x) OVER (...)` is a WINDOW call, not an aggregate --
                // its arguments are still searched, since `sum(sum(v)) over
                // ()` holds a real one.
                if depth > 0
                    && f.over.is_none()
                    && func_name(f)
                        .as_deref()
                        .is_some_and(|n| aggregate_func(n, f.agg_within_group).is_some())
                {
                    return true;
                }
                f.args.iter().any(|a| walk(Some(a), depth + 1))
            }
            Some(N::AExpr(e)) => {
                walk(e.lexpr.as_deref(), depth + 1) || walk(e.rexpr.as_deref(), depth + 1)
            }
            Some(N::TypeCast(tc)) => walk(tc.arg.as_deref(), depth + 1),
            Some(N::BoolExpr(b)) => b.args.iter().any(|a| walk(Some(a), depth + 1)),
            Some(N::CoalesceExpr(c)) => c.args.iter().any(|a| walk(Some(a), depth + 1)),
            Some(N::CaseExpr(c)) => {
                c.args.iter().any(|a| walk(Some(a), depth + 1))
                    || walk(c.defresult.as_deref(), depth + 1)
            }
            _ => false,
        }
    }
    walk(Some(node), 0)
}

/// PostgreSQL's `frame_options` bitmask, measured rather than copied: each
/// constant below was confirmed by parsing the clause it names and printing
/// the mask (`over ()` is `0x422`, `rows between 2 preceding and 1 preceding`
/// is `0x1815`, and so on).
mod frameopt {
    pub const NONDEFAULT: i32 = 0x00001;
    pub const RANGE: i32 = 0x00002;
    pub const ROWS: i32 = 0x00004;
    pub const GROUPS: i32 = 0x00008;
    pub const START_UNBOUNDED_PRECEDING: i32 = 0x00020;
    pub const END_UNBOUNDED_FOLLOWING: i32 = 0x00100;
    pub const START_CURRENT_ROW: i32 = 0x00200;
    pub const END_CURRENT_ROW: i32 = 0x00400;
    pub const START_OFFSET_PRECEDING: i32 = 0x00800;
    pub const END_OFFSET_PRECEDING: i32 = 0x01000;
    pub const START_OFFSET_FOLLOWING: i32 = 0x02000;
    pub const END_OFFSET_FOLLOWING: i32 = 0x04000;
    pub const EXCLUDE_CURRENT_ROW: i32 = 0x08000;
    pub const EXCLUDE_GROUP: i32 = 0x10000;
    pub const EXCLUDE_TIES: i32 = 0x20000;
}

/// True when any select-list target is a window call.
///
/// A window call is a `FuncCall` carrying `OVER`, which is what separates
/// `sum(v) OVER (...)` from the aggregate `sum(v)`. `has_aggregate` has to
/// exclude them for the same reason: routed into the aggregate planner, a
/// window `sum` demanded a GROUP BY and the client got
/// `42803 column "id" must appear in the GROUP BY clause` -- an error blaming
/// the user's own query for a feature this server did not have.
fn has_window(s: &pg_query::protobuf::SelectStmt) -> bool {
    s.target_list.iter().any(|t| {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return false;
        };
        node_has_window(rt.val.as_deref())
    })
}

fn node_has_window(node: Option<&pg_query::protobuf::Node>) -> bool {
    let Some(node) = node.and_then(|n| n.node.as_ref()) else {
        return false;
    };
    if let N::FuncCall(f) = node {
        if f.over.is_some() {
            return true;
        }
    }
    // Anywhere else in the expression -- a COALESCE, a CASE, a cast --
    // through the one walker every expression rewrite shares.
    let mut probe = pg_query::protobuf::Node {
        node: Some(node.clone()),
    };
    let mut found = false;
    let _ = walk_expr(&mut probe, &mut |n| {
        if let Some(N::FuncCall(f)) = n.node.as_ref() {
            found |= f.over.is_some();
        }
        Ok(())
    });
    found
}

/// The window functions by name, with the result type PostgreSQL gives them.
///
/// `sum` and friends are absent: their result type depends on the argument, so
/// they are resolved by `window_result_type` against the column instead.
fn window_func_by_name(name: &str) -> Option<WindowFunc> {
    Some(match name {
        "row_number" => WindowFunc::RowNumber,
        "rank" => WindowFunc::Rank,
        "dense_rank" => WindowFunc::DenseRank,
        "percent_rank" => WindowFunc::PercentRank,
        "cume_dist" => WindowFunc::CumeDist,
        "ntile" => WindowFunc::Ntile,
        "lag" => WindowFunc::Lag,
        "lead" => WindowFunc::Lead,
        "first_value" => WindowFunc::FirstValue,
        "last_value" => WindowFunc::LastValue,
        "nth_value" => WindowFunc::NthValue,
        "sum" => WindowFunc::Sum,
        "count" => WindowFunc::Count,
        "avg" => WindowFunc::Avg,
        "min" => WindowFunc::Min,
        "max" => WindowFunc::Max,
        "string_agg" => WindowFunc::StringAgg,
        "array_agg" => WindowFunc::ArrayAgg,
        "bool_and" => WindowFunc::BoolAnd,
        "bool_or" => WindowFunc::BoolOr,
        _ => return None,
    })
}

/// The type a window function reports, given its argument's type.
fn window_result_type(func: WindowFunc, source: Option<&str>) -> String {
    match func {
        // `row_number`, `rank`, `dense_rank` and `count` are int8 in
        // PostgreSQL, not int4 -- a client that decoded them as int4 would
        // read the wrong width off the wire.
        WindowFunc::RowNumber
        | WindowFunc::Rank
        | WindowFunc::DenseRank
        | WindowFunc::Count
        | WindowFunc::CountStar => "int8".to_string(),
        WindowFunc::Ntile => "int4".to_string(),
        WindowFunc::PercentRank | WindowFunc::CumeDist => "float8".to_string(),
        WindowFunc::Sum => sum_result_type(source).to_string(),
        WindowFunc::Avg => avg_result_type(source).to_string(),
        WindowFunc::StringAgg => "text".to_string(),
        WindowFunc::BoolAnd | WindowFunc::BoolOr => "bool".to_string(),
        WindowFunc::ArrayAgg => format!("{}[]", source.unwrap_or("text")),
        // The value-returning ones keep their argument's type.
        WindowFunc::Lag
        | WindowFunc::Lead
        | WindowFunc::FirstValue
        | WindowFunc::LastValue
        | WindowFunc::NthValue
        | WindowFunc::Min
        | WindowFunc::Max => source.unwrap_or("text").to_string(),
    }
}

/// One `OVER (...)` clause, resolved against any named windows in scope.
fn plan_window_def(
    over: &pg_query::protobuf::WindowDef,
    named: &[pg_query::protobuf::WindowDef],
    def: &TableDef,
    params: &[Bson],
    keys: &mut usize,
) -> Result<(Vec<OrderKey>, Vec<OrderKey>, WindowFrame)> {
    // `OVER w` and `OVER (w ORDER BY ...)` both arrive with `refname` set; the
    // named window supplies what the reference does not override. PostgreSQL
    // forbids a reference overriding PARTITION BY or an existing ORDER BY, and
    // this follows it by taking the named clause whenever it is non-empty.
    // The two spellings put the referenced name in DIFFERENT fields, measured
    // rather than assumed: a bare `OVER w` sets `name`, while `OVER (w ORDER
    // BY ...)` sets `refname`. Reading only `refname` left `OVER w` with no
    // ORDER BY at all, so every row became a peer and `sum(v) OVER w` answered
    // the whole-partition total where PostgreSQL gives a running one -- a
    // wrong answer, not an error.
    let reference = if over.refname.is_empty() {
        over.name.as_str()
    } else {
        over.refname.as_str()
    };
    let base = if reference.is_empty() {
        None
    } else {
        Some(named.iter().find(|w| w.name == reference).ok_or_else(|| {
            Error::UndefinedObject(format!("window \"{reference}\" does not exist"))
        })?)
    };
    let partition_src = match base {
        Some(b) if !b.partition_clause.is_empty() => &b.partition_clause,
        _ => &over.partition_clause,
    };
    let order_src = match base {
        Some(b) if !b.order_clause.is_empty() => &b.order_clause,
        _ => &over.order_clause,
    };
    let (frame_src, frame_offsets) = match base {
        Some(b) if over.frame_options & frameopt::NONDEFAULT == 0 => (b, b),
        _ => (over, over),
    };

    let mut partition_by = Vec::new();
    for item in partition_src {
        partition_by.push(window_sort_key(item, def, params, false, keys)?);
    }
    let mut order_by = Vec::new();
    for item in order_src {
        order_by.push(window_sort_key(item, def, params, true, keys)?);
    }
    let frame = plan_window_frame(frame_src, frame_offsets, params)?;
    Ok((partition_by, order_by, frame))
}

/// One PARTITION BY or ORDER BY key of a window.
///
/// A PARTITION BY item is a bare expression rather than a `SortBy`, so the two
/// are read differently and both land in an `OrderKey` -- partitioning only
/// ever uses its `field`, ordering uses the direction and null placement too.
fn window_sort_key(
    item: &pg_query::protobuf::Node,
    def: &TableDef,
    params: &[Bson],
    sorted: bool,
    keys: &mut usize,
) -> Result<OrderKey> {
    let (node, ascending, nulls) = if sorted {
        let Some(N::SortBy(sb)) = item.node.as_ref() else {
            return Err(Error::Unsupported("this window ORDER BY item".into()));
        };
        let ascending = match SortByDir::try_from(sb.sortby_dir) {
            Ok(SortByDir::SortbyDesc) => false,
            Ok(SortByDir::SortbyDefault | SortByDir::SortbyAsc) => true,
            _ => return Err(Error::Unsupported("window ORDER BY ... USING".into())),
        };
        let nulls = match SortByNulls::try_from(sb.sortby_nulls) {
            Ok(SortByNulls::SortbyNullsFirst) => Nulls::First,
            Ok(SortByNulls::SortbyNullsLast) => Nulls::Last,
            _ if ascending => Nulls::Last,
            _ => Nulls::First,
        };
        (sb.node.as_deref(), ascending, nulls)
    } else {
        (Some(item), true, Nulls::Last)
    };
    let node = node.ok_or_else(|| Error::Unsupported("an empty window key".into()))?;
    // A bare column reads its stored field; anything else is materialised per
    // row into its own synthetic slot, the same way a computed ORDER BY key is.
    if let Some(N::ColumnRef(c)) = node.node.as_ref() {
        if let Some(name) = column_ref_name(c) {
            if let Some(column) = def.column(&name) {
                return Ok(OrderKey {
                    field: column.field(),
                    ascending,
                    nulls,
                    expr: None,
                });
            }
            return Err(Error::UndefinedColumn(name));
        }
    }
    let fields: Vec<RowField> = def
        .columns
        .iter()
        .map(|c| (c.name.clone(), c.field(), c.pg_type.clone()))
        .collect();
    let mut sample = Document::new();
    for c in &def.columns {
        sample.insert(c.field(), sample_value_for_type(&c.pg_type));
    }
    let expr = row_column_expr(node, &fields, params, &sample)?;
    // Named by POSITION across the whole statement, so two computed keys in
    // two different windows cannot collide, and prefixed so no real column
    // can. An empty name here would make every computed key the same field.
    *keys += 1;
    Ok(OrderKey {
        field: format!("__wkey{}", *keys - 1),
        ascending,
        nulls,
        expr: Some(expr),
    })
}

fn plan_window_frame(
    w: &pg_query::protobuf::WindowDef,
    offsets: &pg_query::protobuf::WindowDef,
    params: &[Bson],
) -> Result<WindowFrame> {
    let opts = w.frame_options;
    if opts & frameopt::NONDEFAULT == 0 {
        return Ok(WindowFrame::DEFAULT);
    }
    // EXCLUDE removes rows from the frame once its bounds are known, so it
    // rides the peer groups the bounds already need.
    let exclude = if opts & frameopt::EXCLUDE_CURRENT_ROW != 0 {
        FrameExclude::CurrentRow
    } else if opts & frameopt::EXCLUDE_GROUP != 0 {
        FrameExclude::Group
    } else if opts & frameopt::EXCLUDE_TIES != 0 {
        FrameExclude::Ties
    } else {
        FrameExclude::NoOthers
    };
    let mode = if opts & frameopt::ROWS != 0 {
        FrameMode::Rows
    } else if opts & frameopt::GROUPS != 0 {
        FrameMode::Groups
    } else if opts & frameopt::RANGE != 0 {
        FrameMode::Range
    } else {
        return Err(Error::Unsupported("this window frame".into()));
    };
    let offset = |node: Option<&pg_query::protobuf::Node>| -> Result<i64> {
        let node = node.ok_or_else(|| Error::Parse("frame bound with no offset".into()))?;
        match const_value(node, params)? {
            Bson::Int32(v) => Ok(i64::from(v)),
            Bson::Int64(v) => Ok(v),
            // PostgreSQL's own message for a negative or non-integer bound.
            _ => Err(Error::Unsupported("this window frame offset".into())),
        }
    };
    let start = if opts & frameopt::START_UNBOUNDED_PRECEDING != 0 {
        FrameBound::UnboundedPreceding
    } else if opts & frameopt::START_CURRENT_ROW != 0 {
        FrameBound::CurrentRow
    } else if opts & frameopt::START_OFFSET_PRECEDING != 0 {
        FrameBound::Preceding(offset(offsets.start_offset.as_deref())?)
    } else if opts & frameopt::START_OFFSET_FOLLOWING != 0 {
        FrameBound::Following(offset(offsets.start_offset.as_deref())?)
    } else {
        return Err(Error::Unsupported("this window frame start".into()));
    };
    let end = if opts & frameopt::END_UNBOUNDED_FOLLOWING != 0 {
        FrameBound::UnboundedFollowing
    } else if opts & frameopt::END_CURRENT_ROW != 0 {
        FrameBound::CurrentRow
    } else if opts & frameopt::END_OFFSET_PRECEDING != 0 {
        FrameBound::Preceding(offset(offsets.end_offset.as_deref())?)
    } else if opts & frameopt::END_OFFSET_FOLLOWING != 0 {
        FrameBound::Following(offset(offsets.end_offset.as_deref())?)
    } else {
        return Err(Error::Unsupported("this window frame end".into()));
    };
    Ok(WindowFrame {
        mode,
        start,
        end,
        exclude,
    })
}

/// A windowed select list: output columns, their per-column expressions, the
/// window items themselves, and the synthetic columns their values land in.
type WindowTargets = (
    Vec<(String, String)>,
    Vec<Option<ColumnExpr>>,
    Vec<WindowItem>,
    Vec<Column>,
);

/// Plan the select list of a query carrying window functions.
///
/// Each window call becomes a `WindowItem` computing into `__winN`, and the
/// output column reads that field. Non-window targets are planned exactly as
/// a plain select's are, so `select id, row_number() over ()` keeps `id`'s
/// own type and table provenance.
fn plan_window_targets(
    s: &pg_query::protobuf::SelectStmt,
    def: &TableDef,
    params: &[Bson],
) -> Result<WindowTargets> {
    let named: Vec<pg_query::protobuf::WindowDef> = s
        .window_clause
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::WindowDef(w)) => Some((**w).clone()),
            _ => None,
        })
        .collect();
    let mut columns = Vec::new();
    let mut casts = Vec::new();
    let mut windows: Vec<WindowItem> = Vec::new();
    let mut extra: Vec<Column> = Vec::new();
    let mut keys = 0usize;
    for t in &s.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return Err(Error::Unsupported("this target".into()));
        };
        let Some(N::FuncCall(f)) = rt.val.as_ref().and_then(|v| v.node.as_ref()) else {
            // A plain target beside the windows, planned the ordinary way --
            // unless a window is nested inside it.
            if node_has_window(rt.val.as_deref()) {
                let (c, k) = plan_nested_windows(
                    rt,
                    &named,
                    def,
                    params,
                    &mut keys,
                    &mut windows,
                    &mut extra,
                )?;
                columns.push(c);
                casts.push(k);
                continue;
            }
            let (mut c, mut k) = plan_table_targets(std::slice::from_ref(t), def, params)?;
            columns.append(&mut c);
            casts.append(&mut k);
            continue;
        };
        if f.over.is_none() {
            // Not a window itself -- but it may HOLD one (`v / sum(v) over
            // ()`), which is hoisted below like any nested window.
            if !node_has_window(rt.val.as_deref()) {
                let (mut c, mut k) = plan_table_targets(std::slice::from_ref(t), def, params)?;
                columns.append(&mut c);
                casts.append(&mut k);
                continue;
            }
            let (c, k) =
                plan_nested_windows(rt, &named, def, params, &mut keys, &mut windows, &mut extra)?;
            columns.push(c);
            casts.push(k);
            continue;
        }
        let out = if rt.name.is_empty() {
            func_name(f).unwrap_or_default()
        } else {
            rt.name.clone()
        };
        let field = plan_window_call(f, &named, def, params, &mut keys, &mut windows, &mut extra)?;
        columns.push((out, field));
        casts.push(None);
    }
    Ok((columns, casts, windows, extra))
}

/// One window call, planned into `windows` and `extra`; answers the synthetic
/// field its value lands in.
#[allow(clippy::too_many_arguments)]
fn plan_window_call(
    f: &pg_query::protobuf::FuncCall,
    named: &[pg_query::protobuf::WindowDef],
    def: &TableDef,
    params: &[Bson],
    keys: &mut usize,
    windows: &mut Vec<WindowItem>,
    extra: &mut Vec<Column>,
) -> Result<String> {
    let over = f
        .over
        .as_deref()
        .ok_or_else(|| Error::Internal("a window call without OVER".into()))?;
    let name = func_name(f).unwrap_or_default();
    let func = window_func_by_name(&name)
        .ok_or_else(|| Error::Unsupported(format!("window function {name}()")))?;
    let func = if func == WindowFunc::Count && f.agg_star {
        WindowFunc::CountStar
    } else {
        func
    };
    if f.agg_distinct {
        // PostgreSQL refuses this itself (`DISTINCT is not implemented for
        // window functions`), so it is its answer rather than a gap here.
        return Err(Error::FeatureNotSupported(
            "DISTINCT is not implemented for window functions".into(),
        ));
    }
    let (partition_by, order_by, frame) = plan_window_def(over, named, def, params, keys)?;
    // The first argument is the one evaluated per row; the rest are
    // constants (`lag(v, 1, -1)`, `nth_value(v, 2)`, `ntile(3)`).
    let (arg, source_type, args) = plan_window_args(func, f, def, params)?;
    let field = format!("__win{}", windows.len());
    let result_type = window_result_type(func, source_type.as_deref());
    let filter = match f.agg_filter.as_deref() {
        None => None,
        Some(node) => {
            let fields: Vec<RowField> = def
                .columns
                .iter()
                .map(|c| (c.name.clone(), c.field(), c.pg_type.clone()))
                .collect();
            let mut sample = Document::new();
            for c in &def.columns {
                sample.insert(c.field(), sample_value_for_type(&c.pg_type));
            }
            Some(row_column_expr(node, &fields, params, &sample)?)
        }
    };
    extra.push(Column::new(&field, &result_type, false));
    windows.push(WindowItem {
        field: field.clone(),
        func,
        arg,
        args,
        partition_by,
        order_by,
        frame,
        result_type,
        source_type,
        filter,
    });
    Ok(field)
}

/// A target that HOLDS window calls inside an expression -- `v::numeric /
/// sum(v) over ()`. Each call is planned as its own window, replaced in the
/// expression by a reference to the field it lands in, and the expression is
/// then an ordinary per-row one over the row plus those fields.
#[allow(clippy::too_many_arguments)]
fn plan_nested_windows(
    rt: &pg_query::protobuf::ResTarget,
    named: &[pg_query::protobuf::WindowDef],
    def: &TableDef,
    params: &[Bson],
    keys: &mut usize,
    windows: &mut Vec<WindowItem>,
    extra: &mut Vec<Column>,
) -> Result<((String, String), Option<ColumnExpr>)> {
    let mut val = rt
        .val
        .as_deref()
        .cloned()
        .ok_or_else(|| Error::Parse("a target without a value".into()))?;
    let out = if rt.name.is_empty() {
        expression_column_name(&val)
    } else {
        rt.name.clone()
    };
    let mut failure: Option<Error> = None;
    walk_expr(&mut val, &mut |n| {
        if failure.is_some() {
            return Ok(());
        }
        if let Some(N::FuncCall(f)) = n.node.as_ref() {
            if f.over.is_some() {
                match plan_window_call(f, named, def, params, keys, windows, extra) {
                    Ok(field) => {
                        n.node = Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
                            fields: vec![string_node(&field)],
                            location: -1,
                        }));
                    }
                    Err(e) => failure = Some(e),
                }
            }
        }
        Ok(())
    })?;
    if let Some(e) = failure {
        return Err(e);
    }
    let mut extended = def.clone();
    extended.columns.extend(extra.iter().cloned());
    let target = pg_query::protobuf::Node {
        node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
            name: out,
            val: Some(Box::new(val)),
            location: rt.location,
            ..Default::default()
        }))),
    };
    let (mut c, mut k) = plan_table_targets(std::slice::from_ref(&target), &extended, params)?;
    match (c.pop(), k.pop()) {
        (Some(col), Some(cast)) => Ok((col, cast)),
        _ => Err(Error::Internal(
            "a nested window target planned to nothing".into(),
        )),
    }
}

/// A window call's per-row argument, its source type, and its literal extras.
fn plan_window_args(
    func: WindowFunc,
    f: &pg_query::protobuf::FuncCall,
    def: &TableDef,
    params: &[Bson],
) -> Result<(Option<ColumnExpr>, Option<String>, Vec<Bson>)> {
    if func == WindowFunc::CountStar {
        return Ok((None, None, Vec::new()));
    }
    // `row_number()` / `rank()` / `dense_rank()` / `percent_rank()` /
    // `cume_dist()` take none.
    if f.args.is_empty() {
        return Ok((None, None, Vec::new()));
    }
    let fields: Vec<RowField> = def
        .columns
        .iter()
        .map(|c| (c.name.clone(), c.field(), c.pg_type.clone()))
        .collect();
    let mut sample = Document::new();
    for c in &def.columns {
        sample.insert(c.field(), sample_value_for_type(&c.pg_type));
    }
    // `ntile(3)` takes its bucket count as the ONLY argument, and it is a
    // constant rather than a per-row value.
    if func == WindowFunc::Ntile {
        return Ok((None, None, vec![const_value(&f.args[0], params)?]));
    }
    let source_type = match f.args[0].node.as_ref() {
        Some(N::ColumnRef(c)) => {
            column_ref_name(c).and_then(|n| def.column(&n).map(|c| c.pg_type.clone()))
        }
        _ => None,
    };
    let expr = row_column_expr(&f.args[0], &fields, params, &sample)?;
    let source_type = source_type.or_else(|| match &expr {
        ColumnExpr::Row { result_type, .. } if !result_type.is_empty() => Some(result_type.clone()),
        _ => None,
    });
    let mut args = Vec::new();
    for extra in f.args.iter().skip(1) {
        args.push(const_value(extra, params)?);
    }
    Ok((Some(expr), source_type, args))
}

/// The aggregate names the aggregate planner handles.

fn has_aggregate(s: &pg_query::protobuf::SelectStmt) -> bool {
    // Only the names the aggregate planner actually handles. Any-FuncCall
    // routed a scalar call over a column (`regexp_replace(col, ...)`) into the
    // aggregate planner, whose refusal came out as a GROUPING error -- the
    // wrong error for what was a plain unsupported target.
    s.target_list.iter().any(|t| {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return false;
        };
        match rt.val.as_ref().and_then(|v| v.node.as_ref()) {
            // `f.over.is_none()`: `sum(v) OVER (...)` is a WINDOW call, not an
            // aggregate. Without the check it reached the aggregate planner,
            // which demanded a GROUP BY the query neither has nor needs, and
            // the client got `42803 column "id" must appear in the GROUP BY
            // clause` -- an error blaming the user for a missing feature.
            Some(N::FuncCall(f))
                if f.over.is_none()
                    && func_name(f)
                        .as_deref()
                        .is_some_and(|n| aggregate_func(n, f.agg_within_group).is_some()) =>
            {
                true
            }
            // An aggregate WRAPPED in an expression -- `count(*) + 1`,
            // `coalesce(sum(n), 0)` -- is still an aggregate query. Without
            // this it was planned as a plain SELECT with a computed column
            // and reached the per-row scalar evaluator, which has no `sum`.
            _ => rt.val.as_deref().is_some_and(contains_nested_aggregate),
        }
    })
}

/// A window function OVER an aggregate -- `sum(sum(v)) over (order by g)`
/// beside a GROUP BY -- as the two queries it is: the grouping, then the
/// window over the grouped rows.
///
/// PostgreSQL evaluates windows after GROUP BY and HAVING, so the inner query
/// takes the FROM / WHERE / GROUP BY / HAVING and outputs every aggregate and
/// every column the rest of the statement reads; the outer one keeps the
/// targets (each aggregate and column replaced by the inner output), the
/// window definitions, DISTINCT, ORDER BY and LIMIT.
fn split_window_over_aggregate(
    s: &pg_query::protobuf::SelectStmt,
) -> Result<pg_query::protobuf::SelectStmt> {
    let mut inner_targets: Vec<pg_query::protobuf::Node> = Vec::new();
    let mut seen: Vec<(pg_query::protobuf::Node, String)> = Vec::new();
    // Structural identity ignoring where in the text each was written.
    static LOCATION: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let location =
        LOCATION.get_or_init(|| regex::Regex::new(r"location: -?\d+").expect("static regex"));
    let strip_location = |n: &pg_query::protobuf::Node| -> String {
        location.replace_all(&format!("{n:?}"), "").into_owned()
    };
    let mut outer = s.clone();
    let mut replace = |n: &mut pg_query::protobuf::Node| -> Result<()> {
        let is_agg = matches!(n.node.as_ref(), Some(N::FuncCall(f))
            if f.over.is_none() && func_name(f).is_some_and(|name| aggregate_func(&name, f.agg_within_group).is_some()));
        let is_col = matches!(n.node.as_ref(), Some(N::ColumnRef(c))
            if !c.fields.iter().any(|f| matches!(f.node, Some(N::AStar(_)))));
        if !is_agg && !is_col {
            return Ok(());
        }
        let key = strip_location(n);
        let alias = match seen.iter().position(|(_, k)| *k == key) {
            Some(i) => format!("__g{i}"),
            None => {
                let alias = format!("__g{}", seen.len());
                inner_targets.push(pg_query::protobuf::Node {
                    node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                        name: alias.clone(),
                        val: Some(Box::new(n.clone())),
                        location: -1,
                        ..Default::default()
                    }))),
                });
                seen.push((n.clone(), key));
                alias
            }
        };
        n.node = Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
            fields: vec![string_node(&alias)],
            location: -1,
        }));
        Ok(())
    };
    let mut targets = Vec::new();
    for mut t in std::mem::take(&mut outer.target_list) {
        if let Some(N::ResTarget(rt)) = t.node.as_mut() {
            // An unnamed target keeps the name it would have had.
            if rt.name.is_empty() {
                rt.name = match rt.val.as_deref().and_then(|v| v.node.as_ref()) {
                    Some(N::ColumnRef(c)) => column_ref_name(c).unwrap_or_default(),
                    Some(N::FuncCall(f)) => func_name(f).unwrap_or_default(),
                    Some(_) => expression_column_name(rt.val.as_deref().expect("some")),
                    _ => String::new(),
                };
            }
            if let Some(v) = rt.val.as_deref_mut() {
                walk_expr(v, &mut replace)?;
            }
        }
        targets.push(t);
    }
    outer.target_list = targets;
    for n in outer
        .sort_clause
        .iter_mut()
        .chain(outer.window_clause.iter_mut())
        .chain(outer.distinct_clause.iter_mut())
    {
        walk_expr(n, &mut replace)?;
    }
    let inner = pg_query::protobuf::SelectStmt {
        target_list: inner_targets,
        from_clause: s.from_clause.clone(),
        where_clause: s.where_clause.clone(),
        group_clause: s.group_clause.clone(),
        having_clause: s.having_clause.clone(),
        limit_option: pg_query::protobuf::LimitOption::Default as i32,
        op: pg_query::protobuf::SetOperation::SetopNone as i32,
        ..Default::default()
    };
    outer.from_clause = vec![pg_query::protobuf::Node {
        node: Some(N::RangeSubselect(Box::new(
            pg_query::protobuf::RangeSubselect {
                lateral: false,
                subquery: Some(Box::new(pg_query::protobuf::Node {
                    node: Some(N::SelectStmt(Box::new(inner))),
                })),
                alias: Some(pg_query::protobuf::Alias {
                    aliasname: "__grouped".into(),
                    colnames: Vec::new(),
                }),
            },
        ))),
    }];
    outer.where_clause = None;
    outer.group_clause = Vec::new();
    outer.having_clause = None;
    // Every aggregate moved inside; one still here would split again, for
    // ever.
    if has_aggregate(&outer) {
        return Err(Error::Unsupported(
            "a window function over this aggregate".into(),
        ));
    }
    Ok(outer)
}

/// A `FROM <set-returning function>(...)` item as a materialised source.
///
/// `generate_series` keeps its own path (`series_from_clause`): it is a RANGE
/// and can be produced lazily, so `generate_series(1, 10000000)` must not
/// become ten million rows in a Vec. The functions here are bounded by their
/// arguments, so materialising them costs what the argument already cost.
///
/// The rows are handed back as a `SubSource` wrapping a `ValuesConstant` --
/// the same shape `FROM (SELECT ...) s` produces. That is deliberate: the
/// FROM-subquery path already handles the WHERE, ORDER BY, LIMIT, aggregates,
/// column aliases and `*` expansion that a client puts around one of these,
/// and reusing it means none of that has to be written twice or kept in step.
fn srf_from_clause(from: &pg_query::protobuf::Node, params: &[Bson]) -> Result<Option<SubSource>> {
    let Some(N::RangeFunction(rf)) = from.node.as_ref() else {
        return Ok(None);
    };
    if rf.is_rowsfrom && rf.functions.len() > 1 {
        return Err(Error::Unsupported(
            "ROWS FROM with several functions".into(),
        ));
    }
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
    let name = func_name(&call).unwrap_or_default();
    let Some((names, types, rows)) = srf_rows(&name, &call, params)? else {
        return Ok(None);
    };
    // `AS t(a, b)` renames positionally; `AS t` names the table, and for a
    // single-column function the column takes that name too -- which is what
    // `FROM unnest(...) x` relies on to make `x` both the alias and the column.
    let (alias, colnames): (String, Vec<String>) = match rf.alias.as_ref() {
        Some(a) => (
            a.aliasname.clone(),
            a.colnames.iter().filter_map(alias_colname).collect(),
        ),
        None => (name.clone(), Vec::new()),
    };
    let mut names = names;
    if colnames.len() > names.len() {
        return Err(Error::Parse(format!(
            "table \"{alias}\" has {} columns available but {} columns specified",
            names.len(),
            colnames.len()
        )));
    }
    for (n, given) in names.iter_mut().zip(&colnames) {
        *n = given.clone();
    }
    if colnames.is_empty() && names.len() == 1 && !alias.is_empty() {
        names[0] = alias.clone();
    }
    let plan = Statement::ValuesConstant(ValuesConstant {
        names: names.clone(),
        types: types.clone(),
        rows,
    });
    let mut def = TableDef::new(
        &alias,
        names
            .iter()
            .zip(&types)
            .map(|(n, t)| Column::new(n, t, false))
            .collect(),
    );
    def.name = alias.clone();
    Ok(Some(SubSource {
        alias,
        plan: Box::new(plan),
        def,
    }))
}

/// What a set-returning function yields: the output column names, their
/// declared types, and the rows -- one `Vec<Bson>` per row, one cell per
/// column.
type SrfRows = (Vec<String>, Vec<String>, Vec<Vec<Bson>>);

/// The rows a set-returning function produces. `None` means this is not one of
/// the set-returning functions this server materialises.
fn srf_rows(
    name: &str,
    call: &pg_query::protobuf::FuncCall,
    params: &[Bson],
) -> Result<Option<SrfRows>> {
    let one = |v: Vec<Bson>, ty: &str| {
        (
            vec![name.to_string()],
            vec![ty.to_string()],
            v.into_iter().map(|x| vec![x]).collect::<Vec<_>>(),
        )
    };
    let args = |n: usize| -> Result<Vec<Bson>> {
        if call.args.len() != n {
            return Err(Error::Parse(format!(
                "function {name} does not exist with that argument list"
            )));
        }
        call.args.iter().map(|a| const_value(a, params)).collect()
    };
    // A user-defined set-returning function: the executor runs it.
    if let Some(u) = correlated::user_function(name, call.args.len()).filter(|u| u.returns_set) {
        let a: Vec<Bson> = call
            .args
            .iter()
            .map(|x| const_value(x, params))
            .collect::<Result<_>>()?;
        let (names, types) = if u.columns.is_empty() {
            (vec![name.to_string()], vec![u.return_type.clone()])
        } else {
            (
                u.columns.iter().map(|(n, _)| n.clone()).collect(),
                u.columns.iter().map(|(_, t)| t.clone()).collect(),
            )
        };
        let rows = match correlated::call_user_function(&u, &a)? {
            correlated::FnResult::Rows(_, _, rows) => rows,
            correlated::FnResult::Value(v) => vec![vec![v]],
        };
        return Ok(Some((names, types, rows)));
    }
    // `jsonb_path_query(target, path [, vars [, silent]])`: one jsonb row
    // per item the path yields.
    if matches!(name, "jsonb_path_query" | "jsonb_path_query_tz") {
        let a: Vec<Bson> = call
            .args
            .iter()
            .map(|x| const_value(x, params))
            .collect::<Result<_>>()?;
        let rows = match jsonpath_call(name, &a)? {
            Bson::Array(items) => items.into_iter().map(|i| vec![i]).collect(),
            _ => Vec::new(),
        };
        return Ok(Some((
            vec![name.to_string()],
            vec!["jsonb".to_string()],
            rows,
        )));
    }
    // The JSON set-returning functions. A json / jsonb value is its text
    // here; each element or member is rendered back as the same type.
    let json_arg = || -> Result<Option<(json::Json, bool)>> {
        let a = args(1)?;
        let is_jsonb = name.starts_with("jsonb");
        match &a[0] {
            Bson::Null => Ok(None),
            Bson::String(text) => json::parse(text).map(|j| Some((j, is_jsonb))).map_err(|_| {
                Error::InvalidText(format!("invalid input syntax for type json: {text}"))
            }),
            other => Err(Error::Unsupported(format!(
                "{name}() over {}",
                inferred_type(other)
            ))),
        }
    };
    let render = |j: &json::Json, jsonb: bool| {
        if jsonb {
            json::render_jsonb(j)
        } else {
            json::render_json(j)
        }
    };
    let kind_of = |j: &json::Json| match j {
        json::Json::Object(_) => "an object",
        json::Json::Array(_) => "an array",
        _ => "a scalar",
    };
    match name {
        "jsonb_array_elements"
        | "json_array_elements"
        | "jsonb_array_elements_text"
        | "json_array_elements_text" => {
            let as_text = name.ends_with("_text");
            let ty = if as_text {
                "text"
            } else if name.starts_with("jsonb") {
                "jsonb"
            } else {
                "json"
            };
            let col = "value";
            let rows = match json_arg()? {
                None => Vec::new(),
                Some((json::Json::Array(items), jsonb)) => items
                    .iter()
                    .map(|v| {
                        vec![if as_text {
                            json::as_sql_text(v).map_or(Bson::Null, Bson::String)
                        } else {
                            Bson::String(render(v, jsonb))
                        }]
                    })
                    .collect(),
                Some((other, _)) => {
                    return Err(Error::InvalidParameter(format!(
                        "cannot extract elements from {}",
                        kind_of(&other)
                    )))
                }
            };
            return Ok(Some((vec![col.to_string()], vec![ty.to_string()], rows)));
        }
        "jsonb_each" | "json_each" | "jsonb_each_text" | "json_each_text" => {
            let as_text = name.ends_with("_text");
            let ty = if as_text {
                "text"
            } else if name.starts_with("jsonb") {
                "jsonb"
            } else {
                "json"
            };
            let rows = match json_arg()? {
                None => Vec::new(),
                Some((json::Json::Object(members), jsonb)) => {
                    // jsonb reports its members in its normalised (sorted)
                    // order; json keeps the input's.
                    let mut members = members.clone();
                    if jsonb {
                        members.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then(a.0.cmp(&b.0)));
                        members.dedup_by(|a, b| a.0 == b.0);
                    }
                    members
                        .iter()
                        .map(|(k, v)| {
                            vec![
                                Bson::String(k.clone()),
                                if as_text {
                                    json::as_sql_text(v).map_or(Bson::Null, Bson::String)
                                } else {
                                    Bson::String(render(v, jsonb))
                                },
                            ]
                        })
                        .collect()
                }
                Some((other, _)) => {
                    return Err(Error::InvalidParameter(format!(
                        "cannot call {name} on {}",
                        match other {
                            json::Json::Array(_) => "an array",
                            _ => "a non-object",
                        }
                    )))
                }
            };
            return Ok(Some((
                vec!["key".into(), "value".into()],
                vec!["text".into(), ty.to_string()],
                rows,
            )));
        }
        // One row per match: the capture groups as a text[], or the whole
        // match when there are none. Without `g`, only the first.
        "regexp_matches" => {
            if !(2..=3).contains(&call.args.len()) {
                return Err(Error::UndefinedFunction(format!(
                    "function {name} does not exist with that argument list"
                )));
            }
            let a: Vec<Bson> = call
                .args
                .iter()
                .map(|x| const_value(x, params))
                .collect::<Result<_>>()?;
            if a.contains(&Bson::Null) {
                return Ok(Some((vec![name.into()], vec!["text[]".into()], Vec::new())));
            }
            let text = |v: &Bson| match v {
                Bson::String(s) => s.clone(),
                other => value_text(other),
            };
            let (source, pattern) = (text(&a[0]), text(&a[1]));
            let flags = a.get(2).map(&text).unwrap_or_default();
            let re = regex::Regex::new(&format!(
                "{}{pattern}",
                if flags.contains('i') { "(?i)" } else { "" }
            ))
            .map_err(|_| {
                Error::InvalidRegex(format!("invalid regular expression: \"{pattern}\""))
            })?;
            let row = |c: regex::Captures| -> Vec<Bson> {
                let cells: Vec<Bson> = if c.len() > 1 {
                    (1..c.len())
                        .map(|i| {
                            c.get(i)
                                .map_or(Bson::Null, |m| Bson::String(m.as_str().to_string()))
                        })
                        .collect()
                } else {
                    vec![Bson::String(c[0].to_string())]
                };
                vec![Bson::Array(cells)]
            };
            let rows: Vec<Vec<Bson>> = if flags.contains('g') {
                re.captures_iter(&source).map(row).collect()
            } else {
                re.captures(&source).map(row).into_iter().collect()
            };
            return Ok(Some((vec![name.into()], vec!["text[]".into()], rows)));
        }
        "jsonb_object_keys" | "json_object_keys" => {
            let rows = match json_arg()? {
                None => Vec::new(),
                Some((json::Json::Object(members), jsonb)) => {
                    let mut keys: Vec<String> = members.into_iter().map(|(k, _)| k).collect();
                    if jsonb {
                        keys.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
                        keys.dedup();
                    }
                    keys.into_iter().map(|k| vec![Bson::String(k)]).collect()
                }
                Some((other, _)) => {
                    return Err(Error::InvalidParameter(format!(
                        "cannot call {name} on {}",
                        kind_of(&other)
                    )))
                }
            };
            return Ok(Some((vec![name.to_string()], vec!["text".into()], rows)));
        }
        _ => {}
    }
    Ok(Some(match name {
        "unnest" => {
            // Multi-argument `unnest(a, b)` zips the arrays and pads the short
            // ones with NULLs -- a different shape from this single-column one,
            // and refused by name until it is written.
            // Several arrays ZIP: one column each, as many rows as the longest,
            // the shorter ones padded with NULL.
            if call.args.len() > 1 {
                let mut columns = Vec::new();
                let mut types = Vec::new();
                let mut lists = Vec::new();
                for a in &call.args {
                    let value = const_value(a, params)?;
                    let element = static_type(a, &value)
                        .strip_suffix("[]")
                        .map(str::to_owned)
                        .ok_or_else(|| Error::Unsupported("unnest() over a non-array".into()))?;
                    lists.push(match value {
                        Bson::Null => Vec::new(),
                        v @ Bson::Array(_) => arrays::flatten(&v),
                        _ => return Err(Error::Unsupported("unnest() over a non-array".into())),
                    });
                    columns.push(name.to_string());
                    types.push(element);
                }
                let n = lists.iter().map(Vec::len).max().unwrap_or(0);
                let rows = (0..n)
                    .map(|i| {
                        lists
                            .iter()
                            .map(|l| l.get(i).cloned().unwrap_or(Bson::Null))
                            .collect()
                    })
                    .collect();
                return Ok(Some((columns, types, rows)));
            }
            let value = const_value(&call.args[0], params)?;
            let element = static_type(&call.args[0], &value)
                .strip_suffix("[]")
                .map(str::to_owned)
                .ok_or_else(|| Error::Unsupported("unnest() over a non-array".into()))?;
            // A multidimensional array unnests to its LEAVES, in row-major
            // order -- `unnest(ARRAY[[1,2],[3,4]])` is four rows, not two.
            let values = match value {
                Bson::Null => Vec::new(),
                v @ Bson::Array(_) => arrays::flatten(&v),
                _ => return Err(Error::Unsupported("unnest() over a non-array".into())),
            };
            one(values, &element)
        }
        "generate_subscripts" => {
            let a = args(2)?;
            let dims = arrays::dim_lengths(&a[0]);
            let dim = match &a[1] {
                Bson::Int32(i) => i64::from(*i),
                Bson::Int64(i) => *i,
                _ => return Ok(Some(one(Vec::new(), "int4"))),
            };
            let values = match usize::try_from(dim).ok().filter(|d| *d >= 1) {
                Some(d) if d <= dims.len() => (1..=dims[d - 1])
                    .map(|i| Bson::Int32(i32::try_from(i).unwrap_or(i32::MAX)))
                    .collect(),
                _ => Vec::new(),
            };
            one(values, "int4")
        }
        "regexp_split_to_table" => {
            if call.args.len() < 2 || call.args.len() > 3 {
                return Err(Error::Parse(format!(
                    "function {name} does not exist with that argument list"
                )));
            }
            let a: Vec<Bson> = call
                .args
                .iter()
                .map(|x| const_value(x, params))
                .collect::<Result<_>>()?;
            if a.iter().take(2).any(|v| *v == Bson::Null) {
                return Ok(Some(one(Vec::new(), "text")));
            }
            let parts = scalar::call("regexp_split_to_array", &a)
                .ok_or_else(|| Error::Unsupported("this FROM function".into()))??;
            let values = match parts {
                Bson::Array(items) => items,
                _ => Vec::new(),
            };
            one(values, "text")
        }
        _ => return Ok(None),
    }))
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
    let filter = series_where(s, &series, params)?;
    let mut columns: Vec<(String, String)> = Vec::new();
    let mut casts: Vec<Option<ColumnExpr>> = Vec::new();
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
                casts.push(None);
            }
            // `SELECT 1 FROM generate_series(...)` -- a literal per row, which
            // is how the suite counts a series' rows without reading its value.
            Some(N::AConst(_)) => {
                let val = rt.val.as_ref().expect("ResTarget has a val for AConst");
                let value = const_value(val, params)?;
                let out = if rt.name.is_empty() {
                    "?column?".to_string()
                } else {
                    rt.name.clone()
                };
                let result_type = const_col_type(&value).to_string();
                columns.push((out.clone(), out));
                casts.push(Some(ColumnExpr::Const { value, result_type }));
            }
            // Anything else is an expression over the row -- `i + 1`,
            // `'2021-01-01'::date + i`, `i::int4`.
            Some(_) => {
                let val = rt.val.as_ref().expect("ResTarget has a val");
                let out = if rt.name.is_empty() {
                    expression_column_name(val)
                } else {
                    rt.name.clone()
                };
                let fields = vec![(
                    series.column.clone(),
                    series.column.clone(),
                    "int4".to_string(),
                )];
                // The series' first value is a real row to type the
                // expression from: a scalar call is typed by its result.
                let mut sample = Document::new();
                sample.insert(
                    series.column.clone(),
                    Bson::Int32(i32::try_from(series.start).unwrap_or_default()),
                );
                let row = row_column_expr(val, &fields, params, &sample)?;
                columns.push((out, series.column.clone()));
                casts.push(Some(row));
            }
        }
    }
    if columns.is_empty() {
        columns.push((series.column.clone(), series.column.clone()));
        casts.push(None);
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
            expr: None,
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
    let distinct = plan_distinct(s, &|name| {
        columns
            .iter()
            .find(|(out, _)| out == name)
            .map(|(_, f)| f.clone())
    })?;

    Ok(Statement::Select(Select {
        table: String::new(),
        series: Some(series),
        sub: None,
        windows: Vec::new(),
        join: None,
        columns,
        casts,
        filter,
        residual: None,
        order,
        limit,
        offset,
        distinct,
    }))
}

/// The WHERE clause of a select over a generated series, lowered against the
/// one int4 column the series exposes. `where false` over a series answers
/// no rows and still describes the column (measured), which is what a
/// server-side cursor opened on it relies on.
fn series_where(
    s: &pg_query::protobuf::SelectStmt,
    series: &Series,
    params: &[Bson],
) -> Result<Document> {
    match s.where_clause.as_ref() {
        None => Ok(Document::new()),
        // `where false` / `where $1`: a predicate with no column in it keeps
        // every row or none.
        Some(w) if matches!(w.node.as_ref(), Some(N::AConst(_) | N::ParamRef(_))) => {
            Ok(if constant_where(w, params)? {
                Document::new()
            } else {
                doc! { "$expr": false }
            })
        }
        Some(w) => {
            let def = TableDef::new(
                "generate_series",
                vec![Column::new(&series.column, "int4", false)],
            );
            lower_where(w, &def, params)
        }
    }
}

/// A WHERE with no row to range over -- a constant, or a parameter -- keeps
/// (`true`) or drops (`false`, NULL) what it guards. A non-boolean is 42804,
/// worded as PostgreSQL words it (probed PG 16).
fn constant_where(w: &pg_query::protobuf::Node, params: &[Bson]) -> Result<bool> {
    Ok(match const_value(w, params)? {
        Bson::Boolean(b) => b,
        Bson::Null => false,
        // A bare string literal is of UNKNOWN type and is read as a
        // boolean: `where 'x'` is 22P02, not 42804.
        Bson::String(text) if matches!(w.node.as_ref(), Some(N::AConst(_))) => {
            matches!(cast_value(Bson::String(text), "bool")?, Bson::Boolean(true))
        }
        other => {
            return Err(Error::DatatypeMismatch(format!(
                "argument of WHERE must be type boolean, not type {}",
                display_type(&static_type(w, &other))
            )));
        }
    })
}

/// The name PostgreSQL gives an unaliased expression column: a cast or a
/// parenthesised reference keeps the column's name, a call takes the
/// function's, anything else is `?column?`.
fn expression_column_name(node: &pg_query::protobuf::Node) -> String {
    match node.node.as_ref() {
        Some(N::ColumnRef(c)) => column_ref_name(c).unwrap_or_else(|| "?column?".to_string()),
        // A cast keeps a column's or a call's name (`id::int8` is `id`,
        // `abs(n)::int8` is `abs`) and otherwise takes the TARGET type's bare
        // name: `(1 + 2)::int8` is `int8`, `x::varchar(3)` is `varchar`.
        // Nested casts resolve from the inside: `(id::text)::int8` is still
        // `id`, and `('1'::text)::int8` is the OUTER type, `int8`.
        Some(N::TypeCast(tc)) => {
            let inner = tc.arg.as_deref();
            let strong = inner.is_some_and(cast_source_names_column);
            let inner_name = inner.map(expression_column_name);
            match inner_name {
                Some(n) if strong => n,
                _ => tc
                    .type_name
                    .as_ref()
                    .and_then(|t| t.names.last())
                    .and_then(type_name_of_node)
                    .unwrap_or_else(|| "?column?".to_string()),
            }
        }
        Some(N::FuncCall(f)) => f
            .funcname
            .last()
            .and_then(|n| match n.node.as_ref() {
                Some(N::String(st)) => Some(st.sval.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "?column?".to_string()),
        // The constructor keywords name their column: `array[1]` is `array`,
        // `row(1)` is `row`, `coalesce(1)` / `greatest(1, 2)` / `least(1)` /
        // `nullif(1, 2)` take the keyword. Measured on PG 16.
        Some(N::AArrayExpr(_)) => "array".to_string(),
        Some(N::RowExpr(_)) => "row".to_string(),
        Some(N::CoalesceExpr(_)) => "coalesce".to_string(),
        Some(N::MinMaxExpr(m)) => {
            if m.op == pg_query::protobuf::MinMaxOp::IsGreatest as i32 {
                "greatest".to_string()
            } else {
                "least".to_string()
            }
        }
        Some(N::AExpr(e)) if AExprKind::try_from(e.kind) == Ok(AExprKind::AexprNullif) => {
            "nullif".to_string()
        }
        // A subscript / field selection names from what it selects: `x[1]`
        // is `x`, `(array[1, 2])[1]` is `array`, and `(r).f` is the field.
        Some(N::AIndirection(ind)) => {
            let field = ind.indirection.last().and_then(|n| match n.node.as_ref() {
                Some(N::String(st)) => Some(st.sval.clone()),
                _ => None,
            });
            match (field, ind.arg.as_deref()) {
                (Some(f), _) => f,
                (None, Some(arg)) => expression_column_name(arg),
                (None, None) => "?column?".to_string(),
            }
        }
        _ => "?column?".to_string(),
    }
}

/// Does this cast source name its column -- a column reference or a call,
/// through any number of casts? (PostgreSQL's `FigureColname` strength 2.)
fn cast_source_names_column(node: &pg_query::protobuf::Node) -> bool {
    match node.node.as_ref() {
        // PostgreSQL's FigureColname "strength 2" sources: anything that
        // yields a definite name keeps it through a cast (`array[1]::text`
        // is `array`); only a nameless expression takes the target type.
        Some(N::ColumnRef(_))
        | Some(N::FuncCall(_))
        | Some(N::AArrayExpr(_))
        | Some(N::RowExpr(_))
        | Some(N::CoalesceExpr(_))
        | Some(N::MinMaxExpr(_)) => true,
        Some(N::AExpr(e)) => AExprKind::try_from(e.kind) == Ok(AExprKind::AexprNullif),
        Some(N::AIndirection(ind)) => {
            ind.indirection
                .last()
                .is_some_and(|n| matches!(n.node.as_ref(), Some(N::String(_))))
                || ind.arg.as_deref().is_some_and(cast_source_names_column)
        }
        Some(N::TypeCast(tc)) => tc.arg.as_deref().is_some_and(cast_source_names_column),
        _ => false,
    }
}

/// One row column an expression can reference: (column name, stored field,
/// PostgreSQL type). A generated source names its field as its column; a
/// table's PK column is stored as `_id`.
pub type RowField = (String, String, String);

/// Build a `ColumnExpr::Row` for an expression over the row's `fields`,
/// given the statement's own `params`.
///
/// Every column reference is rewritten into a parameter reference numbered
/// past the statement's parameters, and the result type is inferred with the
/// row columns' types visible to `declared_param_type`, so a `$n` standing in
/// for an `int4` column types as one.
fn row_column_expr(
    node: &pg_query::protobuf::Node,
    fields: &[RowField],
    params: &[Bson],
    sample: &Document,
) -> Result<ColumnExpr> {
    let mut expr = node.clone();
    rewrite_column_refs(&mut expr, fields, params.len())?;
    let mut out = ColumnExpr::Row {
        expr: Box::new(expr.clone()),
        fields: fields.to_vec(),
        params: params.to_vec(),
        result_type: String::new(),
    };
    // A sample row's value types what the node alone cannot (a scalar call's
    // result); an expression that fails on the sample still gets the node's
    // own type, and fails per row when run.
    let sample_value =
        correlated::without_side_effects(|| apply_row_expr(&out, sample)).unwrap_or(Bson::Null);
    let previous = declare_row_fields(params.len(), fields);
    let result_type = static_type(&expr, &sample_value);
    PLAN_PARAM_TYPES.with(|t| *t.borrow_mut() = previous);
    if let ColumnExpr::Row { result_type: t, .. } = &mut out {
        *t = result_type;
    }
    Ok(out)
}

/// Replace each `ColumnRef` under `node` with `ParamRef(n_params + 1 + i)`
/// where `i` is the column's position in `fields`; an unknown column is
/// `42703`. Walks the expression node kinds the constant evaluator handles.
fn rewrite_column_refs(
    node: &mut pg_query::protobuf::Node,
    fields: &[RowField],
    n_params: usize,
) -> Result<()> {
    walk_column_refs(node, &mut |inner, c| {
        let name = column_ref_name(c).ok_or_else(|| Error::Unsupported("this column".into()))?;
        let idx = fields
            .iter()
            .position(|(f, _, _)| *f == name)
            .ok_or_else(|| Error::UndefinedColumn(name.clone()))?;
        let number = i32::try_from(n_params + 1 + idx)
            .map_err(|_| Error::Unsupported("this many columns".into()))?;
        *inner = N::ParamRef(pg_query::protobuf::ParamRef {
            number,
            location: c.location,
        });
        Ok(())
    })
}

/// Walk every `ColumnRef` under `node`, letting the caller rewrite it.
///
/// Split out of `rewrite_column_refs` so `ON CONFLICT DO UPDATE` can rename
/// `excluded.x` BEFORE resolution without a second copy of this traversal.
/// Two walkers over the same twelve node kinds would drift, and the one that
/// drifted would silently stop seeing columns inside (say) a `CASE`.
fn walk_column_refs(
    node: &mut pg_query::protobuf::Node,
    visit: &mut dyn FnMut(&mut N, &pg_query::protobuf::ColumnRef) -> Result<()>,
) -> Result<()> {
    walk_expr(node, &mut |n| {
        let Some(N::ColumnRef(c)) = n.node.as_ref() else {
            return Ok(());
        };
        // Cloned so the callback can replace the node while reading the ref.
        let c = c.clone();
        visit(n.node.as_mut().expect("matched above"), &c)
    })
}

/// Walk every expression node under `node`, OUTERMOST FIRST, letting the
/// caller rewrite each in place.
///
/// One traversal, two users: `walk_column_refs` rewrites column references
/// into parameters, and the subquery resolver replaces `SubLink` nodes with
/// the values they return. Splitting them into two walks over the same dozen
/// node kinds is exactly the drift `walk_column_refs` was already warning
/// about -- whichever fell behind would silently stop seeing expressions
/// inside (say) a `CASE`.
///
/// It deliberately does NOT descend into a `SubLink`'s body: a subquery's own
/// column references belong to ITS tables, not the row being walked, and
/// rewriting them as outer-row fields would bind the wrong values. The
/// SubLink node itself is still visited, which is all the resolver needs --
/// it recurses into the body on its own terms.
fn walk_expr(
    node: &mut pg_query::protobuf::Node,
    visit: &mut dyn FnMut(&mut pg_query::protobuf::Node) -> Result<()>,
) -> Result<()> {
    visit(node)?;
    let Some(inner) = node.node.as_mut() else {
        return Ok(());
    };
    match inner {
        N::TypeCast(tc) => tc
            .arg
            .as_deref_mut()
            .map_or(Ok(()), |a| walk_expr(a, visit)),
        N::AExpr(e) => {
            if let Some(l) = e.lexpr.as_deref_mut() {
                walk_expr(l, visit)?;
            }
            if let Some(r) = e.rexpr.as_deref_mut() {
                walk_expr(r, visit)?;
            }
            Ok(())
        }
        N::FuncCall(f) => {
            f.args.iter_mut().try_for_each(|a| walk_expr(a, visit))?;
            // An aggregate's ORDER BY and FILTER, and a window's PARTITION BY
            // and ORDER BY, read the row too.
            f.agg_order
                .iter_mut()
                .try_for_each(|a| walk_expr(a, visit))?;
            if let Some(filter) = f.agg_filter.as_deref_mut() {
                walk_expr(filter, visit)?;
            }
            if let Some(over) = f.over.as_deref_mut() {
                walk_window_def(over, visit)?;
            }
            Ok(())
        }
        N::WindowDef(w) => walk_window_def(w, visit),
        N::SortBy(sb) => sb
            .node
            .as_deref_mut()
            .map_or(Ok(()), |a| walk_expr(a, visit)),
        N::ResTarget(rt) => rt
            .val
            .as_deref_mut()
            .map_or(Ok(()), |a| walk_expr(a, visit)),
        N::BooleanTest(t) => t.arg.as_deref_mut().map_or(Ok(()), |a| walk_expr(a, visit)),
        N::CollateClause(c) => c.arg.as_deref_mut().map_or(Ok(()), |a| walk_expr(a, visit)),
        N::NamedArgExpr(a) => a.arg.as_deref_mut().map_or(Ok(()), |a| walk_expr(a, visit)),
        N::GroupingSet(g) => g.content.iter_mut().try_for_each(|a| walk_expr(a, visit)),
        N::List(l) => l.items.iter_mut().try_for_each(|a| walk_expr(a, visit)),
        // The TESTED expression of `x IN (select ...)` belongs to this row;
        // the subquery's body does not (see above).
        N::SubLink(sl) => sl
            .testexpr
            .as_deref_mut()
            .map_or(Ok(()), |a| walk_expr(a, visit)),
        N::BoolExpr(b) => b.args.iter_mut().try_for_each(|a| walk_expr(a, visit)),
        N::AArrayExpr(a) => a.elements.iter_mut().try_for_each(|a| walk_expr(a, visit)),
        N::RowExpr(r) => r.args.iter_mut().try_for_each(|a| walk_expr(a, visit)),
        N::CoalesceExpr(c) => c.args.iter_mut().try_for_each(|a| walk_expr(a, visit)),
        N::MinMaxExpr(m) => m.args.iter_mut().try_for_each(|a| walk_expr(a, visit)),
        N::NullTest(t) => t.arg.as_deref_mut().map_or(Ok(()), |a| walk_expr(a, visit)),
        N::CaseExpr(c) => {
            if let Some(a) = c.arg.as_deref_mut() {
                walk_expr(a, visit)?;
            }
            for w in &mut c.args {
                if let Some(N::CaseWhen(cw)) = w.node.as_mut() {
                    if let Some(e) = cw.expr.as_deref_mut() {
                        walk_expr(e, visit)?;
                    }
                    if let Some(r) = cw.result.as_deref_mut() {
                        walk_expr(r, visit)?;
                    }
                }
            }
            c.defresult
                .as_deref_mut()
                .map_or(Ok(()), |d| walk_expr(d, visit))
        }
        N::AIndirection(a) => {
            // A subscript's BOUNDS are expressions of their own: `ia[n]` and
            // `ia[lo:hi]` reference columns there, and leaving them unwalked
            // left the `ColumnRef` in place to be refused at evaluation.
            if let Some(arg) = a.arg.as_deref_mut() {
                walk_expr(arg, visit)?;
            }
            for ind in &mut a.indirection {
                if let Some(N::AIndices(idx)) = ind.node.as_mut() {
                    for bound in [idx.lidx.as_deref_mut(), idx.uidx.as_deref_mut()]
                        .into_iter()
                        .flatten()
                    {
                        walk_expr(bound, visit)?;
                    }
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn walk_window_def(
    w: &mut pg_query::protobuf::WindowDef,
    visit: &mut dyn FnMut(&mut pg_query::protobuf::Node) -> Result<()>,
) -> Result<()> {
    w.partition_clause
        .iter_mut()
        .chain(w.order_clause.iter_mut())
        .try_for_each(|a| walk_expr(a, visit))
}

/// Evaluate a `ColumnExpr::Row` over one row.
pub fn apply_row_expr(expr: &ColumnExpr, row: &Document) -> Result<Bson> {
    let ColumnExpr::Row {
        expr,
        fields,
        params,
        ..
    } = expr
    else {
        return Err(Error::Unsupported("not a row expression".into()));
    };
    let mut all = params.clone();
    all.extend(
        fields
            .iter()
            .map(|(_, f, _)| row.get(f).cloned().unwrap_or(Bson::Null)),
    );
    let previous = declare_row_fields(params.len(), fields);
    let out = const_value(expr, &all);
    PLAN_PARAM_TYPES.with(|t| *t.borrow_mut() = previous);
    out
}

/// Declare a row's fields as the parameter types numbered past the
/// statement's own `n_params`, returning the previous declarations so the
/// caller can restore them.
fn declare_row_fields(n_params: usize, fields: &[RowField]) -> Vec<Option<String>> {
    let mut types = PLAN_PARAM_TYPES.with(|t| t.borrow().clone());
    types.resize(n_params, None);
    types.extend(fields.iter().map(|(_, _, t)| Some(t.clone())));
    PLAN_PARAM_TYPES.with(|t| t.replace(types))
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

/// A planned target list over one table's rows: the (output name, stored
/// field) pairs and the per-column expression.
type TableTargets = (Vec<(String, String)>, Vec<Option<ColumnExpr>>);

/// Plan a target list over one table's rows -- the shape both a SELECT's
/// select list and an INSERT's RETURNING list take.
fn plan_table_targets(
    target_list: &[pg_query::protobuf::Node],
    def: &TableDef,
    params: &[Bson],
) -> Result<TableTargets> {
    let mut columns: Vec<(String, String)> = Vec::new();
    let mut casts: Vec<Option<ColumnExpr>> = Vec::new();
    for t in target_list {
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
            Some(N::TypeCast(tc)) if cast_chain_over_column(tc).is_ok() => {
                let (col_name, chain) = cast_chain_over_column(tc)?;
                let field = def
                    .field_of(&col_name)
                    .ok_or_else(|| Error::UndefinedColumn(col_name.clone()))?;
                // A cast of a column keeps the column's name: `id::int8` is
                // `id` on PostgreSQL, not `int8`.
                let out = if rt.name.is_empty() {
                    col_name.clone()
                } else {
                    rt.name.clone()
                };
                columns.push((out, field));
                casts.push(Some(ColumnExpr::Casts {
                    source: def.column(&col_name).map(|c| c.pg_type.clone()),
                    chain,
                }));
                continue;
            }
            // `regexp_replace(statement, 'pat', '', 'i') AS statement` -- a
            // scalar call with the column among constant arguments. The value
            // is computed per row by the executor; the TYPE is fixed here so
            // the describe pass, which sees no rows, still names it.
            Some(N::FuncCall(f))
                if func_name(f).as_deref().is_some_and(|n| {
                    (scalar::is_scalar(n) && scalar::has_static_result_type(n))
                        || n == "regexp_replace"
                }) && single_column_call(f, params).is_some()
                    && !(func_name(f).as_deref() == Some("to_char")
                        && single_column_call(f, params).is_some_and(|(c, _)| {
                            def.column(&c)
                                .is_some_and(|col| datetime::is_datetime(&col.pg_type))
                        })) =>
            {
                let name = func_name(f).expect("checked");
                let (column, args) = single_column_call(f, params).expect("checked");
                let field = def
                    .field_of(&column)
                    .ok_or_else(|| Error::UndefinedColumn(column.clone()))?;
                // `length` of a tsvector counts its lexemes.
                let name = if name == "length"
                    && def.column(&column).is_some_and(|c| c.pg_type == "tsvector")
                {
                    "tsvector_length".to_string()
                } else {
                    name
                };
                let out = if rt.name.is_empty() {
                    name.clone()
                } else {
                    rt.name.clone()
                };
                columns.push((out, field));
                casts.push(Some(ColumnExpr::Call {
                    result_type: scalar::static_result_type(&name).to_string(),
                    name,
                    args,
                }));
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
            // `SELECT 1 FROM t` -- a literal in the select list. The value is
            // the same for every row; `?column?` is PostgreSQL's name for an
            // unaliased constant. The stored "field" is unused (the Const
            // expression ignores the row) but must be a real column name so the
            // encoder's `d.get(f)` is harmless -- the output name serves.
            Some(N::AConst(_)) => {
                let val = rt.val.as_ref().expect("ResTarget has a val for AConst");
                let value = const_value(val, params)?;
                let out = if rt.name.is_empty() {
                    "?column?".to_string()
                } else {
                    rt.name.clone()
                };
                let result_type = const_col_type(&value).to_string();
                columns.push((out.clone(), out));
                casts.push(Some(ColumnExpr::Const { value, result_type }));
            }
            // Anything else is an expression over the row -- `id + 1`,
            // `abs(n)`, `(a, b)`: the column references are rewritten into
            // parameters and the constant evaluator runs it per row.
            Some(_) => {
                let val = rt.val.as_ref().expect("ResTarget has a val");
                let out = if rt.name.is_empty() {
                    expression_column_name(val)
                } else {
                    rt.name.clone()
                };
                let fields: Vec<RowField> = def
                    .columns
                    .iter()
                    .map(|c| (c.name.clone(), c.field(), c.pg_type.clone()))
                    .collect();
                // A sample row typed from the columns' declared types stands
                // in for a real one, so a scalar call is typed by its result.
                let mut sample = Document::new();
                for c in &def.columns {
                    sample.insert(c.field(), sample_value_for_type(&c.pg_type));
                }
                let row = row_column_expr(val, &fields, params, &sample)?;
                let field = def
                    .columns
                    .first()
                    .map(|c| c.field())
                    .unwrap_or_else(|| out.clone());
                columns.push((out, field));
                casts.push(Some(row));
            }
            None => return Err(Error::Unsupported("an empty target".into())),
        }
    }
    Ok((columns, casts))
}

/// A representative value of a PostgreSQL type, for typing an expression
/// over a row before any row exists: the constant evaluator's result over
/// these is what `static_type` reads a scalar call's type from.
/// The one column and the constant arguments of a scalar call shaped
/// `f(col, const, ...)` -- `None` for any other shape (two columns, no
/// column, a nested expression), which the row-expression path handles.
fn single_column_call(
    f: &pg_query::protobuf::FuncCall,
    params: &[Bson],
) -> Option<(String, Vec<Option<Bson>>)> {
    let mut args: Vec<Option<Bson>> = Vec::new();
    let mut column: Option<String> = None;
    for a in &f.args {
        if let Some(N::ColumnRef(c)) = a.node.as_ref() {
            if column.is_some() {
                return None;
            }
            column = Some(column_ref_name(c)?);
            args.push(None);
            continue;
        }
        args.push(Some(const_value(a, params).ok()?));
    }
    Some((column?, args))
}

fn sample_value_for_type(pg_type: &str) -> Bson {
    match pg_type {
        "int2" | "int4" => Bson::Int32(1),
        "int8" => Bson::Int64(1),
        "float4" | "float8" => Bson::Double(1.0),
        // Without a numeric sample, `n * 2` over a numeric column evaluated
        // to NULL and was typed `int4`, so the client's int loader choked on
        // `3.0`.
        "numeric" | "decimal" => Bson::Decimal128("1".parse().expect("literal")),
        "bool" => Bson::Boolean(true),
        "text" | "varchar" | "bpchar" | "name" => Bson::String(String::new()),
        // An ARRAY column samples as a ONE-ELEMENT array of its element's
        // sample. Without this every expression over an array column typed
        // from a NULL sample, so `length(ta[1])` was described as `text` and
        // the executor then rendered the integer it computed as a string.
        other => match other.strip_suffix("[]") {
            Some(element) => Bson::Array(vec![sample_value_for_type(element)]),
            None => Bson::Null,
        },
    }
}

/// `SELECT DISTINCT` / `SELECT DISTINCT ON (...)` from a parsed select.
///
/// PostgreSQL spells a plain `DISTINCT` as a one-element `distinctClause`
/// whose element is a NULL node; `DISTINCT ON (...)` carries the key
/// expressions. Each key resolves to a stored field, the same way an ORDER BY
/// column does -- anything else is refused rather than ignored, because
/// dropping the clause silently returns duplicate rows.
fn plan_distinct(
    s: &pg_query::protobuf::SelectStmt,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Result<Distinct> {
    if s.distinct_clause.is_empty() {
        return Ok(Distinct::None);
    }
    if s.distinct_clause.len() == 1 && s.distinct_clause[0].node.is_none() {
        return Ok(Distinct::All);
    }
    let mut keys = Vec::with_capacity(s.distinct_clause.len());
    for item in &s.distinct_clause {
        let Some(N::ColumnRef(c)) = item.node.as_ref() else {
            return Err(Error::Unsupported("DISTINCT ON over an expression".into()));
        };
        let name =
            column_ref_name(c).ok_or_else(|| Error::Unsupported("this DISTINCT ON key".into()))?;
        let field = resolve(&name).ok_or_else(|| Error::UndefinedColumn(name.clone()))?;
        keys.push(field);
    }
    Ok(Distinct::On(keys))
}

/// `<query> UNION|INTERSECT|EXCEPT [ALL] <query>`.
///
/// Each side is planned on its own; the ORDER BY / LIMIT / OFFSET on the
/// outer statement belong to the combined result, and its sort terms name
/// OUTPUT columns, which are the left side's.
fn plan_set_operation(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<Statement> {
    use pg_query::protobuf::SetOperation;

    let kind = match SetOperation::try_from(s.op) {
        Ok(SetOperation::SetopUnion) => SetOpKind::Union,
        Ok(SetOperation::SetopIntersect) => SetOpKind::Intersect,
        Ok(SetOperation::SetopExcept) => SetOpKind::Except,
        _ => return Err(Error::Unsupported("this set operation".into())),
    };
    let (Some(larg), Some(rarg)) = (s.larg.as_ref(), s.rarg.as_ref()) else {
        return Err(Error::Parse("a set operation without both sides".into()));
    };
    let left = plan_select(larg, lookup, params)?;
    let right = plan_select(rarg, lookup, params)?;

    // The output columns are the left side's, so an ORDER BY term resolves
    // against them -- by position (`ORDER BY 1`) or by output name.
    let names: Vec<String> = match &left {
        Statement::Select(sel) => sel.columns.iter().map(|(out, _)| out.clone()).collect(),
        Statement::SelectConstant(sc) => sc.columns.iter().map(|(n, ..)| n.clone()).collect(),
        Statement::ValuesConstant(vc) => vc.names.clone(),
        Statement::SetOp(inner) => inner.output_names(),
        _ => Vec::new(),
    };
    let mut order = Vec::new();
    for item in &s.sort_clause {
        let Some(N::SortBy(sb)) = item.node.as_ref() else {
            return Err(Error::Unsupported("this ORDER BY item".into()));
        };
        let index = match sb.node.as_ref().and_then(|n| n.node.as_ref()) {
            Some(N::AConst(c)) => {
                let Some(a_const::Val::Ival(v)) = c.val.as_ref() else {
                    return Err(Error::Unsupported("ORDER BY over an expression".into()));
                };
                usize::try_from(v.ival)
                    .ok()
                    .filter(|n| *n >= 1 && *n <= names.len().max(1))
                    .map(|n| n - 1)
                    .ok_or_else(|| {
                        Error::InvalidColumnReference(format!(
                            "ORDER BY position {} is not in select list",
                            v.ival
                        ))
                    })?
            }
            Some(N::ColumnRef(c)) => {
                let col = column_ref_name(c)
                    .ok_or_else(|| Error::Unsupported("this ORDER BY expression".into()))?;
                names
                    .iter()
                    .position(|n| *n == col)
                    .ok_or_else(|| Error::UndefinedColumn(col.clone()))?
            }
            _ => return Err(Error::Unsupported("ORDER BY over an expression".into())),
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
        order.push(SetOpOrder {
            index,
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

    Ok(Statement::SetOp(SetOpSelect {
        left: Box::new(left),
        right: Box::new(right),
        kind,
        all: s.all,
        order,
        limit,
        offset,
    }))
}

/// `SELECT DISTINCT` over an aggregate: true for a plain DISTINCT.
///
/// `DISTINCT ON (...)` needs its keys to resolve against the GROUP BY output
/// rather than the table, which this slice does not do -- so it is refused as
/// unsupported. Resolving it against the table instead reported the key as an
/// undefined column, which is a different and misleading answer.
/// Plan a `HAVING` predicate against a query's groups and aggregates.
///
/// An aggregate written in HAVING need not be in the SELECT list, so one that
/// is missing is APPENDED to `items` -- it is computed for the test and never
/// projected. A matching item is reused rather than computed twice.
///
/// The accepted shape is a comparison or NULL test on a grouped value against
/// a constant, combined with AND / OR / NOT. Anything else is refused: HAVING
/// decides which rows come back, so a half-understood predicate would answer
/// wrongly rather than slowly.
fn plan_having(
    node: &pg_query::protobuf::Node,
    def: &TableDef,
    group_by: &[GroupKey],
    items: &mut Vec<AggItem>,
    params: &[Bson],
) -> Result<Having> {
    match node.node.as_ref() {
        Some(N::BoolExpr(b)) => {
            let parts: Result<Vec<Having>> = b
                .args
                .iter()
                .map(|a| plan_having(a, def, group_by, items, params))
                .collect();
            let parts = parts?;
            match BoolExprType::try_from(b.boolop) {
                Ok(BoolExprType::AndExpr) => Ok(Having::And(parts)),
                Ok(BoolExprType::OrExpr) => Ok(Having::Or(parts)),
                Ok(BoolExprType::NotExpr) => {
                    let inner = parts
                        .into_iter()
                        .next()
                        .ok_or_else(|| Error::Parse("NOT without an operand".into()))?;
                    Ok(Having::Not(Box::new(inner)))
                }
                _ => Err(Error::Unsupported("this HAVING connective".into())),
            }
        }
        Some(N::NullTest(t)) => {
            let subject = having_subject(t.arg.as_deref(), def, group_by, items, params)?;
            let negated = matches!(
                NullTestType::try_from(t.nulltesttype),
                Ok(NullTestType::IsNotNull)
            );
            Ok(Having::IsNull { subject, negated })
        }
        Some(N::AExpr(e)) => {
            let op = operator_name(e)?;
            if !matches!(op, "=" | "<>" | "!=" | ">" | ">=" | "<" | "<=") {
                return Err(Error::Unsupported(format!("HAVING operator {op}")));
            }
            // The constant may be written on either side; a comparison with a
            // constant on the LEFT flips, so `100 < count(*)` keeps meaning
            // what it says.
            let subject_on_left =
                having_subject(e.lexpr.as_deref(), def, group_by, items, params).is_ok();
            let (subject_node, value_node) = if subject_on_left {
                (e.lexpr.as_deref(), e.rexpr.as_deref())
            } else {
                (e.rexpr.as_deref(), e.lexpr.as_deref())
            };
            let op = if subject_on_left {
                op.to_string()
            } else {
                match op {
                    ">" => "<".to_string(),
                    ">=" => "<=".to_string(),
                    "<" => ">".to_string(),
                    "<=" => ">=".to_string(),
                    other => other.to_string(),
                }
            };
            let subject = having_subject(subject_node, def, group_by, items, params)?;
            let value = const_value(
                value_node.ok_or_else(|| Error::Parse("HAVING without an operand".into()))?,
                params,
            )?;
            Ok(Having::Compare {
                subject,
                op: if op == "!=" { "<>".to_string() } else { op },
                value,
            })
        }
        Some(other) => Err(Error::Unsupported(format!("{} in HAVING", disc(other)))),
        None => Err(Error::Parse("an empty HAVING".into())),
    }
}

/// The grouped value a HAVING term tests: an aggregate (computed for the test
/// if the SELECT list does not already ask for it) or a GROUP BY key.
fn having_subject(
    node: Option<&pg_query::protobuf::Node>,
    def: &TableDef,
    group_by: &[GroupKey],
    items: &mut Vec<AggItem>,
    _params: &[Bson],
) -> Result<OutputCol> {
    let node = node.ok_or_else(|| Error::Parse("a HAVING term without an operand".into()))?;
    match node.node.as_ref() {
        Some(N::FuncCall(f)) if is_aggregate_call(f) => {
            let item = plan_aggregate_item(
                f,
                def,
                items.len(),
                _params,
                format!("__having{}", items.len()),
            )?;
            if let Some(i) = items
                .iter()
                .position(|existing| same_aggregate(existing, &item))
            {
                return Ok(OutputCol::Agg(i));
            }
            items.push(item);
            Ok(OutputCol::Agg(items.len() - 1))
        }
        Some(N::ColumnRef(c)) => {
            let name =
                column_ref_name(c).ok_or_else(|| Error::Unsupported("this HAVING term".into()))?;
            group_by
                .iter()
                .position(|k| k.expr.is_none() && k.name == name)
                .map(OutputCol::Group)
                .ok_or_else(|| {
                    Error::Grouping(format!(
                        "column \"{name}\" must appear in the GROUP BY clause \
                         or be used in an aggregate function"
                    ))
                })
        }
        _ => Err(Error::Unsupported("this HAVING term".into())),
    }
}

/// Do two items compute the same thing (everything but the output name)?
fn same_aggregate(a: &AggItem, b: &AggItem) -> bool {
    let mut x = a.clone();
    x.out.clone_from(&b.out);
    x == *b
}

/// The aggregate a function name calls, with how many ordinary arguments it
/// takes. `None`: not an aggregate this server computes.
fn aggregate_func(name: &str, within_group: bool) -> Option<(AggFunc, usize)> {
    if within_group {
        return Some(match name {
            "percentile_cont" => (AggFunc::PercentileCont, 1),
            "percentile_disc" => (AggFunc::PercentileDisc, 1),
            "mode" => (AggFunc::Mode, 0),
            "rank" => (AggFunc::HypRank, 1),
            "dense_rank" => (AggFunc::HypDenseRank, 1),
            "percent_rank" => (AggFunc::HypPercentRank, 1),
            "cume_dist" => (AggFunc::HypCumeDist, 1),
            _ => return None,
        });
    }
    Some(match name {
        "count" => (AggFunc::Count, 1),
        "sum" => (AggFunc::Sum, 1),
        "min" => (AggFunc::Min, 1),
        "max" => (AggFunc::Max, 1),
        "array_agg" => (AggFunc::ArrayAgg, 1),
        "bool_and" | "every" => (AggFunc::BoolAnd, 1),
        "bool_or" => (AggFunc::BoolOr, 1),
        "avg" => (AggFunc::Avg, 1),
        "string_agg" => (AggFunc::StringAgg, 2),
        "variance" | "var_samp" => (AggFunc::VarSamp, 1),
        "var_pop" => (AggFunc::VarPop, 1),
        "stddev" | "stddev_samp" => (AggFunc::StddevSamp, 1),
        "stddev_pop" => (AggFunc::StddevPop, 1),
        "json_agg" => (AggFunc::JsonAgg, 1),
        "jsonb_agg" => (AggFunc::JsonbAgg, 1),
        "json_object_agg" => (AggFunc::JsonObjectAgg, 2),
        "jsonb_object_agg" => (AggFunc::JsonbObjectAgg, 2),
        "corr" => (AggFunc::Corr, 2),
        "covar_pop" => (AggFunc::CovarPop, 2),
        "covar_samp" => (AggFunc::CovarSamp, 2),
        "regr_count" => (AggFunc::RegrCount, 2),
        "regr_avgx" => (AggFunc::RegrAvgX, 2),
        "regr_avgy" => (AggFunc::RegrAvgY, 2),
        "regr_sxx" => (AggFunc::RegrSxx, 2),
        "regr_syy" => (AggFunc::RegrSyy, 2),
        "regr_sxy" => (AggFunc::RegrSxy, 2),
        "regr_slope" => (AggFunc::RegrSlope, 2),
        "regr_intercept" => (AggFunc::RegrIntercept, 2),
        "regr_r2" => (AggFunc::RegrR2, 2),
        "bit_and" => (AggFunc::BitAnd, 1),
        "bit_or" => (AggFunc::BitOr, 1),
        _ => return None,
    })
}

/// One aggregate call as an `AggItem`: its arguments resolved to stored
/// fields (a bare column), or to hidden per-row slots (`__agg{index}`,
/// `__agg{index}_2`) filled from an expression. `WITHIN GROUP` aggregates
/// take their argument from the ORDER BY and their direct arguments as
/// constants. One builder for the select list, HAVING and aggregates inside
/// expressions, so every aggregate means the same thing in each.
fn plan_aggregate_item(
    f: &pg_query::protobuf::FuncCall,
    def: &TableDef,
    index: usize,
    params: &[Bson],
    out: String,
) -> Result<AggItem> {
    let name = func_name(f).unwrap_or_default();
    let filter = match f.agg_filter.as_deref() {
        None => None,
        Some(node) => Some(lower_where(node, def, params)?),
    };
    if name == "count" && f.agg_star {
        return Ok(AggItem {
            func: AggFunc::CountStar,
            out,
            distinct: f.agg_distinct,
            filter,
            order: plan_aggregate_order(&f.agg_order, def)?,
            source_typmod: -1,
            ..Default::default()
        });
    }
    let (func, nargs) = aggregate_func(&name, f.agg_within_group)
        .ok_or_else(|| Error::Unsupported(format!("aggregate {name}()")))?;
    let fields: Vec<RowField> = def
        .columns
        .iter()
        .map(|c| (c.name.clone(), c.field(), c.pg_type.clone()))
        .collect();
    let mut sample = Document::new();
    for c in &def.columns {
        sample.insert(c.field(), sample_value_for_type(&c.pg_type));
    }
    // (field, type, typmod, expression) for one argument node.
    let resolve = |node: &pg_query::protobuf::Node,
                   slot: String|
     -> Result<(String, Option<String>, i32, Option<ColumnExpr>)> {
        if let Some(N::ColumnRef(c)) = node.node.as_ref() {
            if let Some(col) = column_ref_name(c) {
                let column = def
                    .column(&col)
                    .ok_or_else(|| Error::UndefinedColumn(col.clone()))?;
                return Ok((
                    column.field(),
                    Some(column.pg_type.clone()),
                    column.typmod,
                    None,
                ));
            }
        }
        let expr = row_column_expr(node, &fields, params, &sample)?;
        let ty = match &expr {
            ColumnExpr::Row { result_type, .. } => result_type.clone(),
            _ => "text".to_string(),
        };
        Ok((slot, Some(ty), -1, Some(expr)))
    };
    let mut item = AggItem {
        func,
        out,
        distinct: f.agg_distinct,
        filter,
        source_typmod: -1,
        ..Default::default()
    };
    if f.agg_within_group {
        // `percentile_cont(0.5) WITHIN GROUP (ORDER BY v)`: the ordered
        // argument is `v`, sorted as written; the fraction / hypothetical
        // row is a direct argument.
        let [sort] = f.agg_order.as_slice() else {
            return Err(Error::Unsupported(format!(
                "{name}() WITHIN GROUP over more than one column"
            )));
        };
        let Some(N::SortBy(sb)) = sort.node.as_ref() else {
            return Err(Error::Unsupported("this WITHIN GROUP clause".into()));
        };
        let node = sb
            .node
            .as_deref()
            .ok_or_else(|| Error::Parse("an empty WITHIN GROUP".into()))?;
        let (field, ty, typmod, expr) = resolve(node, format!("__agg{index}"))?;
        let ascending = !matches!(
            SortByDir::try_from(sb.sortby_dir),
            Ok(SortByDir::SortbyDesc)
        );
        let nulls = match SortByNulls::try_from(sb.sortby_nulls) {
            Ok(SortByNulls::SortbyNullsFirst) => Nulls::First,
            Ok(SortByNulls::SortbyNullsLast) => Nulls::Last,
            _ if ascending => Nulls::Last,
            _ => Nulls::First,
        };
        if f.args.len() != nargs {
            return Err(Error::UndefinedFunction(format!(
                "function {name}({}) does not exist",
                vec!["unknown"; f.args.len()].join(", ")
            )));
        }
        item.direct = f
            .args
            .iter()
            .map(|a| const_value(a, params))
            .collect::<Result<_>>()?;
        item.order = vec![OrderKey {
            field: field.clone(),
            ascending,
            nulls,
            expr: None,
        }];
        item.field = Some(field);
        item.source_type = ty;
        item.source_typmod = typmod;
        item.expr = expr;
        return Ok(item);
    }
    if f.args.len() != nargs {
        if func == AggFunc::StringAgg {
            return Err(Error::Unsupported(
                "string_agg takes a value and a separator".into(),
            ));
        }
        return Err(Error::Unsupported(
            "an aggregate with this many arguments".into(),
        ));
    }
    item.order = plan_aggregate_order(&f.agg_order, def)?;
    let (field, ty, typmod, expr) = resolve(&f.args[0], format!("__agg{index}"))?;
    item.field = Some(field);
    item.source_type = ty;
    item.source_typmod = typmod;
    item.expr = expr;
    if func == AggFunc::StringAgg {
        item.sep = Some(const_value(&f.args[1], params)?);
    } else if nargs == 2 {
        let (field2, ty2, _, expr2) = resolve(&f.args[1], format!("__agg{index}_2"))?;
        item.field2 = Some(field2);
        item.source_type2 = ty2;
        item.expr2 = expr2;
    }
    if matches!(func, AggFunc::BitAnd | AggFunc::BitOr)
        && !matches!(
            item.source_type.as_deref(),
            Some("int2" | "int4" | "int8" | "smallint" | "integer" | "bigint" | "bit" | "varbit")
        )
    {
        return Err(Error::UndefinedFunction(format!(
            "function {name}({}) does not exist",
            display_type(item.source_type.as_deref().unwrap_or("unknown"))
        )));
    }
    if func == AggFunc::StringAgg
        && !matches!(
            item.source_type.as_deref(),
            Some("text" | "varchar" | "bpchar" | "name" | "char")
        )
        && item.expr.is_none()
    {
        // PostgreSQL has no `string_agg(integer, ...)`: it is a missing
        // FUNCTION, not an unsupported one.
        return Err(Error::UndefinedFunction(format!(
            "function string_agg({}, unknown) does not exist",
            item.source_type.as_deref().unwrap_or("unknown")
        )));
    }
    Ok(item)
}

/// Replace every aggregate call inside `node` with a reference to a slot
/// holding that aggregate's value, registering the aggregate as an item.
///
/// This is what lets `count(*) + 1` be planned at all: the aggregates are
/// computed per group as usual, and the arithmetic runs afterwards over their
/// results. An identical aggregate already in `items` is reused rather than
/// computed twice.
fn extract_aggregates(
    node: &mut pg_query::protobuf::Node,
    def: &TableDef,
    items: &mut Vec<AggItem>,
    slots: &mut Vec<RowField>,
    params: &[Bson],
) -> Result<bool> {
    let mut found = false;
    if let Some(N::FuncCall(f)) = node.node.as_ref() {
        if is_aggregate_call(f) {
            let f = f.clone();
            let item = plan_aggregate_item(
                &f,
                def,
                items.len(),
                params,
                format!("__having{}", items.len()),
            )?;
            let index = match items
                .iter()
                .position(|existing| same_aggregate(existing, &item))
            {
                Some(i) => i,
                None => {
                    items.push(item);
                    items.len() - 1
                }
            };
            // The slot is named for the item's position, and is BOTH the
            // column name and the stored field: the row the executor builds
            // for the expression keys the value by exactly this.
            let slot = format!("__aggval{index}");
            let pg_type = aggregate_item_type(&items[index]);
            if !slots.iter().any(|(name, _, _)| *name == slot) {
                slots.push((slot.clone(), slot.clone(), pg_type));
            }
            *node = column_ref_node(&slot);
            return Ok(true);
        }
    }
    let Some(inner) = node.node.as_mut() else {
        return Ok(false);
    };
    match inner {
        N::AExpr(e) => {
            if let Some(l) = e.lexpr.as_mut() {
                found |= extract_aggregates(l, def, items, slots, params)?;
            }
            if let Some(r) = e.rexpr.as_mut() {
                found |= extract_aggregates(r, def, items, slots, params)?;
            }
        }
        N::FuncCall(f) => {
            for a in f.args.iter_mut() {
                found |= extract_aggregates(a, def, items, slots, params)?;
            }
        }
        N::TypeCast(tc) => {
            if let Some(a) = tc.arg.as_mut() {
                found |= extract_aggregates(a, def, items, slots, params)?;
            }
        }
        N::CoalesceExpr(c) => {
            for a in c.args.iter_mut() {
                found |= extract_aggregates(a, def, items, slots, params)?;
            }
        }
        N::BoolExpr(b) => {
            for a in b.args.iter_mut() {
                found |= extract_aggregates(a, def, items, slots, params)?;
            }
        }
        _ => {}
    }
    Ok(found)
}

/// A `ColumnRef` node naming one column, for the rewriting above.
fn column_ref_node(name: &str) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
            fields: vec![pg_query::protobuf::Node {
                node: Some(N::String(pg_query::protobuf::String {
                    sval: name.to_string(),
                })),
            }],
            location: 0,
        })),
    }
}

/// The PostgreSQL type one aggregate item answers.
pub fn aggregate_item_type(item: &AggItem) -> String {
    match item.func {
        AggFunc::CountStar | AggFunc::Count => "int8".to_string(),
        AggFunc::Sum => sum_result_type(item.source_type.as_deref()).to_string(),
        AggFunc::Avg => avg_result_type(item.source_type.as_deref()).to_string(),
        AggFunc::Min | AggFunc::Max => item
            .source_type
            .clone()
            .unwrap_or_else(|| "text".to_string()),
        AggFunc::ArrayAgg => format!("{}[]", item.source_type.as_deref().unwrap_or("text")),
        AggFunc::BoolAnd | AggFunc::BoolOr => "bool".to_string(),
        AggFunc::StringAgg => "text".to_string(),
        AggFunc::VarSamp | AggFunc::VarPop | AggFunc::StddevSamp | AggFunc::StddevPop => {
            match item.source_type.as_deref() {
                Some("float4" | "float8" | "real" | "double precision") => "float8".to_string(),
                _ => "numeric".to_string(),
            }
        }
        AggFunc::JsonAgg | AggFunc::JsonObjectAgg => "json".to_string(),
        AggFunc::JsonbAgg | AggFunc::JsonbObjectAgg => "jsonb".to_string(),
        AggFunc::RegrCount | AggFunc::HypRank | AggFunc::HypDenseRank => "int8".to_string(),
        AggFunc::Corr
        | AggFunc::CovarPop
        | AggFunc::CovarSamp
        | AggFunc::RegrAvgX
        | AggFunc::RegrAvgY
        | AggFunc::RegrSxx
        | AggFunc::RegrSyy
        | AggFunc::RegrSxy
        | AggFunc::RegrSlope
        | AggFunc::RegrIntercept
        | AggFunc::RegrR2
        | AggFunc::HypPercentRank
        | AggFunc::HypCumeDist => "float8".to_string(),
        AggFunc::PercentileCont => {
            let base = if item.source_type.as_deref() == Some("interval") {
                "interval"
            } else {
                "float8"
            };
            if matches!(item.direct.first(), Some(Bson::Array(_))) {
                format!("{base}[]")
            } else {
                base.to_string()
            }
        }
        AggFunc::PercentileDisc | AggFunc::Mode | AggFunc::BitAnd | AggFunc::BitOr => {
            let base = item
                .source_type
                .clone()
                .unwrap_or_else(|| "text".to_string());
            if item.func == AggFunc::PercentileDisc
                && matches!(item.direct.first(), Some(Bson::Array(_)))
            {
                format!("{base}[]")
            } else {
                base
            }
        }
    }
}

/// A plausible value of `pg_type`, so an expression over the grouped values
/// can be TYPED without running the query.
fn sample_for_type(pg_type: &str) -> Bson {
    match pg_type {
        "int2" | "int4" => Bson::Int32(1),
        "int8" => Bson::Int64(1),
        "float4" | "float8" => Bson::Double(1.0),
        "numeric" | "decimal" => Bson::String("1".into()),
        "bool" => Bson::Boolean(true),
        t if t.ends_with("[]") => Bson::Array(vec![]),
        _ => Bson::String("x".into()),
    }
}

/// `ORDER BY` written INSIDE an aggregate call, over the table's columns.
///
/// PostgreSQL sorts the group's rows by these before collecting the values,
/// which is the only thing that makes `string_agg` and `array_agg` answer in
/// a defined order. The keys are ordinary columns here; an expression is
/// refused rather than ignored, because ignoring it answers in a DIFFERENT
/// order and says nothing.
/// Expand a `GROUP BY` element list into the grouping SETS it denotes.
///
/// `None` when no element is a grouping construct, which keeps a plain
/// `GROUP BY` on exactly the path it was on before.
///
/// Semantics measured against PostgreSQL 14.13 rather than recalled:
///
/// * `ROLLUP (a, b)` is `(a,b), (a), ()` — prefixes, longest first.
/// * `CUBE (a, b)` is every subset, `(a,b), (a), (b), ()`.
/// * Several constructs in one `GROUP BY` multiply: their sets are crossed.
/// * A duplicate set is KEPT, and emits duplicate rows
///   (`grouping sets ((a),(a))` returns each group twice).
fn expand_grouping_sets(elements: &[GroupElement]) -> Option<Vec<Vec<usize>>> {
    if elements.iter().all(|e| matches!(e, GroupElement::Key(_))) {
        return None;
    }
    // Start with one empty set and cross in each element's alternatives.
    let mut sets: Vec<Vec<usize>> = vec![Vec::new()];
    for element in elements {
        let alternatives: Vec<Vec<usize>> = match element {
            GroupElement::Key(i) => vec![vec![*i]],
            GroupElement::Sets(inner) => inner.clone(),
            GroupElement::Rollup(keys) => {
                (0..=keys.len()).rev().map(|n| keys[..n].to_vec()).collect()
            }
            GroupElement::Cube(keys) => {
                // Subsets in PostgreSQL's order: the full set first, then
                // successively fewer, which for two keys is (a,b),(a),(b),().
                let mut out: Vec<Vec<usize>> = Vec::new();
                for mask in (0..(1u32 << keys.len())).rev() {
                    out.push(
                        keys.iter()
                            .enumerate()
                            .filter(|(bit, _)| mask & (1 << bit) != 0)
                            .map(|(_, k)| *k)
                            .collect(),
                    );
                }
                out
            }
        };
        let mut crossed = Vec::with_capacity(sets.len() * alternatives.len());
        for base in &sets {
            for alt in &alternatives {
                let mut joined = base.clone();
                joined.extend(alt.iter().copied());
                crossed.push(joined);
            }
        }
        sets = crossed;
    }
    Some(sets)
}

/// One element of a `GROUP BY` list, before expansion.
#[derive(Debug, Clone)]
enum GroupElement {
    /// A plain key, by index into the collected `group_by`.
    Key(usize),
    /// `GROUPING SETS (...)` — each inner set already resolved to indices.
    Sets(Vec<Vec<usize>>),
    Rollup(Vec<usize>),
    Cube(Vec<usize>),
}

/// Resolve one `GROUP BY` element to its key.
///
/// Split out of `plan_aggregate`'s loop so a `GROUPING SETS` / `ROLLUP` /
/// `CUBE` member resolves by the same rules as a top-level key.
fn resolve_group_key(
    node: &pg_query::protobuf::Node,
    def: &TableDef,
    fields: &[RowField],
    params: &[Bson],
    sample: &Document,
    s: &pg_query::protobuf::SelectStmt,
) -> Result<(GroupKey, String)> {
    // A name that is no column of the source but IS an output alias names
    // that target (`select length(data) as n ... group by n`); an input
    // column wins the tie, as PostgreSQL resolves it.
    let node = match node.node.as_ref() {
        Some(N::ColumnRef(c)) => match column_ref_name(c) {
            Some(name) if def.column(&name).is_none() => s
                .target_list
                .iter()
                .find_map(|t| match t.node.as_ref() {
                    Some(N::ResTarget(rt)) if rt.name == name => rt.val.as_deref(),
                    _ => None,
                })
                .unwrap_or(node),
            _ => node,
        },
        _ => node,
    };
    // The print is taken from the RESOLVED node, because that is the form the
    // select list looks a group key up by. Taking it from the unresolved node
    // made `GROUP BY length` (an alias for `length(name)`) store the print
    // `length`, so the target `length(name)` found no key and the statement
    // failed with `this target is not supported yet`.
    let print = node_print(node);
    match node.node.as_ref() {
        Some(N::ColumnRef(c)) => {
            let name = column_ref_name(c)
                .ok_or_else(|| Error::Unsupported("this GROUP BY expression".into()))?;
            let column = def
                .column(&name)
                .ok_or_else(|| Error::UndefinedColumn(name.clone()))?;
            Ok((
                GroupKey {
                    name,
                    field: column.field(),
                    expr: None,
                    pg_type: column.pg_type.clone(),
                },
                print,
            ))
        }
        Some(N::FuncCall(f)) if is_aggregate_call(f) => Err(Error::Grouping(
            "aggregate functions are not allowed in GROUP BY".into(),
        )),
        Some(_) => {
            let expr = row_column_expr(node, fields, params, sample)?;
            let pg_type = match &expr {
                ColumnExpr::Row { result_type, .. } => result_type.clone(),
                _ => "text".to_string(),
            };
            Ok((
                GroupKey {
                    name: expression_column_name(node),
                    field: String::new(),
                    expr: Some(expr),
                    pg_type,
                },
                print,
            ))
        }
        None => Err(Error::Unsupported("an empty GROUP BY key".into())),
    }
}

fn plan_aggregate_order(
    nodes: &[pg_query::protobuf::Node],
    def: &TableDef,
) -> Result<Vec<OrderKey>> {
    let mut keys = Vec::with_capacity(nodes.len());
    for item in nodes {
        let Some(N::SortBy(sb)) = item.node.as_ref() else {
            return Err(Error::Unsupported(
                "this ORDER BY inside an aggregate".into(),
            ));
        };
        let Some(N::ColumnRef(c)) = sb.node.as_ref().and_then(|n| n.node.as_ref()) else {
            return Err(Error::Unsupported(
                "ORDER BY over an expression inside an aggregate".into(),
            ));
        };
        let name = column_ref_name(c)
            .ok_or_else(|| Error::Unsupported("this ORDER BY inside an aggregate".into()))?;
        let field = def
            .field_of(&name)
            .ok_or_else(|| Error::UndefinedColumn(name.clone()))?;
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
        keys.push(OrderKey {
            field,
            ascending,
            nulls,
            expr: None,
        });
    }
    Ok(keys)
}

fn aggregate_distinct(s: &pg_query::protobuf::SelectStmt) -> Result<bool> {
    if s.distinct_clause.is_empty() {
        return Ok(false);
    }
    if s.distinct_clause.len() == 1 && s.distinct_clause[0].node.is_none() {
        return Ok(true);
    }
    Err(Error::Unsupported("DISTINCT ON with an aggregate".into()))
}

/// Runs a planned subquery and returns its rows, each row a vector of column
/// values in select-list order. Supplied by the executor: the planner cannot
/// read storage itself, and an UNCORRELATED subquery has to be RUN before the
/// query around it can be lowered.
pub type SubqueryRunner<'a> = &'a dyn Fn(&Statement) -> Result<Vec<Vec<Bson>>>;

/// Replace every uncorrelated subquery in a statement with the values it
/// returns, so the rest of the planner never sees a `SubLink`.
///
/// This is what makes `(SELECT ...)`, `EXISTS (...)`, `x IN (SELECT ...)` and
/// `x op ANY/ALL (SELECT ...)` work without a new plan node or a new
/// executor path: once the subquery is a literal, the lowering that already
/// handles `x IN (1, 2, 3)` and `WHERE true` handles it too. It is also what
/// PostgreSQL does semantically -- an uncorrelated subquery is evaluated once
/// per statement, not once per row.
///
/// A CORRELATED subquery is refused by name rather than resolved: its value
/// depends on the outer row, so there is no single set of values to
/// substitute, and guessing one would be a wrong answer rather than a missing
/// feature.
fn resolve_sublinks(
    node: &mut pg_query::protobuf::Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &mut Vec<Bson>,
    run: SubqueryRunner<'_>,
) -> Result<()> {
    match node.node.as_mut() {
        Some(N::SelectStmt(s)) => resolve_sublinks_in_select(s, lookup, params, run),
        Some(N::UpdateStmt(u)) => {
            let outer = outer_columns(&u.relation, lookup);
            let mut clauses: Vec<&mut pg_query::protobuf::Node> = Vec::new();
            if let Some(w) = u.where_clause.as_deref_mut() {
                clauses.push(w);
            }
            // A SET value may be a subquery too: `SET n = (SELECT ...)`.
            for t in &mut u.target_list {
                if let Some(N::ResTarget(rt)) = t.node.as_mut() {
                    if let Some(v) = rt.val.as_deref_mut() {
                        clauses.push(v);
                    }
                }
            }
            for c in clauses {
                resolve_sublinks_in_expr(c, lookup, params, run, &outer)?;
            }
            Ok(())
        }
        Some(N::DeleteStmt(d)) => {
            let outer = outer_columns(&d.relation, lookup);
            match d.where_clause.as_deref_mut() {
                Some(w) => resolve_sublinks_in_expr(w, lookup, params, run, &outer),
                None => Ok(()),
            }
        }
        _ => Ok(()),
    }
}

/// The column names a single-table statement's own relation exposes, for
/// telling a CORRELATED reference from a typo. Empty when the table is
/// unknown, which makes the check fall back to reporting the original
/// `42703`.
fn outer_columns(
    relation: &Option<pg_query::protobuf::RangeVar>,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Vec<String> {
    relation
        .as_ref()
        .and_then(|r| lookup(&r.relname))
        .map(|d| d.columns.iter().map(|c| c.name.clone()).collect())
        .unwrap_or_default()
}

thread_local! {
    /// The CTEs visible to a subquery being resolved: every enclosing
    /// statement's `WITH` items, innermost last. A subquery is planned on its
    /// own, before the `WITH` around it is inlined, so without this
    /// `with c as (...) select ... where x in (select ... from c)` could not
    /// see `c` at all.
    static VISIBLE_CTES: std::cell::RefCell<Vec<pg_query::protobuf::Node>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn resolve_sublinks_in_select(
    s: &mut pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &mut Vec<Bson>,
    run: SubqueryRunner<'_>,
) -> Result<()> {
    // The scoped body pushes this statement's CTEs once their own bodies are
    // resolved; whatever it pushed is popped here, on every exit.
    let depth = VISIBLE_CTES.with(|v| v.borrow().len());
    let out = resolve_sublinks_in_select_scoped(s, lookup, params, run);
    VISIBLE_CTES.with(|v| v.borrow_mut().truncate(depth));
    out
}

fn resolve_sublinks_in_select_scoped(
    s: &mut pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &mut Vec<Bson>,
    run: SubqueryRunner<'_>,
) -> Result<()> {
    // Every column the query's own FROM exposes. A reference inside a
    // subquery to one of THESE is a correlation; a reference to anything else
    // is a typo, and must keep answering 42703 rather than being reported as
    // an unsupported correlation.
    let mut outer: Vec<String> = Vec::new();
    for item in &s.from_clause {
        collect_from_columns(item, lookup, &mut outer);
    }

    // A CTE body is a select in its own right, and is resolved BEFORE the
    // query that references it -- the inlining that expands `WITH` runs later,
    // inside `plan_select`, so a subquery left in a CTE body here would reach
    // the lowering as an unresolved `SubLink` and be refused for the wrong
    // reason.
    if let Some(with) = s.with_clause.as_mut() {
        for cte in &mut with.ctes {
            if let Some(N::CommonTableExpr(c)) = cte.node.as_mut() {
                if let Some(N::SelectStmt(body)) =
                    c.ctequery.as_deref_mut().and_then(|q| q.node.as_mut())
                {
                    resolve_sublinks_in_select(body, lookup, params, run)?;
                }
            }
        }
        // Visible to the subqueries in the rest of this statement -- pushed
        // now, with their bodies RESOLVED, so a subquery that copies them in
        // never resolves them a second time.
        if !with.recursive {
            let ctes = with.ctes.clone();
            VISIBLE_CTES.with(|v| v.borrow_mut().extend(ctes));
        }
    }
    // A set operation's sides, and a FROM-subquery's body, are selects in
    // their own right.
    for side in [s.larg.as_deref_mut(), s.rarg.as_deref_mut()]
        .into_iter()
        .flatten()
    {
        resolve_sublinks_in_select(side, lookup, params, run)?;
    }
    for item in &mut s.from_clause {
        resolve_sublinks_in_from(item, lookup, params, run)?;
    }

    for t in &mut s.target_list {
        if let Some(N::ResTarget(rt)) = t.node.as_mut() {
            if let Some(v) = rt.val.as_deref_mut() {
                resolve_sublinks_in_expr(v, lookup, params, run, &outer)?;
            }
        }
    }
    for clause in [
        s.where_clause.as_deref_mut(),
        s.having_clause.as_deref_mut(),
    ]
    .into_iter()
    .flatten()
    {
        resolve_sublinks_in_expr(clause, lookup, params, run, &outer)?;
    }
    for item in &mut s.sort_clause {
        if let Some(N::SortBy(sb)) = item.node.as_mut() {
            if let Some(n) = sb.node.as_deref_mut() {
                resolve_sublinks_in_expr(n, lookup, params, run, &outer)?;
            }
        }
    }
    Ok(())
}

fn resolve_sublinks_in_from(
    item: &mut pg_query::protobuf::Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &mut Vec<Bson>,
    run: SubqueryRunner<'_>,
) -> Result<()> {
    match item.node.as_mut() {
        Some(N::RangeSubselect(rs)) => {
            match rs.subquery.as_deref_mut().and_then(|q| q.node.as_mut()) {
                Some(N::SelectStmt(inner)) => {
                    resolve_sublinks_in_select(inner, lookup, params, run)
                }
                _ => Ok(()),
            }
        }
        Some(N::JoinExpr(j)) => {
            for side in [j.larg.as_deref_mut(), j.rarg.as_deref_mut()]
                .into_iter()
                .flatten()
            {
                resolve_sublinks_in_from(side, lookup, params, run)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The columns a FROM item exposes, by name. A subquery's are its select
/// list's output names, which is enough to recognise a correlation.
fn collect_from_columns(
    item: &pg_query::protobuf::Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    out: &mut Vec<String>,
) {
    match item.node.as_ref() {
        Some(N::RangeVar(r)) => {
            if let Some(def) = lookup(&r.relname) {
                out.extend(def.columns.iter().map(|c| c.name.clone()));
            }
        }
        Some(N::JoinExpr(j)) => {
            for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
                collect_from_columns(side, lookup, out);
            }
        }
        Some(N::RangeSubselect(rs)) => {
            if let Some(N::SelectStmt(inner)) = rs.subquery.as_ref().and_then(|q| q.node.as_ref()) {
                for t in &inner.target_list {
                    if let Some(N::ResTarget(rt)) = t.node.as_ref() {
                        if !rt.name.is_empty() {
                            out.push(rt.name.clone());
                        } else if let Some(N::ColumnRef(c)) =
                            rt.val.as_ref().and_then(|v| v.node.as_ref())
                        {
                            if let Some(n) = column_ref_name(c) {
                                out.push(n);
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// Resolve the subqueries inside one expression.
fn resolve_sublinks_in_expr(
    node: &mut pg_query::protobuf::Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &mut Vec<Bson>,
    run: SubqueryRunner<'_>,
    outer: &[String],
) -> Result<()> {
    if let Some(N::SubLink(_)) = node.node.as_ref() {
        let N::SubLink(sl) = node.node.clone().expect("matched above") else {
            unreachable!("matched SubLink")
        };
        let replacement = resolve_one_sublink(&sl, lookup, params, run, outer)?;
        *node = replacement;
        return Ok(());
    }
    // Not a SubLink itself: descend. `walk_expr` visits outermost-first and
    // does not enter a SubLink body, so recursing by hand here keeps the
    // replacement above from being re-walked.
    let Some(inner) = node.node.as_mut() else {
        return Ok(());
    };
    let mut children: Vec<&mut pg_query::protobuf::Node> = Vec::new();
    match inner {
        N::TypeCast(tc) => children.extend(tc.arg.as_deref_mut()),
        N::AExpr(e) => {
            children.extend(e.lexpr.as_deref_mut());
            children.extend(e.rexpr.as_deref_mut());
        }
        N::FuncCall(f) => children.extend(f.args.iter_mut()),
        N::BoolExpr(b) => children.extend(b.args.iter_mut()),
        N::AArrayExpr(a) => children.extend(a.elements.iter_mut()),
        N::RowExpr(r) => children.extend(r.args.iter_mut()),
        N::CoalesceExpr(c) => children.extend(c.args.iter_mut()),
        N::MinMaxExpr(m) => children.extend(m.args.iter_mut()),
        N::NullTest(t) => children.extend(t.arg.as_deref_mut()),
        N::AIndirection(a) => {
            children.extend(a.arg.as_deref_mut());
            for ind in &mut a.indirection {
                if let Some(N::AIndices(idx)) = ind.node.as_mut() {
                    children.extend(idx.lidx.as_deref_mut());
                    children.extend(idx.uidx.as_deref_mut());
                }
            }
        }
        N::CaseExpr(c) => {
            children.extend(c.arg.as_deref_mut());
            for w in &mut c.args {
                if let Some(N::CaseWhen(cw)) = w.node.as_mut() {
                    children.extend(cw.expr.as_deref_mut());
                    children.extend(cw.result.as_deref_mut());
                }
            }
            children.extend(c.defresult.as_deref_mut());
        }
        _ => {}
    }
    for child in children {
        resolve_sublinks_in_expr(child, lookup, params, run, outer)?;
    }
    Ok(())
}

thread_local! {
    /// Is the plan being made the one that will EXECUTE? Every other plan --
    /// a `Describe`, a classification pass -- must not run a subquery that
    /// has a side effect, or `select (select nextval('s'))` advances the
    /// sequence once per plan rather than once per execution. Defaults to
    /// false, so a planning path nobody marked is side-effect free.
    static PLANNING_TO_EXECUTE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Mark the plans made inside `f` as the ones that execute.
pub fn planning_to_execute<R>(f: impl FnOnce() -> R) -> R {
    let previous = PLANNING_TO_EXECUTE.with(|p| p.replace(true));
    let out = f();
    PLANNING_TO_EXECUTE.with(|p| p.set(previous));
    out
}

/// A function this server's reference -- PostgreSQL 14 -- does not have, or
/// refuses in a UTF8 database, answered as PostgreSQL answers it:
/// `regexp_count` and friends arrived in 15 (42883, naming the argument
/// types), and `to_ascii` cannot convert from UTF8 (0A000).
fn function_absent_in_reference(
    f: &pg_query::protobuf::FuncCall,
    params: &[Bson],
) -> Option<Error> {
    let name = func_name(f)?;
    if correlated::user_function_named(&name)
        && correlated::user_function(&name, f.args.len()).is_none()
    {
        let types: Vec<String> = f
            .args
            .iter()
            .map(|a| {
                let v = const_value(a, params).unwrap_or(Bson::Null);
                display_type(&static_type(a, &v))
            })
            .collect();
        return Some(Error::UndefinedFunction(format!(
            "function {name}({}) does not exist",
            types.join(", ")
        )));
    }
    if name == "to_ascii" {
        return Some(Error::FeatureNotSupported(
            "encoding conversion from UTF8 to ASCII not supported".into(),
        ));
    }
    if !matches!(
        name.as_str(),
        "regexp_count" | "regexp_instr" | "regexp_substr" | "regexp_like"
    ) {
        return None;
    }
    let types: Vec<String> = f
        .args
        .iter()
        .map(|a| match a.node.as_ref() {
            Some(N::AConst(c))
                if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_))) =>
            {
                "unknown".to_string()
            }
            _ => {
                let v = const_value(a, params).unwrap_or(Bson::Null);
                display_type(&static_type(a, &v))
            }
        })
        .collect();
    Some(Error::UndefinedFunction(format!(
        "function {name}({}) does not exist",
        types.join(", ")
    )))
}

/// A `TypeName` for an internal type name (`int4`, `text[]`).
fn type_name_node(ty: &str) -> pg_query::protobuf::TypeName {
    let (base, array) = match ty.strip_suffix("[]") {
        Some(b) => (b, true),
        None => (ty, false),
    };
    pg_query::protobuf::TypeName {
        names: vec![string_node(base)],
        typemod: -1,
        array_bounds: if array {
            vec![pg_query::protobuf::Node {
                node: Some(N::Integer(pg_query::protobuf::Integer { ival: -1 })),
            }]
        } else {
            Vec::new()
        },
        location: -1,
        ..Default::default()
    }
}

/// The functions whose call is a side effect or differs per call.
const VOLATILE_FUNCTIONS: &[&str] = &[
    "nextval",
    "setval",
    "currval",
    "lastval",
    "random",
    "gen_random_uuid",
    "uuid_generate_v4",
    "clock_timestamp",
    "timeofday",
    "pg_sleep",
    "pg_advisory_lock",
    "pg_try_advisory_lock",
    "txid_current",
];

fn calls_volatile(s: &pg_query::protobuf::SelectStmt) -> bool {
    let mut probe = s.clone();
    let mut found = false;
    let mut check = |n: &mut pg_query::protobuf::Node| -> Result<()> {
        if let Some(N::FuncCall(f)) = n.node.as_ref() {
            if func_name(f).is_some_and(|name| VOLATILE_FUNCTIONS.contains(&name.as_str())) {
                found = true;
            }
        }
        Ok(())
    };
    for n in probe.target_list.iter_mut() {
        let _ = walk_expr(n, &mut check);
    }
    for n in [
        probe.where_clause.as_deref_mut(),
        probe.having_clause.as_deref_mut(),
    ]
    .into_iter()
    .flatten()
    {
        let _ = walk_expr(n, &mut check);
    }
    found
}

/// One subquery, resolved to the node that stands in for it.
fn resolve_one_sublink(
    sl: &pg_query::protobuf::SubLink,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &mut Vec<Bson>,
    run: SubqueryRunner<'_>,
    outer: &[String],
) -> Result<pg_query::protobuf::Node> {
    let Some(N::SelectStmt(inner)) = sl.subselect.as_ref().and_then(|q| q.node.as_ref()) else {
        return Err(Error::Unsupported("this subquery".into()));
    };
    // A subquery may itself contain subqueries and CTEs; both are resolved
    // before it is planned, innermost first.
    let mut inner = (**inner).clone();
    resolve_sublinks_in_select(&mut inner, lookup, params, run)?;
    // The enclosing statements' CTEs are in scope, behind the subquery's own
    // (inlining takes the first definition of a name, so its own win). Added
    // AFTER the subquery's own resolution: they are already resolved.
    let visible = VISIBLE_CTES.with(|v| v.borrow().clone());
    if !visible.is_empty() {
        let mut with = inner.with_clause.clone().unwrap_or_default();
        with.ctes.extend(visible);
        inner.with_clause = Some(with);
    }
    let inner = inline_ctes(&inner)?;

    // A QUALIFIED reference to something outside the subquery has to be
    // caught BEFORE planning, because planning cannot see it: the lowering
    // resolves a column by its LAST name part and ignores the qualifier, so
    // `(select 1 from sq_emp e where e.dept_id = d.id)` bound the outer
    // `d.id` to `sq_emp`'s OWN `id` and planned clean. The EXISTS around it
    // then answered true for every outer row -- a wrong answer, not an error,
    // and invisible until it was diffed against PostgreSQL.
    // A CORRELATED subquery -- one reading the outer row -- has no single
    // value to substitute, so it becomes a call evaluated per row instead
    // (see `correlated`). Detected two ways, because planning alone cannot
    // see a qualified one: the lowering resolves a column by its LAST name
    // part and ignores the qualifier, so `e.dept_id = d.id` would bind the
    // outer `d.id` to the inner table's own `id` and plan clean.
    if foreign_qualifier(&inner).is_some() {
        let test = sl.testexpr.as_deref().cloned();
        let mut sl = sl.clone();
        if let Some(mut t) = test {
            resolve_sublinks_in_expr(&mut t, lookup, params, run, outer)?;
            sl.testexpr = Some(Box::new(t));
        }
        return correlated::correlate(&sl, &inner, lookup, params, outer);
    }
    let plan = match plan_select(&inner, lookup, params) {
        Ok(p) => p,
        // A column the subquery's own FROM does not have, but the query
        // AROUND it does, is a correlation too. A name the outer query does
        // not have either is an ordinary typo and keeps its 42703.
        Err(Error::UndefinedColumn(name)) if outer.contains(&name) => {
            let test = sl.testexpr.as_deref().cloned();
            let mut sl = sl.clone();
            if let Some(mut t) = test {
                resolve_sublinks_in_expr(&mut t, lookup, params, run, outer)?;
                sl.testexpr = Some(Box::new(t));
            }
            return correlated::correlate(&sl, &inner, lookup, params, outer);
        }
        Err(e) => return Err(e),
    };
    // A subquery with a side effect runs only in the plan that executes; any
    // other plan needs just its TYPE, and sees it return no rows.
    let rows = if calls_volatile(&inner) && !PLANNING_TO_EXECUTE.with(|p| p.get()) {
        Vec::new()
    } else {
        run(&plan)?
    };
    let first_column = |r: Vec<Bson>| r.into_iter().next().unwrap_or(Bson::Null);

    match SubLinkType::try_from(sl.sub_link_type) {
        Ok(SubLinkType::ExistsSublink) => Ok(bool_const_node(!rows.is_empty())),
        Ok(SubLinkType::ExprSublink) => {
            if rows.len() > 1 {
                return Err(Error::CardinalityViolation(
                    "more than one row returned by a subquery used as an expression".into(),
                ));
            }
            // No rows is NULL, not zero rows: `(select x from t where false)`
            // is a NULL value, which is why the empty case is a value at all
            // -- and a NULL of the subquery's column TYPE, which a bare NULL
            // parameter would not carry.
            match rows.into_iter().next().map(first_column) {
                Some(value) => Ok(param_node(params, value)),
                None => {
                    let ty = sub_plan_def(&plan, lookup)?
                        .columns
                        .first()
                        .map(|c| c.pg_type.clone())
                        .unwrap_or_else(|| "text".into());
                    let null = param_node(params, Bson::Null);
                    Ok(pg_query::protobuf::Node {
                        node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
                            arg: Some(Box::new(null)),
                            type_name: Some(type_name_node(&ty)),
                            location: -1,
                        }))),
                    })
                }
            }
        }
        Ok(SubLinkType::ArraySublink) => {
            let values: Vec<Bson> = rows.into_iter().map(first_column).collect();
            Ok(param_node(params, Bson::Array(values)))
        }
        Ok(kind @ (SubLinkType::AnySublink | SubLinkType::AllSublink)) => {
            let values: Vec<Bson> = rows.into_iter().map(first_column).collect();
            let mut test = sl
                .testexpr
                .as_deref()
                .cloned()
                .ok_or_else(|| Error::Unsupported("this ANY/ALL subquery".into()))?;
            // The left-hand side is copied into the replacement node, so a
            // subquery in it (`(select 1) in (select ...)`) has to be resolved
            // here -- the walk that got us here replaced this whole SubLink
            // and will not descend into what we build.
            resolve_sublinks_in_expr(&mut test, lookup, params, run, outer)?;
            // `x IN (subquery)` is `x = ANY (...)` and `x NOT IN (subquery)`
            // is `x <> ALL (...)`; PostgreSQL parses them into exactly these
            // two nodes, so the operator comes off the SubLink rather than
            // being inferred. The array form is deliberate: `lower_scalar_
            // array` already has ANY/ALL's three-valued rules right -- an
            // empty ANY matches nothing, an empty ALL matches everything, and
            // a NULL element makes ALL unsatisfiable, which is what makes
            // `NOT IN` over a column containing NULL return no rows.
            let op = sl
                .oper_name
                .first()
                .and_then(|n| match n.node.as_ref() {
                    Some(N::String(s)) => Some(s.sval.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| "=".to_string());
            let kind = if kind == SubLinkType::AnySublink {
                AExprKind::AexprOpAny
            } else {
                AExprKind::AexprOpAll
            };
            Ok(pg_query::protobuf::Node {
                node: Some(N::AExpr(Box::new(AExpr {
                    kind: kind as i32,
                    name: vec![string_node(&op)],
                    lexpr: Some(Box::new(test)),
                    rexpr: Some(Box::new(param_node(params, Bson::Array(values)))),
                    location: sl.location,
                }))),
            })
        }
        _ => Err(Error::Unsupported("this subquery form".into())),
    }
}

/// The first qualified column reference in `s` whose qualifier names nothing
/// in `s`'s own FROM -- that is, a correlation. `None` when every reference
/// resolves inside the subquery.
fn foreign_qualifier(s: &pg_query::protobuf::SelectStmt) -> Option<String> {
    let mut names: Vec<String> = Vec::new();
    for item in &s.from_clause {
        collect_from_names(item, &mut names);
    }
    let mut found: Option<String> = None;
    let mut s = s.clone();
    let mut check = |node: &mut pg_query::protobuf::Node| -> Result<()> {
        if found.is_some() {
            return Ok(());
        }
        let Some(N::ColumnRef(c)) = node.node.as_ref() else {
            return Ok(());
        };
        // `t.col` qualifies with the second-to-last part; `schema.t.col`
        // likewise, which is why this indexes from the end.
        if c.fields.len() < 2 {
            return Ok(());
        }
        let Some(N::String(q)) = c.fields[c.fields.len() - 2].node.as_ref() else {
            return Ok(());
        };
        if !names.contains(&q.sval) {
            let col = column_ref_name(c).unwrap_or_default();
            found = Some(format!("{}.{col}", q.sval));
        }
        Ok(())
    };
    for t in &mut s.target_list {
        if let Some(N::ResTarget(rt)) = t.node.as_mut() {
            if let Some(v) = rt.val.as_deref_mut() {
                let _ = walk_expr(v, &mut check);
            }
        }
    }
    for clause in [
        s.where_clause.as_deref_mut(),
        s.having_clause.as_deref_mut(),
    ]
    .into_iter()
    .flatten()
    {
        let _ = walk_expr(clause, &mut check);
    }
    for item in &mut s.sort_clause {
        if let Some(N::SortBy(sb)) = item.node.as_mut() {
            if let Some(n) = sb.node.as_deref_mut() {
                let _ = walk_expr(n, &mut check);
            }
        }
    }
    found
}

/// Every name a FROM item can be addressed by: its alias when it has one, and
/// a table's own name as well (`from t` accepts both `t.c` and a bare `c`).
fn collect_from_names(item: &pg_query::protobuf::Node, out: &mut Vec<String>) {
    match item.node.as_ref() {
        Some(N::RangeVar(r)) => {
            out.push(r.relname.clone());
            if let Some(a) = r.alias.as_ref() {
                out.push(a.aliasname.clone());
            }
        }
        Some(N::RangeSubselect(rs)) => {
            if let Some(a) = rs.alias.as_ref() {
                out.push(a.aliasname.clone());
            }
        }
        Some(N::RangeFunction(_)) => {}
        Some(N::JoinExpr(j)) => {
            for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
                collect_from_names(side, out);
            }
        }
        _ => {}
    }
}

/// A literal boolean, for a resolved `EXISTS`.
fn bool_const_node(value: bool) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::AConst(pg_query::protobuf::AConst {
            isnull: false,
            location: -1,
            val: Some(pg_query::protobuf::a_const::Val::Boolval(
                pg_query::protobuf::Boolean { boolval: value },
            )),
        })),
    }
}

/// A resolved subquery's VALUE, as a parameter rather than a literal node.
///
/// Appending to the bound parameters and emitting `$N` reuses the one path
/// that already turns a `Bson` of any type into whatever the lowering needs --
/// dates, numerics, arrays and NULL included. Building `A_Const` nodes instead
/// would mean a second, narrower value encoder, and the types it did not cover
/// would fail in a way that looked like a missing feature.
fn param_node(params: &mut Vec<Bson>, value: Bson) -> pg_query::protobuf::Node {
    params.push(value);
    pg_query::protobuf::Node {
        node: Some(N::ParamRef(pg_query::protobuf::ParamRef {
            // `$N` is 1-based, and the statement's own parameters occupy
            // 1..=n, so appending never collides with one the client bound.
            number: i32::try_from(params.len()).unwrap_or(i32::MAX),
            location: -1,
        })),
    }
}

/// `WITH name AS (SELECT ...)` -- rewritten into the FROM-subqueries it is
/// shorthand for, before anything else looks at the statement.
///
/// PostgreSQL 12 and later inline a non-recursive CTE itself, and for a pure
/// SELECT body an inlined CTE and a materialised one return the same rows, so
/// this reproduces the ANSWER even where it does not reproduce the plan. The
/// two cases where that would NOT hold are both refused below rather than
/// inlined wrongly:
///
/// * `WITH RECURSIVE` -- a self-reference has no subquery to expand into;
/// * a data-modifying CTE (`WITH x AS (INSERT ... RETURNING ...)`) -- inlining
///   it would run the write once per reference, or not at all if the reference
///   is optimised away, and PostgreSQL guarantees it runs exactly once.
///
/// `MATERIALIZED` and `NOT MATERIALIZED` are both accepted: they are planner
/// hints about when the body runs, and neither changes the rows.
fn inline_ctes(s: &pg_query::protobuf::SelectStmt) -> Result<pg_query::protobuf::SelectStmt> {
    let Some(with) = s.with_clause.as_ref() else {
        return Ok(s.clone());
    };
    if with.recursive {
        return Err(Error::Unsupported("WITH RECURSIVE".into()));
    }
    // (name, body, column aliases). Built in declared order, because a CTE may
    // reference the ones written before it -- and only those: PostgreSQL scopes
    // a non-recursive WITH that way too.
    let mut defs: Vec<(
        String,
        pg_query::protobuf::SelectStmt,
        Vec<pg_query::protobuf::Node>,
    )> = Vec::new();
    for cte in &with.ctes {
        let Some(N::CommonTableExpr(c)) = cte.node.as_ref() else {
            return Err(Error::Unsupported("this WITH item".into()));
        };
        let Some(N::SelectStmt(body)) = c.ctequery.as_ref().and_then(|q| q.node.as_ref()) else {
            return Err(Error::Unsupported("a data-modifying WITH".into()));
        };
        // A CTE body may itself carry a WITH, and may reference the CTEs
        // declared before it; both are resolved here so the body that gets
        // substituted is already flat.
        let body = inline_ctes(body)?;
        let body = substitute_ctes(&body, &defs);
        defs.push((c.ctename.clone(), body, c.aliascolnames.clone()));
    }
    let mut out = substitute_ctes(s, &defs);
    out.with_clause = None;
    Ok(out)
}

/// Replace every FROM reference to one of `defs` with the subquery it names.
fn substitute_ctes(
    s: &pg_query::protobuf::SelectStmt,
    defs: &[(
        String,
        pg_query::protobuf::SelectStmt,
        Vec<pg_query::protobuf::Node>,
    )],
) -> pg_query::protobuf::SelectStmt {
    if defs.is_empty() {
        return s.clone();
    }
    let mut out = s.clone();
    for item in &mut out.from_clause {
        substitute_in_from(item, defs);
    }
    // A set operation holds its sides here rather than in `from_clause`, and a
    // CTE is visible to both (`with c as (...) select * from c union select
    // ...`).
    for side in [out.larg.as_mut(), out.rarg.as_mut()].into_iter().flatten() {
        **side = substitute_ctes(side, defs);
    }
    out
}

/// One FROM item, rewritten in place: a bare reference to a CTE becomes the
/// subquery, a JOIN's two sides are rewritten recursively, and an existing
/// subquery's body is rewritten so a CTE is visible inside it.
fn substitute_in_from(
    item: &mut pg_query::protobuf::Node,
    defs: &[(
        String,
        pg_query::protobuf::SelectStmt,
        Vec<pg_query::protobuf::Node>,
    )],
) {
    match item.node.as_mut() {
        Some(N::RangeVar(r)) => {
            // Only an UNQUALIFIED name can be a CTE: `public.c` is a table
            // even where a CTE `c` is in scope, which is PostgreSQL's rule.
            if !r.schemaname.is_empty() || !r.catalogname.is_empty() {
                return;
            }
            let Some((name, body, colnames)) = defs.iter().find(|(n, _, _)| *n == r.relname) else {
                return;
            };
            // The reference's own alias wins (`from c as x`); without one the
            // CTE's name is the alias, which is what an unaliased reference is
            // addressed by.
            let aliasname = r
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| name.clone());
            let colnames = r
                .alias
                .as_ref()
                .map(|a| a.colnames.clone())
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| colnames.clone());
            item.node = Some(N::RangeSubselect(Box::new(
                pg_query::protobuf::RangeSubselect {
                    lateral: false,
                    subquery: Some(Box::new(pg_query::protobuf::Node {
                        node: Some(N::SelectStmt(Box::new(body.clone()))),
                    })),
                    alias: Some(pg_query::protobuf::Alias {
                        aliasname,
                        colnames,
                    }),
                },
            )));
        }
        Some(N::JoinExpr(j)) => {
            for side in [j.larg.as_mut(), j.rarg.as_mut()].into_iter().flatten() {
                substitute_in_from(side, defs);
            }
        }
        Some(N::RangeSubselect(rs)) => {
            if let Some(N::SelectStmt(inner)) = rs.subquery.as_mut().and_then(|q| q.node.as_mut()) {
                **inner = substitute_ctes(inner, defs);
            }
        }
        _ => {}
    }
}

fn plan_select(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<Statement> {
    // `WITH` is shorthand for the FROM-subqueries below, so it is rewritten
    // away before any of the shape checks run -- every one of them would
    // otherwise have to know about it.
    if s.with_clause.is_some() {
        return plan_select(&inline_ctes(s)?, lookup, params);
    }
    // A view is shorthand for the subquery it was defined as, so it is
    // rewritten away here, before any shape check looks at the FROM list.
    let expanded = expand_views(s)?;
    let s = &expanded;
    if s.op != pg_query::protobuf::SetOperation::SetopNone as i32 {
        return plan_set_operation(s, lookup, params);
    }
    // A set-returning function over a column in the select list is a
    // LATERAL join (see `joins::select_list_srf`).
    if let Some(rewritten) = joins::select_list_srf(s)? {
        return plan_select(&rewritten, lookup, params);
    }
    if s.from_clause.is_empty() {
        return plan_select_constant(s, params);
    }
    // A JOIN, or a comma-separated FROM. The narrow two-table join path goes
    // first: it is what psycopg's catalog queries take, and it compares OIDs
    // with the regtype / regclass awareness those need. Anything it refuses
    // becomes a planned SOURCE and the query is rewritten to read it, then
    // planned like any single-source query (see `joins`).
    let planned = if joins::is_join(s) {
        match plan_select_rest(s, lookup, params) {
            Err(Error::Unsupported(_)) => joins::plan_join_source(s, lookup, params)
                .and_then(|rewritten| plan_select(&rewritten, lookup, params))
                .map_err(joins::unmangle),
            other => other,
        }
    } else {
        plan_select_rest(s, lookup, params)
    };
    planned.map_err(|e| qualify_undefined_column(e, s))
}

/// PostgreSQL names a missing QUALIFIED column with its qualifier --
/// `column e.nosuch does not exist` -- where the lowering, which resolves by
/// the last name part, knows only `nosuch`.
fn qualify_undefined_column(e: Error, s: &pg_query::protobuf::SelectStmt) -> Error {
    let Error::UndefinedColumn(name) = &e else {
        return e;
    };
    let mut found: Option<String> = None;
    let mut bare = false;
    let mut probe = s.clone();
    let mut check = |n: &mut pg_query::protobuf::Node| -> Result<()> {
        if let Some(N::ColumnRef(c)) = n.node.as_ref() {
            let parts: Vec<String> = c
                .fields
                .iter()
                .filter_map(|f| match f.node.as_ref()? {
                    N::String(s) => Some(s.sval.clone()),
                    _ => None,
                })
                .collect();
            if parts.len() == 1 && parts[0] == *name {
                // A BARE reference by this name may be the one that failed --
                // and is what the correlation detector looks for -- so the
                // plain error stands.
                bare = true;
            }
            if parts.len() >= 2 && parts.last() == Some(name) && found.is_none() {
                found = Some(parts[parts.len() - 2..].join("."));
            }
        }
        Ok(())
    };
    for n in probe
        .target_list
        .iter_mut()
        .chain(probe.sort_clause.iter_mut())
        .chain(probe.group_clause.iter_mut())
    {
        let _ = walk_expr(n, &mut check);
    }
    for n in [
        probe.where_clause.as_deref_mut(),
        probe.having_clause.as_deref_mut(),
    ]
    .into_iter()
    .flatten()
    {
        let _ = walk_expr(n, &mut check);
    }
    match found {
        Some(q) if !bare => Error::Sqlstate("42703", format!("column {q} does not exist")),
        _ => e,
    }
}

/// `plan_select` past the rewrites that turn a statement into its canonical
/// shape (CTEs, views, set operations, a FROM-less select, a general join).
fn plan_select_rest(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<Statement> {
    if !s.group_clause.is_empty() || has_aggregate(s) {
        // A window function OVER an aggregate (`sum(sum(v)) over (order by
        // g)` beside a GROUP BY) runs the window over the GROUPED rows, which
        // means the aggregate planner would have to grow a window pass of its
        // own. Named here so the refusal says what is missing: routed on into
        // the aggregate planner it came out as `function sum() is not
        // supported yet`, which is false -- `sum` is supported, and a reader
        // chasing that message looks in the wrong place entirely.
        if has_window(s) {
            return plan_select(&split_window_over_aggregate(s)?, lookup, params);
        }
        return plan_aggregate(s, lookup, params);
    }
    // A window function in a WHERE is an ERROR in PostgreSQL, not a filter --
    // the WHERE runs before the windows do, so there is nothing to test.
    // PostgreSQL's own message and class, because it refuses this too.
    if s.where_clause
        .as_deref()
        .is_some_and(|w| node_has_window(Some(w)))
    {
        return Err(Error::Windowing(
            "window functions are not allowed in WHERE".into(),
        ));
    }
    // A window over a JOIN or a generated source. Both are planned by paths
    // that build their output columns from the two sides' columns, and a
    // window target is a column of NEITHER -- so they refuse it, but they
    // refused it as `this subquery target` and `function row_number() is not
    // supported yet`, neither of which is true or points anywhere useful.
    if has_window(s)
        && (s.from_clause.len() == 2
            || matches!(
                s.from_clause.first().and_then(|f| f.node.as_ref()),
                Some(N::JoinExpr(_))
            ))
    {
        return Err(Error::Unsupported("a window function over a JOIN".into()));
    }
    // A window over `generate_series`: the series keeps a lazy path of its
    // own, but a window needs every row at once anyway, so it is materialised
    // into a source here and the query planned over that.
    if has_window(s) && s.from_clause.len() == 1 {
        if let Some(series) = series_from_clause(&s.from_clause[0], params)? {
            let fits = |v: i64| i32::try_from(v).is_ok();
            let wide = !fits(series.start) || !fits(series.stop);
            let ty = if wide { "int8" } else { "int4" };
            let rows: Vec<Vec<Bson>> = series
                .values()
                .into_iter()
                .map(|v| {
                    vec![if wide {
                        Bson::Int64(v)
                    } else {
                        Bson::Int32(i32::try_from(v).unwrap_or_default())
                    }]
                })
                .collect();
            let def = TableDef::new("", vec![Column::new(&series.column, ty, false)]);
            let name = joins::register_source(SubSource {
                alias: series.column.clone(),
                plan: Box::new(Statement::ValuesConstant(ValuesConstant {
                    names: vec![series.column.clone()],
                    types: vec![ty.to_string()],
                    rows,
                })),
                def,
            });
            let mut rewritten = s.clone();
            rewritten.from_clause = vec![joins::placeholder_from(name)];
            return plan_select_rest(&rewritten, lookup, params);
        }
    }
    // `FROM a, b` -- a CROSS join, the comma form of `a CROSS JOIN b`. It
    // rides the JOIN path with no ON predicate.
    if s.from_clause.len() == 2 {
        let join = plan_join_select(s, lookup, params)?;
        return plan_join_plain_select(s, join, lookup, params);
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
    // A top-level JOIN stands in for the table -- `FROM a JOIN b ON ...` in a
    // plain (non-aggregate) select, which is `RangeInfo.fetch`'s shape.
    if matches!(s.from_clause[0].node.as_ref(), Some(N::JoinExpr(_))) {
        let join = plan_join_select(s, lookup, params)?;
        return plan_join_plain_select(s, join, lookup, params);
    }
    // A SET-returning function in FROM becomes a materialised source and rides
    // the FROM-subquery path below. Checked before the single-value function
    // source, which would otherwise claim it and answer `this FROM function`.
    let srf = srf_from_clause(&s.from_clause[0], params)?;
    // Any other function in FROM -- `pg_sleep`, `pg_listening_channels` --
    // is a FROM-less select with the function as its row source.
    if srf.is_none() {
        if let Some(N::RangeFunction(rf)) = s.from_clause[0].node.as_ref() {
            return plan_function_source_select(s, rf, params);
        }
    }
    // `FROM (SELECT ...) s`, and an inlined CTE reference, which arrives as
    // exactly the same node. The subquery's OUTPUT def stands in for the
    // table's, so everything downstream -- the targets, the WHERE, ORDER BY,
    // LIMIT -- plans against it unchanged and never learns the source was not
    // a table.
    let (table, def, sub) = match s.from_clause[0].node.as_ref() {
        _ if srf.is_some() => {
            let src = srf.expect("checked");
            (String::new(), src.def.clone(), Some(Box::new(src)))
        }
        Some(N::RangeSubselect(rs)) => {
            let src = plan_from_subquery(rs, lookup, params)?;
            (String::new(), src.def.clone(), Some(Box::new(src)))
        }
        Some(N::RangeVar(r)) if joins::planned_join(&r.relname).is_some() => {
            let src = joins::planned_join(&r.relname).expect("checked");
            (String::new(), src.def.clone(), Some(Box::new(src)))
        }
        Some(N::RangeVar(r)) => {
            let table = relation_name(r);
            let def = lookup(&table).ok_or_else(|| Error::UndefinedTable(table.clone()))?;
            (table, def, None)
        }
        Some(other) => return Err(Error::Unsupported(disc(other))),
        None => return Err(Error::Parse("empty FROM".into())),
    };

    // Window functions project into synthetic `__winN` fields, which are
    // appended to the def so the row schema and every later lookup resolve
    // them exactly like a stored column.
    let (columns, casts, windows, extra) = if has_window(s) {
        let (c, k, w, e) = plan_window_targets(s, &def, params)?;
        (c, k, w, e)
    } else {
        let (c, k) = plan_table_targets(&s.target_list, &def, params)?;
        (c, k, Vec::new(), Vec::new())
    };
    let def = if extra.is_empty() {
        def
    } else {
        let mut d = def;
        d.columns.extend(extra);
        d
    };

    // A WHERE that does not lower to an MQL filter -- `where (case ... end)`,
    // say -- becomes a RESIDUAL evaluated per row instead of a refusal. Only
    // `Unsupported` falls back: an undefined column or a bad type is a real
    // error and must stay one, or a typo would become a silent full scan that
    // quietly returns nothing.
    let mut residual = None;
    let filter = match s.where_clause.as_ref() {
        None => Document::new(),
        Some(w) => match lower_where(w, &def, params) {
            Ok(f) => f,
            Err(Error::Unsupported(_)) => {
                let fields: Vec<RowField> = def
                    .columns
                    .iter()
                    .map(|c| (c.name.clone(), c.field(), c.pg_type.clone()))
                    .collect();
                let mut sample = Document::new();
                for c in &def.columns {
                    sample.insert(c.field(), sample_value_for_type(&c.pg_type));
                }
                residual = Some(row_column_expr(w, &fields, params, &sample)?);
                Document::new()
            }
            Err(e) => return Err(e),
        },
    };

    let mut order = Vec::new();
    // Row expressions behind any computed sort keys, indexed by the
    // `__orderN` synthetic field names that reference them.
    let mut order_exprs: Vec<ColumnExpr> = Vec::new();
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
                // An OUTPUT NAME wins over a table column, which is
                // PostgreSQL's rule for ORDER BY (and only for ORDER BY --
                // a WHERE cannot see an output alias). `select id * 2 as d
                // ... order by d` and `... row_number() over (...) as rn
                // order by rn` both depend on it, and without it the second
                // one had no way to sort by the window it had just computed.
                match columns.iter().find(|(out, _)| *out == col) {
                    Some((_, field)) => field.clone(),
                    None => def
                        .field_of(&col)
                        .ok_or_else(|| Error::UndefinedColumn(col.clone()))?,
                }
            }
            // A COMPUTED sort key (`order by n * -1`, `order by upper(a)`).
            // Planned as a row expression over the table's columns and given a
            // synthetic field; the executor materialises it per row just
            // before sorting, so `sort_rows` stays one comparison routine.
            Some(_) => {
                let node = sb.node.as_deref().expect("matched Some");
                let fields: Vec<RowField> = def
                    .columns
                    .iter()
                    .map(|c| (c.name.clone(), c.field(), c.pg_type.clone()))
                    .collect();
                let mut sample = Document::new();
                for c in &def.columns {
                    sample.insert(c.field(), sample_value_for_type(&c.pg_type));
                }
                let expr = row_column_expr(node, &fields, params, &sample)?;
                // Named by POSITION so two expression keys in one ORDER BY
                // cannot collide, and prefixed so no real column can.
                order_exprs.push(expr);
                format!("__order{}", order_exprs.len() - 1)
            }
            None => return Err(Error::Unsupported("an empty ORDER BY key".into())),
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
        let expr = field
            .strip_prefix("__order")
            .and_then(|n| n.parse::<usize>().ok())
            .map(|i| order_exprs[i].clone());
        order.push(OrderKey {
            field,
            ascending,
            nulls,
            expr,
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

    let distinct = plan_distinct(s, &|name| def.field_of(name))?;

    Ok(Statement::Select(Select {
        series: None,
        sub,
        windows,
        join: None,
        table,
        columns,
        casts,
        filter,
        residual,
        order,
        limit,
        offset,
        distinct,
    }))
}

/// `FROM (SELECT ...) alias [(col, ...)]` -- plan the inner query and describe
/// its output, so the outer query can be planned against it as though it were
/// a table. The executor materialises the rows before the outer runs.
///
/// An inlined CTE reference arrives here as the same node, so `WITH` costs
/// nothing beyond the rewrite that produces it.
fn plan_from_subquery(
    rs: &pg_query::protobuf::RangeSubselect,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<SubSource> {
    if rs.lateral {
        // A LATERAL subquery sees the row to its left, so it cannot be
        // materialised once up front the way this path does. Refused by name
        // rather than answered wrongly.
        return Err(Error::Unsupported("a LATERAL subquery in FROM".into()));
    }
    let Some(N::SelectStmt(inner)) = rs.subquery.as_ref().and_then(|q| q.node.as_ref()) else {
        return Err(Error::Unsupported("this subquery in FROM".into()));
    };
    // PostgreSQL requires the alias and answers 42601 without one; clients
    // depend on that, so it is not quietly defaulted to anything.
    let alias = rs
        .alias
        .as_ref()
        .map(|a| a.aliasname.clone())
        .unwrap_or_default();
    if alias.is_empty() {
        return Err(Error::Parse("subquery in FROM must have an alias".into()));
    }
    let plan = plan_select(inner, lookup, params)?;
    let mut def = sub_plan_def(&plan, lookup)?;
    // `(select ...) s(a, b)` renames the outputs POSITIONALLY. The executor
    // materialises rows positionally too, so the rename needs nothing else;
    // PostgreSQL answers 42P10 when more names are given than the subquery has
    // columns, and leaves the rest alone when fewer.
    let colnames: Vec<String> = rs
        .alias
        .as_ref()
        .map(|a| a.colnames.iter().filter_map(alias_colname).collect())
        .unwrap_or_default();
    if colnames.len() > def.columns.len() {
        return Err(Error::Parse(format!(
            "table \"{alias}\" has {} columns available but {} columns specified",
            def.columns.len(),
            colnames.len()
        )));
    }
    for (c, name) in def.columns.iter_mut().zip(&colnames) {
        c.name = name.clone();
    }
    def.name = alias.clone();
    Ok(SubSource {
        alias,
        plan: Box::new(plan),
        def,
    })
}

/// One name from an alias's column list (`s(a, b)`).
fn alias_colname(n: &pg_query::protobuf::Node) -> Option<String> {
    match n.node.as_ref()? {
        N::String(s) => Some(s.sval.clone()),
        _ => None,
    }
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
    if let Some(N::RangeVar(r)) = s.from_clause[0].node.as_ref() {
        if let Some(src) = joins::planned_join(&r.relname) {
            let def = src.def.clone();
            return finish_aggregate(s, String::new(), None, Some(Box::new(src)), def, params);
        }
    }
    // `FROM (SELECT ... FROM a JOIN b ON ...) x` -- the joined subquery every
    // psycopg type-registration query is built on. Kept as its own path
    // because the join is planned as ONE source rather than materialised, and
    // it is the shape the catalog queries take.
    if let Some(N::RangeSubselect(rs)) = s.from_clause[0].node.as_ref() {
        let inner = match rs.subquery.as_ref().and_then(|q| q.node.as_ref()) {
            Some(N::SelectStmt(inner)) => inner,
            _ => return Err(Error::Unsupported("this subquery in FROM".into())),
        };
        // Any OTHER subquery shape -- `select max(c) from (select ... group by
        // k) s`, and every aggregate over an inlined CTE -- falls through to
        // the general materialised source below. Before it did not, and the
        // join planner's own `a subquery without a JOIN` refusal surfaced to
        // the client as though a grouped subquery were unimplementable.
        match plan_join_select(inner, lookup, params) {
            Ok(join) => {
                let def = join_output_def(&join, lookup)?;
                return finish_aggregate(s, String::new(), Some(Box::new(join)), None, def, params);
            }
            Err(Error::Unsupported(_)) => {
                let src = plan_from_subquery(rs, lookup, params)?;
                let def = src.def.clone();
                return finish_aggregate(s, String::new(), None, Some(Box::new(src)), def, params);
            }
            Err(e) => return Err(e),
        }
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
                expr: None,
                distinct: false,
                // A generated source has no table for a FILTER to read.
                filter: None,
                sep: None,
                order: Vec::new(),
                source_typmod: -1,
                ..Default::default()
            });
        }
        // The WHERE clause was silently dropped here before: `count(*)
        // from generate_series(1, 5) i where i > 2` answered 5.
        let filter = series_where(s, &series, params)?;
        return Ok(Statement::Aggregate(Aggregate {
            table: String::new(),
            series: Some(series),
            sub: None,
            join: None,
            group_by: Vec::new(),
            // A bare aggregate over a series has no GROUP BY at all.
            grouping_sets: None,
            items,
            select,
            filter,
            order: Vec::new(),
            limit: None,
            offset: 0,
            having: None,
            exprs: Vec::new(),
            distinct: aggregate_distinct(s)?,
        }));
    }
    // An aggregate over a SET-returning function -- `count(*) FROM unnest(...)`,
    // `array_agg(x) FROM unnest(...) x`. The materialised source is the same
    // one the plain select uses, so GROUP BY and HAVING over it work for free.
    if let Some(src) = srf_from_clause(&s.from_clause[0], params)? {
        let def = src.def.clone();
        return finish_aggregate(s, String::new(), None, Some(Box::new(src)), def, params);
    }
    // `select max(c) from (select ... group by k) s` -- and every aggregate
    // over an inlined CTE, which reaches here as the same node.
    if let Some(N::RangeSubselect(rs)) = s.from_clause[0].node.as_ref() {
        let src = plan_from_subquery(rs, lookup, params)?;
        let def = src.def.clone();
        return finish_aggregate(s, String::new(), None, Some(Box::new(src)), def, params);
    }
    let table = match s.from_clause[0].node.as_ref() {
        Some(N::RangeVar(r)) => relation_name(r),
        Some(other) => return Err(Error::Unsupported(disc(other))),
        None => return Err(Error::Parse("empty FROM".into())),
    };
    let def = lookup(&table).ok_or_else(|| Error::UndefinedTable(table.clone()))?;

    finish_aggregate(s, table, None, None, def, params)
}

/// A plain (non-aggregate) SELECT whose source is a top-level JOIN. The join
/// already carries the projected columns, the WHERE and the ORDER BY (the
/// executor's `join_docs` sorts by it), so this just wraps them in a `Select`
/// that projects the join's OUTPUT names in order.
fn plan_join_plain_select(
    s: &pg_query::protobuf::SelectStmt,
    join: JoinSelect,
    _lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<Statement> {
    let columns: Vec<(String, String)> = join
        .columns
        .iter()
        .zip(join_output_keys(&join))
        .map(|((out, _, _), key)| (out.clone(), key))
        .collect();
    let casts = vec![None; columns.len()];
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
    let distinct = plan_distinct(s, &|name| {
        columns
            .iter()
            .find(|(out, _)| out == name)
            .map(|(_, f)| f.clone())
    })?;

    Ok(Statement::Select(Select {
        table: String::new(),
        series: None,
        sub: None,
        windows: Vec::new(),
        join: Some(Box::new(join)),
        columns,
        casts,
        filter: Document::new(),
        residual: None,
        order: Vec::new(),
        limit,
        offset,
        distinct,
    }))
}

/// Plan the inner select of a joined subquery: two tables, one ON equality,
/// projected columns (casts allowed), an optional WHERE equality and one
/// ORDER BY column. Everything else is refused -- a half-supported JOIN
/// quietly returns wrong rows.
/// Parse a JOIN/subquery WHERE into a flat list of predicates. Accepts a single
/// `AExpr` (`=`/`>`/`>=`/`<`/`<=`), a `NOT <boolcol>`, or an `AND` of those.
/// `alias.col` -> (alias, col) from a two-field ColumnRef; None otherwise.
fn qualified_col(n: Option<&pg_query::protobuf::Node>) -> Option<(String, String)> {
    match n.and_then(|x| x.node.as_ref()) {
        Some(N::ColumnRef(c)) if c.fields.len() == 2 => {
            let part = |i: usize| match c.fields[i].node.as_ref() {
                Some(N::String(st)) => Some(st.sval.clone()),
                _ => None,
            };
            Some((part(0)?, part(1)?))
        }
        _ => None,
    }
}

fn join_where_preds(node: &pg_query::protobuf::Node, params: &[Bson]) -> Result<Vec<JoinPred>> {
    use pg_query::protobuf::BoolExprType;
    match node.node.as_ref() {
        // AND of sub-predicates -> flatten each.
        Some(N::BoolExpr(b)) if b.boolop == BoolExprType::AndExpr as i32 => {
            let mut out = Vec::new();
            for arg in &b.args {
                out.extend(join_where_preds(arg, params)?);
            }
            Ok(out)
        }
        // NOT <boolcol>
        Some(N::BoolExpr(b)) if b.boolop == BoolExprType::NotExpr as i32 => {
            let inner = b
                .args
                .first()
                .ok_or_else(|| Error::Unsupported("this subquery WHERE".into()))?;
            let (alias, col) = qualified_col(Some(inner))
                .ok_or_else(|| Error::Unsupported("this subquery WHERE".into()))?;
            Ok(vec![JoinPred {
                alias,
                col,
                op: JoinOp::NotTrue,
                value: Bson::Null,
            }])
        }
        // alias.col <op> const
        Some(N::AExpr(e)) => {
            let op = match operator_name(e) {
                Ok("=") => JoinOp::Eq,
                Ok(">") => JoinOp::Gt,
                Ok(">=") => JoinOp::Ge,
                Ok("<") => JoinOp::Lt,
                Ok("<=") => JoinOp::Le,
                _ => return Err(Error::Unsupported("this subquery WHERE".into())),
            };
            let (alias, col) = qualified_col(e.lexpr.as_deref())
                .ok_or_else(|| Error::Unsupported("this subquery WHERE".into()))?;
            let value = const_value(
                e.rexpr
                    .as_ref()
                    .ok_or_else(|| Error::Parse("no right operand".into()))?,
                params,
            )?;
            Ok(vec![JoinPred {
                alias,
                col,
                op,
                value,
            }])
        }
        _ => Err(Error::Unsupported("this subquery WHERE".into())),
    }
}

fn plan_join_select(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<JoinSelect> {
    use pg_query::protobuf::JoinType;
    // The two sides, the kind, and the ON clause: from a `JoinExpr`, or from
    // the two items of a comma-separated FROM (a CROSS join, no ON).
    let (larg, rarg, left_join, quals) = match s.from_clause.as_slice() {
        [one] => {
            let Some(N::JoinExpr(j)) = one.node.as_ref() else {
                return Err(Error::Unsupported("a subquery without a JOIN".into()));
            };
            let left_join = match JoinType::try_from(j.jointype) {
                Ok(JoinType::JoinLeft) => true,
                Ok(JoinType::JoinInner) => false,
                _ => return Err(Error::Unsupported("this JOIN kind".into())),
            };
            if j.is_natural || !j.using_clause.is_empty() {
                return Err(Error::Unsupported("this JOIN kind".into()));
            }
            (
                j.larg.as_deref(),
                j.rarg.as_deref(),
                left_join,
                j.quals.as_deref(),
            )
        }
        [a, b] => (Some(a), Some(b), false, None),
        _ => return Err(Error::Unsupported("this subquery's FROM".into())),
    };
    #[allow(clippy::type_complexity)]
    let side = |n: Option<&pg_query::protobuf::Node>| -> Result<((String, String), Option<Box<Statement>>)> {
        match n.and_then(|x| x.node.as_ref()) {
            Some(N::RangeVar(r)) => {
                let alias = r
                    .alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .unwrap_or_else(|| r.relname.clone());
                Ok(((relation_name(r), alias), None))
            }
            // A subquery side: `... JOIN (SELECT ...) a`. Plan it recursively;
            // the executor materialises its rows. Carries `""` as its table.
            Some(N::RangeSubselect(rs)) => {
                let inner = match rs.subquery.as_ref().and_then(|q| q.node.as_ref()) {
                    Some(N::SelectStmt(inner)) => inner,
                    _ => return Err(Error::Unsupported("this JOIN subquery".into())),
                };
                let alias = rs
                    .alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .ok_or_else(|| Error::Unsupported("a JOIN subquery without an alias".into()))?;
                let stmt = plan_select(inner, lookup, params)?;
                Ok(((String::new(), alias), Some(Box::new(stmt))))
            }
            _ => Err(Error::Unsupported("this JOIN side".into())),
        }
    };
    let (left, left_sub) = side(larg)?;
    let (right, right_sub) = side(rarg)?;
    // Only a TABLE side needs a catalog lookup; a subquery side is planned.
    if left_sub.is_none() {
        lookup(&left.0).ok_or_else(|| Error::UndefinedTable(left.0.clone()))?;
    }
    if right_sub.is_none() {
        lookup(&right.0).ok_or_else(|| Error::UndefinedTable(right.0.clone()))?;
    }

    // ON a.x = b.y, either order.
    let qualified = |n: Option<&pg_query::protobuf::Node>| -> Option<(String, String)> {
        match n.and_then(|x| x.node.as_ref()) {
            Some(N::ColumnRef(c)) if c.fields.len() == 2 => {
                let part = |i: usize| match c.fields[i].node.as_ref() {
                    Some(N::String(st)) => Some(st.sval.clone()),
                    _ => None,
                };
                Some((part(0)?, part(1)?))
            }
            _ => None,
        }
    };
    let on = match quals.and_then(|q| q.node.as_ref()) {
        Some(N::AExpr(e)) if operator_name(e) == Ok("=") => {
            let l = qualified(e.lexpr.as_deref())
                .ok_or_else(|| Error::Unsupported("this ON clause".into()))?;
            let r = qualified(e.rexpr.as_deref())
                .ok_or_else(|| Error::Unsupported("this ON clause".into()))?;
            Some((l, r))
        }
        // No ON at all: `FROM a, b` or `a CROSS JOIN b`.
        None => None,
        _ => return Err(Error::Unsupported("this ON clause".into())),
    };

    // Projected columns: `alias.col [AS out]`, or a cast chain over one.
    let mut columns = Vec::new();
    let mut exprs = Vec::new();
    for t in &s.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return Err(Error::Unsupported("this subquery target".into()));
        };
        match rt.val.as_ref().and_then(|v| v.node.as_ref()) {
            Some(N::ColumnRef(c)) => {
                let (alias, col) = qualified(rt.val.as_deref())
                    .or_else(|| {
                        // Unqualified: resolve later against either side; carry
                        // an empty alias.
                        column_ref_name(c).map(|n| (String::new(), n))
                    })
                    .ok_or_else(|| Error::Unsupported("this subquery target".into()))?;
                let out = if rt.name.is_empty() {
                    col.clone()
                } else {
                    rt.name.clone()
                };
                columns.push((out, alias, col));
                exprs.push(None);
            }
            // `t.oid::regtype::text AS regtype` -- the cast chain rides
            // beside the column, same as a plain select's. A cast over a
            // CONSTANT (`'t1'::regclass::oid`) is a constant target, below.
            Some(N::TypeCast(tc)) if cast_chain_over_column_qualified(tc).is_ok() => {
                let (col_name, chain) = cast_chain_over_column_qualified(tc)?;
                let out = if rt.name.is_empty() {
                    chain
                        .1
                        .last()
                        .cloned()
                        .unwrap_or_else(|| col_name.1.clone())
                } else {
                    rt.name.clone()
                };
                columns.push((out, col_name.0, col_name.1));
                exprs.push(Some(ColumnExpr::Casts {
                    source: None,
                    chain: chain.1,
                }));
            }
            // `coalesce(a.col, <fallback>) AS out` -- one column argument, the
            // rest constants. A LEFT-JOIN miss makes the column NULL and the
            // fallback stands in (`coalesce(a.fnames, '{}')`).
            Some(N::CoalesceExpr(ce)) => {
                let mut args: Vec<Option<Bson>> = Vec::new();
                let mut column: Option<(String, String)> = None;
                for a in &ce.args {
                    if let Some((alias, col)) = qualified(Some(a)) {
                        if column.is_some() {
                            return Err(Error::Unsupported("a coalesce over two columns".into()));
                        }
                        column = Some((alias, col));
                        args.push(None);
                    } else {
                        // A bare `'{}'` fallback is an empty ARRAY -- the column
                        // it backstops is array-typed in every shape we plan
                        // (`coalesce(array_agg(...), '{}')`). Store it as a real
                        // empty array so a LEFT-JOIN miss encodes as an array,
                        // not the text `"{}"` (which a binary array reader on the
                        // client rejects as a malformed buffer).
                        let cv = match const_value(a, params)? {
                            Bson::String(ref s) if s.trim() == "{}" => Bson::Array(Vec::new()),
                            other => other,
                        };
                        args.push(Some(cv));
                    }
                }
                let (alias, col) =
                    column.ok_or_else(|| Error::Unsupported("a coalesce with no column".into()))?;
                let out = if rt.name.is_empty() {
                    "coalesce".to_string()
                } else {
                    rt.name.clone()
                };
                columns.push((out, alias, col));
                exprs.push(Some(ColumnExpr::Coalesce { args }));
            }
            // A CONSTANT beside the join's columns -- `'t1'::regclass::oid`,
            // `1` -- reads no side at all: the value is fixed at plan time and
            // repeated per row. Anything that is not constant (an expression
            // over a column) stays unsupported.
            Some(_) => {
                let val = rt.val.as_ref().expect("ResTarget has a val");
                let value = const_value(val, params)
                    .map_err(|_| Error::Unsupported("this subquery target".into()))?;
                let out = if rt.name.is_empty() {
                    expression_column_name(val)
                } else {
                    rt.name.clone()
                };
                let result_type = static_type(val, &value);
                columns.push((out, String::new(), String::new()));
                exprs.push(Some(ColumnExpr::Const { value, result_type }));
            }
            None => return Err(Error::Unsupported("this subquery target".into())),
        }
    }

    // WHERE: one predicate, or an AND of several, each `alias.col <op> const`
    // or `NOT alias.col`. Evaluated now against the constants.
    let filter = match s.where_clause.as_deref() {
        None => Vec::new(),
        Some(node) => join_where_preds(node, params)?,
    };
    // Column references this path would resolve LOOSELY -- a bare name both
    // sides have (it took the left one), a qualifier naming neither side,
    // a column the named side lacks -- go to the general planner, whose
    // answers are PostgreSQL's 42702 / 42P01 / 42703.
    {
        let side_def = |sub: &Option<Box<Statement>>, table: &str| -> Option<TableDef> {
            match sub {
                Some(stmt) => sub_plan_def(stmt, lookup).ok(),
                None => lookup(table),
            }
        };
        let ldef = side_def(&left_sub, &left.0);
        let rdef = side_def(&right_sub, &right.0);
        let has = |d: &Option<TableDef>, c: &str| d.as_ref().is_some_and(|d| d.column(c).is_some());
        for (i, (_, alias, col)) in columns.iter().enumerate() {
            if matches!(exprs.get(i), Some(Some(ColumnExpr::Const { .. }))) {
                continue;
            }
            let loose = if alias.is_empty() {
                has(&ldef, col) && has(&rdef, col)
            } else if *alias == left.1 {
                !has(&ldef, col)
            } else if *alias == right.1 {
                !has(&rdef, col)
            } else {
                true
            };
            if loose {
                return Err(Error::Unsupported("this JOIN target".into()));
            }
        }
    }
    // This path applies each predicate to its side BEFORE joining. For the
    // NULLABLE side of a LEFT JOIN that is a different query: PostgreSQL's
    // WHERE runs after the join and drops the NULL-extended rows, while a
    // pre-filter keeps them. Such a query goes to the general join planner.
    if left_join && filter.iter().any(|p| p.alias != left.1) {
        return Err(Error::Unsupported(
            "a WHERE on the nullable side of a LEFT JOIN".into(),
        ));
    }

    // ORDER BY one column.
    let order = match s.sort_clause.len() {
        0 => None,
        1 => {
            let Some(N::SortBy(sb)) = s.sort_clause[0].node.as_ref() else {
                return Err(Error::Unsupported("this subquery ORDER BY".into()));
            };
            let (alias, col) = qualified(sb.node.as_deref())
                .ok_or_else(|| Error::Unsupported("this subquery ORDER BY".into()))?;
            let ascending = !matches!(
                pg_query::protobuf::SortByDir::try_from(sb.sortby_dir),
                Ok(pg_query::protobuf::SortByDir::SortbyDesc)
            );
            Some((alias, col, ascending))
        }
        _ => return Err(Error::Unsupported("this subquery ORDER BY".into())),
    };

    Ok(JoinSelect {
        left,
        right,
        left_join,
        on,
        columns,
        exprs,
        filter,
        order,
        left_sub,
        right_sub,
    })
}

/// A cast chain whose innermost target is a QUALIFIED column.
fn cast_chain_over_column_qualified(
    tc: &pg_query::protobuf::TypeCast,
) -> Result<((String, String), QualifiedColumn)> {
    let ty = tc
        .type_name
        .as_ref()
        .map(type_name_of)
        .ok_or_else(|| Error::Parse("cast with no type".into()))?;
    match tc.arg.as_ref().and_then(|a| a.node.as_ref()) {
        Some(N::ColumnRef(c)) if c.fields.len() == 2 => {
            let part = |i: usize| match c.fields[i].node.as_ref() {
                Some(N::String(st)) => Some(st.sval.clone()),
                _ => None,
            };
            let alias = part(0).ok_or_else(|| Error::Unsupported("this cast target".into()))?;
            let col = part(1).ok_or_else(|| Error::Unsupported("this cast target".into()))?;
            Ok(((alias, col), (String::new(), vec![ty])))
        }
        Some(N::TypeCast(inner)) => {
            let (who, (name, mut chain)) = cast_chain_over_column_qualified(inner)?;
            chain.push(ty);
            Ok((who, (name, chain)))
        }
        _ => Err(Error::Unsupported("a cast over this expression".into())),
    }
}

/// A TableDef standing in for a join's OUTPUT: each projected column with the
/// type its source column (or its last cast) gives it, so the aggregate tail
/// resolves GROUP BY names and types against it unchanged.
/// The OUTPUT schema of an aggregate, derived WITHOUT executing it, so a join
/// with an aggregate subquery side can be typed at Describe time. A `count` /
/// `sum` is `int8`, `min` / `max` keep the input type, `array_agg` is the input
/// type's array; a group key carries its own type from planning.
pub fn aggregate_output_def(agg: &Aggregate) -> Result<TableDef> {
    let mut columns = Vec::new();
    for (out, col) in &agg.select {
        let ty = match col {
            OutputCol::Group(i) => agg.group_by[*i].pg_type.clone(),
            OutputCol::Expr(i) => column_expr_type(&agg.exprs[*i]).to_string(),
            OutputCol::Agg(i) => aggregate_item_type(&agg.items[*i]),
        };
        columns.push(Column::new(out, &ty, false));
    }
    Ok(TableDef::new("", columns))
}

/// The output def of a planned subquery -- a join SIDE (`... JOIN (SELECT ...)
/// a`), a FROM-subquery, or an inlined CTE.
///
/// Every column is built `pk: false` on purpose, by each of the branches
/// below: a materialised subquery's rows are keyed by OUTPUT NAME, and a `pk`
/// column would make `field()` answer `_id` and read the wrong field. The
/// inner query's own primary key is not the subquery's.
pub fn sub_plan_def(
    stmt: &Statement,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<TableDef> {
    match stmt {
        Statement::JoinRows(j) => Ok(j.def.clone()),
        Statement::Aggregate(agg) => aggregate_output_def(agg),
        Statement::Select(sel) => select_output_def(sel, lookup),
        Statement::SetOp(set) => {
            // PostgreSQL takes a set operation's column NAMES and TYPES from
            // its left side, which is what `set_op_fields` reports on the wire.
            sub_plan_def(&set.left, lookup)
        }
        Statement::SelectConstant(sc) => Ok(TableDef::new(
            "",
            sc.columns
                .iter()
                .map(|(name, _, ty, _)| Column::new(name, ty, false))
                .collect(),
        )),
        Statement::ValuesConstant(vc) => Ok(TableDef::new(
            "",
            vc.names
                .iter()
                .zip(&vc.types)
                .map(|(name, ty)| Column::new(name, ty, false))
                .collect(),
        )),
        _ => Err(Error::Unsupported("this subquery shape".into())),
    }
}

/// The output def of a planned plain `SELECT`: what a query reading FROM it as
/// a subquery sees.
pub fn select_output_def(
    sel: &Select,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<TableDef> {
    let source: TableDef = if let Some(sub) = &sel.sub {
        sub.def.clone()
    } else if let Some(join) = &sel.join {
        join_output_def(join, lookup)?
    } else if let Some(series) = &sel.series {
        TableDef::new("", vec![Column::new(&series.column, "int4", false)])
    } else if sel.table.is_empty() {
        TableDef::new("", Vec::new())
    } else {
        lookup(&sel.table).ok_or_else(|| Error::UndefinedTable(sel.table.clone()))?
    };
    // A window's synthetic `__winN` is not in the SOURCE def -- it is
    // computed over it -- so it is added here before the outputs are
    // resolved. Without it `select rn from (select row_number() over (...) as
    // rn from t) s` answered `42703 column "__win0" does not exist`.
    let source = if sel.windows.is_empty() {
        source
    } else {
        let mut d = source;
        for w in &sel.windows {
            d.columns.push(Column::new(&w.field, &w.result_type, false));
        }
        d
    };
    let mut columns = Vec::new();
    for (i, (out, field)) in sel.columns.iter().enumerate() {
        // A computed column carries its own fixed type; `Coalesce` is the one
        // expression that keeps the SOURCE column's type, exactly as
        // `join_output_def` treats it.
        let expr = sel.casts.get(i).and_then(|c| c.as_ref());
        let ty = match expr {
            Some(e) if !matches!(e, ColumnExpr::Coalesce { .. }) => column_expr_type(e).to_string(),
            _ => source
                .columns
                .iter()
                .find(|c| c.field() == *field || c.name == *field)
                .map(|c| c.pg_type.clone())
                .ok_or_else(|| Error::UndefinedColumn(field.clone()))?,
        };
        columns.push(Column::new(out, &ty, false));
    }
    Ok(TableDef::new("", columns))
}

pub fn join_output_def(
    join: &JoinSelect,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<TableDef> {
    let left_def = match &join.left_sub {
        Some(stmt) => sub_plan_def(stmt, lookup)?,
        None => lookup(&join.left.0).ok_or_else(|| Error::UndefinedTable(join.left.0.clone()))?,
    };
    let right_def = match &join.right_sub {
        Some(stmt) => sub_plan_def(stmt, lookup)?,
        None => lookup(&join.right.0).ok_or_else(|| Error::UndefinedTable(join.right.0.clone()))?,
    };
    let mut columns = Vec::new();
    let keys = join_output_keys(join);
    for (i, (_, alias, col)) in join.columns.iter().enumerate() {
        // A coalesce keeps its column's type, so it resolves against the side
        // like a plain column; a cast chain / scalar call / constant uses its
        // fixed type. Only a column read straight through keeps its SOURCE
        // (the RowDescription's `ftable` / `ftablecol`): a cast, a call, a
        // coalesce and a constant are computed, which PostgreSQL reports as
        // table 0 / column 0 (measured 16).
        let expr = join.exprs.get(i).and_then(|e| e.as_ref());
        let (ty, source) = match expr {
            Some(ColumnExpr::Casts { .. })
            | Some(ColumnExpr::Call { .. })
            | Some(ColumnExpr::Const { .. }) => {
                (column_expr_type(expr.expect("some")).to_string(), None)
            }
            _ => {
                let side_def = if *alias == join.left.1 {
                    &left_def
                } else if *alias == join.right.1 {
                    &right_def
                } else if left_def.column(col).is_some() {
                    &left_def
                } else {
                    &right_def
                };
                let c = side_def
                    .column(col)
                    .ok_or_else(|| Error::UndefinedColumn(col.clone()))?;
                (
                    c.pg_type.clone(),
                    expr.is_none().then_some(c.source).flatten(),
                )
            }
        };
        let mut column = Column::new(&keys[i], &ty, false);
        column.source = source;
        columns.push(column);
    }
    Ok(TableDef::new("", columns))
}

/// The key each of a join's output columns is stored under in its row
/// documents, parallel to `columns`. Two outputs may share a NAME (`select
/// 't1'::regclass::oid, 't2'::regclass::oid from t1, t2` has two `oid`s, and
/// PostgreSQL keeps both); a repeat gets a suffix no identifier carries so the
/// second does not overwrite the first.
pub fn join_output_keys(join: &JoinSelect) -> Vec<String> {
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    join.columns
        .iter()
        .map(|(out, _, _)| {
            let n = seen.entry(out.as_str()).or_insert(0);
            *n += 1;
            if *n == 1 {
                out.clone()
            } else {
                format!("{out}\u{1}{n}")
            }
        })
        .collect()
}

/// The shared tail of `plan_aggregate`, over whichever SOURCE the FROM named:
/// a table, or a joined subquery whose output columns stand in for one.
fn finish_aggregate(
    s: &pg_query::protobuf::SelectStmt,
    table: String,
    join: Option<Box<JoinSelect>>,
    sub: Option<Box<SubSource>>,
    def: TableDef,
    params: &[Bson],
) -> Result<Statement> {
    // GROUP BY keys, in declared order. A bare integer is a POSITION in the
    // select list (`GROUP BY 1, 2`); anything but a column reference is an
    // expression over the row, evaluated per row and matched against the
    // targets by structure -- PostgreSQL's `equal()`, which ignores where in
    // the query text each was written.
    let fields: Vec<RowField> = def
        .columns
        .iter()
        .map(|c| (c.name.clone(), c.field(), c.pg_type.clone()))
        .collect();
    let mut sample = Document::new();
    for c in &def.columns {
        sample.insert(c.field(), sample_value_for_type(&c.pg_type));
    }
    let mut group_by: Vec<GroupKey> = Vec::new();
    let mut group_prints: Vec<String> = Vec::new();
    let mut elements: Vec<GroupElement> = Vec::new();
    // `push_key` resolves one GROUP BY element to a key and returns its index,
    // so a member of a GROUPING SETS / ROLLUP / CUBE list resolves exactly as
    // a top-level key does -- including `GROUP BY 2` positions and output
    // aliases. Duplicated resolution here would diverge the moment either
    // gained a case.
    let push_key = |node: &pg_query::protobuf::Node,
                    group_by: &mut Vec<GroupKey>,
                    group_prints: &mut Vec<String>|
     -> Result<usize> {
        let (key, print) = resolve_group_key(node, &def, &fields, params, &sample, s)?;
        // A key named twice is ONE key: `rollup (a, a)` and a select list
        // mentioning `a` must agree on which index they mean.
        if let Some(i) = group_prints.iter().position(|p| *p == print) {
            return Ok(i);
        }
        group_prints.push(print);
        group_by.push(key);
        Ok(group_by.len() - 1)
    };
    for g in &s.group_clause {
        // `GROUPING SETS` / `ROLLUP` / `CUBE` arrive as a `GroupingSet` node
        // rather than an expression. Before this they fell through to the
        // expression arm, failed to resolve as a column, and the statement
        // died with `42803 column "a" must appear in the GROUP BY clause` --
        // an error blaming the user's own query for a clause this server had
        // simply dropped.
        if let Some(N::GroupingSet(gs)) = g.node.as_ref() {
            use pg_query::protobuf::GroupingSetKind as K;
            let members = |content: &[pg_query::protobuf::Node],
                           group_by: &mut Vec<GroupKey>,
                           group_prints: &mut Vec<String>|
             -> Result<Vec<usize>> {
                content
                    .iter()
                    .map(|n| push_key(n, group_by, group_prints))
                    .collect()
            };
            match K::try_from(gs.kind) {
                Ok(K::GroupingSetEmpty) => elements.push(GroupElement::Sets(vec![Vec::new()])),
                Ok(K::GroupingSetRollup) => {
                    let keys = members(&gs.content, &mut group_by, &mut group_prints)?;
                    elements.push(GroupElement::Rollup(keys));
                }
                Ok(K::GroupingSetCube) => {
                    let keys = members(&gs.content, &mut group_by, &mut group_prints)?;
                    elements.push(GroupElement::Cube(keys));
                }
                Ok(K::GroupingSetSets) => {
                    let mut sets = Vec::new();
                    for item in &gs.content {
                        match item.node.as_ref() {
                            // A set of SEVERAL keys, `(a, b)`, parses as a
                            // RowExpr -- NOT as a nested GroupingSet, which is
                            // what this first assumed. The wrong guess made
                            // `grouping sets ((a,b),())` fail with the very
                            // 42803 this change exists to remove, because `b`
                            // never became a key.
                            Some(N::RowExpr(r)) => {
                                sets.push(members(&r.args, &mut group_by, &mut group_prints)?);
                            }
                            // A nested construct, e.g. `grouping sets (rollup(a))`.
                            Some(N::GroupingSet(inner)) => {
                                sets.push(members(
                                    &inner.content,
                                    &mut group_by,
                                    &mut group_prints,
                                )?);
                            }
                            // `(a)` -- a bare key is a one-key set.
                            Some(_) => {
                                sets.push(vec![push_key(item, &mut group_by, &mut group_prints)?]);
                            }
                            None => sets.push(Vec::new()),
                        }
                    }
                    elements.push(GroupElement::Sets(sets));
                }
                _ => return Err(Error::Unsupported("this GROUP BY construct".into())),
            }
            continue;
        }
        let node = match g.node.as_ref() {
            Some(N::AConst(c)) if matches!(c.val, Some(a_const::Val::Ival(_))) => {
                let Some(a_const::Val::Ival(i)) = &c.val else {
                    unreachable!()
                };
                let position = usize::try_from(i.ival).unwrap_or(0);
                let target = position
                    .checked_sub(1)
                    .and_then(|k| s.target_list.get(k))
                    .ok_or_else(|| {
                        Error::InvalidColumnReference(format!(
                            "GROUP BY position {} is not in select list",
                            i.ival
                        ))
                    })?;
                match target.node.as_ref() {
                    Some(N::ResTarget(rt)) => rt.val.as_deref(),
                    _ => None,
                }
                .ok_or_else(|| Error::Unsupported("this GROUP BY position".into()))?
            }
            Some(_) => g,
            None => return Err(Error::Unsupported("an empty GROUP BY key".into())),
        };
        let index = push_key(node, &mut group_by, &mut group_prints)?;
        elements.push(GroupElement::Key(index));
    }
    let grouping_sets = expand_grouping_sets(&elements);

    let mut items: Vec<AggItem> = Vec::new();
    let mut exprs: Vec<ColumnExpr> = Vec::new();
    let mut select: Vec<(String, OutputCol)> = Vec::new();
    for t in &s.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return Err(Error::Unsupported("this target".into()));
        };
        // `GROUPING(col)` reports WHICH set produced a row, which means
        // carrying the producing set through the group -- not done. Named
        // explicitly so the refusal says what is missing: the generic arm
        // below answers `this target is not supported yet`, which tells a
        // reader nothing about which part of their query to change.
        if let Some(N::GroupingFunc(_)) = rt.val.as_ref().and_then(|v| v.node.as_ref()) {
            return Err(Error::Unsupported("the GROUPING function".into()));
        }
        match rt.val.as_ref().and_then(|v| v.node.as_ref()) {
            Some(N::FuncCall(f)) if is_aggregate_call(f) => {
                let name = func_name(f).unwrap_or_default();
                let out = if rt.name.is_empty() {
                    name.clone()
                } else {
                    rt.name.clone()
                };
                let item = plan_aggregate_item(f, &def, items.len(), params, out.clone())?;
                select.push((out, OutputCol::Agg(items.len())));
                items.push(item);
            }
            Some(N::ColumnRef(c)) => {
                // The LAST name part: `c.a` is the column `a` qualified by its
                // relation. The first part named the qualifier, so every
                // qualified grouped column was a 42803 naming the ALIAS.
                let col =
                    column_ref_name(c).ok_or_else(|| Error::Unsupported("this target".into()))?;
                // A bare column alongside an aggregate must be grouped by --
                // PostgreSQL errors 42803 otherwise, and so do we.
                let idx = group_by
                    .iter()
                    .position(|k| k.expr.is_none() && k.name == col)
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
            // Any other expression must BE a GROUP BY key, matched by
            // structure; one that merely references a grouped column is
            // more than this slice lowers.
            Some(_) => {
                let val = rt.val.as_deref().expect("ResTarget has a val");
                let print = node_print(val);
                // An expression OVER the grouped values -- `count(*) + 1`,
                // `coalesce(sum(n), 0)`, `g + count(*)`. Its aggregates become
                // ordinary items and the arithmetic runs over their results.
                if group_prints.iter().all(|p| *p != print) {
                    let mut rewritten = val.clone();
                    let mut slots: Vec<RowField> = Vec::new();
                    if extract_aggregates(&mut rewritten, &def, &mut items, &mut slots, params)? {
                        for (i, key) in group_by.iter().enumerate() {
                            let _ = i;
                            if key.expr.is_none() {
                                slots.push((
                                    key.name.clone(),
                                    key.field.clone(),
                                    key.pg_type.clone(),
                                ));
                            }
                        }
                        let mut sample = Document::new();
                        for (_, field, ty) in &slots {
                            sample.insert(field.clone(), sample_for_type(ty));
                        }
                        let expr = row_column_expr(&rewritten, &slots, params, &sample)?;
                        let out = if rt.name.is_empty() {
                            expression_column_name(val)
                        } else {
                            rt.name.clone()
                        };
                        select.push((out, OutputCol::Expr(exprs.len())));
                        exprs.push(expr);
                        continue;
                    }
                }
                let idx = group_prints
                    .iter()
                    .position(|p| *p == print)
                    .ok_or_else(|| Error::Unsupported("this target".into()))?;
                let out = if rt.name.is_empty() {
                    group_by[idx].name.clone()
                } else {
                    rt.name.clone()
                };
                select.push((out, OutputCol::Group(idx)));
            }
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
        let sort_node = sb
            .node
            .as_ref()
            .ok_or_else(|| Error::Unsupported("this ORDER BY item".into()))?;
        let group_index = match sort_node.node.as_ref() {
            // An OUTPUT alias first (`... as n ... order by n`), then a
            // grouped source column -- PostgreSQL's order for ORDER BY.
            Some(N::ColumnRef(c)) => {
                let col = column_ref_name(c)
                    .ok_or_else(|| Error::Unsupported("this ORDER BY expression".into()))?;
                match select.iter().find(|(out, _)| *out == col).map(|(_, o)| *o) {
                    Some(OutputCol::Group(i)) => i,
                    Some(OutputCol::Agg(_) | OutputCol::Expr(_)) => {
                        return Err(Error::Unsupported(
                            "ORDER BY over an aggregate result".into(),
                        ))
                    }
                    None => group_by
                        .iter()
                        .position(|k| k.expr.is_none() && k.name == col)
                        .ok_or_else(|| {
                            Error::Unsupported("ORDER BY over an aggregate result".into())
                        })?,
                }
            }
            // `ORDER BY 2` is the second OUTPUT column.
            Some(N::AConst(c)) if matches!(c.val, Some(a_const::Val::Ival(_))) => {
                let Some(a_const::Val::Ival(i)) = &c.val else {
                    unreachable!()
                };
                let col = usize::try_from(i.ival)
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|k| select.get(k))
                    .ok_or_else(|| {
                        Error::InvalidColumnReference(format!(
                            "ORDER BY position {} is not in select list",
                            i.ival
                        ))
                    })?;
                match col.1 {
                    OutputCol::Group(i) => i,
                    OutputCol::Agg(_) | OutputCol::Expr(_) => {
                        return Err(Error::Unsupported(
                            "ORDER BY over an aggregate result".into(),
                        ))
                    }
                }
            }
            Some(_) => {
                let print = node_print(sort_node);
                group_prints
                    .iter()
                    .position(|p| *p == print)
                    .ok_or_else(|| Error::Unsupported("ORDER BY over an expression".into()))?
            }
            None => return Err(Error::Unsupported("this ORDER BY item".into())),
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
        order.push(AggOrderKey {
            group_index,
            ascending,
            nulls,
        });
    }

    // HAVING may name an aggregate the SELECT list does not, so it is planned
    // here, while `items` can still grow: such an aggregate is computed for
    // the test and never projected.
    let having = match s.having_clause.as_deref() {
        None => None,
        Some(node) => Some(plan_having(node, &def, &group_by, &mut items, params)?),
    };

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
        sub,
        join,
        table,
        group_by,
        grouping_sets,
        items,
        select,
        filter,
        order,
        limit,
        offset,
        having,
        exprs,
        distinct: aggregate_distinct(s)?,
    }))
}

/// A parse node printed WITHOUT its source positions, so two spellings of the
/// same expression compare equal wherever they sat in the query text --
/// PostgreSQL's `equal()` for the purpose of matching a target to a GROUP BY
/// key.
fn node_print(node: &pg_query::protobuf::Node) -> String {
    static LOCATION: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    LOCATION
        .get_or_init(|| regex::Regex::new(r"location: -?\d+").expect("a fixed pattern"))
        .replace_all(&format!("{node:?}"), "location: _")
        .into_owned()
}

/// Is this call one of the aggregates this planner lowers?
fn is_aggregate_call(f: &pg_query::protobuf::FuncCall) -> bool {
    f.agg_star
        || func_name(f)
            .as_deref()
            .is_some_and(|n| aggregate_func(n, f.agg_within_group).is_some())
}

/// The session functions a connecting client asks for.
///
/// The version string mirrors the Python server's shape so both identify as
/// SecantusDB -- the conformance gauges refuse to run against a daemon whose
/// `version()` does not name it, precisely so a stray real PostgreSQL cannot
/// inflate the numbers.
pub(crate) fn session_function(name: &str) -> Option<Bson> {
    Some(match name {
        "version" => Bson::String(format!(
            "PostgreSQL 15.0 (SecantusDB) on {}, compiled by rust",
            std::env::consts::ARCH
        )),
        "current_schema" => Bson::String("public".into()),
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
/// Whether a DEFAULT expression calls a function whose value changes from row
/// to row (`now()` and its siblings, `current_timestamp`). The planner stores a
/// DEFAULT as one evaluated value, which such a function cannot be.
/// The type of a keyword function -- `CURRENT_DATE`, `LOCALTIME`, `USER`.
fn sql_value_function_type(svf: &pg_query::protobuf::SqlValueFunction) -> &'static str {
    use pg_query::protobuf::SqlValueFunctionOp as Op;
    match Op::try_from(svf.op).unwrap_or(Op::SqlvalueFunctionOpUndefined) {
        Op::SvfopCurrentDate => "date",
        Op::SvfopCurrentTime | Op::SvfopCurrentTimeN => "timetz",
        Op::SvfopCurrentTimestamp | Op::SvfopCurrentTimestampN => "timestamptz",
        Op::SvfopLocaltime | Op::SvfopLocaltimeN => "time",
        Op::SvfopLocaltimestamp | Op::SvfopLocaltimestampN => "timestamp",
        _ => "name",
    }
}

/// A keyword function's value. The date/time ones are the statement's
/// instant cast to their type, which renders it in the session zone as
/// PostgreSQL does; the role ones are the session's.
fn sql_value_function(svf: &pg_query::protobuf::SqlValueFunction) -> Result<Bson> {
    use pg_query::protobuf::SqlValueFunctionOp as Op;
    let op = Op::try_from(svf.op).unwrap_or(Op::SqlvalueFunctionOpUndefined);
    Ok(match op {
        Op::SvfopCurrentUser | Op::SvfopUser | Op::SvfopSessionUser | Op::SvfopCurrentRole => {
            session_user().map_or(Bson::Null, Bson::String)
        }
        Op::SvfopCurrentCatalog => Bson::String(session_database()),
        Op::SvfopCurrentSchema => Bson::String("public".into()),
        Op::SvfopCurrentTimestamp | Op::SvfopCurrentTimestampN => scalar::now_value(),
        Op::SqlvalueFunctionOpUndefined => {
            return Err(Error::Unsupported("this keyword function".into()))
        }
        _ => timestamptz_as_local(&scalar::now_value(), sql_value_function_type(svf))?
            .ok_or_else(|| Error::Internal("the current instant did not render".into()))?,
    })
}

/// A timestamptz as the session zone's wall clock (`2026-09-29
/// 17:26:09.52+02`), cut to the part `target` names and read as that type:
/// `date`, `timestamp`, `time` or `timetz`. `None` when the value is not an
/// instant.
fn timestamptz_as_local(value: &Bson, target: &str) -> Result<Option<Bson>> {
    let Some(text) = timestamptz_value_text(value, &session_timezone()) else {
        return Ok(None);
    };
    let (date, time_tz) = text.split_once(' ').unwrap_or((text.as_str(), ""));
    let offset_at = time_tz.find(['+', '-']).unwrap_or(time_tz.len());
    let time = &time_tz[..offset_at];
    let part = match target {
        "date" => date.to_string(),
        "timestamp" => format!("{date} {time}"),
        "time" => time.to_string(),
        _ => time_tz.to_string(),
    };
    cast_value(Bson::String(part), target).map(Some)
}

/// A column's DEFAULT as an expression node: its sequence's `nextval`, its
/// expression default, its literal, or NULL.
fn column_default_node(column: &Column) -> Result<pg_query::protobuf::Node> {
    let text = if let Some(seq) = column.sequence.as_deref() {
        format!("nextval('{}')", seq.replace('\'', "''"))
    } else if let Some(expr) = column.default_expr() {
        expr.to_string()
    } else {
        return Ok(param_less_const(
            column.default.clone().unwrap_or(Bson::Null),
        ));
    };
    let N::SelectStmt(sel) = parse_one(&format!("SELECT {text}"))? else {
        return Err(Error::Internal("a column default did not parse".into()));
    };
    sel.target_list
        .first()
        .and_then(|t| match t.node.as_ref() {
            Some(N::ResTarget(r)) => r.val.as_deref().cloned(),
            _ => None,
        })
        .ok_or_else(|| Error::Internal("a column default did not parse".into()))
}

/// A literal value as an expression node, for a stored literal default.
fn param_less_const(value: Bson) -> pg_query::protobuf::Node {
    use pg_query::protobuf::a_const::Val;
    let val = match &value {
        Bson::Null => None,
        Bson::Int32(v) => Some(Val::Ival(pg_query::protobuf::Integer { ival: *v })),
        Bson::Boolean(b) => Some(Val::Boolval(pg_query::protobuf::Boolean { boolval: *b })),
        Bson::String(s) => Some(Val::Sval(pg_query::protobuf::String { sval: s.clone() })),
        other => Some(Val::Sval(pg_query::protobuf::String {
            sval: numeric::numeric_text(other).unwrap_or_else(|| other.to_string()),
        })),
    };
    pg_query::protobuf::Node {
        node: Some(N::AConst(pg_query::protobuf::AConst {
            isnull: val.is_none(),
            location: -1,
            val,
        })),
    }
}

/// Is a DEFAULT expression one whose value must be computed per row rather
/// than folded once? A time function, a sequence function, a random one --
/// anywhere in the expression, not only at its top.
fn default_is_volatile(node: &pg_query::protobuf::Node) -> bool {
    let mut probe = node.clone();
    let mut found = false;
    let _ = walk_expr(&mut probe, &mut |n| {
        match n.node.as_ref() {
            Some(N::FuncCall(f)) => {
                if func_name(f).is_some_and(|name| {
                    VOLATILE_FUNCTIONS.contains(&name.as_str())
                        || matches!(
                            name.as_str(),
                            "now" | "transaction_timestamp" | "statement_timestamp"
                        )
                }) {
                    found = true;
                }
            }
            Some(N::SqlvalueFunction(_)) => found = true,
            _ => {}
        }
        Ok(())
    });
    found
}

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
                // A multidimensional array keeps the SAME array type -- a 2-D
                // int array is `int4[]` (oid 1007), not `int4[][]` (which is no
                // type name at all and fell back to varchar).
                Some(t) if t.ends_with("[]") => t,
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
        Some(N::FuncCall(f))
            if func_name(f)
                .is_some_and(|n| correlated::SEQUENCE_FUNCTIONS.contains(&n.as_str())) =>
        {
            "int8".to_string()
        }
        Some(N::FuncCall(f))
            if func_name(f)
                .and_then(|n| correlated::user_function(&n, f.args.len()))
                .is_some() =>
        {
            func_name(f)
                .and_then(|n| correlated::user_function(&n, f.args.len()))
                .map(|u| u.return_type)
                .unwrap_or_default()
        }
        Some(N::FuncCall(f)) if correlated::correlated_type(f).is_some() => {
            correlated::correlated_type(f).expect("checked")
        }
        Some(N::FuncCall(f)) if func_name(f).as_deref() == Some("to_regtype") => {
            "regtype".to_string()
        }
        // `array_cat` / `array_append` / `array_prepend` / `array_remove` /
        // `array_replace` answer whichever ARGUMENT is the array, so the type
        // has to come from the call. Without this they fell back to the
        // value, which is silent until the value is NULL -- and at DESCRIBE
        // time every value is.
        Some(N::FuncCall(f))
            if matches!(
                func_name(f).as_deref(),
                Some(
                    "array_cat"
                        | "array_append"
                        | "array_prepend"
                        | "array_remove"
                        | "array_replace"
                )
            ) =>
        {
            f.args
                .iter()
                .map(|a| static_type(a, &Bson::Null))
                .find(|t| t.ends_with("[]"))
                .unwrap_or_else(|| inferred_type(value).to_string())
        }
        // The datetime functions, typed from their arguments' types.
        Some(N::FuncCall(f))
            if func_name(f)
                .as_deref()
                .is_some_and(|n| datetime_result_type(f, n).is_some()) =>
        {
            datetime_result_type(f, &func_name(f).unwrap_or_default())
                .unwrap_or_else(|| "text".into())
        }
        // The full-text functions each have one result type.
        Some(N::FuncCall(f)) if func_name(f).as_deref().and_then(fts::result_type).is_some() => {
            func_name(f)
                .as_deref()
                .and_then(fts::result_type)
                .unwrap_or("text")
                .to_string()
        }
        // The array functions with a FIXED result type -- `array_length` is
        // `int4` whatever it is handed, `string_to_array` always `text[]`.
        Some(N::FuncCall(f))
            if func_name(f)
                .as_deref()
                .and_then(arrays::static_result_type)
                .is_some() =>
        {
            func_name(f)
                .as_deref()
                .and_then(arrays::static_result_type)
                .unwrap_or("text")
                .to_string()
        }
        // `array_fill(v, ...)` is an array OF v's type.
        Some(N::FuncCall(f)) if func_name(f).as_deref() == Some("array_fill") => {
            match f.args.first().map(|a| static_type(a, &Bson::Null)) {
                Some(t) if !t.ends_with("[]") && t != "text" => format!("{t}[]"),
                _ => inferred_type(value).to_string(),
            }
        }
        // `now()` and its siblings are `timestamptz`. The value alone cannot
        // say so -- a timestamptz INSTANT is stored exactly like a naive
        // `timestamp` -- so `now()::text` rendered the wall clock with no zone
        // suffix where PostgreSQL renders `... +00` in the session zone.
        Some(N::FuncCall(f))
            if matches!(
                func_name(f).as_deref(),
                Some("now" | "transaction_timestamp" | "statement_timestamp" | "clock_timestamp")
            ) =>
        {
            "timestamptz".to_string()
        }
        Some(N::SqlvalueFunction(svf)) => sql_value_function_type(svf).to_string(),
        // `int4range(1,5)` is an `int4range`, not the text it renders as.
        Some(N::FuncCall(f)) if range_constructor_type(f).is_some() => {
            range_constructor_type(f).unwrap_or_default()
        }
        // `lower(int4range(1,5))` is an `int4`; `isempty(...)` a bool.
        Some(N::FuncCall(f)) if range_accessor(f).is_some() => {
            let (element, _, _) = range_accessor(f).unwrap_or_default();
            range::accessor_result_type(&func_name(f).unwrap_or_default(), &element)
        }
        Some(N::BoolExpr(_)) | Some(N::NullTest(_)) => "bool".to_string(),
        // These pick one of their arguments, so they report its type.
        Some(N::RowExpr(_)) => "record".to_string(),
        // `(expr).field` reports the SELECTED field's declared type, taken from
        // the source composite's field list. An anonymous record (or an unknown
        // field) falls back to the value's inferred type.
        Some(N::AIndirection(ind))
            if !ind.indirection.is_empty()
                && ind
                    .indirection
                    .iter()
                    .all(|i| matches!(i.node.as_ref(), Some(N::AIndices(_)))) =>
        {
            // A subscript over an array: an ELEMENT reference drops the `[]`,
            // a SLICE keeps it. `ia[1]` is `int4` where `ia[1:2]` is `int4[]`,
            // and the value alone cannot tell the two apart once a single-row
            // slice has been taken.
            let base = ind
                .arg
                .as_deref()
                .map(|a| static_type(a, &Bson::Null))
                .unwrap_or_default();
            let any_slice = ind
                .indirection
                .iter()
                .any(|i| matches!(i.node.as_ref(), Some(N::AIndices(idx)) if idx.is_slice));
            match base.strip_suffix("[]") {
                Some(_) if any_slice => base,
                Some(element) => element.to_string(),
                None => inferred_type(value).to_string(),
            }
        }
        Some(N::AIndirection(ind)) => {
            let field = ind
                .indirection
                .first()
                .and_then(|n| n.node.as_ref())
                .and_then(|n| match n {
                    N::String(s) => Some(s.sval.as_str()),
                    _ => None,
                });
            let field_type = ind.arg.as_ref().and_then(|arg| {
                let ty = static_type(arg, &Bson::Null);
                user_composite(&ty).and_then(|(_, fields)| {
                    field.and_then(|f| fields.iter().find(|(n, _)| n == f).map(|(_, t)| t.clone()))
                })
            });
            field_type.unwrap_or_else(|| inferred_type(value).to_string())
        }
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
            // The full-text operators.
            if matches!(op, "@@" | "@@@" | "@?") {
                return "bool".to_string();
            }
            if op == "!!" && e.lexpr.is_none() {
                return "tsquery".to_string();
            }
            if matches!(op, "||" | "&&" | "<->") {
                if let Some(l) = e.lexpr.as_deref() {
                    let t = static_type(l, &Bson::Null);
                    if t == "tsvector" || t == "tsquery" {
                        return t;
                    }
                }
            }
            // The hstore operators type from the operator and, for `->`,
            // from whether the RIGHT operand is a key or a key list.
            if static_hstore_operand(e.lexpr.as_deref(), &Bson::Null) {
                if let Some(t) = static_hstore_result(op, e.rexpr.as_deref()) {
                    return t;
                }
            }
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
            // The array containment operators answer a boolean. Reached only
            // when the json branch above did not claim the operator, so an
            // array operand is what is left.
            if matches!(op, "@>" | "<@" | "&&") {
                let side = |n: Option<&pg_query::protobuf::Node>| {
                    n.map(|node| static_type(node, &Bson::Null))
                };
                if side(e.lexpr.as_deref()).is_some_and(|t| t.ends_with("[]"))
                    || side(e.rexpr.as_deref()).is_some_and(|t| t.ends_with("[]"))
                {
                    return "bool".to_string();
                }
            }
            match op {
                // `||` is text concatenation EXCEPT bytea||bytea, which is
                // bytea: type it from the operands so a bare (uncast) concat
                // reports oid 17 rather than 25.
                "||" => {
                    let side = |n: Option<&pg_query::protobuf::Node>| {
                        n.map(|node| static_type(node, &Bson::Null))
                    };
                    let (l, r) = (side(e.lexpr.as_deref()), side(e.rexpr.as_deref()));
                    // An array beside anything is `array_cat` / `array_append`
                    // / `array_prepend`, typed as the array.
                    if let Some(t) = l.iter().chain(r.iter()).find(|t| t.ends_with("[]")) {
                        t.clone()
                    } else if l.as_deref() == Some("bytea") || r.as_deref() == Some("bytea") {
                        "bytea".to_string()
                    } else {
                        "text".to_string()
                    }
                }
                "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=" => "bool".to_string(),
                // Arithmetic keeps the value's type when it computed one, and
                // falls back to int4 for the NULL-placeholder case, which is
                // what PostgreSQL reports for `1 + NULL`.
                _ => {
                    // Datetime arithmetic (`date + int`, `timestamp + interval`,
                    // `interval * n`, ...) types from the OPERANDS, so the result
                    // column is described correctly even at DESCRIBE time when
                    // every value is NULL -- which is when psycopg picks its
                    // result loader. Without this a `timestamp + interval` was
                    // described as `int4`/`text` and the client decoded it wrong.
                    // Unary minus keeps an interval an interval.
                    if e.lexpr.is_none() && matches!(op, "-" | "+") {
                        if let Some(r) = e.rexpr.as_deref() {
                            if static_type(r, &Bson::Null) == "interval" {
                                return "interval".to_string();
                            }
                        }
                    }
                    if let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) {
                        let lt = static_type(l, &Bson::Null);
                        let rt = static_type(r, &Bson::Null);
                        if let Some(t) = datetime_arith_type(op, &lt, &rt) {
                            return t.to_string();
                        }
                        // Numeric arithmetic types from the operands too:
                        // anything beside a `numeric` is `numeric`, and a
                        // float wins over it (`wider_numeric`'s ladder).
                        if matches!(op, "+" | "-" | "*" | "/") {
                            if let Some(t) = wider_numeric(&lt, &rt) {
                                if t == "numeric" || t == "float8" || t == "float4" {
                                    return t;
                                }
                                // Integer arithmetic with no value to look at
                                // (DESCRIBE time) types from the operands too:
                                // `$1::int8 + $2::int8` is int8 on PostgreSQL 16,
                                // not the int4 the NULL placeholder implied.
                                if *value == Bson::Null {
                                    return t;
                                }
                            }
                        }
                    }
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
/// The result type of a datetime `+`/`-`/`*`/`/` from its two operand types, or
/// `None` when this is not a datetime-arithmetic combination.
///
/// Static, computed from the operand TYPES (not values), so it holds at DESCRIBE
/// time when every value is NULL -- which is exactly when the type matters,
/// because psycopg picks its result loader from the described column type. Every
/// operand-type spelling libpg_query can produce (`timestamptz` and `timestamp
/// with time zone`, etc.) is folded first.
fn datetime_arith_type(op: &str, lt: &str, rt: &str) -> Option<&'static str> {
    let norm = |t: &str| -> &'static str {
        match t {
            "int2" | "int4" | "int8" | "smallint" | "integer" | "int" | "bigint" => "int",
            "numeric" | "decimal" | "float4" | "float8" | "real" | "double precision"
            | "double" => "num",
            "date" => "date",
            "time" | "time without time zone" => "time",
            "timetz" | "time with time zone" => "timetz",
            "timestamp" | "timestamp without time zone" => "timestamp",
            "timestamptz" | "timestamp with time zone" => "timestamptz",
            "interval" => "interval",
            _ => "other",
        }
    };
    let (l, r) = (norm(lt), norm(rt));
    let num = |x: &str| x == "int" || x == "num";
    match op {
        "+" => match (l, r) {
            ("date", "int") | ("int", "date") => Some("date"),
            ("date", "interval") | ("interval", "date") => Some("timestamp"),
            ("timestamp", "interval") | ("interval", "timestamp") => Some("timestamp"),
            ("timestamptz", "interval") | ("interval", "timestamptz") => Some("timestamptz"),
            ("time", "interval") | ("interval", "time") => Some("time"),
            ("timetz", "interval") | ("interval", "timetz") => Some("timetz"),
            ("interval", "interval") => Some("interval"),
            _ => None,
        },
        "-" => match (l, r) {
            ("date", "int") => Some("date"),
            ("date", "date") => Some("int4"),
            ("date", "interval") => Some("timestamp"),
            ("timestamp", "interval") => Some("timestamp"),
            ("timestamp", "timestamp") => Some("interval"),
            ("timestamptz", "interval") => Some("timestamptz"),
            ("timestamptz", "timestamptz") => Some("interval"),
            ("time", "interval") => Some("time"),
            ("time", "time") => Some("interval"),
            ("timetz", "interval") => Some("timetz"),
            ("interval", "interval") => Some("interval"),
            _ => None,
        },
        "*" => match (l, r) {
            ("interval", x) | (x, "interval") if num(x) => Some("interval"),
            _ => None,
        },
        "/" => match (l, r) {
            ("interval", x) if num(x) => Some("interval"),
            _ => None,
        },
        _ => None,
    }
}

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
        Bson::Document(d) if d.contains_key(WIDE_NUMERIC_KEY) => "numeric",
        Bson::Document(d) if d.len() == 1 && d.contains_key(REGCLASS_KEY) => "regclass",
        // A MULTIDIMENSIONAL array is the same array type as its elements --
        // `int4[]` (oid 1007), never `int4[][]`, which is no type at all.
        // Recursing is what makes that so: reading only the first element's
        // kind typed `{{1,2},{3,4}}` as `text[]`, and a binary-format client
        // then refused the int rows it was handed as `_text`.
        // `items.first()` was the element kind until 2026-09-29: a leading
        // NULL then typed `{NULL,1}` as `text[]`, and a binary-format client
        // decoded the integer beside it as NULL. The first NON-NULL element
        // is the one that can say.
        Bson::Array(items) => match items.iter().find(|i| *i != &Bson::Null).or(items.first()) {
            Some(Bson::Int32(_)) | None => "int4[]",
            Some(Bson::Int64(_)) => "int8[]",
            Some(Bson::Double(_)) => "float8[]",
            Some(Bson::Decimal128(_)) => "numeric[]",
            Some(Bson::Boolean(_)) => "bool[]",
            Some(Bson::Binary(_)) => "bytea[]",
            Some(inner @ Bson::Array(_)) => inferred_type(inner),
            Some(other) if geo::is_box(other) => "box[]",
            _ => "text[]",
        },
        Bson::Boolean(_) => "bool",
        // A bytea value is stored as BSON binary. Typing it as text made a
        // BINARY-format `set_byte(...)` result hit the text encoder ("cannot
        // send this value as a binary text") while the same query in text
        // format rendered `\x..` under oid 17.
        Bson::Binary(_) => "bytea",
        other if geo::is_box(other) => "box",
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
    // A REGTYPE value, not its display text: `pg_typeof(x)::oid` reads the
    // oid and `::text` the name, and a bare read renders the name -- the
    // string alone could only do the last. A type the catalog cannot number
    // (`unknown`, mostly) still answers its name as text.
    Ok(
        match pgtypes::oid_of_name(&internal).or_else(|| user_type_oid(&internal)) {
            Some(oid) => regtype_value(oid),
            None => Bson::String(display_type(&internal)),
        },
    )
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

/// A target that is `generate_series(...)`, possibly wrapped in one or more
/// casts (`generate_series(1, 2)::int4`, `...::int4::text`).
///
/// Returns the underlying `FuncCall` plus the cast chain, innermost-first, so
/// `apply_column_expr(ColumnExpr::Casts { chain, .. })` reproduces PostgreSQL's
/// left-to-right cast application over each generated value. `None` when the
/// node is not a series (or is a series beside some other expression a cast
/// can't strip).
fn series_target_call(
    node: &pg_query::protobuf::Node,
) -> Option<(&pg_query::protobuf::FuncCall, Vec<String>)> {
    match node.node.as_ref() {
        Some(N::FuncCall(f)) if func_name(f).as_deref() == Some("generate_series") => {
            Some((f, Vec::new()))
        }
        Some(N::TypeCast(tc)) => {
            let ty = tc.type_name.as_ref().map(type_name_of)?;
            let arg = tc.arg.as_ref()?;
            let (f, mut chain) = series_target_call(arg)?;
            chain.push(ty);
            Some((f, chain))
        }
        _ => None,
    }
}

/// A FROM-less select whose target list is a set-returning function.
///
/// Only the single-target form: `select 1, generate_series(1,3)` repeats the
/// constant across the generated rows, which needs the constants carried into
/// each row, and nothing in the corpus asks for it. Refusing is better than a
/// shape that silently drops a column. A single `generate_series(...)::type`
/// cast on the one column IS carried, as an ordinary per-row cast.
fn plan_select_srf(
    s: &pg_query::protobuf::SelectStmt,
    params: &[Bson],
) -> Result<Option<Statement>> {
    let srf_targets = s
        .target_list
        .iter()
        .filter(|t| match t.node.as_ref() {
            Some(N::ResTarget(rt)) => rt
                .val
                .as_ref()
                .and_then(|v| series_target_call(v))
                .is_some(),
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
    let Some((f, cast_chain)) = rt.val.as_ref().and_then(|v| series_target_call(v)) else {
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
    // The predicate sees the series under its OUTPUT name, which is the only
    // name the row has (`select generate_series(1, 3) as bar where bar > 1`).
    let filter = series_where(s, &series, params)?;
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
            expr: None,
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
    // A `generate_series(...)::type` cast rides as an ordinary per-row cast
    // over the one generated column, exactly as a cast over a stored column
    // does. The executor applies it; the describe pass reads the type off the
    // chain's last element.
    let cast = if cast_chain.is_empty() {
        None
    } else {
        Some(ColumnExpr::Casts {
            source: None,
            chain: cast_chain,
        })
    };
    Ok(Some(Statement::Select(Select {
        table: String::new(),
        series: Some(series),
        sub: None,
        windows: Vec::new(),
        join: None,
        columns: vec![(column.clone(), column)],
        casts: vec![cast],
        filter,
        residual: None,
        order,
        limit,
        offset,
        distinct: plan_distinct(s, &|_| None)?,
    })))
}

/// `select <set-returning function>(...)` with no FROM: one row per value the
/// function yields, in a column named after it.
///
/// The arguments are constants here (literals or bound parameters), so the
/// rows are materialised at planning like a `VALUES` list's. The same
/// `srf_rows` the FROM form uses produces them, so the two spellings of
/// `unnest(ARRAY[1,2])` -- as a target and as a FROM item -- cannot drift.
fn plan_select_unnest(
    s: &pg_query::protobuf::SelectStmt,
    params: &[Bson],
) -> Result<Option<Statement>> {
    let [target] = s.target_list.as_slice() else {
        return Ok(None);
    };
    let Some(N::ResTarget(rt)) = target.node.as_ref() else {
        return Ok(None);
    };
    let Some(N::FuncCall(f)) = rt.val.as_ref().and_then(|v| v.node.as_ref()) else {
        return Ok(None);
    };
    let name = func_name(f).unwrap_or_default();
    let Some((names, types, rows)) = srf_rows(&name, f, params)? else {
        return Ok(None);
    };
    // A clause needs a source to apply to, and this shape has none. Refused by
    // name rather than silently ignored -- dropping a WHERE would return rows
    // the client asked not to see.
    if s.where_clause.is_some()
        || !s.sort_clause.is_empty()
        || s.limit_count.is_some()
        || s.limit_offset.is_some()
    {
        return Err(Error::Unsupported(format!(
            "a clause over a set-returning {name}()"
        )));
    }
    let names = vec![if rt.name.is_empty() {
        names.into_iter().next().unwrap_or(name)
    } else {
        rt.name.clone()
    }];
    Ok(Some(Statement::ValuesConstant(ValuesConstant {
        names,
        types,
        rows,
    })))
}

/// Plan a bare `VALUES (...), (...)` query into a `ValuesConstant`.
///
/// Each row's cells are resolved to values, and every column's declared type
/// is taken from the first row that gives it a non-null literal (PostgreSQL
/// unifies the column type across rows; taking the first typed cell covers the
/// cases a client actually sends, including `VALUES (1, NULL), (2, 3)` where
/// the second row is what types the nullable column).
fn plan_values_constant(s: &pg_query::protobuf::SelectStmt, params: &[Bson]) -> Result<Statement> {
    let mut rows: Vec<Vec<Bson>> = Vec::with_capacity(s.values_lists.len());
    let mut types: Vec<String> = Vec::new();
    let mut width = 0usize;
    for vl in &s.values_lists {
        let items = match vl.node.as_ref() {
            Some(N::List(l)) => &l.items,
            _ => return Err(Error::Unsupported("this VALUES form".into())),
        };
        if types.is_empty() {
            width = items.len();
            types = vec![String::new(); width];
        } else if items.len() != width {
            return Err(Error::Unsupported("VALUES rows of differing widths".into()));
        }
        let mut row = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let value = const_value(item, params)?;
            // Fill in this column's type from the first row that offers a
            // non-null typed cell.
            if types[i].is_empty() && !matches!(value, Bson::Null) {
                types[i] = static_type(item, &value);
            }
            row.push(value);
        }
        rows.push(row);
    }
    // A column that was NULL in every row defaults to `text`, as PostgreSQL
    // resolves an all-unknown VALUES column.
    for t in &mut types {
        if t.is_empty() {
            *t = "text".to_string();
        }
    }
    let names = (1..=width).map(|i| format!("column{i}")).collect();
    Ok(Statement::ValuesConstant(ValuesConstant {
        names,
        types,
        rows,
    }))
}

fn plan_select_constant(s: &pg_query::protobuf::SelectStmt, params: &[Bson]) -> Result<Statement> {
    // A bare `VALUES (...), (...)` -- a multi-row literal source with no target
    // list at all. `SelectConstant` below is the single-row case; a multi-row
    // one carries its own rows so COPY and a direct query both read them.
    if !s.values_lists.is_empty() && s.target_list.is_empty() {
        return plan_values_constant(s, params);
    }
    // A SET-RETURNING function in the target list of a FROM-less select is not
    // a constant at all: `select generate_series(1,3)` is three ROWS. It is
    // planned as an ordinary select over a generated source, which is what the
    // `FROM generate_series(...)` form already produces -- so ORDER BY, LIMIT
    // and OFFSET keep working without a second implementation.
    if let Some(stmt) = plan_select_srf(s, params)? {
        return Ok(stmt);
    }
    if let Some(stmt) = plan_select_unnest(s, params)? {
        return Ok(stmt);
    }
    // With no FROM there is nothing for a WHERE to range over: it is a
    // constant predicate that keeps or drops the single row. A non-boolean
    // is 42804, worded as PostgreSQL words it (probed PG 16).
    let where_true = match s.where_clause.as_ref() {
        None => true,
        Some(w) => constant_where(w, params)?,
    };
    let mut columns: Vec<(String, ConstCol, String, i32)> = Vec::new();
    for t in &s.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return Err(Error::Unsupported("this target".into()));
        };
        let (default_name, value, pg_type, typmod) = match rt
            .val
            .as_ref()
            .and_then(|v| v.node.as_ref())
        {
            Some(N::FuncCall(f)) => {
                refuse_untyped_any_args(f)?;
                if let Some(e) = function_absent_in_reference(f, params) {
                    return Err(e);
                }
                if let Some(u) =
                    func_name(f).and_then(|n| correlated::user_function(&n, f.args.len()))
                {
                    if !u.returns_set {
                        let node = rt.val.as_deref().expect("a FuncCall target");
                        let value = const_value(node, params)?;
                        columns.push((
                            if rt.name.is_empty() { u.name.clone() } else { rt.name.clone() },
                            ConstCol::Value(value),
                            u.return_type.clone(),
                            -1,
                        ));
                        continue;
                    }
                }
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
                        -1,
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
                        -1,
                    ));
                    continue;
                }
                // A scalar built-in in the target list. Without this a BARE
                // `select upper('a')` failed while `select upper('a')::text`
                // worked, because only the cast route goes through
                // `const_value` -- and a probe whose every case carried a cast
                // would never notice.
                let type_name = range_constructor_type(f).unwrap_or_default();
                if let (true, Some(text)) = (
                    range::is_range_type(&type_name) || range::is_multirange_type(&type_name),
                    sole_literal_string_arg(f),
                ) {
                    columns.push((
                        if rt.name.is_empty() {
                            name.clone()
                        } else {
                            rt.name.clone()
                        },
                        ConstCol::Value(cast_value(Bson::String(text), &type_name)?),
                        type_name.clone(),
                        -1,
                    ));
                    continue;
                }
                if range::is_multirange_type(&type_name) {
                    let args = f
                        .args
                        .iter()
                        .map(|a| const_value(a, params))
                        .collect::<Result<Vec<_>>>()?;
                    let value =
                        range::render_multirange(&range::multirange_from_args(&args, &type_name)?);
                    columns.push((
                        if rt.name.is_empty() {
                            name.clone()
                        } else {
                            rt.name.clone()
                        },
                        ConstCol::Value(Bson::String(value)),
                        type_name.clone(),
                        -1,
                    ));
                    continue;
                }
                if range::is_range_type(&type_name) {
                    let args = f
                        .args
                        .iter()
                        .map(|a| const_value(a, params))
                        .collect::<Result<Vec<_>>>()?;
                    let literal_flags = !matches!(
                        f.args.get(2).and_then(|a| a.node.as_ref()),
                        Some(N::ParamRef(_))
                    );
                    let value = range::render(&range::from_args(&args, &type_name, literal_flags)?);
                    columns.push((
                        if rt.name.is_empty() {
                            name.clone()
                        } else {
                            rt.name.clone()
                        },
                        ConstCol::Value(Bson::String(value)),
                        type_name.clone(),
                        -1,
                    ));
                    continue;
                }
                if let Some(result) = range_accessor_value(f, params) {
                    let (value, t) = result?;
                    columns.push((
                        if rt.name.is_empty() {
                            name.clone()
                        } else {
                            rt.name.clone()
                        },
                        ConstCol::Value(value),
                        t,
                        -1,
                    ));
                    continue;
                }
                // `defers_to_connection` falls THROUGH to the `ConstCol`
                // branch below: those three are answered by the server when
                // they stand alone, and folding them here would freeze a
                // `current_setting` the session may still change.
                let name = overload_name(f, name);
                let datetime_typed = datetime_result_type(f, &name);
                if let Some(t) = datetime_typed.clone() {
                    if let Some(value) = datetime_call(f, &name, params) {
                        columns.push((
                            if rt.name.is_empty() {
                                name.clone()
                            } else {
                                rt.name.clone()
                            },
                            ConstCol::Value(value?),
                            t,
                            -1,
                        ));
                        continue;
                    }
                }
                if scalar::is_scalar(&name)
                    && !scalar::defers_to_connection(&name)
                    && datetime_typed.is_none()
                {
                    let args = match named_call_args(f, &name, params) {
                        Some(a) => a?,
                        None => f
                            .args
                            .iter()
                            .map(|a| const_value(a, params))
                            .collect::<Result<Vec<_>>>()?,
                    };
                    if let Some(result) = scalar::call(&name, &args) {
                        let value = result?;
                        // A NULL result cannot say its type -- and at DESCRIBE
                        // time every parameter IS null, so `ascii($1)` typed
                        // as text and the executed `91` went out as `'91'`
                        // under oid 25. The function's declared result type
                        // is what PostgreSQL reports there.
                        // A `timestamptz` value is a bare date on the wire
                        // and would read as `timestamp` from its shape.
                        let declared = scalar::static_result_type(&name);
                        // An ARRAY function's type is decided by `static_type`
                        // from the CALL. Reading it off the value instead made
                        // `string_to_array('', 'x')` -- an empty array -- report
                        // `int4[]`, and every NULL-returning one report `text`.
                        let t = if arrays::is_array_function(&name) {
                            match rt.val.as_deref() {
                                Some(n) => static_type(n, &value),
                                None => inferred_type(&value).to_string(),
                            }
                        } else if value == Bson::Null
                            || declared == "timestamptz"
                            || fts::result_type(&name).is_some()
                            || jsonpath::is_function(&name)
                        {
                            declared.to_string()
                        } else {
                            inferred_type(&value).to_string()
                        };
                        columns.push((
                            if rt.name.is_empty() {
                                name.clone()
                            } else {
                                rt.name.clone()
                            },
                            ConstCol::Value(value),
                            t,
                            -1,
                        ));
                        continue;
                    }
                }
                if name == "regexp_replace" {
                    let args = f
                        .args
                        .iter()
                        .map(|a| const_value(a, params))
                        .collect::<Result<Vec<_>>>()?;
                    columns.push((
                        if rt.name.is_empty() {
                            "regexp_replace".to_string()
                        } else {
                            rt.name.clone()
                        },
                        ConstCol::Value(regexp_replace(&args)?),
                        "text".to_string(),
                        -1,
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
                        -1,
                    ));
                    continue;
                }
                // `pg_sleep(seconds)`: a `void` column (oid 2278) rendered as
                // the empty string; the wait itself belongs to execution, not
                // planning (a DESCRIBE must not sleep). Probed PG 16.
                if name == "pg_sleep" {
                    if f.args.len() != 1 {
                        return Err(Error::Unsupported(format!(
                            "pg_sleep() with {} arguments",
                            f.args.len()
                        )));
                    }
                    let seconds = cast_value(const_value(&f.args[0], params)?, "float8")?;
                    columns.push((
                        if rt.name.is_empty() {
                            "pg_sleep".to_string()
                        } else {
                            rt.name.clone()
                        },
                        ConstCol::Sleep(seconds),
                        "void".to_string(),
                        -1,
                    ));
                    continue;
                }
                // `pg_notify(channel, payload)` and `pg_listening_channels()`
                // belong to the session, which only the server has.
                if name == "pg_notify" {
                    if f.args.len() != 2 {
                        return Err(Error::Unsupported(format!(
                            "pg_notify() with {} arguments",
                            f.args.len()
                        )));
                    }
                    let channel = const_value(&f.args[0], params)?;
                    let payload = const_value(&f.args[1], params)?;
                    columns.push((
                        if rt.name.is_empty() {
                            "pg_notify".to_string()
                        } else {
                            rt.name.clone()
                        },
                        ConstCol::PgNotify { channel, payload },
                        "void".to_string(),
                        -1,
                    ));
                    continue;
                }
                if name == "pg_listening_channels" {
                    columns.push((
                        if rt.name.is_empty() {
                            "pg_listening_channels".to_string()
                        } else {
                            rt.name.clone()
                        },
                        ConstCol::ListeningChannels,
                        "text".to_string(),
                        -1,
                    ));
                    continue;
                }
                // `pg_backend_pid()` and `pg_terminate_backend(pid)` need the
                // connection's identity, which the stateless planner does not
                // have -- they become `ConstCol`s the server resolves.
                // The sequence functions read and WRITE server state, so
                // they become `ConstCol`s the server resolves rather than
                // anything the stateless planner can fold.
                // `lastval()` reads only this session's state; it is folded
                // while planning the statement that executes (NULL, and no
                // error, in a Describe's plan).
                if name == "lastval" {
                    let out = if rt.name.is_empty() {
                        name.clone()
                    } else {
                        rt.name.clone()
                    };
                    let value = correlated::call_sequence("lastval", &[])?;
                    columns.push((out, ConstCol::Value(value), "int8".to_string(), -1));
                    continue;
                }
                if matches!(name.as_str(), "nextval" | "currval" | "setval")
                    || name == "pg_get_serial_sequence"
                {
                    let arg = |i: usize| -> Result<ConstCol> {
                        match f.args.get(i) {
                            None => Ok(ConstCol::Value(Bson::Null)),
                            Some(node) => Ok(ConstCol::Value(const_value(node, params)?)),
                        }
                    };
                    let out = if rt.name.is_empty() {
                        name.clone()
                    } else {
                        rt.name.clone()
                    };
                    let (col, ty) = match name.as_str() {
                        "nextval" => (ConstCol::NextVal(Box::new(arg(0)?)), "int8"),
                        "currval" => (ConstCol::CurrVal(Box::new(arg(0)?)), "int8"),
                        "setval" => (
                            ConstCol::SetVal {
                                sequence: Box::new(arg(0)?),
                                value: Box::new(arg(1)?),
                                // The two-argument form leaves the sequence
                                // CALLED, so the next draw is one past it.
                                is_called: Box::new(if f.args.len() > 2 {
                                    arg(2)?
                                } else {
                                    ConstCol::Value(Bson::Boolean(true))
                                }),
                            },
                            "int8",
                        ),
                        _ => (
                            ConstCol::SerialSequence {
                                table: Box::new(arg(0)?),
                                column: Box::new(arg(1)?),
                            },
                            "text",
                        ),
                    };
                    columns.push((out, col, ty.to_string(), -1));
                    continue;
                }
                if name == "pg_backend_pid" {
                    columns.push((
                        if rt.name.is_empty() {
                            "pg_backend_pid".to_string()
                        } else {
                            rt.name.clone()
                        },
                        ConstCol::BackendPid,
                        "int4".to_string(),
                        -1,
                    ));
                    continue;
                }
                if name == "pg_terminate_backend" || name == "pg_cancel_backend" {
                    let arg = f
                        .args
                        .first()
                        .ok_or_else(|| Error::Unsupported(format!("{name}() without a PID")))?;
                    // The PID may itself be `pg_backend_pid()` -- resolve that
                    // nesting into a `BackendPid` the server fills in, so the
                    // idiomatic `pg_terminate_backend(pg_backend_pid())` works.
                    let is_backend_pid = matches!(
                        arg.node.as_ref(),
                        Some(N::FuncCall(inner)) if inner
                            .funcname
                            .iter()
                            .filter_map(|n| match n.node.as_ref()? {
                                N::String(st) => Some(st.sval.as_str()),
                                _ => None,
                            })
                            .next_back()
                            == Some("pg_backend_pid")
                    );
                    let inner = if is_backend_pid {
                        ConstCol::BackendPid
                    } else {
                        ConstCol::Value(const_value(arg, params)?)
                    };
                    columns.push((
                        if rt.name.is_empty() {
                            name.clone()
                        } else {
                            rt.name.clone()
                        },
                        if name == "pg_cancel_backend" {
                            ConstCol::CancelBackend(Box::new(inner))
                        } else {
                            ConstCol::TerminateBackend(Box::new(inner))
                        },
                        "bool".to_string(),
                        -1,
                    ));
                    continue;
                }
                if name == "current_database" || name == "current_catalog" {
                    (name, ConstCol::CurrentDatabase, "name".to_string(), -1)
                } else {
                    let v = session_function(&name)
                        .ok_or_else(|| Error::Unsupported(format!("function {name}()")))?;
                    let t = inferred_type(&v).to_string();
                    (name, ConstCol::Value(v), t, -1)
                }
            }
            // `user`, `current_user`, `current_timestamp` and the other
            // keyword functions: the role ones resolve on the server, which
            // knows the session; `current_timestamp` is `now()`; the
            // remaining date/time ones are refused by name.
            Some(N::SqlvalueFunction(svf)) => {
                use pg_query::protobuf::SqlValueFunctionOp as Op;
                let op = Op::try_from(svf.op).unwrap_or(Op::SqlvalueFunctionOpUndefined);
                if op == Op::SvfopCurrentTimestamp {
                    columns.push((
                        if rt.name.is_empty() {
                            "current_timestamp".to_string()
                        } else {
                            rt.name.clone()
                        },
                        ConstCol::Value(scalar::now_value()),
                        "timestamptz".to_string(),
                        -1,
                    ));
                    continue;
                }
                let (name, col) = match op {
                    Op::SvfopCurrentUser => ("current_user", ConstCol::SessionUser),
                    Op::SvfopUser => ("user", ConstCol::SessionUser),
                    Op::SvfopSessionUser => ("session_user", ConstCol::SessionUser),
                    Op::SvfopCurrentRole => ("current_role", ConstCol::SessionUser),
                    Op::SvfopCurrentCatalog => ("current_catalog", ConstCol::CurrentDatabase),
                    Op::SvfopCurrentSchema => (
                        "current_schema",
                        ConstCol::Value(Bson::String("public".into())),
                    ),
                    other => {
                        // The date/time keywords: folded now, as
                        // `current_timestamp` is above.
                        let name = other
                            .as_str_name()
                            .trim_start_matches("SVFOP_")
                            .trim_end_matches("_N")
                            .to_ascii_lowercase();
                        columns.push((
                            if rt.name.is_empty() { name } else { rt.name.clone() },
                            ConstCol::Value(sql_value_function(svf)?),
                            sql_value_function_type(svf).to_string(),
                            -1,
                        ));
                        continue;
                    }
                };
                (name.to_string(), col, "name".to_string(), -1)
            }
            // `current_database()` and friends spelled as bare column refs.
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
                if name == "current_database" || name == "current_catalog" {
                    (name, ConstCol::CurrentDatabase, "name".to_string(), -1)
                } else {
                    let v = session_function(&name)
                        .ok_or_else(|| Error::UndefinedColumn(name.clone()))?;
                    let t = inferred_type(&v).to_string();
                    (name, ConstCol::Value(v), t, -1)
                }
            }
            Some(
                node @ (N::AConst(_)
                | N::ParamRef(_)
                | N::TypeCast(_)
                | N::AExpr(_)
                | N::BoolExpr(_)
                | N::AArrayExpr(_)
                | N::RowExpr(_)
                | N::AIndirection(_)
                | N::CoalesceExpr(_)
                | N::MinMaxExpr(_)
                | N::NullTest(_)
                // `select case when ... end` with no FROM. This list is an
                // allow-list, so a node absent from it is refused even when
                // `const_value` handles it perfectly well -- which is why CASE
                // worked over a table and not here.
                | N::CaseExpr(_)),
            ) => {
                let val = rt.val.as_ref().expect("checked");
                let v = const_value(val, params)?;
                let t = static_type(val, &v);
                let _ = node;
                let typmod = cast_typmod(val);
                (expression_column_name(val), ConstCol::Value(v), t, typmod)
            }
            Some(other) => return Err(Error::Unsupported(disc(other))),
            None => return Err(Error::Unsupported("an empty target".into())),
        };
        let out = if rt.name.is_empty() {
            default_name
        } else {
            rt.name.clone()
        };
        columns.push((out, value, pg_type, typmod));
    }
    Ok(Statement::SelectConstant(SelectConstant {
        columns,
        where_true,
        source: None,
    }))
}

/// `SELECT ... FROM function(args) [AS alias[(col)]]` for a function that is
/// not `generate_series`: `select 'ok' from pg_sleep(0.5)`, `select * from
/// pg_listening_channels()`. The function is planned as the FROM-less
/// target it would be on its own, and becomes the statement's SOURCE; the
/// select list is planned FROM-less too, with every reference to the
/// function's column (by its alias, the function's name, or `*`) reading the
/// source row.
fn plan_function_source_select(
    s: &pg_query::protobuf::SelectStmt,
    rf: &pg_query::protobuf::RangeFunction,
    params: &[Bson],
) -> Result<Statement> {
    let call = rf
        .functions
        .iter()
        .flat_map(|f| match f.node.as_ref() {
            Some(N::List(l)) => l.items.clone(),
            _ => vec![f.clone()],
        })
        .find(|n| matches!(n.node.as_ref(), Some(N::FuncCall(_))))
        .ok_or_else(|| Error::Unsupported("this FROM function".into()))?;
    let Some(N::FuncCall(func)) = call.node.as_ref() else {
        unreachable!("filtered to FuncCall");
    };
    let func_name = func_name(func).unwrap_or_default();
    // The table alias and the column name: `AS g(x)` names both, `AS g`
    // names the table AND the column (a single-column function's column
    // takes the alias), no alias leaves the function's own name.
    let (table_alias, column) = match rf.alias.as_ref() {
        Some(a) => {
            let col = a
                .colnames
                .iter()
                .find_map(|c| match c.node.as_ref() {
                    Some(N::String(st)) => Some(st.sval.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| a.aliasname.clone());
            (a.aliasname.clone(), col)
        }
        None => (func_name.clone(), func_name.clone()),
    };
    // Plan the function as a FROM-less target to learn its column and type.
    let source_target = pg_query::protobuf::Node {
        node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
            name: column.clone(),
            indirection: Vec::new(),
            val: Some(Box::new(call.clone())),
            location: 0,
        }))),
    };
    let planned = plan_select_constant(
        &pg_query::protobuf::SelectStmt {
            target_list: vec![source_target],
            ..Default::default()
        },
        params,
    )?;
    let Statement::SelectConstant(source) = planned else {
        return Err(Error::Unsupported("this FROM function".into()));
    };
    let (_, source_col, source_type, source_typmod) = source
        .columns
        .into_iter()
        .next()
        .ok_or_else(|| Error::Unsupported("this FROM function".into()))?;
    // Which targets read the function's column: `*`, `col`, `alias.col`,
    // `alias.*`. The rest are planned FROM-less as they stand.
    let refers = |c: &pg_query::protobuf::ColumnRef| -> Option<bool> {
        let parts: Vec<&pg_query::protobuf::Node> = c.fields.iter().collect();
        let name = |n: &pg_query::protobuf::Node| match n.node.as_ref() {
            Some(N::String(st)) => Some(st.sval.clone()),
            _ => None,
        };
        match parts.as_slice() {
            [one] if matches!(one.node.as_ref(), Some(N::AStar(_))) => Some(true),
            [one] => Some(name(one)? == column),
            [t, c] => {
                let t = name(t)?;
                if t != table_alias {
                    return None;
                }
                if matches!(c.node.as_ref(), Some(N::AStar(_))) {
                    return Some(true);
                }
                Some(name(c)? == column)
            }
            _ => None,
        }
    };
    let mut others: Vec<pg_query::protobuf::Node> = Vec::new();
    // Per original target: `Some(name)` for one reading the source column,
    // `None` for one planned among `others`.
    let mut slots: Vec<Option<String>> = Vec::new();
    for t in &s.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return Err(Error::Unsupported("this target".into()));
        };
        if let Some(N::ColumnRef(c)) = rt.val.as_ref().and_then(|v| v.node.as_ref()) {
            match refers(c) {
                Some(true) => {
                    slots.push(Some(if rt.name.is_empty() {
                        column.clone()
                    } else {
                        rt.name.clone()
                    }));
                    continue;
                }
                Some(false) => {
                    return Err(Error::UndefinedColumn(
                        column_ref_name(c).unwrap_or_default(),
                    ));
                }
                None => {}
            }
        }
        slots.push(None);
        others.push(t.clone());
    }
    let planned = plan_select_constant(
        &pg_query::protobuf::SelectStmt {
            target_list: others,
            where_clause: s.where_clause.clone(),
            ..Default::default()
        },
        params,
    )?;
    let Statement::SelectConstant(rest) = planned else {
        return Err(Error::Unsupported("this select list".into()));
    };
    let mut rest_cols = rest.columns.into_iter();
    let mut columns = Vec::with_capacity(slots.len());
    for slot in slots {
        match slot {
            Some(name) => columns.push((
                name,
                ConstCol::FromColumn,
                source_type.clone(),
                source_typmod,
            )),
            None => columns.push(
                rest_cols
                    .next()
                    .ok_or_else(|| Error::Unsupported("this select list".into()))?,
            ),
        }
    }
    Ok(Statement::SelectConstant(SelectConstant {
        columns,
        where_true: rest.where_true,
        source: Some(Box::new(source_col)),
    }))
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
    // PostgreSQL's DATE domain is far wider than a Python `date` (or chrono's
    // common range): `infinity` / `-infinity` are valid values, so are years
    // past 9999 and BC. mongod-side we do not compute on these -- we store the
    // CANONICAL TEXT and let the client's loader decide what it can hold (a
    // Python date raises "date too large", which is what psycopg's overflow
    // tests assert). So accept the shapes chrono cannot and pass them through.
    let lower = t.to_ascii_lowercase();
    if lower == "infinity" || lower == "+infinity" {
        return Ok("infinity".to_string());
    }
    if lower == "-infinity" {
        return Ok("-infinity".to_string());
    }
    // `epoch` is the one special INPUT value that is a constant (`now` / `today`
    // depend on the clock and are filed rather than guessed), mirroring the
    // `timestamp` cast arm.
    if lower == "epoch" {
        return Ok("1970-01-01".to_string());
    }
    let parsed = if t.len() == 8 && t.chars().all(|c| c.is_ascii_digit()) {
        NaiveDate::parse_from_str(t, "%Y%m%d")
    } else {
        NaiveDate::parse_from_str(t, "%Y-%m-%d")
    };
    match parsed {
        Ok(d) => Ok(d.format("%Y-%m-%d").to_string()),
        // A `YYYY-MM-DD` shape chrono rejected only for its YEAR magnitude
        // (PostgreSQL allows year > 9999 and BC) is passed through as canonical
        // text; the client's loader is what ultimately rejects an unrepresentable
        // value. A BAD FIELD (month 13, day 40) is 22008, and a non-date is 22007.
        Err(_) => match classify_wide_date(t) {
            WideDate::Valid => Ok(canonical_wide_date(t)),
            WideDate::FieldOutOfRange => Err(Error::DatetimeFieldOverflow(format!(
                "date/time field value out of range: \"{t}\""
            ))),
            WideDate::NotADate => Err(Error::InvalidDatetimeFormat(format!(
                "invalid input syntax for type date: \"{t}\""
            ))),
        },
    }
}

/// Render a `NaiveDate` in PostgreSQL's ISO `date` text, including the eras a
/// Python `date` cannot hold: a proleptic year <= 0 becomes `NNNN-MM-DD BC`
/// (year 0 is 1 BC), and a year past 9999 keeps its full width (`10000-01-01`).
/// The client's loader is what rejects the values it cannot represent -- which
/// is exactly what psycopg's date-overflow tests assert.
fn render_date_pg(d: NaiveDate) -> String {
    use chrono::Datelike;
    let (y, m, day) = (d.year(), d.month(), d.day());
    if y >= 1 {
        format!("{y:04}-{m:02}-{day:02}")
    } else {
        format!("{:04}-{m:02}-{day:02} BC", 1 - y)
    }
}

/// The canonical text of a timestamp literal PostgreSQL accepts but a Python
/// datetime cannot hold: `infinity` / `-infinity`, a year past 9999, or BC.
/// `None` for an ordinary timestamp (which flows through the micros path). We
/// store these as their TEXT and let the client's loader raise, exactly as for
/// out-of-range dates.
fn special_timestamp_text(t: &str) -> Option<String> {
    let trimmed = t.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower == "infinity" || lower == "+infinity" {
        return Some("infinity".to_string());
    }
    if lower == "-infinity" {
        return Some("-infinity".to_string());
    }
    let date_part = trimmed.split([' ', 'T']).next().unwrap_or(trimmed);
    // BC is ALWAYS beyond a Python datetime's proleptic range, whatever the
    // year magnitude, so a `... BC` literal is always kept as text.
    if lower.ends_with(" bc") {
        // classify_wide_date needs the era, so hand it the date part WITH `BC`.
        let date_bc = format!("{date_part} BC");
        return matches!(classify_wide_date(&date_bc), WideDate::Valid)
            .then(|| canonical_wide_timestamp(trimmed));
    }
    // A wide year (> 9999) chrono cannot parse: keep the text.
    if matches!(classify_wide_date(date_part), WideDate::Valid)
        && NaiveDate::parse_from_str(date_part.trim_start_matches('-'), "%Y-%m-%d").is_err()
    {
        return Some(canonical_wide_timestamp(trimmed));
    }
    None
}

/// Canonicalise a wide/BC timestamp literal to PostgreSQL's rendered text.
/// PostgreSQL keeps the (wide/BC) date as given but always renders the time as
/// `HH:MM:SS` (seconds appended, `00:00:00` when absent), then re-attaches the
/// ` BC` era. The tz-offset a *timestamptz* would carry is deliberately not
/// computed here (see tasks/backlog.md) -- this is the no-offset form.
fn canonical_wide_timestamp(t: &str) -> String {
    let trimmed = t.trim();
    let (body, bc) = match trimmed
        .strip_suffix(" BC")
        .or_else(|| trimmed.strip_suffix(" bc"))
    {
        Some(b) => (b.trim_end(), true),
        None => (trimmed, false),
    };
    let (date, time) = match body.find([' ', 'T']) {
        Some(i) => (&body[..i], body[i + 1..].trim()),
        None => (body, ""),
    };
    let time = if time.is_empty() {
        "00:00:00".to_string()
    } else if time.matches(':').count() == 1 {
        format!("{time}:00")
    } else {
        time.to_string()
    };
    let mut out = format!("{date} {time}");
    if bc {
        out.push_str(" BC");
    }
    out
}

enum WideDate {
    /// A well-formed date whose year is simply out of chrono's range.
    Valid,
    /// The right shape but an impossible month/day.
    FieldOutOfRange,
    /// Not a date at all.
    NotADate,
}

/// Classify a date literal chrono could not parse: a wide/BC year (pass
/// through), a bad field (22008), or not a date (22007).
fn classify_wide_date(t: &str) -> WideDate {
    let (body, bc) = match t.strip_suffix(" BC").or_else(|| t.strip_suffix(" bc")) {
        Some(b) => (b.trim_end(), true),
        None => (t, false),
    };
    let digits = body.trim_start_matches('-');
    let parts: Vec<&str> = digits.split('-').collect();
    let numeric3 = parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    if numeric3 {
        let year = parts[0].parse::<i64>().unwrap_or(0);
        let month = parts[1].parse::<u32>().unwrap_or(0);
        let day = parts[2].parse::<u32>().unwrap_or(0);
        // Only a genuinely WIDE (year > 9999) or BC date is a passthrough
        // candidate: a NORMAL-year date reached this arm because chrono already
        // rejected it, which makes it a real bad field (`2026-02-30` is 22008,
        // never text). For a wide/BC one chrono cannot help, so validate the
        // day against the proleptic-Gregorian calendar ourselves -- PostgreSQL
        // does (`12345-02-29` is out of range, `0001-02-29 BC` is not).
        if bc || year > 9999 {
            // 1 BC is astronomical year 0, 2 BC is -1, ...
            let astro = if bc { 1 - year } else { year };
            return if gregorian_day_valid(astro, month, day) {
                WideDate::Valid
            } else {
                WideDate::FieldOutOfRange
            };
        }
        return WideDate::FieldOutOfRange;
    }
    // Not a wide/BC date and not the 3-number shape: a numeric-shaped value is
    // a bad field (22008), anything else is not a date at all (22007).
    let numeric_shape = t
        .split(['-', '/'])
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    if numeric_shape {
        WideDate::FieldOutOfRange
    } else {
        WideDate::NotADate
    }
}

/// Is `day` a real day of `month` in proleptic-Gregorian `year` (astronomical,
/// so 1 BC = 0)? Leap years follow the standard divisibility rule, which Rust's
/// truncating `%` gets right for negative years too (-3 % 4 != 0).
fn gregorian_day_valid(year: i64, month: u32, day: u32) -> bool {
    if !(1..=12).contains(&month) || day == 0 {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let dim = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if leap {
                29
            } else {
                28
            }
        }
    };
    day <= dim
}

/// The canonical rendering of a wide/BC date: PostgreSQL zero-pads the year to
/// at least four digits and keeps a ` BC` suffix.
fn canonical_wide_date(t: &str) -> String {
    let (body, bc) = match t.strip_suffix(" BC").or_else(|| t.strip_suffix(" bc")) {
        Some(b) => (b.trim(), true),
        None => (t, false),
    };
    let mut out = body.to_string();
    if bc {
        out.push_str(" BC");
    }
    out
}

/// Parse a `time` literal and render it as PostgreSQL does.
///
/// `'12:34'` fills in `:00` seconds; fractional seconds keep only the digits
/// that matter (`12:34:56.5`, not `12:34:56.500000`). An hour past 24 is
/// `22008`, not a parse error.
fn parse_time(text: &str) -> Result<String> {
    let t = text.trim();
    // PostgreSQL accepts `24:00:00` as a valid `time` (the end-of-day instant),
    // rendering it back as `24:00:00`. chrono has no hour 24, so accept the
    // exact all-zero forms here -- `24:00`, `24:00:00`, `24:00:00.000` -- and
    // reject `24:00:01` / `24:00:00.1` as out of range, matching PG. A Python
    // `time` cannot hold it either, so psycopg's loader raises on the way back.
    if is_end_of_day_time(t) {
        return Ok("24:00:00".to_string());
    }
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

/// Whether `t` is PostgreSQL's special end-of-day `time` value: hour 24 with
/// every finer field zero (`24:00`, `24:00:00`, `24:00:00.000000`). `24:00:01`
/// or a non-zero fraction is out of range, not this.
fn is_end_of_day_time(t: &str) -> bool {
    let (clock, frac) = match t.split_once('.') {
        Some((c, f)) => (c, Some(f)),
        None => (t, None),
    };
    if let Some(f) = frac {
        if f.is_empty() || !f.chars().all(|c| c == '0') {
            return false;
        }
    }
    let mut parts = clock.split(':');
    let (Some(h), min, sec, extra) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if extra.is_some() || h != "24" {
        return false;
    }
    // Minutes must be present and zero; seconds, if present, zero.
    matches!(min, Some("00") | Some("0")) && matches!(sec, None | Some("00") | Some("0"))
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

/// The tag key of an anonymous record value.
pub const RECORD_KEY: &str = "__record";

/// An anonymous record (`ROW(...)` / `(a, b, ...)`): an ORDERED field list,
/// tagged so it is distinct from an array (`{...}`) and from any other
/// document. Rendered as `(a,b,...)` and reported as oid 2249.
pub(crate) fn record_value(fields: Vec<Bson>) -> Bson {
    let mut d = Document::new();
    d.insert(RECORD_KEY, Bson::Array(fields));
    Bson::Document(d)
}

/// The companion key holding a `ROW(...)` record's STATIC field types.
///
/// PostgreSQL's binary record format carries an oid per field, and that oid
/// is the field EXPRESSION's type, which the value alone cannot recover: a
/// bare `'x'` inside `ROW(...)` is `unknown` (705) where `'x'::text` is `text`
/// (25), and a bare `null` is `unknown` where `null::text` is `text`. So a
/// record built from a `ROW(...)` expression records each field's static type
/// name beside its fields; a record from any other door carries none and the
/// wire layer infers the oids from the values.
pub const RECORD_TYPES_KEY: &str = "__record_types";

/// A `ROW(...)` record with its fields' static type names alongside.
pub(crate) fn typed_record_value(fields: Vec<Bson>, types: Vec<String>) -> Bson {
    let mut d = Document::new();
    d.insert(RECORD_KEY, Bson::Array(fields));
    d.insert(
        RECORD_TYPES_KEY,
        Bson::Array(types.into_iter().map(Bson::String).collect()),
    );
    Bson::Document(d)
}

/// The static field type names of a `ROW(...)` record, or `None` when the
/// record was built without them.
pub fn record_field_types(v: &Bson) -> Option<Vec<String>> {
    let Bson::Document(d) = v else {
        return None;
    };
    record_fields(v)?;
    match d.get(RECORD_TYPES_KEY) {
        Some(Bson::Array(items)) => items
            .iter()
            .map(|t| t.as_str().map(str::to_owned))
            .collect(),
        _ => None,
    }
}

/// The field list inside a record value, or `None` for any other value.
pub(crate) fn record_fields(v: &Bson) -> Option<&Vec<Bson>> {
    match v {
        Bson::Document(d) if d.len() == 1 || (d.len() == 2 && d.contains_key(RECORD_TYPES_KEY)) => {
            match d.get(RECORD_KEY) {
                Some(Bson::Array(items)) => Some(items),
                _ => None,
            }
        }
        _ => None,
    }
}

/// A record's PostgreSQL text: `(f1,f2,...)`. A NULL field is empty; a field is
/// double-quoted (with `"`->`""` and `\`->`\\`) when it is empty or contains
/// a comma, parenthesis, quote, backslash or whitespace.
pub(crate) fn record_text(fields: &[Bson]) -> String {
    let mut out = String::from("(");
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        if *f == Bson::Null {
            continue;
        }
        // A record field uses the type's OUTPUT function, not its `::text`
        // cast: a bool prints `t`/`f` inside a record, not `true`/`false`.
        let field = match f {
            Bson::Boolean(b) => (if *b { "t" } else { "f" }).to_string(),
            _ => render_value_text(f),
        };
        let needs_quote = field.is_empty()
            || field
                .chars()
                .any(|c| matches!(c, ',' | '(' | ')' | '"' | '\\') || c.is_whitespace());
        if needs_quote {
            out.push('"');
            for c in field.chars() {
                if c == '"' || c == '\\' {
                    out.push(c);
                }
                out.push(c);
            }
            out.push('"');
        } else {
            out.push_str(&field);
        }
    }
    out.push(')');
    out
}

/// Compare two records with PostgreSQL's rules, which DIFFER by operator AND
/// by whether the operands are ROW CONSTRUCTORS or composite VALUES.
///
/// `=`/`<>` examine every pair (a non-null unequal pair decides); the ordering
/// operators short-circuit left to right. The NULL rule depends on `composite`:
/// - **row constructor vs row constructor** (`composite == false`): the SQL
///   three-valued rule -- a NULL the result depends on makes it NULL
///   (`ROW(1,NULL) = ROW(1,NULL)` is NULL, and `ROW(1,NULL) < ROW(2,...)` is
///   NULL because the earlier field's NULL is undecidable).
/// - **composite value vs composite value** (`composite == true`): two NULL
///   field values are EQUAL and a NULL sorts LARGER than any non-NULL, so the
///   comparison always resolves to true/false, never NULL (PostgreSQL's rule
///   for comparing composite-type values, as opposed to row constructors).
///
/// A field that is itself a record compares by the SAME rules, recursively.
fn record_compare(op: &str, a: &[Bson], b: &[Bson], composite: bool) -> Result<Bson> {
    use std::cmp::Ordering;
    // One field pair -> an Ordering, recursing into a nested record. Only
    // reached for a pair with no NULL on either side (the callers handle NULLs
    // first), so the nested record's own NULLs follow `composite`.
    fn cmp_pair(op: &str, x: &Bson, y: &Bson, composite: bool) -> Result<Ordering> {
        if let (Some(rx), Some(ry)) = (record_fields(x), record_fields(y)) {
            // A `<`-form comparison gives a total order over the nested record,
            // which is the tie-break the caller needs.
            return match record_compare("<", rx, ry, composite)? {
                Bson::Boolean(true) => Ok(Ordering::Less),
                _ => match record_compare("=", rx, ry, composite)? {
                    Bson::Boolean(true) => Ok(Ordering::Equal),
                    _ => Ok(Ordering::Greater),
                },
            };
        }
        compare_constants(x, y).ok_or_else(|| {
            Error::Unsupported(format!(
                "comparing {} with {} using {op}",
                bson_kind(x),
                bson_kind(y)
            ))
        })
    }
    if matches!(op, "=" | "<>" | "!=") {
        let mut saw_null = false;
        for (x, y) in a.iter().zip(b.iter()) {
            match (*x == Bson::Null, *y == Bson::Null) {
                (true, true) => {
                    // Composite: two NULLs are equal. Row constructor: the
                    // result depends on a NULL, so it becomes NULL.
                    if !composite {
                        saw_null = true;
                    }
                    continue;
                }
                (true, false) | (false, true) => {
                    // Composite: a NULL beside a non-NULL is UNEQUAL. Row
                    // constructor: still an undecidable NULL.
                    if composite {
                        return Ok(Bson::Boolean(op != "="));
                    }
                    saw_null = true;
                    continue;
                }
                (false, false) => {}
            }
            if cmp_pair(op, x, y, composite)? != Ordering::Equal {
                return Ok(Bson::Boolean(op != "="));
            }
        }
        if a.len() != b.len() {
            return Ok(Bson::Boolean(op != "="));
        }
        if saw_null {
            return Ok(Bson::Null);
        }
        return Ok(Bson::Boolean(op == "="));
    }
    for (x, y) in a.iter().zip(b.iter()) {
        match (*x == Bson::Null, *y == Bson::Null) {
            (true, true) => {
                // Composite: equal on this field, keep looking. Row
                // constructor: undecidable -> NULL.
                if composite {
                    continue;
                }
                return Ok(Bson::Null);
            }
            (true, false) | (false, true) => {
                if composite {
                    // A NULL sorts LARGER than any non-NULL.
                    let ord = if *x == Bson::Null {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    };
                    return Ok(Bson::Boolean(decide_ord(op, ord)));
                }
                return Ok(Bson::Null);
            }
            (false, false) => {}
        }
        match cmp_pair(op, x, y, composite)? {
            Ordering::Equal => continue,
            ord => return Ok(Bson::Boolean(decide_ord(op, ord))),
        }
    }
    Ok(Bson::Boolean(decide_ord(op, a.len().cmp(&b.len()))))
}

fn decide_ord(op: &str, ord: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering::{Greater, Less};
    match op {
        "<" => ord == Less,
        "<=" => ord != Greater,
        ">" => ord == Greater,
        ">=" => ord != Less,
        _ => false,
    }
}

/// Parse PostgreSQL composite TEXT input -- `(f1,f2,...)` -- into one entry per
/// field: `None` for an unquoted-empty field (SQL NULL), `Some(text)` for the
/// decoded field text (a quoted empty is the empty string, not NULL). Mirrors
/// PostgreSQL's `record_in`: leading and trailing whitespace around an unquoted
/// field is ignored, a field may be double-quoted with `""`->`"` and `\x`->`x`.
fn parse_composite_text(input: &str) -> Result<Vec<Option<String>>> {
    let s = input.trim();
    let inner = s
        .strip_prefix('(')
        .and_then(|r| r.strip_suffix(')'))
        .ok_or_else(|| {
            Error::InvalidText(format!(
                "malformed record literal: \"{input}\"\nDetail: Missing left parenthesis."
            ))
        })?;
    let mut fields: Vec<Option<String>> = Vec::new();
    // The empty parenthesis pair `()` is a record with zero fields.
    if inner.is_empty() {
        return Ok(fields);
    }
    let mut chars = inner.chars().peekable();
    loop {
        // A field is quoted, or a bare run up to the next top-level comma.
        let mut value = String::new();
        let mut quoted = false;
        let mut saw_content = false;
        // Skip leading whitespace of an unquoted field. PostgreSQL's `record_in`
        // trims only ASCII whitespace, so a value char that Unicode calls space
        // (U+0085, U+00A0, ...) is content, not padding.
        while matches!(chars.peek(), Some(c) if c.is_ascii_whitespace()) {
            chars.next();
        }
        if chars.peek() == Some(&'"') {
            quoted = true;
            saw_content = true;
            chars.next();
            loop {
                match chars.next() {
                    Some('"') => {
                        if chars.peek() == Some(&'"') {
                            value.push('"');
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    Some('\\') => match chars.next() {
                        Some(c) => value.push(c),
                        None => {
                            return Err(Error::InvalidText(format!(
                                "malformed record literal: \"{input}\""
                            )))
                        }
                    },
                    Some(c) => value.push(c),
                    None => {
                        return Err(Error::InvalidText(format!(
                        "malformed record literal: \"{input}\"\nDetail: Unexpected end of input."
                    )))
                    }
                }
            }
            // Trailing whitespace up to the comma or the end.
            while matches!(chars.peek(), Some(c) if c.is_ascii_whitespace()) {
                chars.next();
            }
        } else {
            // A bare field: content up to the next top-level comma. A backslash
            // still escapes its next character even outside quotes.
            while let Some(&c) = chars.peek() {
                if c == ',' {
                    break;
                }
                chars.next();
                if c == '\\' {
                    match chars.next() {
                        Some(n) => {
                            value.push(n);
                            saw_content = true;
                        }
                        None => {
                            return Err(Error::InvalidText(format!(
                                "malformed record literal: \"{input}\""
                            )))
                        }
                    }
                } else {
                    value.push(c);
                    saw_content = true;
                }
            }
            // A bare field has its trailing ASCII whitespace trimmed.
            let trimmed_len = value
                .trim_end_matches(|c: char| c.is_ascii_whitespace())
                .len();
            value.truncate(trimmed_len);
        }
        // An unquoted, content-free field is SQL NULL; a quoted one is "".
        if quoted || saw_content {
            fields.push(Some(value));
        } else {
            fields.push(None);
        }
        match chars.next() {
            Some(',') => continue,
            None => break,
            Some(c) => {
                return Err(Error::InvalidText(format!(
                    "malformed record literal: \"{input}\"\nDetail: Unexpected character \"{c}\" after the field."
                )))
            }
        }
    }
    Ok(fields)
}

/// Build a composite VALUE (a record-shaped datum) from a source value and the
/// composite's declared fields, coercing each field to its declared type. The
/// source is either PostgreSQL composite TEXT (`(1,x)`) or an already-built
/// record (`row(1,'x')` / a bound tuple). A field count that disagrees with the
/// composite's declaration is the 22P02 PostgreSQL reports.
fn composite_value(value: Bson, target: &str, fields: &[(String, String)]) -> Result<Bson> {
    let (raw, literal): (Vec<Bson>, Option<String>) = match value {
        Bson::String(text) => (
            parse_composite_text(&text)?
                .into_iter()
                .map(|f| match f {
                    Some(s) => Bson::String(s),
                    None => Bson::Null,
                })
                .collect(),
            Some(text),
        ),
        other => match record_fields(&other) {
            Some(items) => (items.clone(), None),
            None => {
                return Err(Error::Unsupported(format!(
                    "a cast of {} to {target}",
                    bson_kind(&other)
                )))
            }
        },
    };
    // Measured on 16: a TEXT literal of the wrong width is 22P02 `malformed
    // record literal: "(1)"` with `Too few columns.` / `Too many columns.`;
    // a RECORD (`row(1)::ct`) is 42846 `cannot cast type record to ct` with
    // `Input has too few columns.` / `Input has too many columns.`.
    if raw.len() != fields.len() {
        let few = raw.len() < fields.len();
        return Err(match literal {
            Some(input) => Error::InvalidText(format!(
                "malformed record literal: \"{input}\"\nDetail: {}",
                if few {
                    "Too few columns."
                } else {
                    "Too many columns."
                }
            )),
            None => Error::CannotCoerce(format!(
                "cannot cast type record to {target}\nDetail: {}",
                if few {
                    "Input has too few columns."
                } else {
                    "Input has too many columns."
                }
            )),
        });
    }
    let mut out = Vec::with_capacity(raw.len());
    for (v, (_, ty)) in raw.into_iter().zip(fields.iter()) {
        out.push(cast_value(v, ty)?);
    }
    Ok(record_value(out))
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
    let user_name = |oid: i64| user_type_name(oid).or_else(|| user_composite_name(oid));
    if let Some(name) = user_name(oid) {
        // PostgreSQL renders a regtype per IDENTIFIER PART: a schema-qualified
        // `testschema.testcomp` prints unquoted as `schema.name` (each part
        // quoted by `quote_identifier`'s rule -- measured: `"CamelCase"` and
        // `"order"`, but `mood`), NOT as one quoted `"schema.name"`.
        return quote_type_path(&name);
    }
    // A user type's array: `mood[]`, the element rendered as above.
    if let Some(name) = user_name(oid - USER_TYPE_ARRAY_OID_OFFSET) {
        return format!("{}[]", quote_type_path(&name));
    }
    oid.to_string()
}

fn quote_type_path(name: &str) -> String {
    name.split('.')
        .map(scalar::quote_identifier)
        .collect::<Vec<_>>()
        .join(".")
}

/// A composite type's resolution NAME by oid -- the reverse of
/// `user_composite_oid`, for rendering its regtype.
fn user_composite_name(oid: i64) -> Option<String> {
    PLAN_USER_COMPOSITES.with(|t| {
        t.borrow()
            .iter()
            .find(|(_, o, _)| *o == oid)
            .map(|(n, _, _)| n.clone())
    })
}

/// The key of the one-field document a `regclass` VALUE is carried as -- the
/// `regtype` convention: an oid that RENDERS as the relation's name.
/// `select 't1'::regclass` prints `t1` under oid 2205; `::oid` reads the
/// number; `where attrelid = 't1'::regclass` compares it.
pub const REGCLASS_KEY: &str = "__regclass_oid";

pub(crate) fn regclass_value(oid: i64) -> Bson {
    let mut d = Document::new();
    d.insert(REGCLASS_KEY, Bson::Int64(oid));
    Bson::Document(d)
}

/// The oid inside a regclass value, or `None` for any other value.
pub fn regclass_oid(v: &Bson) -> Option<i64> {
    match v {
        Bson::Document(d) if d.len() == 1 => d.get_i64(REGCLASS_KEY).ok(),
        _ => None,
    }
}

thread_local! {
    /// The relations of the current database: `(name, oid, temp)`, installed
    /// per statement by the wire layer so a `'t1'::regclass` cast resolves
    /// and a regclass value renders without a catalog read. The oid is the
    /// table's `pg_class` oid -- the same number `pg_attribute.attrelid` and
    /// the RowDescription's `ftable` carry.
    static PLAN_USER_RELATIONS: std::cell::RefCell<Vec<(String, i64, bool)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Relations: `(name, oid, temp)`.
pub fn set_user_relations(relations: Vec<(String, i64, bool)>) {
    PLAN_USER_RELATIONS.with(|t| *t.borrow_mut() = relations);
}

/// The system catalogs this server answers for, under PostgreSQL's own fixed
/// oids (`'pg_class'::regclass::oid` is 1259 on every install; measured 16).
const CATALOG_RELATIONS: &[(&str, i64)] = &[
    ("pg_type", 1247),
    ("pg_attribute", 1249),
    ("pg_proc", 1255),
    ("pg_class", 1259),
    ("pg_database", 1262),
    ("pg_index", 2610),
    ("pg_constraint", 2606),
    ("pg_namespace", 2615),
    ("pg_enum", 3501),
    ("pg_range", 3541),
    ("pg_extension", 3079),
    ("pg_authid", 1260),
];

/// The display rendering of a regclass value: the relation's name, quoted
/// where an identifier needs it (`"Order"`, `"order"`), bare for a catalog
/// relation, `-` for oid 0, and the bare number for an oid nothing has.
/// A user relation prints WITHOUT its schema: `public` and `pg_temp` are
/// both on the default search_path, so it is visible by its bare name.
pub fn regclass_text(oid: i64) -> String {
    if oid == 0 {
        return "-".to_string();
    }
    if let Some(name) = PLAN_USER_RELATIONS.with(|t| {
        t.borrow()
            .iter()
            .find(|(_, o, _)| *o == oid)
            .map(|(n, _, _)| n.clone())
    }) {
        // A schema-qualified table is stored as `schema.name`; each part
        // quotes on its own (`"Order"`, `testschema."Order"`).
        return name
            .split('.')
            .map(scalar::quote_identifier)
            .collect::<Vec<_>>()
            .join(".");
    }
    if let Some((name, _)) = CATALOG_RELATIONS.iter().find(|(_, o)| *o == oid) {
        return (*name).to_string();
    }
    oid.to_string()
}

/// Split a relation reference the way PostgreSQL's `SplitIdentifierString`
/// does for `regclassin`: whitespace around a part is ignored, an unquoted
/// part folds to lower case, a quoted one keeps its case (a doubled quote is
/// one), and anything else -- an empty name, a bare dot, an unquoted part
/// with a space inside, an unterminated quote -- is 42602 `invalid name
/// syntax` (measured 16: `'a b'`, `'.t1'`, `'t1.'`, `'  '`).
fn split_relation_name(text: &str) -> Result<Vec<String>> {
    let invalid = || Error::InvalidName("invalid name syntax".into());
    let mut parts = Vec::new();
    let mut chars = text.trim().chars().peekable();
    if chars.peek().is_none() {
        return Err(invalid());
    }
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let mut part = String::new();
        match chars.peek() {
            Some('"') => {
                chars.next();
                loop {
                    match chars.next() {
                        Some('"') if chars.peek() == Some(&'"') => {
                            part.push('"');
                            chars.next();
                        }
                        Some('"') => break,
                        Some(c) => part.push(c),
                        None => return Err(invalid()),
                    }
                }
            }
            Some(_) => {
                while let Some(&c) = chars.peek() {
                    if c == '.' || c.is_whitespace() {
                        break;
                    }
                    part.push(c.to_ascii_lowercase());
                    chars.next();
                }
                if part.is_empty() {
                    return Err(invalid());
                }
            }
            None => return Err(invalid()),
        }
        parts.push(part);
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        match chars.next() {
            None => return Ok(parts),
            Some('.') => continue,
            Some(_) => return Err(invalid()),
        }
    }
}

/// `'t1'::regclass` -- the oid of the relation a text names. Resolves an
/// unqualified name as PostgreSQL's search_path does (temp tables first,
/// then `public`, then the catalogs), `public.x` / `pg_temp.x` /
/// `pg_catalog.x` by that schema alone, and a database-qualified
/// `db.schema.x` by its last two parts. Unknown is 42P01 with the PARSED
/// name (`relation "nope.t1" does not exist`, `relation "t1"` for a
/// case-folded `'T1'`); four or more parts is PostgreSQL's 42601.
fn resolve_regclass(text: &str) -> Result<i64> {
    let parts = split_relation_name(text)?;
    let (schema, name) = match parts.as_slice() {
        [name] => (None, name.as_str()),
        [schema, name] => (Some(schema.as_str()), name.as_str()),
        [_, schema, name] => (Some(schema.as_str()), name.as_str()),
        _ => {
            return Err(Error::Parse(format!(
                "improper relation name (too many dotted names): {}",
                text.trim()
            )))
        }
    };
    let user = |stored: &str, temp: Option<bool>| -> Option<i64> {
        PLAN_USER_RELATIONS.with(|t| {
            t.borrow()
                .iter()
                .find(|(n, _, is_temp)| n == stored && temp.is_none_or(|want| want == *is_temp))
                .map(|(_, oid, _)| *oid)
        })
    };
    let catalog = || {
        CATALOG_RELATIONS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, oid)| *oid)
    };
    let found = match schema {
        None => user(name, Some(true))
            .or_else(|| user(name, Some(false)))
            .or_else(catalog),
        Some("public") => user(name, Some(false)),
        Some(s) if s == "pg_temp" || s.starts_with("pg_temp_") => user(name, Some(true)),
        Some("pg_catalog") => catalog(),
        // A table in another schema is stored under `schema.name`.
        Some(s) => user(&format!("{s}.{name}"), Some(false)),
    };
    found.ok_or_else(|| {
        Error::UndefinedTable(match schema {
            Some(s) => format!("{s}.{name}"),
            None => name.to_string(),
        })
    })
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
    // A `timestamp` (WITHOUT time zone) accepts a trailing offset and DROPS it,
    // keeping the wall-clock reading -- PostgreSQL does this, and psycopg dumps
    // a tz-aware datetime that can land here with a `+02` / `-05:30` /
    // `-01:02:03` suffix. The offset lives in the TIME portion (after the space
    // that follows the date), so the date's own `-` separators are untouched.
    let normalised = match normalised.find(' ') {
        Some(sp) => match normalised[sp + 1..].find(['+', '-']) {
            Some(pos) => normalised[..sp + 1 + pos].trim_end().to_string(),
            None => normalised,
        },
        None => normalised,
    };
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

/// The output half of PostgreSQL's `DateStyle` GUC: the four display formats
/// (`ISO` / `Postgres` / `SQL` / `German`) crossed with the field ORDER
/// (`YMD` / `MDY` / `DMY`). Only the output side is modelled here -- the input
/// side (how an ambiguous literal like `01/02/03` is parsed) is handled by the
/// datetime parsers, which are already unambiguous about the shapes this server
/// accepts. See `render_date_styled` / `render_timestamp_styled` for the
/// per-format layout, all measured against PostgreSQL 14.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateStyleFormat {
    Iso,
    Postgres,
    Sql,
    German,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateStyleOrder {
    Ymd,
    Mdy,
    Dmy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateStyle {
    pub format: DateStyleFormat,
    pub order: DateStyleOrder,
}

impl Default for DateStyle {
    fn default() -> Self {
        DateStyle {
            format: DateStyleFormat::Iso,
            order: DateStyleOrder::Mdy,
        }
    }
}

impl DateStyle {
    /// Parse a `DateStyle` GUC value. Tolerant of case, whitespace, quoting and
    /// a missing order token (`SET datestyle = German` leaves the order at its
    /// current default, which is what PostgreSQL does). An unknown token is
    /// ignored rather than erroring -- the setting is applied when SET, and the
    /// server has no business refusing a query over a spelling it does not know.
    pub fn parse(value: &str) -> Self {
        let mut format = DateStyleFormat::Iso;
        let mut order = DateStyleOrder::Mdy;
        for tok in value.trim().trim_matches('\'').split(',') {
            let t = tok.trim();
            if t.eq_ignore_ascii_case("iso") {
                format = DateStyleFormat::Iso;
            } else if t.eq_ignore_ascii_case("postgres") {
                format = DateStyleFormat::Postgres;
            } else if t.eq_ignore_ascii_case("sql") {
                format = DateStyleFormat::Sql;
            } else if t.eq_ignore_ascii_case("german") {
                format = DateStyleFormat::German;
            } else if t.eq_ignore_ascii_case("ymd") {
                order = DateStyleOrder::Ymd;
            } else if t.eq_ignore_ascii_case("mdy") {
                order = DateStyleOrder::Mdy;
            } else if t.eq_ignore_ascii_case("dmy") {
                order = DateStyleOrder::Dmy;
            }
        }
        DateStyle { format, order }
    }

    /// The canonical spelling PostgreSQL reports over `ParameterStatus` and
    /// answers `SHOW datestyle` with -- capitalised format, comma, order. The
    /// client (psycopg) matches on the leading letter and the trailing order,
    /// so the exact casing is load-bearing.
    pub fn canonical(&self) -> String {
        let f = match self.format {
            DateStyleFormat::Iso => "ISO",
            DateStyleFormat::Postgres => "Postgres",
            DateStyleFormat::Sql => "SQL",
            DateStyleFormat::German => "German",
        };
        let o = match self.order {
            DateStyleOrder::Ymd => "YMD",
            DateStyleOrder::Mdy => "MDY",
            DateStyleOrder::Dmy => "DMY",
        };
        format!("{f}, {o}")
    }

    /// Whether the day comes before the month in the output layout. German is
    /// always day-first; SQL and Postgres are day-first only under `DMY`; ISO
    /// does not use this (it is always `Y-M-D`).
    fn day_first(&self) -> bool {
        match self.format {
            DateStyleFormat::German => true,
            DateStyleFormat::Iso => false,
            DateStyleFormat::Postgres | DateStyleFormat::Sql => self.order == DateStyleOrder::Dmy,
        }
    }
}

const MONTH_ABBR: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DOW_ABBR: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Split a canonical ISO date string (`YYYY-MM-DD`, year possibly wider than 4
/// digits, an optional trailing ` BC`) into `(year, month, day, era)` text
/// parts. Returns `None` for anything that is not that shape (a `-infinity`,
/// say), so the caller can pass it through unchanged.
fn split_iso_date(iso: &str) -> Option<(&str, &str, &str, &str)> {
    let (body, era) = match iso.strip_suffix(" BC") {
        Some(b) => (b, " BC"),
        None => (iso, ""),
    };
    // Split from the RIGHT so a wide year (`10000-01-01`) keeps all its digits.
    let mut it = body.rsplitn(3, '-');
    let da = it.next()?;
    let mo = it.next()?;
    let ye = it.next()?;
    if ye.is_empty()
        || !ye.bytes().all(|b| b.is_ascii_digit())
        || mo.len() != 2
        || da.len() != 2
        || !mo.bytes().all(|b| b.is_ascii_digit())
        || !da.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((ye, mo, da, era))
}

/// Render a canonical ISO date (`render_date_from_pg_days`' output) in the
/// given `DateStyle`. Measured against PostgreSQL 14:
/// - ISO: `2026-09-08`
/// - Postgres: `09-08-2026` (MDY) / `08-09-2026` (DMY)
/// - SQL: `09/08/2026` (MDY) / `08/09/2026` (DMY)
/// - German: `08.09.2026` (always day-first)
pub fn render_date_styled(iso: &str, ds: &DateStyle) -> String {
    if ds.format == DateStyleFormat::Iso {
        return iso.to_string();
    }
    let Some((ye, mo, da, era)) = split_iso_date(iso) else {
        return iso.to_string();
    };
    let sep = match ds.format {
        DateStyleFormat::German => '.',
        DateStyleFormat::Sql => '/',
        _ => '-', // Postgres
    };
    let (a, b) = if ds.day_first() { (da, mo) } else { (mo, da) };
    format!("{a}{sep}{b}{sep}{ye}{era}")
}

/// Render a canonical ISO timestamp (`render_timestamp`' output:
/// `YYYY-MM-DD HH:MM:SS[.frac][ BC]`) in the given `DateStyle`. The time part
/// (and any fractional seconds) is untouched; only the date is restyled, and
/// Postgres style also gains a day-of-week and spelled-out month:
/// - ISO: `2026-09-08 12:34:56.789`
/// - Postgres: `Tue Sep 08 12:34:56.789 2026` (MDY) / `Tue 08 Sep ... 2026` (DMY)
/// - SQL: `09/08/2026 12:34:56.789`
/// - German: `08.09.2026 12:34:56.789`
pub fn render_timestamp_styled(iso: &str, ds: &DateStyle) -> String {
    if ds.format == DateStyleFormat::Iso {
        return iso.to_string();
    }
    // Peel a trailing " BC" so it can be re-appended after the year.
    let (core, era) = match iso.strip_suffix(" BC") {
        Some(b) => (b, " BC"),
        None => (iso, ""),
    };
    let Some((date_part, time_part)) = core.split_once(' ') else {
        return iso.to_string();
    };
    let Some((ye, mo, da, _)) = split_iso_date(date_part) else {
        return iso.to_string();
    };
    match ds.format {
        DateStyleFormat::Sql => {
            let (a, b) = if ds.day_first() { (da, mo) } else { (mo, da) };
            format!("{a}/{b}/{ye} {time_part}{era}")
        }
        DateStyleFormat::German => format!("{da}.{mo}.{ye} {time_part}{era}"),
        DateStyleFormat::Postgres => {
            let mon = mo
                .parse::<usize>()
                .ok()
                .and_then(|m| MONTH_ABBR.get(m.wrapping_sub(1)).copied())
                .unwrap_or(mo);
            let dow = dow_abbr(ye, mo, da).unwrap_or("");
            let dow_sp = if dow.is_empty() { "" } else { " " };
            if ds.day_first() {
                format!("{dow}{dow_sp}{da} {mon} {time_part} {ye}{era}")
            } else {
                format!("{dow}{dow_sp}{mon} {da} {time_part} {ye}{era}")
            }
        }
        DateStyleFormat::Iso => unreachable!(),
    }
}

/// The 3-letter English day-of-week for an ISO Y/M/D, or `None` for a year
/// chrono cannot represent (a BC or >4-digit year -- Postgres output for those
/// is a feature gap this server does not reach, so an empty DoW is harmless).
fn dow_abbr(ye: &str, mo: &str, da: &str) -> Option<&'static str> {
    use chrono::Datelike;
    let y = ye.parse::<i32>().ok()?;
    let m = mo.parse::<u32>().ok()?;
    let d = da.parse::<u32>().ok()?;
    let date = NaiveDate::from_ymd_opt(y, m, d)?;
    // chrono's Sunday-based weekday number (Sun=0).
    DOW_ABBR
        .get(date.weekday().num_days_from_sunday() as usize)
        .copied()
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
    /// User-defined types (enums, today): `(name, oid)`, set by the wire
    /// layer per statement from the shared store. The planner is pure; the
    /// catalog is not, so the catalog comes TO the planner.
    static PLAN_USER_TYPES: std::cell::RefCell<Vec<(String, i64, Vec<String>)>> =
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
    plan_with_session_types_and_subqueries(sql, lookup, params, param_types, timezone, None)
}

/// `plan_with_session_types`, optionally able to run an uncorrelated
/// subquery. `None` keeps the old behaviour exactly.
pub fn plan_with_session_types_and_subqueries(
    sql: &str,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
    param_types: &[Option<String>],
    timezone: &TimeZoneSetting,
    run: Option<SubqueryRunner<'_>>,
) -> Result<Statement> {
    let previous = PLAN_TIMEZONE.with(|t| t.replace(timezone.clone()));
    let previous_types = PLAN_PARAM_TYPES.with(|t| t.replace(param_types.to_vec()));
    let out = match run {
        Some(run) => plan_with_subqueries(sql, lookup, params, run),
        None => plan_with_params(sql, lookup, params),
    };
    PLAN_TIMEZONE.with(|t| *t.borrow_mut() = previous);
    PLAN_PARAM_TYPES.with(|t| *t.borrow_mut() = previous_types);
    out
}

thread_local! {
    /// The session user, installed per statement by the wire layer: the one
    /// role this server can vouch for (an `aclitem` grantee / grantor).
    static PLAN_SESSION_USER: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    /// WARNINGs the statement in flight raised, as `(sqlstate, message)`; the
    /// wire layer drains them into NoticeResponses.
    static PLAN_WARNINGS: std::cell::RefCell<Vec<(String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Install the session user for the statements that follow on this thread.
pub fn set_session_user(user: Option<String>) {
    PLAN_SESSION_USER.with(|u| *u.borrow_mut() = user);
}

thread_local! {
    /// The database this connection is on, and its GUC settings.
    ///
    /// `current_database()` and `current_setting()` already worked as a BARE
    /// select-list target, where they become a `ConstCol` the server
    /// resolves. Inside an EXPRESSION -- `current_database() IS NOT NULL`,
    /// `current_setting('x') ~ '...'` -- the constant evaluator reached them
    /// instead and had nowhere to ask, so both answered `0A000`. This is the
    /// same thread-local the session user and the timezone already use.
    static PLAN_SESSION_DB: std::cell::RefCell<String> =
        const { std::cell::RefCell::new(String::new()) };
    static PLAN_SETTINGS: std::cell::RefCell<std::collections::HashMap<String, String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Install the database and settings for the statements that follow.
pub fn set_session_context(database: &str, settings: std::collections::HashMap<String, String>) {
    PLAN_SESSION_DB.with(|d| *d.borrow_mut() = database.to_string());
    PLAN_SETTINGS.with(|s| *s.borrow_mut() = settings);
}

pub(crate) fn session_database() -> String {
    PLAN_SESSION_DB.with(|d| d.borrow().clone())
}

pub(crate) fn session_setting(name: &str) -> Option<String> {
    PLAN_SETTINGS.with(|s| s.borrow().get(name).cloned())
}

pub(crate) fn session_user() -> Option<String> {
    PLAN_SESSION_USER.with(|u| u.borrow().clone())
}

pub(crate) fn warn(sqlstate: &str, message: String) {
    PLAN_WARNINGS.with(|w| w.borrow_mut().push((sqlstate.to_string(), message)));
}

/// The WARNINGs raised since the last call, as `(sqlstate, message)`.
pub fn take_warnings() -> Vec<(String, String)> {
    PLAN_WARNINGS.with(|w| std::mem::take(&mut *w.borrow_mut()))
}

/// Install the user-defined types for the statements that follow on this
/// thread. The wire layer reads them from the shared store per statement.
pub fn set_user_types(types: Vec<(String, i64, Vec<String>)>) {
    PLAN_USER_TYPES.with(|t| *t.borrow_mut() = types);
}

thread_local! {
    /// Custom range types: (range name -> subtype element name), installed per
    /// statement by the wire layer, so a `'[1,5)'::myrange` cast resolves its
    /// element without a per-call lookup.
    static PLAN_USER_RANGES: std::cell::RefCell<Vec<(String, String, i64)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Custom range types: `(range name, subtype element, oid)`.
pub fn set_user_ranges(ranges: Vec<(String, String, i64)>) {
    PLAN_USER_RANGES.with(|t| *t.borrow_mut() = ranges);
}

thread_local! {
    /// Composite types: `(resolution name, oid, [(field name, field type)])`,
    /// installed per statement by the wire layer so a `'(1,x)'::testcomp` cast
    /// or a `row(1,'x')::testcomp` record cast resolves its field types without
    /// a per-call catalog read. Composites carry no labels, so they are held
    /// apart from the enum-shaped `PLAN_USER_TYPES` (whose empty label list would
    /// otherwise route a composite cast into the enum arm).
    static PLAN_USER_COMPOSITES: std::cell::RefCell<Vec<CompositeType>> =
        const { std::cell::RefCell::new(Vec::new()) };

    /// Custom MULTIRANGE types: `(resolution name -> multirange oid)`. The
    /// companion multirange PostgreSQL auto-creates for every `CREATE TYPE ...
    /// AS RANGE`. The resolution name is bare in `public` and `schema.name`
    /// otherwise, so `to_regtype('testmultirange')` reaches the public one and
    /// `to_regtype('testschema.testmultirange')` the schema one -- exactly like
    /// ranges. Installed per statement by the wire layer.
    static PLAN_USER_MULTIRANGES: std::cell::RefCell<Vec<(String, i64, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A composite type's fields: `(field name, field type name)`, in order.
pub type CompositeFields = Vec<(String, String)>;

/// A composite type as the planner holds it: `(resolution name, oid, fields)`.
pub type CompositeType = (String, i64, CompositeFields);

/// Composite types: `(resolution name, oid, [(field name, field type)])`.
pub fn set_user_composites(composites: Vec<CompositeType>) {
    PLAN_USER_COMPOSITES.with(|t| *t.borrow_mut() = composites);
}

/// A composite type's `(oid, fields)` by name, folded exactly as `user_enum`:
/// a quoted name keeps its case, a bare one folds to lower.
fn user_composite(name: &str) -> Option<(i64, CompositeFields)> {
    let target = canonical_type_ref(name);
    let trimmed = name.trim();
    let fold = !(trimmed.starts_with('"') || trimmed.contains('"'));
    PLAN_USER_COMPOSITES.with(|t| {
        t.borrow()
            .iter()
            .find(|(n, _, _)| *n == target || (fold && n.eq_ignore_ascii_case(&target)))
            .map(|(_, oid, fields)| (*oid, fields.clone()))
    })
}

/// A composite type's oid by name -- the door `user_type_oid` misses because
/// composites are not in `PLAN_USER_TYPES`.
pub fn user_composite_oid(name: &str) -> Option<i64> {
    user_composite(name).map(|(oid, _)| oid)
}

/// Custom multirange types: `(resolution name, multirange oid, member range
/// name)`.
pub fn set_user_multiranges(multiranges: Vec<(String, i64, String)>) {
    PLAN_USER_MULTIRANGES.with(|t| *t.borrow_mut() = multiranges);
}

thread_local! {
    /// Base types from `CREATE TYPE name` / `CREATE TYPE name (input = ...,
    /// output = ...)`: `(resolution name, oid, defined)`. `defined` is false
    /// while the type is still a SHELL -- a name with no representation yet,
    /// which PostgreSQL lets I/O functions refer to but nothing else use.
    /// Installed per statement by the wire layer.
    static PLAN_USER_BASE_TYPES: std::cell::RefCell<Vec<(String, i64, bool)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Base types: `(resolution name, oid, defined)`.
pub fn set_user_base_types(types: Vec<(String, i64, bool)>) {
    PLAN_USER_BASE_TYPES.with(|t| *t.borrow_mut() = types);
}

/// The base types an EXTENSION owns, whose values this server parses and
/// renders itself rather than passing text through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionType {
    /// `hstore`'s key/value map.
    Hstore,
    /// PostGIS's `geometry`.
    Geometry,
}

impl ExtensionType {
    /// The type's name in `pg_type`.
    pub fn type_name(self) -> &'static str {
        match self {
            ExtensionType::Hstore => "hstore",
            ExtensionType::Geometry => "geometry",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "hstore" => Some(ExtensionType::Hstore),
            "geometry" => Some(ExtensionType::Geometry),
            _ => None,
        }
    }
}

thread_local! {
    /// The names of the base types installed by `CREATE EXTENSION`, a subset
    /// of `PLAN_USER_BASE_TYPES`. Only these get the extension's parser: a
    /// user's own `CREATE TYPE hstore (...)` stays a text pass-through.
    static PLAN_EXTENSION_TYPES: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The base types owned by installed extensions, by resolution name.
pub fn set_extension_types(names: Vec<String>) {
    PLAN_EXTENSION_TYPES.with(|t| *t.borrow_mut() = names);
}

/// The extension behind a type name (`hstore`, `public.geometry`,
/// `hstore[]`'s element), if that extension's type is installed.
pub fn extension_type(name: &str) -> Option<ExtensionType> {
    let trimmed = name.trim();
    let element = trimmed
        .strip_suffix("[]")
        .map(str::trim_end)
        .unwrap_or(trimmed);
    let (registered, _, defined) = user_base_type(element)?;
    if !defined {
        return None;
    }
    let installed = PLAN_EXTENSION_TYPES.with(|t| t.borrow().contains(&registered));
    if !installed {
        return None;
    }
    ExtensionType::from_name(registered.rsplit('.').next().unwrap_or(&registered))
}

/// An extension type's input function: any accepted text form to the
/// canonical text this server stores.
pub fn extension_canonical(ty: ExtensionType, text: &str) -> Result<String> {
    match ty {
        ExtensionType::Hstore => hstore::canonical(text),
        ExtensionType::Geometry => geometry::canonical(text),
    }
}

/// A base type's `(registered name, oid, defined)` by name, folded exactly as
/// `user_composite`: a quoted name keeps its case, a bare one folds to lower.
/// The registered name is the one to print -- the fold has lowercased the
/// caller's spelling.
fn user_base_type(name: &str) -> Option<(String, i64, bool)> {
    let target = canonical_type_ref(name);
    let trimmed = name.trim();
    let fold = !(trimmed.starts_with('"') || trimmed.contains('"'));
    PLAN_USER_BASE_TYPES.with(|t| {
        t.borrow()
            .iter()
            .find(|(n, _, _)| *n == target || (fold && n.eq_ignore_ascii_case(&target)))
            .map(|(n, oid, defined)| (n.clone(), *oid, *defined))
    })
}

/// A base type's resolution NAME by oid -- the reverse door, for rendering
/// `oid::regtype::text` (which PostgreSQL renders for a shell too).
fn user_base_type_name(oid: i64) -> Option<String> {
    PLAN_USER_BASE_TYPES.with(|t| {
        t.borrow()
            .iter()
            .find(|(_, o, _)| *o == oid)
            .map(|(n, _, _)| n.clone())
    })
}

/// Is `name` -- or the element of `name[]` -- a SHELL type? The bare name,
/// for the `type "x" is only a shell` message.
fn shell_type_named(name: &str) -> Option<String> {
    let trimmed = name.trim();
    let element = trimmed
        .strip_suffix("[]")
        .map(str::trim_end)
        .unwrap_or(trimmed);
    match user_base_type(element) {
        Some((name, _, false)) => Some(name),
        _ => None,
    }
}

/// The custom RANGE a custom multirange is built from, by resolution name.
pub fn user_multirange_member(name: &str) -> Option<String> {
    let n = canonical_type_ref(name);
    PLAN_USER_MULTIRANGES.with(|t| {
        t.borrow()
            .iter()
            .find(|(rn, _, _)| *rn == n)
            .map(|(_, _, member)| member.clone())
    })
}

/// A custom multirange type's oid by name, for regtype resolution.
fn user_multirange_oid(name: &str) -> Option<i64> {
    let n = canonical_type_ref(name);
    PLAN_USER_MULTIRANGES.with(|t| {
        t.borrow()
            .iter()
            .find(|(rn, _, _)| *rn == n)
            .map(|(_, oid, _)| *oid)
    })
}

/// A custom multirange type's resolution NAME by oid -- the reverse door, for
/// rendering `oid::regtype::text`.
fn user_multirange_name(oid: i64) -> Option<String> {
    PLAN_USER_MULTIRANGES.with(|t| {
        t.borrow()
            .iter()
            .find(|(_, o, _)| *o == oid)
            .map(|(n, _, _)| n.clone())
    })
}

/// The subtype element of a custom range type by name, if one is registered.
pub fn user_range_subtype(name: &str) -> Option<String> {
    // Canonicalise so a schema-qualified `testschema.testrange` (and its quoted
    // `"testschema"."testrange"` form) resolves to its own registered entry,
    // distinct from a bare `testrange`.
    let n = canonical_type_ref(name);
    PLAN_USER_RANGES.with(|t| {
        t.borrow()
            .iter()
            .find(|(rn, _, _)| *rn == n)
            .map(|(_, sub, _)| sub.clone())
    })
}

/// A custom range type's oid by name, for regtype resolution.
fn user_range_oid(name: &str) -> Option<i64> {
    let n = canonical_type_ref(name);
    PLAN_USER_RANGES.with(|t| {
        t.borrow()
            .iter()
            .find(|(rn, _, _)| *rn == n)
            .map(|(_, _, oid)| *oid)
    })
}

/// A user type's oid by name, quoted or bare -- the bare form FOLDS, exactly
/// as `oid_of_name` does for builtins.
/// One identifier part of a possibly-qualified type reference: a quoted part
/// keeps its case, an unquoted part folds to lower (PostgreSQL's rule).
fn normalize_ident_part(part: &str) -> String {
    let p = part.trim();
    match p.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Some(inner) => inner.replace("\"\"", "\""),
        None => p.to_ascii_lowercase(),
    }
}

/// Canonicalise a type reference to the resolution key composites register
/// under: a bare `name` (unqualified, or explicitly `public`) or `schema.name`.
/// Handles quoted parts (`"testschema"."testcomp"`, from `sql.Identifier`) and
/// splits on a dot only outside quotes.
fn canonical_type_ref(name: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = name.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_quotes && chars.peek() == Some(&'"') {
                    cur.push('"');
                    cur.push('"');
                    chars.next();
                } else {
                    cur.push('"');
                    in_quotes = !in_quotes;
                }
            }
            '.' if !in_quotes => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    let norm: Vec<String> = parts.iter().map(|p| normalize_ident_part(p)).collect();
    match norm.as_slice() {
        [schema, name] if schema == "public" => name.clone(),
        [schema, name] => format!("{schema}.{name}"),
        _ => norm.join("."),
    }
}

/// A user type's ARRAY type is `element oid + this`, the rule the wire layer
/// mints `typarray` by for every enum / composite / range / multirange, so the
/// planner can resolve `to_regtype('mood[]')` without a catalog read.
pub const USER_TYPE_ARRAY_OID_OFFSET: i64 = 100_000;

/// `to_regtype` on a user type name: `mood`, `mood[]`, or the internal `_mood`
/// spelling PostgreSQL accepts for an array type (`to_regtype('_rt1')` renders
/// `rt1[]`). Composites (a table's row type included) resolve through their own
/// registry because they are not in `PLAN_USER_TYPES`.
fn user_type_or_array_oid(name: &str) -> Option<i64> {
    let trimmed = name.trim();
    let element = if let Some(e) = trimmed.strip_suffix("[]") {
        Some(e.trim_end())
    } else if !trimmed.starts_with('"') && trimmed.starts_with('_') {
        Some(&trimmed[1..])
    } else {
        None
    };
    match element {
        Some(e) => user_type_oid(e)
            .or_else(|| user_composite_oid(e))
            .map(|oid| oid + USER_TYPE_ARRAY_OID_OFFSET),
        None => user_type_oid(trimmed).or_else(|| user_composite_oid(trimmed)),
    }
}

fn user_type_oid(name: &str) -> Option<i64> {
    let target = canonical_type_ref(name);
    PLAN_USER_TYPES
        .with(|t| {
            t.borrow()
                .iter()
                .find(|(n, _, _)| *n == target)
                .map(|(_, oid, _)| *oid)
        })
        .or_else(|| user_range_oid(name))
        .or_else(|| user_multirange_oid(name))
        // A shell has an oid but no representation: `to_regtype` answers NULL
        // for it and `::regtype` refuses it (measured on 16), so only a
        // DEFINED base type resolves here.
        .or_else(|| match user_base_type(name) {
            Some((_, oid, true)) => Some(oid),
            _ => None,
        })
}

/// A user ENUM's `(oid, labels)` by name, same folding rule.
fn user_enum(name: &str) -> Option<(i64, Vec<String>)> {
    let trimmed = name.trim();
    let (target, fold) = match trimmed.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Some(inner) => (inner.to_string(), false),
        None => (trimmed.to_ascii_lowercase(), true),
    };
    PLAN_USER_TYPES.with(|t| {
        t.borrow()
            .iter()
            .find(|(n, _, _)| {
                if fold {
                    n.to_ascii_lowercase() == target
                } else {
                    *n == target
                }
            })
            .map(|(_, oid, labels)| (*oid, labels.clone()))
    })
}

/// A user type's NAME by oid -- the reverse door, for rendering a regtype
/// or naming the type behind a custom oid on a binary parameter.
/// Consults enums / composites first, then custom multiranges (a multirange oid
/// is not in `PLAN_USER_TYPES`), so `mr_oid::regtype::text` renders its name.
pub fn user_type_name(oid: i64) -> Option<String> {
    PLAN_USER_TYPES
        .with(|t| {
            t.borrow()
                .iter()
                .find(|(_, o, _)| *o == oid)
                .map(|(n, _, _)| n.clone())
        })
        .or_else(|| user_range_name(oid))
        .or_else(|| user_multirange_name(oid))
        .or_else(|| user_base_type_name(oid))
}

/// A custom range type's resolution NAME by oid -- for rendering `pg_typeof`
/// of a custom range, which is its oid until this door names it.
fn user_range_name(oid: i64) -> Option<String> {
    PLAN_USER_RANGES.with(|t| {
        t.borrow()
            .iter()
            .find(|(_, _, o)| *o == oid)
            .map(|(n, _, _)| n.clone())
    })
}

/// The declared type of `$n`, when the client gave one.
fn declared_param_type(n: usize) -> Option<String> {
    PLAN_PARAM_TYPES.with(|t| t.borrow().get(n.checked_sub(1)?).cloned().flatten())
}

/// Cast a TEXT representation to a declared type, with the session zone in
/// force. The public door onto `cast_value` for the wire layer, which has text
/// from a client and a declared oid and needs the same value a literal of that
/// type would produce.
/// `regexp_replace(source, pattern, replacement [, flags])`.
///
/// The pattern language is POSIX ARE; the `regex` crate covers the subset any
/// measured client sends (`\d`, classes, anchors, alternation). Flags: `i`
/// case-insensitive, `g` replace ALL occurrences -- without `g` PostgreSQL
/// replaces only the FIRST, which is not most regex libraries' default.
fn regexp_replace(args: &[Bson]) -> Result<Bson> {
    if !(3..=4).contains(&args.len()) {
        return Err(Error::UndefinedFunction(
            "function regexp_replace() does not exist with that argument list".into(),
        ));
    }
    if args.contains(&Bson::Null) {
        return Ok(Bson::Null);
    }
    let text = |v: &Bson| match v {
        Bson::String(s) => Ok(s.clone()),
        other => Err(Error::Unsupported(format!(
            "a {} argument to regexp_replace()",
            bson_kind(other)
        ))),
    };
    let source = text(&args[0])?;
    let pattern = text(&args[1])?;
    let replacement = text(&args[2])?;
    let flags = args.get(3).map(&text).transpose()?.unwrap_or_default();
    let mut builder = String::new();
    if flags.contains('i') {
        builder.push_str("(?i)");
    }
    builder.push_str(&pattern);
    let re = regex::Regex::new(&builder)
        .map_err(|_| Error::InvalidRegex(format!("invalid regular expression: \"{pattern}\"")))?;
    // PostgreSQL's `\1` group references are the regex crate's `${1}`, and its
    // `\&` -- the WHOLE match -- is `${0}`. Without the `\&` case the escape
    // passed through literally, so `regexp_replace('abc', 'b', '\&\&')` gave
    // `a\&\&c` where PostgreSQL gives `abbc`. An UNKNOWN escape (`\q`) stays
    // as written, which is also what PostgreSQL does.
    let replacement = {
        let mut out = String::new();
        let mut chars = replacement.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.peek() {
                    Some(d) if d.is_ascii_digit() => {
                        out.push_str("${");
                        out.push(*d);
                        out.push('}');
                        chars.next();
                        continue;
                    }
                    Some('&') => {
                        out.push_str("${0}");
                        chars.next();
                        continue;
                    }
                    Some('\\') => {
                        out.push('\\');
                        chars.next();
                        continue;
                    }
                    _ => {}
                }
            } else if c == '$' {
                out.push_str("$$");
                continue;
            }
            out.push(c);
        }
        out
    };
    Ok(Bson::String(if flags.contains('g') {
        re.replace_all(&source, replacement.as_str()).into_owned()
    } else {
        re.replace(&source, replacement.as_str()).into_owned()
    }))
}

/// Apply one computed-column expression to a row's stored value. The wire
/// layer's door: the executor holds rows and this holds the evaluators.
pub fn apply_column_expr(expr: &ColumnExpr, value: Bson, tz: &TimeZoneSetting) -> Result<Bson> {
    match expr {
        ColumnExpr::Casts { source, chain } => {
            let mut v = value;
            let mut prev: Option<&str> = match source.as_deref() {
                Some("timestamptz") | Some("timestamp with time zone") => Some("timestamptz"),
                _ => None,
            };
            for target in chain {
                // timestamptz -> text renders the instant in the session zone.
                if target == "text" && prev == Some("timestamptz") {
                    if let Some(t) = timestamptz_value_text(&v, tz) {
                        v = Bson::String(t);
                        prev = Some(target);
                        continue;
                    }
                }
                v = cast_value_with_tz(v, target, tz)?;
                prev = Some(target);
            }
            Ok(v)
        }
        ColumnExpr::Call { name, args, .. } => {
            let filled: Vec<Bson> = args
                .iter()
                .map(|a| a.clone().unwrap_or_else(|| value.clone()))
                .collect();
            if name == "regexp_replace" {
                return regexp_replace(&filled);
            }
            scalar::call(name, &filled)
                .unwrap_or_else(|| Err(Error::Unsupported(format!("function {name}()"))))
        }
        ColumnExpr::Coalesce { args } => {
            // The column position (`None`) takes the row's value; the first
            // non-NULL argument in order wins, else NULL.
            for a in args {
                let v = a.clone().unwrap_or_else(|| value.clone());
                if v != Bson::Null {
                    return Ok(v);
                }
            }
            Ok(Bson::Null)
        }
        // A literal ignores the row and returns its constant.
        ColumnExpr::Const { value, .. } => Ok(value.clone()),
        // A row expression over ONE column: the executor's doc-aware path
        // (`apply_row_expr`) is the normal route; this one serves a caller
        // that has only the one value.
        ColumnExpr::Row { fields, .. } => {
            let mut row = Document::new();
            if let Some((_, f, _)) = fields.first() {
                row.insert(f.clone(), value);
            }
            apply_row_expr(expr, &row)
        }
    }
}

/// The type a ColumnExpr's column reads back as.
pub fn column_expr_type(expr: &ColumnExpr) -> &str {
    match expr {
        ColumnExpr::Casts { chain, .. } => chain.last().map(String::as_str).unwrap_or("text"),
        ColumnExpr::Call { result_type, .. } => result_type,
        // A coalesce keeps its column's type; join_output_def resolves that
        // from the side column, so this fallback is not used for typing.
        ColumnExpr::Coalesce { .. } => "text",
        ColumnExpr::Const { result_type, .. } => result_type,
        ColumnExpr::Row { result_type, .. } => result_type,
    }
}

/// The PostgreSQL type name a literal select-list constant reports.
fn const_col_type(v: &Bson) -> &'static str {
    match v {
        Bson::Boolean(_) => "bool",
        Bson::Int32(_) => "int4",
        Bson::Int64(_) => "int8",
        Bson::Double(_) => "float8",
        _ => "text",
    }
}

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
        // A date's `-` never follows a `:` or a space-separated time. With no
        // time at all, a `+` still starts an offset (a date has none), and so
        // does a `-` set off by a space: `'2001-01-01 +05'` is midnight at
        // UTC+5, which ignoring the offset silently moved by five hours.
        if !head.contains(':') && c != '+' && !head.ends_with(char::is_whitespace) {
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

/// Build a stored `timestamptz` VALUE from an absolute instant in microseconds
/// since the Unix epoch -- the same carrier `cast_value("timestamptz")`
/// produces: a bare BSON date when the instant lands on a whole millisecond, a
/// `{__subms_d, __subms_u}` composite when it carries sub-millisecond digits.
///
/// The binary wire form of a `timestamptz` parameter hands us the instant
/// outright (i64 microseconds since 2000-01-01 UTC). Rendering it to
/// session-zone text and shipping THAT as the value dropped the offset the
/// moment anything re-coerced the string as a bare timestamp, so a binary
/// parameter compared UNEQUAL to the very literal it was meant to equal. Going
/// straight to the instant carrier keeps the binary and text paths on the one
/// representation.
pub fn timestamptz_value_from_micros(micros: i64) -> Bson {
    let (ms, rem) = split_subms(micros);
    let date = Bson::DateTime(bson::DateTime::from_millis(ms));
    if rem == 0 {
        date
    } else {
        Bson::Document(doc! { COMPOSITE_DATE: date, COMPOSITE_US: rem })
    }
}

/// An instant as PostgreSQL renders a `timestamptz`: the wall clock in the
/// session zone, then the offset that zone had at that instant.
pub fn render_timestamptz(micros: i64, tz: &TimeZoneSetting) -> String {
    let offset = tz.offset_at(micros);
    let seconds = offset.local_minus_utc();
    let local = micros + i64::from(seconds) * 1_000_000;
    format!("{}{}", render_timestamp(local), render_offset(seconds))
}

/// Render a `timestamptz` instant in the session zone AND the session
/// `DateStyle`. ISO keeps the numeric-offset form `render_timestamptz`
/// produces; the non-ISO styles restyle the local timestamp and append the
/// zone's ABBREVIATION rather than a numeric offset -- which is exactly what
/// PostgreSQL does, and what makes psycopg (whose non-ISO timestamptz loader
/// cannot parse zone names) raise the `NotImplementedError` its own suite
/// expects. Measured against PostgreSQL 14: `Tue Sep 08 12:34:56.789 2026 UTC`,
/// `09/08/2026 12:34:56.789 UTC`, `08.09.2026 12:34:56.789 UTC`.
pub fn render_timestamptz_styled(micros: i64, tz: &TimeZoneSetting, ds: &DateStyle) -> String {
    if ds.format == DateStyleFormat::Iso {
        return render_timestamptz(micros, tz);
    }
    let offset = tz.offset_at(micros);
    let seconds = offset.local_minus_utc();
    let local = micros + i64::from(seconds) * 1_000_000;
    let styled = render_timestamp_styled(&render_timestamp(local), ds);
    format!("{styled} {}", tz_abbreviation(tz, micros, seconds))
}

/// The zone abbreviation PostgreSQL prints in a non-ISO `timestamptz`: `UTC`
/// for UTC, the named zone's abbreviation (`BST`, `CET`, ...) at that instant,
/// and a numeric offset for a bare fixed-offset zone (which has no name).
fn tz_abbreviation(tz: &TimeZoneSetting, micros: i64, seconds: i32) -> String {
    use chrono::TimeZone;
    match tz {
        TimeZoneSetting::Utc => "UTC".to_string(),
        TimeZoneSetting::Fixed(_) => render_offset(seconds),
        TimeZoneSetting::Named(zone) => {
            let instant = chrono::DateTime::from_timestamp_micros(micros).unwrap_or_default();
            zone.from_utc_datetime(&instant.naive_utc())
                .format("%Z")
                .to_string()
        }
    }
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

/// PostgreSQL's `date` binary form: signed days since 2000-01-01.
///
/// The inverse of `render_date_from_pg_days`. Returns `None` for a value the
/// binary form cannot hold (a year outside chrono's range, a BC date rendered
/// with an era suffix); the caller then errors rather than sending wrong bytes,
/// exactly as `encode_binary` does for any type it cannot render.
pub fn date_to_pg_days(text: &str) -> Option<i32> {
    // `date_send` sends the two infinities as the extreme day counts, and a
    // BC date as a negative count (the proleptic year `1 - y`).
    let t = text.trim();
    match t.to_ascii_lowercase().as_str() {
        "infinity" => return Some(i32::MAX),
        "-infinity" => return Some(i32::MIN),
        _ => {}
    }
    let d = parse_date_era(t)?;
    let epoch = NaiveDate::from_ymd_opt(2000, 1, 1)?;
    i32::try_from(d.signed_duration_since(epoch).num_days()).ok()
}

/// A `YYYY-MM-DD` or `YYYY-MM-DD BC` date as a chrono date (BC through the
/// proleptic year `1 - y`, which is how chrono counts before year 1).
fn parse_date_era(t: &str) -> Option<NaiveDate> {
    let (body, bc) = match t.strip_suffix(" BC").or_else(|| t.strip_suffix(" bc")) {
        Some(body) => (body.trim_end(), true),
        None => (t, false),
    };
    let mut parts = body.splitn(3, '-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    NaiveDate::from_ymd_opt(if bc { 1 - y } else { y }, m, d)
}

/// PostgreSQL's `timetz` binary form from the canonical text: microseconds
/// since midnight, and the zone as seconds WEST of UTC (`timetz_send` sends
/// the stored `zone`, whose sign is the reverse of the printed offset:
/// `12:00:00+05:30` carries `-19800`).
pub fn timetz_to_pg_wire(text: &str) -> Option<(i64, i32)> {
    let (body, offset) = split_trailing_offset(text);
    let micros = time_to_pg_micros(&body)?;
    Some((micros, -offset.unwrap_or(0)))
}

/// PostgreSQL's `timestamp` / `timestamptz` binary form from CANONICAL TEXT:
/// microseconds since 2000-01-01, the naive text read as UTC (which is how a
/// range bound is stored: a `tstzrange` keeps its bounds as the naive UTC
/// wall clock). `infinity` / `-infinity` are the wire sentinels, a `... BC`
/// timestamp counts back through the proleptic calendar, a wide year
/// (`10000-01-01 12:00:00`) counts forward past what chrono's `%Y` parses,
/// and a trailing offset (`+00:00`, the pass-through text a wide/BC
/// `timestamptz` keeps) is applied -- PostgreSQL sends every one of these in
/// binary, and the client's loader is what decides it cannot hold them.
pub fn timestamp_text_to_pg_micros(text: &str) -> Option<i64> {
    const EPOCH_2000_US: i64 = 946_684_800 * 1_000_000;
    let t = text.trim();
    match t.to_ascii_lowercase().as_str() {
        "infinity" => return Some(i64::MAX),
        "-infinity" => return Some(i64::MIN),
        _ => {}
    }
    let (body, bc) = match t.strip_suffix(" BC").or_else(|| t.strip_suffix(" bc")) {
        Some(body) => (body.trim_end(), true),
        None => (t, false),
    };
    let (body, offset) = split_trailing_offset(body);
    let (date, time) = body
        .trim()
        .split_once([' ', 'T'])
        .unwrap_or((body.trim(), "00:00:00"));
    let d = parse_date_era(&format!("{date}{}", if bc { " BC" } else { "" }))?;
    let us = time_to_pg_micros(time)?;
    let midnight = d.and_hms_opt(0, 0, 0)?.and_utc().timestamp_micros();
    midnight
        .checked_add(us)?
        .checked_sub(i64::from(offset.unwrap_or(0)) * 1_000_000)?
        .checked_sub(EPOCH_2000_US)
}

/// A stored `tstzrange` bound (naive UTC text) as PostgreSQL prints it: the
/// wall clock in the session zone with that zone's offset. The infinities
/// pass through unchanged.
pub fn utc_text_in_zone(text: &str, tz: &TimeZoneSetting) -> Option<String> {
    let t = text.trim();
    if matches!(t.to_ascii_lowercase().as_str(), "infinity" | "-infinity") {
        return Some(t.to_ascii_lowercase());
    }
    if t.to_ascii_lowercase().ends_with(" bc") {
        return None;
    }
    Some(render_timestamptz(parse_timestamp(t).ok()?, tz))
}

/// PostgreSQL's `time` binary form: microseconds since midnight.
///
/// The inverse of `render_time_from_micros`.
pub fn time_to_pg_micros(text: &str) -> Option<i64> {
    // `24:00:00` is a valid end-of-day `time` (86_400_000_000 on the wire,
    // measured on 16); chrono has no hour 24, so it is the one clock reading
    // spelled out here.
    if let Some(rest) = text.trim().strip_prefix("24:") {
        if rest.chars().all(|c| c == '0' || c == ':' || c == '.') {
            return Some(86_400_000_000);
        }
    }
    let t = NaiveTime::parse_from_str(text.trim(), "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(text.trim(), "%H:%M"))
        .ok()?;
    let secs = i64::from(t.num_seconds_from_midnight());
    let us = i64::from(t.nanosecond()) / 1000;
    Some(secs * 1_000_000 + us)
}

/// PostgreSQL's `timestamp` / `timestamptz` binary form: microseconds since
/// 2000-01-01. Both types store a UTC-instant carrier (`Bson::DateTime` or the
/// sub-millisecond composite), so one converter serves both. `infinity` /
/// `-infinity` (kept as text) map to the sentinels PostgreSQL uses on the wire;
/// BC / wide-year specials the micros form cannot hold return `None`.
pub fn timestamp_bson_to_pg_micros(v: &Bson) -> Option<i64> {
    // 2000-01-01T00:00:00Z is 946684800 seconds after the Unix epoch.
    const EPOCH_2000_US: i64 = 946_684_800 * 1_000_000;
    match v {
        Bson::DateTime(d) => Some(d.timestamp_millis() * 1000 - EPOCH_2000_US),
        Bson::Document(doc) if doc.contains_key(COMPOSITE_DATE) => {
            let ms = match doc.get(COMPOSITE_DATE) {
                Some(Bson::DateTime(d)) => d.timestamp_millis(),
                _ => return None,
            };
            let us = doc.get(COMPOSITE_US).and_then(|v| v.as_i32()).unwrap_or(0);
            Some(ms * 1000 + i64::from(us) - EPOCH_2000_US)
        }
        Bson::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "infinity" => Some(i64::MAX),
            "-infinity" => Some(i64::MIN),
            _ => None,
        },
        _ => None,
    }
}

/// A timestamp VALUE as PostgreSQL's text, whether it arrived as a BSON date
/// or as the composite that carries sub-millisecond digits.
///
/// A stored timestamp is reassembled from its column plus a hidden companion
/// field, which the row path already did. A timestamp that is a CONSTANT never
/// touches a row, so it reached the wire as a composite document that the
/// encoder had no arm for — and `select '2026-01-01 12:00'::timestamp`
/// answered NULL while the same value through a column answered correctly.
/// Render a stored timestamptz INSTANT (a `Bson::DateTime` or the sub-ms
/// composite) as PostgreSQL renders it in the SESSION zone. The wire layer
/// calls this for a column / expression of type `timestamptz` (oid 1184); the
/// naive `timestamp_value_text` is for `timestamp` (1114).
pub fn timestamptz_value_text(v: &Bson, tz: &TimeZoneSetting) -> Option<String> {
    let micros = match v {
        Bson::DateTime(d) => d.timestamp_millis() * 1000,
        Bson::Document(doc) if doc.contains_key(COMPOSITE_DATE) => {
            let ms = match doc.get(COMPOSITE_DATE) {
                Some(Bson::DateTime(d)) => d.timestamp_millis(),
                _ => return None,
            };
            let us = doc.get(COMPOSITE_US).and_then(|v| v.as_i32()).unwrap_or(0);
            ms * 1000 + i64::from(us)
        }
        _ => return None,
    };
    Some(render_timestamptz(micros, tz))
}

/// `timestamptz_value_text`, restyled for the session `DateStyle`. ISO is
/// identical to the plain form; the non-ISO styles restyle the local wall
/// clock and append the zone abbreviation (see `render_timestamptz_styled`).
pub fn timestamptz_value_text_styled(
    v: &Bson,
    tz: &TimeZoneSetting,
    ds: &DateStyle,
) -> Option<String> {
    let micros = match v {
        Bson::DateTime(d) => d.timestamp_millis() * 1000,
        Bson::Document(doc) if doc.contains_key(COMPOSITE_DATE) => {
            let ms = match doc.get(COMPOSITE_DATE) {
                Some(Bson::DateTime(d)) => d.timestamp_millis(),
                _ => return None,
            };
            let us = doc.get(COMPOSITE_US).and_then(|v| v.as_i32()).unwrap_or(0);
            ms * 1000 + i64::from(us)
        }
        _ => return None,
    };
    Some(render_timestamptz_styled(micros, tz, ds))
}

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

/// `timestamp_value_text`, restyled for the session `DateStyle` (ISO is
/// unchanged). A special value already stored as text (`infinity`, a BC or
/// wide-year timestamp) is restyled too where its shape is recognised, else
/// passed through.
pub fn timestamp_value_text_styled(v: &Bson, ds: &DateStyle) -> Option<String> {
    match v {
        Bson::String(s) => Some(render_timestamp_styled(s, ds)),
        _ => timestamp_value_text(v).map(|iso| render_timestamp_styled(&iso, ds)),
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
        Bson::Double(d) => return geo::float8_text(*d),
        Bson::Decimal128(d) => return plain_numeric_text(&d.to_string()),
        _ if is_wide_numeric(v) => return numeric_text(v).unwrap_or_default(),
        Bson::Boolean(b) => return (if *b { "t" } else { "f" }).to_string(),
        Bson::Array(items) => return render_array(items),
        // A box's commas are not the array's delimiter (that is `;`), so the
        // text needs no quoting: `{(3,4),(1,2);(7,8),(5,6)}`.
        _ if geo::is_box(v) => return geo::box_text(&geo::box_coords(v).expect("checked")),
        // A bytea element renders as its `\x…` hex, then the array-quoting
        // below wraps and escapes it (`"\\x01"`), matching PostgreSQL.
        Bson::Binary(b) => bytea::render_hex(&b.bytes),
        // A record / composite element renders as its `(...)` text; the
        // array-quoting below wraps it (it has parens and commas), so
        // `array[row('a',1)::t]` becomes `{"(a,1)"}` as PostgreSQL renders it.
        _ if record_fields(v).is_some() => record_text(record_fields(v).expect("checked")),
        // A timestamp (a BSON date, or the sub-millisecond composite) and an
        // interval (its three-part document) render as the text their scalar
        // cast produces: `{"2020-01-01 00:00:00.5"}`, `{"1 day"}`. These
        // used to fall through to the Debug form (`DateTime(2020-01-01
        // 0:00:00.5 +00:00:00)`), which no PostgreSQL client can read.
        other => match cast_value(other.clone(), "text") {
            Ok(Bson::String(s)) => s,
            _ => format!("{other:?}"),
        },
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
/// The element delimiter is the element type's `typdelim`: `,` for every
/// type but `box`, whose arrays join with `;`.
pub fn render_array(items: &[Bson]) -> String {
    let inner: Vec<String> = items.iter().map(render_array_element).collect();
    format!("{{{}}}", inner.join(array_delimiter(items)))
}

/// The delimiter an array's text form uses, read off its elements: a box
/// anywhere in it (at any depth) makes it a `box[]`.
fn array_delimiter(items: &[Bson]) -> &'static str {
    fn holds_box(items: &[Bson]) -> bool {
        items.iter().any(|v| match v {
            Bson::Array(inner) => holds_box(inner),
            other => geo::is_box(other),
        })
    }
    if holds_box(items) {
        ";"
    } else {
        ","
    }
}

/// Parse PostgreSQL's array text form into elements, coercing each to
/// `element_type`.
///
/// Handles quoting and nesting; a malformed literal is `22P02`, matching what
/// PostgreSQL answers for text that is not a valid array.
/// PostgreSQL requires a multidimensional array to be RECTANGULAR: every
/// sibling sub-array shares one length, and an element is never a mix of array
/// and scalar. Returns false for a ragged or mixed nesting.
fn array_rectangular(items: &[Bson]) -> bool {
    let subs: Vec<&Vec<Bson>> = items
        .iter()
        .filter_map(|x| match x {
            Bson::Array(a) => Some(a),
            _ => None,
        })
        .collect();
    if subs.is_empty() {
        return true; // a flat row of scalars
    }
    if subs.len() != items.len() {
        return false; // a mix of array and scalar elements
    }
    let len0 = subs[0].len();
    subs.iter().all(|a| a.len() == len0) && subs.iter().all(|a| array_rectangular(a))
}

/// The marker `ArrayParser` raises for a structurally broken literal, which
/// `parse_array` turns into the 22P02 the whole text earns.
const STRUCTURAL: &str = "unexpected";

fn parse_array(text: &str, element_type: &str) -> Result<Bson> {
    let malformed = || Error::InvalidText(format!("malformed array literal: \"{text}\""));
    let mut p = ArrayParser {
        chars: text.chars().collect(),
        pos: 0,
        element_type,
        delim: pgtypes::typdelim(element_type),
    };
    p.skip_space();
    // An optional dimension decoration, `[lo:hi]...=`, which PostgreSQL checks
    // against the contents and otherwise discards -- the parsed value carries
    // no lower bounds. A wrong bound count is the same 22P02 as any other
    // malformed literal.
    let mut declared: Vec<usize> = Vec::new();
    while p.peek() == Some('[') {
        p.pos += 1;
        let lo = p.take_int().ok_or_else(malformed)?;
        let hi = if p.peek() == Some(':') {
            p.pos += 1;
            p.take_int().ok_or_else(malformed)?
        } else {
            lo
        };
        if p.peek() != Some(']') || hi < lo {
            return Err(malformed());
        }
        p.pos += 1;
        declared.push(usize::try_from(hi - lo + 1).map_err(|_| malformed())?);
    }
    if !declared.is_empty() {
        p.skip_space();
        if p.peek() != Some('=') {
            return Err(malformed());
        }
        p.pos += 1;
        p.skip_space();
    }
    if p.peek() != Some('{') {
        return Err(malformed());
    }
    // A structural failure is the array's own 22P02; an element that fails
    // to parse keeps its type's error (`'{"[5,1]"}'::int4range[]` is the
    // range's 22000 on PostgreSQL, not "malformed array literal").
    let items = p.parse_braced().map_err(|e| match e {
        Error::InvalidText(ref m) if m == STRUCTURAL => malformed(),
        other => other,
    })?;
    p.skip_space();
    if p.pos != p.chars.len() {
        return Err(malformed());
    }
    if !array_rectangular(&items) {
        return Err(malformed());
    }
    if !declared.is_empty() && array_dims(&items) != declared {
        return Err(malformed());
    }
    Ok(Bson::Array(items))
}

/// The lengths of a (rectangular) array along each dimension, outermost first.
/// `{}` is zero dimensions, as in PostgreSQL.
fn array_dims(items: &[Bson]) -> Vec<usize> {
    if items.is_empty() {
        return Vec::new();
    }
    let mut dims = vec![items.len()];
    if let Some(Bson::Array(inner)) = items.first() {
        dims.extend(array_dims(inner));
    }
    dims
}

/// PostgreSQL's `array_isspace`: the six ASCII whitespace characters and
/// nothing else. Rust's `char::is_whitespace` also strips U+0085 and U+00A0,
/// which PostgreSQL keeps -- a text array carrying either round-tripped to the
/// EMPTY ARRAY -- and `is_ascii_whitespace` omits the vertical tab.
fn array_isspace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{0b}' | '\u{0c}')
}

/// A cursor over one array literal. Mirrors PostgreSQL's `array_in` scanner:
/// an unquoted element runs to the next `,` or `}` with surrounding whitespace
/// dropped; a quoted one keeps everything between the quotes; a backslash
/// escapes the next character in both; an unquoted `NULL` is the null element.
struct ArrayParser<'a> {
    chars: Vec<char>,
    pos: usize,
    element_type: &'a str,
    /// The element type's `typdelim`: `,` except for `box`, which uses `;`.
    delim: char,
}

impl ArrayParser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(array_isspace) {
            self.pos += 1;
        }
    }

    fn take_int(&mut self) -> Option<i64> {
        let start = self.pos;
        if matches!(self.peek(), Some('-' | '+')) {
            self.pos += 1;
        }
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        self.chars[start..self.pos]
            .iter()
            .collect::<String>()
            .parse()
            .ok()
    }

    /// Parse `{ ... }` with the cursor on the opening brace, leaving it just
    /// past the closing one.
    fn parse_braced(&mut self) -> Result<Vec<Bson>> {
        let unexpected = || Error::InvalidText(STRUCTURAL.into());
        self.pos += 1; // the `{`
        let mut items = Vec::new();
        self.skip_space();
        // `{}` is the empty array; `{{}}` is not a literal at all.
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(items);
        }
        loop {
            self.skip_space();
            match self.peek() {
                Some('{') => {
                    let sub = self.parse_braced()?;
                    if sub.is_empty() {
                        return Err(unexpected());
                    }
                    items.push(Bson::Array(sub));
                }
                Some(c) if c == self.delim || c == '}' => return Err(unexpected()),
                None => return Err(unexpected()),
                Some(_) => items.push(self.parse_element()?),
            }
            self.skip_space();
            match self.peek() {
                Some(c) if c == self.delim => self.pos += 1,
                Some('}') => {
                    self.pos += 1;
                    return Ok(items);
                }
                _ => return Err(unexpected()),
            }
        }
    }

    /// One scalar element, quoted or not, with the cursor on its first
    /// character.
    fn parse_element(&mut self) -> Result<Bson> {
        let unexpected = || Error::InvalidText(STRUCTURAL.into());
        let mut raw = String::new();
        let mut was_quoted = false;
        // Whitespace inside an unquoted element is kept when more content
        // follows (`{a b}` is `a b`); trailing whitespace is dropped.
        let mut pending_space = String::new();
        loop {
            match self.peek() {
                None => return Err(unexpected()),
                Some('"') => {
                    // A quote may not follow element text, nor text a quote.
                    if was_quoted || !raw.is_empty() {
                        return Err(unexpected());
                    }
                    was_quoted = true;
                    self.pos += 1;
                    loop {
                        match self.peek() {
                            None => return Err(unexpected()),
                            Some('"') => {
                                self.pos += 1;
                                break;
                            }
                            Some('\\') => {
                                self.pos += 1;
                                raw.push(self.peek().ok_or_else(unexpected)?);
                                self.pos += 1;
                            }
                            Some(c) => {
                                raw.push(c);
                                self.pos += 1;
                            }
                        }
                    }
                }
                Some(c) if c == self.delim || c == '}' => break,
                Some('{') => return Err(unexpected()),
                Some(c) if array_isspace(c) => {
                    pending_space.push(c);
                    self.pos += 1;
                }
                Some(c) => {
                    if was_quoted {
                        return Err(unexpected());
                    }
                    raw.push_str(&pending_space);
                    pending_space.clear();
                    if c == '\\' {
                        self.pos += 1;
                        raw.push(self.peek().ok_or_else(unexpected)?);
                    } else {
                        raw.push(c);
                    }
                    self.pos += 1;
                }
            }
        }
        // An UNQUOTED `NULL` is the null element; a quoted one is the string.
        if !was_quoted && raw.eq_ignore_ascii_case("null") {
            return Ok(Bson::Null);
        }
        cast_value(Bson::String(raw), self.element_type)
    }
}

/// A `numeric` rounded to a whole number, as PostgreSQL rounds it.
///
/// PostgreSQL rounds numeric->integer HALF AWAY FROM ZERO (`1.5`->2, `2.5`->3,
/// `-1.5`->-2), which is not what it does for float->integer (that is
/// half-to-even). Measured on PostgreSQL 14.
///
/// Done on the DIGITS rather than through `f64`: a `numeric` carries any
/// number of digits and an f64 has 15, so routing a big one through a float
/// would round twice and silently return a different integer.
fn decimal_to_integer(v: &Bson) -> Option<i128> {
    numeric::bigint_to_i128(&numeric::numeric_text_to_integer(&numeric_text(v)?)?)
}

/// A value as its PostgreSQL text, which is what `::text` would produce.
pub fn value_text(v: &Bson) -> String {
    render_value_text(v)
}

/// The `(...)` text of an anonymous record value, or `None` for any other
/// value -- the wire layer renders a record in the text format through this.
pub fn record_value_text(v: &Bson) -> Option<String> {
    record_fields(v).map(|f| record_text(f))
}

/// The ordered field values of a record / composite value, or `None` for any
/// other value -- the wire layer's binary record encoder walks these.
pub fn record_field_values(v: &Bson) -> Option<&Vec<Bson>> {
    record_fields(v)
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
    // A cast to a user BASE type (`'hello'::"a-b"`). The value is carried as
    // the text its input function would have read -- every base type this
    // server can hold is declared over text I/O -- so a text source passes
    // through unchanged and any other source has no cast (42846), exactly
    // PostgreSQL's answer for a type with no cast paths. A SHELL has no
    // representation at all: 42704 `is only a shell` (both measured on 16).
    if let Some((name, _, defined)) = user_base_type(target) {
        if !defined {
            return Err(Error::UndefinedObject(format!(
                "type \"{name}\" is only a shell"
            )));
        }
        return match value {
            // An extension's type parses the text and stores its canonical
            // form, which is what its output function prints.
            Bson::String(text) => match extension_type(&name) {
                Some(ext) => extension_canonical(ext, &text).map(Bson::String),
                None => Ok(Bson::String(text)),
            },
            other => Err(Error::CannotCoerce(format!(
                "cannot cast type {} to {}",
                display_type(inferred_type(&other)),
                quote_type_path(&name)
            ))),
        };
    }
    // A cast to a user COMPOSITE type -- `'(1,x)'::testcomp` (text input) or
    // `row(1,'x')::testcomp` (a record). Handled before the bare-record and
    // enum arms below: a composite carries an empty label list in the enum
    // table, so without this arm the enum arm rejected it as an unknown label.
    if let Some((_, comp_fields)) = user_composite(target) {
        return composite_value(value, target, &comp_fields);
    }
    // A box renders to text as `(high),(low)` and is a no-op cast to itself;
    // no other cast is defined for it on PostgreSQL.
    if let Some(coords) = geo::box_coords(&value) {
        return match target {
            "text" | "varchar" | "bpchar" | "name" => Ok(Bson::String(geo::box_text(&coords))),
            "box" => Ok(value),
            _ => Err(Error::CannotCoerce(format!(
                "cannot cast type box to {}",
                display_type(target)
            ))),
        };
    }
    if target == "box" {
        return match &value {
            Bson::String(text) => geo::parse_box(text).map(geo::box_value),
            other => Err(Error::CannotCoerce(format!(
                "cannot cast type {} to box",
                display_type(inferred_type(other))
            ))),
        };
    }
    // A stored `bytea` (Bson::Binary) renders to text as its `\x…` hex form and
    // is a no-op cast to itself; other targets fall through to the usual error.
    if let Bson::Binary(b) = &value {
        match target {
            "text" | "varchar" | "bpchar" | "name" => {
                return Ok(Bson::String(bytea::render_hex(&b.bytes)))
            }
            "bytea" => return Ok(value.clone()),
            _ => {}
        }
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
    // A regclass value likewise: to text as the relation's NAME, to `oid` /
    // the two integer widths as its oid; anything else has no cast path
    // (measured 16: `::int2`, `::numeric`, `::float8`, `::regtype` are all
    // 42846 `cannot cast type regclass to ...`).
    if let Some(oid) = regclass_oid(&value) {
        return match target {
            "regclass" => Ok(value),
            "text" | "varchar" | "name" | "bpchar" => Ok(Bson::String(regclass_text(oid))),
            "int4" | "int8" | "oid" | "integer" | "int" | "bigint" => Ok(Bson::Int64(oid)),
            _ => Err(Error::CannotCoerce(format!(
                "cannot cast type regclass to {}",
                display_type(target)
            ))),
        };
    }
    // An anonymous record renders to text as `(f1,f2,...)`; a cast to record is
    // a no-op. Other targets are not defined for a bare record.
    if let Some(fields) = record_fields(&value) {
        return match target {
            "text" | "varchar" | "bpchar" | "name" => Ok(Bson::String(record_text(fields))),
            "record" => Ok(value.clone()),
            _ => Err(Error::Unsupported(format!("a record cast to {target}"))),
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
            v if is_numeric(&v) => match numeric_text(&v).and_then(|t| t.parse::<i64>().ok()) {
                Some(v) => from_i64(v),
                None => Err(out_of_range()),
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
    // `'sad'::mood` -- an enum VALUE. The value stays its label text (which
    // is also how the store carries it); only membership is checked, and the
    // failure is 22P02 with PostgreSQL's own wording.
    if let Some((_, labels)) = user_enum(target) {
        return match value {
            Bson::String(label) => {
                if labels.contains(&label) {
                    Ok(Bson::String(label))
                } else {
                    Err(Error::InvalidText(format!(
                        "invalid input value for enum {target}: \"{label}\""
                    )))
                }
            }
            other => Err(Error::Unsupported(format!(
                "a cast of {} to {target}",
                bson_kind(&other)
            ))),
        };
    }
    // `'t1'::regclass` names a relation; a number is taken as an oid as it
    // stands (`12345::regclass` renders `12345`, `0::regclass` renders `-`),
    // and so is a string that is all digits (measured 16).
    if target == "regclass" {
        return match value {
            Bson::Int32(oid) => Ok(regclass_value(i64::from(oid))),
            Bson::Int64(oid) => Ok(regclass_value(oid)),
            Bson::String(name) => match name.trim().parse::<i64>() {
                Ok(oid) if (0..(1i64 << 32)).contains(&oid) => Ok(regclass_value(oid)),
                _ => resolve_regclass(&name).map(regclass_value),
            },
            other => Err(Error::CannotCoerce(format!(
                "cannot cast type {} to regclass",
                display_type(inferred_type(&other))
            ))),
        };
    }
    if target == "regtype" {
        return match value {
            Bson::Int32(oid) => Ok(regtype_value(i64::from(oid))),
            Bson::Int64(oid) => Ok(regtype_value(oid)),
            // `'text'::regtype` -- unlike `to_regtype`, an unknown NAME is an
            // error here, which is why psycopg prefers the function.
            Bson::String(name) => {
                match pgtypes::oid_of_name(&name).or_else(|| user_type_or_array_oid(&name)) {
                    Some(oid) => Ok(regtype_value(oid)),
                    None => Err(match shell_type_named(&name) {
                        Some(shell) => {
                            Error::UndefinedObject(format!("type \"{shell}\" is only a shell"))
                        }
                        None => Error::UndefinedObject(format!(
                            "type \"{}\" does not exist",
                            name.trim()
                        )),
                    }),
                }
            }
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
        // `float8out`: `1e+20`, `1e-07`, `Infinity`, and no `.0` on a whole.
        Bson::Double(d) => geo::float8_text(*d),
        Bson::Boolean(b) => (if *b { "true" } else { "false" }).to_string(),
        // Decimal128's own rendering keeps the scale (`1.50`, not `1.5`), and
        // the expansion drops its exponent notation, which PostgreSQL's
        // numeric output never uses.
        Bson::Decimal128(d) => plain_numeric_text(&d.to_string()),
        Bson::Document(_) if is_wide_numeric(v) => numeric_text(v).unwrap_or_default(),
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
            // `boolean -> integer` is 1 / 0; there is no cast to smallint.
            Bson::Boolean(b) if matches!(target, "int4" | "integer" | "int") => {
                Ok(Bson::Int32(i32::from(*b)))
            }
            Bson::Boolean(_) => Err(Error::CannotCoerce(
                "cannot cast type boolean to smallint".to_string(),
            )),
            Bson::Int64(i) => i32::try_from(*i)
                .map(Bson::Int32)
                .map_err(|_| Error::InvalidText(format!("integer out of range: \"{i}\""))),
            // float->integer rounds HALF TO EVEN in PostgreSQL (`2.5` -> 2,
            // `3.5` -> 4), which is NOT the half-away-from-zero rule it uses
            // for numeric->integer. Rust's `round()` is the latter, so using
            // it here answered 3 for `2.5::float8::int`. Measured on PG 14.
            Bson::Double(d) => Ok(Bson::Int32(d.round_ties_even() as i32)),
            v if is_numeric(v) => decimal_to_integer(v)
                .and_then(|n| i32::try_from(n).ok())
                .map(Bson::Int32)
                .ok_or_else(|| {
                    Error::NumericOutOfRange(format!("integer out of range: \"{}\"", as_text(v)))
                }),
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
            Bson::Boolean(_) => Err(Error::CannotCoerce(
                "cannot cast type boolean to bigint".to_string(),
            )),
            Bson::Double(d) => Ok(Bson::Int64(d.round_ties_even() as i64)),
            v if is_numeric(v) => decimal_to_integer(v)
                .and_then(|n| i64::try_from(n).ok())
                .map(Bson::Int64)
                .ok_or_else(|| {
                    Error::NumericOutOfRange(format!("bigint out of range: \"{}\"", as_text(v)))
                }),
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
                        .map(|v| {
                            // A multidimensional array: an element that is
                            // itself an array casts to the SAME array type
                            // (recurse), not to the scalar element type.
                            if matches!(v, Bson::Array(_)) {
                                cast_value(v.clone(), t)
                            } else {
                                cast_value(v.clone(), element)
                            }
                        })
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
            v if is_numeric(v) => Ok(value),
            other => parse_numeric(&as_text(other)),
        },
        "float4" | "float8" | "real" | "double" => match &value {
            Bson::Int32(i) => Ok(Bson::Double(f64::from(*i))),
            Bson::Int64(i) => Ok(Bson::Double(*i as f64)),
            Bson::Double(_) => Ok(value),
            // A decimal literal is `numeric`, so `1.5::float8` arrives here as
            // a Decimal128 rather than a Double. Missing this arm made the
            // cast fail outright once decimal literals stopped being floats.
            v if is_numeric(v) => numeric_text(v)
                .and_then(|t| numeric::numeric_text_to_f64(&t))
                .map(Bson::Double)
                .ok_or_else(|| bad("double precision", &value)),
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
            // PostgreSQL casts only `integer` and text to boolean; a bigint,
            // numeric, double or date has no cast at all (42846), which is a
            // different error from a text that will not parse (22P02).
            other => Err(Error::CannotCoerce(format!(
                "cannot cast type {} to boolean",
                display_type(inferred_type(other))
            ))),
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
            // `json` keeps the input verbatim, so a `\uXXXX` escape it
            // cannot decode -- a lone surrogate, or `\u0000` -- is fine;
            // `jsonb` stores the decoded text and must reject both, with
            // PostgreSQL's two distinct codes.
            let parsed = match json::parse(&text) {
                Ok(v) => Some(v),
                Err(json::ParseError::UnpairedSurrogate | json::ParseError::NulEscape)
                    if target == "json" =>
                {
                    None
                }
                Err(e) => return Err(json_parse_error(e)),
            };
            Ok(Bson::String(match parsed {
                Some(parsed) if target == "jsonb" => json::render_jsonb(&parsed),
                _ => text,
            }))
        }
        "interval" => match Interval::from_bson(&value) {
            Some(iv) => Ok(iv.to_bson()),
            None => Ok(parse_interval(&as_text(&value))?.to_bson()),
        },
        // `date` and `time` are stored as their canonical TEXT, matching what
        // the Python server writes -- the two servers share one store, so the
        // representation is a contract, not an implementation choice.
        // A timestamp value (an instant, or its text) keeps its date part.
        "date"
            if matches!(value, Bson::DateTime(_))
                || matches!(&value, Bson::Document(d) if d.contains_key(COMPOSITE_DATE)) =>
        {
            let micros = instant_micros(&value).ok_or_else(|| {
                Error::InvalidDatetimeFormat("invalid input syntax for type date".into())
            })?;
            Ok(Bson::String(render_date_pg(
                chrono::DateTime::from_timestamp_micros(micros)
                    .map(|d| d.date_naive())
                    .unwrap_or_default(),
            )))
        }
        "date" => match parse_date(&as_text(&value)) {
            Ok(d) => Ok(Bson::String(d)),
            Err(e) => match parse_timestamp(&as_text(&value)) {
                Ok(micros) => Ok(Bson::String(render_date_pg(
                    chrono::DateTime::from_timestamp_micros(micros)
                        .map(|d| d.date_naive())
                        .unwrap_or_default(),
                ))),
                Err(_) => Err(e),
            },
        },
        // `timestamptz` and `timetz` are stored as their canonical TEXT, the
        // same choice `date` and `time` already make here. A `timestamptz`
        // renders in the SESSION zone, so the text is only canonical for the
        // session that produced it -- fine for an expression or a bound value,
        // which is all this server accepts (a timestamptz COLUMN is refused:
        // storing session-relative text in a row would be a wrong answer for
        // every other session that read it).
        "timestamptz" | "timestamp with time zone" => {
            // `epoch` is the one constant special INPUT value -- the UTC instant
            // 0 -- the same case the `timestamp` arm handles.
            if as_text(&value).trim().eq_ignore_ascii_case("epoch") {
                return Ok(Bson::DateTime(bson::DateTime::from_millis(0)));
            }
            // A timestamptz is stored as a UTC INSTANT (the same carrier as
            // `timestamp`) and rendered in the session zone on the way out --
            // storing session-rendered text would be a wrong answer for any
            // other session. infinity / wide-year / BC stay text.
            if let Some(text) = special_timestamp_text(&as_text(&value)) {
                return Ok(Bson::String(text));
            }
            let micros = parse_timestamptz(&as_text(&value), &session_timezone())?;
            let (ms, rem) = split_subms(micros);
            let date = Bson::DateTime(bson::DateTime::from_millis(ms));
            Ok(if rem == 0 {
                date
            } else {
                Bson::Document(doc! { COMPOSITE_DATE: date, COMPOSITE_US: rem })
            })
        }
        "timetz" | "time with time zone" => Ok(Bson::String(parse_timetz(
            &as_text(&value),
            &session_timezone(),
        )?)),
        // A timestamp becomes a BSON date plus, when it carries microseconds,
        // a composite the assignment path unwraps into the hidden companion.
        "timestamp" => {
            // `epoch` is the one special INPUT value that is constant; `now` /
            // `today` etc. depend on the clock and are filed rather than guessed.
            if as_text(&value).trim().eq_ignore_ascii_case("epoch") {
                return cast_value(Bson::String("1970-01-01 00:00:00".into()), "timestamp");
            }
            // infinity / wide-year / BC: keep the text, let the client's
            // loader decide -- the micros path cannot hold them.
            if let Some(text) = special_timestamp_text(&as_text(&value)) {
                return Ok(Bson::String(text));
            }
            // `timestamp` accepts a zone and ignores it: `'2001-01-01+00'`.
            let text = as_text(&value);
            let micros = match parse_timestamp(&text) {
                Ok(m) => m,
                Err(e) => match split_trailing_offset(&text) {
                    (body, Some(_)) => parse_timestamp(&body).map_err(|_| e)?,
                    _ => return Err(e),
                },
            };
            let (ms, rem) = split_subms(micros);
            let date = Bson::DateTime(bson::DateTime::from_millis(ms));
            Ok(if rem == 0 {
                date
            } else {
                Bson::Document(doc! { COMPOSITE_DATE: date, COMPOSITE_US: rem })
            })
        }
        // A timestamp value (an instant, or its text) keeps its time of day.
        "time"
            if matches!(value, Bson::DateTime(_))
                || matches!(&value, Bson::Document(d) if d.contains_key(COMPOSITE_DATE)) =>
        {
            let micros = instant_micros(&value).ok_or_else(|| {
                Error::InvalidDatetimeFormat("invalid input syntax for type time".into())
            })?;
            let day = micros.rem_euclid(86_400_000_000);
            parse_time(&render_timestamp(day)[11..]).map(Bson::String)
        }
        "time" => match parse_time(&as_text(&value)) {
            Ok(t) => Ok(Bson::String(t)),
            Err(e) => match parse_timestamp(&as_text(&value)) {
                Ok(micros) => {
                    parse_time(&render_timestamp(micros.rem_euclid(86_400_000_000))[11..])
                        .map(Bson::String)
                }
                Err(_) => Err(e),
            },
        },
        "inet" => Ok(Bson::String(net::normalize_inet(&as_text(&value))?)),
        "cidr" => Ok(Bson::String(net::normalize_cidr(&as_text(&value))?)),
        "aclitem" => Ok(Bson::String(acl::parse(&as_text(&value))?)),
        "bytea" => {
            let bytes = bytea::parse(&value)?;
            Ok(bytea::to_binary(bytes))
        }
        "uuid" => {
            let text = as_text(&value);
            parse_uuid(&text).map(Bson::String).ok_or_else(|| {
                Error::InvalidText(format!("invalid input syntax for type uuid: \"{text}\""))
            })
        }
        "jsonpath" => Ok(Bson::String(jsonpath::render(&jsonpath::parse(&as_text(
            &value,
        ))?))),
        "tsvector" => Ok(Bson::String(fts::render_vector(&fts::parse_vector(
            &as_text(&value),
        )?))),
        "tsquery" => Ok(Bson::String(fts::render_query(&fts::parse_query(
            &as_text(&value),
        )?))),
        "regconfig" => {
            let text = as_text(&value);
            fts::config(&text)?;
            Ok(Bson::String(
                text.trim()
                    .trim_start_matches("pg_catalog.")
                    .to_ascii_lowercase(),
            ))
        }
        other => Err(Error::Unsupported(format!("a cast to {other}"))),
    }
}

/// A UUID from its 16-byte BINARY wire form -> canonical lowercase text.
pub fn uuid_from_wire(bytes: &[u8]) -> Option<String> {
    if bytes.len() != 16 {
        return None;
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// A UUID's 16-byte BINARY wire form (`uuid_send`) from its text, in any
/// spelling `uuid_in` accepts; `None` when the text is not a uuid.
pub fn uuid_to_wire(text: &str) -> Option<Vec<u8>> {
    let canonical = parse_uuid(text)?;
    let hex: String = canonical.chars().filter(|c| *c != '-').collect();
    (0..16)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok())
        .collect()
}

/// Parse a UUID the way PostgreSQL's `uuid_in` does and return its canonical
/// lowercase `8-4-4-4-12` text. Optional surrounding braces, and a single
/// hyphen is tolerated after ANY complete group of four hex digits (`uuid_in`
/// consumes a byte pair at a time and skips one `-` after each), not only at
/// the four standard boundaries -- PostgreSQL 16 accepts
/// `{a0eebc99-9c0b4ef8-bb6d6bb9-bd380a11}` and `a0ee-bc99-...`, which is what
/// psycopg's uuid suite sends. A hyphen inside a group, two in a row, one
/// before the first digit or after the last, any non-hex character,
/// whitespace, or a count other than 32 hex digits is rejected (`22P02`).
fn parse_uuid(s: &str) -> Option<String> {
    let inner = match (s.strip_prefix('{'), s.strip_suffix('}')) {
        (Some(_), Some(_)) => &s[1..s.len() - 1],
        (None, None) => s,
        _ => return None,
    };
    let mut hex = String::with_capacity(32);
    let mut after_hyphen = false;
    for ch in inner.chars() {
        if ch == '-' {
            if !after_hyphen && hex.len().is_multiple_of(4) && !hex.is_empty() && hex.len() < 32 {
                after_hyphen = true;
                continue;
            }
            return None;
        }
        after_hyphen = false;
        if !ch.is_ascii_hexdigit() || hex.len() == 32 {
            return None;
        }
        hex.push(ch.to_ascii_lowercase());
    }
    if hex.len() != 32 {
        return None;
    }
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
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
/// `instant_micros` for the server: a stored timestamp's UTC microseconds.
pub fn instant_micros_pub(v: &Bson) -> Option<i64> {
    instant_micros(v)
}

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
    let named = |name: String| is_range_family(&name).then_some(name);
    match n.and_then(|x| x.node.as_ref()) {
        Some(N::FuncCall(f)) => range_constructor_type(f),
        Some(N::TypeCast(tc)) => named(type_name_of(tc.type_name.as_ref()?)),
        // A parameter the client DECLARED as a range, a multirange, or an
        // array of either (psycopg sends `[Int4Range(...)]` as `_int4range`).
        // The declared type is what PostgreSQL resolves the literal beside it
        // to; the decoded value alone says only "an array of strings".
        Some(N::ParamRef(p)) => named(declared_param_type(usize::try_from(p.number).unwrap_or(0))?),
        _ => None,
    }
}

/// The range-family type each UNDECLARED parameter takes from its context.
///
/// PostgreSQL's analysis pass gives an `unknown` parameter the type of the
/// operand it is compared against, so `'empty'::int4range = $1` makes `$1`
/// an `int4range` before the value is ever looked at. That matters for a
/// BINARY parameter: psycopg sends a bare `Range(empty=True)` with no type
/// at all, and its binary form is the single flag byte `\x01` -- which is
/// only a range once something says WHICH range. The wire layer asks this
/// before decoding, so the byte reaches the range decoder instead of being
/// read as text.
///
/// `declared` is what the client said for each `$n` (`None` = unspecified);
/// only the unspecified ones are inferred, and only from a comparison where
/// the other side is STATICALLY a range (a constructor, a cast, or a declared
/// parameter). Anything else stays `None`, so `pg_typeof($1)` still reports
/// what PostgreSQL does for a parameter with no context: an error.
pub fn infer_param_types(sql: &str, declared: &[Option<String>]) -> Vec<Option<String>> {
    let mut inferred = declared.to_vec();
    let Ok(parsed) = parse_tree(sql) else {
        return inferred;
    };
    let previous_types = PLAN_PARAM_TYPES.with(|t| t.replace(declared.to_vec()));
    for (node, _, _, _) in parsed.nodes() {
        // `$1::int4range`: a cast names the parameter's type outright, which
        // is how PostgreSQL types an unknown parameter under a cast. Only a
        // range-family target: that is the one family psycopg sends untyped.
        if let pg_query::NodeRef::TypeCast(tc) = node {
            let target = tc.type_name.as_ref().map(type_name_of).unwrap_or_default();
            if let (Some(N::ParamRef(p)), true) = (
                tc.arg.as_deref().and_then(|a| a.node.as_ref()),
                is_range_family(&target),
            ) {
                if let Some(slot) = usize::try_from(p.number)
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|i| inferred.get_mut(i))
                {
                    if slot.is_none() {
                        *slot = Some(target);
                    }
                }
            }
            continue;
        }
        let pg_query::NodeRef::AExpr(e) = node else {
            continue;
        };
        if !matches!(
            operator_name(e),
            Ok("=" | "<>" | "!=" | "<" | "<=" | ">" | ">=")
        ) {
            continue;
        }
        let param_index =
            |n: Option<&pg_query::protobuf::Node>| match n.and_then(|x| x.node.as_ref()) {
                Some(N::ParamRef(p)) => usize::try_from(p.number).ok()?.checked_sub(1),
                _ => None,
            };
        for (param, other) in [
            (e.lexpr.as_deref(), e.rexpr.as_deref()),
            (e.rexpr.as_deref(), e.lexpr.as_deref()),
        ] {
            let Some(i) = param_index(param) else {
                continue;
            };
            if inferred.get(i).is_some_and(Option::is_some) {
                continue;
            }
            if let (Some(slot), Some(name)) = (inferred.get_mut(i), static_range_type(other)) {
                *slot = Some(name);
            } else if let (Some(slot), Some(name)) = (inferred.get_mut(i), static_text_type(other))
            {
                *slot = Some(name);
            }
        }
    }
    PLAN_PARAM_TYPES.with(|t| *t.borrow_mut() = previous_types);
    inferred
}

/// The text-family type an UNDECLARED parameter takes from the operand it is
/// compared against, when that operand is STATICALLY text: a string literal
/// (`$1 = 'a'` resolves both unknowns to text), a cast to a text type, or a
/// call of a function that returns text (`$1 = chr($2)`).
///
/// Without this the parameter's text was SNIFFED into whatever it looked like
/// -- psycopg's `test_dump_1char` sends `chr(49)`, the string `"1"`, and the
/// sniff made it an integer beside a text value ("comparing int32 with
/// string using = is not supported yet"). PostgreSQL 16 answers `true`.
fn static_text_type(n: Option<&pg_query::protobuf::Node>) -> Option<String> {
    const TEXT_FUNCTIONS: &[&str] = &[
        "chr",
        "lower",
        "upper",
        "initcap",
        "concat",
        "concat_ws",
        "ltrim",
        "rtrim",
        "btrim",
        "trim",
        "substr",
        "substring",
        "left",
        "right",
        "repeat",
        "replace",
        "reverse",
        "translate",
        "lpad",
        "rpad",
        "md5",
        "to_char",
        "quote_literal",
        "quote_ident",
        "regexp_replace",
        "split_part",
        "encode",
    ];
    match n.and_then(|x| x.node.as_ref()) {
        Some(N::AConst(c)) if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_))) => {
            Some("text".to_string())
        }
        Some(N::TypeCast(tc)) => {
            let name = type_name_of(tc.type_name.as_ref()?);
            matches!(
                name.as_str(),
                "text" | "varchar" | "bpchar" | "name" | "character varying" | "character"
            )
            .then_some(name)
        }
        Some(N::FuncCall(f)) => {
            let name = func_name(f)?;
            TEXT_FUNCTIONS
                .contains(&name.as_str())
                .then(|| "text".to_string())
        }
        _ => None,
    }
}

/// The highest `$n` the SQL references, or 0 when it has no parameters.
///
/// A client may `Parse` with NO parameter oids at all and leave every type to
/// the server (libpq's `PQprepare` with `nParams = 0`, `send_query_params`
/// with an empty type list). The wire layer sized its placeholders from the
/// oid list, so a describe of `select $1::uuid` answered "there is no
/// parameter $1" -- PostgreSQL infers the parameter from the SQL.
pub fn max_param_number(sql: &str) -> usize {
    // Memoised like `parse_tree`, for the same reason: this runs once per
    // Describe and once per Execute, and the scan goes through C.
    const MAX_ENTRIES: usize = 4096;
    static MEMO: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, usize>>> =
        std::sync::OnceLock::new();
    let memo = MEMO.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Some(n) = memo.lock().unwrap_or_else(|e| e.into_inner()).get(sql) {
        return *n;
    }
    let n = scan_max_param_number(sql);
    let mut guard = memo.lock().unwrap_or_else(|e| e.into_inner());
    if guard.len() >= MAX_ENTRIES {
        guard.clear();
    }
    guard.insert(sql.to_string(), n);
    n
}

fn scan_max_param_number(sql: &str) -> usize {
    // The LEXER, not the parse tree: pg_query's `nodes()` walks a curated
    // subset of each statement (a SELECT's target list, WHERE, FROM, ...)
    // and skips a VALUES list, a RETURNING clause and an UPDATE's SET, so
    // `insert into t values ($1, $2)` counted zero parameters and, prepared
    // with no declared types, executed as "there is no parameter $1". The
    // scanner sees every `$n` token and nothing inside a string or comment.
    let Ok(scanned) = pg_query::scan(sql) else {
        return 0;
    };
    let param = pg_query::protobuf::Token::Param as i32;
    scanned
        .tokens
        .iter()
        .filter(|t| t.token == param)
        .filter_map(|t| sql.get(t.start as usize..t.end as usize))
        .filter_map(|text| text.strip_prefix('$')?.parse::<usize>().ok())
        .max()
        .unwrap_or(0)
}

/// The type of each parameter as `pg_prepared_statements.parameter_types`
/// reports it: what the client declared, else what the statement itself
/// says -- a cast on the parameter (`$1::int`), a comparison with a typed
/// operand, or the column an INSERT puts it in (`column_type(table, column)`
/// resolves that) -- else `text`, which is what PostgreSQL 16 resolves an
/// unconstrained `unknown` parameter to. Catalog reporting only: execution
/// still decodes an undeclared parameter from its context, and widening that
/// is a separate change.
pub fn catalog_param_types(
    sql: &str,
    declared: &[Option<String>],
    column_type: &dyn Fn(&str, ColumnRef<'_>) -> Option<String>,
) -> Vec<String> {
    catalog_param_types_opt(sql, declared, column_type)
        .into_iter()
        .map(|t| t.unwrap_or_else(|| "text".to_string()))
        .collect()
}

/// How `catalog_param_types` names the column an INSERT puts a parameter in:
/// by name when the statement lists its columns (`insert into t (a, b)
/// values ($1, $2)`), by position when it does not (`insert into t values
/// ($1, $2)` puts `$2` in the table's second column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnRef<'a> {
    Name(&'a str),
    Position(usize),
}

/// `catalog_param_types` before the text default: a slot nothing in the
/// statement types stays `None`. Bind uses this for a parameter the client
/// left untyped but sent in BINARY format -- there the type decides how the
/// bytes are read, and psycopg sends an empty `Multirange([])` that way (oid
/// 0, four zero bytes) because no element exists to name the type from.
pub fn catalog_param_types_opt(
    sql: &str,
    declared: &[Option<String>],
    column_type: &dyn Fn(&str, ColumnRef<'_>) -> Option<String>,
) -> Vec<Option<String>> {
    let n = declared.len().max(max_param_number(sql));
    let mut padded = declared.to_vec();
    padded.resize(n, None);
    let mut inferred = infer_param_types(sql, &padded);
    let param_index = |node: &pg_query::protobuf::Node| match node.node.as_ref() {
        Some(N::ParamRef(p)) => usize::try_from(p.number).ok()?.checked_sub(1),
        _ => None,
    };
    if let Ok(parsed) = parse_tree(sql) {
        for (node, _, _, _) in parsed.nodes() {
            match node {
                pg_query::NodeRef::TypeCast(tc) => {
                    let Some(i) = tc.arg.as_deref().and_then(param_index) else {
                        continue;
                    };
                    if let (Some(slot @ None), Some(tn)) =
                        (inferred.get_mut(i), tc.type_name.as_ref())
                    {
                        *slot = Some(type_name_of(tn));
                    }
                }
                pg_query::NodeRef::InsertStmt(ins) => {
                    let table = ins
                        .relation
                        .as_ref()
                        .map(|r| r.relname.clone())
                        .unwrap_or_default();
                    let columns: Vec<String> = ins
                        .cols
                        .iter()
                        .filter_map(|c| match c.node.as_ref() {
                            Some(N::ResTarget(rt)) => Some(rt.name.clone()),
                            _ => None,
                        })
                        .collect();
                    let Some(select) = ins.select_stmt.as_deref().and_then(|s| s.node.as_ref())
                    else {
                        continue;
                    };
                    let N::SelectStmt(sel) = select else {
                        continue;
                    };
                    for row in &sel.values_lists {
                        let Some(N::List(items)) = row.node.as_ref() else {
                            continue;
                        };
                        for (pos, item) in items.items.iter().enumerate() {
                            let Some(i) = param_index(item) else {
                                continue;
                            };
                            let column = if columns.is_empty() {
                                ColumnRef::Position(pos)
                            } else {
                                match columns.get(pos) {
                                    Some(name) => ColumnRef::Name(name),
                                    None => continue,
                                }
                            };
                            if let Some(slot @ None) = inferred.get_mut(i) {
                                *slot = column_type(&table, column);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    inferred
}

/// The functions whose arguments are `"any"` / VARIADIC `"any"`: nothing
/// there gives an UNDECLARED parameter a type, so PostgreSQL refuses the
/// statement outright (`42P18`, "could not determine data type of parameter
/// $n") rather than guess text. psycopg's `test_dump_text_oid` expects
/// exactly that for `concat($1, $2)`.
fn refuse_untyped_any_args(f: &pg_query::protobuf::FuncCall) -> Result<()> {
    const ANY_ARG_FUNCTIONS: &[&str] = &[
        "concat",
        "concat_ws",
        "format",
        "num_nulls",
        "num_nonnulls",
        "json_build_array",
        "json_build_object",
        "jsonb_build_array",
        "jsonb_build_object",
    ];
    let Some(name) = func_name(f) else {
        return Ok(());
    };
    if !ANY_ARG_FUNCTIONS.contains(&name.as_str()) {
        return Ok(());
    }
    // `format`'s first argument is the `text` format string, and `concat_ws`'s
    // the `text` separator: a parameter there resolves as text, so PG names
    // the first VARIADIC parameter (`format($1, $2)` faults `$2`).
    let leading_text = usize::from(matches!(name.as_str(), "format" | "concat_ws"));
    for arg in f.args.iter().skip(leading_text) {
        if let Some(N::ParamRef(p)) = arg.node.as_ref() {
            let n = usize::try_from(p.number).unwrap_or(0);
            if declared_param_type(n).is_none() {
                return Err(Error::IndeterminateDatatype(format!(
                    "could not determine data type of parameter ${n}"
                )));
            }
        }
    }
    Ok(())
}

/// The parameter names and defaults of the built-ins that take named
/// arguments (`silent => true`).
fn named_signature(name: &str) -> Option<(&'static [&'static str], Vec<Bson>)> {
    if jsonpath::is_function(name) {
        return Some((
            &["target", "path", "vars", "silent"],
            vec![
                Bson::Null,
                Bson::Null,
                Bson::String("{}".into()),
                Bson::Boolean(false),
            ],
        ));
    }
    None
}

/// A call's argument values with named arguments placed by name and the
/// rest defaulted; `None` when it has no named argument.
fn named_call_args(
    f: &pg_query::protobuf::FuncCall,
    name: &str,
    params: &[Bson],
) -> Option<Result<Vec<Bson>>> {
    if !f
        .args
        .iter()
        .any(|a| matches!(a.node.as_ref(), Some(N::NamedArgExpr(_))))
    {
        return None;
    }
    let (names, mut values) = named_signature(name)?;
    for (i, a) in f.args.iter().enumerate() {
        let (slot, node) = match a.node.as_ref() {
            Some(N::NamedArgExpr(na)) => match names.iter().position(|n| *n == na.name) {
                Some(p) => (p, na.arg.as_deref()),
                None => {
                    return Some(Err(Error::UndefinedFunction(format!(
                        "function {name}({} => unknown) does not exist",
                        na.name
                    ))))
                }
            },
            _ => (i, Some(a)),
        };
        if let Some(node) = node {
            match const_value(node, params) {
                Ok(v) => values[slot] = v,
                Err(e) => return Some(Err(e)),
            }
        }
    }
    Some(Ok(values))
}

/// Evaluate a `jsonb_path_*` function over its argument values. For
/// `jsonb_path_query` the answer is the ARRAY of items (the caller spreads
/// it into rows).
pub(crate) fn jsonpath_call(name: &str, args: &[Bson]) -> Result<Bson> {
    if args.len() < 2 || args.len() > 4 {
        return Err(Error::UndefinedFunction(format!(
            "function {name} does not exist"
        )));
    }
    if args[..2].iter().any(|a| *a == Bson::Null) {
        return Ok(Bson::Null);
    }
    let vars = match args.get(2) {
        Some(Bson::Null) => return Ok(Bson::Null),
        Some(v) => Some(json_text_of(v)),
        None => None,
    };
    let silent = match args.get(3) {
        Some(Bson::Boolean(b)) => *b,
        Some(Bson::Null) => return Ok(Bson::Null),
        Some(other) => value_text(other).eq_ignore_ascii_case("true"),
        None => false,
    };
    let base = name.strip_suffix("_tz").unwrap_or(name);
    let out = jsonpath::call(
        base,
        &json_text_of(&args[0]),
        &value_text(&args[1]),
        vars.as_deref(),
        silent,
    )?;
    Ok(match (base, out) {
        (_, jsonpath::PathOut::Bool(b)) => b.map_or(Bson::Null, Bson::Boolean),
        ("jsonb_path_query_first", jsonpath::PathOut::Items(items)) => items
            .first()
            .map_or(Bson::Null, |j| Bson::String(json::render_jsonb(j))),
        ("jsonb_path_query_array", jsonpath::PathOut::Items(items)) => {
            Bson::String(json::render_jsonb(&json::Json::Array(items)))
        }
        (_, jsonpath::PathOut::Items(items)) => Bson::Array(
            items
                .iter()
                .map(|j| Bson::String(json::render_jsonb(j)))
                .collect(),
        ),
    })
}

fn json_text_of(v: &Bson) -> String {
    match v {
        Bson::String(s) => s.clone(),
        other => value_text(other),
    }
}

/// A range, a multirange, or an array of either.
fn is_range_family(name: &str) -> bool {
    let element = name.strip_suffix("[]").unwrap_or(name);
    range::is_range_type(element) || range::is_multirange_type(element)
}

/// PostgreSQL's error for JSON text that does not parse. The type is named
/// `json` whether the input was json or jsonb, and the value is not quoted
/// in the message (it goes in the CONTEXT line, which is not carried).
fn json_parse_error(e: json::ParseError) -> Error {
    match e {
        json::ParseError::NulEscape => {
            Error::UntranslatableCharacter("unsupported Unicode escape sequence".into())
        }
        _ => Error::InvalidText("invalid input syntax for type json".into()),
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
    let parsed = json::parse(&text).map_err(json_parse_error)?;
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
        let other = json::parse(&other).map_err(json_parse_error)?;
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

/// Whether an operand is statically an `hstore` -- which needs the extension
/// to be installed, since without it the name is just a name.
fn static_hstore_operand(n: Option<&pg_query::protobuf::Node>, value: &Bson) -> bool {
    let Some(n) = n else {
        return false;
    };
    static_type(n, value) == "hstore" && extension_type("hstore") == Some(ExtensionType::Hstore)
}

/// A datetime function's result type from its call, or `None`.
fn datetime_result_type(f: &pg_query::protobuf::FuncCall, name: &str) -> Option<String> {
    let types: Vec<String> = f.args.iter().map(|a| static_type(a, &Bson::Null)).collect();
    datetime::result_type(name, &types)
}

/// Evaluate a datetime function, typed by its arguments' static types.
fn datetime_call(
    f: &pg_query::protobuf::FuncCall,
    name: &str,
    params: &[Bson],
) -> Option<Result<Bson>> {
    datetime_result_type(f, name)?;
    // `make_interval(days => 10)`: named arguments, the rest defaulted.
    if name == "make_interval"
        && f.args
            .iter()
            .any(|a| matches!(a.node.as_ref(), Some(N::NamedArgExpr(_))))
    {
        const NAMES: [&str; 7] = ["years", "months", "weeks", "days", "hours", "mins", "secs"];
        let mut args = vec![Bson::Int32(0); 7];
        for (i, a) in f.args.iter().enumerate() {
            let (slot, node) = match a.node.as_ref() {
                Some(N::NamedArgExpr(na)) => match NAMES.iter().position(|n| *n == na.name) {
                    Some(p) => (p, na.arg.as_deref()),
                    None => {
                        return Some(Err(Error::UndefinedFunction(format!(
                            "function make_interval({} => unknown) does not exist",
                            na.name
                        ))))
                    }
                },
                _ => (i, Some(a)),
            };
            let Some(node) = node else { continue };
            match const_value(node, params) {
                Ok(v) => args[slot] = v,
                Err(e) => return Some(Err(e)),
            }
        }
        return datetime::make(name, &args);
    }
    let args = match f
        .args
        .iter()
        .map(|a| const_value(a, params))
        .collect::<Result<Vec<_>>>()
    {
        Ok(a) => a,
        Err(e) => return Some(Err(e)),
    };
    let types: Vec<String> = f
        .args
        .iter()
        .zip(&args)
        .map(|(a, v)| static_type(a, v))
        .collect();
    datetime::call(name, &args, &types)
}

/// The built-in a call resolves to when its argument TYPES pick an
/// overload the name alone does not: `length(tsvector)`.
fn overload_name(f: &pg_query::protobuf::FuncCall, name: String) -> String {
    if name == "length" && f.args.len() == 1 && static_type(&f.args[0], &Bson::Null) == "tsvector" {
        return "tsvector_length".to_string();
    }
    name
}

/// Is `n` an untyped string literal or parameter -- a value PostgreSQL's
/// operator resolution types from the OTHER operand?
fn unknown_operand(n: Option<&pg_query::protobuf::Node>) -> bool {
    match n.and_then(|x| x.node.as_ref()) {
        Some(N::AConst(c)) => matches!(c.val.as_ref(), Some(a_const::Val::Sval(_))),
        Some(N::ParamRef(p)) => {
            declared_param_type(usize::try_from(p.number).unwrap_or(0)).is_none()
        }
        _ => false,
    }
}

/// The full-text operators: `@@` / `@@@` (with `text` sides converted as
/// PostgreSQL's `ts_match_tt` / `ts_match_tq` do), `||` / `&&` / `<->` on
/// queries, `||` on vectors. `None`: not a full-text operation.
fn fts_operator(e: &AExpr, op: &str, lhs: &Bson, rhs: &Bson) -> Option<Result<Bson>> {
    if !matches!(op, "@@" | "@@@" | "||" | "&&" | "<->") {
        return None;
    }
    let lt = e
        .lexpr
        .as_deref()
        .map_or_else(String::new, |n| static_type(n, lhs));
    let rt = e
        .rexpr
        .as_deref()
        .map_or_else(String::new, |n| static_type(n, rhs));
    let fts_type = |t: &str| t == "tsvector" || t == "tsquery";
    if op != "@@" && op != "@@@" {
        if !fts_type(&lt) {
            return None;
        }
        if *lhs == Bson::Null || *rhs == Bson::Null {
            return Some(Ok(Bson::Null));
        }
        return Some(fts::operator(op, &lt, lhs, rhs));
    }
    if !fts_type(&lt) && !fts_type(&rt) && !(lt == "text" || unknown_operand(e.lexpr.as_deref())) {
        return None;
    }
    if *lhs == Bson::Null || *rhs == Bson::Null {
        return Some(Ok(Bson::Null));
    }
    // The document side and the query side, with an untyped literal resolved
    // as PostgreSQL resolves it among `tsvector @@ tsquery`,
    // `text @@ tsquery` and `text @@ text`.
    let (lu, ru) = (
        unknown_operand(e.lexpr.as_deref()),
        unknown_operand(e.rexpr.as_deref()),
    );
    let (doc, mut dt, du, query, mut qt, qu) = if lt == "tsquery" {
        (rhs, rt, ru, lhs, lt, lu)
    } else {
        (lhs, lt, lu, rhs, rt, ru)
    };
    if qu {
        qt = if dt == "tsvector" {
            "tsquery".into()
        } else {
            "text".into()
        };
    }
    if du {
        dt = "text".into();
    }
    Some((|| {
        let vector = if dt == "tsvector" {
            fts::parse_vector(&fts::text_of(doc))?
        } else {
            fts::to_tsvector(fts::DEFAULT_CONFIG, &fts::text_of(doc))
        };
        let q = if qt == "tsquery" {
            fts::parse_query(&fts::text_of(query))?
        } else {
            fts::plainto_tsquery(fts::DEFAULT_CONFIG, &fts::text_of(query))
        };
        Ok(Bson::Boolean(fts::matches(&vector, &q)))
    })())
}

/// The result type of an hstore operator, or `None` for an operator this
/// server does not know on hstore.
fn static_hstore_result(op: &str, rhs: Option<&pg_query::protobuf::Node>) -> Option<String> {
    let rhs_is_array = rhs.is_some_and(|n| static_type(n, &Bson::Null).ends_with("[]"));
    Some(match op {
        "->" if rhs_is_array => "text[]".to_string(),
        "->" => "text".to_string(),
        "?" | "?|" | "?&" | "@>" | "<@" => "bool".to_string(),
        "||" | "-" => "hstore".to_string(),
        _ => return None,
    })
}

/// The hstore operators: `->` (a key, or a key list answering `text[]`),
/// `?` / `?|` / `?&` (key tests), `||` (concatenation, right side wins),
/// `-` (delete a key, a key list, or every matching pair of another hstore),
/// `@>` / `<@` (containment). An hstore value travels as its canonical text.
fn hstore_operator(op: &str, lhs: &Bson, rhs: &Bson, rhs_hstore: bool) -> Result<Bson> {
    let left = hstore::parse(&value_text(lhs))?;
    let keys = |v: &Bson| -> Result<Vec<String>> {
        match v {
            Bson::Array(items) => Ok(items.iter().map(value_text).collect()),
            other => match cast_value(other.clone(), "text[]")? {
                Bson::Array(items) => Ok(items.iter().map(value_text).collect()),
                _ => Err(Error::Unsupported(format!("a {op} key list of this shape"))),
            },
        }
    };
    let contains = |outer: &hstore::Pairs, inner: &hstore::Pairs| {
        inner
            .iter()
            .all(|(k, v)| outer.iter().any(|(ok, ov)| ok == k && ov == v))
    };
    Ok(match op {
        "->" => match rhs {
            Bson::Array(items) => Bson::Array(
                items
                    .iter()
                    .map(|k| hstore::get(&left, &value_text(k)).map_or(Bson::Null, Bson::String))
                    .collect(),
            ),
            other => hstore::get(&left, &value_text(other)).map_or(Bson::Null, Bson::String),
        },
        "?" => Bson::Boolean(hstore::has_key(&left, &value_text(rhs))),
        "?|" => Bson::Boolean(keys(rhs)?.iter().any(|k| hstore::has_key(&left, k))),
        "?&" => Bson::Boolean(keys(rhs)?.iter().all(|k| hstore::has_key(&left, k))),
        "||" => {
            let right = hstore::parse(&value_text(rhs))?;
            Bson::String(hstore::render(&hstore::concat(&left, &right)))
        }
        "-" => {
            let kept: hstore::Pairs = match rhs {
                Bson::Array(_) => {
                    let drop = keys(rhs)?;
                    left.into_iter()
                        .filter(|(k, _)| !drop.contains(k))
                        .collect()
                }
                other => {
                    let text = value_text(other);
                    // `hstore - hstore` removes the pairs present in the right
                    // side with the same value; `hstore - text` removes a key.
                    if rhs_hstore {
                        let right = hstore::parse(&text)?;
                        left.into_iter()
                            .filter(|(k, v)| !right.iter().any(|(rk, rv)| rk == k && rv == v))
                            .collect()
                    } else {
                        left.into_iter().filter(|(k, _)| *k != text).collect()
                    }
                }
            };
            Bson::String(hstore::render(&kept))
        }
        "@>" => Bson::Boolean(contains(&left, &hstore::parse(&value_text(rhs))?)),
        "<@" => Bson::Boolean(contains(&hstore::parse(&value_text(rhs))?, &left)),
        _ => return Err(Error::Unsupported(format!("operator {op} on hstore"))),
    })
}

fn coerce_unknown_operand(
    e: &pg_query::protobuf::AExpr,
    lhs: Bson,
    rhs: Bson,
    op: &str,
) -> Result<(Bson, Bson)> {
    if !matches!(
        op,
        "+" | "-" | "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=" | "||"
    ) {
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
            // A parameter the client typed is RESOLVED: it is the side the
            // literal takes its type from, not a second unknown.
            Some(N::ParamRef(p)) => {
                declared_param_type(usize::try_from(p.number).unwrap_or(0)).is_none()
            }
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
    let typed_node = if r_bare {
        e.lexpr.as_deref()
    } else {
        e.rexpr.as_deref()
    };
    // A range beside an unknown parameter: `int4range(10, 20, '[]') = $1`.
    // Both sides are strings by now, so the type has to come from the
    // expression -- and without it the parameter kept the client's spelling
    // while the constructor had been canonicalised, so two spellings of one
    // range compared UNEQUAL while printing identically. This goes FIRST: a
    // range ARRAY parameter decodes to an array of strings, and the array
    // arm below would type the literal beside it as `text[]`, comparing
    // `[1,5]` unequal to the `[1,6)` the parameter canonicalised to.
    if let Some(name) = static_range_type(typed_node) {
        let coerced = cast_value(Bson::String(text.clone()), &name)?;
        return Ok(if r_bare {
            (lhs, coerced)
        } else {
            (coerced, rhs)
        });
    }
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
    let Some(coerced) = coerced else {
        return Ok((lhs, rhs));
    };
    Ok(if r_bare {
        (lhs, coerced)
    } else {
        (coerced, rhs)
    })
}

/// PostgreSQL's `||` on arrays. Arrays of the same dimensionality append;
/// an N-dimensional array takes an (N-1)-dimensional one as a new last (or
/// first) slice, which is how `element || array` and `array || element` are
/// the same rule with N = 1; a NULL or empty side yields the other.
fn array_concat(lhs: Bson, rhs: Bson) -> Result<Bson> {
    fn ndim(v: &Bson) -> usize {
        match v {
            Bson::Array(items) => 1 + items.first().map_or(0, ndim),
            _ => 0,
        }
    }
    let mismatch = || {
        Error::InvalidText(
            "cannot concatenate incompatible arrays: Arrays with differing dimensions are not compatible for concatenation".to_string(),
        )
    };
    match (lhs, rhs) {
        (Bson::Null, other) | (other, Bson::Null) => Ok(other),
        (Bson::Array(a), Bson::Array(b)) if a.is_empty() => Ok(Bson::Array(b)),
        (Bson::Array(a), Bson::Array(b)) if b.is_empty() => Ok(Bson::Array(a)),
        (lhs, rhs) => {
            let (nl, nr) = (ndim(&lhs), ndim(&rhs));
            let out = if nl == nr {
                let (Bson::Array(mut a), Bson::Array(b)) = (lhs, rhs) else {
                    return Err(mismatch());
                };
                a.extend(b);
                a
            } else if nl == nr + 1 {
                let Bson::Array(mut a) = lhs else {
                    return Err(mismatch());
                };
                a.push(rhs);
                a
            } else if nr == nl + 1 {
                let Bson::Array(b) = rhs else {
                    return Err(mismatch());
                };
                let mut a = vec![lhs];
                a.extend(b);
                a
            } else {
                return Err(mismatch());
            };
            if !array_rectangular(&out) {
                return Err(mismatch());
            }
            Ok(Bson::Array(out))
        }
    }
}

/// Is `v` a `time` value's text (`HH:MM[:SS[.ffffff]]`)?
fn is_time_text(v: &Bson) -> bool {
    let Bson::String(s) = v else { return false };
    let parts: Vec<&str> = s.trim().split(':').collect();
    (2..=3).contains(&parts.len())
        && parts[..2]
            .iter()
            .all(|p| (1..=2).contains(&p.len()) && p.chars().all(|c| c.is_ascii_digit()))
        && parts.get(2).is_none_or(|p| {
            let (w, f) = p.split_once('.').unwrap_or((p, "0"));
            w.len() == 2
                && w.chars().all(|c| c.is_ascii_digit())
                && f.chars().all(|c| c.is_ascii_digit())
        })
}

fn eval_binary(op: &str, lhs: Bson, rhs: Bson) -> Result<Bson> {
    // Array concatenation is `array_cat`, which is NOT strict: a NULL beside
    // an array is the array. So it goes before the NULL propagation below.
    if op == "||" && (matches!(lhs, Bson::Array(_)) || matches!(rhs, Bson::Array(_))) {
        return array_concat(lhs, rhs);
    }
    // NULL propagates through every operator here (PG: `1 + NULL` is NULL).
    if lhs == Bson::Null || rhs == Bson::Null {
        return Ok(Bson::Null);
    }

    if op == "||" {
        // `bytea || bytea` is BYTE concatenation, not text.
        if let (Bson::Binary(a), Bson::Binary(b)) = (&lhs, &rhs) {
            let mut bytes = a.bytes.clone();
            bytes.extend_from_slice(&b.bytes);
            return Ok(bytea::to_binary(bytes));
        }
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
            (None, Some(iv)) if is_time_text(&lhs) => {
                return datetime::time_plus(&lhs, &iv, sign);
            }
            (Some(iv), None) if op == "+" && is_time_text(&rhs) => {
                return datetime::time_plus(&rhs, &iv, 1);
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

        // `date +/- int` and `date - date`. A date is carried as `YYYY-MM-DD`
        // text; only an ORDINARY date (not infinity / a wide-year or BC text
        // the client can never hold) takes part, and only against an INTEGER,
        // so an ambiguous string never lands here. The result is rendered in
        // PostgreSQL's own text -- including the BC era and years past 9999 --
        // and typed `date` (or `int4` for date-date) by `static_type`, so the
        // client's loader is what rejects an unrepresentable result.
        let as_int = |v: &Bson| match v {
            Bson::Int32(i) => Some(i64::from(*i)),
            Bson::Int64(i) => Some(*i),
            _ => None,
        };
        let as_ymd = |v: &Bson| match v {
            Bson::String(s) => NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok(),
            _ => None,
        };
        if let (Some(d), Some(n)) = (as_ymd(&lhs), as_int(&rhs)) {
            let out = d
                .checked_add_signed(chrono::Duration::days(sign * n))
                .ok_or_else(|| Error::DatetimeFieldOverflow("date out of range".to_string()))?;
            return Ok(Bson::String(render_date_pg(out)));
        }
        if op == "+" {
            if let (Some(n), Some(d)) = (as_int(&lhs), as_ymd(&rhs)) {
                let out = d
                    .checked_add_signed(chrono::Duration::days(n))
                    .ok_or_else(|| Error::DatetimeFieldOverflow("date out of range".to_string()))?;
                return Ok(Bson::String(render_date_pg(out)));
            }
        }
        if op == "-" {
            if let (Some(a), Some(b)) = (as_ymd(&lhs), as_ymd(&rhs)) {
                let days = a.signed_duration_since(b).num_days();
                return Ok(Bson::Int32(days as i32));
            }
            // timestamp - timestamp: two stored instants (never texts,
            // which a date or a time also is).
            let stored = |v: &Bson| {
                matches!(v, Bson::DateTime(_))
                    || matches!(v, Bson::Document(d) if d.contains_key(COMPOSITE_DATE))
            };
            if stored(&lhs) && stored(&rhs) {
                if let (Some(a), Some(b)) = (instant_micros(&lhs), instant_micros(&rhs)) {
                    return Ok(datetime::timestamp_diff(a, b));
                }
            }
            if is_time_text(&lhs) && is_time_text(&rhs) {
                return datetime::time_diff(&lhs, &rhs);
            }
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
        // Two records compare field by field (see record_compare). The AST-less
        // path treats them as composite VALUES (NULLs equal): the row-constructor
        // three-valued rule is applied by const_value, which alone can tell a
        // bare `ROW(...)` from a composite value.
        if let (Some(a), Some(b)) = (record_fields(&lhs), record_fields(&rhs)) {
            return record_compare(op, a, b, true);
        }
        // A scalar compared to an ARRAY with no ANY/ALL is an operator
        // PostgreSQL does not have (`text = text[]` is 42883). Array = array is
        // a real element-wise operator and stays; only a scalar/array MISMATCH
        // is the undefined one.
        if matches!(&lhs, Bson::Array(_)) ^ matches!(&rhs, Bson::Array(_)) {
            return Err(Error::UndefinedFunction(format!(
                "operator does not exist: {} {op} {}",
                inferred_type(&lhs),
                inferred_type(&rhs)
            )));
        }
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
    if matches!(op, "+" | "-" | "*" | "/")
        && (is_numeric(&lhs) || is_numeric(&rhs))
        && !matches!(lhs, Bson::Double(_))
        && !matches!(rhs, Bson::Double(_))
    {
        let text = numeric::numeric_operand_text;
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
            // declared type really is float8 -- and a numeric on the other side
            // is coerced to it, so `0.1::float8 + 0.2` is float8 arithmetic
            // (PostgreSQL's numeric-to-float8 implicit cast).
            // A numeric with NO float8 beside it stays numeric: what
            // `decimal_arith` refused (division) must not be answered by a
            // float instead, since `1.5 / 3` is a numeric on PostgreSQL.
            let mixed_float = matches!(lhs, Bson::Double(_)) || matches!(rhs, Bson::Double(_));
            let floats = |v: &Bson| match v {
                Bson::Double(d) => Some(*d),
                Bson::Int32(i) => Some(f64::from(*i)),
                Bson::Int64(i) => Some(*i as f64),
                v if mixed_float && is_numeric(v) => {
                    numeric_text(v).and_then(|t| numeric::numeric_text_to_f64(&t))
                }
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
        Bson::Document(d) if d.contains_key(WIDE_NUMERIC_KEY) => "numeric",
        Bson::Document(_) => "document",
        _ => "other",
    }
}

/// The public door onto `compare_constants` for the wire layer's join sort.
pub fn compare_values(a: &Bson, b: &Bson) -> Option<std::cmp::Ordering> {
    compare_constants(a, b)
}

pub(crate) fn compare_constants(a: &Bson, b: &Bson) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Bson::String(x), Bson::String(y)) => Some(x.cmp(y)),
        // `bytea` orders by UNSIGNED byte value, lexicographically -- exactly
        // what `Vec<u8>` gives -- so `'\\x01' < '\\x02'` and a prefix sorts first.
        (Bson::Binary(x), Bson::Binary(y)) => Some(x.bytes.cmp(&y.bytes)),
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
        // A regtype compares as its OID: `where t.oid = to_regtype('text')`;
        // a regclass the same (`where attrelid = 't1'::regclass`).
        (a2, b2)
            if regtype_oid(a2).is_some()
                || regtype_oid(b2).is_some()
                || regclass_oid(a2).is_some()
                || regclass_oid(b2).is_some() =>
        {
            let num = |v: &Bson| -> Option<i64> {
                regtype_oid(v).or_else(|| regclass_oid(v)).or(match v {
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
            // two different numbers equal. Rendered PLAIN first: Decimal128
            // writes `-8.34184E-7` for a small magnitude, and the digit
            // comparison has no notion of an exponent.
            let dec = numeric::numeric_operand_text;
            // A decimal beside a FLOAT compares as floats: PostgreSQL widens
            // the numeric to float8 for that operator, so the float's own
            // precision governs and the exact path would be the wrong answer.
            let mixed_float = matches!(a, Bson::Double(_)) || matches!(b, Bson::Double(_));
            if (is_numeric(a) || is_numeric(b)) && !mixed_float {
                return compare_decimal_text(&dec(a)?, &dec(b)?);
            }
            let f = |v: &Bson| match v {
                Bson::Int32(i) => Some(f64::from(*i)),
                Bson::Int64(i) => Some(*i as f64),
                Bson::Double(d) => Some(*d),
                v if is_numeric(v) => {
                    numeric_text(v).and_then(|t| numeric::numeric_text_to_f64(&t))
                }
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
        // A NULL setting name answers NULL, not an error -- psycopg's own
        // tests pass one through a parameter.
        "current_setting"
            if !f.args.is_empty()
                && f.args.len() <= 2
                && const_value(&f.args[0], params)? == Bson::Null =>
        {
            Ok(Some(ConstCol::Value(Bson::Null)))
        }
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
        // A NULL setting name folds to NULL, not an error -- during a DESCRIBE
        // the value parameters are still unbound, so the name argument arrives
        // as NULL and must not blow up the plan (mirrors `current_setting`,
        // which psycopg's transaction-parameter tests exercise the same way).
        "set_config" if f.args.len() == 3 && const_value(&f.args[0], params)? == Bson::Null => {
            Ok(Some(ConstCol::Value(Bson::Null)))
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
        // `SET TRANSACTION ...` and `SET SESSION CHARACTERISTICS AS TRANSACTION
        // ...` are both VAR_SET_MULTI: the name distinguishes them and the args
        // are DefElem characteristics rather than plain values.
        Ok(VariableSetKind::VarSetMulti) => {
            let modes = parse_transaction_modes(&v.args);
            return match v.name.as_str() {
                "TRANSACTION" => Ok(Statement::SetTransaction(modes)),
                "SESSION CHARACTERISTICS" => Ok(Statement::SetSessionCharacteristics(modes)),
                // `SET TRANSACTION SNAPSHOT` and any other multi form this
                // server does not model.
                _ => Err(Error::Unsupported("this SET form".into())),
            };
        }
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
    // `DROP SCHEMA`: the object is a bare String / one-element List.
    if ObjectType::try_from(d.remove_type) == Ok(ObjectType::ObjectSchema) {
        let mut names = Vec::new();
        for obj in &d.objects {
            match obj.node.as_ref() {
                Some(N::String(st)) => names.push(st.sval.clone()),
                Some(N::List(l)) => {
                    let name = l
                        .items
                        .iter()
                        .filter_map(|n| match n.node.as_ref()? {
                            N::String(st) => Some(st.sval.clone()),
                            _ => None,
                        })
                        .next_back()
                        .ok_or_else(|| Error::Parse("DROP SCHEMA without a name".into()))?;
                    names.push(name);
                }
                _ => return Err(Error::Unsupported("this DROP SCHEMA target".into())),
            }
        }
        return Ok(Statement::DropSchema {
            names,
            if_exists: d.missing_ok,
            cascade: DropBehavior::try_from(d.behavior) == Ok(DropBehavior::DropCascade),
        });
    }
    // `DROP EXTENSION`: each object is a bare String.
    if ObjectType::try_from(d.remove_type) == Ok(ObjectType::ObjectExtension) {
        let names = d
            .objects
            .iter()
            .filter_map(|obj| match obj.node.as_ref()? {
                N::String(s) => Some(s.sval.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if names.is_empty() {
            return Err(Error::Unsupported("this DROP EXTENSION target".into()));
        }
        return Ok(Statement::DropExtension {
            names,
            if_exists: d.missing_ok,
            cascade: DropBehavior::try_from(d.behavior) == Ok(DropBehavior::DropCascade),
        });
    }
    // `DROP TYPE`: the object is a TypeName, not a List of name parts.
    if ObjectType::try_from(d.remove_type) == Ok(ObjectType::ObjectType) {
        let mut names = Vec::new();
        for obj in &d.objects {
            let Some(N::TypeName(tn)) = obj.node.as_ref() else {
                return Err(Error::Unsupported("this DROP TYPE target".into()));
            };
            // Keep the qualifier: `testschema.t` drops the composite keyed on
            // `testschema.t`, distinct from a bare `t`. `public.t` normalises to
            // bare `t`, which is how an unqualified composite is stored.
            let parts: Vec<String> = tn
                .names
                .iter()
                .filter_map(|n| match n.node.as_ref()? {
                    N::String(s) => Some(s.sval.clone()),
                    _ => None,
                })
                .collect();
            let name = match parts.as_slice() {
                [] => return Err(Error::Parse("DROP TYPE without a name".into())),
                [bare] => bare.clone(),
                [schema, bare] if schema == "public" => bare.clone(),
                _ => parts.join("."),
            };
            names.push(name);
        }
        return Ok(Statement::DropType {
            names,
            if_exists: d.missing_ok,
            cascade: DropBehavior::try_from(d.behavior) == Ok(DropBehavior::DropCascade),
        });
    }
    // `DROP FUNCTION`: each object is an ObjectWithArgs -- the name parts plus
    // the declared argument types, which PostgreSQL needs to pick one overload.
    if ObjectType::try_from(d.remove_type) == Ok(ObjectType::ObjectFunction) {
        if d.objects.len() != 1 {
            return Err(Error::Unsupported(
                "DROP FUNCTION of more than one function".into(),
            ));
        }
        let Some(N::ObjectWithArgs(o)) = d.objects[0].node.as_ref() else {
            return Err(Error::Unsupported("this DROP FUNCTION target".into()));
        };
        let name = o
            .objname
            .iter()
            .filter_map(|n| match n.node.as_ref()? {
                N::String(s) => Some(s.sval.clone()),
                _ => None,
            })
            .next_back()
            .ok_or_else(|| Error::Parse("DROP FUNCTION without a name".into()))?;
        let arg_types = if o.args_unspecified {
            None
        } else {
            Some(
                o.objargs
                    .iter()
                    .filter_map(|n| match n.node.as_ref()? {
                        N::TypeName(tn) => Some(type_name_of(tn)),
                        _ => None,
                    })
                    .collect(),
            )
        };
        return Ok(Statement::DropFunction {
            name,
            arg_types,
            if_exists: d.missing_ok,
            cascade: DropBehavior::try_from(d.behavior) == Ok(DropBehavior::DropCascade),
        });
    }
    // `DROP TRIGGER name ON table`: the object is the list
    // `[schema.]table.trigger`.
    if ObjectType::try_from(d.remove_type) == Ok(ObjectType::ObjectTrigger) {
        let Some(N::List(l)) = d.objects.first().and_then(|o| o.node.as_ref()) else {
            return Err(Error::Parse("DROP TRIGGER without a name".into()));
        };
        let parts: Vec<String> = l
            .items
            .iter()
            .filter_map(|n| match n.node.as_ref()? {
                N::String(s) => Some(s.sval.clone()),
                _ => None,
            })
            .collect();
        let [.., table, name] = parts.as_slice() else {
            return Err(Error::Parse("DROP TRIGGER without a table".into()));
        };
        return Ok(Statement::DropTrigger {
            name: name.clone(),
            table: table.clone(),
            if_exists: d.missing_ok,
        });
    }
    // `DROP SEQUENCE`: each object is a List of name parts, like a table's.
    if ObjectType::try_from(d.remove_type) == Ok(ObjectType::ObjectSequence) {
        let mut names = Vec::new();
        for obj in &d.objects {
            let name = match obj.node.as_ref() {
                Some(N::String(s)) => s.sval.clone(),
                Some(N::List(l)) => l
                    .items
                    .iter()
                    .filter_map(|n| match n.node.as_ref()? {
                        N::String(s) => Some(s.sval.clone()),
                        _ => None,
                    })
                    .next_back()
                    .ok_or_else(|| Error::Parse("DROP SEQUENCE without a name".into()))?,
                _ => return Err(Error::Unsupported("this DROP SEQUENCE target".into())),
            };
            names.push(name);
        }
        return Ok(Statement::DropSequence {
            names,
            if_exists: d.missing_ok,
        });
    }
    // `DROP INDEX` / `DROP VIEW`: each object is a List of name parts.
    if matches!(
        ObjectType::try_from(d.remove_type),
        Ok(ObjectType::ObjectIndex | ObjectType::ObjectView)
    ) {
        let index = ObjectType::try_from(d.remove_type) == Ok(ObjectType::ObjectIndex);
        let cascade = DropBehavior::try_from(d.behavior) == Ok(DropBehavior::DropCascade);
        let mut names = Vec::new();
        for obj in &d.objects {
            let name = match obj.node.as_ref() {
                Some(N::String(s)) => s.sval.clone(),
                Some(N::List(l)) => l
                    .items
                    .iter()
                    .filter_map(|n| match n.node.as_ref()? {
                        N::String(s) => Some(s.sval.clone()),
                        _ => None,
                    })
                    .next_back()
                    .ok_or_else(|| Error::Parse("DROP without a name".into()))?,
                _ => return Err(Error::Unsupported("this DROP target".into())),
            };
            names.push(name);
        }
        if index {
            if d.concurrent && names.len() > 1 {
                return Err(Error::FeatureNotSupported(
                    "DROP INDEX CONCURRENTLY does not support dropping multiple objects".into(),
                ));
            }
            // An index has no dependants this server models, so CASCADE and
            // RESTRICT drop the same thing.
            return Ok(Statement::DropIndex {
                names,
                if_exists: d.missing_ok,
            });
        }
        return Ok(Statement::DropView {
            names,
            if_exists: d.missing_ok,
            cascade,
        });
    }
    if ObjectType::try_from(d.remove_type) != Ok(ObjectType::ObjectTable) {
        // Named, not `{:?}`: the debug form of a protobuf enum leaked to the
        // wire here for as long as DROP knew only tables.
        let what = match ObjectType::try_from(d.remove_type) {
            Ok(ObjectType::ObjectSchema) => "a schema",
            Ok(ObjectType::ObjectIndex) => "an index",
            Ok(ObjectType::ObjectView) => "a view",
            Ok(ObjectType::ObjectSequence) => "a sequence",
            Ok(ObjectType::ObjectFunction) => "a function",
            Ok(ObjectType::ObjectDomain) => "a domain",
            _ => "this object kind",
        };
        return Err(Error::Unsupported(format!("DROP of {what}")));
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
    let def = lookup(&table).ok_or_else(|| write_target_missing(&table, "UPDATE of"))?;

    let mut set = Document::new();
    let mut unset: Vec<String> = Vec::new();
    let mut set_exprs: Vec<(String, String, ColumnExpr)> = Vec::new();
    let mut set_subscripts: Vec<SubscriptAssign> = Vec::new();
    let row_fields = || -> (Vec<RowField>, Document) {
        let fields: Vec<RowField> = def
            .columns
            .iter()
            .map(|c| (c.name.clone(), c.field(), c.pg_type.clone()))
            .collect();
        let mut sample = Document::new();
        for c in &def.columns {
            sample.insert(c.field(), sample_value_for_type(&c.pg_type));
        }
        (fields, sample)
    };
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
        let val = rt
            .val
            .as_ref()
            .ok_or_else(|| Error::Parse("SET without a value".into()))?;
        // `SET a[i] = v` -- the target is one element (or slice) of the array
        // the column holds, so the assignment reads the old value. Checked
        // BEFORE `check_assignment_type`, which would compare the element
        // against the column's ARRAY type and report `cannot cast int4 to
        // int4[]` -- an error about a cast the statement never asked for.
        if !rt.indirection.is_empty() {
            let (fields, sample) = row_fields();
            set_subscripts.push(plan_subscript_assign(
                column,
                &field,
                &rt.indirection,
                val,
                &fields,
                params,
                &sample,
            )?);
            continue;
        }
        // `SET c = DEFAULT`: the column's own default, whatever form it has.
        let default_node;
        let val = if matches!(val.node.as_ref(), Some(N::SetToDefault(_))) {
            default_node = column_default_node(column)?;
            &default_node
        } else {
            &**val
        };
        check_assignment_type(column, val)?;
        // A value that reads the row (`num * 2`) has no constant to store;
        // it is planned as a row expression and evaluated per matched row --
        // as is a VOLATILE one (`nextval`, `random()`), which PostgreSQL
        // evaluates once per row, not once per statement.
        if references_columns(val) || default_is_volatile(val) {
            let (fields, sample) = row_fields();
            let row = row_column_expr(val, &fields, params, &sample)?;
            set_exprs.push((field, column.pg_type.clone(), row));
            continue;
        }
        let value = cast_value(const_value(val, params)?, &column.pg_type)?;
        set_stored_value(&mut set, &mut unset, field, value);
    }
    if set.is_empty() && set_exprs.is_empty() && set_subscripts.is_empty() {
        return Err(Error::Parse("UPDATE without a SET list".into()));
    }
    let (filter, residual) = match u.where_clause.as_ref() {
        None => (Document::new(), None),
        Some(w) => lower_where_or_residual(w, &def, params)?,
    };
    let returning = if u.returning_list.is_empty() {
        None
    } else {
        let (columns, casts) = plan_table_targets(&u.returning_list, &def, params)?;
        Some(Returning { columns, casts })
    };

    Ok(Statement::Update(Update {
        table,
        set,
        unset,
        set_exprs,
        set_subscripts,
        filter,
        residual,
        returning,
    }))
}

/// Record one column's new value in an UPDATE's `$set` / `$unset` lists.
///
/// `carry_subms` writes the companion into a scratch document; a remainder
/// becomes another `$set`, its absence an explicit `$unset`, so an update to
/// a whole-millisecond timestamp clears any remainder the row carried.
fn set_stored_value(set: &mut Document, unset: &mut Vec<String>, field: String, value: Bson) {
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

/// The `$set` / `$unset` lists for one matched row of an UPDATE whose SET
/// list reads the row: the constant assignments plus each row expression
/// evaluated over `row` and cast to its column's declared type.
/// The row a `DO UPDATE` expression sees: the EXISTING row, plus the proposed
/// row's fields under `EXCLUDED_PREFIX`.
///
/// Built per conflict rather than once, because the existing row differs for
/// every conflicting key.
pub fn on_conflict_row(existing: &Document, proposed: &Document) -> Document {
    let mut row = existing.clone();
    for (k, v) in proposed {
        row.insert(format!("{EXCLUDED_PREFIX}{k}"), v.clone());
    }
    row
}

/// `update_row_sets` for an `ON CONFLICT DO UPDATE` assignment list.
pub fn on_conflict_row_sets(
    set_exprs: &[(String, String, ColumnExpr)],
    row: &Document,
) -> Result<(Document, Vec<String>)> {
    let mut set = Document::new();
    let mut unset = Vec::new();
    for (field, pg_type, expr) in set_exprs {
        let value = cast_value(apply_row_expr(expr, row)?, pg_type)?;
        set_stored_value(&mut set, &mut unset, field.clone(), value);
    }
    Ok((set, unset))
}

/// Whether a `DO UPDATE ... WHERE` passes for this row.
///
/// SQL's three-valued logic: only TRUE runs the update — NULL and FALSE both
/// skip it, exactly as a `WHERE` on a plain UPDATE matches no row.
pub fn on_conflict_filter_passes(filter: &ColumnExpr, row: &Document) -> Result<bool> {
    Ok(matches!(apply_row_expr(filter, row)?, Bson::Boolean(true)))
}

pub fn update_row_sets(upd: &Update, row: &Document) -> Result<(Document, Vec<String>)> {
    let mut set = upd.set.clone();
    let mut unset = upd.unset.clone();
    for (field, pg_type, expr) in &upd.set_exprs {
        let value = cast_value(apply_row_expr(expr, row)?, pg_type)?;
        set_stored_value(&mut set, &mut unset, field.clone(), value);
    }
    for a in &upd.set_subscripts {
        // `SET a[1] = 1, a[2] = 2` assigns into the array twice, so the second
        // has to see the first: read back what this loop already wrote before
        // falling back to the stored row.
        let current = set
            .get(&a.field)
            .cloned()
            .unwrap_or_else(|| row.get(&a.field).cloned().unwrap_or(Bson::Null));
        let value = cast_value(apply_subscript_assign(a, row, current)?, &a.pg_type)?;
        set_stored_value(&mut set, &mut unset, a.field.clone(), value);
    }
    Ok((set, unset))
}

/// Plan one `SET a[...] = v`.
///
/// Every subscript is planned as an expression over the row, because `a[n]`
/// may name a column. A subscript BELOW 1 is refused: PostgreSQL answers it by
/// moving the array's lower bound (`UPDATE ... SET ia[0] = 0` leaves an
/// `[0:5]={...}`), and this server does not model lower bounds -- see the
/// `arrays` module header. Refusing is the honest answer; re-basing to 1 would
/// silently shift every other subscript into the array.
#[allow(clippy::too_many_arguments)]
fn plan_subscript_assign(
    column: &secantus_pgcatalog::Column,
    field: &str,
    indirection: &[pg_query::protobuf::Node],
    val: &pg_query::protobuf::Node,
    fields: &[RowField],
    params: &[Bson],
    sample: &Document,
) -> Result<SubscriptAssign> {
    let element = column.pg_type.strip_suffix("[]").ok_or_else(|| {
        Error::DatatypeMismatch(format!(
            "cannot subscript type {} because it does not support subscripting",
            column.pg_type
        ))
    })?;
    let mut subs = Vec::with_capacity(indirection.len());
    let mut slices = 0usize;
    for ind in indirection {
        let Some(N::AIndices(idx)) = ind.node.as_ref() else {
            // A field selection (`SET c.f = 1`) on a composite column is a
            // different construct, and this server has none.
            return Err(Error::Unsupported("UPDATE of a field of a column".into()));
        };
        let bound = |n: Option<&pg_query::protobuf::Node>| -> Result<Option<ColumnExpr>> {
            n.map(|n| row_column_expr(n, fields, params, sample))
                .transpose()
        };
        if idx.is_slice {
            slices += 1;
            subs.push(SubscriptTarget::Slice(
                bound(idx.lidx.as_deref())?,
                bound(idx.uidx.as_deref())?,
            ));
        } else {
            let i = bound(idx.uidx.as_deref())?
                .ok_or_else(|| Error::Parse("array subscript with no index".into()))?;
            subs.push(SubscriptTarget::Index(i));
        }
    }
    // A slice assignment rewrites a whole range, and doing that in more than
    // one dimension at once needs the shape rules an element assignment does
    // not. Refused by name rather than half-applied.
    if slices > 1 || (slices == 1 && subs.len() > 1) {
        return Err(Error::Unsupported(
            "UPDATE of a slice of a multidimensional array".into(),
        ));
    }
    let value_type = if slices == 1 {
        column.pg_type.clone()
    } else {
        element.to_string()
    };
    Ok(SubscriptAssign {
        field: field.to_string(),
        pg_type: column.pg_type.clone(),
        value_type,
        subs,
        value: row_column_expr(val, fields, params, sample)?,
    })
}

/// Apply one `SET a[...] = v` to the array the row holds.
///
/// A subscript past the end EXTENDS the array, padding the gap with NULLs:
/// `ia[5] = 5` over `{1,2,3}` is `{1,2,3,NULL,5}`, and assigning into a NULL
/// column builds the array from nothing. A slice whose source is shorter than
/// the range is PostgreSQL's own "source array too small"; a longer one has
/// its tail ignored.
fn apply_subscript_assign(a: &SubscriptAssign, row: &Document, current: Bson) -> Result<Bson> {
    let value = cast_value(apply_row_expr(&a.value, row)?, &a.value_type)?;
    let index = |e: &ColumnExpr| -> Result<i64> {
        let i = arrays::subscript_index(&apply_row_expr(e, row)?)?;
        if i < 1 {
            return Err(Error::Unsupported(
                "UPDATE of an array element below subscript 1".into(),
            ));
        }
        // A subscript past the end EXTENDS the array, so the subscript is the
        // size being asked for -- `SET a[1000000000] = 1` is a one-line
        // statement that would otherwise allocate a billion slots.
        arrays::check_array_size(i)?;
        Ok(i)
    };
    if let [SubscriptTarget::Slice(lo, hi)] = a.subs.as_slice() {
        let items = match &current {
            Bson::Array(items) => items.clone(),
            _ => Vec::new(),
        };
        let lo = lo.as_ref().map(&index).transpose()?.unwrap_or(1);
        let hi = match hi.as_ref().map(&index).transpose()? {
            Some(h) => h,
            None => items.len() as i64,
        };
        let Bson::Array(source) = &value else {
            // A NULL source for a slice is PostgreSQL's own error rather than
            // a no-op: there is nothing to copy into the range.
            return Err(Error::DataException("source array too small".into()));
        };
        let width = (hi - lo + 1).max(0) as usize;
        if source.len() < width {
            return Err(Error::DataException("source array too small".into()));
        }
        let mut out = items;
        out.resize(out.len().max(hi.max(0) as usize), Bson::Null);
        for (n, slot) in (lo..=hi).enumerate() {
            out[slot as usize - 1] = source[n].clone();
        }
        return Ok(Bson::Array(out));
    }
    let mut path = Vec::with_capacity(a.subs.len());
    for sub in &a.subs {
        match sub {
            SubscriptTarget::Index(e) => path.push(index(e)?),
            SubscriptTarget::Slice(..) => {
                return Err(Error::Unsupported(
                    "UPDATE of a slice of a multidimensional array".into(),
                ))
            }
        }
    }
    fn place(current: &Bson, path: &[i64], value: Bson) -> Bson {
        let Some((&head, rest)) = path.split_first() else {
            return value;
        };
        let mut items = match current {
            Bson::Array(items) => items.clone(),
            _ => Vec::new(),
        };
        let slot = head as usize - 1;
        if items.len() <= slot {
            items.resize(slot + 1, Bson::Null);
        }
        items[slot] = place(&items[slot].clone(), rest, value);
        Bson::Array(items)
    }
    Ok(place(&current, &path, value))
}

/// Whether `node` reads a column anywhere beneath it.
fn references_columns(node: &pg_query::protobuf::Node) -> bool {
    let mut probe = node.clone();
    // Rewriting against NO fields turns the first column reference into an
    // `UndefinedColumn`; a node with none rewrites cleanly.
    matches!(
        rewrite_column_refs(&mut probe, &[], 0),
        Err(Error::UndefinedColumn(_))
    )
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
    let def = lookup(&table).ok_or_else(|| write_target_missing(&table, "DELETE from"))?;
    let (filter, residual) = match d.where_clause.as_ref() {
        None => (Document::new(), None),
        Some(w) => lower_where_or_residual(w, &def, params)?,
    };
    let returning = if d.returning_list.is_empty() {
        None
    } else {
        let (columns, casts) = plan_table_targets(&d.returning_list, &def, params)?;
        Some(Returning { columns, casts })
    };

    Ok(Statement::Delete(Delete {
        table,
        filter,
        residual,
        returning,
    }))
}

fn plan_truncate(
    t: &pg_query::protobuf::TruncateStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Statement> {
    let mut tables = Vec::with_capacity(t.relations.len());
    for rel in &t.relations {
        let Some(N::RangeVar(r)) = rel.node.as_ref() else {
            return Err(Error::Parse("TRUNCATE without a relation".into()));
        };
        lookup(&r.relname).ok_or_else(|| Error::UndefinedTable(r.relname.clone()))?;
        if !tables.contains(&r.relname) {
            tables.push(r.relname.clone());
        }
    }
    Ok(Statement::Truncate {
        tables,
        restart_identity: t.restart_seqs,
        cascade: DropBehavior::try_from(t.behavior) == Ok(DropBehavior::DropCascade),
    })
}

/// A WHERE predicate as a Mongo filter over STORED FIELDS.
pub fn lower_where(
    node: &pg_query::protobuf::Node,
    def: &TableDef,
    params: &[Bson],
) -> Result<Document> {
    // A predicate over no column -- `WHERE false`, `WHERE $1` -- is decided
    // once: every row, or none (NULL is none).
    if matches!(
        node.node.as_ref(),
        Some(N::AConst(_) | N::TypeCast(_) | N::ParamRef(_))
    ) && !references_columns(node)
    {
        return Ok(match const_value(node, params)? {
            Bson::Boolean(true) => Document::new(),
            Bson::Boolean(false) | Bson::Null => doc! { "_id": { "$in": [] } },
            other => {
                return Err(Error::DatatypeMismatch(format!(
                    "argument of WHERE must be type boolean, not type {}",
                    inferred_type(&other)
                )))
            }
        });
    }
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
        // Casting a timestamptz INSTANT to text renders it in the session zone
        // (the instant alone cannot say it is a timestamptz, so the source cast
        // decides). PLAN_TIMEZONE is set during planning, where this evaluates.
        if target == "text" && static_type(arg, &value) == "timestamptz" {
            if let Some(t) = timestamptz_value_text(&value, &session_timezone()) {
                return Ok(Bson::String(t));
            }
        }
        // To a zone-less type, a timestamptz is first the session zone's WALL
        // CLOCK: `now()::date` is today where the session is, not in UTC.
        if matches!(target.as_str(), "date" | "timestamp" | "time" | "timetz")
            && static_type(arg, &value) == "timestamptz"
        {
            if let Some(v) = timestamptz_as_local(&value, &target)? {
                return Ok(v);
            }
        }
        // A redundant `timestamptz` -> `timestamptz` cast (e.g. `$1::timestamptz`
        // over a parameter already declared timestamptz) is a NO-OP: the value
        // is the stored INSTANT. Routing it through `cast_value` would render it
        // to offset-less wall-clock text and re-parse THAT with the session
        // zone, applying the zone a second time and moving the instant -- the
        // `timestamp` -> `timestamptz` path legitimately applies the zone, and
        // must not fire for a value that is already an instant. (`timestamp` ->
        // `timestamptz` still reaches `cast_value`, since its source type is
        // `timestamp`, not `timestamptz`.)
        let is_stored_instant = matches!(&value, Bson::DateTime(_))
            || matches!(&value, Bson::Document(d) if d.contains_key(COMPOSITE_DATE));
        if target == "timestamptz" && is_stored_instant && static_type(arg, &value) == "timestamptz"
        {
            return Ok(value);
        }
        // `boolean` has casts from `integer` and text only. A `smallint` or a
        // `date` is stored in the same Bson shape as an integer or a text (an
        // Int32, a String), so `cast_value` cannot tell `1::int2::bool` from
        // `1::bool`, or `'2021-01-01'::date::bool` from `'2021-01-01'::bool`;
        // the SOURCE type at the cast site is what decides (42846 on
        // PostgreSQL, where the text form is a 22P02 parse failure).
        if matches!(target.as_str(), "bool" | "boolean") {
            let source = static_type(arg, &value);
            if matches!(
                source.as_str(),
                "int2"
                    | "smallint"
                    | "date"
                    | "time"
                    | "timetz"
                    | "timestamp"
                    | "timestamptz"
                    | "interval"
            ) {
                return Err(Error::CannotCoerce(format!(
                    "cannot cast type {} to boolean",
                    display_type(&source)
                )));
            }
        }
        return cast_value(value, &target);
    }
    // `(expr).field` -- select a named field from a composite or record value.
    // The field NAME resolves to a position through the source's type: a named
    // composite carries its field list, an anonymous record names them f1, f2,
    // ... by position.
    if let Some(N::AIndirection(ind)) = node.node.as_ref() {
        let arg = ind
            .arg
            .as_ref()
            .ok_or_else(|| Error::Parse("field selection with no operand".into()))?;
        // An indirection made ENTIRELY of subscripts is an array reference,
        // not a field selection: `ia[1]`, `ia[2:3]`, `m[1][2]`, `m[1:2][1:1]`.
        if ind
            .indirection
            .iter()
            .all(|i| matches!(i.node.as_ref(), Some(N::AIndices(_))))
            && !ind.indirection.is_empty()
        {
            return array_subscript(arg, &ind.indirection, params);
        }
        // Only single field-name selection is supported here; `(rec).*` and a
        // mixed subscript/field chain are separate constructs.
        if ind.indirection.len() != 1 {
            return Err(Error::Unsupported("this field selection".into()));
        }
        let field = match ind.indirection[0].node.as_ref() {
            Some(N::String(s)) => s.sval.clone(),
            _ => return Err(Error::Unsupported("this field selection".into())),
        };
        let value = const_value(arg, params)?;
        if value == Bson::Null {
            return Ok(Bson::Null);
        }
        let fields = record_fields(&value)
            .ok_or_else(|| Error::Unsupported("field selection on a non-record value".into()))?;
        let ty = static_type(arg, &value);
        let (idx, err) = if let Some((_, comp_fields)) = user_composite(&ty) {
            (
                comp_fields.iter().position(|(n, _)| *n == field),
                format!("column \"{field}\" not found in data type {ty}"),
            )
        } else {
            // An anonymous record names its fields f1, f2, ... by position.
            (
                field
                    .strip_prefix('f')
                    .and_then(|n| n.parse::<usize>().ok())
                    .filter(|n| *n >= 1)
                    .map(|n| n - 1),
                format!("could not identify column \"{field}\" in record data type"),
            )
        };
        // A position past the end is the same error as an unknown name: an
        // anonymous record has exactly as many `fN` fields as it has values.
        return fields
            .get(idx.ok_or_else(|| Error::UndefinedField(err.clone()))?)
            .cloned()
            .ok_or(Error::UndefinedField(err));
    }
    if let Some(N::SqlvalueFunction(svf)) = node.node.as_ref() {
        return sql_value_function(svf);
    }
    // `x COLLATE "C"`: every comparison here is already bytewise, which IS
    // the C collation, so the C-equivalent names change nothing. A locale
    // collation would order differently, and is refused by name.
    if let Some(N::CollateClause(cc)) = node.node.as_ref() {
        let arg = cc
            .arg
            .as_deref()
            .ok_or_else(|| Error::Parse("COLLATE without an operand".into()))?;
        let value = const_value(arg, params)?;
        let ty = static_type(arg, &value);
        if !matches!(
            ty.as_str(),
            "text" | "varchar" | "bpchar" | "name" | "unknown"
        ) {
            return Err(Error::DatatypeMismatch(format!(
                "collations are not supported by type {}",
                display_type(&ty)
            )));
        }
        let collation = cc
            .collname
            .iter()
            .filter_map(|n| match n.node.as_ref()? {
                N::String(s) => Some(s.sval.clone()),
                _ => None,
            })
            .next_back()
            .unwrap_or_default();
        if !matches!(collation.as_str(), "C" | "POSIX" | "default" | "ucs_basic") {
            return Err(Error::Unsupported(format!("COLLATE \"{collation}\"")));
        }
        return Ok(value);
    }
    if let Some(N::FuncCall(f)) = node.node.as_ref() {
        if let Some(result) = correlated::eval_correlated(f, params) {
            return result;
        }
        if let Some(e) = function_absent_in_reference(f, params) {
            return Err(e);
        }
        if let Some(u) = func_name(f).and_then(|n| correlated::user_function(&n, f.args.len())) {
            if u.returns_set {
                return Err(Error::FeatureNotSupported(
                    "set-valued function called in context that cannot accept a set".into(),
                ));
            }
            let args = f
                .args
                .iter()
                .map(|a| const_value(a, params))
                .collect::<Result<Vec<_>>>()?;
            return match correlated::call_user_function(&u, &args)? {
                correlated::FnResult::Value(v) => Ok(v),
                correlated::FnResult::Rows(..) => Ok(Bson::Null),
            };
        }
        if let Some(name) =
            func_name(f).filter(|n| correlated::SEQUENCE_FUNCTIONS.contains(&n.as_str()))
        {
            let args = f
                .args
                .iter()
                .map(|a| const_value(a, params))
                .collect::<Result<Vec<_>>>()?;
            return correlated::call_sequence(&name, &args);
        }
        refuse_untyped_any_args(f)?;
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
                Bson::String(name) => {
                    match pgtypes::oid_of_name(&name).or_else(|| user_type_or_array_oid(&name)) {
                        Some(oid) => regtype_value(oid),
                        None => Bson::Null,
                    }
                }
                _ => Bson::Null,
            });
        }
        if let Some(name) = func_name(f) {
            let type_name = range_constructor_type(f).unwrap_or_default();
            if let (true, Some(text)) = (
                range::is_range_type(&type_name) || range::is_multirange_type(&type_name),
                sole_literal_string_arg(f),
            ) {
                return cast_value(Bson::String(text), &type_name);
            }
            // `int4multirange(int4range(1,5), ...)`: each argument is a range.
            if range::is_multirange_type(&type_name) {
                let args = f
                    .args
                    .iter()
                    .map(|a| const_value(a, params))
                    .collect::<Result<Vec<_>>>()?;
                return Ok(Bson::String(range::render_multirange(
                    &range::multirange_from_args(&args, &type_name)?,
                )));
            }
            // `int4range(1,5)` and friends: a constructor named for its type.
            if range::is_range_type(&type_name) {
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
                    &type_name,
                    literal_flags,
                )?)));
            }
            if let Some(result) = range_accessor_value(f, params) {
                return result.map(|(value, _)| value);
            }
            let name = overload_name(f, name);
            if let Some(out) = datetime_call(f, &name, params) {
                return out;
            }
            if let Some(args) = named_call_args(f, &name, params) {
                if let Some(result) = scalar::call(&name, &args?) {
                    return result;
                }
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
            if name == "regexp_replace" {
                let args = f
                    .args
                    .iter()
                    .map(|a| const_value(a, params))
                    .collect::<Result<Vec<_>>>()?;
                return regexp_replace(&args);
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
    // `x IS [NOT] NULL` over a value. A row is null when EVERY field is
    // null and not null when NO field is -- so `row(1, null)` answers false
    // to both (measured on PG 16).
    if let Some(N::NullTest(t)) = node.node.as_ref() {
        let arg = t
            .arg
            .as_ref()
            .ok_or_else(|| Error::Parse("IS NULL with no operand".into()))?;
        let value = const_value(arg, params)?;
        let (all_null, none_null) = match record_fields(&value) {
            Some(fields) => (
                fields.iter().all(|f| *f == Bson::Null),
                fields.iter().all(|f| *f != Bson::Null),
            ),
            None => (value == Bson::Null, value != Bson::Null),
        };
        return match NullTestType::try_from(t.nulltesttype) {
            Ok(NullTestType::IsNull) => Ok(Bson::Boolean(all_null)),
            Ok(NullTestType::IsNotNull) => Ok(Bson::Boolean(none_null)),
            _ => Err(Error::Unsupported("this IS [NOT] NULL form".into())),
        };
    }
    // `CASE` in both of PostgreSQL's forms. The walker already descended into
    // one (`walk_column_refs` has a CaseExpr arm, so a column inside a branch
    // resolved correctly); only the VALUE evaluator was missing, which is why
    // the refusal was a bare `CaseExpr is not supported yet`.
    //
    // Semantics measured against PostgreSQL 14.13:
    //
    // * The SEARCHED form `CASE WHEN c THEN v ... END` takes the first branch
    //   whose condition is TRUE. NULL is not true, so it falls through — the
    //   same three-valued rule a WHERE uses.
    // * The SIMPLE form `CASE x WHEN a THEN v ... END` compares `x` to each
    //   label with `=`. A NULL `x` matches NOTHING, not even `WHEN NULL`,
    //   because `NULL = NULL` is NULL.
    // * With no branch taken and no ELSE, the result is NULL.
    //
    // Branches are evaluated LAZILY: only the taken one, so `case when n <> 0
    // then 1/n else 0 end` does not divide by zero on the rows it guards.
    if let Some(N::CaseExpr(c)) = node.node.as_ref() {
        let subject = match c.arg.as_deref() {
            Some(a) => Some(const_value(a, params)?),
            None => None,
        };
        for w in &c.args {
            let Some(N::CaseWhen(cw)) = w.node.as_ref() else {
                return Err(Error::Unsupported("this CASE branch".into()));
            };
            let test = cw
                .expr
                .as_deref()
                .ok_or_else(|| Error::Parse("CASE WHEN with no condition".into()))?;
            let taken = match &subject {
                // Simple form: the parser leaves the comparison implicit.
                Some(subject) => {
                    let label = const_value(test, params)?;
                    *subject != Bson::Null
                        && label != Bson::Null
                        && eval_binary("=", subject.clone(), label)? == Bson::Boolean(true)
                }
                // Searched form: only TRUE takes the branch.
                None => const_value(test, params)? == Bson::Boolean(true),
            };
            if taken {
                let result = cw
                    .result
                    .as_deref()
                    .ok_or_else(|| Error::Parse("CASE WHEN with no result".into()))?;
                return const_value(result, params);
            }
        }
        return match c.defresult.as_deref() {
            Some(d) => const_value(d, params),
            None => Ok(Bson::Null),
        };
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
        if !array_rectangular(&items) {
            return Err(Error::ArraySubscript(
                "multidimensional arrays must have array expressions with matching dimensions"
                    .into(),
            ));
        }
        return Ok(Bson::Array(items));
    }
    // `a AND b`, `a OR b`, `NOT a` as a VALUE (`select 1 = 1 and 2 = 2`),
    // in SQL's three-valued logic: AND is false if any operand is false,
    // else NULL if any is NULL; OR is true if any is true, else NULL if any
    // is NULL; NOT NULL is NULL.
    if let Some(N::BoolExpr(b)) = node.node.as_ref() {
        let truth = |n: &pg_query::protobuf::Node| -> Result<Option<bool>> {
            let value = const_value(n, params)?;
            // An UNTYPED string literal is read as a boolean, so `not 'x'`
            // is the 22P02 of a bad boolean rather than a type mismatch.
            let value = match n.node.as_ref() {
                Some(N::AConst(c)) if matches!(c.val, Some(a_const::Val::Sval(_))) => {
                    cast_value(value, "bool")?
                }
                _ => value,
            };
            match value {
                Bson::Null => Ok(None),
                Bson::Boolean(v) => Ok(Some(v)),
                other => Err(Error::DatatypeMismatch(format!(
                    "argument of {} must be type boolean, not type {}",
                    match BoolExprType::try_from(b.boolop) {
                        Ok(BoolExprType::AndExpr) => "AND",
                        Ok(BoolExprType::OrExpr) => "OR",
                        _ => "NOT",
                    },
                    display_type(inferred_type(&other))
                ))),
            }
        };
        let out = match BoolExprType::try_from(b.boolop) {
            Ok(BoolExprType::AndExpr) => {
                let mut acc = Some(true);
                for a in &b.args {
                    match truth(a)? {
                        Some(false) => {
                            acc = Some(false);
                            break;
                        }
                        None => acc = None,
                        Some(true) => {}
                    }
                }
                acc
            }
            Ok(BoolExprType::OrExpr) => {
                let mut acc = Some(false);
                for a in &b.args {
                    match truth(a)? {
                        Some(true) => {
                            acc = Some(true);
                            break;
                        }
                        None => acc = None,
                        Some(false) => {}
                    }
                }
                acc
            }
            Ok(BoolExprType::NotExpr) => {
                b.args.first().map(truth).transpose()?.flatten().map(|v| !v)
            }
            _ => return Err(Error::Unsupported("this boolean operator".into())),
        };
        return Ok(out.map_or(Bson::Null, Bson::Boolean));
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
        if matches!(
            AExprKind::try_from(e.kind),
            Ok(AExprKind::AexprOpAny | AExprKind::AexprOpAll)
        ) {
            let is_any = AExprKind::try_from(e.kind) == Ok(AExprKind::AexprOpAny);
            let op = operator_name(e)?.to_string();
            let lhs = match e.lexpr.as_ref() {
                Some(l) => const_value(l, params)?,
                None => return Err(Error::Parse("ANY/ALL with no left operand".into())),
            };
            let rhs = const_value(
                e.rexpr
                    .as_ref()
                    .ok_or_else(|| Error::Parse("ANY/ALL with no array operand".into()))?,
                params,
            )?;
            return eval_scalar_array_const(&op, lhs, rhs, is_any);
        }
        // `LIKE` / `ILIKE` as a VALUE (`select a like 'a%'`), not just as a
        // WHERE predicate. Their own AExpr kind, so they never reached the
        // operator path.
        if matches!(
            AExprKind::try_from(e.kind),
            Ok(AExprKind::AexprLike | AExprKind::AexprIlike)
        ) {
            return eval_pattern_match_const(e, params);
        }
        // `a IS [NOT] DISTINCT FROM b`: equality in which NULL is a value --
        // two NULLs are not distinct, a NULL and a value are -- so it is
        // never NULL itself.
        if matches!(
            AExprKind::try_from(e.kind),
            Ok(AExprKind::AexprDistinct | AExprKind::AexprNotDistinct)
        ) {
            let not_distinct = AExprKind::try_from(e.kind) == Ok(AExprKind::AexprNotDistinct);
            let operand = |n: Option<&Box<pg_query::protobuf::Node>>| -> Result<Bson> {
                const_value(
                    n.ok_or_else(|| Error::Parse("DISTINCT FROM with a missing operand".into()))?,
                    params,
                )
            };
            let lhs = operand(e.lexpr.as_ref())?;
            let rhs = operand(e.rexpr.as_ref())?;
            let same = match (lhs == Bson::Null, rhs == Bson::Null) {
                (true, true) => true,
                (true, false) | (false, true) => false,
                (false, false) => {
                    let mut eq = e.clone();
                    eq.kind = AExprKind::AexprOp as i32;
                    eq.name = vec![string_node("=")];
                    matches!(
                        const_value(
                            &pg_query::protobuf::Node {
                                node: Some(N::AExpr(eq)),
                            },
                            params,
                        )?,
                        Bson::Boolean(true)
                    )
                }
            };
            return Ok(Bson::Boolean(same == not_distinct));
        }
        if AExprKind::try_from(e.kind) != Ok(AExprKind::AexprOp) {
            return Err(Error::Unsupported("this operator form".into()));
        }
        let op = operator_name(e)?.to_string();
        // The regex operators are ordinary AexprOp.
        if pattern_operator(&op).is_some() {
            return eval_pattern_match_const(e, params);
        }
        let rhs = const_value(
            e.rexpr
                .as_ref()
                .ok_or_else(|| Error::Parse("operator with no right operand".into()))?,
            params,
        )?;
        // A missing left operand is unary: `-3`, `+3`. A double is negated
        // outright rather than subtracted from zero, which is the difference
        // between `-(0.0::float8)` printing `-0` (PostgreSQL) and `0`.
        let lhs = match e.lexpr.as_ref() {
            Some(l) => const_value(l, params)?,
            None => match op.as_str() {
                "-" if matches!(rhs, Bson::Double(_)) => {
                    let Bson::Double(d) = rhs else { unreachable!() };
                    return Ok(Bson::Double(-d));
                }
                // A numeric is negated as TEXT so that `-'NaN'::numeric`
                // stays `NaN` and `-'Infinity'::numeric` is `-Infinity`
                // (measured), which `0 - x` cannot produce: the decimal
                // arithmetic has no special values.
                "-" if is_numeric(&rhs) => {
                    return negate_numeric_text(&numeric_text(&rhs).unwrap_or_default());
                }
                "-" if Interval::from_bson(&rhs).is_some() => {
                    let iv = Interval::from_bson(&rhs).expect("checked");
                    return Ok(Interval {
                        months: -iv.months,
                        days: -iv.days,
                        micros: -iv.micros,
                    }
                    .to_bson());
                }
                "-" => Bson::Int32(0),
                "+" => return Ok(rhs),
                "!!" if rhs == Bson::Null => return Ok(Bson::Null),
                "!!" => return fts::not_value(&rhs),
                _ => return Err(Error::Unsupported(format!("unary {op}"))),
            },
        };
        // Two records compare with rules that depend on whether each side is a
        // ROW CONSTRUCTOR or a composite VALUE (see record_compare). Only the
        // AST distinguishes them -- a bare `ROW(...)` is an `N::RowExpr`, while
        // `row(...)::t` / a bound composite / a stored composite is anything
        // else -- and only three-valued NULL logic applies when BOTH sides are
        // row constructors. `eval_binary` has no AST, so decide here.
        if matches!(op.as_str(), "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=") {
            if let (Some(a), Some(b)) = (record_fields(&lhs), record_fields(&rhs)) {
                let is_row_ctor = |n: Option<&pg_query::protobuf::Node>| {
                    matches!(n.and_then(|n| n.node.as_ref()), Some(N::RowExpr(_)))
                };
                let composite =
                    !(is_row_ctor(e.lexpr.as_deref()) && is_row_ctor(e.rexpr.as_deref()));
                return record_compare(&op, a, b, composite);
            }
        }
        // SQL/JSON path: `jsonb @? jsonpath` and `jsonb @@ jsonpath`, the
        // silent forms of jsonb_path_exists / jsonb_path_match.
        if op == "@?"
            || (op == "@@"
                && e.lexpr
                    .as_deref()
                    .is_some_and(|l| matches!(static_type(l, &lhs).as_str(), "jsonb" | "json")))
        {
            let f = if op == "@?" {
                "jsonb_path_exists"
            } else {
                "jsonb_path_match"
            };
            return jsonpath_call(
                f,
                &[lhs, rhs, Bson::String("{}".into()), Bson::Boolean(true)],
            );
        }
        // Full-text search: `@@` between a document and a query, and the
        // combining operators, told apart by the operands' STATIC types.
        if let Some(out) = fts_operator(e, &op, &lhs, &rhs) {
            return out;
        }
        // The hstore operators, like the json ones below, are told apart
        // from everything else by the left operand's STATIC type.
        if matches!(
            op.as_str(),
            "->" | "?" | "?|" | "?&" | "||" | "-" | "@>" | "<@"
        ) && static_hstore_operand(e.lexpr.as_deref(), &lhs)
        {
            if lhs == Bson::Null || rhs == Bson::Null {
                return Ok(Bson::Null);
            }
            // An UNKNOWN right operand (a bare literal, an untyped
            // parameter) takes the LEFT operand's type, as PostgreSQL's
            // operator resolution does -- so `h - 'a'` is `hstore - hstore`
            // and only `h - 'a'::text` deletes a key.
            let rhs_unknown = match e.rexpr.as_deref().and_then(|x| x.node.as_ref()) {
                Some(N::AConst(c)) => matches!(c.val.as_ref(), Some(a_const::Val::Sval(_))),
                Some(N::ParamRef(p)) => {
                    declared_param_type(usize::try_from(p.number).unwrap_or(0)).is_none()
                }
                _ => false,
            };
            let rhs_hstore = rhs_unknown || static_hstore_operand(e.rexpr.as_deref(), &rhs);
            return hstore_operator(&op, &lhs, &rhs, rhs_hstore);
        }
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
        // The array containment operators. Told apart from the json ones
        // above by the operands being arrays -- a json value is a string here,
        // so the two never both match.
        if matches!(op.as_str(), "@>" | "<@" | "&&")
            && (matches!(lhs, Bson::Array(_)) || matches!(rhs, Bson::Array(_)))
        {
            if lhs == Bson::Null || rhs == Bson::Null {
                return Ok(Bson::Null);
            }
            return Ok(Bson::Boolean(match op.as_str() {
                "@>" => arrays::contains(&lhs, &rhs),
                "<@" => arrays::contains(&rhs, &lhs),
                _ => arrays::overlaps(&lhs, &rhs),
            }));
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
                Some(a_const::Val::Fval(f)) => parse_numeric(&f.fval),
                Some(a_const::Val::Boolval(b)) => Ok(Bson::Boolean(b.boolval)),
                _ => Err(Error::Unsupported("this constant".into())),
            }
        }
        // `ROW(...)` and the bare `(a, b, ...)` parenthesised list build an
        // anonymous record; each field is any constant expression.
        // Each field's STATIC type rides with the value for the binary record
        // format: a bare string literal or bare `null` is `unknown` inside a
        // row (PostgreSQL never resolves it to text there), everything else
        // is what the expression says.
        Some(N::RowExpr(r)) => {
            let mut fields = Vec::with_capacity(r.args.len());
            let mut types = Vec::with_capacity(r.args.len());
            for a in &r.args {
                let v = const_value(a, params)?;
                let t = match a.node.as_ref() {
                    Some(N::AConst(c))
                        if c.isnull || matches!(c.val, Some(a_const::Val::Sval(_))) =>
                    {
                        "unknown".to_string()
                    }
                    _ => static_type(a, &v),
                };
                fields.push(v);
                types.push(t);
            }
            Ok(typed_record_value(fields, types))
        }
        // `current_timestamp` inside an expression (`current_timestamp::text`)
        // is `now()`; the bare-column form is handled by `plan_select_constant`.
        Some(N::SqlvalueFunction(svf))
            if pg_query::protobuf::SqlValueFunctionOp::try_from(svf.op)
                == Ok(pg_query::protobuf::SqlValueFunctionOp::SvfopCurrentTimestamp) =>
        {
            Ok(scalar::now_value())
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

/// Whether `field <op> value` needs the two-width numeric lowering: the column
/// is declared `numeric`, or the constant itself is too wide for Decimal128.
/// A numeric column can hold either stored form, and neither an MQL number
/// nor an MQL document comparison reaches across to the other.
fn needs_numeric_filter(def: &TableDef, field: &str, value: &Bson) -> bool {
    if is_wide_numeric(value) {
        return true;
    }
    let declared = def
        .columns
        .iter()
        .find(|c| c.field() == field || c.name == field)
        .map(|c| c.pg_type.as_str());
    matches!(declared, Some("numeric" | "decimal"))
        && matches!(value, Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_))
}

/// The declared type of the column stored under `field`.
fn field_type<'a>(def: &'a TableDef, field: &str) -> Option<&'a str> {
    def.columns
        .iter()
        .find(|c| c.field() == field || c.name == field)
        .map(|c| c.pg_type.as_str())
}

fn is_timestamp_field(def: &TableDef, field: &str) -> bool {
    matches!(field_type(def, field), Some("timestamp" | "timestamptz"))
}

/// A string literal compared with a column is PostgreSQL's UNKNOWN-typed
/// constant, and resolves to the COLUMN's type: `n > '5'` compares integers
/// and `t > '2026-03-01'` instants. Left a string, the filter compared across
/// BSON types and matched NOTHING -- a silent empty answer for every quoted
/// number, boolean, timestamp or interval in a WHERE (measured 2026-09-29:
/// 15 of 23 shapes). The types listed are the ones stored as something other
/// than their text; a text-stored type (date, time, uuid) compares correctly
/// as a string already and is left alone.
fn coerce_to_column(def: &TableDef, field: &str, value: Bson) -> Result<Bson> {
    let Bson::String(_) = &value else {
        return Ok(value);
    };
    match field_type(def, field) {
        Some(
            ty @ ("int2" | "int4" | "int8" | "numeric" | "float4" | "float8" | "bool" | "timestamp"
            | "timestamptz" | "interval" | "oid"),
        ) => cast_value(value, ty),
        _ => Ok(value),
    }
}

/// A comparison against a `timestamp` / `timestamptz` column, which stores a
/// millisecond `DateTime` plus a hidden companion holding any microsecond
/// REMAINDER (absent when it is zero). A plain MQL comparison of the
/// `DateTime` alone is wrong both ways: `t > '10:00:00.123'` must match a
/// stored `10:00:00.123456`, whose millisecond part is equal, and `t =
/// '...123456'` compares a value carrying the remainder. So each operator
/// compares the millisecond part and, where that is equal, the remainder.
/// `None` for a value that is not an instant (`infinity`, say), which keeps
/// the plain comparison.
fn timestamp_filter(field: &str, op: &str, value: &Bson) -> Option<Document> {
    let (date, us) = match value {
        Bson::DateTime(_) => (value.clone(), 0),
        Bson::Document(d) => match (d.get(COMPOSITE_DATE), d.get(COMPOSITE_US)) {
            (Some(date @ Bson::DateTime(_)), Some(us)) => (date.clone(), us.as_i32().unwrap_or(0)),
            _ => return None,
        },
        _ => return None,
    };
    let comp = companion_field(field);
    // The remainder of a stored row is its companion, or 0 when absent.
    let rem_eq = if us == 0 {
        doc! { &comp: { "$exists": false } }
    } else {
        doc! { &comp: us }
    };
    let rem_gt = if us == 0 {
        doc! { &comp: { "$exists": true } }
    } else {
        doc! { &comp: { "$gt": us } }
    };
    let rem_lt = if us == 0 {
        None
    } else {
        Some(doc! { "$or": [ { &comp: { "$exists": false } }, { &comp: { "$lt": us } } ] })
    };
    let at = |rem: Document| -> Document {
        let mut d = doc! { field: date.clone() };
        d.extend(rem);
        d
    };
    let eq = at(rem_eq.clone());
    Some(match op {
        "$eq" => eq,
        "$ne" => doc! { "$and": [
            doc! { "$nor": [eq] },
            doc! { field: { "$ne": Bson::Null } },
        ]},
        "$gt" => doc! { "$or": [ { field: { "$gt": date.clone() } }, at(rem_gt) ] },
        "$gte" => doc! { "$or": [ { field: { "$gt": date.clone() } }, at(rem_gt), eq ] },
        "$lt" => match rem_lt {
            None => doc! { field: { "$lt": date.clone() } },
            Some(rem) => doc! { "$or": [ { field: { "$lt": date.clone() } }, at(rem) ] },
        },
        "$lte" => {
            let mut arms = vec![Bson::Document(doc! { field: { "$lt": date.clone() } })];
            if let Some(rem) = rem_lt {
                arms.push(Bson::Document(at(rem)));
            }
            arms.push(Bson::Document(eq));
            doc! { "$or": arms }
        }
        _ => return None,
    })
}

/// `field <mql_op> value`, exact for a numeric column of either width, or the
/// plain MQL form for anything else.
fn scalar_filter(def: &TableDef, field: &str, mql_op: &str, value: Bson) -> Document {
    if is_timestamp_field(def, field) {
        if let Some(d) = timestamp_filter(field, mql_op, &value) {
            return d;
        }
    }
    if needs_numeric_filter(def, field, &value) {
        if let Some(d) = numeric::numeric_filter(field, mql_op, &value) {
            return d;
        }
    }
    match mql_op {
        "$eq" => doc! { field: value },
        "$ne" => doc! { "$and": [
            doc! { field: { "$ne": value } },
            doc! { field: { "$ne": Bson::Null } },
        ]},
        op => doc! { field: { op: value } },
    }
}

/// A SQL `LIKE` pattern as an anchored regular expression.
///
/// `%` is any run, `_` is one character, and everything else is literal — so
/// every regex metacharacter in the pattern must be escaped, or `a.c` would
/// match `abc`. The escape character (default `\\`, settable by `ESCAPE`)
/// makes the NEXT character literal, including `%` and `_`.
///
/// Anchored at both ends because SQL's `LIKE` matches the WHOLE string, unlike
/// `~`, which matches anywhere.
fn like_to_regex(pattern: &str, escape: Option<char>) -> Result<String> {
    let mut out = String::from("^");
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if Some(c) == escape {
            match chars.next() {
                Some(next) => out.push_str(&regex_escape(next)),
                // PostgreSQL 14.13: a pattern ending in the escape character
                // is an error, not a literal backslash.
                None => {
                    return Err(Error::InvalidText(
                        "LIKE pattern must not end with escape character".into(),
                    ))
                }
            }
            continue;
        }
        match c {
            '%' => out.push_str(".*"),
            '_' => out.push('.'),
            other => out.push_str(&regex_escape(other)),
        }
    }
    out.push('$');
    Ok(out)
}

/// One character, safe to drop into a regex as a literal.
fn regex_escape(c: char) -> String {
    if "\\^$.|?*+()[]{}".contains(c) {
        format!("\\{c}")
    } else {
        c.to_string()
    }
}

/// The pattern and escape character of a LIKE's right-hand side.
///
/// `LIKE p ESCAPE e` is folded by the parser into a `like_escape(p, e)` CALL,
/// not into a third operand — so a bare `LIKE p` has no such call and takes the
/// default backslash. Reading the call is what makes `ESCAPE` work at all;
/// without it the RHS evaluated as an unknown function.
///
/// `ESCAPE ''` disables escaping entirely (PostgreSQL 14.13), which is why the
/// escape is an `Option` rather than a char with a sentinel.
fn like_pattern_and_escape(
    rhs: &pg_query::protobuf::Node,
    params: &[Bson],
) -> Result<(Bson, Option<char>)> {
    if let Some(N::FuncCall(f)) = rhs.node.as_ref() {
        if func_name(f).as_deref() == Some("like_escape") && f.args.len() == 2 {
            let pattern = const_value(&f.args[0], params)?;
            let escape = match const_value(&f.args[1], params)? {
                Bson::String(e) => e.chars().next(),
                _ => Some('\\'),
            };
            return Ok((pattern, escape));
        }
    }
    Ok((const_value(rhs, params)?, Some('\\')))
}

/// `~~` / `!~~` / `~~*` / `!~~*` (LIKE and friends) and `~` / `!~` / `~*` /
/// `!~*` (regex) as (regex-is-negated, case-insensitive), or `None`.
fn pattern_operator(op: &str) -> Option<(bool, bool, bool)> {
    // (negated, case-insensitive, is_like)
    Some(match op {
        "~~" => (false, false, true),
        "!~~" => (true, false, true),
        "~~*" => (false, true, true),
        "!~~*" => (true, true, true),
        "~" => (false, false, false),
        "!~" => (true, false, false),
        "~*" => (false, true, false),
        "!~*" => (true, true, false),
        _ => return None,
    })
}

/// `LIKE` / `ILIKE` / `~` and their negations as a VALUE.
///
/// Three-valued, as PostgreSQL has it: a NULL on either side is NULL, not
/// FALSE -- so `NOT LIKE` over a NULL is NULL too, and a WHERE built on it
/// matches nothing rather than everything.
fn eval_pattern_match_const(e: &AExpr, params: &[Bson]) -> Result<Bson> {
    let op = operator_name(e)?.to_string();
    let (negated, insensitive, is_like) =
        pattern_operator(&op).ok_or_else(|| Error::Unsupported(format!("the {op} operator")))?;
    let subject = const_value(
        e.lexpr
            .as_deref()
            .ok_or_else(|| Error::Parse("pattern match with no subject".into()))?,
        params,
    )?;
    let rexpr = e
        .rexpr
        .as_deref()
        .ok_or_else(|| Error::Parse("pattern match with no pattern".into()))?;
    let (pattern, escape) = if is_like {
        like_pattern_and_escape(rexpr, params)?
    } else {
        (const_value(rexpr, params)?, None)
    };
    let (Bson::String(subject), Bson::String(pattern)) = (&subject, &pattern) else {
        return Ok(Bson::Null);
    };
    let source = if is_like {
        like_to_regex(pattern, escape)?
    } else {
        pattern.clone()
    };
    let re = regex::RegexBuilder::new(&source)
        .case_insensitive(insensitive)
        .build()
        .map_err(|err| Error::InvalidText(format!("invalid regular expression: {err}")))?;
    Ok(Bson::Boolean(re.is_match(subject) != negated))
}

/// Lower `LIKE` / `ILIKE` / `~` and their negations to an MQL `$regex`.
///
/// SQL's three-valued logic needs help on the NEGATED side. A NULL column
/// matches no regex, so the positive form is right for free — but MQL's `$not`
/// MATCHES a null or missing field, where PostgreSQL's `NOT LIKE` over NULL is
/// NULL and selects nothing. The first cut of this claimed otherwise in a
/// comment and returned the NULL row; the differential caught it. So the
/// negated form pairs `$not` with an explicit `$ne: null`, which is measured
/// behaviour rather than an assumption about MQL.
fn lower_pattern_match(e: &AExpr, def: &TableDef, params: &[Bson]) -> Result<Document> {
    let op = operator_name(e)?;
    let (negated, insensitive, is_like) =
        pattern_operator(op).ok_or_else(|| Error::Unsupported(format!("the {op} operator")))?;
    let field = column_field(e.lexpr.as_deref(), def)?;
    // PostgreSQL has no `~~` for a non-text left operand: `n LIKE 'x'` over an
    // integer is `42883 operator does not exist: integer ~~ unknown`. Lowering
    // it to a regex anyway matched NOTHING and returned no rows -- an empty
    // result where PostgreSQL raises, which is the silent-divergence class
    // this server refuses to ship. Measured on 14.13.
    if let Some(column) = def.columns.iter().find(|c| c.field() == field) {
        if !matches!(
            column.pg_type.as_str(),
            "text" | "varchar" | "bpchar" | "name" | "citext"
        ) {
            return Err(Error::UndefinedFunction(format!(
                "operator does not exist: {} {} unknown",
                display_type(&column.pg_type),
                op
            )));
        }
    }
    let rexpr = e
        .rexpr
        .as_deref()
        .ok_or_else(|| Error::Parse("pattern match with no pattern".into()))?;
    let (rhs, escape) = if is_like {
        like_pattern_and_escape(rexpr, params)?
    } else {
        (const_value(rexpr, params)?, None)
    };
    let Bson::String(pattern) = rhs else {
        // A NULL pattern is NULL for every row, which is no rows.
        return Ok(doc! { "__never__": Bson::Null, "$comment": "NULL pattern" });
    };
    let regex = if is_like {
        like_to_regex(&pattern, escape)?
    } else {
        pattern
    };
    let mut spec = doc! { "$regex": regex };
    if insensitive {
        spec.insert("$options", "i");
    }
    Ok(if negated {
        doc! { field: { "$not": spec, "$ne": Bson::Null } }
    } else {
        doc! { field: spec }
    })
}

fn lower_aexpr(e: &AExpr, def: &TableDef, params: &[Bson]) -> Result<Document> {
    // Named enum, never the wire integer. Written against the integers first,
    // this had `Op = 0` (it is 1, so every plain `=` was refused) and
    // `Between = 10` (it is 11, so BETWEEN silently ran the NOT BETWEEN arm and
    // returned the complement). Same mistake as the BoolExpr one below.
    match AExprKind::try_from(e.kind) {
        Ok(AExprKind::AexprIn) => return lower_in(e, def, params),
        Ok(AExprKind::AexprOpAny) => return lower_scalar_array(e, def, params, true),
        Ok(AExprKind::AexprOpAll) => return lower_scalar_array(e, def, params, false),
        Ok(AExprKind::AexprBetween | AExprKind::AexprNotBetween) => {
            return lower_between(e, def, params)
        }
        Ok(AExprKind::AexprOp) => {}
        // `LIKE` / `ILIKE` and their NOT forms arrive as their OWN kind, so
        // they never reached the operator path below -- the statement died
        // with `this operator form is not supported yet`.
        Ok(AExprKind::AexprLike | AExprKind::AexprIlike) => {
            return lower_pattern_match(e, def, params)
        }
        Ok(AExprKind::AexprDistinct | AExprKind::AexprNotDistinct) => {
            return lower_distinct(e, def, params)
        }
        _ => return Err(Error::Unsupported("this operator form".into())),
    }
    let op = operator_name(e)?;
    // The regex operators are ordinary AexprOp, so they land here rather than
    // in the arm above.
    if pattern_operator(op).is_some() {
        return lower_pattern_match(e, def, params);
    }

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
    // A SCALAR column compared to an ARRAY with no ANY/ALL is an operator
    // PostgreSQL does not have (`text = text[]` is 42883). An array COLUMN
    // compared to an array is a real element-wise operator and is left alone.
    if let Bson::Array(_) = &value {
        let coltype = def
            .columns
            .iter()
            .find(|c| c.name == col)
            .map(|c| c.pg_type.as_str());
        if !coltype.map(|t| t.ends_with("[]")).unwrap_or(false) {
            return Err(Error::UndefinedFunction(format!(
                "operator does not exist: {} {op} {}",
                coltype.unwrap_or("text"),
                inferred_type(&value)
            )));
        }
    }
    // Comparison against a `char(n)` column IGNORES TRAILING BLANKS: a
    // `char(4)` holding `ab` equals both `'ab'` and `'ab  '` (PostgreSQL
    // 14.24). The stored value is unpadded -- padding happens on output --
    // so the literal is stripped to match it, rather than padded to a width
    // the stored side does not carry.
    let value = match (&value, def.columns.iter().find(|c| c.name == col)) {
        (Bson::String(text), Some(c)) if c.pg_type == "bpchar" => {
            Bson::String(text.trim_end_matches(' ').to_string())
        }
        _ => value,
    };
    // A regtype / regclass value filters by its OID -- the stored column is
    // a number.
    let value = match regtype_oid(&value).or_else(|| regclass_oid(&value)) {
        Some(oid) => Bson::Int64(oid),
        None => value,
    };
    let value = coerce_to_column(def, &field, value)?;

    // `=` and the range operators are already NULL-correct: MQL brackets by
    // type, so a null column value matches none of them -- which is what
    // three-valued logic gives.
    //
    // `<>` is the exception, and it is a WRONG-ROWS bug, not a nicety.
    // MQL's `$ne` matches a missing-or-null field, so `n <> 1` returned the
    // row whose `n` is NULL. SQL says `NULL <> 1` is NULL, so PostgreSQL
    // excludes it (probed 14). The explicit not-null guard restores that.
    let mongo_op = op_to_mql(op).ok_or_else(|| Error::Unsupported(format!("operator {op}")))?;
    Ok(scalar_filter(def, &field, mongo_op, value))
}

/// A WHERE as a filter, or -- when it does not lower -- an empty filter and
/// a RESIDUAL evaluated per row. Only `Unsupported` falls back: an undefined
/// column or a bad type is a real error and must stay one, or a typo would
/// become a silent full scan.
fn lower_where_or_residual(
    w: &pg_query::protobuf::Node,
    def: &TableDef,
    params: &[Bson],
) -> Result<(Document, Option<ColumnExpr>)> {
    match lower_where(w, def, params) {
        Ok(f) => Ok((f, None)),
        Err(Error::Unsupported(_)) => {
            let fields: Vec<RowField> = def
                .columns
                .iter()
                .map(|c| (c.name.clone(), c.field(), c.pg_type.clone()))
                .collect();
            let mut sample = Document::new();
            for c in &def.columns {
                sample.insert(c.field(), sample_value_for_type(&c.pg_type));
            }
            Ok((
                Document::new(),
                Some(row_column_expr(w, &fields, params, &sample)?),
            ))
        }
        Err(e) => Err(e),
    }
}

/// `col IS [NOT] DISTINCT FROM <constant>`. Anything else -- a column on
/// the right, an expression on the left -- is refused here and evaluated per
/// row as a residual by the caller.
fn lower_distinct(e: &AExpr, def: &TableDef, params: &[Bson]) -> Result<Document> {
    if !matches!(
        e.lexpr.as_deref().and_then(|l| l.node.as_ref()),
        Some(N::ColumnRef(_))
    ) {
        return Err(Error::Unsupported("this DISTINCT FROM".into()));
    }
    let rhs = e
        .rexpr
        .as_ref()
        .ok_or_else(|| Error::Parse("DISTINCT FROM with no right operand".into()))?;
    let value =
        const_value(rhs, params).map_err(|_| Error::Unsupported("this DISTINCT FROM".into()))?;
    let not_distinct = AExprKind::try_from(e.kind) == Ok(AExprKind::AexprNotDistinct);
    let col = match e.lexpr.as_deref().and_then(|l| l.node.as_ref()) {
        Some(N::ColumnRef(c)) => {
            column_ref_name(c).ok_or_else(|| Error::Unsupported("this column reference".into()))?
        }
        _ => unreachable!("checked above"),
    };
    let field = def
        .field_of(&col)
        .ok_or_else(|| Error::UndefinedColumn(col.clone()))?;
    if value == Bson::Null {
        return Ok(if not_distinct {
            doc! { field: Bson::Null }
        } else {
            doc! { field: { "$ne": Bson::Null } }
        });
    }
    let mut eq = e.clone();
    eq.kind = AExprKind::AexprOp as i32;
    eq.name = vec![string_node("=")];
    let equal = lower_aexpr(&eq, def, params)?;
    Ok(if not_distinct {
        equal
    } else {
        doc! { "$nor": [equal] }
    })
}

/// `x IN (a, b)` / `x NOT IN (a, b)`.
///
/// PostgreSQL's three-valued logic drives both edge cases here, probed on 14:
/// `n NOT IN (1)` does NOT return a row whose `n` is NULL (because `NULL <> 1`
/// is NULL, not true), and `n NOT IN (1, NULL)` returns nothing at all. MQL's
/// `$nin` would match a null on both counts, so the guard is explicit.
/// PostgreSQL coerces an UNKNOWN-typed operand of `ANY`/`ALL` to the array
/// type of the other side. psycopg sends a bare `= ANY(%s)` array parameter
/// without a type, so it arrives as the array literal TEXT `{a,b}`; parse it
/// into a real array using the scalar side's element type. A non-`{...}` value
/// (a properly typed array, or a genuine scalar) is returned unchanged.
fn coerce_any_array(rhs: Bson, element_type: &str) -> Bson {
    if let Bson::String(s) = &rhs {
        if s.starts_with('{') {
            if let Ok(arr) = parse_array(s, element_type) {
                return arr;
            }
        }
    }
    rhs
}

/// The MQL comparison operator for a SQL scalar comparison operator.
fn op_to_mql(op: &str) -> Option<&'static str> {
    Some(match op {
        "=" => "$eq",
        "<>" | "!=" => "$ne",
        "<" => "$lt",
        "<=" => "$lte",
        ">" => "$gt",
        ">=" => "$gte",
        _ => return None,
    })
}

/// `scalar <op> ANY(array)` / `scalar <op> ALL(array)` as a constant.
///
/// Three-valued, matching PostgreSQL: a NULL scalar is NULL; `ANY` is TRUE on
/// the first match, else NULL if any element (or comparison) was NULL, else
/// FALSE (an empty array is FALSE); `ALL` is FALSE on the first mismatch, else
/// NULL if any element was NULL, else TRUE (an empty array is TRUE).
fn eval_scalar_array_const(op: &str, lhs: Bson, rhs: Bson, is_any: bool) -> Result<Bson> {
    if lhs == Bson::Null {
        // Over an EMPTY array the answer does not depend on the left side:
        // `NULL > ALL ('{}')` is TRUE and `NULL = ANY ('{}')` FALSE
        // (PostgreSQL 14.13), which is how a correlated `x > ALL (subquery)`
        // with no rows keeps a row whose `x` is NULL.
        if matches!(&rhs, Bson::Array(v) if v.is_empty()) {
            return Ok(Bson::Boolean(!is_any));
        }
        return Ok(Bson::Null);
    }
    let rhs = coerce_any_array(rhs, inferred_type(&lhs));
    let elems = match rhs {
        Bson::Array(v) => v,
        Bson::Null => return Ok(Bson::Null),
        other => {
            return Err(Error::UndefinedFunction(format!(
                "operator does not exist: {} {op} {}",
                inferred_type(&lhs),
                inferred_type(&other)
            )))
        }
    };
    let mut any_null = false;
    for el in elems {
        if el == Bson::Null {
            any_null = true;
            continue;
        }
        match eval_binary(op, lhs.clone(), el)? {
            Bson::Boolean(true) if is_any => return Ok(Bson::Boolean(true)),
            Bson::Boolean(false) if !is_any => return Ok(Bson::Boolean(false)),
            Bson::Null => any_null = true,
            _ => {}
        }
    }
    Ok(if any_null {
        Bson::Null
    } else {
        Bson::Boolean(!is_any)
    })
}

/// `col <op> ANY(array)` / `col <op> ALL(array)` as a WHERE filter.
///
/// NULL-correct for row filtering: a NULL array element can never make `ANY`
/// true, and makes `ALL` unsatisfiable; a NULL column value satisfies none of
/// these scalar comparisons, so a `$ne`-based clause carries an explicit
/// not-null guard (MQL `$ne` would otherwise match a missing/null field).
fn lower_scalar_array(
    e: &AExpr,
    def: &TableDef,
    params: &[Bson],
    is_any: bool,
) -> Result<Document> {
    let field = column_field(e.lexpr.as_deref(), def)?;
    let op = operator_name(e)?.to_string();
    let mql = op_to_mql(&op).ok_or_else(|| Error::Unsupported(format!("{op} ANY/ALL")))?;
    let rhs = const_value(
        e.rexpr
            .as_ref()
            .ok_or_else(|| Error::Parse("ANY/ALL with no array operand".into()))?,
        params,
    )?;
    let elem_type = def
        .columns
        .iter()
        .find(|c| c.name == field)
        .map(|c| c.pg_type.as_str())
        .unwrap_or("text");
    let rhs = coerce_any_array(rhs, elem_type);
    let elems = match rhs {
        Bson::Array(v) => v,
        // A NULL array (a genuine NULL operand, or an unbound parameter at
        // DESCRIBE time before Bind) matches nothing: `x = ANY(NULL)` is NULL.
        Bson::Null => return Ok(match_nothing()),
        _ => return Err(Error::Unsupported("this ANY/ALL operand".into())),
    };
    let mut nonnull = Vec::new();
    let mut saw_null = false;
    for el in elems {
        let el = reg_oid_operand(el);
        if el == Bson::Null {
            saw_null = true;
        } else {
            nonnull.push(el);
        }
    }
    if is_any {
        // ANY: a NULL element cannot help; an empty (or all-NULL) array matches
        // nothing.
        if nonnull.is_empty() {
            return Ok(match_nothing());
        }
        let numeric = nonnull.iter().any(|v| needs_numeric_filter(def, &field, v));
        if op == "=" && !numeric {
            // Index-friendly and NULL-correct: `$in` excludes a NULL column.
            return Ok(doc! { &field: { "$in": nonnull } });
        }
        let clauses: Vec<Document> = nonnull
            .into_iter()
            .map(|v| scalar_filter(def, &field, mql, v))
            .collect();
        if mql == "$ne" {
            // Each `$ne` arm already carries its own not-null guard.
            return Ok(doc! { "$or": clauses });
        }
        return Ok(doc! { "$and": [
            doc! { "$or": clauses },
            doc! { &field: { "$ne": Bson::Null } },
        ]});
    }
    // ALL: a NULL element makes it unsatisfiable; an empty array is vacuously
    // true (every row, including a NULL column).
    if saw_null {
        return Ok(match_nothing());
    }
    if nonnull.is_empty() {
        return Ok(Document::new());
    }
    let numeric = nonnull.iter().any(|v| needs_numeric_filter(def, &field, v));
    if op == "<>" && !numeric {
        return Ok(doc! { "$and": [
            doc! { &field: { "$nin": nonnull } },
            doc! { &field: { "$ne": Bson::Null } },
        ]});
    }
    let mut arms: Vec<Document> = nonnull
        .into_iter()
        .map(|v| scalar_filter(def, &field, mql, v))
        .collect();
    arms.push(doc! { &field: { "$ne": Bson::Null } });
    Ok(doc! { "$and": arms })
}

/// A `regtype` / `regclass` operand, reduced to the OID a stored column holds.
///
/// The scalar comparison path already does this (see `lower_scalar`), because
/// a regclass VALUE is a one-field document (`{__regclass_oid: N}`) while the
/// column it is compared against is a plain number. The list paths did not,
/// so `conrelid IN ('t'::regclass, 'u'::regclass)` compared documents against
/// numbers, matched nothing, and returned ZERO ROWS with no error -- while the
/// same predicate written with `OR` returned the right ones. That is the
/// dominant shape in catalog reflection (SQLAlchemy and pgjdbc both emit
/// `WHERE <oid col> IN (...)` / `= ANY(...)`), so it read as "this server has
/// no constraints/columns" rather than as a bug.
fn reg_oid_operand(v: Bson) -> Bson {
    match regtype_oid(&v).or_else(|| regclass_oid(&v)) {
        Some(oid) => Bson::Int64(oid),
        None => v,
    }
}

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
        let v = coerce_to_column(def, &field, reg_oid_operand(const_value(item, params)?))?;
        if v == Bson::Null {
            saw_null = true;
        } else {
            values.push(v);
        }
    }
    // A timestamp compares through its companion too, so it takes the
    // per-value arms the numeric case already builds.
    let numeric = is_timestamp_field(def, &field)
        || values.iter().any(|v| needs_numeric_filter(def, &field, v));
    if negated {
        if saw_null {
            // `NOT IN` over a list containing NULL is never true.
            return Ok(match_nothing());
        }
        if numeric {
            let arms: Vec<Document> = values
                .into_iter()
                .map(|v| scalar_filter(def, &field, "$ne", v))
                .collect();
            return Ok(doc! { "$and": arms });
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
    if numeric {
        let arms: Vec<Document> = values
            .into_iter()
            .map(|v| scalar_filter(def, &field, "$eq", v))
            .collect();
        return Ok(doc! { "$or": arms });
    }
    Ok(doc! { field: { "$in": values } })
}

/// `x BETWEEN a AND b` / `x NOT BETWEEN a AND b`.
fn lower_between(e: &AExpr, def: &TableDef, params: &[Bson]) -> Result<Document> {
    let field = column_field(e.lexpr.as_deref(), def)?;
    let bounds = match e.rexpr.as_ref().and_then(|r| r.node.as_ref()) {
        Some(N::List(l)) if l.items.len() == 2 => &l.items,
        _ => return Err(Error::Unsupported("this BETWEEN form".into())),
    };
    let lo = coerce_to_column(def, &field, const_value(&bounds[0], params)?)?;
    let hi = coerce_to_column(def, &field, const_value(&bounds[1], params)?)?;
    if lo == Bson::Null || hi == Bson::Null {
        return Ok(match_nothing());
    }
    // Inclusive both ends. A NULL column value matches neither bound in MQL,
    // which is what PostgreSQL's three-valued logic gives too.
    if AExprKind::try_from(e.kind) == Ok(AExprKind::AexprNotBetween) {
        return Ok(doc! {
            "$and": [
                doc! { "$or": [
                    scalar_filter(def, &field, "$lt", lo),
                    scalar_filter(def, &field, "$gt", hi),
                ]},
                doc! { &field: { "$ne": Bson::Null } },
            ]
        });
    }
    if is_timestamp_field(def, &field)
        || needs_numeric_filter(def, &field, &lo)
        || needs_numeric_filter(def, &field, &hi)
    {
        return Ok(doc! { "$and": [
            scalar_filter(def, &field, "$gte", lo),
            scalar_filter(def, &field, "$lte", hi),
        ]});
    }
    Ok(doc! { field: { "$gte": lo, "$lte": hi } })
}

#[cfg(test)]
mod tests;
