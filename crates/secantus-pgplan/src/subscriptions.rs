//! `CREATE / ALTER / DROP SUBSCRIPTION` (RP-169), planned as PostgreSQL
//! 15.19's grammar builds them. The options are handed over as written:
//! the executor (`secantus-pg`'s `subscriptions.rs`) parses them as
//! `parse_subscription_options` does, after the checks PostgreSQL makes
//! first, and keeps the `pg_subscription` rows.

use super::*;

/// An option's value as written.
#[derive(Debug, Clone, PartialEq)]
pub enum OptArg {
    /// No value (`WITH (binary)`).
    None,
    Integer(i64),
    /// Any other value, as `defGetString` reads it.
    Text(String),
}

/// One `WITH (name = value)` option.
#[derive(Debug, Clone, PartialEq)]
pub struct RawOpt {
    pub name: String,
    pub arg: OptArg,
}

/// What an `ALTER SUBSCRIPTION` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlterKind {
    Options,
    Connection,
    SetPublication,
    AddPublication,
    DropPublication,
    Refresh,
    Enabled,
    Skip,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptionOp {
    Create {
        name: String,
        conninfo: String,
        publications: Vec<String>,
        options: Vec<RawOpt>,
    },
    Alter {
        kind: AlterKind,
        name: String,
        conninfo: String,
        publications: Vec<String>,
        options: Vec<RawOpt>,
    },
    Drop {
        name: String,
        if_exists: bool,
    },
    Rename {
        name: String,
        to: String,
    },
    Owner {
        name: String,
        owner: String,
    },
}

impl SubscriptionOp {
    pub fn tag(&self) -> &'static str {
        match self {
            SubscriptionOp::Create { .. } => "CREATE SUBSCRIPTION",
            SubscriptionOp::Drop { .. } => "DROP SUBSCRIPTION",
            _ => "ALTER SUBSCRIPTION",
        }
    }
}

fn arg_of(d: &pg_query::protobuf::DefElem) -> OptArg {
    use pg_query::protobuf::a_const::Val;
    let Some(n) = d.arg.as_deref().and_then(|a| a.node.as_ref()) else {
        return OptArg::None;
    };
    let text = |s: String| OptArg::Text(s);
    match n {
        N::Integer(i) => OptArg::Integer(i64::from(i.ival)),
        N::String(s) => text(s.sval.clone()),
        N::Float(f) => text(f.fval.clone()),
        N::Boolean(b) => text(if b.boolval { "true" } else { "false" }.into()),
        N::TypeName(t) => text(
            t.names
                .iter()
                .filter_map(|n| match n.node.as_ref() {
                    Some(N::String(s)) => Some(s.sval.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("."),
        ),
        N::AConst(c) => match c.val.as_ref() {
            Some(Val::Ival(i)) => OptArg::Integer(i64::from(i.ival)),
            Some(Val::Sval(s)) => text(s.sval.clone()),
            Some(Val::Fval(f)) => text(f.fval.clone()),
            Some(Val::Boolval(b)) => text(if b.boolval { "true" } else { "false" }.into()),
            Some(Val::Bsval(b)) => text(b.bsval.clone()),
            None => OptArg::None,
        },
        _ => OptArg::None,
    }
}

fn options(defs: &[pg_query::protobuf::Node]) -> Vec<RawOpt> {
    defs.iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::DefElem(d)) => Some(RawOpt {
                name: d.defname.clone(),
                arg: arg_of(d),
            }),
            _ => None,
        })
        .collect()
}

fn names(list: &[pg_query::protobuf::Node]) -> Vec<String> {
    list.iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

pub(crate) fn plan_create(c: &pg_query::protobuf::CreateSubscriptionStmt) -> Result<Statement> {
    Ok(Statement::Subscription(SubscriptionOp::Create {
        name: c.subname.clone(),
        conninfo: c.conninfo.clone(),
        publications: names(&c.publication),
        options: options(&c.options),
    }))
}

pub(crate) fn plan_alter(a: &pg_query::protobuf::AlterSubscriptionStmt) -> Result<Statement> {
    use pg_query::protobuf::AlterSubscriptionType as T;
    let kind = match T::try_from(a.kind) {
        Ok(T::AlterSubscriptionOptions) => AlterKind::Options,
        Ok(T::AlterSubscriptionConnection) => AlterKind::Connection,
        Ok(T::AlterSubscriptionSetPublication) => AlterKind::SetPublication,
        Ok(T::AlterSubscriptionAddPublication) => AlterKind::AddPublication,
        Ok(T::AlterSubscriptionDropPublication) => AlterKind::DropPublication,
        Ok(T::AlterSubscriptionRefresh) => AlterKind::Refresh,
        Ok(T::AlterSubscriptionEnabled) => AlterKind::Enabled,
        Ok(T::AlterSubscriptionSkip) => AlterKind::Skip,
        _ => return Err(Error::Unsupported("this ALTER SUBSCRIPTION".into())),
    };
    Ok(Statement::Subscription(SubscriptionOp::Alter {
        kind,
        name: a.subname.clone(),
        conninfo: a.conninfo.clone(),
        publications: names(&a.publication),
        options: options(&a.options),
    }))
}

pub(crate) fn plan_drop(d: &pg_query::protobuf::DropSubscriptionStmt) -> Result<Statement> {
    Ok(Statement::Subscription(SubscriptionOp::Drop {
        name: d.subname.clone(),
        if_exists: d.missing_ok,
    }))
}

fn object_string(n: Option<&pg_query::protobuf::Node>) -> String {
    match n.and_then(|n| n.node.as_ref()) {
        Some(N::String(s)) => s.sval.clone(),
        _ => String::new(),
    }
}

pub(crate) fn plan_rename(r: &pg_query::protobuf::RenameStmt) -> Result<Statement> {
    Ok(Statement::Subscription(SubscriptionOp::Rename {
        name: object_string(r.object.as_deref()),
        to: r.newname.clone(),
    }))
}

pub(crate) fn plan_owner(a: &pg_query::protobuf::AlterOwnerStmt) -> Result<Statement> {
    Ok(Statement::Subscription(SubscriptionOp::Owner {
        name: object_string(a.object.as_deref()),
        owner: a.newowner.as_ref().map(role_spec_name).unwrap_or_default(),
    }))
}
