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
}

impl PlError {
    pub fn new(sqlstate: &str, message: impl Into<String>) -> Self {
        PlError {
            sqlstate: sqlstate.into(),
            message: message.into(),
            detail: None,
            hint: None,
        }
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
}

/// What the interpreter asks of the server.
mod open_cursor;

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

struct Interp<'a> {
    datums: Vec<Datum>,
    /// The datum of each argument, in declared order, for `$n`.
    arg_slots: Vec<usize>,
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
    /// The error a handler is running for: `RAISE;` re-raises it.
    handling: Option<PlError>,
    /// Bound cursors' queries, by datum number (`open_cursor`).
    cursor_exprs: HashMap<usize, String>,
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
    let f = v
        .get(0)
        .and_then(|f| f.get("PLpgSQL_function"))
        .cloned()
        .ok_or_else(|| PlError::new("42601", "the function body did not parse"))?;
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(create_sql.to_string(), f.clone());
    Ok(f)
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
    let mut interp = Interp {
        datums,
        arg_slots,
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
        handling: None,
        cursor_exprs: open_cursor::cursor_exprs(&f),
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
    // Declared defaults, in declaration order: `x int := a * 2` may read a
    // parameter or an earlier variable.
    let defaults: Vec<(usize, Value)> = f
        .get("datums")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .filter_map(|(i, d)| {
            d.get("PLpgSQL_var")
                .and_then(|v| v.get("default_val"))
                .map(|e| (i, e.clone()))
        })
        .collect();
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
                        }
                    }
                } else if next != Some("(") {
                    if let Some((v, ty)) = self.scalar(&name) {
                        bound = Some((v, ty, i));
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

    fn scalar(&self, name: &str) -> Option<(Bson, String)> {
        // The TG_ variables come first: PL/pgSQL declares them as datums of
        // a trigger function, which would otherwise shadow them with NULL.
        if let Some(v) = self.trigger_scalar(name) {
            return Some(v);
        }
        for d in self.datums.iter().rev() {
            if let Datum::Var { name: n, ty, value } = d {
                if n == name {
                    return Some((value.clone(), ty.clone()));
                }
            }
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
        self.datums.iter().rev().find_map(|d| match d {
            Datum::Rec { name: n, rec } if n == name => rec.as_ref(),
            _ => None,
        })
    }

    /// The type of `field` in a trigger's NEW or OLD that this operation
    /// leaves unassigned, read off the one it does assign.
    fn unassigned_trigger_field(&self, name: &str, field: &str) -> Option<String> {
        let t = self.trigger.as_ref()?;
        let other = match name {
            "new" if t.new.is_none() => t.old.as_ref(),
            "old" if t.old.is_none() => t.new.as_ref(),
            _ => None,
        }?;
        other.get(field).map(|(_, ty)| ty.to_string())
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
        let sql = if mode == 0 {
            body
        } else {
            format!("SELECT {body}")
        };
        let (sql, params, types) = self.bind(&sql)?;
        let out = self.host.query(&sql, &params, &types)?;
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

    /// Evaluate an expression cast to datum `no`'s declared type.
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
        let sql = match ty {
            Some(ty) => format!("SELECT ({body})::{}", sql_type(&ty)),
            None => format!("SELECT {body}"),
        };
        let (sql, params, types) = self.bind(&sql)?;
        let out = self.host.query(&sql, &params, &types)?;
        Ok(out
            .rows
            .into_iter()
            .next()
            .and_then(|r| r.into_iter().next())
            .unwrap_or(Bson::Null))
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
    fn assign_row(
        &mut self,
        target: usize,
        columns: &[(String, String)],
        row: Option<&[Bson]>,
    ) -> Result<(), PlError> {
        match self.datums.get(target) {
            Some(Datum::Rec { .. }) => {
                let rec = row.map(|r| Record {
                    columns: columns.to_vec(),
                    values: r.to_vec(),
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
                    self.set_var(*f, v)?;
                }
                Ok(())
            }
            Some(Datum::Var { .. }) => {
                let v = row.and_then(|r| r.first().cloned()).unwrap_or(Bson::Null);
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

    fn stmt(&mut self, s: &Value) -> Result<Flow, PlError> {
        let Some((kind, body)) = s.as_object().and_then(|o| o.iter().next()) else {
            return Ok(Flow::Next);
        };
        let label = body
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_string);
        match kind.as_str() {
            "PLpgSQL_stmt_block" => {
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
                        Flow::Exit(Some(l)) if label.as_deref() == Some(l.as_str()) => {
                            Ok(Flow::Next)
                        }
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
                                self.set_named("sqlstate", Bson::String(err.sqlstate.clone()));
                                self.set_named("sqlerrm", Bson::String(err.message.clone()));
                                let previous = self.handling.replace(err.clone());
                                let flow = self.stmts(h.get("action").unwrap_or(&Value::Null));
                                self.handling = previous;
                                return flow;
                            }
                        }
                        Err(err)
                    }
                }
            }
            "PLpgSQL_stmt_assign" => {
                let no = body.get("varno").and_then(Value::as_u64).unwrap_or(0) as usize;
                let expr = body.get("expr").cloned().unwrap_or(Value::Null);
                let v = match self.datums.get(no) {
                    Some(Datum::RecField { field, parent }) => {
                        // Cast to the field's type, as an assignment would.
                        let ty = match self.datums.get(*parent) {
                            Some(Datum::Rec { rec: Some(r), .. }) => {
                                r.get(field).map(|(_, t)| t.to_string())
                            }
                            _ => None,
                        };
                        let v = self.eval(&expr)?;
                        match ty {
                            Some(t) => self.cast(v, &t)?,
                            None => v,
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
                    name,
                    ty: "int4".into(),
                    value: Bson::Null,
                });
                let slot = self.datums.len() - 1;
                let mut i = lower;
                let result = loop {
                    if (!reverse && i > upper) || (reverse && i < upper) {
                        break Ok(Flow::Next);
                    }
                    if let Some(Datum::Var { value, .. }) = self.datums.get_mut(slot) {
                        *value = Bson::Int32(i32::try_from(i).unwrap_or(i32::MAX));
                    }
                    match self.stmts(body.get("body").unwrap_or(&Value::Null))? {
                        Flow::Exit(l) if Self::label_matches(&l, label.as_deref()) => {
                            break Ok(Flow::Next)
                        }
                        Flow::Continue(l) if Self::label_matches(&l, label.as_deref()) => {}
                        Flow::Next => {}
                        other => break Ok(other),
                    }
                    i = if reverse { i - step } else { i + step };
                };
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
                let out = if kind == "PLpgSQL_stmt_dynfors" {
                    let sql = self.dynamic_sql(body.get("query").unwrap_or(&Value::Null))?;
                    self.host.query(&sql, &[], &[])?
                } else {
                    let (text, _) = expr_query(body.get("query").unwrap_or(&Value::Null))
                        .ok_or_else(|| PlError::new("XX000", "a FOR without a query"))?;
                    let (sql, params, types) = self.bind(text)?;
                    self.host.query(&sql, &params, &types)?
                };
                self.set_found(!out.rows.is_empty());
                for row in &out.rows {
                    self.assign_row(target, &out.columns, Some(row))?;
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
                    let target = body
                        .get("target")
                        .and_then(|t| t.as_object())
                        .and_then(|o| o.values().next())
                        .and_then(|v| v.get("dno").and_then(Value::as_u64))
                        .map(|n| n as usize);
                    let target = match target {
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
                    };
                    let first = out.rows.first().map(|r| r.as_slice());
                    self.assign_row(target, &out.columns, first)?;
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
                    self.assign_row(target, &out.columns, first)?;
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
                    let ty = self.datums.iter().rev().find_map(|d| match d {
                        Datum::Var { name, ty, .. } if name == var => Some(ty.clone()),
                        _ => None,
                    });
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

    fn set_named(&mut self, name: &str, value: Bson) {
        for d in self.datums.iter_mut().rev() {
            if let Datum::Var {
                name: n, value: v, ..
            } = d
            {
                if n == name {
                    *v = value;
                    return;
                }
            }
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
                self.eval(p).map(|v| match v {
                    Bson::Null => "<NULL>".to_string(),
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
        let mut err = PlError::new(if level >= 21 { "P0001" } else { "00000" }, text);
        if level == 19 {
            err.sqlstate = "01000".into();
        }
        if let Some(cond) = body.get("condname").and_then(Value::as_str) {
            if let Some(code) = condition_sqlstate(&cond.to_ascii_lowercase()) {
                err.sqlstate = code.into();
                if message.is_none() {
                    err.message = cond.to_string();
                }
            }
        }
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
                    err.sqlstate = condition_sqlstate(&v.to_ascii_lowercase())
                        .map(str::to_string)
                        .unwrap_or(v)
                }
                1 => err.message = v,
                2 => err.detail = Some(v),
                3 => err.hint = Some(v),
                _ => {}
            }
        }
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

/// A type name as a cast can write it: an array type keeps its brackets, and
/// a name PostgreSQL would need quoted is not produced by `canonical_type`.
fn sql_type(ty: &str) -> String {
    ty.to_string()
}

/// A `CALL`'s arguments as written: the variable each names, when it is a
/// bare name (the only kind an OUTPUT parameter can write back to).
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
            Some(N::ColumnRef(r)) if r.fields.len() == 1 => match r.fields[0].node.as_ref() {
                Some(N::String(s)) => Some(s.sval.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}
