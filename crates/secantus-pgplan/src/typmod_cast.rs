//! The length-coercion functions behind a typmod cast, called by name:
//! `pg_catalog."varchar"(x, 7, true)` is `x::varchar(3)`, and
//! `pg_catalog."numeric"(x, 131078)` is `x::numeric(2, 2)`.
//!
//! PostgreSQL runs one of these for every cast to a type with a modifier;
//! a statement can also call it, with the modifier in its stored form
//! (`atttypmod`). Each is rewritten here into the cast it performs, so the
//! one cast implementation answers both. Measured on PostgreSQL 15.19:
//!
//! * a modifier below the type's smallest (`VARHDRSZ`, 4, for the string
//!   and numeric types; 0 for the bit strings; -1 for the date/time types)
//!   leaves the value as it is;
//! * the string modifiers count the 4-byte header (`varchar(3)` is 7), the
//!   numeric one packs precision and scale (`((p << 16) | s) + 4`), the
//!   interval one a field mask and a precision (`(mask << 16) | p`).
//!
//! The `isExplicit` argument of the string and bit-string forms decides
//! whether an overlong value is truncated (`true`, a cast) or refused (an
//! assignment, `false`); the assignment form, and a modifier that is not a
//! constant, go through `__typmod_coerce` (a column assignment's rules).

use super::*;

/// The interval field masks PostgreSQL's grammar can produce (`INTERVAL_MASK`
/// combinations in `gram.y`), as `(mask, the fields as written)`.
pub(crate) const INTERVAL_RANGES: &[(i32, &str)] = &[
    (0x7FFF, ""),
    (4, "year"),
    (2, "month"),
    (8, "day"),
    (1024, "hour"),
    (2048, "minute"),
    (4096, "second"),
    (6, "year to month"),
    (1032, "day to hour"),
    (3080, "day to minute"),
    (7176, "day to second"),
    (3072, "hour to minute"),
    (7168, "hour to second"),
    (6144, "minute to second"),
];

fn int_arg(n: &pg_query::protobuf::Node) -> Option<i32> {
    match n.node.as_ref()? {
        N::AConst(c) => match c.val.as_ref()? {
            pg_query::protobuf::a_const::Val::Ival(i) => Some(i.ival),
            _ => None,
        },
        N::TypeCast(t) => int_arg(t.arg.as_deref()?),
        _ => None,
    }
}

/// Could `n` be an `int4` modifier: not a literal or a cast of another type?
fn may_be_typmod(n: &pg_query::protobuf::Node) -> bool {
    match n.node.as_ref() {
        Some(N::AConst(c)) => matches!(
            c.val,
            Some(pg_query::protobuf::a_const::Val::Ival(_)) | None
        ),
        Some(N::TypeCast(t)) => t.type_name.as_ref().is_some_and(|tn| {
            tn.names.iter().any(|n| {
                matches!(n.node.as_ref(), Some(N::String(s))
                    if matches!(s.sval.as_str(), "int2" | "int4" | "int8"))
            })
        }),
        _ => true,
    }
}

fn bool_arg(n: &pg_query::protobuf::Node) -> Option<bool> {
    match n.node.as_ref()? {
        N::AConst(c) => match c.val.as_ref()? {
            pg_query::protobuf::a_const::Val::Boolval(b) => Some(b.boolval),
            _ => None,
        },
        _ => None,
    }
}

/// The cast text (`varchar(3)`, `numeric`, `interval day to second(2)`)
/// one of these calls performs, when its arguments are constants.
fn cast_text(f: &pg_query::protobuf::FuncCall) -> Option<String> {
    let parts: Vec<&str> = f
        .funcname
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.as_str()),
            _ => None,
        })
        .collect();
    // Only the qualified form names the function: a bare `numeric(x, 2)`
    // is a type name with modifiers in PostgreSQL's grammar.
    let ["pg_catalog", name] = parts.as_slice() else {
        return None;
    };
    let typmod = int_arg(f.args.get(1)?)?;
    let explicit = match f.args.get(2) {
        Some(b) => Some(bool_arg(b)?),
        None => None,
    };
    let (string, arity) = match *name {
        "varchar" | "bpchar" | "bit" | "varbit" => (true, 3),
        "numeric" | "interval" | "time" | "timetz" | "timestamp" | "timestamptz" => (false, 2),
        _ => return None,
    };
    // `bit(int4, typmod)` / `bit(int8, typmod)` are the integer-to-bit
    // casts, with no `isExplicit` argument.
    let int_to_bit = *name == "bit" && f.args.len() == 2;
    if !int_to_bit && (f.args.len() != arity || (string && explicit != Some(true))) {
        return None;
    }
    Some(match *name {
        "varchar" | "bpchar" if typmod < 4 => (*name).to_string(),
        "varchar" | "bpchar" => format!("{name}({})", typmod - 4),
        "bit" | "varbit" if typmod <= 0 => (*name).to_string(),
        "bit" | "varbit" => format!("{name}({typmod})"),
        "numeric" if typmod < 4 => "numeric".to_string(),
        "numeric" => {
            let t = typmod - 4;
            let precision = (t >> 16) & 0xffff;
            let scale = ((t & 0x7ff) ^ 1024) - 1024;
            format!("numeric({precision}, {scale})")
        }
        _ if typmod < 0 => (*name).to_string(),
        "interval" => {
            let (mask, precision) = (typmod >> 16, typmod & 0xffff);
            let fields = INTERVAL_RANGES
                .iter()
                .find(|(m, _)| *m == mask)
                .map(|(_, f)| *f)?;
            let precision = (precision != 0xffff).then(|| format!("({precision})"));
            match (fields, precision) {
                ("", None) => "interval".to_string(),
                ("", Some(p)) => format!("interval{p}"),
                (f, None) => format!("interval {f}"),
                // Only a range ending in seconds takes a precision.
                (f, Some(p)) if f.ends_with("second") => format!("interval {f}{p}"),
                _ => return None,
            }
        }
        _ if typmod > 6 => return None,
        other => format!("{other}({typmod})"),
    })
}

/// One of these calls the cast rewrite cannot take -- a modifier that is
/// not a constant, or `isExplicit` false (an assignment, which refuses an
/// overlong string or bit string rather than truncating it) -- as
/// `__typmod_coerce(value, 'type', typmod)`, which applies the modifier as
/// a column assignment does (`fit_length`).
fn assignment_call(f: &pg_query::protobuf::FuncCall) -> Option<pg_query::protobuf::Node> {
    let parts: Vec<&str> = f
        .funcname
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.as_str()),
            _ => None,
        })
        .collect();
    let ["pg_catalog", name] = parts.as_slice() else {
        return None;
    };
    let arity = match *name {
        "varchar" | "bpchar" | "varbit" => 3,
        "bit" if f.args.len() == 3 => 3,
        "numeric" | "interval" | "time" | "timetz" | "timestamp" | "timestamptz" => 2,
        _ => return None,
    };
    if f.args.len() != arity {
        return None;
    }
    // `timestamp(date, time)` and `timestamptz(date, time[tz])` are the
    // date-and-time constructors, not a modifier's coercion.
    if arity == 2 && !may_be_typmod(&f.args[1]) {
        return None;
    }
    // A cast (`isExplicit` true) with a modifier that is not constant is
    // not an assignment; only `false` (or no such argument) is taken here.
    if arity == 3 && bool_arg(&f.args[2]) != Some(false) {
        return None;
    }
    let text = |v: &str| pg_query::protobuf::Node {
        node: Some(N::AConst(pg_query::protobuf::AConst {
            isnull: false,
            location: -1,
            val: Some(pg_query::protobuf::a_const::Val::Sval(
                pg_query::protobuf::String { sval: v.into() },
            )),
        })),
    };
    let call = pg_query::protobuf::Node {
        node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
            funcname: vec![pg_query::protobuf::Node {
                node: Some(N::String(pg_query::protobuf::String {
                    sval: "__typmod_coerce".into(),
                })),
            }],
            args: vec![f.args[0].clone(), text(name), f.args[1].clone()],
            funcformat: pg_query::protobuf::CoercionForm::CoerceExplicitCall as i32,
            location: f.location,
            ..Default::default()
        }))),
    };
    // Typed as the function's result, without changing its value -- except
    // `bit`, whose bare name means `bit(1)` and would cut it.
    if *name == "bit" {
        return Some(call);
    }
    Some(pg_query::protobuf::Node {
        node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
            arg: Some(Box::new(call)),
            type_name: Some(type_name_node(name)),
            location: -1,
        }))),
    })
}

/// Is `f` one of these calls?
pub(crate) fn wanted(f: &pg_query::protobuf::FuncCall) -> bool {
    cast_text(f).is_some() || assignment_call(f).is_some()
}

/// The cast a call stands for: `(<value>)::<type>`, parsed from its text.
pub(crate) fn replacement(n: &pg_query::protobuf::Node) -> Option<pg_query::protobuf::Node> {
    let N::FuncCall(f) = n.node.as_ref()? else {
        return None;
    };
    let Some(text) = cast_text(f) else {
        return assignment_call(f);
    };
    let parsed = pg_query::parse(&format!("select null::{text}")).ok()?;
    let target = parsed.protobuf.stmts.first()?.stmt.as_ref()?;
    let N::SelectStmt(s) = target.node.as_ref()? else {
        return None;
    };
    let Some(N::ResTarget(rt)) = s.target_list.first()?.node.as_ref() else {
        return None;
    };
    let Some(N::TypeCast(tc)) = rt.val.as_deref()?.node.as_ref() else {
        return None;
    };
    let mut type_name = tc.type_name.clone()?;
    type_name.location = f.location;
    Some(pg_query::protobuf::Node {
        node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
            arg: Some(Box::new(f.args.first()?.clone())),
            type_name: Some(type_name),
            location: -1,
        }))),
    })
}
