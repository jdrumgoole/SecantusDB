//! `CALL`: running a procedure (`CREATE PROCEDURE`).
//!
//! A procedure is a function document with `is_procedure` (the Python
//! server's shape), keyed by EVERY parameter: a `CALL` passes a placeholder
//! for each OUT one, so the arguments line up with the declared parameters
//! position for position. Its OUT / INOUT values come back as one row;
//! without any, the answer is the bare `CALL` tag.

use bson::{Bson, Document};
use futures::stream::{self, StreamExt};
use pgwire::api::results::{DataRowEncoder, QueryResponse, Response, Tag};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use std::sync::Arc;

use crate::{
    column_at, encode_field_value, plpgsql_create_sql, plpgsql_fn, user_fn_of, wire_type,
    PgHandler, PlHost,
};

fn strings(d: &Document, key: &str) -> Vec<String> {
    d.get_array(key)
        .map(|a| {
            a.iter()
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

impl PgHandler {
    /// `CALL name(args)`.
    pub(crate) fn call_procedure(
        &self,
        name: &str,
        args: Vec<Bson>,
        arg_types: Vec<String>,
    ) -> PgWireResult<Vec<Response>> {
        let docs = self.user_function_docs()?;
        let shown = || {
            format!(
                "{name}({})",
                arg_types
                    .iter()
                    .map(|t| secantus_pgplan::display_type(t))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        // Every declared parameter, OUT ones included.
        let all_types = |d: &Document| -> Vec<String> {
            let all = strings(d, "all_param_types");
            if all.is_empty() {
                strings(d, "param_types")
            } else {
                all
            }
        };
        let procedure = docs.iter().find(|d| {
            d.get_str("name") == Ok(name)
                && d.get_bool("is_procedure").unwrap_or(false)
                && all_types(d).len() == args.len()
        });
        let Some(doc) = procedure else {
            // A FUNCTION by that name and arity: PostgreSQL's 42809.
            if docs.iter().any(|d| {
                d.get_str("name") == Ok(name)
                    && !d.get_bool("is_procedure").unwrap_or(false)
                    && strings(d, "param_types").len() == args.len()
            }) {
                let mut info = ErrorInfo::new(
                    "ERROR".into(),
                    "42809".into(),
                    format!("{} is not a procedure", shown()),
                );
                info.hint = Some("To call a function, use SELECT.".into());
                return Err(PgWireError::UserError(Box::new(info)));
            }
            let mut info = ErrorInfo::new(
                "ERROR".into(),
                "42883".into(),
                format!("procedure {} does not exist", shown()),
            );
            info.hint = Some(
                "No procedure matches the given name and argument types. You might need to \
                 add explicit type casts."
                    .into(),
            );
            return Err(PgWireError::UserError(Box::new(info)));
        };
        let modes = strings(doc, "param_modes");
        let names = {
            let all = strings(doc, "all_params");
            if all.is_empty() {
                strings(doc, "params")
            } else {
                all
            }
        };
        let types = all_types(doc);
        // Each argument as its declared type, so a literal `'1'` for an
        // `int` parameter arrives as the integer.
        let tz = self.session_timezone();
        let mut bound = Vec::with_capacity(args.len());
        for (i, a) in args.into_iter().enumerate() {
            let out_only = modes.get(i).is_some_and(|m| m == "o");
            bound.push(match (&a, types.get(i)) {
                (Bson::Null, _) => Bson::Null,
                _ if out_only => Bson::Null,
                (_, Some(t)) => {
                    secantus_pgplan::cast_value_with_tz(a, t, &tz).map_err(|e| Self::err(&e))?
                }
                (_, None) => a,
            });
        }
        let outputs: Vec<usize> = (0..types.len())
            .filter(|i| modes.get(*i).is_some_and(|m| m == "o" || m == "b"))
            .collect();
        let value = match doc.get_str("language").unwrap_or_default() {
            "plpgsql" => {
                let host = PlHost { h: self };
                let out_names: Vec<String> = outputs
                    .iter()
                    .map(|i| names.get(*i).cloned().unwrap_or_default())
                    .collect();
                let outcome = self.with_call_depth(|| {
                    plpgsql_fn::run(
                        &plpgsql_create_sql(doc),
                        plpgsql_fn::Invocation {
                            args: &bound,
                            arg_types: &types,
                            trigger: None,
                            returns_set: false,
                            out_params: &out_names,
                            procedure: true,
                        },
                        &host,
                    )
                    .map_err(crate::wire_pl_error)
                })?;
                match outcome {
                    plpgsql_fn::Outcome::Value(v) => v,
                    _ => Bson::Null,
                }
            }
            _ => {
                // A SQL procedure runs its statements with the INPUT
                // arguments bound; the last one's row is the OUT values.
                let inputs: Vec<Bson> = bound
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| modes.get(*i).is_none_or(|m| m != "o"))
                    .map(|(_, v)| v.clone())
                    .collect();
                let u = user_fn_of(doc);
                match self.call_sql_function(doc, &u, &inputs)? {
                    secantus_pgplan::FnResult::Value(v) => v,
                    secantus_pgplan::FnResult::Rows(_, _, rows) => rows
                        .into_iter()
                        .next()
                        .map(|r| match r.as_slice() {
                            [one] => one.clone(),
                            many => Bson::Array(many.to_vec()),
                        })
                        .unwrap_or(Bson::Null),
                }
            }
        };
        if outputs.is_empty() {
            return Ok(vec![Response::Execution(Tag::new("CALL"))]);
        }
        let row: Vec<Option<Bson>> = if outputs.len() == 1 {
            vec![Some(value)]
        } else {
            let fields = match &value {
                Bson::Array(items) => Some(items.clone()),
                other => secantus_pgplan::record_values(other).cloned(),
            }
            .unwrap_or_else(|| vec![Bson::Null; outputs.len()]);
            fields.into_iter().map(Some).collect()
        };
        let fields = outputs
            .iter()
            .map(|i| {
                self.field(
                    names.get(*i).cloned().unwrap_or_default(),
                    wire_type(&types[*i]),
                )
            })
            .collect::<Vec<_>>();
        let schema = Arc::new(fields);
        let schema_ref = schema.clone();
        let ds = self.session_datestyle();
        let cenc = self.client_encoding();
        let rows = stream::iter(vec![row]).map(move |vals| {
            let mut enc = DataRowEncoder::new(schema_ref.clone());
            for (i, v) in vals.iter().enumerate() {
                encode_field_value(
                    &mut enc,
                    column_at(&schema_ref, i)?,
                    v.as_ref(),
                    &tz,
                    &ds,
                    cenc,
                )?;
            }
            Ok(enc.take_row())
        });
        let mut response = QueryResponse::new(schema, rows);
        response.set_bare_command_tag("CALL");
        Ok(vec![Response::Query(response)])
    }
}
