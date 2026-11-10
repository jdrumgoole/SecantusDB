//! PL/pgSQL FUNCTIONS: an interpreter over PostgreSQL's own parse of the body.
//!
//! The body is parsed by libpg_query's PL/pgSQL grammar (`parse_plpgsql`),
//! which hands back PostgreSQL's statement tree -- blocks, assignments,
//! `IF` / `CASE` / loops, `RETURN`, `RAISE`, `PERFORM`, SQL with `INTO`,
//! exception handlers -- with every EXPRESSION left as the SQL text it was
//! written as. So nothing here parses PL/pgSQL, and nothing here evaluates an
//! expression: each one is run as SQL through the host (the wire layer's own
//! planner and executor), with the function's variables substituted in as
//! typed parameters. An expression therefore means exactly what the same text
//! means anywhere else in the server.
//!
//! Variables are substituted TOKEN by token with PostgreSQL's own scanner
//! (`pg_query::scan`), which is what PL/pgSQL itself does with its parser
//! hooks: an identifier naming a variable, not called as a function and not
//! the second half of a qualified name, becomes `$n::<type>`; `rec.field`
//! becomes the field's value. A column that shares a variable's name is read
//! as the variable -- PostgreSQL would raise an ambiguity there instead.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use bson::Bson;
use serde_json::Value;

/// A PL/pgSQL error, carried as PostgreSQL reports one.
#[derive(Debug, Clone)]
pub struct PlError {
    pub sqlstate: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    /// The CONTEXT stack, innermost first, as PostgreSQL prints it (boxed:
    /// the error travels in every `Result` here and is kept small).
    pub context: Option<Box<str>>,
    /// Raised by a SQL statement the body ran (through the host), not by
    /// the interpreter itself: only such an error gets a `SQL statement`
    /// / `SQL expression` frame.
    pub from_sql: bool,
    /// This function's `SQL ...` frame is on the stack.
    sql_framed: bool,
    /// This function's `PL/pgSQL function f() line N at X` frame is on
    /// the stack: an enclosing statement adds none.
    framed: bool,
}

impl PlError {
    pub fn new(sqlstate: &str, message: impl Into<String>) -> Self {
        PlError {
            sqlstate: sqlstate.into(),
            message: message.into(),
            detail: None,
            hint: None,
            context: None,
            from_sql: false,
            sql_framed: false,
            framed: false,
        }
    }

    /// Add an outer frame to the CONTEXT stack.
    fn push_context(&mut self, frame: String) {
        self.context = Some(
            match self.context.take() {
                Some(inner) => format!("{inner}\n{frame}"),
                None => frame,
            }
            .into_boxed_str(),
        );
    }

    /// The `SQL statement "..."` style frame for a statement or expression
    /// the body ran, once.
    fn with_sql_frame(mut self, frame: impl FnOnce() -> Option<String>) -> Self {
        if self.from_sql && !self.sql_framed && !self.framed {
            if let Some(f) = frame() {
                self.push_context(f);
            }
            self.sql_framed = true;
        }
        self
    }

    fn unsupported(what: &str) -> Self {
        PlError::new(
            "0A000",
            format!("{what} in a PL/pgSQL function is not supported yet"),
        )
    }
}

/// A query's result: `(name, type)` per column, and the rows.
pub struct QueryOut {
    pub columns: Vec<(String, String)>,
    pub rows: Vec<Vec<Bson>>,
    /// Each column's type modifier (`exprTypmod`, -1 for none), where the
    /// result's description carries one; empty when it is not known.
    pub typmods: Vec<i32>,
    /// The domain each column is of, where the source keeps it (a
    /// portal's row type, for `FETCH`); empty when it is not known.
    pub domains: Vec<Option<String>>,
}

/// What the interpreter asks of the server.
mod block_scope;
mod open_cursor;
mod percent_type;
pub use percent_type::percent_types;

pub trait Host {
    /// `OPEN cur FOR query`: make a portal over the query's rows, named
    /// `name` or (`None`) `<unnamed portal N>`; answers the name.
    fn open_cursor(
        &self,
        name: Option<&str>,
        statement: &str,
        sql: &str,
        params: &[Bson],
        types: &[String],
        scroll: bool,
    ) -> Result<String, PlError> {
        let _ = (name, statement, sql, params, types, scroll);
        Err(PlError::unsupported("OPEN"))
    }
    /// Run a statement that yields rows, with `params` typed by `types`.
    fn query(&self, sql: &str, params: &[Bson], types: &[String]) -> Result<QueryOut, PlError>;
    /// Run a statement for its effect; answers the rows it affected.
    fn execute(&self, sql: &str, params: &[Bson], types: &[String]) -> Result<u64, PlError>;
    /// A `RAISE` below ERROR: `(severity, sqlstate, message)`.
    fn notice(&self, severity: &str, sqlstate: &str, message: String);
    /// Run an INSERT / UPDATE / DELETE and answer its RETURNING rows
    /// (`... RETURNING ... INTO`).
    /// `CALL p(...)` from a body: each OUTPUT parameter as `(argument
    /// position, parameter name)`, and their values in that order.
    #[allow(clippy::type_complexity)]
    fn call(
        &self,
        sql: &str,
        params: &[Bson],
        types: &[String],
    ) -> Result<(Vec<(usize, String)>, Vec<Bson>), PlError> {
        let _ = (sql, params, types);
        Err(PlError::unsupported("CALL"))
    }
    /// `exec_assign_value`'s cast of an assigned value into a record field
    /// of type `ty`, when `ty` is a domain (`None` when it is not one).
    /// `sql` is the assigned expression as `SELECT <expr>`, bound over
    /// parameters of `types`. A value already of the domain (`exprType`)
    /// is no coercion and is kept as it is; any other is cast to the
    /// domain's base type and checked against its constraints (RP-670).
    fn coerce_to_domain(
        &self,
        ty: &str,
        sql: &str,
        types: &[String],
        value: Bson,
    ) -> Option<Result<Bson, PlError>> {
        let _ = (ty, sql, types, value);
        None
    }
    /// The base type and modifier an assignment to a variable of declared
    /// type `ty` fits its value to by the assignment rule, when that is not
    /// what an explicit cast does: `ty` is (a domain over) `char(n)`,
    /// `varchar(n)`, `bit(n)` or `varbit(n)`, and the expression `sql`
    /// over parameters of `types` is not already of the domain. `None`
    /// otherwise (RP-687).
    fn assignment_length(&self, ty: &str, sql: &str, types: &[String]) -> Option<(String, i32)> {
        let _ = (ty, sql, types);
        None
    }
    /// `value`, of type `base`, fitted to modifier `typmod` by the
    /// assignment rule: an overlong `char(n)` / `varchar(n)` value is
    /// 22001 unless the excess is blanks, a `bit(n)` of another length
    /// 22026 (`exec_cast_value`, RP-687).
    fn fit_assigned(&self, value: Bson, base: &str, typmod: i32) -> Result<Bson, PlError> {
        let _ = (base, typmod);
        Ok(value)
    }
    /// The type of the one column the query `sql` (over parameters of
    /// `types`) returns, as a Describe of it answers, without running it;
    /// `None` when it does not describe as one column. PL/pgSQL decides
    /// an assignment's coercion from that type (`exec_cast_value`, RP-712).
    fn expression_type(&self, sql: &str, types: &[String]) -> Option<String> {
        let _ = (sql, types);
        None
    }
    /// What a DECLARE's `words%TYPE` names when no variable of the
    /// function does: a type, or a relation's column, as a declared type's
    /// text (`secantus_pgplan::plpgsql_percent_type_outer`, RP-818).
    fn percent_type(&self, words: &[String]) -> Option<String> {
        let _ = words;
        None
    }
    /// The domain each column of the query `sql` (over parameters of
    /// `types`) is of, in order; empty when not known. A column of the
    /// domain its `INTO` variable is of moves into it with no coercion
    /// (RP-694).
    fn output_domains(&self, sql: &str, types: &[String]) -> Vec<Option<String>> {
        let _ = (sql, types);
        Vec::new()
    }
    /// Enter a block with EXCEPTION handlers: a subtransaction, so a caught
    /// error undoes the block's writes. Answers a token for `subtxn_end`.
    fn subtxn_begin(&self) -> Result<u64, PlError> {
        Ok(0)
    }
    /// Leave that subtransaction: keep its writes, or (`rollback`) undo them.
    fn subtxn_end(&self, token: u64, rollback: bool) -> Result<(), PlError> {
        let _ = (token, rollback);
        Ok(())
    }
    /// `COMMIT` / `ROLLBACK` [`AND CHAIN`] in a procedure or DO block: end the
    /// session's transaction and start the next. Only a `CALL` / `DO` run
    /// outside a transaction block may; anywhere else it is 2D000.
    fn end_transaction(&self, commit: bool, chain: bool) -> Result<(), PlError> {
        let _ = (commit, chain);
        Err(PlError::new("2D000", "invalid transaction termination"))
    }
    fn returning(&self, sql: &str, params: &[Bson], types: &[String]) -> Result<QueryOut, PlError> {
        let _ = (sql, params, types);
        Err(PlError::unsupported(
            "INSERT / UPDATE / DELETE ... RETURNING INTO",
        ))
    }
    /// `FETCH` / `MOVE` on the portal `name`: one step as a SQL `FETCH`
    /// takes it, answering how many rows it moved over and, for a FETCH,
    /// the row the cursor then sits on (none past either end).
    fn fetch_cursor(
        &self,
        name: &str,
        direction: secantus_pgplan::FetchDirection,
        count: i64,
    ) -> Result<(u64, QueryOut), PlError> {
        let _ = (name, direction, count);
        Err(PlError::unsupported("FETCH"))
    }
    /// `CLOSE` the portal `name`.
    fn close_cursor(&self, name: &str) -> Result<(), PlError> {
        let _ = name;
        Err(PlError::unsupported("CLOSE"))
    }
    /// Whether the session has a portal named `name` (`SPI_cursor_find`).
    fn cursor_exists(&self, name: &str) -> bool {
        let _ = name;
        false
    }
}

/// A record value: its columns (name, type) and their values, or NULL.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Record {
    pub columns: Vec<(String, String)>,
    pub values: Vec<Bson>,
}

impl Record {
    fn get(&self, field: &str) -> Option<(&Bson, &str)> {
        let i = self.columns.iter().position(|(n, _)| n == field)?;
        Some((&self.values[i], &self.columns[i].1))
    }
}

/// A trigger invocation's special variables.
/// `TriggerData::level` for an event trigger: `op` is the command tag
/// (`TG_TAG`) and `when` the event (`TG_EVENT`).
pub const EVENT_LEVEL: &str = "EVENT";

#[derive(Debug, Clone, Default)]

pub struct TriggerData {
    pub new: Option<Record>,
    pub old: Option<Record>,
    pub op: String,
    pub name: String,
    pub table: String,
    pub when: String,
    pub level: String,
    /// `TG_ARGV`: the literal arguments CREATE TRIGGER named.
    pub args: Vec<String>,
    /// The table's columns and types, for a statement-level trigger, whose
    /// NEW and OLD are both NULL records of the table's row type.
    pub row_type: Vec<(String, String)>,
}

/// One call.
pub struct Invocation<'a> {
    pub args: &'a [Bson],
    /// The parameters' declared types, from the catalog. The parsed function
    /// loses an ARRAY parameter's brackets (`int4[]` reads as `int4`), so
    /// these win over what the parse says.
    pub arg_types: &'a [String],
    pub trigger: Option<TriggerData>,
    pub returns_set: bool,
    /// The OUT / INOUT parameters' names, in order: a non-set function
    /// returns their values when control reaches its end (or a bare RETURN)
    /// -- the one value, or a record of several.
    pub out_params: &'a [String],
    /// A procedure: reaching the end without RETURN is how it finishes.
    pub procedure: bool,
    /// A procedure or DO block, whose body may COMMIT / ROLLBACK; a
    /// function's (or trigger's) may not.
    pub nonatomic: bool,
}

/// What a call produced.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// A scalar function's value.
    Value(Bson),
    /// A trigger function's RETURN NEW / OLD (`None`: RETURN NULL).
    Record(Option<Record>),
    /// A set-returning function's rows (`RETURN NEXT` / `RETURN QUERY`).
    Rows(Vec<Vec<Bson>>),
}

enum Datum {
    Var {
        name: String,
        ty: String,
        value: Bson,
    },
    Rec {
        name: String,
        rec: Option<Record>,
    },
    Row {
        fields: Vec<usize>,
    },
    RecField {
        field: String,
        parent: usize,
    },
}

/// Control flow out of a statement list.
enum Flow {
    Next,
    Exit(Option<String>),
    Continue(Option<String>),
    Return(Outcome),
}

/// An expression's described type, by its bound SQL and parameter types.
type ExpressionTypes = HashMap<(String, Vec<String>), Option<String>>;

struct Interp<'a> {
    datums: Vec<Datum>,
    /// The datum of each argument, in declared order, for `$n`.
    arg_slots: Vec<usize>,
    /// The function's name: the label of the block its parameters are
    /// declared in, so `fname.p` names the parameter `p`. `None` for a DO
    /// block.
    fn_name: Option<String>,
    host: &'a dyn Host,
    trigger: Option<TriggerData>,
    returns_set: bool,
    out_params: Vec<String>,
    procedure: bool,
    nonatomic: bool,
    /// Blocks with EXCEPTION handlers entered and not left: a COMMIT inside
    /// one is refused (a subtransaction is active).
    subtxn_depth: usize,
    set_rows: Vec<Vec<Bson>>,
    row_count: u64,
    found_no: Option<usize>,
    /// The datums of the function's own level end here: the parameters,
    /// FOUND, and a trigger's NEW and OLD, which libpg_query's compile makes
    /// after FOUND. `None` without a FOUND.
    function_level: Option<usize>,
    /// The error a handler is running for: `RAISE;` re-raises it.
    handling: Option<PlError>,
    /// Bound cursors' queries, by datum number (`open_cursor`).
    cursor_exprs: HashMap<usize, String>,
    /// A bound cursor's declared options (`SCROLL` / `NO SCROLL`), by datum.
    cursor_options: HashMap<usize, u64>,
    /// A bound cursor's argument row (`c CURSOR (k int) FOR ...`), by datum:
    /// the row of the variables its OPEN assigns (`cursor_explicit_argrow`).
    cursor_argrows: HashMap<usize, usize>,
    /// How CONTEXT names this function: `f(integer)`, or
    /// `inline_code_block` for a DO block.
    label: String,
    /// The text of the EXECUTE statement being run, for its CONTEXT frame.
    dyn_text: Option<String>,
    /// Each block's namespace level, by the `__scope` number on the block.
    scopes: Vec<block_scope::Scope>,
    /// Each variable's declared default (`default_val`), by datum number.
    defaults: HashMap<usize, Value>,
    /// The levels of the blocks and labelled FOR loops being run, outermost
    /// first: what a `label.name` is looked up in.
    active: Vec<block_scope::Scope>,
    /// The type each assigned expression is of (`SELECT <expr>` over its
    /// parameters' types), as [`Host::expression_type`] described it:
    /// PL/pgSQL keeps a cast per source and target type for the body's
    /// life (`get_cast_hashentry`), so a loop's assignment describes once.
    expression_types: std::cell::RefCell<ExpressionTypes>,
}

impl Interp<'_> {
    /// The OUT parameters' values as the function's result: the one value,
    /// or a record of several (named, so `(f()).b` and `select * from f()`
    /// read them). `None` for a function without OUT parameters.
    fn out_value(&self) -> Option<Bson> {
        if self.out_params.is_empty() || self.returns_set {
            return None;
        }
        let value_of = |name: &str| -> Bson {
            self.datums
                .iter()
                .find_map(|d| match d {
                    Datum::Var { name: n, value, .. } if n == name => Some(value.clone()),
                    _ => None,
                })
                .unwrap_or(Bson::Null)
        };
        if let [one] = self.out_params.as_slice() {
            return Some(value_of(one));
        }
        let mut record = bson::Document::new();
        record.insert(
            secantus_pgplan::RECORD_KEY,
            self.out_params
                .iter()
                .map(|n| value_of(n))
                .collect::<Vec<_>>(),
        );
        record.insert(
            secantus_pgplan::RECORD_NAMES_KEY,
            self.out_params
                .iter()
                .map(|n| Bson::String(n.clone()))
                .collect::<Vec<_>>(),
        );
        Some(Bson::Document(record))
    }
}

/// The parsed body of a function, by its full CREATE text.
fn parsed(create_sql: &str) -> Result<Value, PlError> {
    static CACHE: OnceLock<Mutex<HashMap<String, Value>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(create_sql)
    {
        return Ok(v.clone());
    }
    // PL/pgSQL records `RETURN NEXT v;` over a bare variable as a datum
    // number (`retvarno`) that the JSON rendering omits, so the statement
    // would arrive with nothing to return. Parenthesised, it is an ordinary
    // expression; a record variable is recognised again by name below.
    let mut rewritten = parenthesise_return_next(create_sql);
    // A refcursor PARAMETER opened as a cursor: see `open_cursor`.
    for _ in 0..8 {
        let Err(e) = pg_query::parse_plpgsql(&rewritten) else {
            break;
        };
        let text = e.to_string();
        let Some(var) = text
            .split("variable \"")
            .nth(1)
            .and_then(|r| r.strip_suffix("\" must be of type cursor or refcursor"))
            .or_else(|| {
                text.split("variable \"")
                    .nth(1)
                    .and_then(|r| r.split('"').next())
                    .filter(|_| text.contains("must be of type cursor or refcursor"))
            })
        else {
            break;
        };
        match open_cursor::cursor_param_rewrite(&rewritten, var) {
            Some(next) => rewritten = next,
            None => break,
        }
    }
    let v = pg_query::parse_plpgsql(&rewritten).map_err(|e| {
        let text = e.to_string();
        PlError::new(
            "42601",
            text.strip_prefix("Invalid statement: ")
                .unwrap_or(&text)
                .to_string(),
        )
    })?;
    let mut f = v
        .get(0)
        .and_then(|f| f.get("PLpgSQL_function"))
        .cloned()
        .ok_or_else(|| PlError::new("42601", "the function body did not parse"))?;
    block_scope::annotate(&mut f, &rewritten);
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(create_sql.to_string(), f.clone());
    Ok(f)
}

/// Every SQL text a PL/pgSQL body can run, each as a statement (an
/// expression or an assignment's value as `SELECT <expr>`), when the body
/// runs nothing else: no dynamic SQL, CALL, cursor, transaction control or
/// EXCEPTION block (a subtransaction). `None` for any other body -- the
/// caller (whether a read may run apart from its block) then refuses it.
pub fn static_queries(create_sql: &str) -> Option<Vec<String>> {
    const ALLOWED: &[&str] = &[
        "PLpgSQL_stmt_block",
        "PLpgSQL_stmt_assign",
        "PLpgSQL_stmt_if",
        "PLpgSQL_stmt_case",
        "PLpgSQL_stmt_loop",
        "PLpgSQL_stmt_while",
        "PLpgSQL_stmt_fori",
        "PLpgSQL_stmt_fors",
        "PLpgSQL_stmt_foreach_a",
        "PLpgSQL_stmt_exit",
        "PLpgSQL_stmt_return",
        "PLpgSQL_stmt_return_next",
        "PLpgSQL_stmt_return_query",
        "PLpgSQL_stmt_raise",
        "PLpgSQL_stmt_assert",
        "PLpgSQL_stmt_perform",
        "PLpgSQL_stmt_execsql",
        "PLpgSQL_stmt_getdiag",
    ];
    fn walk(v: &Value, out: &mut Vec<String>) -> Option<()> {
        match v {
            Value::Array(items) => {
                for i in items {
                    walk(i, out)?;
                }
            }
            Value::Object(map) => {
                for (k, inner) in map {
                    if k.starts_with("PLpgSQL_stmt_") {
                        if !ALLOWED.contains(&k.as_str()) {
                            return None;
                        }
                        if inner.get("exceptions").is_some_and(|e| !e.is_null())
                            || inner.get("dynquery").is_some()
                        {
                            return None;
                        }
                    }
                    if k == "cursor_explicit_expr" {
                        return None;
                    }
                    if k == "PLpgSQL_expr" {
                        let q = inner.get("query").and_then(Value::as_str)?;
                        let mode = inner.get("parseMode").and_then(Value::as_u64).unwrap_or(0);
                        out.push(match mode {
                            0 => q.to_string(),
                            // An assignment, `target := value`.
                            3..=5 => {
                                let (_, value) =
                                    q.split_once(":=").or_else(|| q.split_once('='))?;
                                format!("SELECT {value}")
                            }
                            _ => format!("SELECT {q}"),
                        });
                        continue;
                    }
                    walk(inner, out)?;
                }
            }
            _ => {}
        }
        Some(())
    }
    let f = parsed(create_sql).ok()?;
    let mut out = Vec::new();
    walk(&f, &mut out)?;
    Some(out)
}

/// An assignment statement's text split at its `:=` (or `=`) into the
/// target and the value, outside any bracket or parenthesis.
fn split_assignment(text: &str) -> Option<(&str, &str)> {
    let scan = pg_query::scan(text).ok()?;
    let mut depth = 0i32;
    for t in &scan.tokens {
        let (start, end) = (t.start as usize, t.end as usize);
        match text.get(start..end)? {
            "[" | "(" => depth += 1,
            "]" | ")" => depth -= 1,
            ":=" | "=" if depth == 0 => return Some((&text[..start], &text[end..])),
            _ => {}
        }
    }
    None
}

/// The subscripts and field selections of an assignment's target
/// (`x[1]`, `x[i][j]`, `x[2:]`), or `None` when it has none.
fn target_indirection(target: &str) -> Option<Vec<pg_query::protobuf::Node>> {
    use pg_query::protobuf::node::Node as N;
    let parsed = pg_query::parse(&format!("SELECT {target}")).ok()?;
    let stmt = parsed
        .protobuf
        .stmts
        .first()?
        .stmt
        .as_ref()?
        .node
        .as_ref()?;
    let N::SelectStmt(s) = stmt else { return None };
    let Some(N::ResTarget(rt)) = s.target_list.first()?.node.as_ref() else {
        return None;
    };
    match rt.val.as_ref()?.node.as_ref()? {
        N::AIndirection(ind) if !ind.indirection.is_empty() => Some(ind.indirection.clone()),
        _ => None,
    }
}

/// One subscript's text: `(is_slice, lower, upper)`.
type SubscriptText<'a> = (bool, Option<&'a str>, Option<&'a str>);

/// The text of each subscript of an assignment's target, in order:
/// `(is_slice, lower, upper)`, a bound left out `None` (`x[2:]`). Read off
/// the scanner's tokens, so a `::` cast or a bracket inside a bound is no
/// separator.
fn subscript_texts(target: &str) -> Option<Vec<SubscriptText<'_>>> {
    let scan = pg_query::scan(target).ok()?;
    let mut out = Vec::new();
    let (mut brackets, mut parens) = (0i32, 0i32);
    // The open subscript: where it starts, and where its `:` is.
    let mut open: Option<(usize, Option<(usize, usize)>)> = None;
    let bound = |from: usize, to: usize| Some(target[from..to].trim()).filter(|t| !t.is_empty());
    for t in &scan.tokens {
        let (start, end) = (t.start as usize, t.end as usize);
        match target.get(start..end)? {
            "(" => parens += 1,
            ")" => parens -= 1,
            "[" => {
                if brackets == 0 && parens == 0 {
                    open = Some((end, None));
                }
                brackets += 1;
            }
            "]" => {
                brackets -= 1;
                if brackets == 0 && parens == 0 {
                    let (from, colon) = open.take()?;
                    out.push(match colon {
                        Some((c_start, c_end)) => (true, bound(from, c_start), bound(c_end, start)),
                        None => (false, None, bound(from, start)),
                    });
                }
            }
            ":" if brackets == 1 && parens == 0 => {
                if let Some((_, colon @ None)) = open.as_mut() {
                    *colon = Some((start, end));
                }
            }
            _ => {}
        }
    }
    Some(out)
}

/// Is `expr` a bare string or NULL constant, of type `unknown` until its
/// context gives it one?
fn untyped_literal(expr: &str) -> bool {
    use pg_query::protobuf::node::Node as N;
    let Ok(parsed) = pg_query::parse(&format!("SELECT {expr}")) else {
        return false;
    };
    let target = parsed
        .protobuf
        .stmts
        .first()
        .and_then(|s| s.stmt.as_ref())
        .and_then(|s| match s.node.as_ref() {
            Some(N::SelectStmt(sel)) => sel.target_list.first().cloned(),
            _ => None,
        });
    let Some(N::ResTarget(rt)) = target.and_then(|t| t.node) else {
        return false;
    };
    matches!(
        rt.val.as_ref().and_then(|v| v.node.as_ref()),
        Some(N::AConst(c)) if c.isnull || matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_)))
    )
}

/// A planner error as the interpreter raises it, its DETAIL split off.
fn plan_error(e: secantus_pgplan::Error) -> PlError {
    let message = e.to_string();
    let mut err = match message.split_once("\nDetail: ") {
        Some((m, d)) => {
            let mut err = PlError::new(e.sqlstate(), m);
            err.detail = Some(d.to_string());
            err
        }
        None => PlError::new(e.sqlstate(), message.clone()),
    };
    err.from_sql = false;
    err
}

/// `RETURN NEXT ident;` → `RETURN NEXT (ident);`, case-insensitively.
fn parenthesise_return_next(sql: &str) -> String {
    let lower = sql.to_ascii_lowercase();
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len() + 8);
    let mut last = 0;
    let mut from = 0;
    while let Some(at) = lower[from..].find("return") {
        let start = from + at;
        from = start + 6;
        if start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            continue;
        }
        let mut i = start + 6;
        let skip_ws = |mut i: usize| {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            i
        };
        let ws = skip_ws(i);
        if ws == i || !lower[ws..].starts_with("next") {
            continue;
        }
        i = ws + 4;
        let id_start = skip_ws(i);
        if id_start == i {
            continue;
        }
        let mut id_end = id_start;
        while id_end < bytes.len()
            && (bytes[id_end].is_ascii_alphanumeric() || bytes[id_end] == b'_')
        {
            id_end += 1;
        }
        if id_end == id_start || bytes[id_start].is_ascii_digit() {
            continue;
        }
        let semi = skip_ws(id_end);
        if semi >= bytes.len() || bytes[semi] != b';' {
            continue;
        }
        out.push_str(&sql[last..id_start]);
        out.push('(');
        out.push_str(&sql[id_start..id_end]);
        out.push(')');
        last = id_end;
        from = id_end;
    }
    out.push_str(&sql[last..]);
    out
}

/// Check a body parses, for CREATE FUNCTION: PostgreSQL validates at CREATE.
pub fn validate(create_sql: &str) -> Result<(), PlError> {
    parsed(create_sql).map(|_| ())
}

/// A variable a DECLARE section (or a cursor's argument list) declares:
/// its line in the body, its name as PL/pgSQL records it, and its type's
/// text exactly as written (libpg_query keeps `read_datatype`'s source
/// text, `%TYPE` and `%ROWTYPE` included, and no location).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    /// Its datum number.
    pub datum: usize,
    pub lineno: usize,
    pub refname: String,
    pub typname: String,
}

/// Every variable the body declares, in the order PL/pgSQL compiles them
/// (its datums). The routine's parameters (no line) and the variables the
/// compiler makes itself -- `found`, a FOR loop's integer, an exception
/// block's `sqlstate` / `sqlerrm`, a cursor's `refcursor` -- are left out:
/// their types are the compiler's, not text the body wrote. Empty when the
/// body does not parse (`validate` reports that).
pub fn declared_types(create_sql: &str) -> Vec<Declared> {
    const COMPILER_TYPES: &[&str] = &[
        "pg_catalog.\"boolean\"",
        "pg_catalog.boolean",
        "pg_catalog.\"integer\"",
        "pg_catalog.integer",
        "pg_catalog.text",
        "pg_catalog.refcursor",
    ];
    let Ok(f) = parsed(create_sql) else {
        return Vec::new();
    };
    f.get("datums")
        .and_then(Value::as_array)
        .map(|ds| {
            ds.iter()
                .enumerate()
                .filter_map(|(datum, d)| {
                    let v = d.get("PLpgSQL_var")?;
                    let lineno = v.get("lineno").and_then(Value::as_u64)? as usize;
                    let typname = v
                        .get("datatype")?
                        .get("PLpgSQL_type")?
                        .get("typname")?
                        .as_str()?;
                    if COMPILER_TYPES.contains(&typname) {
                        return None;
                    }
                    Some(Declared {
                        datum,
                        lineno,
                        refname: v.get("refname")?.as_str()?.to_string(),
                        typname: typname.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Where in `body` (a byte offset) the type of `d` begins: the first place
/// from the start of its line where its text follows its name, with an
/// optional `CONSTANT` between, as the declaration (`decl_varname
/// decl_const decl_datatype`) or cursor argument wrote it. `None` when it
/// cannot be found that way.
pub fn declared_type_offset(body: &str, d: &Declared) -> Option<usize> {
    let line_start = if d.lineno <= 1 {
        0
    } else {
        body.match_indices('\n').nth(d.lineno - 2)?.0 + 1
    };
    let text = d.typname.trim_end();
    if text.is_empty() {
        return None;
    }
    let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let ends_with_word = |s: &str, w: &str| -> Option<usize> {
        let cut = s.len().checked_sub(w.len())?;
        let tail = s.get(cut..)?;
        let before = s[..cut].chars().next_back();
        (tail.eq_ignore_ascii_case(w) && !before.is_some_and(is_ident)).then_some(cut)
    };
    let quoted = format!("\"{}\"", d.refname.replace('"', "\"\""));
    for (i, _) in body[line_start..].match_indices(text) {
        let at = line_start + i;
        let before = &body[..at];
        let head = before.trim_end();
        if head.len() == before.len() {
            continue;
        }
        let head = match ends_with_word(head, "constant") {
            Some(cut) if head[..cut].len() > head[..cut].trim_end().len() => head[..cut].trim_end(),
            _ => head,
        };
        let named = head.ends_with(&quoted)
            || (d.refname == d.refname.to_lowercase()
                && ends_with_word(head, &d.refname).is_some());
        if named {
            return Some(at);
        }
    }
    None
}

/// A type name as PL/pgSQL records it (`pg_catalog.int4`, `int `,
/// `pg_catalog."integer"`) in the server's canonical spelling.
fn canonical_type(raw: &str) -> String {
    let t = raw
        .trim()
        .trim_start_matches("pg_catalog.")
        .replace('"', "");
    let t = t.trim().to_ascii_lowercase();
    let (base, array) = match t.strip_suffix("[]") {
        Some(b) => (b.trim().to_string(), true),
        None => (t, false),
    };
    let base = match secantus_pgplan::pgtypes::oid_of_name(&base)
        .and_then(secantus_pgplan::pgtypes::name_of_oid)
    {
        Some(n) => n.to_string(),
        None => match base.as_str() {
            "int" | "integer" => "int4".into(),
            "bigint" => "int8".into(),
            "smallint" => "int2".into(),
            "boolean" => "bool".into(),
            "double precision" => "float8".into(),
            "real" => "float4".into(),
            "character varying" => "varchar".into(),
            "timestamp with time zone" => "timestamptz".into(),
            "timestamp without time zone" => "timestamp".into(),
            _ => base,
        },
    };
    if array {
        format!("{base}[]")
    } else {
        base
    }
}

/// PostgreSQL's condition names for the SQLSTATEs a handler most often names.
fn condition_sqlstate(name: &str) -> Option<&'static str> {
    Some(match name {
        "division_by_zero" => "22012",
        "unique_violation" => "23505",
        "foreign_key_violation" => "23503",
        "not_null_violation" => "23502",
        "check_violation" => "23514",
        "numeric_value_out_of_range" => "22003",
        "invalid_text_representation" => "22P02",
        "raise_exception" => "P0001",
        "no_data_found" => "P0002",
        "too_many_rows" => "P0003",
        "assert_failure" => "P0004",
        "undefined_table" => "42P01",
        "undefined_column" => "42703",
        "undefined_function" => "42883",
        "syntax_error" => "42601",
        "feature_not_supported" => "0A000",
        "data_exception" => "22000",
        "integrity_constraint_violation" => "23000",
        "invalid_parameter_value" => "22023",
        "string_data_right_truncation" => "22001",
        "null_value_not_allowed" => "22004",
        "case_not_found" => "20000",
        "cardinality_violation" => "21000",
        "serialization_failure" => "40001",
        "deadlock_detected" => "40P01",
        "lock_not_available" => "55P03",
        "insufficient_privilege" => "42501",
        "duplicate_object" => "42710",
        "duplicate_table" => "42P07",
        _ => return None,
    })
}

/// Does a handler's condition catch `sqlstate`? `others` catches all but a
/// cancel; a class condition (`integrity_constraint_violation`, a code ending
/// `000`) catches its class.
fn condition_matches(cond: &Value, sqlstate: &str) -> bool {
    let name = cond
        .get("PLpgSQL_condition")
        .and_then(|c| c.get("condname"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name == "others" {
        return sqlstate != "57014";
    }
    if let Some(state) = cond
        .get("PLpgSQL_condition")
        .and_then(|c| c.get("sqlerrstate"))
        .and_then(Value::as_str)
    {
        return state == sqlstate;
    }
    match condition_sqlstate(&name) {
        Some(code) if code.ends_with("000") => sqlstate.starts_with(&code[..2]),
        Some(code) => code == sqlstate,
        None => name.len() == 5 && name.eq_ignore_ascii_case(sqlstate),
    }
}

/// Run one call of a PL/pgSQL function.
pub fn run(create_sql: &str, inv: Invocation<'_>, host: &dyn Host) -> Result<Outcome, PlError> {
    let f = parsed(create_sql)?;
    let mut datums = Vec::new();
    let mut found_no = None;
    for (i, d) in f
        .get("datums")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        if let Some(v) = d.get("PLpgSQL_var") {
            let name = v
                .get("refname")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if name == "found" && found_no.is_none() {
                found_no = Some(i);
            }
            let ty = v
                .get("datatype")
                .and_then(|t| t.get("PLpgSQL_type"))
                .and_then(|t| t.get("typname"))
                .and_then(Value::as_str)
                .map(canonical_type)
                .unwrap_or_else(|| "text".into());
            let value = if name == "found" {
                Bson::Boolean(false)
            } else {
                Bson::Null
            };
            datums.push(Datum::Var { name, ty, value });
        } else if let Some(r) = d.get("PLpgSQL_rec") {
            let name = r
                .get("refname")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            datums.push(Datum::Rec { name, rec: None });
        } else if let Some(r) = d.get("PLpgSQL_row") {
            let fields = r
                .get("fields")
                .and_then(Value::as_array)
                .map(|fs| {
                    fs.iter()
                        .filter_map(|x| x.get("varno").and_then(Value::as_u64))
                        .map(|n| n as usize)
                        .collect()
                })
                .unwrap_or_default();
            datums.push(Datum::Row { fields });
        } else if let Some(r) = d.get("PLpgSQL_recfield") {
            datums.push(Datum::RecField {
                field: r
                    .get("fieldname")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                parent: r.get("recparentno").and_then(Value::as_u64).unwrap_or(0) as usize,
            });
        } else {
            datums.push(Datum::Var {
                name: String::new(),
                ty: "text".into(),
                value: Bson::Null,
            });
        }
    }
    // The arguments are the first datums, in declared order.
    let mut arg = 0;
    let mut arg_slots = Vec::new();
    for (slot, d) in datums.iter_mut().enumerate() {
        if arg >= inv.args.len() {
            break;
        }
        if let Datum::Var { value, name, ty } = d {
            if name == "found" {
                continue;
            }
            arg_slots.push(slot);
            *value = inv.args[arg].clone();
            if let Some(declared) = inv.arg_types.get(arg) {
                *ty = canonical_type(declared);
            }
            arg += 1;
        }
    }
    // A `%TYPE` declaration has the type it names (RP-818).
    let mut kinds: Vec<percent_type::Kind> = datums
        .iter()
        .map(|d| match d {
            Datum::Var { ty, .. } => percent_type::Kind::Var(ty.clone()),
            _ => percent_type::Kind::Other,
        })
        .collect();
    let fname = function_name(create_sql);
    for (d, t) in percent_type::resolve(&f, fname.as_deref(), &mut kinds, &|w| host.percent_type(w))
    {
        if let (Some(t), Some(Datum::Var { ty, .. })) = (t, datums.get_mut(d)) {
            *ty = t;
        }
    }
    let mut interp = Interp {
        datums,
        arg_slots,
        fn_name: fname,
        host,
        trigger: inv.trigger.clone(),
        returns_set: inv.returns_set,
        out_params: inv.out_params.to_vec(),
        procedure: inv.procedure,
        nonatomic: inv.nonatomic,
        subtxn_depth: 0,
        set_rows: Vec::new(),
        row_count: 0,
        found_no,
        function_level: found_no.map(|found| {
            ["new_varno", "old_varno"]
                .iter()
                .filter_map(|k| f.get(*k).and_then(Value::as_u64))
                .map(|n| n as usize + 1)
                .fold(found + 1, usize::max)
        }),
        handling: None,
        cursor_exprs: open_cursor::cursor_exprs(&f),
        cursor_options: open_cursor::cursor_options(&f),
        cursor_argrows: open_cursor::cursor_argrows(&f),
        expression_types: std::cell::RefCell::default(),
        label: function_label(create_sql),
        dyn_text: None,
        scopes: block_scope::scopes(&f),
        defaults: f
            .get("datums")
            .and_then(Value::as_array)
            .map(|ds| {
                ds.iter()
                    .enumerate()
                    .filter_map(|(i, d)| {
                        let v = d.get("PLpgSQL_var")?;
                        match v.get("default_val") {
                            Some(e) => Some((i, e.clone())),
                            None => open_cursor::cursor_name_default(v).map(|e| (i, e)),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        active: Vec::new(),
    };
    if let Some(t) = &inv.trigger {
        let new_no = f
            .get("new_varno")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let old_no = f
            .get("old_varno")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        for (no, rec) in [(new_no, &t.new), (old_no, &t.old)] {
            if let Some(Datum::Rec { rec: slot, .. }) = no.and_then(|n| interp.datums.get_mut(n)) {
                *slot = rec.clone();
            }
        }
    }
    // A block initialises the variables it declares each time it is
    // entered (`Interp::enter_block`). Only a default no rebuilt level owns
    // (a body that does not scan) is set here, before the body runs, in
    // declaration order: `x int := a * 2` may read a parameter or an
    // earlier variable.
    let owned: std::collections::HashSet<usize> = interp
        .scopes
        .iter()
        .flat_map(|s| s.init.iter().copied())
        .collect();
    let mut defaults: Vec<(usize, Value)> = interp
        .defaults
        .iter()
        .filter(|(i, _)| !owned.contains(i))
        .map(|(i, e)| (*i, e.clone()))
        .collect();
    defaults.sort_by_key(|(i, _)| *i);
    for (i, e) in defaults {
        let v = interp.eval_typed(&e, i)?;
        interp.set_var(i, v)?;
    }
    let action = f.get("action").cloned().unwrap_or(Value::Null);
    match interp.stmt(&action)? {
        Flow::Return(o) => Ok(o),
        _ if interp.returns_set => Ok(Outcome::Rows(std::mem::take(&mut interp.set_rows))),
        // A procedure simply ends; its OUT / INOUT values are its result.
        _ if interp.procedure => Ok(Outcome::Value(interp.out_value().unwrap_or(Bson::Null))),
        // A function with OUT parameters returns them at its end.
        _ if interp.trigger.is_none() && !interp.out_params.is_empty() => {
            Ok(Outcome::Value(interp.out_value().unwrap_or(Bson::Null)))
        }
        // An event trigger function returns nothing.
        _ if interp
            .trigger
            .as_ref()
            .is_some_and(|t| t.level == EVENT_LEVEL) =>
        {
            Ok(Outcome::Value(Bson::Null))
        }
        _ if interp.trigger.is_some() => Err(PlError::new(
            "2F005",
            "control reached end of trigger procedure without RETURN",
        )),
        _ => Err(PlError::new(
            "2F005",
            "control reached end of function without RETURN",
        )),
    }
}

/// The function's own name, as its CREATE FUNCTION spells it.
fn function_name(create_sql: &str) -> Option<String> {
    use pg_query::protobuf::node::Node as N;
    let p = pg_query::parse(create_sql).ok()?;
    let N::CreateFunctionStmt(c) = p.protobuf.stmts.first()?.stmt.as_ref()?.node.as_ref()? else {
        return None;
    };
    match c.funcname.last()?.node.as_ref()? {
        N::String(s) => Some(s.sval.clone()),
        _ => None,
    }
}

/// How CONTEXT names the function: `name(argument types)` as
/// `format_procedure` prints it; a DO block is `inline_code_block`.
fn function_label(create_sql: &str) -> String {
    use pg_query::protobuf::node::Node as N;
    use pg_query::protobuf::FunctionParameterMode as M;
    let Some((name, args)) = pg_query::parse(create_sql).ok().and_then(|p| {
        let stmt = p.protobuf.stmts.first()?.stmt.as_ref()?.node.as_ref()?;
        let N::CreateFunctionStmt(c) = stmt else {
            return None;
        };
        let name = match c.funcname.last()?.node.as_ref()? {
            N::String(s) => s.sval.clone(),
            _ => return None,
        };
        let mut args = Vec::new();
        for p in &c.parameters {
            let Some(N::FunctionParameter(fp)) = p.node.as_ref() else {
                continue;
            };
            if matches!(
                M::try_from(fp.mode),
                Ok(M::FuncParamOut | M::FuncParamTable)
            ) {
                continue;
            }
            let t = fp.arg_type.as_ref()?;
            let base = match t.names.last()?.node.as_ref()? {
                N::String(s) => s.sval.clone(),
                _ => return None,
            };
            let array = if t.array_bounds.is_empty() { "" } else { "[]" };
            args.push(format!(
                "{}{array}",
                secantus_pgplan::display_type(&canonical_type(&base))
            ));
        }
        Some((name, args))
    }) else {
        return "inline_code_block".into();
    };
    if name == "inline_code_block" {
        return name;
    }
    format!("{name}({})", args.join(","))
}

/// `plpgsql_stmt_typename`: what CONTEXT says a statement is.
fn stmt_typename(kind: &str, body: &Value) -> &'static str {
    let flag = |k: &str| body.get(k).and_then(Value::as_bool).unwrap_or(false);
    match kind.strip_prefix("PLpgSQL_stmt_").unwrap_or(kind) {
        "block" => "statement block",
        "assign" => "assignment",
        "if" => "IF",
        "case" => "CASE",
        "loop" => "LOOP",
        "while" => "WHILE",
        "fori" => "FOR with integer loop variable",
        "fors" => "FOR over SELECT rows",
        "forc" => "FOR over cursor",
        "foreach_a" => "FOREACH over array",
        "exit" if flag("is_exit") => "EXIT",
        "exit" => "CONTINUE",
        "return" => "RETURN",
        "return_next" => "RETURN NEXT",
        "return_query" => "RETURN QUERY",
        "raise" => "RAISE",
        "assert" => "ASSERT",
        "execsql" => "SQL statement",
        "dynexecute" => "EXECUTE",
        "dynfors" => "FOR over EXECUTE statement",
        "getdiag" if flag("is_stacked") => "GET STACKED DIAGNOSTICS",
        "getdiag" => "GET DIAGNOSTICS",
        "open" => "OPEN",
        "fetch" if flag("is_move") => "MOVE",
        "fetch" => "FETCH",
        "close" => "CLOSE",
        "perform" => "PERFORM",
        "call" if flag("is_call") => "CALL",
        "call" => "DO",
        "commit" => "COMMIT",
        "rollback" => "ROLLBACK",
        _ => "unknown",
    }
}

/// The CONTEXT frame for an expression whose evaluation failed (`sql` is
/// the query it became, before its variables were bound), or `None`
/// where PostgreSQL adds none.
///
/// PostgreSQL adds `SQL expression "..."` (`_SPI_error_callback`) to an
/// error raised while SPI parses, plans or runs the query -- except one
/// that carries a cursor position (an unreadable literal, `'x'::int`),
/// which becomes an internal query position instead. PL/pgSQL runs a
/// SIMPLE expression (one target, no FROM, no subquery) outside SPI once
/// it is planned, so an error it raises while EXECUTING has no frame;
/// one raised while PLANNING it does. Planning folds every immutable
/// subexpression over constants (`eval_const_expressions`), so the error
/// is a planning one exactly when one of those subexpressions fails on
/// its own: `fails` evaluates such a subexpression (`SELECT <it>`) and
/// answers whether it raised the error's SQLSTATE. Measured against PostgreSQL 15.19 over 60
/// shapes (batch 64).
/// The frame an error binding the expression `text` takes: what
/// PostgreSQL names the expression by (`expr_frame`'s names), always, since
/// the expression is where it was raised.
fn bind_frame(text: &str, mode: u64) -> Option<String> {
    Some(match mode {
        3..=5 => format!("PL/pgSQL assignment \"{text}\""),
        2 => format!("SQL expression \"{text}\""),
        _ => format!("SQL statement \"{text}\""),
    })
}

/// An input error on a literal of `sql` (`'x'::int`): it carries a cursor
/// position, which `_SPI_error_callback` turns into an internal query
/// position instead of a `SQL ...` CONTEXT frame.
fn literal_input_error(sql: &str, err: &PlError) -> bool {
    err.sqlstate.starts_with("22")
        && err
            .message
            .strip_suffix('"')
            .and_then(|m| m.rsplit_once(": \""))
            .map(|(_, v)| v)
            .is_some_and(|lit| sql.contains(&format!("'{}'", lit.replace('\'', "''"))))
}

fn expr_frame(
    text: &str,
    mode: u64,
    sql: &str,
    err: &PlError,
    fails: &dyn Fn(&str) -> bool,
) -> Option<String> {
    use pg_query::protobuf::node::Node as N;
    let frame = || {
        Some(match mode {
            3..=5 => format!("PL/pgSQL assignment \"{text}\""),
            2 => format!("SQL expression \"{text}\""),
            _ => format!("SQL statement \"{text}\""),
        })
    };
    if literal_input_error(sql, err) {
        return None;
    }
    let parsed = pg_query::parse(sql).ok()?;
    let simple = match parsed
        .protobuf
        .stmts
        .first()
        .and_then(|s| s.stmt.as_ref())
        .and_then(|n| n.node.as_ref())
    {
        Some(N::SelectStmt(s)) => {
            s.from_clause.is_empty()
                && s.where_clause.is_none()
                && s.target_list.len() == 1
                && s.larg.is_none()
                && s.with_clause.is_none()
        }
        _ => false,
    };
    let nodes = parsed.protobuf.nodes();
    let has_sublink = nodes
        .iter()
        .any(|(n, _, _, _)| matches!(n, pg_query::NodeRef::SubLink(_)));
    if !simple || has_sublink {
        return frame();
    }
    // Every operator / call / cast subexpression that folds, tried alone.
    let folds = expression_folds;
    for (n, _, _, _) in &nodes {
        if !matches!(
            n,
            pg_query::NodeRef::AExpr(_)
                | pg_query::NodeRef::FuncCall(_)
                | pg_query::NodeRef::TypeCast(_)
                | pg_query::NodeRef::BoolExpr(_)
                | pg_query::NodeRef::CaseExpr(_)
                | pg_query::NodeRef::CoalesceExpr(_)
                | pg_query::NodeRef::MinMaxExpr(_)
                | pg_query::NodeRef::NullTest(_)
                | pg_query::NodeRef::AArrayExpr(_)
                | pg_query::NodeRef::AIndirection(_)
        ) {
            continue;
        }
        // The statement as parsed (every enum field valid, which the
        // deparser asserts), its one target replaced by the subexpression.
        let mut wrapped = parsed.protobuf.clone();
        let Some(N::SelectStmt(sel)) = wrapped
            .stmts
            .first_mut()
            .and_then(|s| s.stmt.as_mut())
            .and_then(|n| n.node.as_mut())
        else {
            continue;
        };
        let Some(N::ResTarget(rt)) = sel.target_list.first_mut().and_then(|t| t.node.as_mut())
        else {
            continue;
        };
        rt.name.clear();
        rt.val = Some(Box::new(pg_query::protobuf::Node {
            node: Some(n.to_enum()),
        }));
        let Ok(text) = pg_query::deparse(&wrapped) else {
            continue;
        };
        if folds(&n.to_enum()) && fails(&text) {
            return frame();
        }
    }
    None
}

/// The one target expression of the query `sql`, a `SELECT <expr>`.
fn select_target(sql: &str) -> Option<pg_query::protobuf::node::Node> {
    use pg_query::protobuf::node::Node as N;
    let parsed = pg_query::parse(sql).ok()?;
    let stmt = parsed.protobuf.stmts.into_iter().next()?.stmt?.node?;
    let N::SelectStmt(sel) = stmt else {
        return None;
    };
    let [target] = sel.target_list.as_slice() else {
        return None;
    };
    let Some(N::ResTarget(rt)) = target.node.as_ref() else {
        return None;
    };
    rt.val.as_ref()?.node.clone()
}

/// Does `expr` fold to a constant when its query is planned: no
/// parameter, column or subquery, and no call PostgreSQL leaves to the
/// executor (a volatile or stable one, an aggregate, a window call)?
fn expression_folds(expr: &pg_query::protobuf::node::Node) -> bool {
    use pg_query::protobuf::node::Node as N;
    !expr.nodes().iter().any(|(n, _, _, _)| match n {
        pg_query::NodeRef::ParamRef(_)
        | pg_query::NodeRef::ColumnRef(_)
        | pg_query::NodeRef::SubLink(_) => true,
        pg_query::NodeRef::FuncCall(f) => {
            let name = f
                .funcname
                .last()
                .and_then(|n| match n.node.as_ref() {
                    Some(N::String(s)) => Some(s.sval.as_str()),
                    _ => None,
                })
                .unwrap_or("");
            !secantus_pgplan::is_known_function(name)
                || !f.agg_order.is_empty()
                || f.over.is_some()
                || matches!(
                    name,
                    "random"
                        | "now"
                        | "nextval"
                        | "currval"
                        | "setval"
                        | "clock_timestamp"
                        | "timeofday"
                        | "pg_sleep"
                        | "gen_random_uuid"
                        | "statement_timestamp"
                        | "transaction_timestamp"
                        | "current_setting"
                        | "set_config"
                )
        }
        _ => false,
    })
}

/// A record as a whole, as a value of its row type.
fn record_value(rec: &Record) -> Bson {
    let mut d = bson::Document::new();
    d.insert(secantus_pgplan::RECORD_KEY, Bson::Array(rec.values.clone()));
    d.insert(
        secantus_pgplan::RECORD_TYPES_KEY,
        Bson::Array(
            rec.columns
                .iter()
                .map(|(_, t)| Bson::String(t.clone()))
                .collect(),
        ),
    );
    Bson::Document(d)
}

fn expr_query(e: &Value) -> Option<(&str, u64)> {
    let e = e.get("PLpgSQL_expr")?;
    Some((
        e.get("query").and_then(Value::as_str)?,
        e.get("parseMode").and_then(Value::as_u64).unwrap_or(0),
    ))
}

impl Interp<'_> {
    fn set_found(&mut self, found: bool) {
        if let Some(n) = self.found_no {
            if let Some(Datum::Var { value, .. }) = self.datums.get_mut(n) {
                *value = Bson::Boolean(found);
            }
        }
    }

    /// The SQL an expression becomes, with its variables bound: `(sql,
    /// params, types)`.
    fn bind(&self, text: &str) -> Result<(String, Vec<Bson>, Vec<String>), PlError> {
        let scan = pg_query::scan(text).map_err(|e| PlError::new("42601", e.to_string()))?;
        let tokens = scan.tokens;
        let tok = |i: usize| -> Option<&str> {
            let t = tokens.get(i)?;
            text.get(t.start as usize..t.end as usize)
        };
        let mut out = String::new();
        let mut params: Vec<Bson> = Vec::new();
        let mut types: Vec<String> = Vec::new();
        let mut cursor = 0usize;
        let mut i = 0usize;
        let ident = |i: usize| -> Option<String> {
            let t = tokens.get(i)?;
            let is_word = t.token == pg_query::protobuf::Token::Ident as i32
                || t.keyword_kind == pg_query::protobuf::KeywordKind::UnreservedKeyword as i32
                || t.keyword_kind == pg_query::protobuf::KeywordKind::ColNameKeyword as i32;
            if !is_word {
                return None;
            }
            let raw = text.get(t.start as usize..t.end as usize)?;
            Some(if raw.starts_with('"') {
                raw.trim_matches('"').to_string()
            } else {
                raw.to_ascii_lowercase()
            })
        };
        while i < tokens.len() {
            let t = &tokens[i];
            let prev_dot = i > 0 && tok(i - 1) == Some(".");
            let next = tok(i + 1);
            let mut bound: Option<(Bson, String, usize)> = None;
            if let (Some(name), false) = (ident(i), prev_dot) {
                if next == Some(".") {
                    if let Some(field) = ident(i + 2) {
                        if let Some((v, ty)) = self.record_field(&name, &field) {
                            bound = Some((v, ty, i + 2));
                        } else if let Some(ty) = self.unassigned_trigger_field(&name, &field) {
                            // OLD in an INSERT trigger (NEW in a DELETE one)
                            // is NULL, and so is each of its fields.
                            bound = Some((Bson::Null, ty, i + 2));
                        } else if self.unassigned_record(&name) {
                            // A field of a record nothing has filled: its
                            // row type is not known yet (55000, raised by
                            // the expression, as PostgreSQL frames it).
                            let mut e = PlError::new(
                                "55000",
                                format!("record \"{name}\" is not assigned yet"),
                            );
                            e.detail = Some(
                                "The tuple structure of a not-yet-assigned record is indeterminate."
                                    .into(),
                            );
                            e.from_sql = true;
                            return Err(e);
                        } else if !matches!(tok(i + 3), Some(".") | Some("(")) {
                            // `blk.x`: a variable of a block (or FOR loop)
                            // labelled `blk`, innermost first; then `fname.p`,
                            // a variable of the function's own block,
                            // labelled with its name -- not `fname.r.f`, a
                            // field of a record parameter.
                            if let Some((v, ty)) = self
                                .labelled(&name, &field)
                                .and_then(|d| self.datum_value(d))
                                .or_else(|| self.function_level(&name, &field))
                            {
                                bound = Some((v, ty, i + 2));
                            }
                        } else if tok(i + 3) == Some(".") && tok(i + 5) != Some("(") {
                            // `blk.r.f`: a field of a record block `blk`
                            // declares.
                            let rec = self.labelled(&name, &field).and_then(|d| {
                                match self.datums.get(d) {
                                    Some(Datum::Rec { rec: Some(r), .. }) => Some(r),
                                    _ => None,
                                }
                            });
                            if let Some((v, ty)) = ident(i + 4)
                                .and_then(|f2| rec.and_then(|r| r.get(&f2)))
                                .map(|(v, ty)| (v.clone(), ty.to_string()))
                            {
                                bound = Some((v, ty, i + 4));
                            }
                        }
                    }
                } else if !matches!(next, Some("(" | "=>" | ":=")) {
                    // A named argument's name (`p(r => r)`) is not a column
                    // reference, so not a variable either.
                    if let Some((v, ty)) = self.scalar(&name) {
                        bound = Some((v, ty, i));
                    } else if let Some(rec) = self.record(&name) {
                        // A record as a whole: a value of its row type.
                        bound = Some((record_value(rec), "record".into(), i));
                    } else if self.unassigned_record(&name) {
                        // Unassigned, it is NULL as a whole (only a field of
                        // it is 55000).
                        bound = Some((Bson::Null, "record".into(), i));
                    }
                }
            }
            // `$n` in the body is the function's n-th argument (PL/pgSQL
            // names an argument's datum `$n`, whether or not it has a name).
            if bound.is_none() {
                let n = tok(i)
                    .and_then(|p| p.strip_prefix('$'))
                    .and_then(|d| d.parse::<usize>().ok());
                if let Some(Datum::Var { value, ty, .. }) = n
                    .filter(|n| *n >= 1)
                    .and_then(|n| self.arg_slots.get(n - 1))
                    .and_then(|slot| self.datums.get(*slot))
                {
                    bound = Some((value.clone(), ty.clone(), i));
                }
            }
            // `TG_ARGV[i]` counts from ZERO; the array bound here counts
            // from one, so the subscript is shifted.
            if let (Some((value, ty, _)), true) = (
                bound.as_ref(),
                ident(i).as_deref() == Some("tg_argv") && next == Some("["),
            ) {
                out.push_str(&text[cursor..t.start as usize]);
                params.push(value.clone());
                types.push(ty.clone());
                out.push_str(&format!("(${}::{})[1 + ", params.len(), sql_type(ty)));
                cursor = tokens[i + 1].end as usize;
                i += 2;
                continue;
            }
            // `WHERE CURRENT OF c` names the portal the cursor variable
            // holds, as PL/pgSQL passes it.
            let current_of = i >= 2
                && tok(i - 2).is_some_and(|w| w.eq_ignore_ascii_case("current"))
                && tok(i - 1).is_some_and(|w| w.eq_ignore_ascii_case("of"));
            if let (Some((Bson::String(portal), _, last)), true) = (bound.as_ref(), current_of) {
                out.push_str(&text[cursor..t.start as usize]);
                out.push_str(&secantus_pgplan::scalar::quote_identifier(portal));
                cursor = tokens[*last].end as usize;
                i = last + 1;
                continue;
            }
            if let Some((value, ty, last)) = bound {
                out.push_str(&text[cursor..t.start as usize]);
                params.push(value);
                types.push(ty.clone());
                // An array is parenthesised so a following subscript applies
                // to the VALUE, not to the cast's type name.
                if ty.ends_with("[]") {
                    out.push_str(&format!("(${}::{})", params.len(), sql_type(&ty)));
                } else {
                    out.push_str(&format!("${}::{}", params.len(), sql_type(&ty)));
                }
                cursor = tokens[last].end as usize;
                i = last + 1;
                continue;
            }
            i += 1;
        }
        out.push_str(&text[cursor..]);
        Ok((out, params, types))
    }

    /// The value of a variable, or of a record as a whole (NULL while
    /// nothing has filled it), by datum number.
    fn datum_value(&self, d: usize) -> Option<(Bson, String)> {
        match self.datums.get(d)? {
            Datum::Var { ty, value, .. } => Some((value.clone(), ty.clone())),
            Datum::Rec { rec: Some(r), .. } => Some((record_value(r), "record".into())),
            Datum::Rec { rec: None, .. } => Some((Bson::Null, "record".into())),
            _ => None,
        }
    }

    fn scalar(&self, name: &str) -> Option<(Bson, String)> {
        // The TG_ variables come first: PL/pgSQL declares them as datums of
        // a trigger function, which would otherwise shadow them with NULL.
        if let Some(v) = self.trigger_scalar(name) {
            return Some(v);
        }
        match self.datums.get(self.named(name)?)? {
            Datum::Var { ty, value, .. } => Some((value.clone(), ty.clone())),
            _ => None,
        }
    }

    /// `label.name` where `label` is the function's own name: the variables
    /// `do_compile` declares in the block it labels with that name -- the
    /// parameters (IN and OUT), FOUND and a trigger's TG_ variables. A
    /// variable of the body's own blocks is not one of them, and a
    /// parameter shadowed by one is still reached.
    fn function_level(&self, label: &str, name: &str) -> Option<(Bson, String)> {
        if self.fn_name.as_deref() != Some(label) {
            return None;
        }
        if let Some(v) = self.trigger_scalar(name) {
            return Some(v);
        }
        let var = |slot: usize| match self.datums.get(slot) {
            Some(Datum::Var { name: n, ty, value }) if n == name => {
                Some((value.clone(), ty.clone()))
            }
            _ => None,
        };
        if let Some(v) = self.arg_slots.iter().find_map(|s| var(*s)) {
            return Some(v);
        }
        if name == "found" {
            return self.found_no.and_then(var);
        }
        if self.out_params.iter().any(|o| o == name) {
            // The OUT parameters' datums follow the IN ones', ahead of
            // anything the body declares: the first of that name is it.
            return (0..self.datums.len()).find_map(var);
        }
        None
    }

    fn trigger_scalar(&self, name: &str) -> Option<(Bson, String)> {
        let t = self.trigger.as_ref()?;
        let text = |s: &str| Some((Bson::String(s.to_string()), "text".to_string()));
        if t.level == EVENT_LEVEL {
            return match name {
                "tg_event" => text(&t.when),
                "tg_tag" => text(&t.op),
                _ => None,
            };
        }
        match name {
            "tg_op" => text(&t.op),
            "tg_name" => text(&t.name),
            "tg_table_name" | "tg_relname" => text(&t.table),
            "tg_when" => text(&t.when),
            "tg_level" => text(&t.level),
            "tg_table_schema" => text("public"),
            "tg_nargs" => Some((Bson::Int32(t.args.len() as i32), "int4".to_string())),
            "tg_argv" => Some((
                Bson::Array(t.args.iter().map(|a| Bson::String(a.clone())).collect()),
                "text[]".to_string(),
            )),
            _ => None,
        }
    }

    fn record(&self, name: &str) -> Option<&Record> {
        match self.datums.get(self.named(name)?)? {
            Datum::Rec { rec, .. } => rec.as_ref(),
            _ => None,
        }
    }

    /// The type of `field` in a trigger's NEW or OLD that this operation
    /// leaves unassigned, read off the one it does assign.
    fn unassigned_trigger_field(&self, name: &str, field: &str) -> Option<String> {
        let t = self.trigger.as_ref()?;
        let unassigned = match name {
            "new" => t.new.is_none(),
            "old" => t.old.is_none(),
            _ => false,
        };
        if !unassigned {
            return None;
        }
        let other = if name == "new" {
            t.old.as_ref()
        } else {
            t.new.as_ref()
        };
        match other {
            Some(other) => other.get(field).map(|(_, ty)| ty.to_string()),
            None => t
                .row_type
                .iter()
                .find(|(c, _)| c == field)
                .map(|(_, ty)| ty.clone()),
        }
    }

    /// Is `name` a record variable that nothing has assigned (the
    /// innermost one of that name)?
    fn unassigned_record(&self, name: &str) -> bool {
        self.trigger.is_none()
            && matches!(
                self.named(name).and_then(|d| self.datums.get(d)),
                Some(Datum::Rec { rec: None, .. })
            )
    }

    fn record_field(&self, name: &str, field: &str) -> Option<(Bson, String)> {
        let rec = self.record(name)?;
        let (v, ty) = rec.get(field)?;
        Some((v.clone(), ty.to_string()))
    }

    /// Evaluate an expression to its single value.
    fn eval(&self, e: &Value) -> Result<Bson, PlError> {
        let (text, mode) =
            expr_query(e).ok_or_else(|| PlError::new("XX000", "an expression without a query"))?;
        let body = match mode {
            3 | 4 => text
                .split_once(":=")
                .map_or(text, |(_, r)| r)
                .trim()
                .to_string(),
            _ => text.to_string(),
        };
        self.eval_body(body, text, mode)
    }

    /// Evaluate `body`, a part of the expression `text` (of parse mode
    /// `mode`, which frames an error), to its single value.
    fn eval_body(&self, body: String, text: &str, mode: u64) -> Result<Bson, PlError> {
        let sql = if mode == 0 {
            body
        } else {
            format!("SELECT {body}")
        };
        let raw = sql.clone();
        let (sql, params, types) = self
            .bind(&sql)
            .map_err(|e| e.with_sql_frame(|| bind_frame(text, mode)))?;
        let out = self.host.query(&sql, &params, &types).map_err(|e| {
            let fails = |q: &str| {
                self.host
                    .query(q, &[], &[])
                    .is_err_and(|f| f.sqlstate == e.sqlstate)
            };
            let frame = if e.from_sql && !e.sql_framed && !e.framed {
                expr_frame(text, mode, &raw, &e, &fails)
            } else {
                None
            };
            e.with_sql_frame(|| frame)
        })?;
        if out.rows.len() > 1 {
            return Err(PlError::new("21000", "query returned more than one row"));
        }
        Ok(out
            .rows
            .into_iter()
            .next()
            .and_then(|r| r.into_iter().next())
            .unwrap_or(Bson::Null))
    }

    /// Evaluate an expression cast to datum `no`'s declared type, by the
    /// assignment rule (`exec_assign_value` -> `exec_cast_value`): a
    /// variable of (a domain over) `varchar(3)` refuses an overlong value
    /// with 22001 where an explicit cast would truncate it (RP-687). The
    /// value is read as the bare base type, fitted to the modifier, and
    /// only then cast to the declared type, which pads a `char(n)` and
    /// checks a domain's constraints.
    fn eval_typed(&self, e: &Value, no: usize) -> Result<Bson, PlError> {
        let ty = match self.datums.get(no) {
            Some(Datum::Var { ty, .. }) => Some(ty.clone()),
            _ => None,
        };
        let (text, mode) =
            expr_query(e).ok_or_else(|| PlError::new("XX000", "an expression without a query"))?;
        let body = match mode {
            3 | 4 => text
                .split_once(":=")
                .map_or(text, |(_, r)| r)
                .trim()
                .to_string(),
            _ => text.to_string(),
        };
        self.eval_typed_body(ty, body, text, mode)
    }

    /// [`Self::eval_typed`] of `body`, a part of the expression `text`,
    /// cast to the declared type `ty` (none: as it is).
    fn eval_typed_body(
        &self,
        ty: Option<String>,
        body: String,
        text: &str,
        mode: u64,
    ) -> Result<Bson, PlError> {
        // With no implicit or assignment cast from the expression's type,
        // the value goes through its output function and the declared
        // type's input (`COERCION_PLPGSQL`'s `COERCEVIAIO`): evaluated as
        // it is, then converted as a moved column is (RP-712).
        if let Some(src) = ty.as_deref().and_then(|ty| {
            let src = self.expression_type(&body)?;
            secantus_pgplan::plpgsql_assigns_via_io(&src, ty).then_some(src)
        }) {
            let v = self.eval_body(body.clone(), text, mode)?;
            // An assignment statement's coercion is part of its query
            // (`transformPLAssignStmt`), so an error in it is framed by
            // the assignment; a declaration's default is coerced outside
            // the query (`exec_assign_value`), with no frame.
            // A conversion over a constant through immutable I/O
            // functions folds while the query is planned, and its error
            // is framed by it; any other runs in the executor's simple
            // expression path, which frames nothing.
            let planned = matches!(mode, 3..=5)
                && secantus_pgplan::plpgsql_io_folds(&src, ty.as_deref().unwrap_or_default())
                && select_target(&format!("SELECT {body}")).is_some_and(|n| expression_folds(&n));
            return self
                .convert_to(ty.as_deref().unwrap_or_default(), v, &src)
                .map_err(|err| err.with_sql_frame(|| planned.then(|| bind_frame(text, mode))?));
        }
        let length = ty.as_deref().and_then(|ty| {
            let (sql, _, types) = self.bind(&format!("SELECT {body}")).ok()?;
            self.host.assignment_length(ty, &sql, &types)
        });
        let sql = match (&ty, &length) {
            // A bare `bit` is the grammar's `bit(1)`; `varbit` reads the
            // value at its own length, for the fit to measure.
            (_, Some((base, _))) => format!("SELECT ({body})::{}", length_read_type(base)),
            (Some(ty), None) => format!("SELECT ({body})::{}", sql_type(ty)),
            (None, None) => format!("SELECT {body}"),
        };
        let raw = sql.clone();
        let (sql, params, types) = self
            .bind(&sql)
            .map_err(|e| e.with_sql_frame(|| bind_frame(text, mode)))?;
        let out = self.host.query(&sql, &params, &types).map_err(|e| {
            let fails = |q: &str| {
                self.host
                    .query(q, &[], &[])
                    .is_err_and(|f| f.sqlstate == e.sqlstate)
            };
            let frame = if e.from_sql && !e.sql_framed && !e.framed {
                expr_frame(text, mode, &raw, &e, &fails)
            } else {
                None
            };
            e.with_sql_frame(|| frame)
        })?;
        let v = out
            .rows
            .into_iter()
            .next()
            .and_then(|r| r.into_iter().next())
            .unwrap_or(Bson::Null);
        match (ty, length) {
            (Some(ty), Some((base, typmod))) => {
                let v = self.host.fit_assigned(v, &base, typmod)?;
                let out = self.host.query(
                    &format!("SELECT $1::{}", sql_type(&ty)),
                    &[v],
                    std::slice::from_ref(&base),
                )?;
                Ok(out
                    .rows
                    .into_iter()
                    .next()
                    .and_then(|r| r.into_iter().next())
                    .unwrap_or(Bson::Null))
            }
            _ => Ok(v),
        }
    }

    /// The type `SELECT <body>` describes as, over the body's variables
    /// (`None` when it does not describe as one column), remembered for
    /// the body's life.
    fn expression_type(&self, body: &str) -> Option<String> {
        let (sql, _, types) = self.bind(&format!("SELECT {body}")).ok()?;
        let key = (sql, types);
        if let Some(known) = self.expression_types.borrow().get(&key) {
            return known.clone();
        }
        let ty = self.host.expression_type(&key.0, &key.1);
        self.expression_types.borrow_mut().insert(key, ty.clone());
        ty
    }

    /// The value `exec_move_row` stores in variable `no` from a column of
    /// type `src` (a `SELECT`, `FETCH` or `EXECUTE ... INTO`, a `FOR` over
    /// a query): `exec_assign_value`'s coercion, the one a `:=` makes
    /// ([`Self::eval_typed`]). No coercion when the column is of the
    /// variable's own type and the declaration has no modifier
    /// (`exec_cast_value` compares the types and modifiers); otherwise the
    /// value is cast to the declared type, a length fitted by the
    /// assignment rule -- an overlong `varchar(3)` value is 22001, not
    /// truncated -- and a domain's constraints checked (RP-694).
    fn coerce_moved(&self, no: usize, v: Bson, src: &str) -> Result<Bson, PlError> {
        // `exec_move_row` casts outside the query that produced the row:
        // an error in the cast takes no `SQL statement` frame.
        self.convert_moved(no, v, src)
            .map_err(|e| e.with_sql_frame(|| None))
    }

    /// [`Self::coerce_moved`]'s conversion, its errors unframed.
    fn convert_moved(&self, no: usize, v: Bson, src: &str) -> Result<Bson, PlError> {
        let Some(Datum::Var { ty, .. }) = self.datums.get(no) else {
            return Ok(v);
        };
        self.convert_to(&ty.clone(), v, src)
    }

    /// The value `v`, of type `src`, converted to the declared type `ty`
    /// by PL/pgSQL's assignment rule ([`Self::coerce_moved`]).
    fn convert_to(&self, ty: &str, v: Bson, src: &str) -> Result<Bson, PlError> {
        let ty = ty.to_string();
        let unmodified_same = secantus_pgplan::declared_type_and_typmod(&ty)
            .is_some_and(|(base, typmod)| typmod < 0 && base == src);
        if unmodified_same || secantus_pgplan::domains::same_domain(src, &ty) {
            return Ok(v);
        }
        // No implicit or assignment cast: the source's output function,
        // then the declared type's input, as `text` (RP-712).
        if secantus_pgplan::plpgsql_assigns_via_io(src, &ty) {
            let text = match &v {
                Bson::Null => Bson::Null,
                v => Bson::String(secantus_pgplan::typeout::output_text(v, src)),
            };
            return self.convert_to(&ty, text, "text");
        }
        let types = [src.to_string()];
        let length = self.host.assignment_length(&ty, "SELECT $1", &types);
        let read = match &length {
            // `varbit` reads a bit string at its own length, for the fit
            // to measure (as `eval_typed` does).
            Some((base, _)) => length_read_type(base),
            None => sql_type(&ty),
        };
        let first = |out: QueryOut| {
            out.rows
                .into_iter()
                .next()
                .and_then(|r| r.into_iter().next())
                .unwrap_or(Bson::Null)
        };
        let v = first(
            self.host
                .query(&format!("SELECT $1::{read}"), &[v], &types)?,
        );
        match length {
            Some((base, typmod)) => {
                let v = self.host.fit_assigned(v, &base, typmod)?;
                Ok(first(self.host.query(
                    &format!("SELECT $1::{}", sql_type(&ty)),
                    &[v],
                    std::slice::from_ref(&base),
                )?))
            }
            None => Ok(v),
        }
    }

    /// The value an assignment to an array element or slice stores in
    /// variable `no` (`x[i] := v`, `x[i:j] := v`), or `None` when the
    /// assignment's target has no subscript. PostgreSQL 15 compiles it
    /// into one `SubscriptingRef` over the variable
    /// (`transformPLAssignStmt`): the subscripts are evaluated as `int4`
    /// and checked first, then the value, coerced by the assignment rule
    /// to the element type (to the array type for a slice), and
    /// `array_set_element` / `array_set_slice` place it (RP-711).
    fn assign_subscripted(&self, no: usize, e: &Value) -> Result<Option<Bson>, PlError> {
        use pg_query::protobuf::node::Node as N;
        let Some((text, mode)) = expr_query(e) else {
            return Ok(None);
        };
        if !matches!(mode, 3..=5) {
            return Ok(None);
        }
        let Some((target, value)) = split_assignment(text) else {
            return Ok(None);
        };
        let Some(indirection) = target_indirection(target) else {
            return Ok(None);
        };
        // The array is a variable's value or a record's field (`r.a[2] :=
        // 5`), which `set_var` writes back.
        let (ty, current) = match self.datums.get(no) {
            Some(Datum::Var { ty, value, .. }) => (ty.clone(), value.clone()),
            Some(Datum::RecField { field, parent }) => match self.datums.get(*parent) {
                Some(Datum::Rec { rec: Some(r), name }) => match r.get(field) {
                    Some((v, t)) => (t.to_string(), v.clone()),
                    None => {
                        return Err(PlError::new(
                            "42703",
                            format!("record \"{name}\" has no field \"{field}\""),
                        ))
                    }
                },
                Some(Datum::Rec { name, .. }) => {
                    return Err(PlError::new(
                        "55000",
                        format!("record \"{name}\" is not assigned yet"),
                    ))
                }
                _ => return Err(PlError::unsupported("this assignment target")),
            },
            _ => return Err(PlError::unsupported("this assignment target")),
        };
        if !indirection
            .iter()
            .all(|i| matches!(i.node.as_ref(), Some(N::AIndices(_))))
        {
            return Err(PlError::unsupported(
                "assignment to a field of an array element",
            ));
        }
        if ty == "jsonb" {
            return self
                .assign_jsonb_subscripted(target, value, text, mode, current)
                .map(Some);
        }
        // A domain over an array is subscripted as its base type, and the
        // new array is coerced back to the domain, which checks it.
        let (array_ty, domain) = if ty.ends_with("[]") {
            (ty.clone(), None)
        } else {
            match secantus_pgplan::domains::base_type_and_typmod(&ty) {
                Some((base, typmod, _)) if base.ends_with("[]") => {
                    let element = base.trim_end_matches("[]");
                    let element = if typmod >= 0 {
                        secantus_pgplan::scalar::format_type_text_public(element, Some(typmod))
                    } else {
                        element.to_string()
                    };
                    (format!("{element}[]"), Some((ty.clone(), base)))
                }
                _ => {
                    return Err(PlError::new(
                        "42804",
                        format!(
                            "cannot subscript type {} because it does not support subscripting",
                            secantus_pgplan::display_type(&ty)
                        ),
                    ))
                }
            }
        };
        let texts = subscript_texts(target)
            .filter(|t| t.len() == indirection.len())
            .ok_or_else(|| PlError::unsupported("this assignment target"))?;
        // Each subscript is coerced to `int4` by the assignment rule, a
        // parse-time check made before anything is evaluated.
        for t in texts.iter().flat_map(|(_, lo, hi)| [lo, hi]).flatten() {
            let typed = !untyped_literal(t);
            if typed
                && self
                    .expression_type(t)
                    .is_some_and(|src| !secantus_pgplan::subscript_coerces(&src))
            {
                return Err(PlError::new(
                    "42804",
                    "array subscript must have type integer",
                ));
            }
        }
        let subscript = |t: &Option<&str>| -> Result<Option<Bson>, PlError> {
            match t {
                Some(t) => self.eval_body(format!("({t})::int4"), text, mode).map(Some),
                None => Ok(None),
            }
        };
        let mut subs = Vec::with_capacity(texts.len());
        for (is_slice, lower, upper) in &texts {
            subs.push(if *is_slice {
                secantus_pgplan::array_assign::AssignSubscript::Slice(
                    subscript(lower)?,
                    subscript(upper)?,
                )
            } else {
                secantus_pgplan::array_assign::AssignSubscript::Index(
                    subscript(upper)?.unwrap_or(Bson::Null),
                )
            });
        }
        let is_slice = texts.iter().any(|(slice, _, _)| *slice);
        let value_ty = if is_slice {
            array_ty.clone()
        } else {
            array_ty.trim_end_matches("[]").to_string()
        };
        let value = value.trim().to_string();
        let v = match secantus_pgplan::array_assign::check_subscripts(&subs) {
            Ok(()) => self.eval_typed_body(Some(value_ty), value, text, mode)?,
            // The NULL check runs in the executor; a value that folds to a
            // constant was computed while the assignment was planned, so
            // its error comes first (`x[null] := 1/0` is 22012).
            Err(null) => {
                if select_target(&format!("SELECT {value}")).is_some_and(|n| expression_folds(&n)) {
                    self.eval_typed_body(Some(value_ty), value, text, mode)?;
                }
                return Err(plan_error(null));
            }
        };
        let out = secantus_pgplan::array_assign::assign_subscripts(current, &subs, v)
            .map_err(plan_error)?;
        Ok(Some(match domain {
            Some((domain, base)) => self.convert_to(&domain, out, &base)?,
            None => out,
        }))
    }

    /// `x['a'][1] := v` over a `jsonb` variable (RP-772): jsonb's
    /// subscripting handler (`jsonbsubs.c`). Each subscript is coerced to
    /// `int4` or `text` (`jsonb_subscript_transform`), evaluated and checked
    /// (none NULL), then the value is coerced to `jsonb` and placed by
    /// `jsonb_set_element`, which builds a missing path; a NULL variable
    /// starts as `[]` when the first subscript is an integer, else `{}`.
    fn assign_jsonb_subscripted(
        &self,
        target: &str,
        value: &str,
        text: &str,
        mode: u64,
        current: Bson,
    ) -> Result<Bson, PlError> {
        use secantus_pgplan::jsonb_subs::{self, SubscriptKind};
        let texts = subscript_texts(target)
            .ok_or_else(|| PlError::unsupported("this assignment target"))?;
        if texts.iter().any(|(slice, _, _)| *slice) {
            return Err(plan_error(jsonb_subs::slice_error()));
        }
        let mut subs = Vec::with_capacity(texts.len());
        for (_, _, upper) in &texts {
            let t = upper.ok_or_else(|| plan_error(jsonb_subs::slice_error()))?;
            let source = if untyped_literal(t) {
                "unknown".to_string()
            } else {
                self.expression_type(t).unwrap_or_default()
            };
            subs.push((t, jsonb_subs::subscript_kind(&source).map_err(plan_error)?));
        }
        let mut path = Vec::with_capacity(subs.len());
        let mut any_null = false;
        for (t, kind) in &subs {
            let cast = match kind {
                SubscriptKind::Int => "int4",
                SubscriptKind::Text => "text",
            };
            let v = self.eval_body(format!("({t})::{cast}"), text, mode)?;
            match jsonb_subs::path_step(&v, *kind).map_err(plan_error)? {
                Some(step) => path.push(step),
                None => any_null = true,
            }
        }
        let value = value.trim().to_string();
        if any_null {
            // As for an array: a value that folds to a constant was computed
            // while the assignment was planned, so its error comes first.
            if select_target(&format!("SELECT {value}")).is_some_and(|n| expression_folds(&n)) {
                self.eval_typed_body(Some("jsonb".to_string()), value, text, mode)?;
            }
            return Err(plan_error(jsonb_subs::null_subscript_error()));
        }
        let v = self.eval_typed_body(Some("jsonb".to_string()), value, text, mode)?;
        let expect_array = subs.first().map(|(_, k)| *k) == Some(SubscriptKind::Int);
        jsonb_subs::assign(&current, &path, expect_array, &v).map_err(plan_error)
    }

    /// The value an assignment stores in a record field of type `ty`
    /// (`exec_assign_value` over a `PLpgSQL_recfield`): the expression's
    /// value cast from its own type to the field's. A field of a domain
    /// checks the domain's constraints unless the value is already of the
    /// domain, so `new.a := -5` fails on its own line, whatever the trigger
    /// assigns after it (RP-670). A field of any other type takes the
    /// value by the assignment rule, as a variable does: a `varchar(3)`
    /// field refuses an overlong value with 22001 (RP-688).
    fn assign_field(&self, e: &Value, ty: &str) -> Result<Bson, PlError> {
        if secantus_pgplan::domains::user_domain(ty).is_none() {
            if let Some((text, mode)) = expr_query(e) {
                let body = match mode {
                    3 | 4 => text.split_once(":=").map_or(text, |(_, r)| r).trim(),
                    _ => text,
                };
                return self.eval_typed_body(Some(ty.to_string()), body.to_string(), text, mode);
            }
        }
        let v = self.eval(e)?;
        let bound = expr_query(e).and_then(|(text, mode)| {
            let body = match mode {
                3 | 4 => text.split_once(":=").map_or(text, |(_, r)| r).trim(),
                _ => text,
            };
            self.bind(&format!("SELECT {body}")).ok()
        });
        if let Some((sql, _, types)) = bound {
            if let Some(out) = self.host.coerce_to_domain(ty, &sql, &types, v.clone()) {
                return out;
            }
        }
        self.cast(v, ty)
    }

    fn eval_bool(&self, e: &Value) -> Result<bool, PlError> {
        Ok(matches!(self.eval(e)?, Bson::Boolean(true)))
    }

    fn set_var(&mut self, no: usize, value: Bson) -> Result<(), PlError> {
        match self.datums.get(no) {
            Some(Datum::RecField { field, parent }) => {
                let (field, parent) = (field.clone(), *parent);
                match self.datums.get_mut(parent) {
                    Some(Datum::Rec {
                        rec: Some(rec),
                        name,
                    }) => match rec.columns.iter().position(|(n, _)| *n == field) {
                        Some(i) => {
                            rec.values[i] = value;
                            Ok(())
                        }
                        None => Err(PlError::new(
                            "42703",
                            format!("record \"{name}\" has no field \"{field}\""),
                        )),
                    },
                    Some(Datum::Rec { name, .. }) => Err(PlError::new(
                        "55000",
                        format!("record \"{name}\" is not assigned yet"),
                    )),
                    _ => Err(PlError::new("XX000", "a field of something not a record")),
                }
            }
            Some(Datum::Var { .. }) => {
                if let Some(Datum::Var { value: v, .. }) = self.datums.get_mut(no) {
                    *v = value;
                }
                Ok(())
            }
            _ => Err(PlError::unsupported("this assignment target")),
        }
    }

    /// Assign one result row to an INTO / FOR target (a record or a row).
    /// The datum an `INTO` fills: a record or row variable by number, or an
    /// anonymous row of the variables listed.
    pub(super) fn target_datum(&mut self, body: &Value) -> Result<usize, PlError> {
        let target = body
            .get("target")
            .and_then(|t| t.as_object())
            .and_then(|o| o.values().next())
            .and_then(|v| v.get("dno").and_then(Value::as_u64))
            .map(|n| n as usize);
        Ok(match target {
            Some(t) => t,
            None => {
                // An anonymous row of variables.
                let fields: Vec<usize> = body
                    .get("target")
                    .and_then(|t| t.get("PLpgSQL_row"))
                    .and_then(|r| r.get("fields"))
                    .and_then(Value::as_array)
                    .map(|fs| {
                        fs.iter()
                            .filter_map(|x| x.get("varno").and_then(Value::as_u64))
                            .map(|n| n as usize)
                            .collect()
                    })
                    .unwrap_or_default();
                self.datums.push(Datum::Row { fields });
                self.datums.len() - 1
            }
        })
    }

    /// `domains` names the domain each column is of, where known
    /// ([`Host::output_domains`]). A record takes the row as it is; a
    /// variable, or each variable of a row, is assigned its column's value
    /// by the assignment rule ([`Self::coerce_moved`]).
    fn assign_row(
        &mut self,
        target: usize,
        out: &QueryOut,
        row: Option<&[Bson]>,
        domains: &[Option<String>],
    ) -> Result<(), PlError> {
        let columns = out.columns.as_slice();
        let source = |i: usize| -> String {
            domains
                .get(i)
                .cloned()
                .flatten()
                .or_else(|| columns.get(i).map(|(_, t)| t.clone()))
                .unwrap_or_else(|| "text".to_string())
        };
        match self.datums.get(target) {
            Some(Datum::Rec { .. }) => {
                // No row still gives the record the query's row type, every
                // field NULL (`exec_move_row` over a NULL tuple). Each field
                // is of its column's declared type (`exprType` /
                // `exprTypmod`): the domain a column is of, or the base
                // type at its modifier, so an assignment to the field
                // checks the domain or the length (RP-688).
                let typed = columns
                    .iter()
                    .enumerate()
                    .map(|(i, (name, ty))| {
                        let declared = match domains.get(i).cloned().flatten() {
                            Some(d) => d,
                            None => match out.typmods.get(i) {
                                Some(m) if *m >= 0 => {
                                    secantus_pgplan::scalar::format_type_text_public(ty, Some(*m))
                                }
                                _ => ty.clone(),
                            },
                        };
                        (name.clone(), declared)
                    })
                    .collect();
                let rec = Some(Record {
                    columns: typed,
                    values: row.map_or_else(|| vec![Bson::Null; columns.len()], <[Bson]>::to_vec),
                });
                if let Some(Datum::Rec { rec: slot, .. }) = self.datums.get_mut(target) {
                    *slot = rec;
                }
                Ok(())
            }
            Some(Datum::Row { fields }) => {
                let fields = fields.clone();
                for (i, f) in fields.iter().enumerate() {
                    let v = row.and_then(|r| r.get(i).cloned()).unwrap_or(Bson::Null);
                    let v = self.coerce_moved(*f, v, &source(i))?;
                    self.set_var(*f, v)?;
                }
                Ok(())
            }
            Some(Datum::Var { .. }) => {
                let v = row.and_then(|r| r.first().cloned()).unwrap_or(Bson::Null);
                let v = self.coerce_moved(target, v, &source(0))?;
                self.set_var(target, v)
            }
            _ => Err(PlError::unsupported("this INTO target")),
        }
    }

    fn stmts(&mut self, list: &Value) -> Result<Flow, PlError> {
        for s in list.as_array().cloned().unwrap_or_default() {
            match self.stmt(&s)? {
                Flow::Next => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Next)
    }

    fn label_matches(label: &Option<String>, own: Option<&str>) -> bool {
        match label {
            None => true,
            Some(l) => own == Some(l.as_str()),
        }
    }

    /// Run one statement; an error leaving it gets this function's CONTEXT
    /// frame (`PL/pgSQL function f() line N at RETURN`), plus the
    /// `SQL statement "..."` frame of a statement it ran.
    fn stmt(&mut self, s: &Value) -> Result<Flow, PlError> {
        let Some((kind, body)) = s.as_object().and_then(|o| o.iter().next()) else {
            return Ok(Flow::Next);
        };
        let out = self.stmt_inner(s);
        let Err(mut e) = out else {
            return out;
        };
        if e.framed || kind == "PLpgSQL_stmt_block" {
            return Err(e);
        }
        let text = |key: &str| {
            body.get(key)
                .and_then(|q| expr_query(q))
                .map(|(t, _)| t.to_string())
        };
        let sql_text = match kind.as_str() {
            "PLpgSQL_stmt_execsql" => text("sqlstmt"),
            "PLpgSQL_stmt_perform" => text("expr"),
            "PLpgSQL_stmt_return_query" => text("query").or_else(|| self.dyn_text.take()),
            "PLpgSQL_stmt_fors" => text("query"),
            "PLpgSQL_stmt_dynexecute" | "PLpgSQL_stmt_dynfors" => self.dyn_text.take(),
            _ => None,
        };
        if let Some(t) = sql_text {
            e = e.with_sql_frame(|| Some(format!("SQL statement \"{t}\"")));
        }
        let line = body.get("lineno").and_then(Value::as_u64).unwrap_or(0);
        e.push_context(format!(
            "PL/pgSQL function {} line {line} at {}",
            self.label,
            stmt_typename(kind, body)
        ));
        e.framed = true;
        Err(e)
    }

    /// Enter a block: push its namespace level and give each variable it
    /// declares its default, or NULL, as `exec_stmt_block` does on every
    /// entry -- a block inside a loop starts afresh each pass, and a
    /// default reads the state at that moment. A default is read in the
    /// levels being run plus the names its own level declares before it.
    /// Its error carries the block's `during statement block local variable
    /// initialization` frame.
    fn enter_block(&mut self, body: &Value, level: block_scope::Scope) -> Result<(), PlError> {
        let vars = level.vars.clone();
        self.active.push(block_scope::Scope {
            vars: Vec::new(),
            ..level
        });
        for (name, d) in vars {
            if self.active.last().is_some_and(|l| l.init.contains(&d)) {
                let frame = |mut e: PlError| {
                    if !e.framed {
                        let line = body.get("lineno").and_then(Value::as_u64).unwrap_or(0);
                        e.push_context(format!(
                            "PL/pgSQL function {} line {line} during statement block \
                             local variable initialization",
                            self.label
                        ));
                        e.framed = true;
                    }
                    e
                };
                let value = match self.defaults.get(&d).cloned() {
                    Some(e) => Some(self.eval_typed(&e, d).map_err(frame)?),
                    None => {
                        // With no DEFAULT, a variable of a domain is
                        // assigned its NULL through `exec_assign_value`,
                        // claimed to be of type `unknown` so the coercion
                        // runs: the domain's NOT NULL and CHECKs judge it,
                        // and `x dnn int not null` refuses the block's
                        // entry (RP-714).
                        if let Some(Datum::Var { ty, .. }) = self.datums.get(d) {
                            if let Some(checked) =
                                self.host
                                    .coerce_to_domain(ty, "SELECT NULL", &[], Bson::Null)
                            {
                                checked.map_err(frame)?;
                            }
                        }
                        None
                    }
                };
                match (self.datums.get_mut(d), value) {
                    (Some(Datum::Rec { rec, .. }), _) => *rec = None,
                    (Some(Datum::Var { value: slot, .. }), None) => *slot = Bson::Null,
                    (Some(Datum::Var { .. }), Some(v)) => self.set_var(d, v)?,
                    _ => {}
                }
            }
            if let Some(own) = self.active.last_mut() {
                own.vars.push((name, d));
            }
        }
        Ok(())
    }

    /// Run a block's statements; with EXCEPTION handlers, as a
    /// subtransaction whose handler runs for an error it catches.
    fn block(&mut self, body: &Value, label: Option<String>) -> Result<Flow, PlError> {
        let handlers = body
            .get("exceptions")
            .and_then(|e| e.get("PLpgSQL_exception_block"))
            .and_then(|b| b.get("exc_list"))
            .and_then(Value::as_array)
            .cloned();
        // A block with handlers runs as a subtransaction: an error it
        // catches undoes the block's writes, as PostgreSQL has it.
        let token = match handlers {
            Some(_) => {
                self.subtxn_depth += 1;
                match self.host.subtxn_begin() {
                    Ok(t) => Some(t),
                    Err(e) => {
                        self.subtxn_depth -= 1;
                        return Err(e);
                    }
                }
            }
            None => None,
        };
        let result = self.stmts(body.get("body").unwrap_or(&Value::Null));
        if let Some(t) = token {
            self.subtxn_depth -= 1;
            self.host.subtxn_end(t, result.is_err())?;
        }
        let Some(handlers) = handlers else {
            return match result? {
                Flow::Exit(Some(l)) if label.as_deref() == Some(l.as_str()) => Ok(Flow::Next),
                other => Ok(other),
            };
        };
        match result {
            Ok(flow) => Ok(flow),
            Err(err) => {
                for h in &handlers {
                    let h = h.get("PLpgSQL_exception").cloned().unwrap_or(Value::Null);
                    let conds = h
                        .get("conditions")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    if conds.iter().any(|c| condition_matches(c, &err.sqlstate)) {
                        // SQLSTATE and SQLERRM are a level of their own,
                        // which the handler runs in.
                        let depth = self.active.len();
                        if let Some(level) =
                            block_scope::scope_of(body).and_then(|k| self.handler_level(k))
                        {
                            self.active.push(level);
                        }
                        self.set_named("sqlstate", Bson::String(err.sqlstate.clone()));
                        self.set_named("sqlerrm", Bson::String(err.message.clone()));
                        let previous = self.handling.replace(err.clone());
                        let flow = self.stmts(h.get("action").unwrap_or(&Value::Null));
                        self.handling = previous;
                        self.active.truncate(depth);
                        return flow;
                    }
                }
                Err(err)
            }
        }
    }

    fn stmt_inner(&mut self, s: &Value) -> Result<Flow, PlError> {
        let Some((kind, body)) = s.as_object().and_then(|o| o.iter().next()) else {
            return Ok(Flow::Next);
        };
        let label = body
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_string);
        match kind.as_str() {
            "PLpgSQL_stmt_block" => {
                // The block's level stays active through its handlers; a
                // level an error left behind inside it goes with it.
                let depth = self.active.len();
                let level = block_scope::scope_of(body).and_then(|k| self.scopes.get(k).cloned());
                let out = match level {
                    Some(level) => self
                        .enter_block(body, level)
                        .and_then(|()| self.block(body, label)),
                    None => self.block(body, label),
                };
                self.active.truncate(depth);
                out
            }
            "PLpgSQL_stmt_assign" => {
                let no = body.get("varno").and_then(Value::as_u64).unwrap_or(0) as usize;
                let expr = body.get("expr").cloned().unwrap_or(Value::Null);
                if let Some(v) = self.assign_subscripted(no, &expr)? {
                    self.set_var(no, v)?;
                    return Ok(Flow::Next);
                }
                let v = match self.datums.get(no) {
                    Some(Datum::RecField { field, parent }) => {
                        // Cast to the field's type, as an assignment would.
                        let ty = match self.datums.get(*parent) {
                            Some(Datum::Rec { rec: Some(r), .. }) => {
                                r.get(field).map(|(_, t)| t.to_string())
                            }
                            _ => None,
                        };
                        match ty {
                            Some(t) => self.assign_field(&expr, &t)?,
                            None => self.eval(&expr)?,
                        }
                    }
                    _ => self.eval_typed(&expr, no)?,
                };
                self.set_var(no, v)?;
                Ok(Flow::Next)
            }
            "PLpgSQL_stmt_if" => {
                if self.eval_bool(body.get("cond").unwrap_or(&Value::Null))? {
                    return self.stmts(body.get("then_body").unwrap_or(&Value::Null));
                }
                for e in body
                    .get("elsif_list")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                {
                    let e = e.get("PLpgSQL_if_elsif").cloned().unwrap_or(Value::Null);
                    if self.eval_bool(e.get("cond").unwrap_or(&Value::Null))? {
                        return self.stmts(e.get("stmts").unwrap_or(&Value::Null));
                    }
                }
                self.stmts(body.get("else_body").unwrap_or(&Value::Null))
            }
            "PLpgSQL_stmt_case" => {
                let subject = match body.get("t_expr") {
                    Some(e) => Some(self.eval(e)?),
                    None => None,
                };
                let subject_no = body
                    .get("t_varno")
                    .and_then(Value::as_u64)
                    .map(|n| n as usize);
                if let (Some(v), Some(no)) = (subject.clone(), subject_no) {
                    self.set_var(no, v)?;
                }
                for w in body
                    .get("case_when_list")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                {
                    let w = w.get("PLpgSQL_case_when").cloned().unwrap_or(Value::Null);
                    if self.eval_bool(w.get("expr").unwrap_or(&Value::Null))? {
                        return self.stmts(w.get("stmts").unwrap_or(&Value::Null));
                    }
                }
                if body
                    .get("have_else")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    return self.stmts(body.get("else_stmts").unwrap_or(&Value::Null));
                }
                Err(PlError::new("20000", "case not found"))
            }
            "PLpgSQL_stmt_loop" | "PLpgSQL_stmt_while" => loop {
                if kind == "PLpgSQL_stmt_while"
                    && !self.eval_bool(body.get("cond").unwrap_or(&Value::Null))?
                {
                    return Ok(Flow::Next);
                }
                match self.stmts(body.get("body").unwrap_or(&Value::Null))? {
                    Flow::Exit(l) if Self::label_matches(&l, label.as_deref()) => {
                        return Ok(Flow::Next)
                    }
                    Flow::Continue(l) if Self::label_matches(&l, label.as_deref()) => {}
                    Flow::Next => {}
                    other => return Ok(other),
                }
            },
            "PLpgSQL_stmt_exit" => {
                let is_exit = body
                    .get("is_exit")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if let Some(c) = body.get("cond") {
                    if !self.eval_bool(c)? {
                        return Ok(Flow::Next);
                    }
                }
                Ok(if is_exit {
                    Flow::Exit(label)
                } else {
                    Flow::Continue(label)
                })
            }
            "PLpgSQL_stmt_fori" => {
                let var = body
                    .get("var")
                    .and_then(|v| v.get("PLpgSQL_var"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let name = var
                    .get("refname")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let to_i = |v: Bson| -> Result<i64, PlError> {
                    match v {
                        Bson::Int32(i) => Ok(i64::from(i)),
                        Bson::Int64(i) => Ok(i),
                        Bson::Null => Err(PlError::new(
                            "22004",
                            "lower bound of FOR loop cannot be null",
                        )),
                        other => Err(PlError::new(
                            "42804",
                            format!("FOR loop bound {other} is not an integer"),
                        )),
                    }
                };
                let lower = to_i(self.eval(body.get("lower").unwrap_or(&Value::Null))?)?;
                let upper = to_i(self.eval(body.get("upper").unwrap_or(&Value::Null))?)?;
                let step = match body.get("step") {
                    Some(e) => to_i(self.eval(e)?)?,
                    None => 1,
                };
                if step <= 0 {
                    return Err(PlError::new(
                        "22023",
                        "BY value of FOR loop must be greater than zero",
                    ));
                }
                let reverse = body
                    .get("reverse")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.datums.push(Datum::Var {
                    name: name.clone(),
                    ty: "int4".into(),
                    value: Bson::Null,
                });
                let slot = self.datums.len() - 1;
                // The loop's own level: its label qualifies the variable.
                let depth = self.active.len();
                self.active.push(block_scope::Scope {
                    label: label.clone(),
                    vars: vec![(name, slot)],
                    parent: None,
                    handler: Vec::new(),
                    init: Vec::new(),
                });
                let mut i = lower;
                let result = loop {
                    if (!reverse && i > upper) || (reverse && i < upper) {
                        break Ok(Flow::Next);
                    }
                    if let Some(Datum::Var { value, .. }) = self.datums.get_mut(slot) {
                        *value = Bson::Int32(i32::try_from(i).unwrap_or(i32::MAX));
                    }
                    let flow = match self.stmts(body.get("body").unwrap_or(&Value::Null)) {
                        Ok(flow) => flow,
                        Err(e) => break Err(e),
                    };
                    match flow {
                        Flow::Exit(l) if Self::label_matches(&l, label.as_deref()) => {
                            break Ok(Flow::Next)
                        }
                        Flow::Continue(l) if Self::label_matches(&l, label.as_deref()) => {}
                        Flow::Next => {}
                        other => break Ok(other),
                    }
                    i = if reverse { i - step } else { i + step };
                };
                self.active.truncate(depth);
                self.datums.truncate(slot);
                result
            }
            "PLpgSQL_stmt_fors" | "PLpgSQL_stmt_dynfors" => {
                let target = body
                    .get("var")
                    .and_then(|v| v.as_object())
                    .and_then(|o| o.values().next())
                    .and_then(|v| v.get("dno"))
                    .and_then(Value::as_u64)
                    .map(|n| n as usize)
                    .ok_or_else(|| PlError::unsupported("this FOR target"))?;
                let (out, domains) = if kind == "PLpgSQL_stmt_dynfors" {
                    let sql = self.dynamic_sql(body.get("query").unwrap_or(&Value::Null))?;
                    let out = self.host.query(&sql, &[], &[])?;
                    (out, self.host.output_domains(&sql, &[]))
                } else {
                    let (text, _) = expr_query(body.get("query").unwrap_or(&Value::Null))
                        .ok_or_else(|| PlError::new("XX000", "a FOR without a query"))?;
                    let (sql, params, types) = self.bind(text)?;
                    let out = self.host.query(&sql, &params, &types)?;
                    (out, self.host.output_domains(&sql, &types))
                };
                self.set_found(!out.rows.is_empty());
                for row in &out.rows {
                    self.assign_row(target, &out, Some(row), &domains)?;
                    match self.stmts(body.get("body").unwrap_or(&Value::Null))? {
                        Flow::Exit(l) if Self::label_matches(&l, label.as_deref()) => break,
                        Flow::Continue(l) if Self::label_matches(&l, label.as_deref()) => {}
                        Flow::Next => {}
                        other => return Ok(other),
                    }
                }
                Ok(Flow::Next)
            }
            "PLpgSQL_stmt_foreach_a" => {
                let no = body.get("varno").and_then(Value::as_u64).unwrap_or(0) as usize;
                let items = match self.eval(body.get("expr").unwrap_or(&Value::Null))? {
                    Bson::Array(items) => items,
                    Bson::Null => {
                        return Err(PlError::new("22004", "FOREACH expression must not be null"))
                    }
                    _ => {
                        return Err(PlError::new(
                            "42804",
                            "FOREACH expression must yield an array, not type text",
                        ))
                    }
                };
                for item in items {
                    self.set_var(no, item)?;
                    match self.stmts(body.get("body").unwrap_or(&Value::Null))? {
                        Flow::Exit(l) if Self::label_matches(&l, label.as_deref()) => break,
                        Flow::Continue(l) if Self::label_matches(&l, label.as_deref()) => {}
                        Flow::Next => {}
                        other => return Ok(other),
                    }
                }
                Ok(Flow::Next)
            }
            "PLpgSQL_stmt_return" => {
                if self.returns_set {
                    return Ok(Flow::Return(Outcome::Rows(std::mem::take(
                        &mut self.set_rows,
                    ))));
                }
                let Some(expr) = body.get("expr") else {
                    if let Some(no) = body.get("retvarno").and_then(Value::as_u64) {
                        if let Some(Datum::Rec { rec, .. }) = self.datums.get(no as usize) {
                            return Ok(Flow::Return(Outcome::Record(rec.clone())));
                        }
                    }
                    if let Some(v) = self.out_value() {
                        return Ok(Flow::Return(Outcome::Value(v)));
                    }
                    return Ok(Flow::Return(Outcome::Value(Bson::Null)));
                };
                if self.trigger.is_some() {
                    let (text, _) = expr_query(expr).unwrap_or(("NULL", 2));
                    let word = text.trim().to_ascii_lowercase();
                    return Ok(Flow::Return(Outcome::Record(match word.as_str() {
                        "null" => None,
                        name => self.record(name).cloned(),
                    })));
                }
                Ok(Flow::Return(Outcome::Value(self.eval(expr)?)))
            }
            "PLpgSQL_stmt_return_next" => {
                if let Some(expr) = body.get("expr") {
                    let bare = expr_query(expr).map(|(q, _)| {
                        q.trim()
                            .trim_start_matches('(')
                            .trim_end_matches(')')
                            .trim()
                            .to_string()
                    });
                    if let Some(r) = bare.as_deref().and_then(|name| self.record(name)).cloned() {
                        self.set_rows.push(r.values);
                        return Ok(Flow::Next);
                    }
                    let v = self.eval(expr)?;
                    self.set_rows.push(vec![v]);
                } else if let Some(no) = body.get("retvarno").and_then(Value::as_u64) {
                    match self.datums.get(no as usize) {
                        Some(Datum::Rec { rec: Some(r), .. }) => {
                            self.set_rows.push(r.values.clone())
                        }
                        Some(Datum::Var { value, .. }) => self.set_rows.push(vec![value.clone()]),
                        _ => {}
                    }
                }
                Ok(Flow::Next)
            }
            "PLpgSQL_stmt_return_query" => {
                let out = match body.get("query") {
                    Some(q) => {
                        let (text, _) = expr_query(q)
                            .ok_or_else(|| PlError::new("XX000", "RETURN QUERY without a query"))?;
                        let (sql, params, types) = self.bind(text)?;
                        self.host.query(&sql, &params, &types)?
                    }
                    None => {
                        let sql = self.dynamic_sql(body.get("dynquery").unwrap_or(&Value::Null))?;
                        self.dyn_text = Some(sql.clone());
                        self.host.query(&sql, &[], &[])?
                    }
                };
                self.set_found(!out.rows.is_empty());
                self.row_count = out.rows.len() as u64;
                self.set_rows.extend(out.rows);
                Ok(Flow::Next)
            }
            "PLpgSQL_stmt_raise" => self.raise(body),
            "PLpgSQL_stmt_open" => self.open_cursor(body),
            "PLpgSQL_stmt_forc" => self.for_cursor(body, label.as_deref()),
            "PLpgSQL_stmt_fetch" => self.fetch_cursor(body),
            "PLpgSQL_stmt_close" => self.close_cursor(body),
            "PLpgSQL_stmt_perform" => {
                let (text, _) = expr_query(body.get("expr").unwrap_or(&Value::Null))
                    .ok_or_else(|| PlError::new("XX000", "PERFORM without a query"))?;
                let (sql, params, types) = self.bind(text)?;
                let out = self.host.query(&sql, &params, &types)?;
                self.set_found(!out.rows.is_empty());
                self.row_count = out.rows.len() as u64;
                Ok(Flow::Next)
            }
            "PLpgSQL_stmt_execsql" => {
                let (text, _) = expr_query(body.get("sqlstmt").unwrap_or(&Value::Null))
                    .ok_or_else(|| PlError::new("XX000", "a SQL statement without text"))?;
                let into = body.get("into").and_then(Value::as_bool).unwrap_or(false);
                let strict = body.get("strict").and_then(Value::as_bool).unwrap_or(false);
                let (sql, params, types) = self.bind(text)?;
                let head = sql.trim_start().to_ascii_lowercase();
                let is_query = head.starts_with("select")
                    || head.starts_with("with")
                    || head.starts_with("values");
                if into {
                    let out = if is_query {
                        self.host.query(&sql, &params, &types)?
                    } else {
                        // A write with `RETURNING ... INTO`: its returned
                        // row fills the target, as a SELECT INTO's would.
                        self.host.returning(&sql, &params, &types)?
                    };
                    if strict && out.rows.is_empty() {
                        return Err(PlError::new("P0002", "query returned no rows"));
                    }
                    if strict && out.rows.len() > 1 {
                        return Err(PlError::new("P0003", "query returned more than one row"));
                    }
                    let target = self.target_datum(body)?;
                    let first = out.rows.first().map(|r| r.as_slice());
                    let domains = if is_query {
                        self.host.output_domains(&sql, &types)
                    } else {
                        Vec::new()
                    };
                    self.assign_row(target, &out, first, &domains)?;
                    self.set_found(!out.rows.is_empty());
                    self.row_count = out.rows.len().min(1) as u64;
                    return Ok(Flow::Next);
                }
                if is_query {
                    let out = self.host.query(&sql, &params, &types)?;
                    self.set_found(!out.rows.is_empty());
                    self.row_count = out.rows.len() as u64;
                } else {
                    let n = self.host.execute(&sql, &params, &types)?;
                    self.set_found(n > 0);
                    self.row_count = n;
                }
                Ok(Flow::Next)
            }
            "PLpgSQL_stmt_dynexecute" => {
                let sql = self.dynamic_sql(body.get("query").unwrap_or(&Value::Null))?;
                self.dyn_text = Some(sql.clone());
                let using: Vec<Bson> = body
                    .get("params")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|p| self.eval(p))
                    .collect::<Result<_, _>>()?;
                let types: Vec<String> = Vec::new();
                if body.get("into").and_then(Value::as_bool).unwrap_or(false) {
                    let out = self.host.query(&sql, &using, &types)?;
                    let target = body
                        .get("target")
                        .and_then(|t| t.as_object())
                        .and_then(|o| o.values().next())
                        .and_then(|v| v.get("dno").and_then(Value::as_u64))
                        .map(|n| n as usize)
                        .ok_or_else(|| PlError::unsupported("this EXECUTE INTO target"))?;
                    let first = out.rows.first().map(|r| r.as_slice());
                    let domains = self.host.output_domains(&sql, &types);
                    self.assign_row(target, &out, first, &domains)?;
                    self.set_found(!out.rows.is_empty());
                } else {
                    let head = sql.trim_start().to_ascii_lowercase();
                    if head.starts_with("select") || head.starts_with("with") {
                        let out = self.host.query(&sql, &using, &types)?;
                        self.row_count = out.rows.len() as u64;
                    } else {
                        self.row_count = self.host.execute(&sql, &using, &types)?;
                    }
                }
                Ok(Flow::Next)
            }
            "PLpgSQL_stmt_getdiag" => {
                for item in body
                    .get("diag_items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                {
                    let item = item
                        .get("PLpgSQL_diag_item")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let no = item.get("target").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let kind = item.get("kind").and_then(Value::as_u64).unwrap_or(0);
                    let v = match kind {
                        // PLPGSQL_GETDIAG_ROW_COUNT
                        0 => Bson::Int64(self.row_count as i64),
                        _ => return Err(PlError::unsupported("this GET DIAGNOSTICS item")),
                    };
                    let v = match self.datums.get(no) {
                        Some(Datum::Var { ty, .. }) => {
                            let ty = ty.clone();
                            self.cast(v, &ty)?
                        }
                        _ => v,
                    };
                    self.set_var(no, v)?;
                }
                Ok(Flow::Next)
            }
            "PLpgSQL_stmt_assert" => {
                if !self.eval_bool(body.get("cond").unwrap_or(&Value::Null))? {
                    let msg = match body.get("message") {
                        Some(m) => secantus_pgplan::value_text(&self.eval(m)?),
                        None => "assertion failed".into(),
                    };
                    return Err(PlError::new("P0004", msg));
                }
                Ok(Flow::Next)
            }
            "PLpgSQL_stmt_call" => {
                let (text, _) = expr_query(body.get("expr").unwrap_or(&Value::Null))
                    .ok_or_else(|| PlError::new("XX000", "a CALL without text"))?;
                let (sql, params, types) = self.bind(text)?;
                // A nested `DO` parses to the same statement.
                if !body.get("is_call").and_then(Value::as_bool).unwrap_or(true) {
                    self.host.execute(&sql, &params, &types)?;
                    return Ok(Flow::Next);
                }
                let (outputs, values) = self.host.call(&sql, &params, &types)?;
                // An OUTPUT parameter's value goes back into the variable
                // passed for it, which must be one.
                let targets = call_arguments(text);
                for (k, (pos, param)) in outputs.iter().enumerate() {
                    let Some(Some(var)) = targets.get(*pos) else {
                        return Err(PlError::new(
                            "42601",
                            format!(
                                "procedure parameter \"{param}\" is an output parameter \
                                 but corresponding argument is not writable"
                            ),
                        ));
                    };
                    let value = values.get(k).cloned().unwrap_or(Bson::Null);
                    let ty = match self.named(var).and_then(|d| self.datums.get(d)) {
                        Some(Datum::Var { ty, .. }) => Some(ty.clone()),
                        _ => None,
                    };
                    let value = match (ty, &value) {
                        (_, Bson::Null) | (None, _) => value,
                        (Some(t), _) => self.cast(value, &t)?,
                    };
                    self.set_named(var, value);
                }
                Ok(Flow::Next)
            }
            "PLpgSQL_stmt_commit" | "PLpgSQL_stmt_rollback" => {
                let commit = kind == "PLpgSQL_stmt_commit";
                if !self.nonatomic {
                    return Err(PlError::new("2D000", "invalid transaction termination"));
                }
                if self.subtxn_depth > 0 {
                    return Err(PlError::new(
                        "2D000",
                        if commit {
                            "cannot commit while a subtransaction is active"
                        } else {
                            "cannot roll back while a subtransaction is active"
                        },
                    ));
                }
                let chain = body.get("chain").and_then(Value::as_bool).unwrap_or(false);
                self.host.end_transaction(commit, chain)?;
                Ok(Flow::Next)
            }
            other => Err(PlError::unsupported(&format!(
                "the statement {}",
                other.trim_start_matches("PLpgSQL_stmt_")
            ))),
        }
    }

    /// Set the variable a bare `name` names where the statement runs.
    fn set_named(&mut self, name: &str, value: Bson) {
        if let Some(Datum::Var { value: v, .. }) =
            self.named(name).and_then(|d| self.datums.get_mut(d))
        {
            *v = value;
        }
    }

    fn cast(&self, v: Bson, ty: &str) -> Result<Bson, PlError> {
        let out = self.host.query(
            &format!("SELECT $1::{}", sql_type(ty)),
            &[v],
            &[ty.to_string()],
        )?;
        Ok(out
            .rows
            .into_iter()
            .next()
            .and_then(|r| r.into_iter().next())
            .unwrap_or(Bson::Null))
    }

    fn dynamic_sql(&self, e: &Value) -> Result<String, PlError> {
        match self.eval(e)? {
            Bson::String(s) => Ok(s),
            Bson::Null => Err(PlError::new(
                "22004",
                "query string argument of EXECUTE is null",
            )),
            other => Ok(secantus_pgplan::value_text(&other)),
        }
    }

    fn raise(&mut self, body: &Value) -> Result<Flow, PlError> {
        let level = body.get("elog_level").and_then(Value::as_u64).unwrap_or(21);
        let message = body.get("message").and_then(Value::as_str);
        if message.is_none() && body.get("condname").is_none() && body.get("options").is_none() {
            // `RAISE;` re-raises the error a handler is running for.
            return Err(self.handling.clone().unwrap_or_else(|| {
                PlError::new(
                    "0Z002",
                    "RAISE without parameters cannot be used outside an exception handler",
                )
            }));
        }
        let params: Vec<String> = body
            .get("params")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|p| {
                // As the type's output function writes it: `boolout`'s
                // `t` / `f`, not a text cast's `true`.
                self.eval(p).map(|v| match v {
                    Bson::Null => "<NULL>".to_string(),
                    Bson::Boolean(b) => if b { "t" } else { "f" }.to_string(),
                    other => secantus_pgplan::value_text(&other),
                })
            })
            .collect::<Result<_, _>>()?;
        let mut text = String::new();
        let mut args = params.into_iter();
        let mut chars = message.unwrap_or_default().chars().peekable();
        while let Some(c) = chars.next() {
            if c == '%' {
                if chars.peek() == Some(&'%') {
                    chars.next();
                    text.push('%');
                } else {
                    text.push_str(&args.next().unwrap_or_default());
                }
            } else {
                text.push(c);
            }
        }
        // `exec_stmt_raise`: the condition, then the options, each given
        // once; a condition's name is the message when nothing else is.
        let already = |name: &str| {
            Err(PlError::new(
                "42601",
                format!("RAISE option already specified: {name}"),
            ))
        };
        let mut err_code: Option<String> = None;
        let mut condname: Option<String> = None;
        let mut err_message = message.map(|_| text);
        if let Some(cond) = body.get("condname").and_then(Value::as_str) {
            // `RAISE SQLSTATE '23503'` stores the code itself, which
            // `plpgsql_recognize_err_condition` takes as one.
            err_code = condition_sqlstate(&cond.to_ascii_lowercase())
                .map(str::to_string)
                .or_else(|| is_sqlstate_text(cond).then(|| cond.to_string()));
            condname = Some(cond.to_string());
        }
        let mut detail = None;
        let mut hint = None;
        for o in body
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let o = o
                .get("PLpgSQL_raise_option")
                .cloned()
                .unwrap_or(Value::Null);
            let v = secantus_pgplan::value_text(&self.eval(o.get("expr").unwrap_or(&Value::Null))?);
            match o.get("opt_type").and_then(Value::as_u64).unwrap_or(0) {
                0 => {
                    if err_code.is_some() {
                        return already("ERRCODE");
                    }
                    if is_sqlstate_text(&v) {
                        err_code = Some(v);
                    } else {
                        err_code = Some(
                            condition_sqlstate(&v.to_ascii_lowercase())
                                .map(str::to_string)
                                .unwrap_or_else(|| v.clone()),
                        );
                        condname = Some(v);
                    }
                }
                1 => {
                    if err_message.is_some() {
                        return already("MESSAGE");
                    }
                    err_message = Some(v);
                }
                2 => {
                    if detail.is_some() {
                        return already("DETAIL");
                    }
                    detail = Some(v);
                }
                3 => {
                    if hint.is_some() {
                        return already("HINT");
                    }
                    hint = Some(v);
                }
                _ => {}
            }
        }
        let code = err_code.unwrap_or_else(|| if level >= 21 { "P0001" } else { "00000" }.into());
        let message = err_message.or(condname).unwrap_or_else(|| code.clone());
        let mut err = PlError::new(&code, message);
        if level == 19 && err.sqlstate == "00000" {
            err.sqlstate = "01000".into();
        }
        err.detail = detail;
        err.hint = hint;
        if level >= 21 {
            return Err(err);
        }
        let severity = match level {
            17 => "INFO",
            18 => "NOTICE",
            19 => "WARNING",
            _ => return Ok(Flow::Next),
        };
        self.host.notice(severity, &err.sqlstate, err.message);
        Ok(Flow::Next)
    }
}

/// Five digits or upper-case letters: an SQLSTATE written as itself.
fn is_sqlstate_text(s: &str) -> bool {
    s.len() == 5
        && s.bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
}

/// A type name as a cast can write it: an array type keeps its brackets, and
/// a name PostgreSQL would need quoted is not produced by `canonical_type`.
/// The type an assignment reads its value as before fitting it to a
/// length modifier ([`Host::assignment_length`]'s base): a bare `bit` is
/// the grammar's `bit(1)`, so a bit string -- or an array of them -- is read
/// as `varbit`, at its own length, for the fit to measure (RP-687, RP-695).
fn length_read_type(base: &str) -> String {
    match base {
        "bit" => "varbit".to_string(),
        "bit[]" => "varbit[]".to_string(),
        other => other.to_string(),
    }
}

fn sql_type(ty: &str) -> String {
    ty.to_string()
}

/// A `CALL`'s arguments as written: the variable each names (a named
/// argument's value), when it is a bare name (the only kind an OUTPUT
/// parameter can write back to).
fn call_arguments(text: &str) -> Vec<Option<String>> {
    use pg_query::protobuf::node::Node as N;
    let Ok(parsed) = pg_query::parse(text) else {
        return Vec::new();
    };
    let Some(N::CallStmt(c)) = parsed
        .protobuf
        .stmts
        .first()
        .and_then(|s| s.stmt.as_ref())
        .and_then(|s| s.node.as_ref())
    else {
        return Vec::new();
    };
    let Some(f) = c.funccall.as_ref() else {
        return Vec::new();
    };
    f.args
        .iter()
        .map(|a| match a.node.as_ref() {
            Some(N::NamedArgExpr(na)) => na.arg.as_deref(),
            _ => Some(a),
        })
        .map(|a| match a.and_then(|a| a.node.as_ref()) {
            Some(N::ColumnRef(r)) if r.fields.len() == 1 => match r.fields[0].node.as_ref() {
                Some(N::String(s)) => Some(s.sval.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod rp573_declarations {
    use super::*;

    /// The variables a body declares, with their type text as written and
    /// their line; a routine's parameters and the compiler's own variables
    /// are not among them (RP-573).
    #[test]
    fn declared_types_are_the_bodys() {
        let sql = "create function f573(a int) returns int language plpgsql as $$\n\
                   declare x nosuch573[]; \"Y\" constant t573%rowtype := 1;\n \
                   c cursor (q nosuch) for select 1;\n\
                   begin declare v s.t; begin for i in 1..2 loop null; end loop; return 1; \
                   exception when others then return 2; end; end $$";
        let got: Vec<(usize, String, String)> = declared_types(sql)
            .into_iter()
            .map(|d| (d.lineno, d.refname, d.typname))
            .collect();
        let want = [
            (2, "x", "nosuch573[]"),
            (2, "Y", "t573%rowtype "),
            (3, "q", "nosuch"),
            (4, "v", "s.t"),
        ];
        assert_eq!(got, want.map(|(l, n, t)| (l, n.to_string(), t.to_string())));
    }

    /// A declared type is found after its variable's name, past a
    /// `CONSTANT`, from the start of its line.
    #[test]
    fn a_declared_type_is_found_after_its_name() {
        let d = |lineno: usize, refname: &str, typname: &str| Declared {
            datum: 0,
            lineno,
            refname: refname.into(),
            typname: typname.into(),
        };
        let body = " declare nosuch int; x nosuch; \"Y\" CONSTANT nosuch := 1;\n  c cursor (a nosuch) for select 1;";
        assert_eq!(declared_type_offset(body, &d(1, "x", "nosuch")), Some(23));
        assert_eq!(declared_type_offset(body, &d(1, "Y", "nosuch ")), Some(44));
        assert_eq!(declared_type_offset(body, &d(2, "a", "nosuch")), Some(71));
        assert_eq!(declared_type_offset(body, &d(1, "z", "nosuch")), None);
    }
}
