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

/// `(output positions, parameter names, parameter types, output values)`.
pub(crate) type ProcedureOutput = (Vec<usize>, Vec<String>, Vec<String>, Vec<Option<Bson>>);

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
        let (outputs, names, types, row) = self.run_procedure(name, args, arg_types)?;
        if outputs.is_empty() {
            return Ok(vec![Response::Execution(Tag::new("CALL"))]);
        }
        let tz = self.session_timezone();
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

    /// Run a procedure: its OUTPUT parameters' positions, every parameter's
    /// name and type, and the output values (in position order).
    pub(crate) fn run_procedure(
        &self,
        name: &str,
        args: Vec<Bson>,
        arg_types: Vec<String>,
    ) -> PgWireResult<ProcedureOutput> {
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
                            nonatomic: true,
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
            return Ok((outputs, names, types, Vec::new()));
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
        Ok((outputs, names, types, row))
    }
}

impl PgHandler {
    /// `ALTER FUNCTION | PROCEDURE | ROUTINE name[(args)] ...`, on the
    /// routine's stored document (the shared shape, not its read-back).
    pub(crate) fn alter_function(
        &self,
        kind: &str,
        name: &str,
        arg_types: Option<Vec<String>>,
        action: secantus_pgplan::alter_routine::AlterFunctionAction,
    ) -> PgWireResult<Vec<Response>> {
        use secantus_pgplan::alter_routine::AlterFunctionAction as A;
        let tag = match kind {
            "procedure" => "ALTER PROCEDURE",
            "routine" => "ALTER ROUTINE",
            _ => "ALTER FUNCTION",
        };
        let noun = if kind == "procedure" {
            "procedure"
        } else {
            "function"
        };
        let error = |code: &str, message: String| {
            PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                code.into(),
                message,
            )))
        };
        let docs = self.type_catalog_docs(Self::FUNCTION_COLLECTION)?;
        let by_name: Vec<&Document> = docs
            .iter()
            .filter(|d| d.get_str("name") == Ok(name))
            .collect();
        let shown = |types: &[String]| {
            format!(
                "{name}({})",
                types
                    .iter()
                    .map(|t| secantus_pgplan::display_type(t))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let target: &Document = match &arg_types {
            Some(types) => {
                let keys = types
                    .iter()
                    .map(|t| self.function_type_key(t))
                    .collect::<PgWireResult<Vec<_>>>()?;
                let Some(d) = by_name.iter().find(|d| strings(d, "param_types") == keys) else {
                    return Err(error(
                        "42883",
                        format!("{noun} {} does not exist", shown(types)),
                    ));
                };
                d
            }
            None => match by_name.as_slice() {
                [] => {
                    return Err(error(
                        "42883",
                        format!("could not find a {noun} named \"{name}\""),
                    ))
                }
                [one] => one,
                _ => {
                    return Err(error(
                        "42725",
                        format!("{noun} name \"{name}\" is not unique"),
                    ))
                }
            },
        };
        let is_procedure = target.get_bool("is_procedure").unwrap_or(false);
        let signature = shown(&strings(target, "param_types"));
        if (kind == "procedure" && !is_procedure) || (kind == "function" && is_procedure) {
            return Err(error("42809", format!("{signature} is not a {kind}")));
        }
        let id = target.get_str("_id").unwrap_or_default().to_string();
        let raw = self.type_catalog_docs_raw(Self::FUNCTION_COLLECTION)?;
        let Some(mut doc) = raw
            .iter()
            .find(|d| d.get_str("_id") == Ok(id.as_str()))
            .cloned()
        else {
            return Err(error("42883", format!("{noun} {signature} does not exist")));
        };
        let mut new_id = id.clone();
        match action {
            A::Rename(to) => {
                let inputs = strings(target, "param_types");
                if docs.iter().any(|d| {
                    d.get_str("name") == Ok(to.as_str()) && strings(d, "param_types") == inputs
                }) {
                    return Err(error(
                        "42723",
                        format!(
                            "{noun} {} already exists in schema \"{}\"",
                            shown(&inputs).replacen(name, &to, 1),
                            target.get_str("schema").unwrap_or("public")
                        ),
                    ));
                }
                let nargs = doc.get_i32("nargs").unwrap_or(inputs.len() as i32);
                new_id = format!("{to}/{nargs}");
                if raw.iter().any(|d| d.get_str("_id") == Ok(new_id.as_str())) {
                    new_id = format!("{new_id}/{}", inputs.join(","));
                }
                doc.insert("name", to);
                doc.insert("_id", new_id.clone());
            }
            A::SetSchema(schema) => {
                if !self.namespaces().iter().any(|(n, _)| *n == schema) {
                    return Err(error(
                        "3F000",
                        format!("schema \"{schema}\" does not exist"),
                    ));
                }
                if schema == "public" {
                    doc.remove("schema");
                } else {
                    doc.insert("schema", schema);
                }
            }
            A::Owner(role) => {
                let role = if matches!(
                    role.as_str(),
                    "CURRENT_USER" | "SESSION_USER" | "CURRENT_ROLE"
                ) {
                    self.session_user_name()
                } else {
                    role
                };
                if role != self.session_user_name() && self.role(&role)?.is_none() {
                    return Err(error("42704", format!("role \"{role}\" does not exist")));
                }
                doc.insert("owner", role);
            }
            A::Options(options) => {
                let mut config: Vec<String> = strings(&doc, "config");
                for (attr, value) in options {
                    match attr.as_str() {
                        "volatility" => {
                            doc.insert("volatility", value);
                        }
                        "strict" => {
                            doc.insert("strict", value == "true");
                        }
                        "security" => {
                            doc.insert("security_definer", value == "true");
                        }
                        "leakproof" => {
                            doc.insert("leakproof", value == "true");
                        }
                        "cost" => {
                            let cost: f64 = value.parse().unwrap_or(0.0);
                            if cost <= 0.0 {
                                return Err(error("22023", "COST must be positive".into()));
                            }
                            doc.insert("cost", cost);
                        }
                        "rows" => {
                            let rows: f64 = value.parse().unwrap_or(0.0);
                            if rows <= 0.0 {
                                return Err(error("22023", "ROWS must be positive".into()));
                            }
                            if !target.get_bool("returns_set").unwrap_or(false)
                                && !target.get_bool("is_table").unwrap_or(false)
                            {
                                return Err(error(
                                    "22023",
                                    "ROWS is not applicable when function does not return a set"
                                        .into(),
                                ));
                            }
                            doc.insert("rows", rows);
                        }
                        "parallel" => {
                            let code = match value.as_str() {
                                "safe" => "s",
                                "restricted" => "r",
                                "unsafe" => "u",
                                other => {
                                    return Err(error(
                                        "22023",
                                        format!(
                                            "parameter \"parallel\" must be SAFE, RESTRICTED, or UNSAFE, not \"{other}\""
                                        ),
                                    ))
                                }
                            };
                            doc.insert("parallel", code);
                        }
                        "set" => {
                            let key = value.split('=').next().unwrap_or_default().to_string();
                            config.retain(|c| c.split('=').next() != Some(key.as_str()));
                            config.push(value);
                        }
                        "reset" if value == "all" => config.clear(),
                        "reset" => config.retain(|c| c.split('=').next() != Some(value.as_str())),
                        other => {
                            return Err(error(
                                "0A000",
                                format!(
                                    "ALTER FUNCTION ... {} is not supported yet",
                                    other.to_uppercase()
                                ),
                            ))
                        }
                    }
                }
                if config.is_empty() {
                    doc.remove("config");
                } else {
                    doc.insert(
                        "config",
                        config.into_iter().map(Bson::String).collect::<Vec<_>>(),
                    );
                }
            }
        }
        self.delete_type_doc(Self::FUNCTION_COLLECTION, &id)?;
        self.insert_type_doc(Self::FUNCTION_COLLECTION, &new_id, doc)?;
        Ok(vec![Response::Execution(Tag::new(tag))])
    }
}
