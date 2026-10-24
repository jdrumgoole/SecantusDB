//! `CREATE / DROP TRANSFORM` (RP-169), as PostgreSQL 15.19's grammar builds
//! them. The executor (`secantus-pg`'s `transforms.rs`) checks them as
//! `CreateTransform` does and keeps the `pg_transform` rows. No procedural
//! language here calls a transform's functions (SQL and PL/pgSQL never
//! do), so a transform is its catalog row and its dependencies.

use super::*;
use crate::ts_configs::QualifiedName;

/// A transform's function: its name parts and argument types.
pub type TransformFunction = (QualifiedName, Vec<String>);

#[derive(Debug, Clone, PartialEq)]
pub enum TransformOp {
    Create {
        replace: bool,
        /// The type as the planner spells it, and as `TypeNameToString`
        /// prints it.
        type_name: String,
        type_written: String,
        language: String,
        from_sql: Option<TransformFunction>,
        to_sql: Option<TransformFunction>,
    },
    Drop {
        type_name: String,
        type_written: String,
        language: String,
        if_exists: bool,
    },
}

impl TransformOp {
    pub fn tag(&self) -> &'static str {
        match self {
            TransformOp::Create { .. } => "CREATE TRANSFORM",
            TransformOp::Drop { .. } => "DROP TRANSFORM",
        }
    }
}

fn function(o: &pg_query::protobuf::ObjectWithArgs) -> TransformFunction {
    (
        o.objname
            .iter()
            .filter_map(|n| match n.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.clone()),
                _ => None,
            })
            .collect(),
        o.objargs
            .iter()
            .filter_map(|n| match n.node.as_ref() {
                Some(N::TypeName(t)) => Some(type_name_of(t)),
                _ => None,
            })
            .collect(),
    )
}

pub(crate) fn plan_create(c: &pg_query::protobuf::CreateTransformStmt) -> Result<Statement> {
    let t = c.type_name.as_ref();
    Ok(Statement::Transform(TransformOp::Create {
        replace: c.replace,
        type_name: t.map(type_name_of).unwrap_or_default(),
        type_written: t
            .map(crate::opclasses::type_name_written)
            .unwrap_or_default(),
        language: c.lang.clone(),
        from_sql: c.fromsql.as_ref().map(function),
        to_sql: c.tosql.as_ref().map(function),
    }))
}

/// `(type, language)` of a transform object: the grammar's
/// `list_make2(TypeName, String)`.
pub(crate) fn object(n: Option<&pg_query::protobuf::Node>) -> (String, String, String) {
    let items = match n.and_then(|n| n.node.as_ref()) {
        Some(N::List(l)) => l.items.clone(),
        _ => Vec::new(),
    };
    let mut ty = (String::new(), String::new());
    let mut lang = String::new();
    for i in &items {
        match i.node.as_ref() {
            Some(N::TypeName(t)) => {
                ty = (type_name_of(t), crate::opclasses::type_name_written(t));
            }
            Some(N::String(s)) => lang = s.sval.clone(),
            _ => {}
        }
    }
    (ty.0, ty.1, lang)
}

pub(crate) fn plan_drop(d: &pg_query::protobuf::DropStmt) -> Result<Statement> {
    let mut drops = Vec::new();
    for o in &d.objects {
        let (type_name, type_written, language) = object(Some(o));
        drops.push(Statement::Transform(TransformOp::Drop {
            type_name,
            type_written,
            language,
            if_exists: d.missing_ok,
        }));
    }
    match drops.len() {
        1 => Ok(drops.remove(0)),
        _ => Ok(Statement::Sequence("DROP TRANSFORM", drops)),
    }
}
