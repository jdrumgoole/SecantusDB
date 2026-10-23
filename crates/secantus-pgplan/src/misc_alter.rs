//! Small `ALTER`s of objects whose catalogs live elsewhere: `ALTER
//! EXTENSION ... UPDATE`, `ALTER OPERATOR ... OWNER TO`, and `ALTER
//! COLLATION ... RENAME TO` / `REFRESH VERSION`. The executor checks and
//! applies them.

use super::*;

/// One of the statements.
#[derive(Debug, Clone, PartialEq)]
pub enum MiscAlter {
    /// `ALTER EXTENSION name UPDATE [TO version]`.
    ExtensionUpdate {
        name: String,
        version: Option<String>,
    },
    /// `ALTER OPERATOR name (left, right) OWNER TO role`, `left` `None`
    /// for a prefix operator.
    OperatorOwner {
        name: String,
        left: Option<String>,
        right: String,
        owner: String,
    },
    /// `ALTER COLLATION name RENAME TO new`.
    CollationRename { name: String, to: String },
    /// `ALTER COLLATION name REFRESH VERSION`.
    CollationRefresh { name: String },
}

impl MiscAlter {
    /// The command tag.
    pub fn tag(&self) -> &'static str {
        match self {
            MiscAlter::ExtensionUpdate { .. } => "ALTER EXTENSION",
            MiscAlter::OperatorOwner { .. } => "ALTER OPERATOR",
            MiscAlter::CollationRename { .. } | MiscAlter::CollationRefresh { .. } => {
                "ALTER COLLATION"
            }
        }
    }
}

/// The last part of a qualified name list: collations live in `public`.
fn last_name(items: &[pg_query::protobuf::Node]) -> String {
    string_list(items).pop().unwrap_or_default()
}

pub(crate) fn plan_extension_update(
    a: &pg_query::protobuf::AlterExtensionStmt,
) -> Result<Statement> {
    let version = a.options.iter().find_map(|o| match o.node.as_ref() {
        Some(N::DefElem(e)) if e.defname == "new_version" => {
            match e.arg.as_deref()?.node.as_ref()? {
                N::String(s) => Some(s.sval.clone()),
                _ => None,
            }
        }
        _ => None,
    });
    Ok(Statement::MiscAlter(MiscAlter::ExtensionUpdate {
        name: a.extname.clone(),
        version,
    }))
}

pub(crate) fn plan_operator_owner(a: &pg_query::protobuf::AlterOwnerStmt) -> Result<Statement> {
    let Some(N::ObjectWithArgs(o)) = a.object.as_deref().and_then(|o| o.node.as_ref()) else {
        return Err(Error::Parse("ALTER OPERATOR without an operator".into()));
    };
    let name = string_list(&o.objname).pop().unwrap_or_default();
    let types: Vec<Option<String>> = o
        .objargs
        .iter()
        .map(|n| match n.node.as_ref() {
            Some(N::TypeName(tn)) => Some(type_name_of(tn)),
            _ => None,
        })
        .collect();
    let (left, right) = match types.as_slice() {
        [l, Some(r)] => (l.clone(), r.clone()),
        [Some(r)] => (None, r.clone()),
        _ => {
            return Err(Error::Parse(
                "ALTER OPERATOR needs its argument types".into(),
            ))
        }
    };
    Ok(Statement::MiscAlter(MiscAlter::OperatorOwner {
        name,
        left,
        right,
        owner: a.newowner.as_ref().map(role_spec_name).unwrap_or_default(),
    }))
}

pub(crate) fn plan_collation_rename(r: &pg_query::protobuf::RenameStmt) -> Result<Statement> {
    let name = match r.object.as_deref().and_then(|o| o.node.as_ref()) {
        Some(N::List(l)) => last_name(&l.items),
        Some(N::String(s)) => s.sval.clone(),
        _ => return Err(Error::Parse("ALTER COLLATION without a collation".into())),
    };
    Ok(Statement::MiscAlter(MiscAlter::CollationRename {
        name,
        to: r.newname.clone(),
    }))
}

pub(crate) fn plan_collation_refresh(
    a: &pg_query::protobuf::AlterCollationStmt,
) -> Result<Statement> {
    Ok(Statement::MiscAlter(MiscAlter::CollationRefresh {
        name: last_name(&a.collname),
    }))
}

/// `CREATE / DROP / ALTER CONVERSION`.
#[derive(Debug, Clone, PartialEq)]
pub enum ConversionOp {
    /// `CREATE [DEFAULT] CONVERSION name FOR 'src' TO 'dst' FROM function`.
    Create {
        name: Vec<String>,
        for_encoding: String,
        to_encoding: String,
        function: Vec<String>,
        default: bool,
    },
    /// `DROP CONVERSION [IF EXISTS] names [CASCADE]`.
    Drop {
        names: Vec<Vec<String>>,
        if_exists: bool,
    },
    /// `ALTER CONVERSION name RENAME TO new`.
    Rename { name: Vec<String>, to: String },
    /// `ALTER CONVERSION name OWNER TO role`.
    Owner { name: Vec<String>, owner: String },
    /// `ALTER CONVERSION name SET SCHEMA s`.
    SetSchema { name: Vec<String>, schema: String },
}

impl ConversionOp {
    pub fn tag(&self) -> &'static str {
        match self {
            ConversionOp::Create { .. } => "CREATE CONVERSION",
            ConversionOp::Drop { .. } => "DROP CONVERSION",
            _ => "ALTER CONVERSION",
        }
    }
}

fn name_parts(n: Option<&pg_query::protobuf::Node>) -> Vec<String> {
    match n.and_then(|n| n.node.as_ref()) {
        Some(N::List(l)) => string_list(&l.items),
        Some(N::String(s)) => vec![s.sval.clone()],
        _ => Vec::new(),
    }
}

pub(crate) fn plan_create_conversion(
    c: &pg_query::protobuf::CreateConversionStmt,
) -> Result<Statement> {
    Ok(Statement::Conversion(ConversionOp::Create {
        name: string_list(&c.conversion_name),
        for_encoding: c.for_encoding_name.clone(),
        to_encoding: c.to_encoding_name.clone(),
        function: string_list(&c.func_name),
        default: c.def,
    }))
}

pub(crate) fn plan_drop_conversion(d: &pg_query::protobuf::DropStmt) -> Result<Statement> {
    Ok(Statement::Conversion(ConversionOp::Drop {
        names: d.objects.iter().map(|o| name_parts(Some(o))).collect(),
        if_exists: d.missing_ok,
    }))
}

pub(crate) fn plan_rename_conversion(r: &pg_query::protobuf::RenameStmt) -> Result<Statement> {
    Ok(Statement::Conversion(ConversionOp::Rename {
        name: name_parts(r.object.as_deref()),
        to: r.newname.clone(),
    }))
}

pub(crate) fn plan_conversion_owner(a: &pg_query::protobuf::AlterOwnerStmt) -> Result<Statement> {
    Ok(Statement::Conversion(ConversionOp::Owner {
        name: name_parts(a.object.as_deref()),
        owner: a.newowner.as_ref().map(role_spec_name).unwrap_or_default(),
    }))
}

pub(crate) fn plan_conversion_schema(
    a: &pg_query::protobuf::AlterObjectSchemaStmt,
) -> Result<Statement> {
    Ok(Statement::Conversion(ConversionOp::SetSchema {
        name: name_parts(a.object.as_deref()),
        schema: a.newschema.clone(),
    }))
}

/// An encoding's number and canonical name (`pg_char_to_encoding`), by
/// any of PostgreSQL's names for it.
pub fn encoding(name: &str) -> Option<(i32, &'static str)> {
    let canonical = crate::builtins_text::encoding_by_name(name)?;
    let id = crate::scalar::PG_ENCODINGS
        .iter()
        .position(|n| *n == canonical)?;
    Some((i32::try_from(id).ok()?, canonical))
}

/// An encoding's canonical name by number (`pg_encoding_to_char`).
pub fn encoding_name(id: i64) -> Option<&'static str> {
    usize::try_from(id)
        .ok()
        .and_then(|i| crate::scalar::PG_ENCODINGS.get(i))
        .copied()
}

/// `CREATE / DROP ACCESS METHOD`.
#[derive(Debug, Clone, PartialEq)]
pub enum AccessMethodOp {
    /// `CREATE ACCESS METHOD name TYPE TABLE | INDEX HANDLER function`:
    /// `kind` is `t` or `i`, as `pg_am.amtype` holds it.
    Create {
        name: String,
        kind: char,
        handler: Vec<String>,
    },
    /// `DROP ACCESS METHOD [IF EXISTS] names [CASCADE]`.
    Drop {
        names: Vec<String>,
        if_exists: bool,
        cascade: bool,
    },
}

impl AccessMethodOp {
    pub fn tag(&self) -> &'static str {
        match self {
            AccessMethodOp::Create { .. } => "CREATE ACCESS METHOD",
            AccessMethodOp::Drop { .. } => "DROP ACCESS METHOD",
        }
    }
}

pub(crate) fn plan_create_am(c: &pg_query::protobuf::CreateAmStmt) -> Result<Statement> {
    Ok(Statement::AccessMethod(AccessMethodOp::Create {
        name: c.amname.clone(),
        kind: c.amtype.chars().next().unwrap_or('t'),
        handler: string_list(&c.handler_name),
    }))
}

pub(crate) fn plan_drop_am(d: &pg_query::protobuf::DropStmt) -> Result<Statement> {
    Ok(Statement::AccessMethod(AccessMethodOp::Drop {
        names: d.objects.iter().flat_map(|o| name_parts(Some(o))).collect(),
        if_exists: d.missing_ok,
        cascade: pg_query::protobuf::DropBehavior::try_from(d.behavior)
            == Ok(pg_query::protobuf::DropBehavior::DropCascade),
    }))
}

/// The index access methods PostgreSQL itself has (`pg_am`, `amtype` `i`).
pub const BUILTIN_INDEX_AMS: [&str; 6] = ["btree", "hash", "gist", "gin", "spgist", "brin"];

thread_local! {
    /// The database's own access methods: `(name, amtype)`.
    static USER_AMS: std::cell::RefCell<Vec<(String, char)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Install the database's access methods for the statements that follow.
pub fn set_user_access_methods(ams: Vec<(String, char)>) {
    USER_AMS.with(|a| *a.borrow_mut() = ams);
}

/// A user access method's `amtype`.
pub(crate) fn user_access_method(name: &str) -> Option<char> {
    USER_AMS.with(|a| a.borrow().iter().find(|(n, _)| n == name).map(|(_, k)| *k))
}

/// The table access method `USING am` names, checked as PostgreSQL's
/// `get_table_am_oid` checks it: `None` for `heap` (and no clause), the
/// name for one of the database's own.
pub(crate) fn table_access_method(name: &str) -> Result<Option<String>> {
    if name.is_empty() || name == "heap" {
        return Ok(None);
    }
    match user_access_method(name) {
        Some('t') => Ok(Some(name.to_string())),
        Some(_) => Err(Error::Sqlstate(
            "55000",
            format!("access method \"{name}\" is not of type TABLE"),
        )),
        None if BUILTIN_INDEX_AMS.contains(&name) => Err(Error::Sqlstate(
            "55000",
            format!("access method \"{name}\" is not of type TABLE"),
        )),
        None => Err(Error::Sqlstate(
            "42704",
            format!("access method \"{name}\" does not exist"),
        )),
    }
}
