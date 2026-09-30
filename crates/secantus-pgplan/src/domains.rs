//! `CREATE DOMAIN`: a base type with a NOT NULL, CHECK constraints and a
//! default.
//!
//! A domain column is stored as its BASE type (PostgreSQL sends the base
//! type's oid on the wire too) and tagged with `domain_type` in the catalog,
//! which is how the Python server records one. Every value written to it,
//! and every `x::domain` cast, is checked here: NULL against NOT NULL
//! (23502), then each CHECK in declared order with `VALUE` bound to the value
//! (23514; a NULL verdict passes, SQL's rule). Messages measured on
//! PostgreSQL 14.

use super::*;

/// One domain, as the server publishes it to the planner.
#[derive(Debug, Clone, PartialEq)]
pub struct Domain {
    pub name: String,
    pub oid: i64,
    /// The base type, as a pg type name (`int4`, `text`, ...).
    pub base: String,
    pub typmod: i32,
    pub not_null: bool,
    /// `(constraint name, expression SQL)` in declared order.
    pub checks: Vec<(String, String)>,
    /// The DEFAULT expression's SQL.
    pub default_sql: Option<String>,
}

thread_local! {
    static PLAN_USER_DOMAINS: std::cell::RefCell<Vec<Domain>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Install this database's domains.
pub fn set_user_domains(domains: Vec<Domain>) {
    PLAN_USER_DOMAINS.with(|d| *d.borrow_mut() = domains);
}

/// A domain by (case-folded unless quoted) name.
pub fn user_domain(name: &str) -> Option<Domain> {
    let trimmed = name.trim();
    let target = match trimmed.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Some(inner) => inner.to_string(),
        None => trimmed.to_ascii_lowercase(),
    };
    let target = target
        .strip_prefix("public.")
        .unwrap_or(&target)
        .to_string();
    PLAN_USER_DOMAINS.with(|d| d.borrow().iter().find(|x| x.name == target).cloned())
}

/// A domain's name by its type oid.
pub fn domain_name_of_oid(oid: i64) -> Option<String> {
    PLAN_USER_DOMAINS.with(|d| {
        d.borrow()
            .iter()
            .find(|x| x.oid == oid)
            .map(|x| x.name.clone())
    })
}

/// A domain's base type, for the wire.
pub fn domain_base(name: &str) -> Option<String> {
    user_domain(name).map(|d| d.base)
}

/// Check `value` against domain `name`: 23502 for a NULL the domain refuses,
/// 23514 naming the first CHECK that is false.
pub fn domain_check(name: &str, value: &Bson) -> Result<()> {
    let Some(d) = user_domain(name) else {
        return Ok(());
    };
    if *value == Bson::Null && d.not_null {
        return Err(Error::Sqlstate(
            "23502",
            format!("domain {} does not allow null values", d.name),
        ));
    }
    // Every CHECK is evaluated on NULL too; `value > 0` is NULL, which
    // passes, but `value IS NOT NULL` fails.
    for (cname, expr) in &d.checks {
        let parsed =
            pg_query::parse(&format!("SELECT {expr}")).map_err(|e| Error::Parse(e.to_string()))?;
        let Some(N::SelectStmt(sel)) = parsed
            .protobuf
            .stmts
            .first()
            .and_then(|s| s.stmt.as_ref())
            .and_then(|s| s.node.clone())
        else {
            continue;
        };
        let Some(N::ResTarget(rt)) = sel.target_list.first().and_then(|t| t.node.clone()) else {
            continue;
        };
        let Some(mut node) = rt.val.map(|v| *v) else {
            continue;
        };
        // `VALUE` is the value being checked, typed as the base type.
        walk_expr(&mut node, &mut |n| {
            if let Some(N::ColumnRef(c)) = n.node.as_ref() {
                if column_ref_name(c).is_some_and(|v| v.eq_ignore_ascii_case("value"))
                    && c.fields.len() == 1
                {
                    *n = pg_query::protobuf::Node {
                        node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
                            arg: Some(Box::new(pg_query::protobuf::Node {
                                node: Some(N::ParamRef(pg_query::protobuf::ParamRef {
                                    number: 1,
                                    location: -1,
                                })),
                            })),
                            type_name: Some(type_name_node(&d.base)),
                            location: -1,
                        }))),
                    };
                }
            }
            Ok(())
        })?;
        if const_value(&node, std::slice::from_ref(value))? == Bson::Boolean(false) {
            return Err(Error::Sqlstate(
                "23514",
                format!(
                    "value for domain {} violates check constraint \"{cname}\"",
                    d.name
                ),
            ));
        }
    }
    Ok(())
}

/// A constraint list's pieces: NOT NULL, named CHECKs (unnamed ones take
/// `<domain>_check`, then `<domain>_check1`, ... as PostgreSQL numbers
/// them), and a DEFAULT.
type DomainConstraints = (Option<bool>, Vec<(String, String)>, Option<String>);

fn constraints_of(
    domain: &str,
    nodes: &[pg_query::protobuf::Node],
    existing: &[String],
) -> Result<DomainConstraints> {
    use pg_query::protobuf::ConstrType as CT;
    let mut not_null = None;
    let mut checks: Vec<(String, String)> = Vec::new();
    let mut default = None;
    for n in nodes {
        let Some(N::Constraint(k)) = n.node.as_ref() else {
            continue;
        };
        match CT::try_from(k.contype) {
            Ok(CT::ConstrNotnull) => not_null = Some(true),
            Ok(CT::ConstrNull) => not_null = Some(false),
            Ok(CT::ConstrCheck) => {
                let raw = k
                    .raw_expr
                    .as_deref()
                    .ok_or_else(|| Error::Parse("CHECK without an expression".into()))?;
                let name = if k.conname.is_empty() {
                    let mut candidate = format!("{domain}_check");
                    let mut i = 0;
                    while existing.contains(&candidate)
                        || checks.iter().any(|(c, _)| *c == candidate)
                    {
                        i += 1;
                        candidate = format!("{domain}_check{i}");
                    }
                    candidate
                } else {
                    k.conname.clone()
                };
                checks.push((name, deparse_expr(raw)?));
            }
            Ok(CT::ConstrDefault) => {
                let raw = k
                    .raw_expr
                    .as_deref()
                    .ok_or_else(|| Error::Parse("DEFAULT without an expression".into()))?;
                default = Some(deparse_expr(raw)?);
            }
            _ => return Err(Error::Unsupported("this domain constraint".into())),
        }
    }
    Ok((not_null, checks, default))
}

fn domain_name(parts: &[pg_query::protobuf::Node]) -> String {
    let names: Vec<String> = parts
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect();
    match names.as_slice() {
        [schema, bare] if schema == "public" => bare.clone(),
        _ => names.join("."),
    }
}

/// `CREATE DOMAIN name AS type [constraints]`.
pub(crate) fn plan_create_domain(c: &pg_query::protobuf::CreateDomainStmt) -> Result<Statement> {
    let name = domain_name(&c.domainname);
    let tn = c
        .type_name
        .as_ref()
        .ok_or_else(|| Error::Parse("CREATE DOMAIN without a type".into()))?;
    let base = type_name_of(tn);
    let (not_null, checks, default_sql) = constraints_of(&name, &c.constraints, &[])?;
    Ok(Statement::CreateDomain(Domain {
        name,
        oid: 0,
        base,
        typmod: declared_typmod(tn),
        not_null: not_null.unwrap_or(false),
        checks,
        default_sql,
    }))
}

/// What an `ALTER DOMAIN` does.
#[derive(Debug, Clone, PartialEq)]
pub enum DomainChange {
    SetDefault(Option<String>),
    NotNull(bool),
    AddCheck(String, String),
    DropConstraint { name: String, missing_ok: bool },
}

/// `ALTER DOMAIN name ...`.
pub(crate) fn plan_alter_domain(a: &pg_query::protobuf::AlterDomainStmt) -> Result<Statement> {
    let name = domain_name(&a.type_name);
    let change = match a.subtype.as_str() {
        "T" => DomainChange::SetDefault(match a.def.as_deref() {
            Some(expr) => Some(deparse_expr(expr)?),
            None => None,
        }),
        "N" => DomainChange::NotNull(false),
        "O" => DomainChange::NotNull(true),
        "C" => {
            let existing = user_domain(&name)
                .map(|d| d.checks.into_iter().map(|(n, _)| n).collect::<Vec<_>>())
                .unwrap_or_default();
            let nodes: Vec<pg_query::protobuf::Node> =
                a.def.as_deref().cloned().into_iter().collect();
            let (_, mut checks, _) = constraints_of(&name, &nodes, &existing)?;
            let (cname, expr) = checks
                .pop()
                .ok_or_else(|| Error::Unsupported("this ALTER DOMAIN constraint".into()))?;
            DomainChange::AddCheck(cname, expr)
        }
        "X" => DomainChange::DropConstraint {
            name: a.name.clone(),
            missing_ok: a.missing_ok,
        },
        _ => return Err(Error::Unsupported("this ALTER DOMAIN".into())),
    };
    Ok(Statement::AlterDomain { name, change })
}

/// The `SET DEFAULT` a column inheriting its domain's default takes when the
/// domain's default changes (`None`: DROP DEFAULT).
pub fn inherited_default(
    column: &str,
    pg_type: &str,
    sql: Option<&str>,
) -> Result<AlterTableAction> {
    let Some(sql) = sql else {
        return Ok(AlterTableAction::SetDefault {
            column: column.to_string(),
            value: None,
            expr: None,
            sql: None,
        });
    };
    let raw = parse_default_sql(sql)?;
    let (value, expr) = match default_value_or_expr(&raw, pg_type, &[])? {
        DefaultSpec::Value(v) => (Some(v), None),
        DefaultSpec::Expr(e) => (None, Some(e)),
    };
    Ok(AlterTableAction::SetDefault {
        column: column.to_string(),
        value,
        expr,
        sql: ruleutils_default(&raw),
    })
}

/// A DEFAULT's SQL as the expression node it parses to.
pub(crate) fn parse_default_sql(sql: &str) -> Result<pg_query::protobuf::Node> {
    pg_query::parse(&format!("SELECT {sql}"))
        .map_err(|e| Error::Parse(e.to_string()))?
        .protobuf
        .stmts
        .first()
        .and_then(|s| s.stmt.clone())
        .and_then(|s| match s.node {
            Some(N::SelectStmt(sel)) => sel.target_list.first().cloned(),
            _ => None,
        })
        .and_then(|t| match t.node {
            Some(N::ResTarget(rt)) => rt.val.map(|v| *v),
            _ => None,
        })
        .ok_or_else(|| Error::Parse(format!("a default of {sql}")))
}
