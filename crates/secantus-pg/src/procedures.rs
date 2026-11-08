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

/// `(output positions, parameter names, parameter types, output values,
/// the argument written for each parameter)`.
pub(crate) type ProcedureOutput = (
    Vec<usize>,
    Vec<String>,
    Vec<String>,
    Vec<Option<Bson>>,
    Vec<Option<usize>>,
);

fn strings(d: &Document, key: &str) -> Vec<String> {
    d.get_array(key)
        .map(|a| {
            a.iter()
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Every declared parameter's type, OUT ones included.
fn all_types(d: &Document) -> Vec<String> {
    let all = strings(d, "all_param_types");
    if all.is_empty() {
        strings(d, "param_types")
    } else {
        all
    }
}

/// Every declared parameter's name, OUT ones included.
fn all_names(d: &Document) -> Vec<String> {
    let all = strings(d, "all_params");
    if all.is_empty() {
        strings(d, "params")
    } else {
        all
    }
}

impl PgHandler {
    /// `CALL name(args)`.
    pub(crate) fn call_procedure(
        &self,
        name: &str,
        args: Vec<Bson>,
        arg_types: Vec<String>,
        arg_names: Vec<Option<String>>,
    ) -> PgWireResult<Vec<Response>> {
        let (outputs, names, types, row, _) =
            self.run_procedure(name, args, arg_types, arg_names)?;
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

    /// The procedure a CALL reaches, as `FuncnameGetCandidates` finds a
    /// CALL's (`include_out_arguments`): every routine of the name,
    /// procedure or function, whose parameters -- OUT ones included -- the
    /// arguments can fill, positionally first and then by name
    /// (`MatchNamedCall`), with a DEFAULT for each one left out. Then
    /// `func_get_detail` picks among them by the arguments' types (in the
    /// order written, `unknown` for an untyped literal); a FUNCTION picked
    /// is 42809. The procedure's document, and the parameter each argument
    /// fills.
    pub(crate) fn resolve_call(
        &self,
        name: &str,
        arg_types: &[String],
        arg_names: &[Option<String>],
    ) -> PgWireResult<(Document, Vec<usize>)> {
        let nargs = arg_types.len();
        let docs = self.user_function_docs()?;
        let positional = arg_names.iter().take_while(|n| n.is_none()).count();
        // `func_signature_string`: a named argument shows its name.
        let shown = || {
            format!(
                "{name}({})",
                arg_types
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let ty = secantus_pgplan::display_type(t);
                        match arg_names.get(i).cloned().flatten() {
                            Some(n) if i >= positional => format!("{n} => {ty}"),
                            _ => ty,
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let fail = |code: &str, message: String, hint: &str| {
            let mut info = ErrorInfo::new("ERROR".into(), code.into(), message);
            info.hint = Some(hint.into());
            PgWireError::UserError(Box::new(info))
        };
        self.require_routine_schema(name)?;
        let functions = self.functions()?;
        let reached: Vec<&str> = Self::routines_named(&functions, name)
            .into_iter()
            .map(|f| f.key.as_str())
            .collect();
        // Each candidate: its document, the parameter each argument fills,
        // and those parameters' types in the arguments' order.
        let mut candidates: Vec<(&Document, Vec<usize>, Vec<String>)> = Vec::new();
        for d in docs
            .iter()
            .filter(|d| d.get_str("_id").is_ok_and(|id| reached.contains(&id)))
        {
            let types = all_types(d);
            let pronargs = types.len();
            let ndefaults = d
                .get_array("param_defaults")
                .map(|a| a.iter().filter(|v| !matches!(v, Bson::Null)).count())
                .unwrap_or(0);
            if nargs > pronargs {
                continue;
            }
            let mut filled = vec![false; pronargs];
            let mut numbers = Vec::with_capacity(nargs);
            for (k, given) in arg_names.iter().enumerate() {
                let at = match given {
                    None => Some(k),
                    Some(n) => all_names(d).iter().position(|p| p == n),
                };
                match at {
                    Some(i) if i < pronargs && !filled[i] => {
                        filled[i] = true;
                        numbers.push(i);
                    }
                    _ => break,
                }
            }
            // Every parameter left out must be one of the trailing ones
            // with a DEFAULT.
            let first_default = pronargs.saturating_sub(ndefaults);
            if numbers.len() != nargs || filled[..first_default].iter().any(|f| !f) {
                continue;
            }
            let list = numbers.iter().map(|&i| types[i].clone()).collect();
            candidates.push((d, numbers, list));
        }
        // Two candidates the arguments see as the same parameter types (one
        // of them through its defaults) are ambiguous, whichever is picked.
        let lists: Vec<Vec<String>> = candidates.iter().map(|c| c.2.clone()).collect();
        let doubled = |i: usize| lists.iter().filter(|l| **l == lists[i]).count() > 1;
        use secantus_pgplan::RoutineChoice as C;
        let chosen = match secantus_pgplan::select_routine(&lists, arg_types) {
            C::One(i) if !doubled(i) => i,
            C::One(_) | C::Ambiguous => {
                return Err(fail(
                    "42725",
                    format!("procedure {} is not unique", shown()),
                    "Could not choose a best candidate procedure. You might need to add \
                     explicit type casts.",
                ))
            }
            C::None => {
                return Err(fail(
                    "42883",
                    format!("procedure {} does not exist", shown()),
                    "No procedure matches the given name and argument types. You might need \
                     to add explicit type casts.",
                ))
            }
            C::Undecided => {
                return Err(Self::err(&secantus_pgplan::Error::Unsupported(format!(
                    "choosing among the overloads of {name} for a call over a type with no \
                     type category here"
                ))))
            }
        };
        let (doc, numbers, _) = &candidates[chosen];
        if !doc.get_bool("is_procedure").unwrap_or(false) {
            return Err(fail(
                "42809",
                format!("{} is not a procedure", shown()),
                "To call a function, use SELECT.",
            ));
        }
        Ok(((*doc).clone(), numbers.clone()))
    }

    /// Run a procedure: its OUTPUT parameters' positions, every parameter's
    /// name and type, the output values (in position order), and for each
    /// parameter the argument written for it. The procedure is the one
    /// [`Self::resolve_call`] finds.
    pub(crate) fn run_procedure(
        &self,
        name: &str,
        args: Vec<Bson>,
        arg_types: Vec<String>,
        arg_names: Vec<Option<String>>,
    ) -> PgWireResult<ProcedureOutput> {
        let (doc, numbers) = self.resolve_call(name, &arg_types, &arg_names)?;
        let doc = &doc;
        let modes = strings(doc, "param_modes");
        let names = all_names(doc);
        let types = all_types(doc);
        let defaults: Vec<Option<String>> = doc
            .get_array("param_defaults")
            .map(|a| a.iter().map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        // The argument written for each parameter.
        let mut sources: Vec<Option<usize>> = vec![None; types.len()];
        for (k, &i) in numbers.iter().enumerate() {
            sources[i] = Some(k);
        }
        // Each argument as its declared type, so a literal `'1'` for an
        // `int` parameter arrives as the integer; a parameter left out
        // takes its DEFAULT, evaluated now.
        let tz = self.session_timezone();
        let mut args: Vec<Option<Bson>> = args.into_iter().map(Some).collect();
        let mut bound = Vec::with_capacity(types.len());
        let mut input = 0;
        for (i, t) in types.iter().enumerate() {
            let out_only = modes.get(i).is_some_and(|m| m == "o");
            let a = sources[i].and_then(|k| args[k].take());
            bound.push(match a {
                _ if out_only => Bson::Null,
                Some(Bson::Null) => Bson::Null,
                Some(a) => {
                    secantus_pgplan::cast_value_with_tz(a, t, &tz).map_err(|e| Self::err(&e))?
                }
                None => match defaults.get(input).cloned().flatten() {
                    Some(sql) => secantus_pgplan::correlated::evaluate_default(&sql, t)
                        .map_err(|e| Self::err(&e))?,
                    None => Bson::Null,
                },
            });
            if !out_only {
                input += 1;
            }
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
            return Ok((outputs, names, types, Vec::new(), sources));
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
        Ok((outputs, names, types, row, sources))
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
        self.require_routine_schema(name)?;
        let docs = self.type_catalog_docs(Self::FUNCTION_COLLECTION)?;
        let functions = self.functions()?;
        let reached: Vec<&str> = Self::routines_named(&functions, name)
            .into_iter()
            .map(|f| f.key.as_str())
            .collect();
        let by_name: Vec<&Document> = docs
            .iter()
            .filter(|d| d.get_str("_id").is_ok_and(|id| reached.contains(&id)))
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
            None => {
                // With no argument list only the statement's kind is looked
                // among (`LookupFuncWithArgs`, RP-508).
                let of_kind: Vec<&str> = Self::routines_of_kind(&functions, name, kind)
                    .into_iter()
                    .map(|f| f.key.as_str())
                    .collect();
                let found: Vec<&Document> = docs
                    .iter()
                    .filter(|d| d.get_str("_id").is_ok_and(|id| of_kind.contains(&id)))
                    .collect();
                match found.as_slice() {
                    [one] => one,
                    _ => return Err(Self::routine_lookup_error(kind, name, found.len())),
                }
            }
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
        let schema = target.get_str("schema").unwrap_or("public").to_string();
        let inputs = strings(target, "param_types");
        let nargs = doc.get_i32("nargs").unwrap_or(inputs.len() as i32) as usize;
        // The routine's key once it is `name` in `schema`: a second overload
        // at one arity there takes the long form.
        let key_in = |schema: &str, name: &str| {
            let key = Self::routine_key(schema, name, nargs);
            if raw.iter().any(|d| d.get_str("_id") == Ok(key.as_str())) {
                format!("{key}/{}", inputs.join(","))
            } else {
                key
            }
        };
        let in_schema = |d: &Document, s: &str| d.get_str("schema").unwrap_or("public") == s;
        match action {
            A::Rename(to) => {
                if docs.iter().any(|d| {
                    d.get_str("name") == Ok(to.as_str())
                        && in_schema(d, &schema)
                        && strings(d, "param_types") == inputs
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
                new_id = key_in(&schema, &to);
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
                let bare = target.get_str("name").unwrap_or_default();
                if docs.iter().any(|d| {
                    d.get_str("name") == Ok(bare)
                        && in_schema(d, &schema)
                        && strings(d, "param_types") == inputs
                }) {
                    return Err(error(
                        "42723",
                        format!(
                            "{noun} {} already exists in schema \"{schema}\"",
                            shown(&inputs).replacen(name, bare, 1)
                        ),
                    ));
                }
                new_id = key_in(&schema, bare);
                doc.insert("_id", new_id.clone());
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
