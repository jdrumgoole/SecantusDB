//! `CREATE CAST` / `DROP CAST`: a conversion between two types that the
//! user defines -- through a function (`WITH FUNCTION f(t)`), through the
//! types' text forms (`WITH INOUT`), or as a relabelling (`WITHOUT
//! FUNCTION`). The executor stores them and installs them here; a cast
//! whose source and target match one is evaluated through it.
//!
//! The context decides WHERE a cast applies, as in PostgreSQL: an explicit
//! cast (`x::t`, `CAST(x AS t)`) uses any of them; an assignment (a value
//! stored into a column) uses `AS ASSIGNMENT` and `AS IMPLICIT` ones.

use super::*;
use pg_query::protobuf::CoercionContext;

/// One user cast.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UserCast {
    /// The type names as written (canonicalised).
    pub source: String,
    pub target: String,
    pub source_oid: i64,
    pub target_oid: i64,
    /// `WITH FUNCTION name(args)`: the name and declared argument types.
    pub function: Option<(String, Vec<String>)>,
    /// `f` function, `i` I/O conversion, `b` binary.
    pub method: char,
    /// `e` explicit, `a` assignment, `i` implicit.
    pub context: char,
    /// Assigned by the executor; 0 at plan time.
    pub oid: i64,
}

thread_local! {
    static USER_CASTS: std::cell::RefCell<Vec<UserCast>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Install the database's user casts for the statements that follow.
pub fn set_user_casts(casts: Vec<UserCast>) {
    USER_CASTS.with(|c| *c.borrow_mut() = casts);
}

/// A type's oid: built in, or the database's own (enum, composite, domain,
/// range, base type, and their arrays).
pub fn type_oid(name: &str) -> Option<i64> {
    pgtypes::oid_of_name(name).or_else(|| user_type_or_array_oid(name))
}

/// The user cast from `source` to `target`, when there is one.
pub(crate) fn find(source: &str, target: &str) -> Option<UserCast> {
    if USER_CASTS.with(|c| c.borrow().is_empty()) {
        return None;
    }
    let (s, t) = (type_oid(source)?, type_oid(target)?);
    USER_CASTS.with(|c| {
        c.borrow()
            .iter()
            .find(|u| u.source_oid == s && u.target_oid == t)
            .cloned()
    })
}

/// Is `target` the target of any user cast?
pub(crate) fn casts_to(target: &str) -> bool {
    let Some(t) = type_oid(target) else {
        return false;
    };
    USER_CASTS.with(|c| c.borrow().iter().any(|u| u.target_oid == t))
}

/// Does a chain of casts from `source` take a user cast at any step? Such a
/// chain cannot ride the projection's cast fast path, which runs where no
/// user function can be called.
pub(crate) fn in_chain(source: Option<&str>, chain: &[String]) -> bool {
    if USER_CASTS.with(|c| c.borrow().is_empty()) {
        return false;
    }
    let mut from = source.map(str::to_string);
    for target in chain {
        if from.as_deref().is_some_and(|f| find(f, target).is_some()) {
            return true;
        }
        from = Some(target.clone());
    }
    false
}

/// Is any user cast installed?
pub fn any() -> bool {
    USER_CASTS.with(|c| !c.borrow().is_empty())
}

/// An ASSIGNMENT of `value`, of type oid `source_oid`, to a `target` column:
/// `Some` converted value when an assignment (or implicit) user cast joins
/// the two, `None` when none does.
pub fn assign(value: &Bson, source_oid: i64, target: &str) -> Result<Option<Bson>> {
    let Some(t) = type_oid(target) else {
        return Ok(None);
    };
    let cast = USER_CASTS.with(|c| {
        c.borrow()
            .iter()
            .find(|u| {
                u.source_oid == source_oid && u.target_oid == t && matches!(u.context, 'a' | 'i')
            })
            .cloned()
    });
    cast.map(|c| apply(&c, value.clone())).transpose()
}

/// A user cast usable in an ASSIGNMENT (`AS ASSIGNMENT` / `AS IMPLICIT`).
pub(crate) fn find_assignment(source: &str, target: &str) -> Option<UserCast> {
    find(source, target).filter(|c| matches!(c.context, 'a' | 'i'))
}

/// Convert `value` through `cast`.
pub(crate) fn apply(cast: &UserCast, value: Bson) -> Result<Bson> {
    match cast.method {
        'f' => {
            let (name, args) = cast.function.clone().unwrap_or_default();
            let same = |a: &str, b: &str| match (type_oid(a), type_oid(b)) {
                (Some(x), Some(y)) => x == y,
                _ => a.eq_ignore_ascii_case(b),
            };
            if let Some(u) = correlated::user_functions().into_iter().find(|u| {
                u.name == name
                    && u.arg_types.len() == args.len()
                    && u.arg_types.iter().zip(&args).all(|(p, a)| same(p, a))
            }) {
                return match correlated::call_user_function(&u, &[value])? {
                    correlated::FnResult::Value(v) => Ok(v),
                    correlated::FnResult::Rows(..) => Ok(Bson::Null),
                };
            }
            scalar::call(&name, &[value]).unwrap_or_else(|| {
                Err(Error::UndefinedFunction(format!(
                    "function {name}({}) does not exist",
                    args.iter()
                        .map(|a| display_type(a))
                        .collect::<Vec<_>>()
                        .join(", ")
                )))
            })
        }
        // Through the text forms: the source's output, the target's input.
        'i' => match value {
            Bson::Null => Ok(Bson::Null),
            v => cast_value(Bson::String(value_text(&v)), &cast.target),
        },
        _ => Ok(value),
    }
}

fn type_name(tn: Option<&pg_query::protobuf::TypeName>) -> Result<String> {
    let tn = tn.ok_or_else(|| Error::Parse("a cast needs its types".into()))?;
    Ok(type_name_of(tn))
}

/// `CREATE CAST (source AS target) ...`.
pub(crate) fn plan_create(c: &pg_query::protobuf::CreateCastStmt) -> Result<Statement> {
    let source = type_name(c.sourcetype.as_ref())?;
    let target = type_name(c.targettype.as_ref())?;
    let function = c.func.as_ref().map(|f| {
        let name = f
            .objname
            .iter()
            .rev()
            .find_map(|n| match n.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let args = f
            .objargs
            .iter()
            .filter_map(|n| match n.node.as_ref() {
                Some(N::TypeName(t)) => Some(type_name_of(t)),
                _ => None,
            })
            .collect();
        (name, args)
    });
    let method = if function.is_some() {
        'f'
    } else if c.inout {
        'i'
    } else {
        'b'
    };
    let context = match CoercionContext::try_from(c.context) {
        Ok(CoercionContext::CoercionImplicit) => 'i',
        Ok(CoercionContext::CoercionAssignment) => 'a',
        _ => 'e',
    };
    Ok(Statement::CreateCast(UserCast {
        source,
        target,
        function,
        method,
        context,
        ..Default::default()
    }))
}

/// `DROP CAST [IF EXISTS] (source AS target)`.
pub(crate) fn plan_drop(d: &pg_query::protobuf::DropStmt) -> Result<Statement> {
    let [obj] = d.objects.as_slice() else {
        return Err(Error::Unsupported("DROP CAST of more than one cast".into()));
    };
    let Some(N::List(l)) = obj.node.as_ref() else {
        return Err(Error::Unsupported("this DROP CAST target".into()));
    };
    let types: Vec<String> = l
        .items
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::TypeName(t)) => Some(type_name_of(t)),
            _ => None,
        })
        .collect();
    let [source, target] = types.as_slice() else {
        return Err(Error::Parse("DROP CAST needs two types".into()));
    };
    Ok(Statement::DropCast {
        source: source.clone(),
        target: target.clone(),
        if_exists: d.missing_ok,
    })
}
