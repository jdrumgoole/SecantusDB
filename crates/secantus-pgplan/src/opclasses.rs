//! `CREATE / ALTER / DROP OPERATOR FAMILY` and `OPERATOR CLASS` (RP-169),
//! as PostgreSQL 15.19's `opclasscmds.c` plans them: the statement's names,
//! types and members as written. The executor (`secantus-pg`'s
//! `opclasses.rs`) resolves them against the catalog, checks them as
//! `DefineOpClass` / `AlterOpFamily` do, and keeps the `pg_opfamily`,
//! `pg_opclass`, `pg_amop` and `pg_amproc` rows.

use super::*;
use crate::ts_configs::QualifiedName;

/// What an `OPERATOR` / `FUNCTION` / `STORAGE` item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    Operator,
    Function,
    Storage,
}

/// One item of `CREATE OPERATOR CLASS ... AS` or `ALTER OPERATOR FAMILY ...
/// ADD / DROP`.
#[derive(Debug, Clone, PartialEq)]
pub struct OpClassItem {
    pub kind: MemberKind,
    pub number: i32,
    /// The operator's or function's name parts.
    pub name: QualifiedName,
    /// Its argument types as written (`None` for an operator's `NONE`), or
    /// `None` when the item gave none.
    pub args: Option<Vec<Option<String>>>,
    /// `FOR ORDER BY family`.
    pub order_family: Option<QualifiedName>,
    /// The `(left[, right])` types before a function, or of a DROP item.
    pub class_args: Vec<String>,
    /// `STORAGE type`.
    pub storage: Option<String>,
}

/// An operator family or class statement. `class` is true for `OPERATOR
/// CLASS`, false for `OPERATOR FAMILY`.
#[derive(Debug, Clone, PartialEq)]
pub enum OpClassOp {
    CreateFamily {
        name: QualifiedName,
        method: String,
    },
    CreateClass {
        name: QualifiedName,
        method: String,
        /// The indexed type, as the planner spells it.
        datatype: String,
        /// The same as `TypeNameToString` prints it (`pg_catalog.int4`).
        datatype_written: String,
        family: Option<QualifiedName>,
        default: bool,
        items: Vec<OpClassItem>,
    },
    AlterFamily {
        name: QualifiedName,
        method: String,
        drop: bool,
        items: Vec<OpClassItem>,
    },
    Drop {
        class: bool,
        name: QualifiedName,
        method: String,
        if_exists: bool,
        cascade: bool,
    },
    Rename {
        class: bool,
        name: QualifiedName,
        method: String,
        to: String,
    },
    Owner {
        class: bool,
        name: QualifiedName,
        method: String,
        owner: String,
    },
    SetSchema {
        class: bool,
        name: QualifiedName,
        method: String,
        schema: String,
    },
}

impl OpClassOp {
    pub fn tag(&self) -> &'static str {
        let class = match self {
            OpClassOp::CreateFamily { .. } | OpClassOp::AlterFamily { .. } => false,
            OpClassOp::CreateClass { .. } => true,
            OpClassOp::Drop { class, .. }
            | OpClassOp::Rename { class, .. }
            | OpClassOp::Owner { class, .. }
            | OpClassOp::SetSchema { class, .. } => *class,
        };
        match (self, class) {
            (OpClassOp::CreateFamily { .. }, _) => "CREATE OPERATOR FAMILY",
            (OpClassOp::CreateClass { .. }, _) => "CREATE OPERATOR CLASS",
            (OpClassOp::AlterFamily { .. }, _) => "ALTER OPERATOR FAMILY",
            (OpClassOp::Drop { .. }, true) => "DROP OPERATOR CLASS",
            (OpClassOp::Drop { .. }, false) => "DROP OPERATOR FAMILY",
            (_, true) => "ALTER OPERATOR CLASS",
            (_, false) => "ALTER OPERATOR FAMILY",
        }
    }
}

fn strings(list: &[pg_query::protobuf::Node]) -> QualifiedName {
    list.iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// `TypeNameToString`: the name's parts dotted, `[]` for an array.
pub(crate) fn type_name_written(t: &pg_query::protobuf::TypeName) -> String {
    let mut s = strings(&t.names).join(".");
    if !t.array_bounds.is_empty() {
        s.push_str("[]");
    }
    s
}

fn type_names(list: &[pg_query::protobuf::Node]) -> Vec<String> {
    list.iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::TypeName(t)) => Some(type_name_of(t)),
            _ => None,
        })
        .collect()
}

fn items(list: &[pg_query::protobuf::Node]) -> Vec<OpClassItem> {
    list.iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::CreateOpClassItem(i)) => Some(i),
            _ => None,
        })
        .map(|i| OpClassItem {
            kind: match i.itemtype {
                1 => MemberKind::Operator,
                2 => MemberKind::Function,
                _ => MemberKind::Storage,
            },
            number: i.number,
            name: i
                .name
                .as_ref()
                .map(|o| strings(&o.objname))
                .unwrap_or_default(),
            args: i.name.as_ref().and_then(|o| {
                (!o.objargs.is_empty() || !o.args_unspecified && i.itemtype == 2).then(|| {
                    o.objargs
                        .iter()
                        .map(|a| match a.node.as_ref() {
                            Some(N::TypeName(t)) => Some(type_name_of(t)),
                            _ => None,
                        })
                        .collect()
                })
            }),
            order_family: (!i.order_family.is_empty()).then(|| strings(&i.order_family)),
            class_args: type_names(&i.class_args),
            storage: i.storedtype.as_ref().map(type_name_of),
        })
        .collect()
}

pub(crate) fn plan_create_family(c: &pg_query::protobuf::CreateOpFamilyStmt) -> Result<Statement> {
    Ok(Statement::OpClass(OpClassOp::CreateFamily {
        name: strings(&c.opfamilyname),
        method: c.amname.clone(),
    }))
}

pub(crate) fn plan_create_class(c: &pg_query::protobuf::CreateOpClassStmt) -> Result<Statement> {
    let t = c.datatype.as_ref();
    Ok(Statement::OpClass(OpClassOp::CreateClass {
        name: strings(&c.opclassname),
        method: c.amname.clone(),
        datatype: t.map(type_name_of).unwrap_or_default(),
        datatype_written: t.map(type_name_written).unwrap_or_default(),
        family: (!c.opfamilyname.is_empty()).then(|| strings(&c.opfamilyname)),
        default: c.is_default,
        items: items(&c.items),
    }))
}

pub(crate) fn plan_alter_family(a: &pg_query::protobuf::AlterOpFamilyStmt) -> Result<Statement> {
    Ok(Statement::OpClass(OpClassOp::AlterFamily {
        name: strings(&a.opfamilyname),
        method: a.amname.clone(),
        drop: a.is_drop,
        items: items(&a.items),
    }))
}

/// An object `[method, schema?, name]`, as the grammar builds `name USING
/// method`.
fn method_and_name(n: Option<&pg_query::protobuf::Node>) -> (String, QualifiedName) {
    let parts = match n.and_then(|n| n.node.as_ref()) {
        Some(N::List(l)) => strings(&l.items),
        _ => Vec::new(),
    };
    match parts.split_first() {
        Some((method, name)) => (method.clone(), name.to_vec()),
        None => (String::new(), Vec::new()),
    }
}

pub(crate) fn is_class(object_type: i32) -> Option<bool> {
    match ObjectType::try_from(object_type) {
        Ok(ObjectType::ObjectOpclass) => Some(true),
        Ok(ObjectType::ObjectOpfamily) => Some(false),
        _ => None,
    }
}

pub(crate) fn plan_drop(d: &pg_query::protobuf::DropStmt, class: bool) -> Result<Statement> {
    let mut drops = Vec::new();
    for o in &d.objects {
        let (method, name) = method_and_name(Some(o));
        drops.push(Statement::OpClass(OpClassOp::Drop {
            class,
            name,
            method,
            if_exists: d.missing_ok,
            cascade: pg_query::protobuf::DropBehavior::try_from(d.behavior)
                == Ok(pg_query::protobuf::DropBehavior::DropCascade),
        }));
    }
    match drops.len() {
        1 => Ok(drops.remove(0)),
        _ => Ok(Statement::Sequence(
            if class {
                "DROP OPERATOR CLASS"
            } else {
                "DROP OPERATOR FAMILY"
            },
            drops,
        )),
    }
}

pub(crate) fn plan_rename(r: &pg_query::protobuf::RenameStmt, class: bool) -> Result<Statement> {
    let (method, name) = method_and_name(r.object.as_deref());
    Ok(Statement::OpClass(OpClassOp::Rename {
        class,
        name,
        method,
        to: r.newname.clone(),
    }))
}

pub(crate) fn plan_owner(a: &pg_query::protobuf::AlterOwnerStmt, class: bool) -> Result<Statement> {
    let (method, name) = method_and_name(a.object.as_deref());
    Ok(Statement::OpClass(OpClassOp::Owner {
        class,
        name,
        method,
        owner: a.newowner.as_ref().map(role_spec_name).unwrap_or_default(),
    }))
}

pub(crate) fn plan_set_schema(
    a: &pg_query::protobuf::AlterObjectSchemaStmt,
    class: bool,
) -> Result<Statement> {
    let (method, name) = method_and_name(a.object.as_deref());
    Ok(Statement::OpClass(OpClassOp::SetSchema {
        class,
        name,
        method,
        schema: a.newschema.clone(),
    }))
}
