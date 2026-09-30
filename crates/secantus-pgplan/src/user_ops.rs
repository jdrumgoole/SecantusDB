//! `CREATE OPERATOR`: a symbol bound to a function. A use of one is
//! rewritten, as the statement is parsed, into what it stands for -- the
//! function called on the operands, or, when the function is one of the
//! built-ins behind an ordinary operator (`int4pl`, `textcat`, `int4eq`),
//! that operator -- so the planner never sees a symbol it does not know.

use super::*;

/// One operator, as `CREATE OPERATOR` declared it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UserOperator {
    pub name: String,
    /// `None` for a prefix operator.
    pub left: Option<String>,
    pub right: String,
    pub function: String,
    pub result: String,
}

thread_local! {
    static USER_OPERATORS: std::cell::RefCell<Vec<UserOperator>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Install the database's user operators for the statements that follow.
pub fn set_user_operators(ops: Vec<UserOperator>) {
    USER_OPERATORS.with(|o| *o.borrow_mut() = ops);
}

/// The operator names PostgreSQL 15 defines in `pg_catalog`.
const BUILTIN: &[&str] = &[
    "!!", "!~", "!~*", "!~~", "!~~*", "##", "#", "#-", "#>", "#>>", "%", "&&", "&", "&<", "&<|",
    "&>", "*", "*<", "*<=", "*<>", "*=", "*>", "*>=", "+", "-", "->", "->>", "-|-", "/", "<",
    "<->", "<<", "<<=", "<<|", "<=", "<>", "<@", "<^", "=", ">", ">=", ">>", ">>=", ">^", "?#",
    "?&", "?", "?-", "?-|", "?|", "?||", "@", "@-@", "@>", "@?", "@@", "@@@", "^", "^@", "|&>",
    "|", "|/", "|>>", "||", "||/", "~", "~*", "~<=~", "~<~", "~=", "~>=~", "~>~", "~~", "~~*",
    "!=",
];

/// Does PostgreSQL itself have an operator of this name? (`!=` is `<>`.)
pub fn is_builtin(op: &str) -> bool {
    BUILTIN.contains(&op)
}

/// The ordinary operator a built-in operator FUNCTION implements.
fn builtin_symbol(function: &str) -> Option<&'static str> {
    let ends = |s: &str| function.ends_with(s);
    Some(if function == "textcat" {
        "||"
    } else if ends("pl") || function == "numeric_add" {
        "+"
    } else if ends("mi") || function == "numeric_sub" {
        "-"
    } else if ends("mul") || function == "numeric_mul" {
        "*"
    } else if ends("div") || function == "numeric_div" {
        "/"
    } else if ends("eq") {
        "="
    } else if ends("ne") {
        "<>"
    } else if ends("lt") {
        "<"
    } else if ends("le") {
        "<="
    } else if ends("gt") {
        ">"
    } else if ends("ge") {
        ">="
    } else {
        return None;
    })
}

/// The scalar function a built-in unary operator function is.
fn builtin_unary(function: &str) -> Option<&'static str> {
    match function {
        "int2abs" | "int4abs" | "int8abs" | "float4abs" | "float8abs" | "numeric_abs" => {
            Some("abs")
        }
        "dsqrt" | "numeric_sqrt" => Some("sqrt"),
        "dcbrt" => Some("cbrt"),
        _ => None,
    }
}

/// What operator `function` over `(left, right)` returns, or 42883 when it
/// names no function taking those types.
pub(crate) fn resolve_function(function: &str, left: Option<&str>, right: &str) -> Result<String> {
    let args: Vec<String> = left
        .into_iter()
        .chain([right])
        .map(str::to_string)
        .collect();
    let same = |a: &str, b: &str| {
        let oid = |t: &str| pgtypes::oid_of_name(t);
        match (oid(a), oid(b)) {
            (Some(x), Some(y)) => x == y,
            _ => a.eq_ignore_ascii_case(b),
        }
    };
    if let Some(u) = correlated::user_functions().into_iter().find(|u| {
        u.name == function
            && u.arg_types.len() == args.len()
            && u.arg_types.iter().zip(&args).all(|(p, a)| same(p, a))
    }) {
        return Ok(u.return_type);
    }
    if let Some(sym) = builtin_symbol(function).filter(|_| left.is_some()) {
        return Ok(match sym {
            "=" | "<>" | "<" | "<=" | ">" | ">=" => "bool".into(),
            "||" => "text".into(),
            _ => left.unwrap_or(right).to_string(),
        });
    }
    if builtin_unary(function).is_some() && left.is_none() {
        return Ok(right.to_string());
    }
    if scalar::is_scalar(function) {
        return Ok(if scalar::has_static_result_type(function) {
            scalar::static_result_type(function).to_string()
        } else {
            right.to_string()
        });
    }
    Err(Error::UndefinedFunction(format!(
        "function {function}({}) does not exist",
        args.iter()
            .map(|a| display_type(a))
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

fn op_name(names: &[pg_query::protobuf::Node]) -> Option<&str> {
    match names {
        [n] | [_, n] => match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.as_str()),
            _ => None,
        },
        _ => None,
    }
}

/// The user operator an `AExpr` uses, when it uses one.
fn operator_for(e: &pg_query::protobuf::AExpr) -> Option<UserOperator> {
    if pg_query::protobuf::AExprKind::try_from(e.kind) != Ok(pg_query::protobuf::AExprKind::AexprOp)
    {
        return None;
    }
    let name = op_name(&e.name)?;
    let prefix = e.lexpr.is_none();
    let candidates: Vec<UserOperator> = USER_OPERATORS.with(|o| {
        o.borrow()
            .iter()
            .filter(|u| u.name == name && u.left.is_none() == prefix)
            .cloned()
            .collect()
    });
    // Several overloads: prefer the one whose types a literal operand
    // names; otherwise the first.
    fn literal(n: Option<&pg_query::protobuf::Node>) -> Option<&'static str> {
        match n.and_then(|n| n.node.as_ref()) {
            Some(N::AConst(c)) => match c.val.as_ref()? {
                pg_query::protobuf::a_const::Val::Ival(_) => Some("int4"),
                pg_query::protobuf::a_const::Val::Fval(_) => Some("numeric"),
                pg_query::protobuf::a_const::Val::Boolval(_) => Some("bool"),
                _ => None,
            },
            // `-5` is a unary minus over a literal.
            Some(N::AExpr(m)) if m.lexpr.is_none() && op_name(&m.name) == Some("-") => {
                literal(m.rexpr.as_deref())
            }
            _ => None,
        }
    }
    let want = literal(e.rexpr.as_deref()).or_else(|| literal(e.lexpr.as_deref()));
    let exact = candidates
        .iter()
        .find(|u| want.is_some_and(|w| u.right == w || u.left.as_deref() == Some(w)));
    // A symbol PostgreSQL also has (`!!` is tsquery's) is the user's only
    // when the operand's type says so.
    if is_builtin(name) {
        return exact.cloned();
    }
    exact.or_else(|| candidates.first()).cloned()
}

/// Does any `AExpr` in the statement use a user operator?
pub(crate) fn wanted(node: &N) -> bool {
    if USER_OPERATORS.with(|o| o.borrow().is_empty()) {
        return false;
    }
    node.nodes().iter().any(|(n, _, _, _)| match n {
        pg_query::NodeRef::AExpr(e) => operator_for(e).is_some(),
        _ => false,
    })
}

/// The expression a user-operator `AExpr` stands for.
pub(crate) fn replacement(n: &pg_query::protobuf::Node) -> Option<pg_query::protobuf::Node> {
    let Some(N::AExpr(e)) = n.node.as_ref() else {
        return None;
    };
    let op = operator_for(e)?;
    let string = |s: &str| pg_query::protobuf::Node {
        node: Some(N::String(pg_query::protobuf::String { sval: s.into() })),
    };
    let args: Vec<pg_query::protobuf::Node> = e
        .lexpr
        .iter()
        .chain(e.rexpr.iter())
        .map(|b| (**b).clone())
        .collect();
    if op.left.is_some() {
        if let Some(sym) = builtin_symbol(&op.function) {
            let mut out = (**e).clone();
            out.name = vec![string(sym)];
            return Some(pg_query::protobuf::Node {
                node: Some(N::AExpr(Box::new(out))),
            });
        }
    }
    let function = builtin_unary(&op.function).unwrap_or(&op.function);
    // An untyped literal takes the operator's declared type, as parse
    // analysis gives it one (`'x' === 'y'` over integers is 22P02).
    let declared: Vec<&str> = op
        .left
        .iter()
        .map(String::as_str)
        .chain([op.right.as_str()])
        .collect();
    let args: Vec<pg_query::protobuf::Node> = args
        .into_iter()
        .zip(declared)
        .map(|(a, t)| match a.node.as_ref() {
            Some(N::AConst(c))
                if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_))) =>
            {
                pg_query::protobuf::Node {
                    node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
                        arg: Some(Box::new(a)),
                        type_name: Some(pg_query::protobuf::TypeName {
                            names: vec![string(t)],
                            typemod: -1,
                            location: -1,
                            ..Default::default()
                        }),
                        location: -1,
                    }))),
                }
            }
            _ => a,
        })
        .collect();
    Some(pg_query::protobuf::Node {
        node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
            funcname: vec![string(function)],
            args,
            funcformat: pg_query::protobuf::CoercionForm::CoerceExplicitCall as i32,
            location: e.location,
            ..Default::default()
        }))),
    })
}
