//! PostgreSQL's `select_common_type`: the one type the arms of a CASE, the
//! arguments of COALESCE / GREATEST / LEAST, or the columns of a UNION /
//! VALUES all resolve to, and the two errors raised when they cannot.
//!
//! The rule, measured on 16.15: the first TYPED input's type is the
//! candidate (a NULL, an untyped string literal or an undeclared parameter
//! says nothing). Each later typed input must be of the same type category,
//! else `42804 <KIND> types <candidate> and <input> cannot be matched` at that
//! input; within a category the candidate moves to the input only when the
//! candidate is not a preferred type and coerces IMPLICITLY to the input while
//! the input does not coerce back (`int4` beside `numeric` is `numeric`, but
//! `varchar` beside `text` stays whichever came first, both being implicit
//! each way). Every typed input must then coerce to the candidate, else
//! `42846 <KIND> could not convert type <input> to <candidate>`, and every
//! untyped literal is read as the candidate at parse time -- so `case when
//! false then 'x' else 1 end` is the integer's 22P02 even though the arm is
//! never taken. A CASE examines its DEFAULT arm first: `transformCaseExpr`
//! conses it onto the front of the result list.

use crate::{
    assignment_value_location, at_literal, cast_value, declared_param_type, display_type,
    is_unknown_literal, position_suffix, range, split_length_suffix, static_type, Error, Result,
};
use bson::Bson;
use pg_query::protobuf::AExprKind;
use pg_query::protobuf::{a_const, node::Node as N, Node};

/// The construct whose inputs are being resolved, as its error messages name it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Construct {
    Case,
    Coalesce,
    Greatest,
    Least,
    Union,
    Intersect,
    Except,
    Values,
}

impl Construct {
    fn name(self) -> &'static str {
        match self {
            Construct::Case => "CASE",
            Construct::Coalesce => "COALESCE",
            Construct::Greatest => "GREATEST",
            Construct::Least => "LEAST",
            Construct::Union => "UNION",
            Construct::Intersect => "INTERSECT",
            Construct::Except => "EXCEPT",
            Construct::Values => "VALUES",
        }
    }

    /// The context `coerce_to_common_type` names in a 42846: the CASE's arms
    /// are converted as `CASE/WHEN`.
    fn coercion_context(self) -> &'static str {
        match self {
            Construct::Case => "CASE/WHEN",
            other => other.name(),
        }
    }
}

/// An input contributes no type of its own: a NULL literal, an untyped
/// string literal, or a parameter the client did not declare.
pub(crate) fn is_untyped(node: &Node) -> bool {
    match node.node.as_ref() {
        Some(N::AConst(c)) if c.isnull => true,
        Some(N::ParamRef(p)) => {
            declared_param_type(usize::try_from(p.number).unwrap_or(0)).is_none()
        }
        _ => is_unknown_literal(node),
    }
}

/// `pg_type.typcategory` for the types this server carries.
pub(crate) fn category(t: &str) -> char {
    if t.ends_with("[]") {
        return 'A';
    }
    match t {
        "int2" | "int4" | "int8" | "numeric" | "float4" | "float8" | "oid" | "money"
        | "regclass" | "regtype" | "regnamespace" | "regproc" | "regprocedure" | "regoper"
        | "regoperator" | "regconfig" | "regdictionary" | "regrole" | "regcollation" => 'N',
        "text" | "varchar" | "bpchar" | "name" => 'S',
        "char" => 'Z',
        "date" | "timestamp" | "timestamptz" | "time" | "timetz" => 'D',
        "interval" => 'T',
        "bool" => 'B',
        "inet" | "cidr" => 'I',
        "point" | "line" | "lseg" | "box" | "path" | "polygon" | "circle" => 'G',
        "bit" | "varbit" => 'V',
        "unknown" => 'X',
        t if range::is_range_type(t) || range::is_multirange_type(t) => 'R',
        // json / jsonb / uuid / bytea / xml / macaddr / tsvector, and the
        // user-defined types this server has no category record for.
        _ => 'U',
    }
}

/// `pg_type.typispreferred`.
pub(crate) fn is_preferred(t: &str) -> bool {
    matches!(
        t,
        "float8" | "text" | "timestamptz" | "interval" | "bool" | "oid" | "inet" | "varbit"
    )
}

const NUMERIC_LADDER: [&str; 6] = ["int2", "int4", "int8", "numeric", "float4", "float8"];

/// Whether `pg_cast` holds an IMPLICIT cast from `from` to `to` (the only
/// context a common-type resolution may use).
pub(crate) fn implicitly_coercible(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    if let (Some(f), Some(t)) = (from.strip_suffix("[]"), to.strip_suffix("[]")) {
        return implicitly_coercible(f, t);
    }
    let rank = |t: &str| NUMERIC_LADDER.iter().position(|x| *x == t);
    if let (Some(rf), Some(rt)) = (rank(from), rank(to)) {
        return rf < rt;
    }
    // `name` reads as any string type implicitly, but only `text` reads as
    // `name` that way -- `pg_cast` has name-to-varchar and name-to-bpchar as
    // ASSIGNMENT casts, which is why `varchar UNION name` is `name`.
    let string = |t: &str| matches!(t, "text" | "varchar" | "bpchar" | "name");
    if string(from) && string(to) {
        return !(from == "name" && to != "text");
    }
    matches!(
        (from, to),
        ("int2" | "int4" | "int8", "oid")
            | ("date", "timestamp" | "timestamptz")
            | ("timestamp", "timestamptz")
            | ("time", "timetz" | "interval")
            | ("cidr", "inet")
            | ("char", "text")
            | ("bit", "varbit")
            | ("varbit", "bit")
    )
}

/// What one input says about its type.
enum Input {
    /// A NULL, an untyped literal or an undeclared parameter: read as the
    /// common type once there is one.
    Untyped,
    /// An expression the planner can only type from a VALUE -- a subquery,
    /// a call it does not know: it neither proposes a type nor is checked.
    Opaque,
    /// A type the node itself names, with any length modifier dropped.
    Typed(String),
}

fn classify(node: &Node) -> Input {
    if is_untyped(node) {
        return Input::Untyped;
    }
    // `static_type` falls back to the value's type for a node it cannot
    // read, and a NULL value is text there; a node whose answer moves with
    // the value has no static type.
    let t = static_type(node, &Bson::Null);
    if t != static_type(node, &Bson::Int32(0)) {
        return Input::Opaque;
    }
    let (base, _) = split_length_suffix(&t);
    Input::Typed(base.to_string())
}

/// A typed NULL at `location`: stands in for an input whose type is known
/// but whose expression the resolver cannot read -- a table column, a
/// nested set operation's output, a VALUES column already resolved.
pub(crate) fn typed_placeholder(pg_type: &str, location: i32) -> Node {
    Node {
        node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
            arg: Some(Box::new(Node {
                node: Some(N::AConst(pg_query::protobuf::AConst {
                    val: None,
                    isnull: true,
                    location,
                })),
            })),
            type_name: Some(pg_query::protobuf::TypeName {
                names: vec![Node {
                    node: Some(N::String(pg_query::protobuf::String {
                        sval: pg_type.to_string(),
                    })),
                }],
                type_oid: 0,
                setof: false,
                pct_type: false,
                typmods: Vec::new(),
                typemod: -1,
                array_bounds: Vec::new(),
                location: -1,
            }),
            location: -1,
        }))),
    }
}

/// A cast over a string literal -- `'1'::uuid` -- is read at parse time,
/// before the construct around it resolves anything: its 22P02 comes first,
/// positioned at the literal.
fn validate_cast_literal(node: &Node) -> Result<()> {
    if let Some(N::TypeCast(tc)) = node.node.as_ref() {
        if let (Some(arg), Some(tn)) = (tc.arg.as_deref(), tc.type_name.as_ref()) {
            if let Some(N::AConst(c)) = arg.node.as_ref() {
                if let Some(a_const::Val::Sval(s)) = c.val.as_ref() {
                    let target = crate::type_name_of(tn);
                    let (base, _) = split_length_suffix(&target);
                    cast_value(Bson::String(s.sval.clone()), base)
                        .map_err(|e| at_literal(e, Some(arg)))?;
                }
            }
        }
    }
    Ok(())
}

/// The common type of `inputs`, examined in the order given (a CASE's
/// default arm first), or `None` when no input is typed -- PostgreSQL then
/// takes `text`, and the callers' value-derived fallback already does.
/// Validates every untyped literal against the common type, positioned at
/// the literal, and every typed input's implicit coercion to it.
pub(crate) fn resolve(kind: Construct, inputs: &[&Node]) -> Result<Option<String>> {
    let mut candidate: Option<String> = None;
    let mut preferred = false;
    for node in inputs {
        validate_cast_literal(node)?;
        let t = match classify(node) {
            Input::Typed(t) => t,
            Input::Untyped | Input::Opaque => continue,
        };
        match &candidate {
            None => {
                preferred = is_preferred(&t);
                candidate = Some(t);
            }
            Some(p) if *p == t => {}
            Some(p) => {
                if category(p) != category(&t) {
                    return Err(Error::DatatypeMismatch(format!(
                        "{} types {} and {} cannot be matched{}",
                        kind.name(),
                        display_type(p),
                        display_type(&t),
                        position_suffix(assignment_value_location(node))
                    )));
                }
                if !preferred && implicitly_coercible(p, &t) && !implicitly_coercible(&t, p) {
                    preferred = is_preferred(&t);
                    candidate = Some(t);
                }
            }
        }
    }
    let Some(common) = candidate else {
        return Ok(None);
    };
    for node in inputs {
        let t = match classify(node) {
            Input::Typed(t) => t,
            Input::Untyped => {
                validate_literal(node, &common)?;
                continue;
            }
            Input::Opaque => continue,
        };
        if t != common && !implicitly_coercible(&t, &common) {
            return Err(Error::CannotCoerce(format!(
                "{} could not convert type {} to {}{}",
                kind.coercion_context(),
                display_type(&t),
                display_type(&common),
                position_suffix(assignment_value_location(node))
            )));
        }
    }
    Ok(Some(common))
}

/// An untyped string literal is read as `target` at parse time: the read's
/// error (22P02 / 22007 / 22008 / 22003) is positioned at the literal.
pub(crate) fn validate_literal(node: &Node, target: &str) -> Result<()> {
    if let Some(N::AConst(c)) = node.node.as_ref() {
        if let Some(a_const::Val::Sval(s)) = c.val.as_ref() {
            if target != "text" {
                cast_value(Bson::String(s.sval.clone()), target)
                    .map_err(|e| at_literal(e, Some(node)))?;
            }
        }
    }
    Ok(())
}

/// The value one input contributes once the common type is settled: an
/// untyped literal read as the common type, a typed input widened along an
/// implicit cast the value carrier must reflect (`int4` to `numeric`,
/// `date` to `timestamptz`). String and array inputs already share their
/// carrier, and a NULL stays NULL.
pub(crate) fn coerce_input(node: &Node, value: Bson, common: &str) -> Result<Bson> {
    if value == Bson::Null {
        return Ok(value);
    }
    if is_untyped(node) {
        return match node.node.as_ref() {
            Some(N::AConst(c)) if matches!(c.val, Some(a_const::Val::Sval(_))) => {
                cast_value(value, common).map_err(|e| at_literal(e, Some(node)))
            }
            _ => Ok(value),
        };
    }
    match classify(node) {
        Input::Typed(t) if t != common && matches!(category(common), 'N' | 'D') => {
            cast_value(value, common)
        }
        _ => Ok(value),
    }
}

/// The inputs of a CASE in the order PostgreSQL resolves them: the default
/// arm first, then each WHEN's result.
pub(crate) fn case_results(c: &pg_query::protobuf::CaseExpr) -> Vec<&Node> {
    c.defresult
        .as_deref()
        .into_iter()
        .chain(c.args.iter().filter_map(|w| match w.node.as_ref() {
            Some(N::CaseWhen(cw)) => cw.result.as_deref(),
            _ => None,
        }))
        .collect()
}

/// The construct and inputs of a node that resolves a common type, if it is
/// one.
pub(crate) fn inputs_of(node: &Node) -> Option<(Construct, Vec<&Node>)> {
    match node.node.as_ref()? {
        N::CaseExpr(c) => Some((Construct::Case, case_results(c))),
        N::CoalesceExpr(c) => Some((Construct::Coalesce, c.args.iter().collect())),
        N::MinMaxExpr(m) => {
            let kind = if m.op == pg_query::protobuf::MinMaxOp::IsGreatest as i32 {
                Construct::Greatest
            } else {
                Construct::Least
            };
            Some((kind, m.args.iter().collect()))
        }
        _ => None,
    }
}

/// The first common-type error anywhere under `node`, in evaluation order
/// (the outer construct's arguments are resolved before it is).
pub(crate) fn check_tree(node: &Node) -> Result<()> {
    let mut probe = node.clone();
    let mut found: Option<Error> = None;
    crate::walk::walk(&mut probe, &mut |n| {
        if found.is_none() {
            if let Some((kind, inputs)) = inputs_of(n) {
                if let Err(e) = resolve(kind, &inputs) {
                    found = Some(e);
                }
            } else if let Some(N::AExpr(e)) = n.node.as_ref() {
                if AExprKind::try_from(e.kind) == Ok(AExprKind::AexprNullif) {
                    if let Err(e) = nullif_operands(e, Bson::Null, Bson::Null) {
                        found = Some(e);
                    }
                }
            } else if let Some(N::FuncCall(f)) = n.node.as_ref() {
                // The arguments are transformed before the function is
                // looked up: their fault comes first.
                if let Err(e) = f
                    .args
                    .iter()
                    .try_for_each(check_tree)
                    .and_then(|()| crate::signatures::check_call(f))
                {
                    found = Some(e);
                }
            }
        }
        false
    });
    found.map_or(Ok(()), Err)
}

/// The operand types NULLIF compares under: PostgreSQL resolves `nullif(a,
/// b)` as the operator `a = b`, so an untyped literal is read as the other
/// operand's type (22P02 at the literal) and two typed operands with no `=`
/// between them are `42883 operator does not exist: a = b` at the NULLIF.
/// Returns each operand's value with the literal side coerced.
pub(crate) fn nullif_operands(
    e: &pg_query::protobuf::AExpr,
    lhs: Bson,
    rhs: Bson,
) -> Result<(Bson, Bson)> {
    let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) else {
        return Ok((lhs, rhs));
    };
    validate_cast_literal(l)?;
    validate_cast_literal(r)?;
    match (classify(l), classify(r)) {
        (Input::Typed(lt), Input::Untyped) => {
            validate_literal(r, &lt)?;
            Ok((lhs, coerce_input(r, rhs, &lt)?))
        }
        (Input::Untyped, Input::Typed(rt)) => {
            validate_literal(l, &rt)?;
            Ok((coerce_input(l, lhs, &rt)?, rhs))
        }
        (Input::Typed(lt), Input::Typed(rt)) if lt != rt => {
            if category(&lt) != category(&rt)
                && !implicitly_coercible(&lt, &rt)
                && !implicitly_coercible(&rt, &lt)
            {
                return Err(Error::UndefinedFunction(format!(
                    "operator does not exist: {} = {}{}",
                    display_type(&lt),
                    display_type(&rt),
                    position_suffix(e.location)
                )));
            }
            // The operator compares under the wider operand's type, and
            // NULLIF answers under it too: `nullif(1, 1.0)` is numeric.
            if implicitly_coercible(&lt, &rt) && !implicitly_coercible(&rt, &lt) {
                return Ok((coerce_input(l, lhs, &rt)?, rhs));
            }
            Ok((lhs, coerce_input(r, rhs, &lt)?))
        }
        _ => Ok((lhs, rhs)),
    }
}

/// The type NULLIF answers: its left operand's, or -- when that is an
/// untyped literal read as the right operand's type -- the right's.
pub(crate) fn nullif_type(e: &pg_query::protobuf::AExpr) -> Option<String> {
    let (l, r) = (e.lexpr.as_deref()?, e.rexpr.as_deref()?);
    match (classify(l), classify(r)) {
        (Input::Untyped, Input::Typed(rt)) => Some(rt),
        (Input::Typed(lt), Input::Typed(rt))
            if lt != rt && implicitly_coercible(&lt, &rt) && !implicitly_coercible(&rt, &lt) =>
        {
            Some(rt)
        }
        (Input::Typed(lt), _) => Some(lt),
        _ => None,
    }
}
