//! Running an inline `DO` block.
//!
//! The body is compiled by `plpgsql_do` into a handful of statement kinds;
//! this module runs them. Every SQL expression in the block -- a RAISE
//! argument, a USING option, the query text of an EXECUTE -- is evaluated by
//! handing `select <expr>` back to the ordinary statement path, so a block
//! sees exactly the values a client would. Notices queue up on the session
//! and are flushed to the wire by the query handlers, before the block's
//! result or error.
//!
//! Messages, SQLSTATEs and CONTEXT lines were measured on PostgreSQL 16.15.

use futures::TryStreamExt;
use pgwire::api::results::{Response, Tag};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};

use crate::plpgsql_do::{self as pl, Level, ParseError, Raise, RaiseWhat, Stmt};
use crate::{encoding, PgHandler};

fn user_err(code: &str, message: impl Into<String>) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".into(),
        code.into(),
        message.into(),
    )))
}

/// The `CONTEXT` line PL/pgSQL adds for a statement of an inline block.
fn block_context(line: usize, at: &str) -> String {
    format!("PL/pgSQL function inline_code_block line {line} at {at}")
}

/// Add an outer frame to an error's context: innermost first, as
/// PostgreSQL prints a stack.
fn add_context(info: &mut ErrorInfo, ctx: String) {
    info.where_context = Some(match info.where_context.take() {
        Some(inner) => format!("{inner}\n{ctx}"),
        None => ctx,
    });
}

impl PgHandler {
    /// Run `DO [LANGUAGE ..] $$ body $$`; `query` is the whole statement, for
    /// error positions.
    pub(crate) async fn run_do(
        &self,
        language: &str,
        body: &str,
        query: &str,
    ) -> PgWireResult<Vec<Response>> {
        match language.to_ascii_lowercase().as_str() {
            "plpgsql" => {}
            "sql" => {
                return Err(user_err(
                    "0A000",
                    format!("language \"{language}\" does not support inline code execution"),
                ))
            }
            _ => {
                return Err(user_err(
                    "42704",
                    format!("language \"{language}\" does not exist"),
                ))
            }
        }
        let stmts = pl::parse(body).map_err(|e| Self::do_parse_error(e, body, query))?;
        for stmt in stmts {
            match stmt {
                Stmt::Null => {}
                Stmt::Raise(raise) => self.do_raise(raise).await?,
                Stmt::Execute { query, line } => self.do_execute(&query, line).await?,
                Stmt::Perform { expr, line } => {
                    self.do_run_sql(&format!("SELECT {expr}"), line, "PERFORM", false)
                        .await?;
                }
            }
        }
        Ok(vec![Response::Execution(Tag::new("DO"))])
    }

    fn do_parse_error(e: ParseError, body: &str, query: &str) -> PgWireError {
        match e {
            ParseError::Syntax { message, offset } => {
                let mut info = ErrorInfo::new("ERROR".into(), "42601".into(), message);
                // The body sits somewhere inside the statement (after `$$` or
                // a quote); a position is only meaningful when the body text
                // appears verbatim, which a `'...'` body with `''` escapes
                // does not.
                if let (Some(offset), Some(start)) = (offset, query.find(body)) {
                    let before = query[..start].chars().count();
                    info.position = Some((before + offset).to_string());
                }
                PgWireError::UserError(Box::new(info))
            }
            ParseError::Compile { message, line } => {
                let mut info = ErrorInfo::new("ERROR".into(), "42601".into(), message);
                info.where_context = Some(format!(
                    "compilation of PL/pgSQL function \"inline_code_block\" near line {line}"
                ));
                PgWireError::UserError(Box::new(info))
            }
            ParseError::Unsupported(what) => {
                user_err("0A000", format!("{what} is not supported yet"))
            }
        }
    }

    /// Run one SQL statement on behalf of the block. `at` names the PL/pgSQL
    /// statement for the CONTEXT line; `is_expr` marks a `select <expr>`
    /// built from an expression, whose context is `SQL expression "..."`
    /// rather than `SQL statement "..."`.
    async fn do_run_sql(
        &self,
        sql: &str,
        line: usize,
        at: &str,
        is_expr: bool,
    ) -> PgWireResult<Vec<Response>> {
        // Values are read back as TEXT whatever the client asked for.
        let binary = self
            .binary_results
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        let out = Box::pin(self.run_typed(sql, &[], &[], 0)).await;
        self.binary_results
            .store(binary, std::sync::atomic::Ordering::Relaxed);
        out.map_err(|e| match e {
            PgWireError::UserError(mut info) => {
                let inner = if is_expr {
                    format!("SQL expression \"{}\"", &sql["SELECT ".len()..])
                } else if info.code.starts_with("42") {
                    // A parse / analysis error names the statement as the
                    // INTERNAL query and points its position into it.
                    info.internal_query = Some(sql.to_string());
                    info.internal_position = info.position.take();
                    String::new()
                } else {
                    format!("SQL statement \"{sql}\"")
                };
                if !inner.is_empty() {
                    add_context(&mut info, inner);
                }
                add_context(&mut info, block_context(line, at));
                PgWireError::UserError(info)
            }
            other => other,
        })
    }

    /// Evaluate one SQL expression to its text form (`None` for NULL).
    async fn do_eval(&self, expr: &str, line: usize, at: &str) -> PgWireResult<Option<String>> {
        let responses = self
            .do_run_sql(&format!("SELECT {expr}"), line, at, true)
            .await?;
        let Some(Response::Query(q)) = responses.into_iter().next() else {
            return Err(user_err(
                "0A000",
                format!("evaluating \"{expr}\" in an inline code block is not supported yet"),
            ));
        };
        let rows = q.data_rows.try_collect::<Vec<_>>().await?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let enc = self.client_encoding();
        Ok(crate::split_single_field(row).map(|b| encoding::decode(enc, &b)))
    }

    async fn do_raise(&self, raise: Raise) -> PgWireResult<()> {
        let Raise {
            level,
            what,
            options,
            line,
        } = raise;
        let mut message: Option<String> = None;
        let mut sqlstate: Option<String> = None;
        match what {
            RaiseWhat::Format { text, args } => {
                let mut values = Vec::with_capacity(args.len());
                for arg in &args {
                    values.push(self.do_eval(arg, line, "RAISE").await?);
                }
                message = Some(pl::render_format(&text, &values));
            }
            RaiseWhat::Condition(name) => {
                // A bare condition NAME is resolved when the block is
                // compiled, so its error carries the compilation context;
                // an `errcode = '...'` option is checked at run time.
                let code = pl::condition_sqlstate(&name)
                    .ok_or_else(|| Self::unknown_condition(&name, line, true))?;
                sqlstate = Some(code.to_string());
                message = Some(name);
            }
            RaiseWhat::Sqlstate(code) => {
                if !pl::is_sqlstate(&code) {
                    return Err(Self::unknown_condition(&code, line, false));
                }
                message = Some(code.clone());
                sqlstate = Some(code);
            }
            RaiseWhat::Nothing => {}
        }
        let mut detail = None;
        let mut hint = None;
        let mut column = None;
        let mut constraint = None;
        let mut datatype = None;
        let mut table = None;
        let mut schema = None;
        let mut seen: Vec<String> = Vec::new();
        for (name, expr) in options {
            if seen.contains(&name) || (name == "MESSAGE" && message.is_some()) {
                let mut info = ErrorInfo::new(
                    "ERROR".into(),
                    "42601".into(),
                    format!("RAISE option already specified: {name}"),
                );
                info.where_context = Some(block_context(line, "RAISE"));
                return Err(PgWireError::UserError(Box::new(info)));
            }
            let value = self.do_eval(&expr, line, "RAISE").await?;
            let Some(value) = value else {
                let mut info = ErrorInfo::new(
                    "ERROR".into(),
                    "22004".into(),
                    "RAISE statement option cannot be null".into(),
                );
                info.where_context = Some(block_context(line, "RAISE"));
                return Err(PgWireError::UserError(Box::new(info)));
            };
            match name.as_str() {
                "MESSAGE" => message = Some(value),
                "DETAIL" => detail = Some(value),
                "HINT" => hint = Some(value),
                "COLUMN" => column = Some(value),
                "CONSTRAINT" => constraint = Some(value),
                "DATATYPE" => datatype = Some(value),
                "TABLE" => table = Some(value),
                "SCHEMA" => schema = Some(value),
                "ERRCODE" => {
                    if pl::is_sqlstate(&value) {
                        sqlstate = Some(value);
                    } else {
                        let code = pl::condition_sqlstate(&value)
                            .ok_or_else(|| Self::unknown_condition(&value, line, false))?;
                        sqlstate = Some(code.to_string());
                    }
                }
                _ => unreachable!("the parser admits only known options"),
            }
            seen.push(name);
        }
        if message.is_none()
            && sqlstate.is_none()
            && level == Level::Exception
            && !seen.iter().any(|s| s == "ERRCODE")
            && detail.is_none()
            && hint.is_none()
            && column.is_none()
            && constraint.is_none()
            && datatype.is_none()
            && table.is_none()
            && schema.is_none()
        {
            let mut info = ErrorInfo::new(
                "ERROR".into(),
                "0Z002".into(),
                "RAISE without parameters cannot be used outside an exception handler".into(),
            );
            info.where_context = Some(block_context(line, "RAISE"));
            return Err(PgWireError::UserError(Box::new(info)));
        }
        let Some(severity) = level.severity() else {
            // DEBUG / LOG are below `client_min_messages`; nothing is sent.
            return Ok(());
        };
        let code = sqlstate.unwrap_or_else(|| level.default_sqlstate().to_string());
        // A bare `raise using hint = ...` carries the SQLSTATE as its message,
        // exactly as PostgreSQL's does.
        let message = message.unwrap_or_else(|| code.clone());
        let mut info = ErrorInfo::new(severity.into(), code, message);
        info.detail = detail;
        info.hint = hint;
        info.column = column;
        info.constraint = constraint;
        info.datatype = datatype;
        info.table = table;
        info.schema = schema;
        info.where_context = Some(block_context(line, "RAISE"));
        if level == Level::Exception {
            return Err(PgWireError::UserError(Box::new(info)));
        }
        self.pending_notices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(info);
        Ok(())
    }

    fn unknown_condition(name: &str, line: usize, at_compile: bool) -> PgWireError {
        let mut info = ErrorInfo::new(
            "ERROR".into(),
            "42704".into(),
            format!("unrecognized exception condition \"{name}\""),
        );
        info.where_context = Some(if at_compile {
            format!("compilation of PL/pgSQL function \"inline_code_block\" near line {line}")
        } else {
            block_context(line, "RAISE")
        });
        PgWireError::UserError(Box::new(info))
    }

    async fn do_execute(&self, query_expr: &str, line: usize) -> PgWireResult<()> {
        let sql = self.do_eval(query_expr, line, "EXECUTE").await?;
        let Some(sql) = sql else {
            let mut info = ErrorInfo::new(
                "ERROR".into(),
                "22004".into(),
                "query string argument of EXECUTE is null".into(),
            );
            info.where_context = Some(block_context(line, "EXECUTE"));
            return Err(PgWireError::UserError(Box::new(info)));
        };
        let responses = self.do_run_sql(&sql, line, "EXECUTE", false).await?;
        // The rows of a SELECT are produced lazily; drain them so any error
        // in the row path surfaces here, inside the block.
        for r in responses {
            if let Response::Query(q) = r {
                q.data_rows.try_collect::<Vec<_>>().await?;
            }
        }
        Ok(())
    }
}
