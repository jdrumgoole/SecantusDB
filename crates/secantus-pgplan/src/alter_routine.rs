//! `ALTER FUNCTION | PROCEDURE | ROUTINE`: rename, move to a schema, change
//! owner, or change attributes (volatility, STRICT, SECURITY DEFINER,
//! LEAKPROOF, COST, ROWS, PARALLEL, SET / RESET configuration). PostgreSQL
//! parses these as four statements; each plans to one `AlterFunction`.

use pg_query::protobuf::node::Node as N;
use pg_query::protobuf::ObjectType;

use crate::{type_name_of, Error, Result, Statement};

/// What an `ALTER FUNCTION` changes.
#[derive(Debug, Clone, PartialEq)]
pub enum AlterFunctionAction {
    Rename(String),
    SetSchema(String),
    Owner(String),
    /// `(attribute, value)`: `volatility` (`immutable` / `stable` /
    /// `volatile`), `strict` / `security` / `leakproof` (`true` / `false`),
    /// `cost` / `rows` (a number), `parallel` (`safe` / `restricted` /
    /// `unsafe`), `set` (`name=value`), `reset` (a name, or `all`).
    Options(Vec<(String, String)>),
}

/// `function`, `procedure` or `routine` for a routine object type.
fn kind_of(t: i32) -> Option<&'static str> {
    match ObjectType::try_from(t) {
        Ok(ObjectType::ObjectFunction) => Some("function"),
        Ok(ObjectType::ObjectProcedure) => Some("procedure"),
        Ok(ObjectType::ObjectRoutine) => Some("routine"),
        _ => None,
    }
}

/// The routine an ObjectWithArgs names: its (last) name and, unless left
/// off, its argument types.
fn target(o: &pg_query::protobuf::ObjectWithArgs) -> Result<(String, Option<Vec<String>>)> {
    let name = o
        .objname
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        })
        .next_back()
        .ok_or_else(|| Error::Parse("a routine without a name".into()))?;
    let args = (!o.args_unspecified).then(|| {
        o.objargs
            .iter()
            .filter_map(|n| match n.node.as_ref()? {
                N::TypeName(tn) => Some(type_name_of(tn)),
                _ => None,
            })
            .collect()
    });
    Ok((name, args))
}

fn object_with_args(
    n: Option<&pg_query::protobuf::Node>,
) -> Result<&pg_query::protobuf::ObjectWithArgs> {
    match n.and_then(|n| n.node.as_ref()) {
        Some(N::ObjectWithArgs(o)) => Ok(o),
        _ => Err(Error::Parse("a routine without a signature".into())),
    }
}

fn statement(
    kind: &str,
    o: &pg_query::protobuf::ObjectWithArgs,
    action: AlterFunctionAction,
) -> Result<Statement> {
    let (name, arg_types) = target(o)?;
    Ok(Statement::AlterFunction {
        kind: kind.to_string(),
        name,
        arg_types,
        action,
    })
}

/// `ALTER FUNCTION f(...) RENAME TO g`.
pub(crate) fn plan_rename(r: &pg_query::protobuf::RenameStmt) -> Option<Result<Statement>> {
    let kind = kind_of(r.rename_type)?;
    Some(
        object_with_args(r.object.as_deref())
            .and_then(|o| statement(kind, o, AlterFunctionAction::Rename(r.newname.clone()))),
    )
}

/// `ALTER FUNCTION f(...) SET SCHEMA s`.
pub(crate) fn plan_set_schema(
    a: &pg_query::protobuf::AlterObjectSchemaStmt,
) -> Option<Result<Statement>> {
    let kind = kind_of(a.object_type)?;
    Some(
        object_with_args(a.object.as_deref())
            .and_then(|o| statement(kind, o, AlterFunctionAction::SetSchema(a.newschema.clone()))),
    )
}

/// `ALTER FUNCTION f(...) OWNER TO r`.
pub(crate) fn plan_owner(a: &pg_query::protobuf::AlterOwnerStmt) -> Option<Result<Statement>> {
    let kind = kind_of(a.object_type)?;
    let owner = a
        .newowner
        .as_ref()
        .map(crate::role_spec_name)
        .unwrap_or_default();
    Some(
        object_with_args(a.object.as_deref())
            .and_then(|o| statement(kind, o, AlterFunctionAction::Owner(owner))),
    )
}

/// `ALTER FUNCTION f(...) <attribute> ...`.
pub(crate) fn plan_alter(a: &pg_query::protobuf::AlterFunctionStmt) -> Result<Statement> {
    let kind = kind_of(a.objtype).unwrap_or("function");
    let o = a
        .func
        .as_ref()
        .ok_or_else(|| Error::Parse("ALTER FUNCTION without a function".into()))?;
    let mut options = Vec::new();
    for d in &a.actions {
        let Some(N::DefElem(e)) = d.node.as_ref() else {
            continue;
        };
        let arg = e.arg.as_deref().and_then(|a| a.node.as_ref());
        let text = match arg {
            Some(N::String(s)) => s.sval.clone(),
            Some(N::Boolean(b)) => b.boolval.to_string(),
            Some(N::Integer(i)) => i.ival.to_string(),
            Some(N::Float(f)) => f.fval.clone(),
            Some(N::VariableSetStmt(v)) => {
                use pg_query::protobuf::VariableSetKind as K;
                match K::try_from(v.kind) {
                    Ok(K::VarSetValue) => {
                        let values: Vec<String> = v
                            .args
                            .iter()
                            .filter_map(|a| match a.node.as_ref() {
                                Some(N::AConst(c)) => match c.val.as_ref() {
                                    Some(pg_query::protobuf::a_const::Val::Sval(s)) => {
                                        Some(s.sval.clone())
                                    }
                                    Some(pg_query::protobuf::a_const::Val::Ival(i)) => {
                                        Some(i.ival.to_string())
                                    }
                                    Some(pg_query::protobuf::a_const::Val::Fval(f)) => {
                                        Some(f.fval.clone())
                                    }
                                    Some(pg_query::protobuf::a_const::Val::Boolval(b)) => {
                                        Some(if b.boolval { "on" } else { "off" }.to_string())
                                    }
                                    _ => None,
                                },
                                _ => None,
                            })
                            .collect();
                        options.push((
                            "set".to_string(),
                            format!("{}={}", v.name, values.join(", ")),
                        ));
                    }
                    Ok(K::VarReset) => options.push(("reset".to_string(), v.name.clone())),
                    Ok(K::VarResetAll) => options.push(("reset".to_string(), "all".to_string())),
                    _ => return Err(Error::Unsupported("this SET form in ALTER FUNCTION".into())),
                }
                continue;
            }
            None => "true".to_string(),
            _ => return Err(Error::Unsupported("this ALTER FUNCTION attribute".into())),
        };
        options.push((e.defname.to_ascii_lowercase(), text));
    }
    statement(kind, o, AlterFunctionAction::Options(options))
}
