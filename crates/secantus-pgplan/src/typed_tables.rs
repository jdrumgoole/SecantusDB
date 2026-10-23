//! `CREATE TABLE t OF type`: a typed table, whose columns are the composite
//! type's attributes, in order, with `a WITH OPTIONS ...` adding a column's
//! constraints and defaults. The table records its type (`extra.of_type`,
//! the type's resolution name), which `pg_class.reloftype` reports and
//! which keeps its columns fixed: ALTER TABLE cannot add, drop or rename one
//! (42809), and `ALTER TYPE ... CASCADE` changes them with the type.

use super::*;

/// The `extra` key a typed table records its type under.
pub const OF_TYPE_KEY: &str = "of_type";

/// `c` with an `OF type` clause turned into the type's columns, and the
/// type's resolution name; `None` when `c` has no OF clause.
pub(crate) fn lower(
    c: &pg_query::protobuf::CreateStmt,
) -> Result<Option<(pg_query::protobuf::CreateStmt, String)>> {
    let Some(of) = c.of_typename.as_ref() else {
        return Ok(None);
    };
    let name = type_name_of(of);
    let Some((_, fields)) = user_composite(&name) else {
        if pgtypes::oid_of_name(&name).is_some() || user_type_oid(&name).is_some() {
            return Err(Error::Sqlstate(
                "42809",
                format!("type {} is not a composite type", display_type(&name)),
            ));
        }
        return Err(Error::Sqlstate(
            "42704",
            format!(
                "type \"{}\" does not exist",
                pgtypes::user_type_catalog_name(&name)
            ),
        ));
    };
    // `a WITH OPTIONS ...`: each names an attribute of the type.
    let mut options: Vec<pg_query::protobuf::ColumnDef> = Vec::new();
    let mut constraints = Vec::new();
    for e in &c.table_elts {
        match e.node.as_ref() {
            Some(N::ColumnDef(cd)) => {
                if !fields.iter().any(|(f, _)| *f == cd.colname) {
                    return Err(Error::Sqlstate(
                        "42703",
                        format!("column \"{}\" does not exist", cd.colname),
                    ));
                }
                options.push((**cd).clone());
            }
            _ => constraints.push(e.clone()),
        }
    }
    let mut elts = Vec::with_capacity(fields.len() + constraints.len());
    for (field, ty) in &fields {
        let mut cd = column_def_of(field, ty)?;
        if let Some(o) = options.iter().find(|o| o.colname == *field) {
            cd.constraints = o.constraints.clone();
            cd.raw_default = o.raw_default.clone();
        }
        elts.push(pg_query::protobuf::Node {
            node: Some(N::ColumnDef(Box::new(cd))),
        });
    }
    elts.extend(constraints);
    let mut out = c.clone();
    out.table_elts = elts;
    out.of_typename = None;
    Ok(Some((out, canonical_type_ref(&name))))
}

/// A column definition of `name` with the type the catalog spells `ty`,
/// parsed back from SQL so the type reaches the planner as any declared
/// column's does.
fn column_def_of(name: &str, ty: &str) -> Result<pg_query::protobuf::ColumnDef> {
    let sql = format!("CREATE TABLE x ({} {})", scalar::quote_identifier(name), ty);
    let N::CreateStmt(c) = parse_one(&sql)? else {
        return Err(Error::Parse(format!(
            "the type {ty} of a typed table's column"
        )));
    };
    match c.table_elts.first().and_then(|e| e.node.as_ref()) {
        Some(N::ColumnDef(cd)) => Ok((**cd).clone()),
        _ => Err(Error::Parse(format!(
            "the type {ty} of a typed table's column"
        ))),
    }
}

/// One action of `ALTER TYPE t ...` on a composite's attributes; `cascade`
/// carries it to the type's typed tables.
#[derive(Debug, Clone, PartialEq)]
pub enum AttributeAction {
    /// `ADD ATTRIBUTE name type`.
    Add {
        name: String,
        ty: String,
        typmod: i32,
        cascade: bool,
    },
    /// `DROP ATTRIBUTE [IF EXISTS] name`.
    Drop {
        name: String,
        if_exists: bool,
        cascade: bool,
    },
    /// `ALTER ATTRIBUTE name [SET DATA] TYPE type`.
    AlterType {
        name: String,
        ty: String,
        typmod: i32,
        cascade: bool,
    },
}

/// `ALTER TYPE t ADD | DROP | ALTER ATTRIBUTE ...` (PostgreSQL plans it as
/// an ALTER TABLE of the type's relation).
pub(crate) fn plan_alter_type(
    a: &pg_query::protobuf::AlterTableStmt,
    relation: &pg_query::protobuf::RangeVar,
) -> Result<Statement> {
    use pg_query::protobuf::{AlterTableType as AT, DropBehavior};
    let name = if matches!(relation.schemaname.as_str(), "" | "public") {
        pgtypes::user_type_key(&relation.relname)
    } else {
        format!("{}.{}", relation.schemaname, relation.relname)
    };
    let mut actions = Vec::with_capacity(a.cmds.len());
    for cmd in &a.cmds {
        let Some(N::AlterTableCmd(cmd)) = cmd.node.as_ref() else {
            return Err(Error::Unsupported("this ALTER TYPE action".into()));
        };
        let cascade = DropBehavior::try_from(cmd.behavior) == Ok(DropBehavior::DropCascade);
        let typed = |cmd: &pg_query::protobuf::AlterTableCmd| -> Result<(String, String, i32)> {
            let Some(N::ColumnDef(cd)) = cmd.def.as_ref().and_then(|d| d.node.as_ref()) else {
                return Err(Error::Parse("ALTER TYPE attribute without a type".into()));
            };
            let tn = cd
                .type_name
                .as_ref()
                .ok_or_else(|| Error::Parse("ALTER TYPE attribute without a type".into()))?;
            let attr = if cd.colname.is_empty() {
                cmd.name.clone()
            } else {
                cd.colname.clone()
            };
            Ok((attr, type_name_of(tn), declared_typmod(tn)))
        };
        actions.push(match AT::try_from(cmd.subtype) {
            Ok(AT::AtAddColumn) => {
                let (name, ty, typmod) = typed(cmd)?;
                AttributeAction::Add {
                    name,
                    ty,
                    typmod,
                    cascade,
                }
            }
            Ok(AT::AtDropColumn) => AttributeAction::Drop {
                name: cmd.name.clone(),
                if_exists: cmd.missing_ok,
                cascade,
            },
            Ok(AT::AtAlterColumnType) => {
                let (_, ty, typmod) = typed(cmd)?;
                AttributeAction::AlterType {
                    name: cmd.name.clone(),
                    ty,
                    typmod,
                    cascade,
                }
            }
            _ => return Err(Error::Unsupported("this ALTER TYPE action".into())),
        });
    }
    Ok(Statement::AlterTypeAttributes { name, actions })
}

/// `ALTER TABLE t OF type`: the table's columns must be the type's
/// attributes, by name and type, in order (`ATExecAddOf`'s 42804s).
pub(crate) fn plan_add_of(
    cmd: &pg_query::protobuf::AlterTableCmd,
    def: &TableDef,
) -> Result<AlterTableAction> {
    let Some(N::TypeName(tn)) = cmd.def.as_deref().and_then(|d| d.node.as_ref()) else {
        return Err(Error::Parse("OF without a type".into()));
    };
    let name = type_name_of(tn);
    let Some((_, fields)) = user_composite(&name) else {
        if pgtypes::oid_of_name(&name).is_some() || user_type_oid(&name).is_some() {
            return Err(Error::Sqlstate(
                "42809",
                format!("type {} is not a composite type", display_type(&name)),
            ));
        }
        return Err(Error::Sqlstate(
            "42704",
            format!(
                "type \"{}\" does not exist",
                pgtypes::user_type_catalog_name(&name)
            ),
        ));
    };
    let mismatch = |m: String| Error::Sqlstate("42804", m);
    let same_type = |a: &str, b: &str| {
        a == b
            || matches!(
                (pgtypes::oid_of_name(a), pgtypes::oid_of_name(b)),
                (Some(x), Some(y)) if x == y
            )
    };
    for (i, (field, ty)) in fields.iter().enumerate() {
        let Some(c) = def.columns.get(i) else {
            return Err(mismatch(format!("table is missing column \"{field}\"")));
        };
        if c.name != *field {
            return Err(mismatch(format!(
                "table has column \"{}\" where type requires \"{field}\"",
                c.name
            )));
        }
        if !same_type(&c.pg_type, ty) {
            return Err(mismatch(format!(
                "table \"{}\" has different type for column \"{}\"",
                schemas::split_key(&def.name).1,
                c.name
            )));
        }
    }
    if let Some(extra) = def.columns.get(fields.len()) {
        return Err(mismatch(format!(
            "table has extra column \"{}\"",
            extra.name
        )));
    }
    Ok(AlterTableAction::OfType(Some(canonical_type_ref(&name))))
}

/// `ALTER TABLE t NOT OF`.
pub(crate) fn plan_drop_of(table: &str, def: &TableDef) -> Result<AlterTableAction> {
    if def.extra.get_str(OF_TYPE_KEY).is_err() {
        return Err(Error::Sqlstate(
            "42809",
            format!("\"{}\" is not a typed table", schemas::split_key(table).1),
        ));
    }
    Ok(AlterTableAction::OfType(None))
}
