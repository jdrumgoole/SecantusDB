//! SBE expressions (`exec/sbe/expressions/expression.h`): the slot
//! programs mongod 8.2.11's stage builders emit for a pushed-down `$group`
//! and the filter in front of it, printed as its `DebugPrinter` prints them and evaluated as its VM
//! evaluates them.
//!
//! A value is `Option<Bson>`: `None` is SBE's `Nothing` (a missing field),
//! which is not `null`.

use bson::{Bson, Document};
use std::cmp::Ordering;

/// One SBE value; `None` is `Nothing`.
pub(crate) type Value = Option<Bson>;

/// A slot id (`sN`).
pub(crate) type Slot = u32;

/// `EPrimBinary`'s operators that the builders here emit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum BinOp {
    FillEmpty,
    Eq,
    Lt,
    Lte,
    Gt,
    Gte,
    Div,
    LogicOr,
    LogicAnd,
}

/// An SBE expression.
#[derive(Clone, Debug)]
pub(crate) enum Expr {
    /// `EVariable` over a slot: `sN`.
    Slot(Slot),
    /// `EVariable` over a frame's local: `lF.I`; `moved` prints `move(...)`.
    Local { frame: u32, moved: bool },
    /// `EConstant`.
    Const(Value),
    /// A 64-bit integer constant, which prints with mongod's `ll` suffix.
    Int64(i64),
    /// `EFunction`: `name(args, ...)`.
    Func(&'static str, Vec<Expr>),
    /// `EPrimBinary`.
    Bin(BinOp, Box<Expr>, Box<Expr>),
    /// `EPrimUnary` logical not: `!(e)`.
    Not(Box<Expr>),
    /// `EIf`.
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `ELambda` with one argument in frame `frame`.
    Lambda { frame: u32, body: Box<Expr> },
    /// `ETypeMatch`: `typeMatch(e, mask)`.
    TypeMatch(Box<Expr>, u32),
    /// `ENumericConvert` to int32: `convert ( e, int32)`.
    ConvertInt32(Box<Expr>),
    /// A slot the plan's environment binds to an `InList` (an `$in`'s
    /// values, in the order the command gave them): prints `sN`.
    InList(Slot, Vec<Bson>),
    /// `makeBsonObj`'s spec: the fields it keeps (`Closed`, the rest
    /// dropped) or drops (`Open`, the rest kept), in the projection's order.
    ObjSpec { fields: Vec<String>, open: bool },
}

/// The mask `typeMatch` and `fillType` take for null and undefined:
/// `getBSONTypeMask(jstNULL) | getBSONTypeMask(Undefined)`.
pub(crate) const NULLISH_MASK: u32 = 1088;

impl Expr {
    /// The `InList`s the expression reads, in the order it prints them.
    pub fn in_lists(&self) -> Vec<(Slot, Vec<Bson>)> {
        let mut out = Vec::new();
        self.collect_lists(&mut out);
        out
    }

    fn collect_lists(&self, out: &mut Vec<(Slot, Vec<Bson>)>) {
        match self {
            Expr::InList(s, values) => out.push((*s, values.clone())),
            Expr::Func(_, args) => args.iter().for_each(|a| a.collect_lists(out)),
            Expr::Bin(_, l, r) => {
                l.collect_lists(out);
                r.collect_lists(out);
            }
            Expr::If(c, t, e) => {
                c.collect_lists(out);
                t.collect_lists(out);
                e.collect_lists(out);
            }
            Expr::Not(e) | Expr::TypeMatch(e, _) | Expr::ConvertInt32(e) => e.collect_lists(out),
            Expr::Lambda { body, .. } => body.collect_lists(out),
            Expr::Slot(_)
            | Expr::Local { .. }
            | Expr::Const(_)
            | Expr::Int64(_)
            | Expr::ObjSpec { .. } => {}
        }
    }

    pub fn null() -> Expr {
        Expr::Const(Some(Bson::Null))
    }

    pub fn nothing() -> Expr {
        Expr::Const(None)
    }

    pub fn func(name: &'static str, args: Vec<Expr>) -> Expr {
        Expr::Func(name, args)
    }

    pub fn bin(op: BinOp, l: Expr, r: Expr) -> Expr {
        Expr::Bin(op, Box::new(l), Box::new(r))
    }

    pub fn fill_empty(l: Expr, r: Expr) -> Expr {
        Expr::bin(BinOp::FillEmpty, l, r)
    }

    pub fn if_(c: Expr, t: Expr, e: Expr) -> Expr {
        Expr::If(Box::new(c), Box::new(t), Box::new(e))
    }
}

/// The frames' locals a lambda body sees while it runs.
pub(crate) struct Env<'a> {
    pub slots: &'a dyn Fn(Slot) -> Value,
    pub locals: Vec<(u32, Value)>,
}

impl Env<'_> {
    fn local(&self, frame: u32) -> Value {
        self.locals
            .iter()
            .rev()
            .find(|(f, _)| *f == frame)
            .and_then(|(_, v)| v.clone())
    }
}

fn is_number(v: &Bson) -> bool {
    matches!(
        v,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
    )
}

fn type_mask(v: &Bson) -> u32 {
    1u32 << (v.element_type() as u8 as u32 & 31)
}

fn truthy(v: &Value) -> Option<bool> {
    match v {
        Some(Bson::Boolean(b)) => Some(*b),
        _ => None,
    }
}

/// Numbers as SBE's arithmetic widens them: int32 + int32 that overflows is
/// int64, an int64 overflow is a double, any double makes a double.
pub(crate) fn add(a: &Bson, b: &Bson) -> Value {
    use Bson::{Decimal128, Double, Int32, Int64};
    Some(match (a, b) {
        (Int32(x), Int32(y)) => match x.checked_add(*y) {
            Some(n) => Int32(n),
            None => Int64(*x as i64 + *y as i64),
        },
        (Int32(_) | Int64(_), Int32(_) | Int64(_)) => {
            let (x, y) = (as_i64(a)?, as_i64(b)?);
            match x.checked_add(y) {
                Some(n) => Int64(n),
                None => Double(x as f64 + y as f64),
            }
        }
        (Decimal128(_), _) | (_, Decimal128(_)) => {
            return secantus_core::expressions::evaluate(
                &Document::new(),
                &Bson::Document(bson::doc! { "$add": [
                    { "$literal": a.clone() }, { "$literal": b.clone() }
                ] }),
                &Document::new(),
            )
            .ok()
        }
        _ if is_number(a) && is_number(b) => Double(as_f64(a)? + as_f64(b)?),
        _ => return None,
    })
}

fn as_i64(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(n) => Some(*n as i64),
        Bson::Int64(n) => Some(*n),
        _ => None,
    }
}

pub(crate) fn as_f64(v: &Bson) -> Option<f64> {
    match v {
        Bson::Int32(n) => Some(*n as f64),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Double(d) => Some(*d),
        Bson::Decimal128(d) => d.to_string().parse().ok(),
        _ => None,
    }
}

/// How SBE's comparison operators compare two values (`genericCompare`):
/// numbers with numbers, strings with strings, and otherwise only values of
/// one type; any other pair is `Nothing`, as is a NaN against a number that
/// is not one (IEEE comparison: every ordering is false).
fn sbe_compare(op: BinOp, x: &Bson, y: &Bson) -> Value {
    let comparable = (is_number(x) && is_number(y))
        || matches!(
            (x, y),
            (
                Bson::String(_) | Bson::Symbol(_),
                Bson::String(_) | Bson::Symbol(_)
            )
        )
        || x.element_type() == y.element_type();
    if !comparable {
        return None;
    }
    let nan = |v: &Bson| is_number(v) && as_f64(v).is_some_and(f64::is_nan);
    let o = match (nan(x), nan(y)) {
        (false, false) => match (x, y) {
            (Bson::String(a) | Bson::Symbol(a), Bson::String(b) | Bson::Symbol(b)) => a.cmp(b),
            _ => secantus_core::order::cmp(x, y),
        },
        (true, true) => Ordering::Equal,
        _ => return Some(Bson::Boolean(false)),
    };
    Some(Bson::Boolean(match op {
        BinOp::Eq => o == Ordering::Equal,
        BinOp::Lt => o == Ordering::Less,
        BinOp::Lte => o != Ordering::Greater,
        BinOp::Gt => o == Ordering::Greater,
        BinOp::Gte => o != Ordering::Less,
        _ => return None,
    }))
}

/// `traverseF(input, lambda, compareArray)`: whether the lambda holds of
/// the input or, when the input is an array, of any of its elements (one
/// level: an element that is itself an array is handed over whole); with
/// `compareArray`, of the array itself as well.
fn traverse_f(input: Value, whole_array: bool, apply: &mut dyn FnMut(Value) -> Value) -> Value {
    let holds = |v: Value| truthy(&v) == Some(true);
    Some(Bson::Boolean(match input {
        Some(Bson::Array(items)) => {
            items.iter().any(|item| holds(apply(Some(item.clone()))))
                || (whole_array && holds(apply(Some(Bson::Array(items)))))
        }
        other => holds(apply(other)),
    }))
}

/// BSON order between two values, `Nothing` lowest.
pub(crate) fn compare(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(x), Some(y)) => secantus_core::order::cmp(x, y),
    }
}

/// `getField`: the field of an object, `Nothing` for anything else.
fn get_field(v: &Value, name: &str) -> Value {
    match v {
        Some(Bson::Document(d)) => d.get(name).cloned(),
        _ => None,
    }
}

/// `traverseP(input, lambda, maxDepth)`: the lambda applied to the input, or
/// to each element of an array (recursing into nested arrays while depth
/// remains), with the results that are `Nothing` left out.
fn traverse_p(input: Value, depth: Option<i64>, apply: &mut dyn FnMut(Value) -> Value) -> Value {
    match input {
        Some(Bson::Array(items)) if depth.is_none_or(|d| d > 0) => {
            let next = depth.map(|d| d - 1);
            let out: Vec<Bson> = items
                .into_iter()
                .filter_map(|item| match item {
                    Bson::Array(_) if next.is_none_or(|d| d > 0) => {
                        traverse_p(Some(item), next, apply)
                    }
                    other => apply(Some(other)),
                })
                .collect();
            Some(Bson::Array(out))
        }
        other => apply(other),
    }
}

/// `newObj` / `newBsonObj`: the named values, `Nothing` ones left out.
fn new_obj(args: &[Value]) -> Value {
    let mut out = Document::new();
    for pair in args.chunks(2) {
        if let [Some(Bson::String(name)), Some(v)] = pair {
            out.insert(name.clone(), v.clone());
        }
    }
    Some(Bson::Document(out))
}

impl Expr {
    /// Run the expression.
    pub fn eval(&self, env: &mut Env<'_>) -> Value {
        match self {
            Expr::Slot(s) => (env.slots)(*s),
            Expr::Local { frame, .. } => env.local(*frame),
            Expr::Const(v) => v.clone(),
            Expr::Int64(n) => Some(Bson::Int64(*n)),
            Expr::Bin(op, l, r) => {
                let a = l.eval(env);
                match op {
                    BinOp::FillEmpty => a.or_else(|| r.eval(env)),
                    BinOp::LogicOr => match truthy(&a) {
                        Some(true) => Some(Bson::Boolean(true)),
                        Some(false) => r.eval(env),
                        None => None,
                    },
                    BinOp::LogicAnd => match truthy(&a) {
                        Some(true) => r.eval(env),
                        Some(false) => Some(Bson::Boolean(false)),
                        None => None,
                    },
                    _ => {
                        let b = r.eval(env);
                        let (Some(x), Some(y)) = (a, b) else {
                            return None;
                        };
                        match op {
                            BinOp::Div => {
                                let (p, q) = (as_f64(&x)?, as_f64(&y)?);
                                (q != 0.0).then(|| Bson::Double(p / q))
                            }
                            _ => sbe_compare(*op, &x, &y),
                        }
                    }
                }
            }
            Expr::Not(e) => truthy(&e.eval(env)).map(|b| Bson::Boolean(!b)),
            Expr::If(c, t, e) => match truthy(&c.eval(env)) {
                Some(true) => t.eval(env),
                _ => e.eval(env),
            },
            // A lambda evaluates only where a traversal applies it.
            Expr::Lambda { .. } => None,
            Expr::TypeMatch(e, mask) => e
                .eval(env)
                .map(|v| Bson::Boolean(type_mask(&v) & mask != 0)),
            Expr::ConvertInt32(e) => match e.eval(env)? {
                Bson::Int32(n) => Some(Bson::Int32(n)),
                Bson::Int64(n) => i32::try_from(n).ok().map(Bson::Int32),
                Bson::Double(d) if d.fract() == 0.0 && d.abs() <= i32::MAX as f64 => {
                    Some(Bson::Int32(d as i32))
                }
                _ => None,
            },
            Expr::InList(_, values) => Some(Bson::Array(values.clone())),
            // Read only by the `makeBsonObj` it is the spec of.
            Expr::ObjSpec { .. } => None,
            Expr::Func(name, args) => self.call(name, args, env),
        }
    }

    fn call(&self, name: &str, args: &[Expr], env: &mut Env<'_>) -> Value {
        match name {
            "getField" => {
                let v = args[0].eval(env);
                match &args[1] {
                    Expr::Const(Some(Bson::String(f))) => get_field(&v, f),
                    _ => None,
                }
            }
            "traverseP" => {
                let input = args[0].eval(env);
                let depth = match args[2].eval(env) {
                    Some(Bson::Int32(n)) => Some(n as i64),
                    Some(Bson::Int64(n)) => Some(n),
                    _ => None,
                };
                let Expr::Lambda { frame, body } = &args[1] else {
                    return None;
                };
                let mut apply = |v: Value| {
                    env.locals.push((*frame, v));
                    let out = body.eval(env);
                    env.locals.pop();
                    out
                };
                traverse_p(input, depth, &mut apply)
            }
            "traverseF" => {
                let input = args[0].eval(env);
                let whole_array = truthy(&args[2].eval(env)) == Some(true);
                let Expr::Lambda { frame, body } = &args[1] else {
                    return None;
                };
                let mut apply = |v: Value| {
                    env.locals.push((*frame, v));
                    let out = body.eval(env);
                    env.locals.pop();
                    out
                };
                traverse_f(input, whole_array, &mut apply)
            }
            "newObj" | "newBsonObj" => {
                let vals: Vec<Value> = args.iter().map(|a| a.eval(env)).collect();
                new_obj(&vals)
            }
            // `isMember(v, inList)`: `v` equals one of the list's values
            // under BSON comparison; Nothing is a member of nothing.
            "isMember" => {
                let v = args[0].eval(env);
                let list = args[1].eval(env);
                match (v, list) {
                    (Some(v), Some(Bson::Array(list))) => {
                        Some(Bson::Boolean(list.iter().any(|m| {
                            compare(&Some(m.clone()), &Some(v.clone())) == Ordering::Equal
                        })))
                    }
                    (None, Some(_)) => Some(Bson::Boolean(false)),
                    _ => None,
                }
            }
            // `makeBsonObj(spec, root)`: the root's fields, kept or dropped
            // by the spec, in the root's order; Nothing over a non-object
            // (`RetNothing`) for a closed spec, the input for an open one
            // (`RetInput`).
            "makeBsonObj" => {
                let Expr::ObjSpec { fields, open } = &args[0] else {
                    return None;
                };
                match args[1].eval(env) {
                    Some(Bson::Document(d)) => Some(Bson::Document(
                        d.into_iter()
                            .filter(|(k, _)| fields.contains(k) != *open)
                            .collect(),
                    )),
                    other if *open => other,
                    _ => None,
                }
            }
            "isNumber" => args[0].eval(env).map(|v| Bson::Boolean(is_number(&v))),
            "fillType" => {
                let v = args[0].eval(env);
                let mask = match &args[1] {
                    Expr::Const(Some(Bson::Int32(m))) => *m as u32,
                    _ => 0,
                };
                match v {
                    Some(b) if type_mask(&b) & mask != 0 => args[2].eval(env),
                    other => other,
                }
            }
            "getElement" => match (args[0].eval(env), args[1].eval(env)) {
                (Some(Bson::Array(a)), Some(Bson::Int32(i))) => a.get(i as usize).cloned(),
                _ => None,
            },
            "doubleDoubleSumFinalize" => match args[0].eval(env) {
                Some(Bson::Array(state)) => state.first().cloned(),
                other => other,
            },
            _ => None,
        }
    }
}
