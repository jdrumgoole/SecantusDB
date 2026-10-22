//! mongod's optimisation of a query's `$expr`, as `explain` reports it.
//!
//! mongod optimises the aggregation expression inside `$expr` before it plans
//! (`ExprMatchExpression::optimize`), and then `RewriteExpr` derives match
//! predicates from it that an index can serve. So `{$expr: {$eq: ["$a", 1]}}`
//! is parsed as `{$and: [{$expr: {$eq: ["$a", {$const: 1}]}}, {a:
//! {$_internalExprEq: 1}}]}`, and `{$expr: false}` as `{$alwaysFalse: 1}`.
//! Every rule below was measured against mongod 8.2.11 (2026-10-16); what was
//! not measured is left as written, which is how this server answered before.

use bson::{Bson, Document};

/// What `$expr`'s argument becomes.
pub(crate) enum ExprClauses {
    /// A constant: the whole clause is `$alwaysTrue` (dropped) or
    /// `$alwaysFalse`.
    Constant(bool),
    /// The optimised expression, and the predicates derived from it.
    Rewritten { expr: Bson, derived: Vec<Document> },
}

/// Optimise `$expr`'s argument and derive its index-usable twins.
pub(crate) fn expr_clauses(arg: &Bson) -> ExprClauses {
    match optimize(arg) {
        Opt::Const(v) => ExprClauses::Constant(truthy(&v)),
        Opt::Expr(expr) => {
            let derived = rewrite(&expr);
            ExprClauses::Rewritten { expr, derived }
        }
    }
}

/// An optimised sub-expression: a constant value, or an expression to print.
enum Opt {
    Const(Bson),
    Expr(Bson),
}

impl Opt {
    /// The form mongod prints: a constant as `{$const: v}`.
    fn shown(self) -> Bson {
        match self {
            Opt::Const(v) => one("$const", v),
            Opt::Expr(e) => e,
        }
    }
}

fn one(key: &str, value: Bson) -> Bson {
    let mut d = Document::new();
    d.insert(key, value);
    Bson::Document(d)
}

/// mongod's truthiness of a constant: false, null, undefined and zero are
/// false.
fn truthy(v: &Bson) -> bool {
    match v {
        Bson::Boolean(b) => *b,
        Bson::Null | Bson::Undefined => false,
        Bson::Int32(n) => *n != 0,
        Bson::Int64(n) => *n != 0,
        Bson::Double(d) => *d != 0.0,
        Bson::Decimal128(_) => crate::decimal::from_bson(v)
            .map(|d| !d.is_zero())
            .unwrap_or(true),
        _ => true,
    }
}

/// Operators whose argument is a document of named parameters rather than a
/// list. mongod prints those in a form of its own, which was not measured, so
/// an expression holding one is printed as written.
fn has_named_arguments(op: &str) -> bool {
    matches!(
        op,
        "$cond"
            | "$switch"
            | "$let"
            | "$map"
            | "$filter"
            | "$reduce"
            | "$sortArray"
            | "$zip"
            | "$regexMatch"
            | "$regexFind"
            | "$regexFindAll"
            | "$replaceOne"
            | "$replaceAll"
            | "$trim"
            | "$ltrim"
            | "$rtrim"
            | "$convert"
            | "$getField"
            | "$setField"
            | "$unsetField"
            | "$dateAdd"
            | "$dateSubtract"
            | "$dateDiff"
            | "$dateTrunc"
            | "$dateFromParts"
            | "$dateToParts"
            | "$dateFromString"
            | "$dateToString"
            | "$median"
            | "$percentile"
            | "$firstN"
            | "$lastN"
            | "$maxN"
            | "$minN"
            | "$function"
            | "$accumulator"
            | "$literal"
    )
}

/// Whether `e` reads nothing: no field path, no variable.
fn is_constant(e: &Bson) -> bool {
    match e {
        Bson::String(s) => !s.starts_with('$'),
        Bson::Array(a) => a.iter().all(is_constant),
        Bson::Document(d) => match d.iter().next() {
            Some((k, _)) if d.len() == 1 && k == "$literal" => true,
            Some((k, _)) if d.len() == 1 && k.starts_with('$') => d.values().all(is_constant),
            _ => d.values().all(is_constant),
        },
        _ => true,
    }
}

fn evaluate_constant(e: &Bson) -> Option<Bson> {
    crate::expressions::evaluate(&Document::new(), e, &Document::new()).ok()
}

/// The error mongod raises while it optimizes a query's `$expr`: the first
/// constant sub-expression that fails to evaluate (measured 8.2.11,
/// 2026-10-16: `{$add: ["$a", {$divide: [1, 0]}]}` fails before any
/// document is read, on an empty collection too). Folding visits every
/// constant subtree, so an `$and` that would short-circuit at run time does
/// not hide one.
pub fn constant_expr_fault(arg: &Bson) -> Option<crate::fallback::Fallback> {
    // Constant AND one mongod folds: some operators it never evaluates while
    // optimizing, whatever their arguments (`aggregate::is_constant_expression`).
    if is_constant(arg) && crate::aggregate::is_constant_expression(arg, &[]) {
        return crate::expressions::evaluate(&Document::new(), arg, &Document::new())
            .err()
            // A PARSE error (a missing required argument) is reported bare,
            // and by the command's own checks, not as an optimizer failure.
            .filter(|f| f.as_mongo().is_some() && !f.is_bare());
    }
    match arg {
        Bson::Array(items) => items.iter().find_map(constant_expr_fault),
        Bson::Document(d) => match d.iter().next() {
            Some((op, _)) if d.len() == 1 && has_named_arguments(op) => None,
            _ => d.values().find_map(constant_expr_fault),
        },
        _ => None,
    }
}

/// [`constant_expr_fault`] over every `$expr` in a query filter, as mongod's
/// `(code, message)`.
pub fn constant_expr_error_in_filter(filter: &Document) -> Option<(i32, String)> {
    for (key, value) in filter {
        let found = match (key.as_str(), value) {
            ("$expr", arg) => constant_expr_fault(arg).and_then(|f| {
                f.as_mongo().map(|(c, m)| {
                    (
                        c,
                        format!("Failed to optimize expression :: caused by :: {m}"),
                    )
                })
            }),
            ("$and" | "$or" | "$nor", Bson::Array(subs)) => subs
                .iter()
                .filter_map(Bson::as_document)
                .find_map(constant_expr_error_in_filter),
            _ => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

/// A field path as mongod prints it: `$$CURRENT.a` is `$a`.
fn field_path(s: &str) -> Option<String> {
    if let Some(rest) = s.strip_prefix("$$CURRENT.") {
        return Some(format!("${rest}"));
    }
    (s.starts_with('$') && !s.starts_with("$$")).then(|| s.to_string())
}

fn optimize(e: &Bson) -> Opt {
    if is_constant(e) {
        // A constant mongod cannot evaluate is an error at run time; keep it
        // as written rather than guess its fold.
        return match evaluate_constant(e) {
            Some(v) => Opt::Const(v),
            None => Opt::Expr(e.clone()),
        };
    }
    match e {
        Bson::String(s) => Opt::Expr(Bson::String(field_path(s).unwrap_or_else(|| s.clone()))),
        Bson::Array(items) => Opt::Expr(Bson::Array(
            items.iter().map(|i| optimize(i).shown()).collect(),
        )),
        Bson::Document(d) => match d.iter().next() {
            Some((op, args)) if d.len() == 1 && op.starts_with('$') => {
                optimize_operator(op, args, e)
            }
            _ => Opt::Expr(Bson::Document(
                d.iter()
                    .map(|(k, v)| (k.clone(), optimize(v).shown()))
                    .collect(),
            )),
        },
        other => Opt::Expr(other.clone()),
    }
}

fn optimize_operator(op: &str, args: &Bson, whole: &Bson) -> Opt {
    if has_named_arguments(op) {
        return Opt::Expr(whole.clone());
    }
    let list: Vec<Bson> = match args {
        Bson::Array(a) => a.clone(),
        single => vec![single.clone()],
    };
    let mut parts: Vec<Opt> = list.iter().map(optimize).collect();
    match op {
        // A constant true argument leaves `$and`, a false one `$or`; the
        // opposite constant decides it.
        "$and" | "$or" => {
            let absorbing = op == "$or";
            let mut kept = Vec::new();
            for p in parts {
                match p {
                    Opt::Const(v) if truthy(&v) == absorbing => {
                        return Opt::Const(Bson::Boolean(absorbing));
                    }
                    Opt::Const(_) => {}
                    expr => kept.push(expr.shown()),
                }
            }
            if kept.is_empty() {
                return Opt::Const(Bson::Boolean(!absorbing));
            }
            Opt::Expr(one(op, Bson::Array(kept)))
        }
        // An `$in` of an empty list is false whatever it is asked about.
        "$in" if matches!(parts.get(1), Some(Opt::Const(Bson::Array(a))) if a.is_empty()) => {
            Opt::Const(Bson::Boolean(false))
        }
        _ => Opt::Expr(one(
            op,
            Bson::Array(parts.drain(..).map(Opt::shown).collect()),
        )),
    }
}

/// The match predicates mongod derives from an optimised expression.
fn rewrite(e: &Bson) -> Vec<Document> {
    let Bson::Document(d) = e else {
        return vec![];
    };
    let Some((op, Bson::Array(args))) = d.iter().next().filter(|_| d.len() == 1) else {
        return vec![];
    };
    match op.as_str() {
        "$and" => args.iter().flat_map(rewrite).collect(),
        "$or" => {
            let mut branches = Vec::new();
            for arg in args {
                let mut found = rewrite(arg);
                match found.len() {
                    0 => return vec![],
                    1 => branches.push(Bson::Document(found.remove(0))),
                    _ => branches.push(one(
                        "$and",
                        Bson::Array(found.into_iter().map(Bson::Document).collect()),
                    )),
                }
            }
            // An `$or` of one branch is that branch.
            if branches.len() == 1 {
                return match branches.remove(0) {
                    Bson::Document(d) => match d.get("$and") {
                        Some(Bson::Array(parts)) => parts
                            .iter()
                            .filter_map(|p| p.as_document().cloned())
                            .collect(),
                        _ => vec![d],
                    },
                    _ => vec![],
                };
            }
            match one("$or", Bson::Array(branches)) {
                Bson::Document(d) => vec![d],
                _ => vec![],
            }
        }
        "$eq" | "$lt" | "$lte" | "$gt" | "$gte" => comparison(op, args).into_iter().collect(),
        "$in" => in_list(args).into_iter().collect(),
        _ => vec![],
    }
}

fn constant_of(e: &Bson) -> Option<&Bson> {
    match e {
        Bson::Document(d) if d.len() == 1 => d.get("$const"),
        _ => None,
    }
}

fn path_of(e: &Bson) -> Option<&str> {
    match e {
        Bson::String(s) if s.starts_with('$') && !s.starts_with("$$") => Some(&s[1..]),
        _ => None,
    }
}

/// `{$op: ["$path", constant]}`, either way round, is `{path:
/// {$_internalExprOp: constant}}`; not for an array constant.
fn comparison(op: &str, args: &[Bson]) -> Option<Document> {
    let [left, right] = args else { return None };
    let (path, value, flipped) = match (path_of(left), constant_of(right)) {
        (Some(p), Some(v)) => (p, v, false),
        _ => (path_of(right)?, constant_of(left)?, true),
    };
    if matches!(value, Bson::Array(_)) {
        return None;
    }
    let name = match (op, flipped) {
        ("$eq", _) => "$_internalExprEq",
        ("$lt", false) | ("$gt", true) => "$_internalExprLt",
        ("$lte", false) | ("$gte", true) => "$_internalExprLte",
        ("$gt", false) | ("$lt", true) => "$_internalExprGt",
        _ => "$_internalExprGte",
    };
    let mut pred = Document::new();
    pred.insert(name, value.clone());
    let mut out = Document::new();
    out.insert(path, pred);
    Some(out)
}

/// `{$in: ["$path", [constants]]}` is `{path: {$in: [...]}}` while no member
/// is an array or null.
fn in_list(args: &[Bson]) -> Option<Document> {
    let [left, right] = args else { return None };
    let path = path_of(left)?;
    let Bson::Array(items) = constant_of(right)? else {
        return None;
    };
    if items.is_empty()
        || items
            .iter()
            .any(|i| matches!(i, Bson::Array(_) | Bson::Null | Bson::Undefined))
    {
        return None;
    }
    let mut pred = Document::new();
    pred.insert("$in", Bson::Array(items.clone()));
    let mut out = Document::new();
    out.insert(path, pred);
    Some(out)
}

#[cfg(test)]
mod tests {
    use crate::{canonical_match, residual_key};
    use bson::{bson, doc, Bson};

    fn parsed(filter: Bson) -> Bson {
        Bson::Document(canonical_match(&filter))
    }

    /// mongod 8.2.11's `parsedQuery` for each (probed 2026-10-16, RM-074).
    #[test]
    fn expr_is_optimised_as_mongod_parses_it() {
        assert_eq!(parsed(bson!({"$expr": false})), bson!({"$alwaysFalse": 1}));
        assert_eq!(parsed(bson!({"$expr": 1})), bson!({}));
        assert_eq!(
            parsed(bson!({"$expr": {"$in": ["$a", []]}})),
            bson!({"$alwaysFalse": 1})
        );
        assert_eq!(
            parsed(bson!({"$expr": {"$eq": ["$a", 1]}})),
            bson!({"$and": [{"$expr": {"$eq": ["$a", {"$const": 1}]}}, {"a": {"$_internalExprEq": 1}}]})
        );
        assert_eq!(
            parsed(bson!({"$expr": {"$gt": [1, "$$CURRENT.a"]}})),
            bson!({"$and": [{"$expr": {"$gt": [{"$const": 1}, "$a"]}}, {"a": {"$_internalExprLt": 1}}]})
        );
        assert_eq!(
            parsed(bson!({"$expr": {"$and": [true, {"$eq": ["$a", {"$add": [1, 1]}]}]}})),
            bson!({"$and": [{"$expr": {"$and": [{"$eq": ["$a", {"$const": 2}]}]}}, {"a": {"$_internalExprEq": 2}}]})
        );
        assert_eq!(
            parsed(bson!({"$expr": {"$in": ["$a", [1, 2]]}})),
            bson!({"$and": [{"a": {"$in": [1, 2]}}, {"$expr": {"$in": ["$a", {"$const": [1, 2]}]}}]})
        );
        // No twin: an array constant, a null in `$in`, two field paths.
        assert_eq!(
            parsed(bson!({"$expr": {"$eq": ["$a", [1]]}})),
            bson!({"$expr": {"$eq": ["$a", {"$const": [1]}]}})
        );
        assert_eq!(
            parsed(bson!({"$expr": {"$in": ["$a", [1, null]]}})),
            bson!({"$expr": {"$in": ["$a", {"$const": [1, null]}]}})
        );
        assert_eq!(
            parsed(bson!({"$expr": {"$abs": "$a"}})),
            bson!({"$expr": {"$abs": ["$a"]}})
        );
    }

    #[test]
    fn a_fetch_rechecks_by_path() {
        let mut clauses = vec![
            doc! {"b": {"$eq": 2}},
            doc! {"c": {"$not": {"$eq": 1}}},
            doc! {"a": {"$gt": 1}},
            doc! {"$expr": "$q"},
        ];
        clauses.sort_by_key(residual_key);
        assert_eq!(
            clauses,
            vec![
                doc! {"c": {"$not": {"$eq": 1}}},
                doc! {"$expr": "$q"},
                doc! {"a": {"$gt": 1}},
                doc! {"b": {"$eq": 2}},
            ]
        );
    }
}
