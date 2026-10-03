//! `pg_get_viewdef` as PostgreSQL's `ruleutils.c` prints a view: every
//! column qualified by its relation, each literal with the type the parser
//! resolved for it, each implicit coercion written out as a cast, every
//! operator parenthesised, and the query laid out by `appendContextKeyword`'s
//! indentation rules (`get_basic_select_query`, `get_target_list`,
//! `get_from_clause`, ...), transcribed from PostgreSQL 15.
//!
//! PostgreSQL prints the ANALYSED query, so the types and coercions are
//! resolved here by a small analyser over the parse tree: the operator and
//! function families a view commonly uses. A shape it does not know answers
//! `None`, and the caller keeps the definition as written -- never a wrong
//! reconstruction.

use pg_query::protobuf::a_const::Val;
use pg_query::protobuf::node::Node as N;
use pg_query::protobuf::{
    AExprKind, BoolExprType, BoolTestType, JoinType, NullTestType, SetOperation, SortByDir,
    SortByNulls, SubLinkType,
};

use crate::TableDef;

type Node = pg_query::protobuf::Node;

const PRETTYINDENT_STD: i32 = 8;
const PRETTYINDENT_JOIN: i32 = 4;
const PRETTYINDENT_VAR: i32 = 4;

/// One relation in a query's FROM, as ruleutils names it.
#[derive(Clone, Debug)]
struct Rte {
    /// The name ruleutils PRINTS: the alias or relation name, made unique
    /// against the enclosing queries' and this level's earlier names
    /// (`t_1`), as `set_rtable_names` does.
    refname: String,
    /// The name the query WROTE, which its column references use.
    name: String,
    columns: Vec<(String, String)>,
    /// A rule's NEW / OLD: named only when qualified.
    qualified_only: bool,
}

/// Where a query's relations and views come from.
pub struct Catalog<'a> {
    pub lookup: &'a dyn Fn(&str) -> Option<TableDef>,
    /// A view's definition SQL, by name.
    pub view_sql: &'a dyn Fn(&str) -> Option<String>,
}

struct Printer<'a> {
    cat: &'a Catalog<'a>,
    buf: String,
    indent: i32,
    /// The enclosing queries' relations, innermost last (a correlated
    /// reference resolves outward).
    scopes: Vec<Vec<Rte>>,
    /// CTEs visible by name: their output columns.
    ctes: Vec<(String, Vec<(String, String)>)>,
    depth: usize,
    /// ruleutils' `colNamesVisible`: false inside a sublink, where an
    /// unnamed column shows no `AS "?column?"`.
    col_names_visible: bool,
    /// PRETTYFLAG_PAREN: parentheses only where precedence needs them.
    pretty: bool,
    /// `pg_get_expr` over one relation: columns print bare.
    unqualified: bool,
    /// Leave each target-list / FROM-list line-wrap decision to
    /// [`render_wrapped`] (the int `wrapColumn` form of `pg_get_viewdef`)
    /// rather than wrapping every item, as the default column (0) does.
    markers: bool,
}

/// What a sub-expression is printed inside, for `isSimpleNode`.
#[derive(Clone, Copy, PartialEq)]
enum Parent<'o> {
    /// Printed with `get_rule_expr`: never parenthesised for its parent.
    Direct,
    /// An operator's operand: the operator, and whether it is the left one.
    Op(&'o str, bool),
    Bool(BoolExprType),
    Cast,
    /// Any other `get_rule_expr_paren` parent.
    Other,
}

fn arith_priority(op: &str) -> u8 {
    match op.chars().next() {
        Some('+' | '-') if op.len() == 1 => 1,
        Some('*' | '/' | '%') if op.len() == 1 => 2,
        _ => 0,
    }
}

/// `format_type` of a type with no modifier: a bare `char` is `bpchar`.
fn display_type(t: &str) -> String {
    match t {
        "bpchar" => "bpchar".into(),
        "bpchar[]" => "bpchar[]".into(),
        other => crate::display_type(other),
    }
}

fn q(ident: &str) -> String {
    crate::scalar::quote_identifier(ident)
}

fn sval(n: &Node) -> Option<&str> {
    match n.node.as_ref()? {
        N::String(s) => Some(&s.sval),
        _ => None,
    }
}

fn names(nodes: &[Node]) -> Vec<String> {
    nodes
        .iter()
        .filter_map(|n| sval(n).map(str::to_string))
        .collect()
}

/// The type families the analyser distinguishes.
fn is_int(t: &str) -> bool {
    matches!(t, "int2" | "int4" | "int8")
}

fn is_numberish(t: &str) -> bool {
    matches!(
        t,
        "int2" | "int4" | "int8" | "numeric" | "float4" | "float8"
    )
}

fn is_stringish(t: &str) -> bool {
    matches!(t, "text" | "varchar" | "bpchar" | "name" | "unknown")
}

/// The numeric type two numeric operands meet at (int48 operators exist, so
/// integers keep their own widths).
fn numeric_meet(a: &str, b: &str) -> Option<(&'static str, bool)> {
    let rank = |t: &str| match t {
        "int2" | "int4" | "int8" => 1,
        "numeric" => 2,
        "float4" | "float8" => 3,
        _ => 0,
    };
    let (ra, rb) = (rank(a), rank(b));
    if ra == 0 || rb == 0 {
        return None;
    }
    Some(match ra.max(rb) {
        1 => ("", false),
        2 => ("numeric", true),
        _ => ("float8", true),
    })
}

/// A function's known signature: `(argument type every argument coerces to,
/// result)`; `None` result means the first argument's type.
fn function_sig(name: &str) -> Option<(Option<&'static str>, Option<&'static str>)> {
    Some(match name {
        "upper" | "lower" | "initcap" | "btrim" | "ltrim" | "rtrim" | "md5" | "reverse"
        | "replace" | "translate" | "repeat" | "lpad" | "rpad" | "split_part" | "left"
        | "right" | "substr" | "quote_ident" | "quote_literal" => (Some("text"), Some("text")),
        "length" | "char_length" | "character_length" | "octet_length" | "strpos" | "ascii" => {
            (Some("text"), Some("int4"))
        }
        "concat" | "concat_ws" | "format" => (None, Some("text")),
        "now" | "transaction_timestamp" | "statement_timestamp" | "clock_timestamp" => {
            (None, Some("timestamptz"))
        }
        "count" | "row_number" | "rank" | "dense_rank" | "ntile" => (None, Some("int8")),
        "percent_rank" | "cume_dist" => (None, Some("float8")),
        "abs" | "max" | "min" | "lag" | "lead" | "first_value" | "last_value" | "nth_value" => {
            (None, None)
        }
        "random" => (None, Some("float8")),
        "pg_typeof" => (None, Some("regtype")),
        "array_agg" | "string_agg" | "json_agg" | "jsonb_agg" | "sum" | "avg" | "bool_and"
        | "bool_or" | "every" => (None, None),
        _ => return None,
    })
}

impl<'a> Printer<'a> {
    fn new(cat: &'a Catalog<'a>, indent: i32) -> Self {
        Self {
            cat,
            buf: String::new(),
            indent,
            scopes: Vec::new(),
            ctes: Vec::new(),
            depth: 0,
            col_names_visible: true,
            pretty: false,
            unqualified: false,
            markers: false,
        }
    }

    /// A list item whose line wrap [`render_wrapped`] decides: `kind` is
    /// `T` / `t` for the first / a later target-list entry, `f` for a later
    /// FROM item; the newline-and-indent a wrap would add rides along.
    fn mark(&mut self, kind: char, item: &str) {
        let saved = std::mem::take(&mut self.buf);
        self.keyword("", -PRETTYINDENT_STD, PRETTYINDENT_STD, PRETTYINDENT_VAR);
        let nl = std::mem::replace(&mut self.buf, saved);
        self.buf.push(MARK_OPEN);
        self.buf.push(kind);
        self.buf.push_str(&nl);
        self.buf.push(MARK_ITEM);
        self.buf.push_str(item);
        self.buf.push(MARK_CLOSE);
    }

    fn remove_trailing_spaces(&mut self) {
        while self.buf.ends_with(' ') {
            self.buf.pop();
        }
    }

    /// ruleutils' `appendContextKeyword` under PRETTYFLAG_INDENT.
    fn keyword(&mut self, s: &str, before: i32, after: i32, plus: i32) {
        self.indent += before;
        self.remove_trailing_spaces();
        self.buf.push('\n');
        let amount = self.indent.max(0) + plus;
        self.buf.push_str(&" ".repeat(amount as usize));
        self.buf.push_str(s);
        self.indent += after;
        if self.indent < 0 {
            self.indent = 0;
        }
    }

    // ------------------------------------------------------------------
    // Name resolution and types
    // ------------------------------------------------------------------

    /// A relation's columns: a table, a view (its own output), a CTE.
    fn relation_columns(&mut self, name: &str) -> Option<Vec<(String, String)>> {
        if let Some((_, cols)) = self.ctes.iter().rev().find(|(n, _)| n == name) {
            return Some(cols.clone());
        }
        if let Some(def) = (self.cat.lookup)(name) {
            return Some(
                def.columns
                    .iter()
                    .map(|c| (c.name.clone(), base_type(&c.pg_type)))
                    .collect(),
            );
        }
        let sql = (self.cat.view_sql)(name)?;
        if self.depth > 16 {
            return None;
        }
        let node = parse_select(&sql)?;
        let mut inner = Printer::new(self.cat, 0);
        inner.depth = self.depth + 1;
        inner.pretty = self.pretty;
        inner.markers = self.markers;
        inner.query_columns(&node)
    }

    /// Resolve a column reference: `(refname, column, type)`.
    fn resolve(&self, fields: &[String]) -> Option<(String, String, String)> {
        let find_col = |r: &Rte, c: &str| {
            r.columns
                .iter()
                .find(|(n, _)| n == c)
                .map(|(_, t)| t.clone())
        };
        for scope in self.scopes.iter().rev() {
            match fields {
                [col] => {
                    let hits: Vec<&Rte> = scope
                        .iter()
                        .filter(|r| !r.qualified_only && r.columns.iter().any(|(n, _)| n == col))
                        .collect();
                    match hits.as_slice() {
                        [r] => return Some((r.refname.clone(), col.clone(), find_col(r, col)?)),
                        [] => continue,
                        _ => return None,
                    }
                }
                [.., rel, col] => {
                    if let Some(r) = scope.iter().find(|r| r.name == *rel) {
                        return Some((r.refname.clone(), col.clone(), find_col(r, col)?));
                    }
                }
                _ => return None,
            }
        }
        None
    }

    /// The analysed type of an expression; `unknown` for an untyped string
    /// literal or NULL.
    fn typ(&mut self, n: &Node) -> Option<String> {
        Some(match n.node.as_ref()? {
            N::ColumnRef(c) => self.resolve(&names(&c.fields))?.2,
            N::AConst(c) => match c.val.as_ref() {
                Some(Val::Ival(_)) => "int4".into(),
                Some(Val::Fval(f)) => {
                    // An integer too wide for int4 is int8, then numeric.
                    if f.fval.contains(['.', 'e', 'E']) {
                        "numeric".into()
                    } else if f.fval.parse::<i64>().is_ok() {
                        "int8".into()
                    } else {
                        "numeric".into()
                    }
                }
                Some(Val::Boolval(_)) => "bool".into(),
                Some(Val::Sval(_)) | None => "unknown".into(),
                Some(Val::Bsval(_)) => "bit".into(),
            },
            N::TypeCast(tc) => base_type(&crate::type_name_of(tc.type_name.as_ref()?)),
            N::AExpr(e) => {
                let kind = AExprKind::try_from(e.kind).ok()?;
                match kind {
                    AExprKind::AexprOp => {
                        let op = crate::operator_name(e).ok()?.to_string();
                        if matches!(
                            op.as_str(),
                            "=" | "<>"
                                | "!="
                                | "<"
                                | ">"
                                | "<="
                                | ">="
                                | "~~"
                                | "!~~"
                                | "~~*"
                                | "!~~*"
                                | "~"
                                | "~*"
                                | "!~"
                                | "!~*"
                        ) {
                            return Some("bool".into());
                        }
                        let r = self.typ(e.rexpr.as_deref()?)?;
                        let Some(l) = e.lexpr.as_deref() else {
                            return Some(r);
                        };
                        let l = self.typ(l)?;
                        Self::arith_result(&op, &l, &r)?.2
                    }
                    AExprKind::AexprLike
                    | AExprKind::AexprIlike
                    | AExprKind::AexprIn
                    | AExprKind::AexprBetween
                    | AExprKind::AexprNotBetween
                    | AExprKind::AexprDistinct
                    | AExprKind::AexprNotDistinct
                    | AExprKind::AexprOpAny
                    | AExprKind::AexprOpAll => "bool".into(),
                    AExprKind::AexprNullif => self.typ(e.lexpr.as_deref()?)?,
                    _ => return None,
                }
            }
            N::BoolExpr(_) | N::NullTest(_) | N::BooleanTest(_) => "bool".into(),
            N::FuncCall(f) => {
                let name = crate::func_name(f)?;
                let Some((_, result)) = function_sig(&name) else {
                    let types = f
                        .args
                        .iter()
                        .map(|a| self.typ(a))
                        .collect::<Option<Vec<String>>>()?;
                    return crate::funcsig::selected_result_type(&name, &types)
                        .or_else(|| crate::funcsig::resolved_result_type(&name, &types));
                };
                match result {
                    Some(r) => r.to_string(),
                    None => match name.as_str() {
                        "sum" => match self.typ(f.args.first()?)?.as_str() {
                            "int2" | "int4" => "int8".into(),
                            "int8" | "numeric" => "numeric".into(),
                            other => other.to_string(),
                        },
                        "avg" => match self.typ(f.args.first()?)?.as_str() {
                            "float4" | "float8" => "float8".into(),
                            _ => "numeric".into(),
                        },
                        "array_agg" => format!("{}[]", self.typ(f.args.first()?)?),
                        "string_agg" => "text".into(),
                        "json_agg" => "json".into(),
                        "jsonb_agg" => "jsonb".into(),
                        "bool_and" | "bool_or" | "every" => "bool".into(),
                        _ => {
                            let t = self.typ(f.args.first()?)?;
                            if t == "unknown" {
                                "text".into()
                            } else {
                                t
                            }
                        }
                    },
                }
            }
            N::CoalesceExpr(c) => self.common_type(&c.args)?,
            N::MinMaxExpr(m) => self.common_type(&m.args)?,
            N::CaseExpr(c) => {
                let mut results: Vec<Node> = c
                    .args
                    .iter()
                    .filter_map(|w| match w.node.as_ref() {
                        Some(N::CaseWhen(cw)) => cw.result.as_deref().cloned(),
                        _ => None,
                    })
                    .collect();
                if let Some(d) = c.defresult.as_deref() {
                    results.push(d.clone());
                }
                self.common_type(&results)?
            }
            N::SubLink(s) => match SubLinkType::try_from(s.sub_link_type).ok()? {
                SubLinkType::ExistsSublink | SubLinkType::AnySublink | SubLinkType::AllSublink => {
                    "bool".into()
                }
                SubLinkType::ExprSublink => {
                    let cols = self.subquery_columns(s.subselect.as_deref()?)?;
                    cols.first()?.1.clone()
                }
                SubLinkType::ArraySublink => {
                    let cols = self.subquery_columns(s.subselect.as_deref()?)?;
                    format!("{}[]", cols.first()?.1)
                }
                _ => return None,
            },
            N::AArrayExpr(a) => {
                let t = self.common_type(&a.elements)?;
                format!("{t}[]")
            }
            N::SqlvalueFunction(v) => {
                use pg_query::protobuf::SqlValueFunctionOp as Op;
                match Op::try_from(v.op).ok()? {
                    Op::SvfopCurrentDate => "date".into(),
                    Op::SvfopCurrentTimestamp | Op::SvfopCurrentTimestampN => "timestamptz".into(),
                    Op::SvfopLocaltimestamp | Op::SvfopLocaltimestampN => "timestamp".into(),
                    Op::SvfopCurrentTime | Op::SvfopCurrentTimeN => "timetz".into(),
                    Op::SvfopLocaltime | Op::SvfopLocaltimeN => "time".into(),
                    _ => "name".into(),
                }
            }
            _ => return None,
        })
    }

    /// The type several expressions resolve to (`select_common_type`), with
    /// an untyped literal taking the others' type, and all-unknown as text.
    fn common_type(&mut self, nodes: &[Node]) -> Option<String> {
        let mut out: Option<String> = None;
        for n in nodes {
            let t = self.typ(n)?;
            if t == "unknown" {
                continue;
            }
            out = Some(match out {
                None => t,
                Some(o) if o == t => o,
                Some(o) => {
                    if let Some((meet, _)) = numeric_meet(&o, &t) {
                        if meet.is_empty() {
                            // Integers meet at the widest.
                            let w = |x: &str| match x {
                                "int2" => 1,
                                "int4" => 2,
                                _ => 3,
                            };
                            if w(&o) >= w(&t) {
                                o
                            } else {
                                t
                            }
                        } else {
                            meet.to_string()
                        }
                    } else if is_stringish(&o) && is_stringish(&t) {
                        "text".into()
                    } else {
                        return None;
                    }
                }
            });
        }
        Some(out.unwrap_or_else(|| "text".into()))
    }

    /// An arithmetic or string operator: `(left coerced to, right coerced
    /// to, result)`, empty meaning "as is".
    fn arith_result(op: &str, l: &str, r: &str) -> Option<(String, String, String)> {
        if op == "||" {
            let lt = if l == "text" { "" } else { "text" };
            let rt = if r == "text" { "" } else { "text" };
            return Some((lt.into(), rt.into(), "text".into()));
        }
        if matches!(op, "+" | "-" | "*" | "/" | "%") {
            if let Some((meet, cast)) = numeric_meet(l, r) {
                if !cast {
                    let w = |x: &str| match x {
                        "int2" => 1,
                        "int4" => 2,
                        _ => 3,
                    };
                    let res = if w(l) >= w(r) { l } else { r };
                    return Some((String::new(), String::new(), res.to_string()));
                }
                let lt = if l == meet { "" } else { meet };
                let rt = if r == meet { "" } else { meet };
                return Some((lt.into(), rt.into(), meet.into()));
            }
            // date +/- integer is a date; date - date an integer.
            return match (l, r, op) {
                ("date", "int4", "+" | "-") | ("int4", "date", "+") => {
                    Some((String::new(), String::new(), "date".into()))
                }
                ("date", "date", "-") => Some((String::new(), String::new(), "int4".into())),
                ("timestamptz" | "timestamp", "interval", "+" | "-") => {
                    Some((String::new(), String::new(), l.into()))
                }
                (_, "unknown", _) if is_numberish(l) => Some((String::new(), l.into(), l.into())),
                _ => None,
            };
        }
        None
    }

    /// The coercions a comparison puts on its operands.
    fn compare_coercion(l: &str, r: &str) -> Option<(String, String)> {
        if l == r {
            if l == "varchar" {
                return Some(("text".into(), "text".into()));
            }
            if l == "unknown" {
                return Some(("text".into(), "text".into()));
            }
            return Some((String::new(), String::new()));
        }
        if l == "unknown" {
            let t = match r {
                "varchar" | "name" => "text",
                other => other,
            };
            let rt = if t == r { "" } else { t };
            return Some((t.into(), rt.into()));
        }
        if r == "unknown" {
            let t = match l {
                "varchar" | "name" => "text",
                other => other,
            };
            let lt = if t == l { "" } else { t };
            return Some((lt.into(), t.into()));
        }
        if let Some((meet, cast)) = numeric_meet(l, r) {
            if !cast || (is_int(l) && is_int(r)) {
                return Some((String::new(), String::new()));
            }
            let lt = if l == meet { "" } else { meet };
            let rt = if r == meet { "" } else { meet };
            return Some((lt.into(), rt.into()));
        }
        if is_stringish(l) && is_stringish(r) {
            // `text = name` and `name = text` are operators of their own
            // (PostgreSQL 12+): a name meeting text, or varchar-as-text,
            // stays a name.
            let keep = |t: &str| t == "text" || t == "name";
            let text_side = |t: &str| matches!(t, "text" | "varchar");
            let (lt, rt) = if (l == "name" && text_side(r)) || (r == "name" && text_side(l)) {
                (
                    if keep(l) { "" } else { "text" },
                    if keep(r) { "" } else { "text" },
                )
            } else {
                (
                    if l == "text" { "" } else { "text" },
                    if r == "text" { "" } else { "text" },
                )
            };
            return Some((lt.into(), rt.into()));
        }
        // date / timestamp / timestamptz compare across one another with
        // operators of their own (`date_lt_timestamp` ...): no cast.
        let datetime = |t: &str| matches!(t, "date" | "timestamp" | "timestamptz");
        if datetime(l) && datetime(r) {
            return Some((String::new(), String::new()));
        }
        None
    }

    // ------------------------------------------------------------------
    // Expressions
    // ------------------------------------------------------------------

    /// Print `n`, coerced to `want` when that differs from its own type,
    /// directly (as `get_rule_expr` does).
    fn expr_as(&mut self, n: &Node, want: &str) -> Option<()> {
        self.child(n, want, Parent::Direct)
    }

    /// `get_rule_expr_paren`: `n` inside `parent`, coerced to `want`.
    fn child(&mut self, n: &Node, want: &str, parent: Parent<'_>) -> Option<()> {
        if !want.is_empty() {
            let own = self.typ(n)?;
            if own == "unknown" {
                // An untyped literal or NULL takes the wanted type directly.
                return self.const_as(n, want);
            }
            if own != want {
                if self.pretty {
                    self.child(n, "", Parent::Cast)?;
                    self.buf.push_str(&format!("::{}", display_type(want)));
                } else {
                    self.buf.push('(');
                    self.expr(n)?;
                    self.buf.push_str(&format!(")::{}", display_type(want)));
                }
                return Some(());
            }
        }
        if self.pretty && parent != Parent::Direct && !self.simple(n, parent) {
            self.buf.push('(');
            self.expr(n)?;
            self.buf.push(')');
            return Some(());
        }
        self.expr(n)
    }

    /// ruleutils' `isSimpleNode`: can `n` go inside `parent` unparenthesised?
    fn simple(&self, n: &Node, parent: Parent<'_>) -> bool {
        // The parents under which an operator-like node needs none.
        let group = |parent: Parent<'_>| matches!(parent, Parent::Bool(_));
        match n.node.as_ref() {
            Some(
                N::ColumnRef(_)
                | N::AConst(_)
                | N::ParamRef(_)
                | N::FuncCall(_)
                | N::CoalesceExpr(_)
                | N::MinMaxExpr(_)
                | N::CaseExpr(_)
                | N::AArrayExpr(_)
                | N::SqlvalueFunction(_)
                | N::TypeCast(_),
            ) => true,
            Some(N::AExpr(e)) => match AExprKind::try_from(e.kind) {
                Ok(AExprKind::AexprNullif) => true,
                Ok(AExprKind::AexprOp | AExprKind::AexprLike | AExprKind::AexprIlike) => {
                    if let Parent::Op(pop, first) = parent {
                        let op = crate::operator_name(e).unwrap_or("");
                        let (c, p) = (arith_priority(op), arith_priority(pop));
                        if c == 0 || p == 0 || e.lexpr.is_none() {
                            return false;
                        }
                        return match c.cmp(&p) {
                            std::cmp::Ordering::Greater => true,
                            std::cmp::Ordering::Less => false,
                            std::cmp::Ordering::Equal => first,
                        };
                    }
                    group(parent)
                }
                Ok(AExprKind::AexprDistinct | AExprKind::AexprNotDistinct) => group(parent),
                Ok(AExprKind::AexprBetween | AExprKind::AexprNotBetween) => {
                    // An AND (or OR) of two comparisons.
                    let own = if e.kind == AExprKind::AexprBetween as i32 {
                        BoolExprType::AndExpr
                    } else {
                        BoolExprType::OrExpr
                    };
                    bool_simple(own, parent)
                }
                _ => false,
            },
            Some(N::SubLink(_) | N::NullTest(_) | N::BooleanTest(_)) => group(parent),
            Some(N::BoolExpr(b)) => match BoolExprType::try_from(b.boolop) {
                Ok(t) => bool_simple(t, parent),
                Err(_) => false,
            },
            _ => false,
        }
    }

    /// An untyped literal or NULL, as a constant of type `t`.
    fn const_as(&mut self, n: &Node, t: &str) -> Option<()> {
        match n.node.as_ref()? {
            N::AConst(c) => match c.val.as_ref() {
                None => self.buf.push_str(&format!("NULL::{}", display_type(t))),
                Some(Val::Sval(s)) => {
                    let lit = s.sval.clone();
                    match t {
                        "int2" | "int4" | "int8" => {
                            let v: i64 = lit.trim().parse().ok()?;
                            self.const_number(&v.to_string(), t);
                        }
                        "numeric" => {
                            lit.trim().parse::<f64>().ok()?;
                            self.const_number(lit.trim(), t);
                        }
                        "bool" => {
                            let b = match lit.trim().to_ascii_lowercase().as_str() {
                                "t" | "true" | "yes" | "on" | "1" => "true",
                                "f" | "false" | "no" | "off" | "0" => "false",
                                _ => return None,
                            };
                            self.buf.push_str(b);
                        }
                        _ => self.buf.push_str(&format!(
                            "{}::{}",
                            crate::scalar::quote_literal(&lit),
                            display_type(t)
                        )),
                    }
                }
                _ => return None,
            },
            _ => return None,
        }
        Some(())
    }

    /// `get_const_expr` for a number: bare when it reads back as its type,
    /// quoted and labelled otherwise.
    fn const_number(&mut self, text: &str, t: &str) {
        let label = display_type(t);
        if text.starts_with('-') {
            self.buf.push_str(&format!("'{text}'::{label}"));
            return;
        }
        match t {
            "int4" => self.buf.push_str(text),
            "numeric" if text.contains(['.', 'e', 'E']) => self.buf.push_str(text),
            _ => self.buf.push_str(&format!("{text}::{label}")),
        }
    }

    fn expr(&mut self, n: &Node) -> Option<()> {
        match n.node.as_ref()? {
            N::ColumnRef(c) => {
                let fields = names(&c.fields);
                let (rel, col, _) = self.resolve(&fields)?;
                if self.unqualified {
                    self.buf.push_str(&q(&col));
                } else {
                    self.buf.push_str(&format!("{}.{}", q(&rel), q(&col)));
                }
            }
            N::AConst(c) => match c.val.as_ref() {
                None => self.buf.push_str("NULL::text"),
                Some(Val::Ival(i)) => self.const_number(&i.ival.to_string(), "int4"),
                Some(Val::Fval(f)) => {
                    let t = self.typ(n)?;
                    self.const_number(&f.fval, &t)
                }
                Some(Val::Boolval(b)) => {
                    self.buf.push_str(if b.boolval { "true" } else { "false" })
                }
                Some(Val::Sval(s)) => self
                    .buf
                    .push_str(&format!("{}::text", crate::scalar::quote_literal(&s.sval))),
                Some(Val::Bsval(_)) => return None,
            },
            N::TypeCast(tc) => {
                let tn = tc.type_name.as_ref()?;
                let target = base_type(&crate::type_name_of(tn));
                let typmod = crate::declared_typmod_public(tn);
                let label = crate::scalar::format_type_text_public(&target, Some(typmod));
                let arg = tc.arg.as_deref()?;
                let own = self.typ(arg)?;
                if own == "unknown" {
                    // The parser folds a literal into a constant of the
                    // target type.
                    if typmod >= 0 {
                        if let Some(N::AConst(ac)) = arg.node.as_ref() {
                            if let Some(Val::Sval(s)) = ac.val.as_ref() {
                                self.buf.push_str(&format!(
                                    "{}::{label}",
                                    crate::scalar::quote_literal(&s.sval)
                                ));
                                return Some(());
                            }
                        }
                    }
                    return self.const_as(arg, &target);
                }
                if own == target && typmod < 0 {
                    return self.expr(arg);
                }
                if self.pretty {
                    self.child(arg, "", Parent::Cast)?;
                    self.buf.push_str(&format!("::{label}"));
                } else {
                    self.buf.push('(');
                    self.expr(arg)?;
                    self.buf.push_str(&format!(")::{label}"));
                }
            }
            N::AExpr(e) => self.a_expr(e)?,
            N::BoolExpr(b) => {
                let op = BoolExprType::try_from(b.boolop).ok()?;
                let open = !self.pretty;
                if op == BoolExprType::NotExpr {
                    self.buf.push_str(if open { "(NOT " } else { "NOT " });
                    self.child(b.args.first()?, "", Parent::Bool(op))?;
                    if open {
                        self.buf.push(')');
                    }
                    return Some(());
                }
                let word = if op == BoolExprType::AndExpr {
                    " AND "
                } else {
                    " OR "
                };
                // The analyser flattens nested ANDs (and ORs).
                let mut flat = Vec::new();
                flatten_bool(b, op, &mut flat);
                if open {
                    self.buf.push('(');
                }
                for (i, a) in flat.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(word);
                    }
                    self.child(a, "", Parent::Bool(op))?;
                }
                if open {
                    self.buf.push(')');
                }
            }
            N::NullTest(t) => {
                let open = !self.pretty;
                if open {
                    self.buf.push('(');
                }
                self.child(t.arg.as_deref()?, "", Parent::Other)?;
                self.buf.push_str(
                    if NullTestType::try_from(t.nulltesttype).ok()? == NullTestType::IsNull {
                        " IS NULL"
                    } else {
                        " IS NOT NULL"
                    },
                );
                if open {
                    self.buf.push(')');
                }
            }
            N::BooleanTest(t) => {
                let open = !self.pretty;
                if open {
                    self.buf.push('(');
                }
                self.child(t.arg.as_deref()?, "", Parent::Other)?;
                self.buf
                    .push_str(match BoolTestType::try_from(t.booltesttype).ok()? {
                        BoolTestType::IsTrue => " IS TRUE",
                        BoolTestType::IsNotTrue => " IS NOT TRUE",
                        BoolTestType::IsFalse => " IS FALSE",
                        BoolTestType::IsNotFalse => " IS NOT FALSE",
                        BoolTestType::IsUnknown => " IS UNKNOWN",
                        BoolTestType::IsNotUnknown => " IS NOT UNKNOWN",
                        _ => return None,
                    });
                if open {
                    self.buf.push(')');
                }
            }
            N::FuncCall(f) => self.func_call(f)?,
            N::CoalesceExpr(c) => self.variadic_call("COALESCE", &c.args)?,
            N::MinMaxExpr(m) => {
                let word = if m.op == pg_query::protobuf::MinMaxOp::IsGreatest as i32 {
                    "GREATEST"
                } else {
                    "LEAST"
                };
                self.variadic_call(word, &m.args)?
            }
            N::CaseExpr(c) => self.case_expr(c)?,
            N::SubLink(s) => self.sublink(s)?,
            N::AArrayExpr(a) => {
                let t = self.typ(n)?;
                let elem = t.trim_end_matches("[]").to_string();
                self.buf.push_str("ARRAY[");
                for (i, x) in a.elements.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.expr_as(x, &elem)?;
                }
                self.buf.push(']');
            }
            N::SqlvalueFunction(v) => {
                use pg_query::protobuf::SqlValueFunctionOp as Op;
                self.buf.push_str(match Op::try_from(v.op).ok()? {
                    Op::SvfopCurrentDate => "CURRENT_DATE",
                    Op::SvfopCurrentTimestamp => "CURRENT_TIMESTAMP",
                    Op::SvfopLocaltimestamp => "LOCALTIMESTAMP",
                    Op::SvfopCurrentTime => "CURRENT_TIME",
                    Op::SvfopLocaltime => "LOCALTIME",
                    Op::SvfopCurrentUser => "CURRENT_USER",
                    Op::SvfopSessionUser => "SESSION_USER",
                    Op::SvfopUser => "CURRENT_USER",
                    Op::SvfopCurrentRole => "CURRENT_ROLE",
                    _ => return None,
                });
            }
            _ => return None,
        }
        Some(())
    }

    /// An operator expression: parenthesised unless pretty.
    fn binary(&mut self, l: &Node, lw: &str, op: &str, r: &Node, rw: &str) -> Option<()> {
        let open = !self.pretty;
        if open {
            self.buf.push('(');
        }
        self.child(l, lw, Parent::Op(op, true))?;
        self.buf.push_str(&format!(" {op} "));
        self.child(r, rw, Parent::Op(op, false))?;
        if open {
            self.buf.push(')');
        }
        Some(())
    }

    fn a_expr(&mut self, e: &pg_query::protobuf::AExpr) -> Option<()> {
        let kind = AExprKind::try_from(e.kind).ok()?;
        match kind {
            AExprKind::AexprOp => {
                let op = crate::operator_name(e).ok()?.to_string();
                let r = e.rexpr.as_deref()?;
                let Some(l) = e.lexpr.as_deref() else {
                    // A prefix operator.
                    self.buf.push_str(&format!("({op} "));
                    self.expr(r)?;
                    self.buf.push(')');
                    return Some(());
                };
                let (lt, rt) = (self.typ(l)?, self.typ(r)?);
                let (lw, rw) = if matches!(op.as_str(), "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=")
                {
                    Self::compare_coercion(&lt, &rt)?
                } else if matches!(
                    op.as_str(),
                    "~~" | "!~~" | "~~*" | "!~~*" | "~" | "~*" | "!~" | "!~*"
                ) {
                    Self::string_match_coercion(&lt, &rt)?
                } else {
                    let (a, b, _) = Self::arith_result(&op, &lt, &rt)?;
                    (a, b)
                };
                let op = if op == "!=" { "<>".to_string() } else { op };
                self.binary(l, &lw, &op, r, &rw)?;
            }
            AExprKind::AexprLike | AExprKind::AexprIlike => {
                let op = crate::operator_name(e).ok()?.to_string();
                let op = match (kind, op.as_str()) {
                    (AExprKind::AexprLike, "~~") => "~~",
                    (AExprKind::AexprLike, "!~~") => "!~~",
                    (AExprKind::AexprIlike, "~~*") => "~~*",
                    (AExprKind::AexprIlike, "!~~*") => "!~~*",
                    _ => return None,
                };
                let (l, r) = (e.lexpr.as_deref()?, e.rexpr.as_deref()?);
                let (lt, rt) = (self.typ(l)?, self.typ(r)?);
                let (lw, rw) = Self::string_match_coercion(&lt, &rt)?;
                self.binary(l, &lw, op, r, &rw)?;
            }
            AExprKind::AexprIn => {
                let op = crate::operator_name(e).ok()?.to_string();
                let l = e.lexpr.as_deref()?;
                let Some(N::List(list)) = e.rexpr.as_deref().and_then(|r| r.node.as_ref()) else {
                    return None;
                };
                let lt = self.typ(l)?;
                let mut all = vec![l.clone()];
                all.extend(list.items.iter().cloned());
                let common = self.common_type(&all)?;
                let lw = if lt == common {
                    String::new()
                } else {
                    common.clone()
                };
                let (word, quant) = if op == "=" {
                    ("=", "ANY")
                } else {
                    ("<>", "ALL")
                };
                // varchar has no `=` of its own: the operator is text's, so
                // the column and the whole array are cast to text.
                let via_text = matches!(common.as_str(), "varchar" | "name");
                let lw = if via_text { "text".to_string() } else { lw };
                let open = !self.pretty;
                if open {
                    self.buf.push('(');
                }
                self.child(l, &lw, Parent::Other)?;
                self.buf.push_str(&format!(" {word} {quant} ("));
                if via_text && open {
                    self.buf.push('(');
                }
                self.buf.push_str("ARRAY[");
                for (i, x) in list.items.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.expr_as(x, &common)?;
                }
                self.buf.push(']');
                if via_text {
                    if open {
                        self.buf.push(')');
                    }
                    self.buf.push_str("::text[]");
                }
                self.buf.push(')');
                if open {
                    self.buf.push(')');
                }
            }
            AExprKind::AexprBetween | AExprKind::AexprNotBetween => {
                let x = e.lexpr.as_deref()?;
                let Some(N::List(list)) = e.rexpr.as_deref().and_then(|r| r.node.as_ref()) else {
                    return None;
                };
                let [lo, hi] = list.items.as_slice() else {
                    return None;
                };
                let xt = self.typ(x)?;
                let (xl, lw) = Self::compare_coercion(&xt, &self.typ(lo)?)?;
                let (xh, hw) = Self::compare_coercion(&xt, &self.typ(hi)?)?;
                let not = kind == AExprKind::AexprNotBetween;
                let (a, b, join) = if not {
                    ("<", ">", " OR ")
                } else {
                    (">=", "<=", " AND ")
                };
                let open = !self.pretty;
                if open {
                    self.buf.push('(');
                }
                self.binary(x, &xl, a, lo, &lw)?;
                self.buf.push_str(join);
                self.binary(x, &xh, b, hi, &hw)?;
                if open {
                    self.buf.push(')');
                }
            }
            AExprKind::AexprDistinct | AExprKind::AexprNotDistinct => {
                let (l, r) = (e.lexpr.as_deref()?, e.rexpr.as_deref()?);
                let (lw, rw) = Self::compare_coercion(&self.typ(l)?, &self.typ(r)?)?;
                let open = !self.pretty;
                if open {
                    self.buf.push('(');
                }
                self.child(l, &lw, Parent::Other)?;
                self.buf.push_str(if kind == AExprKind::AexprDistinct {
                    " IS DISTINCT FROM "
                } else {
                    " IS NOT DISTINCT FROM "
                });
                self.child(r, &rw, Parent::Other)?;
                if open {
                    self.buf.push(')');
                }
            }
            AExprKind::AexprNullif => {
                let (l, r) = (e.lexpr.as_deref()?, e.rexpr.as_deref()?);
                let (lw, rw) = Self::compare_coercion(&self.typ(l)?, &self.typ(r)?)?;
                self.buf.push_str("NULLIF(");
                self.expr_as(l, &lw)?;
                self.buf.push_str(", ");
                self.expr_as(r, &rw)?;
                self.buf.push(')');
            }
            _ => return None,
        }
        Some(())
    }

    /// LIKE and the regex operators take text on both sides (a `char(n)`
    /// keeps its own `bpchar ~~ text`).
    fn string_match_coercion(l: &str, r: &str) -> Option<(String, String)> {
        if !is_stringish(l) || !is_stringish(r) {
            return None;
        }
        let lw = if matches!(l, "text" | "bpchar") {
            ""
        } else {
            "text"
        };
        let rw = if r == "text" { "" } else { "text" };
        Some((lw.into(), rw.into()))
    }

    fn variadic_call(&mut self, word: &str, args: &[Node]) -> Option<()> {
        let common = self.common_type(args)?;
        self.buf.push_str(word);
        self.buf.push('(');
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                self.buf.push_str(", ");
            }
            self.expr_as(a, &common)?;
        }
        self.buf.push(')');
        Some(())
    }

    fn func_call(&mut self, f: &pg_query::protobuf::FuncCall) -> Option<()> {
        if !f.agg_order.is_empty() || f.agg_filter.is_some() || f.func_variadic {
            return None;
        }
        let parts = names(&f.funcname);
        let name = parts.last()?.clone();
        if parts.len() > 1 && parts[0] != "pg_catalog" {
            return None;
        }
        // `EXTRACT(field FROM x)` is SQL syntax over `extract(text, x)`, and
        // prints as the syntax (`get_func_sql_syntax`).
        if name == "extract"
            && f.funcformat == pg_query::protobuf::CoercionForm::CoerceSqlSyntax as i32
            && f.args.len() == 2
            && f.over.is_none()
        {
            let field = match f.args[0].node.as_ref() {
                Some(N::AConst(c)) => match &c.val {
                    Some(pg_query::protobuf::a_const::Val::Sval(s)) => s.sval.clone(),
                    _ => return None,
                },
                _ => return None,
            };
            let types = vec!["text".to_string(), self.typ(&f.args[1])?];
            let declared = crate::funcsig::selected_args("extract", &types)?;
            self.buf.push_str(&format!("EXTRACT({} FROM ", field));
            self.expr_as(&f.args[1], declared.get(1)?)?;
            self.buf.push(')');
            return Some(());
        }
        let Some((arg_type, _)) = function_sig(&name) else {
            return self.call_by_signature(f, &name);
        };
        self.buf.push_str(&q(&name));
        self.buf.push('(');
        if f.agg_star {
            self.buf.push('*');
        }
        if f.agg_distinct {
            self.buf.push_str("DISTINCT ");
        }
        for (i, a) in f.args.iter().enumerate() {
            if i > 0 {
                self.buf.push_str(", ");
            }
            // The first argument of a text function is text; the others of
            // `substr` / `lpad` / ... are integers the analyser leaves be.
            let want = match arg_type {
                Some(t) => {
                    let own = self.typ(a)?;
                    // `char(n)` has its own length functions (bpcharlen).
                    let bpchar_ok = own == "bpchar"
                        && matches!(
                            name.as_str(),
                            "length" | "char_length" | "character_length" | "octet_length"
                        );
                    if bpchar_ok {
                        ""
                    } else if i == 0 || is_stringish(&own) {
                        if own == t {
                            ""
                        } else {
                            t
                        }
                    } else {
                        ""
                    }
                }
                None => {
                    // An untyped literal argument of a polymorphic function
                    // (`max('x')`) is text.
                    if self.typ(a)? == "unknown" {
                        "text"
                    } else {
                        ""
                    }
                }
            };
            self.expr_as(a, want)?;
        }
        self.buf.push(')');
        if let Some(over) = &f.over {
            self.buf.push_str(" OVER ");
            self.window_spec(over)?;
        }
        Some(())
    }

    /// A built-in outside `function_sig`'s table, printed when one of its
    /// `pg_proc` overloads declares exactly the arguments' types -- so no
    /// cast is implied -- as `int4range(a, (a + 1))`.
    fn call_by_signature(&mut self, f: &pg_query::protobuf::FuncCall, name: &str) -> Option<()> {
        if f.agg_star || f.agg_distinct || f.over.is_some() {
            return None;
        }
        let types = f
            .args
            .iter()
            .map(|a| self.typ(a))
            .collect::<Option<Vec<String>>>()?;
        // Otherwise the overload PostgreSQL's `func_select_candidate` picks,
        // with each argument coerced to its declared type as the analysed
        // query carries it: `to_char(d, 'YYYY')` over a date is
        // `to_char((d)::timestamp with time zone, 'YYYY'::text)`.
        let declared = if crate::funcsig::has_exact(name, &types) {
            vec![String::new(); types.len()]
        } else {
            let d = crate::funcsig::selected_args(name, &types)
                .or_else(|| crate::funcsig::resolved_args(name, &types))?;
            if d.len() != types.len() || d.iter().any(|t| t.starts_with("any") || t == "internal") {
                return None;
            }
            d
        };
        self.buf.push_str(&q(name));
        self.buf.push('(');
        for (i, a) in f.args.iter().enumerate() {
            if i > 0 {
                self.buf.push_str(", ");
            }
            self.expr_as(a, &declared[i])?;
        }
        self.buf.push(')');
        Some(())
    }

    fn window_spec(&mut self, w: &pg_query::protobuf::WindowDef) -> Option<()> {
        if !w.name.is_empty() && w.partition_clause.is_empty() && w.order_clause.is_empty() {
            self.buf.push_str(&q(&w.name));
            return Some(());
        }
        if !w.refname.is_empty() {
            return None;
        }
        self.buf.push('(');
        let mut need_space = false;
        if !w.partition_clause.is_empty() {
            self.buf.push_str("PARTITION BY ");
            for (i, p) in w.partition_clause.iter().enumerate() {
                if i > 0 {
                    self.buf.push_str(", ");
                }
                self.expr(p)?;
            }
            need_space = true;
        }
        if !w.order_clause.is_empty() {
            if need_space {
                self.buf.push(' ');
            }
            self.buf.push_str("ORDER BY ");
            self.sort_list(&w.order_clause)?;
            need_space = true;
        }
        self.frame(w, need_space)?;
        self.buf.push(')');
        Some(())
    }

    /// A non-default frame, as `get_rule_windowspec` prints it.
    fn frame(&mut self, w: &pg_query::protobuf::WindowDef, need_space: bool) -> Option<()> {
        let o = w.frame_options;
        if o & 0x00001 == 0 {
            return Some(());
        }
        // A RANGE offset over a NUMBER ordering prints as the constant it
        // is (measured on PostgreSQL 15 over int2 / int4 / int8 / float8 /
        // numeric); an interval offset over a date/time one is not
        // reproduced.
        if o & 0x00002 != 0 && (w.start_offset.is_some() || w.end_offset.is_some()) {
            let ordering = match w.order_clause.as_slice() {
                [one] => match one.node.as_ref() {
                    Some(N::SortBy(b)) => self.typ(b.node.as_deref()?),
                    _ => None,
                },
                _ => None,
            };
            let datetime = ordering
                .as_deref()
                .is_some_and(|t| matches!(t, "date" | "timestamp" | "timestamptz"));
            let interval = |n: Option<&Node>| match n.and_then(|n| n.node.as_ref()) {
                None => true,
                Some(N::TypeCast(tc)) => tc
                    .type_name
                    .as_ref()
                    .is_some_and(|t| crate::type_name_of(t) == "interval"),
                _ => false,
            };
            if datetime && interval(w.start_offset.as_deref()) && interval(w.end_offset.as_deref())
            {
                // `'1 day'::interval`, printed as the cast it is.
            } else if !ordering.as_deref().is_some_and(is_numberish) {
                return None;
            }
        }
        if need_space {
            self.buf.push(' ');
        }
        self.buf.push_str(if o & 0x00002 != 0 {
            "RANGE "
        } else if o & 0x00004 != 0 {
            "ROWS "
        } else if o & 0x00008 != 0 {
            "GROUPS "
        } else {
            return None;
        });
        let between = o & 0x00010 != 0;
        if between {
            self.buf.push_str("BETWEEN ");
        }
        let offset = |p: &mut Self, n: Option<&Node>| -> Option<()> {
            match n?.node.as_ref()? {
                N::AConst(c) if matches!(c.val, Some(Val::Ival(_) | Val::Fval(_))) => p.expr(n?),
                N::TypeCast(tc)
                    if tc
                        .type_name
                        .as_ref()
                        .is_some_and(|t| crate::type_name_of(t) == "interval") =>
                {
                    p.expr(n?)
                }
                _ => None,
            }
        };
        if o & 0x00020 != 0 {
            self.buf.push_str("UNBOUNDED PRECEDING ");
        } else if o & 0x00200 != 0 {
            self.buf.push_str("CURRENT ROW ");
        } else if o & 0x00800 != 0 {
            offset(self, w.start_offset.as_deref())?;
            self.buf.push_str(" PRECEDING ");
        } else if o & 0x02000 != 0 {
            offset(self, w.start_offset.as_deref())?;
            self.buf.push_str(" FOLLOWING ");
        } else {
            return None;
        }
        if between {
            self.buf.push_str("AND ");
            if o & 0x00100 != 0 {
                self.buf.push_str("UNBOUNDED FOLLOWING ");
            } else if o & 0x00040 != 0 {
                self.buf.push_str("UNBOUNDED PRECEDING ");
            } else if o & 0x00400 != 0 {
                self.buf.push_str("CURRENT ROW ");
            } else if o & 0x01000 != 0 {
                offset(self, w.end_offset.as_deref())?;
                self.buf.push_str(" PRECEDING ");
            } else if o & 0x04000 != 0 {
                offset(self, w.end_offset.as_deref())?;
                self.buf.push_str(" FOLLOWING ");
            } else {
                return None;
            }
        }
        if o & 0x08000 != 0 {
            self.buf.push_str("EXCLUDE CURRENT ROW ");
        } else if o & 0x10000 != 0 {
            self.buf.push_str("EXCLUDE GROUP ");
        } else if o & 0x20000 != 0 {
            self.buf.push_str("EXCLUDE TIES ");
        }
        self.buf.pop();
        Some(())
    }

    fn case_expr(&mut self, c: &pg_query::protobuf::CaseExpr) -> Option<()> {
        let mut results: Vec<Node> = c
            .args
            .iter()
            .filter_map(|w| match w.node.as_ref() {
                Some(N::CaseWhen(cw)) => cw.result.as_deref().cloned(),
                _ => None,
            })
            .collect();
        if let Some(d) = c.defresult.as_deref() {
            results.push(d.clone());
        }
        let common = self.common_type(&results)?;
        let arg_type = match c.arg.as_deref() {
            Some(a) => Some(self.typ(a)?),
            None => None,
        };
        self.keyword("CASE", 0, PRETTYINDENT_VAR, 0);
        if let Some(a) = c.arg.as_deref() {
            self.buf.push(' ');
            self.expr(a)?;
        }
        for w in &c.args {
            let Some(N::CaseWhen(cw)) = w.node.as_ref() else {
                return None;
            };
            self.keyword("WHEN ", 0, 0, 0);
            let cond = cw.expr.as_deref()?;
            match &arg_type {
                // `CASE x WHEN v`: the value is compared with x's type.
                Some(t) => {
                    let (_, vw) = Self::compare_coercion(t, &self.typ(cond)?)?;
                    self.expr_as(cond, &vw)?;
                }
                None => self.expr(cond)?,
            }
            self.buf.push_str(" THEN ");
            self.expr_as(cw.result.as_deref()?, &common)?;
        }
        self.keyword("ELSE ", 0, 0, 0);
        match c.defresult.as_deref() {
            Some(d) => self.expr_as(d, &common)?,
            None => self
                .buf
                .push_str(&format!("NULL::{}", display_type(&common))),
        }
        self.keyword("END", -PRETTYINDENT_VAR, 0, 0);
        Some(())
    }

    fn sublink(&mut self, s: &pg_query::protobuf::SubLink) -> Option<()> {
        let sub = s.subselect.as_deref()?;
        match SubLinkType::try_from(s.sub_link_type).ok()? {
            SubLinkType::ExistsSublink => {
                self.buf.push_str("(EXISTS (");
                self.subquery(sub)?;
                self.buf.push_str("))");
            }
            SubLinkType::ExprSublink => {
                self.buf.push('(');
                self.subquery(sub)?;
                self.buf.push(')');
            }
            SubLinkType::ArraySublink => {
                self.buf.push_str("ARRAY(");
                self.subquery(sub)?;
                self.buf.push(')');
            }
            SubLinkType::AnySublink => {
                let test = s.testexpr.as_deref()?;
                let names_ = names(&s.oper_name);
                let op = names_.last().map(String::as_str).unwrap_or("=");
                let lt = self.typ(test)?;
                let cols = self.subquery_columns(sub)?;
                let (lw, _) = Self::compare_coercion(&lt, &cols.first()?.1)?;
                if !lw.is_empty() {
                    return None;
                }
                self.buf.push('(');
                self.expr(test)?;
                if op == "=" && s.oper_name.is_empty() {
                    self.buf.push_str(" IN (");
                } else {
                    self.buf.push_str(&format!(" {op} ANY ("));
                }
                self.subquery(sub)?;
                self.buf.push_str("))");
            }
            _ => return None,
        }
        Some(())
    }

    /// A subquery printed at the current indentation, seeing this query's
    /// relations as outer references.
    fn subquery(&mut self, sub: &Node) -> Option<()> {
        let mut inner = Printer::new(self.cat, self.indent);
        inner.col_names_visible = false;
        inner.scopes = self.scopes.clone();
        inner.ctes = self.ctes.clone();
        inner.depth = self.depth + 1;
        inner.pretty = self.pretty;
        inner.markers = self.markers;
        inner.query(sub, None)?;
        self.buf.push_str(&inner.buf);
        Some(())
    }

    fn subquery_visible(&mut self, sub: &Node) -> Option<()> {
        let mut inner = Printer::new(self.cat, self.indent);
        inner.scopes = self.scopes.clone();
        inner.ctes = self.ctes.clone();
        inner.depth = self.depth + 1;
        inner.pretty = self.pretty;
        inner.markers = self.markers;
        inner.query(sub, None)?;
        self.buf.push_str(&inner.buf);
        Some(())
    }

    fn subquery_columns(&mut self, sub: &Node) -> Option<Vec<(String, String)>> {
        let mut inner = Printer::new(self.cat, 0);
        inner.scopes = self.scopes.clone();
        inner.ctes = self.ctes.clone();
        inner.depth = self.depth + 1;
        inner.pretty = self.pretty;
        inner.markers = self.markers;
        inner.query_columns(sub)
    }

    /// `get_rule_sortgroupclause`: a function call, aggregate or window
    /// function is parenthesised.
    fn sort_expr(&mut self, n: &Node) -> Option<()> {
        let is_var = matches!(n.node.as_ref(), Some(N::ColumnRef(_)));
        if !is_var && (self.pretty || matches!(n.node.as_ref(), Some(N::FuncCall(_)))) {
            self.buf.push('(');
            self.expr(n)?;
            self.buf.push(')');
            return Some(());
        }
        self.expr(n)
    }

    fn sort_list(&mut self, items: &[Node]) -> Option<()> {
        for (i, s) in items.iter().enumerate() {
            let Some(N::SortBy(sb)) = s.node.as_ref() else {
                return None;
            };
            if i > 0 {
                self.buf.push_str(", ");
            }
            if !sb.use_op.is_empty() {
                return None;
            }
            self.sort_expr(sb.node.as_deref()?)?;
            let dir = SortByDir::try_from(sb.sortby_dir).ok()?;
            let desc = dir == SortByDir::SortbyDesc;
            if desc {
                self.buf.push_str(" DESC");
            }
            match SortByNulls::try_from(sb.sortby_nulls).ok()? {
                SortByNulls::SortbyNullsFirst if !desc => self.buf.push_str(" NULLS FIRST"),
                SortByNulls::SortbyNullsLast if desc => self.buf.push_str(" NULLS LAST"),
                _ => {}
            }
        }
        Some(())
    }
}

fn flatten_bool<'n>(
    b: &'n pg_query::protobuf::BoolExpr,
    op: BoolExprType,
    out: &mut Vec<&'n Node>,
) {
    for a in &b.args {
        match a.node.as_ref() {
            Some(N::BoolExpr(inner)) if BoolExprType::try_from(inner.boolop).ok() == Some(op) => {
                flatten_bool(inner, op, out)
            }
            _ => out.push(a),
        }
    }
}

/// A declared type without its modifier, as the analyser compares types.
fn base_type(t: &str) -> String {
    let t = t.trim();
    let base = t.split('(').next().unwrap_or(t).trim();
    match base {
        "int" | "integer" => "int4",
        "smallint" => "int2",
        "bigint" => "int8",
        "real" => "float4",
        "double precision" => "float8",
        "decimal" => "numeric",
        "boolean" => "bool",
        "character varying" => "varchar",
        "character" | "char" => "bpchar",
        "serial" => "int4",
        "bigserial" => "int8",
        "smallserial" => "int2",
        other => other,
    }
    .to_string()
}

fn parse_select(sql: &str) -> Option<Node> {
    let parsed = pg_query::parse(sql).ok()?;
    let stmt = parsed.protobuf.stmts.first()?.stmt.as_deref()?.clone();
    Some(stmt)
}

/// `FigureColname`: the name the analyser gives an unaliased target.
fn figure_colname(n: &Node) -> String {
    let unnamed = "?column?".to_string();
    match n.node.as_ref() {
        Some(N::ColumnRef(c)) => names(&c.fields).pop().unwrap_or(unnamed),
        Some(N::FuncCall(f)) => names(&f.funcname).pop().unwrap_or(unnamed),
        Some(N::TypeCast(tc)) => {
            let inner = tc.arg.as_deref().map(figure_colname).unwrap_or_default();
            if inner != "?column?" && !inner.is_empty() {
                inner
            } else {
                tc.type_name
                    .as_ref()
                    .and_then(|t| names(&t.names).pop())
                    .unwrap_or(unnamed)
            }
        }
        Some(N::CaseExpr(_)) => "case".into(),
        Some(N::CoalesceExpr(_)) => "coalesce".into(),
        Some(N::MinMaxExpr(m)) => {
            if m.op == pg_query::protobuf::MinMaxOp::IsGreatest as i32 {
                "greatest".into()
            } else {
                "least".into()
            }
        }
        Some(N::AArrayExpr(_)) => "array".into(),
        Some(N::AExpr(e)) if e.kind == AExprKind::AexprNullif as i32 => "nullif".into(),
        Some(N::SubLink(s)) => match SubLinkType::try_from(s.sub_link_type) {
            Ok(SubLinkType::ExistsSublink) => "exists".into(),
            Ok(SubLinkType::ArraySublink) => "array".into(),
            Ok(SubLinkType::ExprSublink) => {
                match s.subselect.as_deref().and_then(|x| x.node.as_ref()) {
                    Some(N::SelectStmt(sel)) => sel
                        .target_list
                        .first()
                        .and_then(|t| match t.node.as_ref() {
                            Some(N::ResTarget(rt)) if !rt.name.is_empty() => Some(rt.name.clone()),
                            Some(N::ResTarget(rt)) => rt.val.as_deref().map(figure_colname),
                            _ => None,
                        })
                        .unwrap_or(unnamed),
                    _ => unnamed,
                }
            }
            _ => unnamed,
        },
        Some(N::SqlvalueFunction(v)) => {
            use pg_query::protobuf::SqlValueFunctionOp as Op;
            match Op::try_from(v.op) {
                Ok(Op::SvfopCurrentDate) => "current_date",
                Ok(Op::SvfopCurrentTimestamp) => "current_timestamp",
                Ok(Op::SvfopLocaltimestamp) => "localtimestamp",
                Ok(Op::SvfopCurrentTime) => "current_time",
                Ok(Op::SvfopLocaltime) => "localtime",
                Ok(Op::SvfopCurrentUser) | Ok(Op::SvfopUser) => "current_user",
                Ok(Op::SvfopSessionUser) => "session_user",
                Ok(Op::SvfopCurrentRole) => "current_role",
                _ => "?column?",
            }
            .into()
        }
        _ => unnamed,
    }
}

/// One output column of a select list: its expression, its name, and
/// whether it is a bare column reference of that same name.
struct Target {
    expr: Node,
    name: String,
}

impl Printer<'_> {
    /// The FROM items' relations, each as ruleutils names it.
    fn collect_rtes(&mut self, items: &[Node], out: &mut Vec<Rte>) -> Option<()> {
        for it in items {
            match it.node.as_ref()? {
                N::RangeVar(rv) => {
                    if !rv.schemaname.is_empty() && rv.schemaname != "public" {
                        return None;
                    }
                    let alias = rv.alias.as_ref();
                    let mut columns = self.relation_columns(&rv.relname)?;
                    let renames = alias.map(|a| names(&a.colnames)).unwrap_or_default();
                    if renames.len() > columns.len() {
                        return None;
                    }
                    for (c, n) in columns.iter_mut().zip(&renames) {
                        c.0 = n.clone();
                    }
                    let name = alias
                        .map(|a| a.aliasname.clone())
                        .unwrap_or_else(|| rv.relname.clone());
                    let refname = self.unique_refname(&name, out);
                    out.push(Rte {
                        refname,
                        name,
                        columns,
                        qualified_only: false,
                    });
                }
                N::JoinExpr(j) => {
                    if j.alias.is_some() {
                        return None;
                    }
                    self.collect_rtes(
                        &[j.larg.as_deref()?.clone(), j.rarg.as_deref()?.clone()],
                        out,
                    )?;
                }
                N::RangeSubselect(rs) => {
                    let alias = rs.alias.as_ref()?;
                    // A LATERAL subquery sees the items before it.
                    if rs.lateral {
                        self.scopes.push(out.clone());
                    }
                    let columns = self.subquery_columns(rs.subquery.as_deref()?);
                    if rs.lateral {
                        self.scopes.pop();
                    }
                    let mut columns = columns?;
                    let renames = names(&alias.colnames);
                    if renames.len() > columns.len() {
                        return None;
                    }
                    for (c, n) in columns.iter_mut().zip(&renames) {
                        c.0 = n.clone();
                    }
                    let name = alias.aliasname.clone();
                    let refname = self.unique_refname(&name, out);
                    out.push(Rte {
                        refname,
                        name,
                        columns,
                        qualified_only: false,
                    });
                }
                _ => return None,
            }
        }
        Some(())
    }

    /// `set_rtable_names`: `name`, or `name_N` for the least N free, when an
    /// enclosing query or an earlier item of this one already uses it.
    fn unique_refname(&self, name: &str, level: &[Rte]) -> String {
        let taken = |n: &str| {
            level.iter().any(|r| r.refname == n)
                || self.scopes.iter().flatten().any(|r| r.refname == n)
        };
        if !taken(name) {
            return name.to_string();
        }
        (1..)
            .map(|i| format!("{name}_{i}"))
            .find(|n| !taken(n))
            .unwrap_or_else(|| name.to_string())
    }

    /// A select list with `*` expanded.
    fn targets(&mut self, s: &pg_query::protobuf::SelectStmt, rtes: &[Rte]) -> Option<Vec<Target>> {
        let mut out = Vec::new();
        for t in &s.target_list {
            let Some(N::ResTarget(rt)) = t.node.as_ref() else {
                return None;
            };
            let val = rt.val.as_deref()?;
            if let Some(N::ColumnRef(c)) = val.node.as_ref() {
                let is_star = c
                    .fields
                    .last()
                    .is_some_and(|f| matches!(f.node.as_ref(), Some(N::AStar(_))));
                if is_star {
                    let qual = names(&c.fields);
                    for r in rtes {
                        if !qual.is_empty() && qual.last() != Some(&r.name) {
                            continue;
                        }
                        for (col, _) in &r.columns {
                            out.push(Target {
                                expr: column_ref(&[&r.refname, col]),
                                name: col.clone(),
                            });
                        }
                    }
                    continue;
                }
            }
            let name = if rt.name.is_empty() {
                figure_colname(val)
            } else {
                rt.name.clone()
            };
            out.push(Target {
                expr: val.clone(),
                name,
            });
        }
        Some(out)
    }

    /// A query's output columns and their types.
    fn query_columns(&mut self, n: &Node) -> Option<Vec<(String, String)>> {
        let Some(N::SelectStmt(s)) = n.node.as_ref() else {
            return None;
        };
        let pushed_ctes = self.ctes.len();
        if let Some(w) = &s.with_clause {
            for c in &w.ctes {
                let Some(N::CommonTableExpr(cte)) = c.node.as_ref() else {
                    return None;
                };
                let cols = self.cte_columns(cte, w.recursive)?;
                self.ctes.push((cte.ctename.clone(), cols));
            }
        }
        let out = if s.op != SetOperation::SetopNone as i32 {
            let l = self.query_columns(&Node {
                node: Some(N::SelectStmt(s.larg.clone()?)),
            })?;
            let r = self.query_columns(&Node {
                node: Some(N::SelectStmt(s.rarg.clone()?)),
            })?;
            let mut out = Vec::new();
            for ((ln, lt), (_, rt)) in l.into_iter().zip(r) {
                let t = if lt == rt {
                    lt
                } else if lt == "unknown" {
                    rt
                } else if rt == "unknown" {
                    lt
                } else if let Some((m, _)) = numeric_meet(&lt, &rt) {
                    if m.is_empty() {
                        lt
                    } else {
                        m.to_string()
                    }
                } else if is_stringish(&lt) && is_stringish(&rt) {
                    "text".into()
                } else {
                    return None;
                };
                out.push((ln, t));
            }
            Some(out)
        } else if !s.values_lists.is_empty() {
            self.values_types(s).map(|ts| {
                ts.into_iter()
                    .enumerate()
                    .map(|(i, t)| (format!("column{}", i + 1), t))
                    .collect()
            })
        } else {
            let mut rtes = Vec::new();
            self.collect_rtes(&s.from_clause, &mut rtes)?;
            self.scopes.push(rtes.clone());
            let targets = self.targets(s, &rtes);
            let out = targets.and_then(|ts| {
                ts.iter()
                    .map(|t| {
                        let ty = self.typ(&t.expr)?;
                        Some((
                            t.name.clone(),
                            if ty == "unknown" { "text".into() } else { ty },
                        ))
                    })
                    .collect::<Option<Vec<_>>>()
            });
            self.scopes.pop();
            out
        };
        self.ctes.truncate(pushed_ctes);
        out
    }

    /// `get_query_def`: print a query at the current indentation.
    fn query(&mut self, n: &Node, colnames: Option<&[String]>) -> Option<()> {
        if self.depth > 32 {
            return None;
        }
        let Some(N::SelectStmt(s)) = n.node.as_ref() else {
            return None;
        };
        if s.into_clause.is_some() || !s.locking_clause.is_empty() {
            return None;
        }
        let pushed_ctes = self.ctes.len();
        if let Some(w) = &s.with_clause {
            // This level's relations take their names BEFORE the WITH bodies
            // print (`set_deparse_for_query`), so a CTE body's reference to
            // a name the query also uses becomes `name_1`.
            for c in &w.ctes {
                let Some(N::CommonTableExpr(cte)) = c.node.as_ref() else {
                    return None;
                };
                let cols = self.cte_columns(cte, w.recursive)?;
                self.ctes.push((cte.ctename.clone(), cols));
            }
            let mut level = Vec::new();
            if s.op == SetOperation::SetopNone as i32 {
                self.collect_rtes(&s.from_clause, &mut level)?;
            }
            self.ctes.truncate(pushed_ctes);
            self.scopes.push(level);
            let printed = self.with_clause(w);
            self.scopes.pop();
            printed?;
        }
        if s.op != SetOperation::SetopNone as i32 {
            let outputs = match colnames {
                Some(c) => c.to_vec(),
                None => self.query_columns(n)?.into_iter().map(|(n, _)| n).collect(),
            };
            self.setop(s, &outputs)?;
            // A set operation's ORDER BY names its output columns, and
            // ruleutils prints them by NUMBER (`force_colno`).
            if !s.sort_clause.is_empty() {
                self.keyword(" ORDER BY ", -PRETTYINDENT_STD, PRETTYINDENT_STD, 1);
                let mut resolved = Vec::new();
                for sb in &s.sort_clause {
                    let Some(N::SortBy(b)) = sb.node.as_ref() else {
                        return None;
                    };
                    let pos = match b.node.as_deref()?.node.as_ref()? {
                        N::AConst(c) => match c.val.as_ref()? {
                            Val::Ival(i) => i.ival,
                            _ => return None,
                        },
                        N::ColumnRef(c) if c.fields.len() == 1 => {
                            let name = names(&c.fields).pop()?;
                            let at = outputs.iter().position(|n| *n == name)?;
                            i32::try_from(at + 1).ok()?
                        }
                        _ => return None,
                    };
                    let mut b = (**b).clone();
                    b.node = Some(Box::new(Node {
                        node: Some(N::AConst(pg_query::protobuf::AConst {
                            val: Some(Val::Ival(pg_query::protobuf::Integer { ival: pos })),
                            ..Default::default()
                        })),
                    }));
                    resolved.push(Node {
                        node: Some(N::SortBy(Box::new(b))),
                    });
                }
                self.sort_list(&resolved)?;
            }
            self.offset_limit(s)?;
        } else {
            self.basic_select(s, colnames)?;
        }
        self.ctes.truncate(pushed_ctes);
        Some(())
    }

    fn with_clause(&mut self, w: &pg_query::protobuf::WithClause) -> Option<()> {
        self.indent += PRETTYINDENT_STD;
        self.buf.push(' ');
        let mut sep = if w.recursive {
            "WITH RECURSIVE "
        } else {
            "WITH "
        };
        for c in &w.ctes {
            let Some(N::CommonTableExpr(cte)) = c.node.as_ref() else {
                return None;
            };
            let materialized =
                match pg_query::protobuf::CteMaterialize::try_from(cte.ctematerialized) {
                    Ok(pg_query::protobuf::CteMaterialize::Always) => "MATERIALIZED ",
                    Ok(pg_query::protobuf::CteMaterialize::Never) => "NOT MATERIALIZED ",
                    _ => "",
                };
            let body = cte.ctequery.as_deref()?;
            let cols = self.cte_columns(cte, w.recursive)?;
            // A recursive body reads itself.
            if w.recursive {
                self.ctes.push((cte.ctename.clone(), cols.clone()));
            }
            self.buf.push_str(sep);
            self.buf.push_str(&q(&cte.ctename));
            let aliases = names(&cte.aliascolnames);
            if !aliases.is_empty() {
                let quoted: Vec<String> = aliases.iter().map(|a| q(a)).collect();
                self.buf.push_str(&format!("({})", quoted.join(", ")));
            }
            self.buf.push_str(" AS ");
            self.buf.push_str(materialized);
            self.buf.push('(');
            self.keyword("", 0, 0, 0);
            let mut inner = Printer::new(self.cat, self.indent);
            inner.scopes = self.scopes.clone();
            inner.ctes = self.ctes.clone();
            inner.depth = self.depth + 1;
            inner.pretty = self.pretty;
            inner.markers = self.markers;
            inner.query(body, None)?;
            self.buf.push_str(&inner.buf);
            self.keyword("", 0, 0, 0);
            self.buf.push(')');
            if !w.recursive {
                self.ctes.push((cte.ctename.clone(), cols));
            }
            sep = ", ";
        }
        self.indent -= PRETTYINDENT_STD;
        self.keyword("", 0, 0, 0);
        Some(())
    }

    /// A CTE's output columns: its body's (a recursive one's non-recursive
    /// term's), renamed by its column list.
    fn cte_columns(
        &mut self,
        cte: &pg_query::protobuf::CommonTableExpr,
        recursive: bool,
    ) -> Option<Vec<(String, String)>> {
        let body = cte.ctequery.as_deref()?;
        let Some(N::SelectStmt(b)) = body.node.as_ref() else {
            return None;
        };
        let mut cols = if recursive && b.op != SetOperation::SetopNone as i32 {
            self.query_columns(&Node {
                node: Some(N::SelectStmt(b.larg.clone()?)),
            })?
        } else {
            self.query_columns(body)?
        };
        let aliases = names(&cte.aliascolnames);
        if aliases.len() > cols.len() {
            return None;
        }
        for (c, a) in cols.iter_mut().zip(aliases) {
            c.0 = a;
        }
        Some(cols)
    }

    /// `get_setop_query`.
    fn setop(&mut self, s: &pg_query::protobuf::SelectStmt, colnames: &[String]) -> Option<()> {
        let op = SetOperation::try_from(s.op).ok()?;
        if op == SetOperation::SetopNone {
            let leaf_paren = s.with_clause.is_some()
                || !s.sort_clause.is_empty()
                || s.limit_count.is_some()
                || s.limit_offset.is_some();
            let sub = if leaf_paren {
                self.buf.push('(');
                self.keyword("", PRETTYINDENT_STD, 0, 0);
                PRETTYINDENT_STD
            } else {
                0
            };
            let mut inner = Printer::new(self.cat, self.indent);
            inner.scopes = self.scopes.clone();
            inner.ctes = self.ctes.clone();
            inner.depth = self.depth + 1;
            inner.pretty = self.pretty;
            inner.markers = self.markers;
            inner.col_names_visible = self.col_names_visible;
            inner.query(
                &Node {
                    node: Some(N::SelectStmt(Box::new(s.clone()))),
                },
                Some(colnames),
            )?;
            self.buf.push_str(&inner.buf);
            if leaf_paren {
                self.keyword(")", -sub, 0, 0);
            }
            return Some(());
        }
        let larg = s.larg.as_deref()?;
        let rarg = s.rarg.as_deref()?;
        let need_paren =
            larg.op != SetOperation::SetopNone as i32 && (larg.op != s.op || larg.all != s.all);
        let sub = if need_paren {
            self.buf.push('(');
            self.keyword("", PRETTYINDENT_STD, 0, 0);
            PRETTYINDENT_STD
        } else {
            0
        };
        self.setop(larg, colnames)?;
        if need_paren {
            self.keyword(") ", -sub, 0, 0);
        } else {
            self.keyword("", -sub, 0, 0);
        }
        self.buf.push_str(match op {
            SetOperation::SetopUnion => "UNION ",
            SetOperation::SetopIntersect => "INTERSECT ",
            SetOperation::SetopExcept => "EXCEPT ",
            SetOperation::SetopNone | SetOperation::Undefined => return None,
        });
        if s.all {
            self.buf.push_str("ALL ");
        }
        let need_paren = rarg.op != SetOperation::SetopNone as i32;
        let sub = if need_paren {
            self.buf.push('(');
            PRETTYINDENT_STD
        } else {
            0
        };
        self.keyword("", sub, 0, 0);
        // Only the leftmost query's column names are visible outside.
        let visible = std::mem::replace(&mut self.col_names_visible, false);
        let printed = self.setop(rarg, colnames);
        self.col_names_visible = visible;
        printed?;
        self.indent -= sub;
        if need_paren {
            self.keyword(")", 0, 0, 0);
        }
        Some(())
    }

    /// `get_basic_select_query` plus the ORDER BY / OFFSET / LIMIT of
    /// `get_select_query_def`.
    fn basic_select(
        &mut self,
        s: &pg_query::protobuf::SelectStmt,
        colnames: Option<&[String]>,
    ) -> Option<()> {
        if s.group_distinct {
            return None;
        }
        if !s.values_lists.is_empty() {
            self.buf.push(' ');
            return self.values_def(s);
        }
        let mut rtes = Vec::new();
        self.collect_rtes(&s.from_clause, &mut rtes)?;
        self.scopes.push(rtes.clone());
        let targets = self.targets(s, &rtes)?;
        self.indent += PRETTYINDENT_STD;
        self.buf.push(' ');
        self.buf.push_str("SELECT");
        if !s.distinct_clause.is_empty() {
            let plain = s.distinct_clause.len() == 1 && s.distinct_clause[0].node.is_none();
            if plain {
                self.buf.push_str(" DISTINCT");
            } else {
                self.buf.push_str(" DISTINCT ON (");
                for (i, d) in s.distinct_clause.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    let e = self.sort_target(d, &targets)?;
                    self.sort_expr(&e)?;
                }
                self.buf.push(')');
            }
        }
        // get_target_list, wrapping every column after the first.
        let mut sep = " ";
        for (i, t) in targets.iter().enumerate() {
            self.buf.push_str(sep);
            sep = ", ";
            let saved = std::mem::take(&mut self.buf);
            self.expr(&t.expr)?;
            let name = colnames
                .and_then(|c| c.get(i).cloned())
                .unwrap_or_else(|| t.name.clone());
            let plain_var = match t.expr.node.as_ref() {
                Some(N::ColumnRef(c)) => names(&c.fields).last() == Some(&name),
                _ => !self.col_names_visible && name == "?column?",
            };
            if !plain_var {
                self.buf.push_str(" AS ");
                self.buf.push_str(&q(&name));
            }
            let item = std::mem::replace(&mut self.buf, saved);
            if self.markers {
                self.mark(if i == 0 { 'T' } else { 't' }, &item);
                continue;
            }
            if item.starts_with('\n') {
                self.remove_trailing_spaces();
            } else if i > 0 {
                self.keyword("", -PRETTYINDENT_STD, PRETTYINDENT_STD, PRETTYINDENT_VAR);
            }
            self.buf.push_str(&item);
        }
        // get_from_clause.
        for (i, it) in s.from_clause.iter().enumerate() {
            if i == 0 {
                self.keyword(" FROM ", -PRETTYINDENT_STD, PRETTYINDENT_STD, 2);
                self.print_from_item(it)?;
            } else {
                self.buf.push_str(", ");
                let saved = std::mem::take(&mut self.buf);
                self.print_from_item(it)?;
                let item = std::mem::replace(&mut self.buf, saved);
                if self.markers {
                    self.mark('f', &item);
                    continue;
                }
                if item.starts_with('\n') {
                    self.remove_trailing_spaces();
                } else {
                    self.keyword("", -PRETTYINDENT_STD, PRETTYINDENT_STD, PRETTYINDENT_VAR);
                }
                self.buf.push_str(&item);
            }
        }
        if let Some(w) = s.where_clause.as_deref() {
            self.keyword(" WHERE ", -PRETTYINDENT_STD, PRETTYINDENT_STD, 1);
            self.expr(w)?;
        }
        if !s.group_clause.is_empty() {
            self.keyword(" GROUP BY ", -PRETTYINDENT_STD, PRETTYINDENT_STD, 1);
            for (i, g) in s.group_clause.iter().enumerate() {
                if i > 0 {
                    self.buf.push_str(", ");
                }
                let e = self.group_target(g, &targets)?;
                self.sort_expr(&e)?;
            }
        }
        if let Some(h) = s.having_clause.as_deref() {
            self.keyword(" HAVING ", -PRETTYINDENT_STD, PRETTYINDENT_STD, 0);
            self.expr(h)?;
        }
        let mut sep: Option<&str> = None;
        for w in &s.window_clause {
            let Some(N::WindowDef(wd)) = w.node.as_ref() else {
                return None;
            };
            if wd.name.is_empty() {
                continue;
            }
            match sep {
                None => self.keyword(" WINDOW ", -PRETTYINDENT_STD, PRETTYINDENT_STD, 1),
                Some(x) => self.buf.push_str(x),
            }
            self.buf.push_str(&format!("{} AS ", q(&wd.name)));
            let mut spec = (**wd).clone();
            spec.name = String::new();
            self.window_spec(&spec)?;
            sep = Some(", ");
        }
        if !s.sort_clause.is_empty() {
            self.keyword(" ORDER BY ", -PRETTYINDENT_STD, PRETTYINDENT_STD, 1);
            let mut resolved = Vec::new();
            for sb in &s.sort_clause {
                let Some(N::SortBy(b)) = sb.node.as_ref() else {
                    return None;
                };
                let mut b = (**b).clone();
                b.node = Some(Box::new(self.sort_target(b.node.as_deref()?, &targets)?));
                resolved.push(Node {
                    node: Some(N::SortBy(Box::new(b))),
                });
            }
            self.sort_list(&resolved)?;
        }
        self.offset_limit(s)?;
        self.scopes.pop();
        Some(())
    }

    /// `OFFSET` / `LIMIT`, as `get_select_query_def` prints them.
    fn offset_limit(&mut self, s: &pg_query::protobuf::SelectStmt) -> Option<()> {
        if let Some(o) = s.limit_offset.as_deref() {
            self.keyword(" OFFSET ", -PRETTYINDENT_STD, PRETTYINDENT_STD, 0);
            self.expr(o)?;
        }
        if let Some(l) = s.limit_count.as_deref() {
            if s.limit_option != pg_query::protobuf::LimitOption::Count as i32 {
                return None;
            }
            self.keyword(" LIMIT ", -PRETTYINDENT_STD, PRETTYINDENT_STD, 0);
            match l.node.as_ref() {
                Some(N::AConst(c)) if c.val.is_none() => self.buf.push_str("ALL"),
                _ => self.expr(l)?,
            }
        }
        Some(())
    }

    /// An ORDER BY / DISTINCT ON item: an output column's number or name
    /// stands for its expression.
    fn sort_target(&self, n: &Node, targets: &[Target]) -> Option<Node> {
        match n.node.as_ref()? {
            N::AConst(c) => match c.val.as_ref() {
                Some(Val::Ival(i)) => {
                    let k = usize::try_from(i.ival).ok()?.checked_sub(1)?;
                    Some(targets.get(k)?.expr.clone())
                }
                _ => Some(n.clone()),
            },
            N::ColumnRef(c) if c.fields.len() == 1 => {
                let name = names(&c.fields).pop()?;
                match targets.iter().filter(|t| t.name == name).count() {
                    0 => Some(n.clone()),
                    1 => Some(targets.iter().find(|t| t.name == name)?.expr.clone()),
                    _ => None,
                }
            }
            _ => Some(n.clone()),
        }
    }

    /// A GROUP BY item: an input column first, then an output name.
    fn group_target(&self, n: &Node, targets: &[Target]) -> Option<Node> {
        match n.node.as_ref()? {
            N::AConst(_) => self.sort_target(n, targets),
            N::ColumnRef(c) if c.fields.len() == 1 => {
                if self.resolve(&names(&c.fields)).is_some() {
                    Some(n.clone())
                } else {
                    self.sort_target(n, targets)
                }
            }
            _ => Some(n.clone()),
        }
    }

    /// This level's relation the query wrote as `name`.
    fn level_rte(&self, name: &str) -> Option<Rte> {
        self.scopes.last()?.iter().find(|r| r.name == name).cloned()
    }

    /// `(a, b, c)` after an item's alias.
    fn column_alias_list(&mut self, rte: &Rte) {
        let cols: Vec<String> = rte.columns.iter().map(|(n, _)| q(n)).collect();
        self.buf.push_str(&format!("({})", cols.join(", ")));
    }

    /// `get_values_def`: `VALUES (1,'a'::text), (2,'b'::text)`, each column
    /// coerced to the rows' common type.
    fn values_def(&mut self, v: &pg_query::protobuf::SelectStmt) -> Option<()> {
        let types = self.values_types(v)?;
        self.buf.push_str("VALUES ");
        for (i, row) in v.values_lists.iter().enumerate() {
            let Some(N::List(l)) = row.node.as_ref() else {
                return None;
            };
            if i > 0 {
                self.buf.push_str(", ");
            }
            self.buf.push('(');
            for (j, item) in l.items.iter().enumerate() {
                if j > 0 {
                    self.buf.push(',');
                }
                // An implicit numeric widening prints as its argument.
                let want = types.get(j)?;
                let own = self.typ(item)?;
                if own != *want && is_numberish(&own) && is_numberish(want) {
                    self.expr(item)?;
                } else {
                    self.expr_as(item, want)?;
                }
            }
            self.buf.push(')');
        }
        Some(())
    }

    /// Each VALUES column's type: the rows' common type, `text` for an
    /// all-unknown column.
    fn values_types(&mut self, v: &pg_query::protobuf::SelectStmt) -> Option<Vec<String>> {
        let rows: Vec<&Vec<Node>> = v
            .values_lists
            .iter()
            .map(|r| match r.node.as_ref() {
                Some(N::List(l)) => Some(&l.items),
                _ => None,
            })
            .collect::<Option<_>>()?;
        let width = rows.first()?.len();
        (0..width)
            .map(|j| {
                let col: Vec<Node> = rows
                    .iter()
                    .map(|r| r.get(j).cloned())
                    .collect::<Option<_>>()?;
                let t = self.common_type(&col)?;
                Some(if t == "unknown" { "text".into() } else { t })
            })
            .collect()
    }

    /// `get_from_clause_item`.
    fn print_from_item(&mut self, it: &Node) -> Option<()> {
        match it.node.as_ref()? {
            N::RangeVar(rv) => {
                // A table of another schema is stored `schema.name`: printed
                // qualified while its schema is off the search path (as
                // `generate_relation_name` does), its alias compared with
                // the bare name.
                let bare = match rv.relname.split_once('.') {
                    Some((schema, bare)) => {
                        if !crate::schemas::search_path().iter().any(|s| s == schema) {
                            self.buf.push_str(&q(schema));
                            self.buf.push('.');
                        }
                        bare.to_string()
                    }
                    None => rv.relname.clone(),
                };
                self.buf.push_str(&q(&bare));
                let written = rv
                    .alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .unwrap_or_else(|| rv.relname.clone());
                let rte = self.level_rte(&written)?;
                let shown = if rv.relname.contains('.') {
                    rte.refname != bare
                } else {
                    rv.alias.is_some() || rte.refname != rv.relname
                };
                if shown {
                    self.buf.push(' ');
                    self.buf.push_str(&q(&rte.refname));
                }
                // An alias list names EVERY column once it names any.
                if rv.alias.as_ref().is_some_and(|a| !a.colnames.is_empty()) {
                    self.column_alias_list(&rte);
                }
            }
            N::RangeSubselect(rs) => {
                if rs.lateral {
                    self.buf.push_str("LATERAL ");
                }
                self.buf.push('(');
                let sub = rs.subquery.as_deref()?;
                if matches!(sub.node.as_ref(), Some(N::SelectStmt(s)) if !s.values_lists.is_empty())
                {
                    let Some(N::SelectStmt(v)) = sub.node.as_ref() else {
                        return None;
                    };
                    self.buf.push(' ');
                    self.values_def(v)?;
                } else {
                    self.subquery_visible(sub)?;
                }
                self.buf.push(')');
                self.buf.push(' ');
                let alias = rs.alias.as_ref()?;
                let rte = self.level_rte(&alias.aliasname)?;
                self.buf.push_str(&q(&rte.refname));
                if !alias.colnames.is_empty() {
                    self.column_alias_list(&rte);
                }
            }
            N::JoinExpr(j) => {
                let open = !self.pretty;
                if open {
                    self.buf.push('(');
                }
                self.print_from_item(j.larg.as_deref()?)?;
                let jt = JoinType::try_from(j.jointype).ok()?;
                let word = match jt {
                    JoinType::JoinInner if j.is_natural => " NATURAL JOIN ",
                    JoinType::JoinInner if j.quals.is_none() && j.using_clause.is_empty() => {
                        " CROSS JOIN "
                    }
                    JoinType::JoinInner => " JOIN ",
                    JoinType::JoinLeft if j.is_natural => " NATURAL LEFT JOIN ",
                    JoinType::JoinLeft => " LEFT JOIN ",
                    JoinType::JoinRight if j.is_natural => " NATURAL RIGHT JOIN ",
                    JoinType::JoinRight => " RIGHT JOIN ",
                    JoinType::JoinFull if j.is_natural => " NATURAL FULL JOIN ",
                    JoinType::JoinFull => " FULL JOIN ",
                    _ => return None,
                };
                self.keyword(word, -PRETTYINDENT_STD, PRETTYINDENT_STD, PRETTYINDENT_JOIN);
                self.print_from_item(j.rarg.as_deref()?)?;
                if !j.using_clause.is_empty() {
                    let cols: Vec<String> = names(&j.using_clause).iter().map(|c| q(c)).collect();
                    self.buf.push_str(&format!(" USING ({})", cols.join(", ")));
                } else if let Some(qual) = j.quals.as_deref() {
                    self.buf.push_str(" ON ");
                    if open {
                        self.buf.push('(');
                    }
                    self.expr(qual)?;
                    if open {
                        self.buf.push(')');
                    }
                }
                if open {
                    self.buf.push(')');
                }
            }
            _ => return None,
        }
        Some(())
    }
}

fn column_ref(parts: &[&str]) -> Node {
    Node {
        node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
            fields: parts
                .iter()
                .map(|p| Node {
                    node: Some(N::String(pg_query::protobuf::String {
                        sval: (*p).to_string(),
                    })),
                })
                .collect(),
            location: -1,
        })),
    }
}

/// `pg_get_viewdef(view)`: the view's `SELECT` as ruleutils prints it, with
/// its leading space and trailing semicolon; `None` for a shape this does
/// not reproduce.
pub fn viewdef(sql: &str, cat: &Catalog<'_>) -> Option<String> {
    viewdef_with(sql, cat, false)
}

/// `pg_get_viewdef(view, true)`: parentheses only where precedence needs
/// them (PRETTYFLAG_PAREN).
pub fn viewdef_pretty(sql: &str, cat: &Catalog<'_>) -> Option<String> {
    viewdef_with(sql, cat, true)
}

fn viewdef_with(sql: &str, cat: &Catalog<'_>, pretty: bool) -> Option<String> {
    let node = parse_select(sql)?;
    let mut p = Printer::new(cat, 0);
    p.pretty = pretty;
    p.query(&node, None)?;
    Some(format!("{};", p.buf))
}

const MARK_OPEN: char = '\u{1}';
const MARK_ITEM: char = '\u{2}';
const MARK_CLOSE: char = '\u{3}';

/// `pg_get_viewdef(view, wrap_column)`: pretty, with a target-list or FROM
/// item moved to a new line only where ruleutils' `get_target_list` /
/// `get_from_clause` would for that column. `marked` is
/// [`viewdef_marked`]'s output.
pub fn render_wrapped(marked: &str, wrap: i64) -> String {
    let chars: Vec<char> = marked.chars().collect();
    render_level(&chars, wrap)
}

/// One buffer's worth: each item is rendered into a buffer of its own
/// first (as ruleutils prints it into `targetbuf` / `itembuf`), so a nested
/// list's wrapping is measured from that buffer's own last line.
fn render_level(s: &[char], wrap: i64) -> String {
    let mut out = String::new();
    let mut last_multiline = false;
    let mut i = 0;
    while i < s.len() {
        if s[i] != MARK_OPEN {
            out.push(s[i]);
            i += 1;
            continue;
        }
        let kind = s[i + 1];
        let mut j = i + 2;
        let mut nl = String::new();
        while s[j] != MARK_ITEM {
            nl.push(s[j]);
            j += 1;
        }
        let start = j + 1;
        let mut depth = 0;
        let mut k = start;
        loop {
            match s[k] {
                MARK_OPEN => depth += 1,
                MARK_CLOSE if depth == 0 => break,
                MARK_CLOSE => depth -= 1,
                _ => {}
            }
            k += 1;
        }
        let item = render_level(&s[start..k], wrap);
        if kind == 'T' {
            last_multiline = false;
        }
        // A negative column turns wrapping off altogether.
        if wrap >= 0 && item.starts_with('\n') {
            while out.ends_with(' ') {
                out.pop();
            }
        } else if wrap >= 0 && kind != 'T' {
            let line = out.rsplit('\n').next().unwrap_or("").len();
            let overflow = (line + item.len()) as i64 > wrap;
            if overflow || (kind == 't' && last_multiline) {
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push_str(&nl);
            }
        }
        if kind != 'f' {
            // `strchr(targetbuf + leading_nl_pos + 1, '\n')`.
            let rest = item.strip_prefix('\n').unwrap_or(&item);
            last_multiline = rest.contains('\n');
        }
        out.push_str(&item);
        i = k + 1;
    }
    out
}

/// `viewdef_pretty` with each wrap decision left to [`render_wrapped`].
pub fn viewdef_marked(sql: &str, cat: &Catalog<'_>) -> Option<String> {
    let node = parse_select(sql)?;
    let mut p = Printer::new(cat, 0);
    p.pretty = true;
    p.markers = true;
    p.query(&node, None)?;
    Some(format!("{};", p.buf))
}

/// `isSimpleNode` for a boolean expression of type `own` under `parent`.
fn bool_simple(own: BoolExprType, parent: Parent<'_>) -> bool {
    match parent {
        Parent::Bool(p) => match own {
            BoolExprType::NotExpr | BoolExprType::AndExpr => {
                matches!(p, BoolExprType::AndExpr | BoolExprType::OrExpr)
            }
            BoolExprType::OrExpr => p == BoolExprType::OrExpr,
            _ => false,
        },
        _ => false,
    }
}

impl Printer<'_> {
    /// A rule action: `get_insert_query_def` / `_update_` / `_delete_`.
    fn dml(&mut self, n: &Node) -> Option<()> {
        match n.node.as_ref()? {
            N::InsertStmt(ins) => {
                if ins.on_conflict_clause.is_some()
                    || !ins.returning_list.is_empty()
                    || ins.with_clause.is_some()
                {
                    return None;
                }
                let rel = ins.relation.as_ref()?;
                if rel.alias.is_some() {
                    return None;
                }
                let columns = self.relation_columns(&rel.relname)?;
                self.indent += PRETTYINDENT_STD;
                self.buf.push(' ');
                self.buf
                    .push_str(&format!("INSERT INTO {} ", q(&rel.relname)));
                let targets: Vec<(String, String)> = if ins.cols.is_empty() {
                    columns.clone()
                } else {
                    ins.cols
                        .iter()
                        .map(|c| match c.node.as_ref() {
                            Some(N::ResTarget(rt)) if rt.indirection.is_empty() => {
                                columns.iter().find(|(n, _)| *n == rt.name).cloned()
                            }
                            _ => None,
                        })
                        .collect::<Option<Vec<_>>>()?
                };
                let Some(N::SelectStmt(sel)) =
                    ins.select_stmt.as_deref().and_then(|s| s.node.as_ref())
                else {
                    return None;
                };
                let [row] = sel.values_lists.as_slice() else {
                    return None;
                };
                let Some(N::List(values)) = row.node.as_ref() else {
                    return None;
                };
                let used = &targets[..values.items.len().min(targets.len())];
                let cols: Vec<String> = used.iter().map(|(n, _)| q(n)).collect();
                self.buf.push_str(&format!("({}) ", cols.join(", ")));
                self.keyword("VALUES (", -PRETTYINDENT_STD, PRETTYINDENT_STD, 2);
                for (i, (v, (_, ty))) in values.items.iter().zip(used).enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    // Only an untyped literal shows the column's type; an
                    // assignment cast is implicit and not printed here.
                    if self.typ(v)? == "unknown" {
                        self.const_as(v, ty)?;
                    } else {
                        self.expr(v)?;
                    }
                }
                self.buf.push(')');
            }
            N::UpdateStmt(u) => {
                if !u.from_clause.is_empty()
                    || !u.returning_list.is_empty()
                    || u.with_clause.is_some()
                {
                    return None;
                }
                let rel = u.relation.as_ref()?;
                if rel.alias.is_some() {
                    return None;
                }
                let columns = self.relation_columns(&rel.relname)?;
                self.scopes.last_mut()?.push(Rte {
                    refname: rel.relname.clone(),
                    name: rel.relname.clone(),
                    columns: columns.clone(),
                    qualified_only: false,
                });
                self.indent += PRETTYINDENT_STD;
                self.buf.push(' ');
                self.buf
                    .push_str(&format!("UPDATE {} SET ", q(&rel.relname)));
                for (i, t) in u.target_list.iter().enumerate() {
                    let Some(N::ResTarget(rt)) = t.node.as_ref() else {
                        return None;
                    };
                    if !rt.indirection.is_empty() {
                        return None;
                    }
                    let ty = columns.iter().find(|(n, _)| *n == rt.name)?.1.clone();
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.buf.push_str(&format!("{} = ", q(&rt.name)));
                    let v = rt.val.as_deref()?;
                    if self.typ(v)? == "unknown" {
                        self.const_as(v, &ty)?;
                    } else {
                        self.expr(v)?;
                    }
                }
                if let Some(w) = u.where_clause.as_deref() {
                    self.keyword(" WHERE ", -PRETTYINDENT_STD, PRETTYINDENT_STD, 1);
                    self.expr(w)?;
                }
            }
            N::DeleteStmt(d) => {
                if !d.using_clause.is_empty()
                    || !d.returning_list.is_empty()
                    || d.with_clause.is_some()
                {
                    return None;
                }
                let rel = d.relation.as_ref()?;
                if rel.alias.is_some() {
                    return None;
                }
                let columns = self.relation_columns(&rel.relname)?;
                self.scopes.last_mut()?.push(Rte {
                    refname: rel.relname.clone(),
                    name: rel.relname.clone(),
                    columns,
                    qualified_only: false,
                });
                self.indent += PRETTYINDENT_STD;
                self.buf.push(' ');
                self.buf
                    .push_str(&format!("DELETE FROM {}", q(&rel.relname)));
                if let Some(w) = d.where_clause.as_deref() {
                    self.keyword(" WHERE ", -PRETTYINDENT_STD, PRETTYINDENT_STD, 1);
                    self.expr(w)?;
                }
            }
            N::SelectStmt(_) => self.query(n, None)?,
            _ => return None,
        }
        Some(())
    }
}

/// `pg_get_ruledef` / `pg_rules.definition` for an INSERT / UPDATE /
/// DELETE rule (`make_ruledef` under PRETTYFLAG_INDENT).
pub fn rule_def(
    name: &str,
    table: &str,
    event: &str,
    instead: bool,
    condition: Option<&str>,
    actions: &[String],
    cat: &Catalog<'_>,
) -> Option<String> {
    let mut base = Printer::new(cat, 0);
    let columns = base.relation_columns(table)?;
    // NEW and OLD, as the event gives them.
    let mut pseudo = Vec::new();
    if event != "DELETE" {
        pseudo.push(Rte {
            refname: "new".into(),
            name: "new".into(),
            columns: columns.clone(),
            qualified_only: true,
        });
    }
    if event != "INSERT" {
        pseudo.push(Rte {
            refname: "old".into(),
            name: "old".into(),
            columns,
            qualified_only: true,
        });
    }
    let mut out = format!(
        "CREATE RULE {} AS\n    ON {event} TO public.{}",
        q(name),
        q(table)
    );
    if let Some(c) = condition {
        let node = parse_select(&format!("SELECT {c}"))?;
        let Some(N::SelectStmt(s)) = node.node.as_ref() else {
            return None;
        };
        let Some(N::ResTarget(rt)) = s.target_list.first()?.node.as_ref() else {
            return None;
        };
        let mut p = Printer::new(cat, 0);
        p.scopes.push(pseudo.clone());
        p.expr(rt.val.as_deref()?)?;
        out.push_str("\n   WHERE ");
        out.push_str(&p.buf);
    }
    out.push_str(" DO ");
    if instead {
        out.push_str("INSTEAD ");
    }
    let render = |sql: &str| -> Option<String> {
        let node = parse_select(sql)?;
        let mut p = Printer::new(cat, 0);
        p.scopes.push(pseudo.clone());
        p.dml(&node)?;
        Some(p.buf)
    };
    match actions {
        [] => out.push_str("NOTHING"),
        [one] => out.push_str(&render(one)?),
        many => {
            out.push('(');
            for a in many {
                out.push_str(&render(a)?);
                out.push_str(";\n");
            }
            out.push(')');
        }
    }
    out.push(';');
    Some(out)
}

/// A single-relation WHERE as EXPLAIN shows it: each top-level AND conjunct
/// printed as ruleutils prints it (columns unqualified), with the column it
/// compares to a constant when it is an index-usable shape
/// (`col op const`, op one of `= < <= > >=`). `None` for a shape the printer
/// does not know.
pub fn explain_conjuncts(
    where_clause: &pg_query::protobuf::Node,
    def: &TableDef,
) -> Option<Vec<(String, Option<String>)>> {
    let mut parts = Vec::new();
    fn flatten<'n>(n: &'n Node, out: &mut Vec<&'n Node>) {
        match n.node.as_ref() {
            Some(N::BoolExpr(b)) if b.boolop == BoolExprType::AndExpr as i32 => {
                for a in &b.args {
                    flatten(a, out);
                }
            }
            _ => out.push(n),
        }
    }
    flatten(where_clause, &mut parts);
    parts
        .into_iter()
        .map(|p| {
            let text = expr_node_def(p, def)?;
            let column = match p.node.as_ref() {
                Some(N::AExpr(e))
                    if e.kind == AExprKind::AexprOp as i32
                        && matches!(
                            crate::operator_name(e).ok(),
                            Some("=" | "<" | "<=" | ">" | ">=")
                        ) =>
                {
                    let col = |n: Option<&Node>| match n.and_then(|n| n.node.as_ref()) {
                        Some(N::ColumnRef(c)) => names(&c.fields).pop(),
                        _ => None,
                    };
                    let konst = |n: Option<&Node>| {
                        matches!(
                            n.and_then(|n| n.node.as_ref()),
                            Some(N::AConst(_) | N::TypeCast(_) | N::ParamRef(_))
                        )
                    };
                    let (l, r) = (e.lexpr.as_deref(), e.rexpr.as_deref());
                    if konst(r) {
                        col(l)
                    } else if konst(l) {
                        col(r)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            Some((text, column))
        })
        .collect()
}

/// Each JOIN's ON condition in a SELECT's FROM, post-order (a left-deep
/// join's inner joins first), printed as EXPLAIN does (qualified).
pub fn join_conditions(
    s: &pg_query::protobuf::SelectStmt,
    cat: &Catalog<'_>,
) -> Option<Vec<String>> {
    let mut p = Printer::new(cat, 0);
    let mut rtes = Vec::new();
    p.collect_rtes(&s.from_clause, &mut rtes)?;
    p.scopes.push(rtes);
    fn walk(p: &mut Printer<'_>, n: &Node, out: &mut Vec<String>) -> Option<()> {
        if let Some(N::JoinExpr(j)) = n.node.as_ref() {
            walk(p, j.larg.as_deref()?, out)?;
            walk(p, j.rarg.as_deref()?, out)?;
            match j.quals.as_deref() {
                Some(q) => {
                    p.buf.clear();
                    p.expr(q)?;
                    out.push(std::mem::take(&mut p.buf));
                }
                None => out.push(String::new()),
            }
        }
        Some(())
    }
    let mut out = Vec::new();
    for item in &s.from_clause {
        walk(&mut p, item, &mut out)?;
    }
    Some(out)
}

/// A joined SELECT's WHERE as EXPLAIN places it: each AND conjunct printed
/// qualified, with the relation it belongs to when it names exactly one
/// relation that no outer join makes nullable (PostgreSQL pushes that one
/// down to the relation's scan; any other stays on the join).
pub fn join_where(
    s: &pg_query::protobuf::SelectStmt,
    cat: &Catalog<'_>,
) -> Option<Vec<(String, Option<String>)>> {
    let w = s.where_clause.as_deref()?;
    let mut p = Printer::new(cat, 0);
    let mut rtes = Vec::new();
    p.collect_rtes(&s.from_clause, &mut rtes)?;
    p.scopes.push(rtes);
    // The relations an outer join can null-extend.
    fn refnames(n: &Node, out: &mut Vec<String>) {
        match n.node.as_ref() {
            Some(N::RangeVar(r)) => out.push(
                r.alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .unwrap_or_else(|| r.relname.clone()),
            ),
            Some(N::RangeSubselect(r)) => {
                if let Some(a) = r.alias.as_ref() {
                    out.push(a.aliasname.clone());
                }
            }
            Some(N::JoinExpr(j)) => {
                for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
                    refnames(side, out);
                }
            }
            _ => {}
        }
    }
    fn nullable(n: &Node, out: &mut Vec<String>) {
        if let Some(N::JoinExpr(j)) = n.node.as_ref() {
            let (l, r) = (j.larg.as_deref(), j.rarg.as_deref());
            match JoinType::try_from(j.jointype) {
                Ok(JoinType::JoinLeft) => r.into_iter().for_each(|x| refnames(x, out)),
                Ok(JoinType::JoinRight) => l.into_iter().for_each(|x| refnames(x, out)),
                Ok(JoinType::JoinFull) => {
                    [l, r].into_iter().flatten().for_each(|x| refnames(x, out))
                }
                _ => {}
            }
            for side in [l, r].into_iter().flatten() {
                nullable(side, out);
            }
        }
    }
    let mut nulls = Vec::new();
    for item in &s.from_clause {
        nullable(item, &mut nulls);
    }
    let mut parts = Vec::new();
    fn flatten<'n>(n: &'n Node, out: &mut Vec<&'n Node>) {
        match n.node.as_ref() {
            Some(N::BoolExpr(b)) if b.boolop == BoolExprType::AndExpr as i32 => {
                for a in &b.args {
                    flatten(a, out);
                }
            }
            _ => out.push(n),
        }
    }
    flatten(w, &mut parts);
    let mut out = Vec::new();
    for part in parts {
        p.buf.clear();
        p.expr(part)?;
        let text = std::mem::take(&mut p.buf);
        let mut rels: Vec<String> = Vec::new();
        let mut copy = part.clone();
        crate::walk_expr(&mut copy, &mut |n| {
            if let Some(N::ColumnRef(c)) = n.node.as_ref() {
                if let Some((rel, _, _)) = p.resolve(&names(&c.fields)) {
                    if !rels.contains(&rel) {
                        rels.push(rel);
                    }
                }
            }
            Ok(())
        })
        .ok()?;
        let home = match rels.as_slice() {
            [one] if !nulls.contains(one) => Some(one.clone()),
            _ => None,
        };
        // A scan's own filter prints its columns bare (`show_scan_qual`).
        let text = if home.is_some() {
            p.unqualified = true;
            p.buf.clear();
            p.expr(part)?;
            p.unqualified = false;
            std::mem::take(&mut p.buf)
        } else {
            text
        };
        out.push((text, home));
    }
    Some(out)
}

/// An expression node over one table, printed as `pg_get_expr` does.
pub fn expr_node_def(n: &Node, def: &TableDef) -> Option<String> {
    let none_lookup = |_: &str| None;
    let none_views = |_: &str| None;
    let cat = Catalog {
        lookup: &none_lookup,
        view_sql: &none_views,
    };
    let mut p = Printer::new(&cat, 0);
    p.unqualified = true;
    p.scopes.push(vec![Rte {
        refname: def.name.clone(),
        name: def.name.clone(),
        columns: def
            .columns
            .iter()
            .map(|c| (c.name.clone(), base_type(&c.pg_type)))
            .collect(),
        qualified_only: false,
    }]);
    p.expr(n)?;
    Some(p.buf)
}

/// `pg_get_expr(expr, relid)`: an expression over one table's columns, as
/// ruleutils prints a stored default, generated column, CHECK or policy
/// (columns unqualified). `None` for a shape this does not reproduce.
pub fn expr_def(sql: &str, def: &TableDef) -> Option<String> {
    let node = parse_select(&format!("SELECT {sql}"))?;
    let Some(N::SelectStmt(s)) = node.node.as_ref() else {
        return None;
    };
    let [target] = s.target_list.as_slice() else {
        return None;
    };
    let Some(N::ResTarget(rt)) = target.node.as_ref() else {
        return None;
    };
    let none_lookup = |_: &str| None;
    let none_views = |_: &str| None;
    let cat = Catalog {
        lookup: &none_lookup,
        view_sql: &none_views,
    };
    let mut p = Printer::new(&cat, 0);
    p.unqualified = true;
    p.scopes.push(vec![Rte {
        refname: def.name.clone(),
        name: def.name.clone(),
        columns: def
            .columns
            .iter()
            .map(|c| (c.name.clone(), base_type(&c.pg_type)))
            .collect(),
        qualified_only: false,
    }]);
    p.expr(rt.val.as_deref()?)?;
    Some(p.buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use secantus_pgcatalog::Column;

    fn cat_lookup(n: &str) -> Option<TableDef> {
        match n {
            "vd_t" => Some(TableDef::new(
                "vd_t",
                vec![
                    Column::new("id", "int4", true),
                    Column::new("v", "varchar", false),
                    Column::new("t", "text", false),
                    Column::new("n", "numeric", false),
                    Column::new("d", "date", false),
                    Column::new("b", "bool", false),
                ],
            )),
            "vd_u" => Some(TableDef::new(
                "vd_u",
                vec![
                    Column::new("id", "int4", false),
                    Column::new("t_id", "int4", false),
                    Column::new("w", "text", false),
                ],
            )),
            _ => None,
        }
    }

    fn render(sql: &str) -> Option<String> {
        let none = |_: &str| None;
        let cat = Catalog {
            lookup: &cat_lookup,
            view_sql: &none,
        };
        viewdef(sql, &cat)
    }

    #[test]
    fn a_simple_view_reads_as_ruleutils_prints_it() {
        assert_eq!(
            render("SELECT id, t FROM vd_t").as_deref(),
            Some(" SELECT vd_t.id,\n    vd_t.t\n   FROM vd_t;")
        );
    }
}
