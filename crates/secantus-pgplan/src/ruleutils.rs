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

/// One relation in a query's FROM, as ruleutils names it: a range-table
/// entry, with its `deparse_columns`. A level's entries are kept in
/// PostgreSQL's range-table order -- a JOIN after both of its inputs.
#[derive(Clone, Debug, Default)]
struct Rte {
    /// The name ruleutils PRINTS: the alias or relation name, made unique
    /// against the enclosing queries' and this level's earlier names
    /// (`t_1`), as `set_rtable_names` does. Empty for an unnamed JOIN.
    refname: String,
    /// The name the query WROTE, which its column references use (empty
    /// for an unnamed JOIN).
    name: String,
    /// Each column's name as the query references it (`eref->colnames`),
    /// and its type.
    columns: Vec<(String, String)>,
    /// Named only when qualified: a rule's NEW / OLD, a JOIN's input under
    /// an unnamed JOIN (`setNamespaceColumnVisibility`), a USING alias.
    qualified_only: bool,
    /// Out of the namespace: an input of an aliased JOIN, which hides it
    /// completely. The index is the innermost JOIN that hid it (or whose
    /// unnamed form hid its columns, for `cols_hidden_by`), so that JOIN's
    /// own ON clause still sees it.
    hidden_by: Option<usize>,
    cols_hidden_by: Option<usize>,
    kind: RteKind,
    /// A relation's catalog column names (`real_colnames`); empty for the
    /// other kinds, whose real names are `columns`'.
    real: Vec<String>,
    /// Each column's type modifier, where known (`-1` for none).
    typmods: Vec<Option<i32>>,
    /// The user wrote an alias column list.
    alias_cols: bool,
    /// `colinfo->colnames`, as `set_deparse_for_query` chose them; empty
    /// until then, when the columns' own names stand.
    printed: Vec<String>,
    /// `colinfo->printaliases`.
    print_aliases: bool,
    join: Option<Box<JoinRte>>,
    /// A JOIN ... USING's `AS alias`: the JOIN's index in the level. It
    /// names the JOIN's merged columns only.
    using_alias_of: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum RteKind {
    #[default]
    Relation,
    Subquery,
    Function,
    Join,
    /// A CTE, or anything else `set_relation_column_names` treats as
    /// "other".
    Other,
}

/// A JOIN's range-table entry: where each of its columns comes from
/// (`identify_join_columns`).
#[derive(Clone, Debug)]
struct JoinRte {
    /// The first index of the JOIN's inputs in the level (its subtree is
    /// `start..` its own index).
    start: usize,
    left: usize,
    right: usize,
    /// For each column, the input column it is: `leftattnos` /
    /// `rightattnos`, both for a merged one.
    sources: Vec<(Option<usize>, Option<usize>)>,
    /// The USING columns' names as written (or a NATURAL join's common
    /// ones), the join's first columns.
    using: Vec<String>,
    /// For each merged column, the input whose plain column its join alias
    /// variable is (`buildMergedJoinVar`): `Some(false)` left, `Some(true)`
    /// right, `None` when it is a COALESCE or a coercion ("dangerous").
    merged_var: Vec<Option<bool>>,
    named: bool,
    alias_cols: Vec<String>,
    /// `colinfo->usingNames`, as `set_using_names` chose them.
    using_names: Vec<String>,
}

/// A column reference already resolved to a level's entry, which `*`
/// expansion produces: `(scope, entry, column)` encoded in a ColumnRef
/// whose first field is this marker.
const VAR_MARK: &str = "\u{1f}var";

fn var_ref(scope: usize, rte: usize, col: usize) -> Node {
    column_ref(&[
        VAR_MARK,
        &scope.to_string(),
        &rte.to_string(),
        &col.to_string(),
    ])
}

/// A type whose modifier is part of the type (`atttypmod`).
fn has_typmod(t: &str) -> bool {
    matches!(
        t.trim_end_matches("[]"),
        "varchar"
            | "bpchar"
            | "numeric"
            | "bit"
            | "varbit"
            | "time"
            | "timetz"
            | "timestamp"
            | "timestamptz"
            | "interval"
    )
}

/// `NAMEDATALEN - 1`.
const MAX_IDENT: usize = 63;

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
    /// EXPLAIN prints the planned expression, whose constants
    /// `eval_const_expressions` has folded: an integer literal coerced to
    /// `oid` is the constant `'1259'::oid`, not `(1259)::oid`.
    fold_consts: bool,
    /// Leave each target-list / FROM-list line-wrap decision to
    /// [`render_wrapped`] (the int `wrapColumn` form of `pg_get_viewdef`)
    /// rather than wrapping every item, as the default column (0) does.
    markers: bool,
    /// EXPLAIN's join helpers: a JOIN's inputs stay plain entries of the
    /// level, and an aliased JOIN is not modelled (`None`).
    legacy_joins: bool,
    /// Whether the statement wrote the alias of each `RangeVar` of the text
    /// being printed, by its location there (`rte->alias != NULL`); see
    /// [`Printer::alias_written`].
    written_alias: std::rc::Rc<std::collections::HashMap<i32, bool>>,
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
        | "bool_or" | "every" | "percentile_cont" | "percentile_disc" | "mode" => (None, None),
        _ => return None,
    })
}

/// A `jsonb` operator over a jsonb left operand: `(the right operand's
/// type, the result's)` -- the overload an untyped literal right operand
/// resolves to (`->` and `->>` take text over integer for one).
fn jsonb_operator(op: &str, r: &str) -> Option<(&'static str, &'static str)> {
    Some(match (op, r) {
        ("->", "int4") => ("int4", "jsonb"),
        ("->>", "int4") => ("int4", "text"),
        ("->", _) => ("text", "jsonb"),
        ("->>", _) => ("text", "text"),
        ("#>", _) => ("text[]", "jsonb"),
        ("#>>", _) => ("text[]", "text"),
        ("?", _) => ("text", "bool"),
        ("?|" | "?&", _) => ("text[]", "bool"),
        ("@>" | "<@", _) => ("jsonb", "bool"),
        _ => return None,
    })
}

/// The built-ins taking `VARIADIC "any"`, by result type: each argument is
/// taken as it is, an untyped literal printed bare.
fn variadic_any(name: &str) -> Option<&'static str> {
    Some(match name {
        "jsonb_build_object" | "jsonb_build_array" => "jsonb",
        "json_build_object" | "json_build_array" => "json",
        _ => return None,
    })
}

/// A `json` operator over a json left operand, as [`jsonb_operator`].
fn json_operator(op: &str, r: &str) -> Option<(&'static str, &'static str)> {
    Some(match (op, r) {
        ("->", "int4") => ("int4", "json"),
        ("->>", "int4") => ("int4", "text"),
        ("->", _) => ("text", "json"),
        ("->>", _) => ("text", "text"),
        ("#>", _) => ("text[]", "json"),
        ("#>>", _) => ("text[]", "text"),
        _ => return None,
    })
}

/// Is `t` an array type?
fn is_array(t: &str) -> bool {
    t.ends_with("[]")
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
            fold_consts: false,
            markers: false,
            legacy_joins: false,
            written_alias: std::rc::Rc::default(),
        }
    }

    /// `rte->alias != NULL` for a FROM reference of the printed text. A
    /// reference qualification folded to its `schema.name` key carries its
    /// bare name as an alias that was not written (`schemas::add_alias`),
    /// which the text cannot tell from one that was: the definition's
    /// [`crate::schemas::written_aliases`], when known, says which. Without
    /// them (a definition stored before they were kept), an alias that is
    /// the bare name of a `schema.name` key is taken for the added one.
    fn alias_written(&self, rv: &pg_query::protobuf::RangeVar) -> bool {
        let Some(alias) = rv.alias.as_ref() else {
            return false;
        };
        if let Some(&written) = self.written_alias.get(&rv.location) {
            return written;
        }
        !(alias.colnames.is_empty()
            && rv
                .relname
                .split_once('.')
                .is_some_and(|(_, bare)| bare == alias.aliasname))
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

    /// Resolve a column reference as ruleutils prints it: `(refname,
    /// column, type)`, the refname empty where the column prints
    /// unqualified (a "dangerous" merged USING column of an unnamed JOIN).
    fn resolve(&self, fields: &[String]) -> Option<(String, String, String)> {
        let (scope, rte, col) = self.lookup(fields)?;
        self.var_name(scope, rte, col, 0)
    }

    /// The entry a column reference names: `(scope, entry, column)`.
    fn lookup(&self, fields: &[String]) -> Option<(usize, usize, usize)> {
        if let [mark, scope, rte, col] = fields {
            if mark == VAR_MARK {
                return Some((scope.parse().ok()?, rte.parse().ok()?, col.parse().ok()?));
            }
        }
        // A column named once in the entry (twice is ambiguous).
        let position = |r: &Rte, c: &str| -> Option<Option<usize>> {
            let mut hits = r.columns.iter().enumerate().filter(|(_, (n, _))| n == c);
            match (hits.next(), hits.next()) {
                (Some((i, _)), None) => Some(Some(i)),
                (None, _) => Some(None),
                _ => None,
            }
        };
        for (si, scope) in self.scopes.iter().enumerate().rev() {
            match fields {
                [col] => {
                    let mut found = None;
                    for (ri, r) in scope.iter().enumerate() {
                        if r.qualified_only || r.hidden_by.is_some() || r.cols_hidden_by.is_some() {
                            continue;
                        }
                        if let Some(ci) = position(r, col)? {
                            if found.is_some() {
                                return None;
                            }
                            found = Some((si, ri, ci));
                        }
                    }
                    if found.is_some() {
                        return found;
                    }
                }
                [.., rel, col] => {
                    if let Some(ri) = scope
                        .iter()
                        .position(|r| r.hidden_by.is_none() && !r.name.is_empty() && r.name == *rel)
                    {
                        return Some((si, ri, position(&scope[ri], col)??));
                    }
                }
                _ => return None,
            }
        }
        None
    }

    /// `get_variable`: an unnamed JOIN's column prints as the input column
    /// its join alias variable is, and any other as `refname.colname` with
    /// the name `set_deparse_for_query` chose.
    fn var_name(
        &self,
        scope: usize,
        rte: usize,
        col: usize,
        depth: usize,
    ) -> Option<(String, String, String)> {
        let r = self.scopes.get(scope)?.get(rte)?;
        if depth > 64 {
            return None;
        }
        if let Some(j) = r.using_alias_of {
            return self.var_name(scope, j, col, depth + 1);
        }
        let printed = |r: &Rte, c: usize| {
            r.printed
                .get(c)
                .cloned()
                .or_else(|| r.columns.get(c).map(|x| x.0.clone()))
        };
        if let Some(j) = r.join.as_deref().filter(|j| !j.named) {
            let (l, rr) = *j.sources.get(col)?;
            let side = if col < j.using.len() {
                match j.merged_var.get(col)? {
                    Some(right) => Some(*right),
                    None => {
                        let ty = r.columns.get(col)?.1.clone();
                        return Some((String::new(), printed(r, col)?, ty));
                    }
                }
            } else {
                Some(l.is_none())
            };
            return match side {
                Some(false) => self.var_name(scope, j.left, l?, depth + 1),
                _ => self.var_name(scope, j.right, rr?, depth + 1),
            };
        }
        Some((
            r.refname.clone(),
            printed(r, col)?,
            r.columns.get(col)?.1.clone(),
        ))
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
                                | "&&"
                        ) {
                            return Some("bool".into());
                        }
                        if matches!(op.as_str(), "@>" | "<@") {
                            let l = self.typ(e.lexpr.as_deref()?)?;
                            if is_array(&l) || l == "jsonb" {
                                return Some("bool".into());
                            }
                        }
                        let r = self.typ(e.rexpr.as_deref()?)?;
                        let Some(l) = e.lexpr.as_deref() else {
                            return Some(r);
                        };
                        let l = self.typ(l)?;
                        if l == "jsonb" {
                            if let Some((_, result)) = jsonb_operator(&op, &r) {
                                return Some(result.into());
                            }
                        }
                        if l == "json" {
                            if let Some((_, result)) = json_operator(&op, &r) {
                                return Some(result.into());
                            }
                        }
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
                    | AExprKind::AexprOpAll
                    | AExprKind::AexprSimilar
                    | AExprKind::AexprBetweenSym
                    | AExprKind::AexprNotBetweenSym => "bool".into(),
                    AExprKind::AexprNullif => self.typ(e.lexpr.as_deref()?)?,
                    _ => return None,
                }
            }
            N::BoolExpr(_) | N::NullTest(_) | N::BooleanTest(_) => "bool".into(),
            N::FuncCall(f) => {
                let name = crate::func_name(f)?;
                if let Some(t) = variadic_any(&name) {
                    return Some(t.into());
                }
                let Some((_, result)) = function_sig(&name) else {
                    let types = f
                        .args
                        .iter()
                        .map(|a| self.typ(a))
                        .collect::<Option<Vec<String>>>()?;
                    return crate::funcsig::selected_result_type(&name, &types)
                        .or_else(|| crate::funcsig::resolved_result_type(&name, &types))
                        .or_else(|| {
                            crate::funcsig::session_reader_result_type(&name, types.len())
                        });
                };
                match result {
                    Some(r) => r.to_string(),
                    None => match name.as_str() {
                        "percentile_cont" => {
                            let sort = f.agg_order.first().and_then(|o| match o.node.as_ref() {
                                Some(N::SortBy(sb)) => sb.node.as_deref(),
                                _ => None,
                            })?;
                            let t = self.typ(sort)?;
                            let elem = if t == "interval" {
                                "interval"
                            } else {
                                "float8"
                            };
                            if is_array(&self.typ(f.args.first()?)?) {
                                format!("{elem}[]")
                            } else {
                                elem.into()
                            }
                        }
                        "percentile_disc" | "mode" => {
                            let sort = f.agg_order.first().and_then(|o| match o.node.as_ref() {
                                Some(N::SortBy(sb)) => sb.node.as_deref(),
                                _ => None,
                            })?;
                            let t = self.typ(sort)?;
                            if name == "percentile_disc" && is_array(&self.typ(f.args.first()?)?) {
                                format!("{t}[]")
                            } else {
                                t
                            }
                        }
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
            // `arr[i]`: the element type (a slice keeps the array's).
            N::AIndirection(ind) => {
                let t = self.typ(ind.arg.as_deref()?)?;
                let elem = t.strip_suffix("[]")?.to_string();
                let mut slice = false;
                for i in &ind.indirection {
                    match i.node.as_ref()? {
                        N::AIndices(x) => slice |= x.is_slice,
                        _ => return None,
                    }
                }
                if slice {
                    t
                } else {
                    elem
                }
            }
            N::RowExpr(_) => "record".into(),
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
                Some(o) => meet_types(&o, &t)?,
            });
        }
        Some(out.unwrap_or_else(|| "text".into()))
    }

    /// An arithmetic or string operator: `(left coerced to, right coerced
    /// to, result)`, empty meaning "as is".
    fn arith_result(op: &str, l: &str, r: &str) -> Option<(String, String, String)> {
        if op == "||" {
            // Arrays: `anyarray || anyarray`, `anyarray || anyelement`,
            // `anyelement || anyarray` -- the element types must agree.
            if is_array(l) || is_array(r) {
                let el = |t: &str| t.strip_suffix("[]").unwrap_or(t).to_string();
                if el(l) != el(r) {
                    return None;
                }
                let res = if is_array(l) { l } else { r };
                return Some((String::new(), String::new(), res.to_string()));
            }
            // `text || anynonarray` / `anynonarray || text`: the non-string
            // side is taken as it is (`textanycat`), a string one as text.
            let side = |t: &str, other: &str| -> Option<&'static str> {
                if t == "text" {
                    Some("")
                } else if is_stringish(t) {
                    Some("text")
                } else if is_stringish(other) && other != "unknown" {
                    Some("")
                } else {
                    None
                }
            };
            let lt = side(l, r)?;
            let rt = side(r, l)?;
            return Some((lt.into(), rt.into(), "text".into()));
        }
        if op == "^" {
            // `numeric ^ numeric` when either side is numeric, else float8.
            let meet = if (l == "numeric" || r == "numeric")
                && !l.starts_with("float")
                && !r.starts_with("float")
            {
                "numeric"
            } else {
                "float8"
            };
            if !is_numberish(l) || !is_numberish(r) {
                return None;
            }
            let lt = if l == meet { "" } else { meet };
            let rt = if r == meet { "" } else { meet };
            return Some((lt.into(), rt.into(), meet.into()));
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
                ("interval", "timestamptz" | "timestamp", "+") => {
                    Some((String::new(), String::new(), r.into()))
                }
                // date +/- interval is a timestamp (`date_pl_interval`).
                ("date", "interval", "+" | "-") | ("interval", "date", "+") => {
                    Some((String::new(), String::new(), "timestamp".into()))
                }
                ("interval", "interval", "+" | "-") => {
                    Some((String::new(), String::new(), "interval".into()))
                }
                // interval * / number: `interval_mul(interval, float8)`.
                ("interval", n, "*" | "/") if is_numberish(n) => {
                    let nt = if n == "float8" { "" } else { "float8" };
                    Some((String::new(), nt.into(), "interval".into()))
                }
                (n, "interval", "*") if is_numberish(n) => {
                    let nt = if n == "float8" { "" } else { "float8" };
                    Some((nt.into(), String::new(), "interval".into()))
                }
                // A difference of two instants is an interval, the
                // narrower side widened (date -> timestamp -> timestamptz).
                (l, r, "-")
                    if matches!(l, "date" | "timestamp" | "timestamptz")
                        && matches!(r, "date" | "timestamp" | "timestamptz")
                        && (l != "date" || r != "date") =>
                {
                    let rank = |t: &str| match t {
                        "date" => 0,
                        "timestamp" => 1,
                        _ => 2,
                    };
                    let wide = if rank(l) >= rank(r) { l } else { r };
                    let lt = if l == wide { "" } else { wide };
                    let rt = if r == wide { "" } else { wide };
                    Some((lt.into(), rt.into(), "interval".into()))
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
        // An untyped literal takes the other side's type when that type has
        // an `=` of its own (`binary_oper_exact`): `name = name` exists, so
        // a name stays a name; `varchar` has none and meets as text.
        if l == "unknown" {
            let t = match r {
                "varchar" => "text",
                other => other,
            };
            let rt = if t == r { "" } else { t };
            return Some((t.into(), rt.into()));
        }
        if r == "unknown" {
            let t = match l {
                "varchar" => "text",
                other => other,
            };
            let lt = if t == l { "" } else { t };
            return Some((lt.into(), t.into()));
        }
        // `oid` meets an integer through the implicit cast to `oid`
        // (`oideq`): the integer side is coerced, a constant folded into
        // an `oid` literal (`'1259'::oid`).
        if l == "oid" && is_int(r) {
            return Some((String::new(), "oid".into()));
        }
        if r == "oid" && is_int(l) {
            return Some(("oid".into(), String::new()));
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
                if self.fold_consts && want == "oid" {
                    if let Some(N::AConst(c)) = n.node.as_ref() {
                        if let Some(Val::Ival(i)) = c.val.as_ref() {
                            self.buf.push_str(&format!("'{}'::oid", i.ival));
                            return Some(());
                        }
                    }
                }
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
                | N::AIndirection(_)
                | N::TypeCast(_),
            ) => true,
            // A row comparison by an ordering operator is a RowCompareExpr,
            // which no parent takes bare.
            Some(N::AExpr(e))
                if matches!(
                    (
                        e.lexpr.as_deref().and_then(|x| x.node.as_ref()),
                        e.rexpr.as_deref().and_then(|x| x.node.as_ref())
                    ),
                    (Some(N::RowExpr(_)), Some(N::RowExpr(_)))
                ) && matches!(
                    crate::operator_name(e).unwrap_or(""),
                    "<" | "<=" | ">" | ">="
                ) =>
            {
                false
            }
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
                Ok(AExprKind::AexprDistinct) => group(parent),
                // A one-element IN is the plain operator.
                Ok(AExprKind::AexprIn)
                    if matches!(e.rexpr.as_deref().and_then(|r| r.node.as_ref()),
                        Some(N::List(l)) if l.items.len() == 1) =>
                {
                    group(parent)
                }
                Ok(AExprKind::AexprNotDistinct) => bool_simple(BoolExprType::NotExpr, parent),
                Ok(AExprKind::AexprSimilar) => group(parent),
                Ok(AExprKind::AexprBetweenSym) => bool_simple(BoolExprType::OrExpr, parent),
                Ok(AExprKind::AexprNotBetweenSym) => bool_simple(BoolExprType::AndExpr, parent),
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
                        // A date / time constant prints in its type's output
                        // form (`'2020-01-01 00:00:00'::timestamp ...`).
                        "timestamp" | "date" | "time" => {
                            let v = crate::cast_value(bson::Bson::String(lit.clone()), t).ok()?;
                            let text = crate::cast_value(v, "text").ok()?;
                            let bson::Bson::String(text) = text else {
                                return None;
                            };
                            self.buf.push_str(&format!(
                                "{}::{}",
                                crate::scalar::quote_literal(&text),
                                display_type(t)
                            ));
                        }
                        // An interval constant prints as interval's output.
                        "interval" => {
                            let v = crate::cast_value(bson::Bson::String(lit.clone()), "interval")
                                .ok()?;
                            self.buf.push_str(&format!(
                                "{}::interval",
                                crate::scalar::quote_literal(&crate::value_text(&v))
                            ));
                        }
                        // An array constant prints as `array_out` gives it
                        // (`'{ 1 }'` is `'{1}'::integer[]`).
                        _ if is_array(t) => {
                            let v = crate::cast_value(bson::Bson::String(lit.clone()), t).ok()?;
                            let text = crate::cast_value(v, "text").ok()?;
                            let bson::Bson::String(text) = text else {
                                return None;
                            };
                            self.buf.push_str(&format!(
                                "{}::{}",
                                crate::scalar::quote_literal(&text),
                                display_type(t)
                            ));
                        }
                        // A jsonb constant prints as jsonb's output form.
                        "jsonb" => {
                            let v = crate::json::parse(&lit).ok()?;
                            self.buf.push_str(&format!(
                                "{}::jsonb",
                                crate::scalar::quote_literal(&crate::json::render_jsonb(&v))
                            ));
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
                if self.unqualified || rel.is_empty() {
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
                // `eval_const_expressions` runs a cast of a constant and
                // leaves a Const of the target type, which `get_const_expr`
                // prints in that type's output form: `1::bigint` is the
                // constant `'1'::bigint`, not `(1)::bigint` (RP-370).
                if self.fold_consts && typmod < 0 {
                    if let Some(text) = folded_numeric_cast(arg, &target) {
                        match target.as_str() {
                            "int4" => self.const_number(&text, &target),
                            _ => self.buf.push_str(&format!(
                                "{}::{label}",
                                crate::scalar::quote_literal(&text)
                            )),
                        }
                        return Some(());
                    }
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
            N::AIndirection(ind) => {
                let arg = ind.arg.as_deref()?;
                self.typ(n)?;
                let bare = matches!(arg.node.as_ref(), Some(N::ColumnRef(_)));
                if !bare {
                    self.buf.push('(');
                }
                self.expr(arg)?;
                if !bare {
                    self.buf.push(')');
                }
                for i in &ind.indirection {
                    let Some(N::AIndices(x)) = i.node.as_ref() else {
                        return None;
                    };
                    self.buf.push('[');
                    if x.is_slice {
                        if let Some(lo) = x.lidx.as_deref() {
                            self.expr_as(lo, "int4")?;
                        }
                        self.buf.push(':');
                        if let Some(hi) = x.uidx.as_deref() {
                            self.expr_as(hi, "int4")?;
                        }
                    } else {
                        self.expr_as(x.uidx.as_deref()?, "int4")?;
                    }
                    self.buf.push(']');
                }
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
                    // A prefix operator: parenthesised unless pretty.
                    let open = !self.pretty;
                    if open {
                        self.buf.push('(');
                    }
                    self.buf.push_str(&format!("{op} "));
                    self.child(r, "", Parent::Op(&op, false))?;
                    if open {
                        self.buf.push(')');
                    }
                    return Some(());
                };
                // Two row constructors: `=` / `<>` compare field by field
                // (an AND / OR of the pairs), the ordering operators as a
                // row comparison printed `ROW(...) op ROW(...)`.
                if let (Some(N::RowExpr(lr)), Some(N::RowExpr(rr))) =
                    (l.node.as_ref(), r.node.as_ref())
                {
                    return self.row_compare(&op, &lr.args, &rr.args);
                }
                let (lt, rt) = (self.typ(l)?, self.typ(r)?);
                // Array containment / overlap: same element types, no cast.
                if matches!(op.as_str(), "@>" | "<@" | "&&") && is_array(&lt) && is_array(&rt) {
                    if lt != rt {
                        return None;
                    }
                    return self.binary(l, "", &op, r, "");
                }
                // An untyped literal against an array takes the array's
                // type (`anyarray` resolution), printed as its constant.
                if matches!(op.as_str(), "@>" | "<@" | "&&") {
                    if is_array(&lt) && rt == "unknown" {
                        return self.binary(l, "", &op, r, &lt);
                    }
                    if lt == "unknown" && is_array(&rt) {
                        return self.binary(l, &rt, &op, r, "");
                    }
                }
                let json_op = match lt.as_str() {
                    "jsonb" => jsonb_operator(&op, &rt),
                    "json" => json_operator(&op, &rt),
                    _ => None,
                };
                if let Some((want, _)) = json_op {
                    let rw = if rt == want { "" } else { want };
                    return self.binary(l, "", &op, r, rw);
                }
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
                // One element: the plain operator (`transformAExprIn`).
                if let [only] = list.items.as_slice() {
                    let (lw, rw) = Self::compare_coercion(&lt, &self.typ(only)?)?;
                    let word = if op == "=" { "=" } else { "<>" };
                    return self.binary(l, &lw, word, only, &rw);
                }
                let mut all = vec![l.clone()];
                all.extend(list.items.iter().cloned());
                let common = self.common_type(&all)?;
                // The integer comparison operators take mixed widths
                // (`int24eq`): an integer left side keeps its own type.
                let lw = if lt == common || (is_int(&lt) && is_int(&common)) {
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
                // `eval_const_expressions` folds an ARRAY of constants (and
                // varchar's coercion of it to text[]) to one array constant,
                // printed as its `array_out` text (RP-348).
                if self.fold_consts {
                    let elem = if via_text { "text" } else { common.as_str() };
                    if let Some(lit) = folded_array_literal(&list.items, elem) {
                        self.buf.push_str(&lit);
                        self.buf.push_str(&format!("::{}[]", display_type(elem)));
                        self.buf.push(')');
                        if open {
                            self.buf.push(')');
                        }
                        return Some(());
                    }
                }
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
                // IS NOT DISTINCT FROM is NOT over a DistinctExpr.
                let not = kind == AExprKind::AexprNotDistinct;
                if not {
                    self.buf.push_str(if open { "(NOT " } else { "NOT " });
                }
                if open {
                    self.buf.push('(');
                }
                self.child(l, &lw, Parent::Other)?;
                self.buf.push_str(" IS DISTINCT FROM ");
                self.child(r, &rw, Parent::Other)?;
                if open {
                    self.buf.push(')');
                }
                if not && open {
                    self.buf.push(')');
                }
            }
            AExprKind::AexprSimilar => {
                let op = crate::operator_name(e).ok()?.to_string();
                if !matches!(op.as_str(), "~" | "!~") {
                    return None;
                }
                let (l, r) = (e.lexpr.as_deref()?, e.rexpr.as_deref()?);
                let (lw, rw) = Self::string_match_coercion(&self.typ(l)?, &self.typ(r)?)?;
                self.binary(l, &lw, &op, r, &rw)?;
            }
            AExprKind::AexprBetweenSym | AExprKind::AexprNotBetweenSym => {
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
                let not = kind == AExprKind::AexprNotBetweenSym;
                let (a, b, inner, outer) = if not {
                    ("<", ">", " OR ", " AND ")
                } else {
                    (">=", "<=", " AND ", " OR ")
                };
                let open = !self.pretty;
                // In pretty form an OR inside the AND needs its parentheses.
                let group = open || not;
                if open {
                    self.buf.push('(');
                }
                for (k, (first, fw, second, sw)) in [
                    (lo, (&xl, &lw), hi, (&xh, &hw)),
                    (hi, (&xh, &hw), lo, (&xl, &lw)),
                ]
                .into_iter()
                .enumerate()
                {
                    if k > 0 {
                        self.buf.push_str(outer);
                    }
                    if group {
                        self.buf.push('(');
                    }
                    self.binary(x, fw.0, a, first, fw.1)?;
                    self.buf.push_str(inner);
                    self.binary(x, sw.0, b, second, sw.1)?;
                    if group {
                        self.buf.push(')');
                    }
                }
                if open {
                    self.buf.push(')');
                }
            }
            AExprKind::AexprOpAny | AExprKind::AexprOpAll => {
                let op = crate::operator_name(e).ok()?.to_string();
                if !matches!(op.as_str(), "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=") {
                    return None;
                }
                let (l, r) = (e.lexpr.as_deref()?, e.rexpr.as_deref()?);
                let lt = self.typ(l)?;
                let at = self.typ(r)?;
                let elem = at.strip_suffix("[]")?.to_string();
                let (lw, ew) = Self::compare_coercion(&lt, &elem)?;
                // A cast of the whole array is not modelled.
                if !ew.is_empty() {
                    return None;
                }
                let op = if op == "!=" { "<>".to_string() } else { op };
                let quant = if kind == AExprKind::AexprOpAny {
                    "ANY"
                } else {
                    "ALL"
                };
                let open = !self.pretty;
                if open {
                    self.buf.push('(');
                }
                self.child(l, &lw, Parent::Other)?;
                self.buf.push_str(&format!(" {op} {quant} ("));
                self.expr(r)?;
                self.buf.push(')');
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

    /// `ROW(a, b) op ROW(c, d)`: `=` is the AND of the pairs' equalities,
    /// `<>` the OR of their inequalities (the parser expands both), and an
    /// ordering operator stays a row comparison.
    fn row_compare(&mut self, op: &str, l: &[Node], r: &[Node]) -> Option<()> {
        if l.len() != r.len() || l.is_empty() {
            return None;
        }
        let mut coercions = Vec::new();
        for (a, b) in l.iter().zip(r) {
            coercions.push(Self::compare_coercion(&self.typ(a)?, &self.typ(b)?)?);
        }
        let open = !self.pretty;
        match op {
            "=" | "<>" | "!=" => {
                let (pair_op, join) = if op == "=" {
                    ("=", " AND ")
                } else {
                    ("<>", " OR ")
                };
                if l.len() == 1 {
                    return self.binary(&l[0], &coercions[0].0, pair_op, &r[0], &coercions[0].1);
                }
                if open {
                    self.buf.push('(');
                }
                for (i, ((a, b), (aw, bw))) in l.iter().zip(r).zip(&coercions).enumerate() {
                    if i > 0 {
                        self.buf.push_str(join);
                    }
                    self.binary(a, aw, pair_op, b, bw)?;
                }
                if open {
                    self.buf.push(')');
                }
            }
            "<" | "<=" | ">" | ">=" => {
                // A RowCompareExpr prints its own parentheses, pretty or not.
                self.buf.push('(');
                for (side, (items, first)) in [(l, true), (r, false)].into_iter().enumerate() {
                    if side == 1 {
                        self.buf.push_str(&format!(" {op} "));
                    }
                    self.buf.push_str("ROW(");
                    for (i, (x, (aw, bw))) in items.iter().zip(&coercions).enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        self.expr_as(x, if first { aw } else { bw })?;
                    }
                    self.buf.push(')');
                }
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
        if f.func_variadic {
            return None;
        }
        let parts = names(&f.funcname);
        let name = parts.last()?.clone();
        if parts.len() > 1 && parts[0] != "pg_catalog" {
            return None;
        }
        let sql_syntax = f.funcformat == pg_query::protobuf::CoercionForm::CoerceSqlSyntax as i32;
        // `SUBSTRING(x FROM a FOR b)` and `TRIM(BOTH c FROM x)`: SQL syntax
        // over `substring(text, int, int)` / `btrim(text, text)`, printed as
        // the syntax (`get_func_sql_syntax`).
        if sql_syntax && f.over.is_none() && f.agg_filter.is_none() && f.agg_order.is_empty() {
            match (name.as_str(), f.args.as_slice()) {
                ("substring", [x, from, rest @ ..]) if rest.len() <= 1 => {
                    let ints = [from]
                        .into_iter()
                        .chain(rest.iter())
                        .all(|a| self.typ(a).is_some_and(|t| t == "int4"));
                    if !ints || !is_stringish(&self.typ(x)?) {
                        return None;
                    }
                    self.buf.push_str("SUBSTRING(");
                    self.expr_as(x, "text")?;
                    self.buf.push_str(" FROM ");
                    self.expr(from)?;
                    if let [len] = rest {
                        self.buf.push_str(" FOR ");
                        self.expr(len)?;
                    }
                    self.buf.push(')');
                    return Some(());
                }
                // `ts AT TIME ZONE zone`: `timezone(zone, ts)`, always
                // parenthesised.
                ("timezone", [zone, ts]) => {
                    let tt = self.typ(ts)?;
                    if !matches!(tt.as_str(), "timestamp" | "timestamptz" | "timetz") {
                        return None;
                    }
                    let zt = self.typ(zone)?;
                    let zw = if zt == "interval" { "" } else { "text" };
                    if !(zt == "interval" || is_stringish(&zt)) {
                        return None;
                    }
                    self.buf.push('(');
                    self.child(ts, "", Parent::Other)?;
                    self.buf.push_str(" AT TIME ZONE ");
                    self.child(zone, zw, Parent::Other)?;
                    self.buf.push(')');
                    return Some(());
                }
                ("position", [string, sub]) => {
                    if !is_stringish(&self.typ(string)?) || !is_stringish(&self.typ(sub)?) {
                        return None;
                    }
                    self.buf.push_str("POSITION((");
                    self.expr_as(sub, "text")?;
                    self.buf.push_str(") IN (");
                    self.expr_as(string, "text")?;
                    self.buf.push_str("))");
                    return Some(());
                }
                ("overlay", [x, y, from, rest @ ..]) if rest.len() <= 1 => {
                    let ints = [from]
                        .into_iter()
                        .chain(rest.iter())
                        .all(|a| self.typ(a).is_some_and(|t| t == "int4"));
                    if !ints || !is_stringish(&self.typ(x)?) || !is_stringish(&self.typ(y)?) {
                        return None;
                    }
                    self.buf.push_str("OVERLAY(");
                    self.expr_as(x, "text")?;
                    self.buf.push_str(" PLACING ");
                    self.expr_as(y, "text")?;
                    self.buf.push_str(" FROM ");
                    self.expr(from)?;
                    if let [len] = rest {
                        self.buf.push_str(" FOR ");
                        self.expr(len)?;
                    }
                    self.buf.push(')');
                    return Some(());
                }
                ("btrim" | "ltrim" | "rtrim", [x, chars @ ..]) if chars.len() <= 1 => {
                    if !is_stringish(&self.typ(x)?) {
                        return None;
                    }
                    let side = match name.as_str() {
                        "btrim" => "BOTH",
                        "ltrim" => "LEADING",
                        _ => "TRAILING",
                    };
                    self.buf.push_str(&format!("TRIM({side}"));
                    if let [c] = chars {
                        if !is_stringish(&self.typ(c)?) {
                            return None;
                        }
                        self.buf.push(' ');
                        self.expr_as(c, "text")?;
                    }
                    self.buf.push_str(" FROM ");
                    self.expr_as(x, "text")?;
                    self.buf.push(')');
                    return Some(());
                }
                _ => {}
            }
        }
        if variadic_any(&name).is_some()
            && f.over.is_none()
            && f.agg_order.is_empty()
            && f.agg_filter.is_none()
            && !f.agg_star
        {
            self.buf.push_str(&q(&name));
            self.buf.push('(');
            for (i, a) in f.args.iter().enumerate() {
                if i > 0 {
                    self.buf.push_str(", ");
                }
                match a.node.as_ref() {
                    Some(N::AConst(c)) if matches!(c.val, Some(Val::Sval(_)) | None) => {
                        match c.val.as_ref() {
                            Some(Val::Sval(sv)) => {
                                self.buf.push_str(&crate::scalar::quote_literal(&sv.sval))
                            }
                            _ => self.buf.push_str("NULL"),
                        }
                    }
                    _ => {
                        self.typ(a)?;
                        self.expr(a)?;
                    }
                }
            }
            self.buf.push(')');
            return Some(());
        }
        // `percentile_cont(0.5) WITHIN GROUP (ORDER BY x)`: the fraction is
        // float8 (or float8[]), and percentile_cont orders by float8 or
        // interval.
        if f.agg_within_group {
            if !matches!(
                name.as_str(),
                "percentile_cont" | "percentile_disc" | "mode"
            ) || f.over.is_some()
            {
                return None;
            }
            let [sort] = f.agg_order.as_slice() else {
                return None;
            };
            let Some(N::SortBy(sb)) = sort.node.as_ref() else {
                return None;
            };
            let st = self.typ(sb.node.as_deref()?)?;
            if name == "percentile_cont" && !matches!(st.as_str(), "float8" | "interval") {
                return None;
            }
            self.buf.push_str(&q(&name));
            self.buf.push('(');
            for (i, a) in f.args.iter().enumerate() {
                if i > 0 {
                    self.buf.push_str(", ");
                }
                let want = if is_array(&self.typ(a)?) {
                    "float8[]"
                } else {
                    "float8"
                };
                self.expr_as(a, want)?;
            }
            self.buf.push_str(") WITHIN GROUP (ORDER BY ");
            self.sort_list(&f.agg_order)?;
            self.buf.push(')');
            if let Some(filter) = f.agg_filter.as_deref() {
                self.buf.push_str(" FILTER (WHERE ");
                self.expr(filter)?;
                self.buf.push(')');
            }
            return Some(());
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
        if !f.agg_order.is_empty() {
            self.buf.push_str(" ORDER BY ");
            self.sort_list(&f.agg_order)?;
        }
        self.buf.push(')');
        if let Some(filter) = f.agg_filter.as_deref() {
            self.buf.push_str(" FILTER (WHERE ");
            self.expr(filter)?;
            self.buf.push(')');
        }
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
        if f.agg_star
            || f.agg_distinct
            || f.over.is_some()
            || !f.agg_order.is_empty()
            || f.agg_filter.is_some()
        {
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
            if d.len() != types.len() || d.iter().any(|t| t == "internal") {
                return None;
            }
            // A polymorphic parameter takes its argument as it is (an
            // untyped literal would need the polymorphic resolution).
            let mut out = Vec::with_capacity(d.len());
            for (want, have) in d.iter().zip(&types) {
                if want == "any" && have == "unknown" {
                    // `"any"` takes an untyped literal as it is: printed
                    // bare (`jsonb_build_object('a', x)`).
                    out.push("\u{0}bare".into());
                } else if want.starts_with("any") {
                    let fits = match want.as_str() {
                        "anyarray" | "anycompatiblearray" => is_array(have),
                        "anynonarray" | "anycompatiblenonarray" => !is_array(have),
                        _ => true,
                    };
                    if have == "unknown" || !fits {
                        return None;
                    }
                    out.push(String::new());
                } else {
                    out.push(want.clone());
                }
            }
            out
        };
        self.buf.push_str(&q(name));
        self.buf.push('(');
        for (i, a) in f.args.iter().enumerate() {
            if i > 0 {
                self.buf.push_str(", ");
            }
            if declared[i] == "\u{0}bare" {
                let Some(N::AConst(c)) = a.node.as_ref() else {
                    return None;
                };
                match c.val.as_ref() {
                    Some(Val::Sval(sv)) => {
                        self.buf.push_str(&crate::scalar::quote_literal(&sv.sval))
                    }
                    None => self.buf.push_str("NULL"),
                    _ => return None,
                }
                continue;
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
            kind @ (SubLinkType::AnySublink | SubLinkType::AllSublink) => {
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
                if kind == SubLinkType::AllSublink {
                    self.buf.push_str(&format!(" {op} ALL ("));
                } else if op == "=" && s.oper_name.is_empty() {
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
        inner.written_alias = std::rc::Rc::clone(&self.written_alias);
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
        inner.written_alias = std::rc::Rc::clone(&self.written_alias);
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
        inner.written_alias = std::rc::Rc::clone(&self.written_alias);
        inner.ctes = self.ctes.clone();
        inner.depth = self.depth + 1;
        inner.pretty = self.pretty;
        inner.markers = self.markers;
        inner.query_columns(sub)
    }

    /// `get_rule_sortgroupclause`: a function call, aggregate or window
    /// function is parenthesised.
    fn sort_expr(&mut self, n: &Node) -> Option<()> {
        // A Var, or a Const (a set operation's output position), is never
        // parenthesised (`get_rule_sortgroupclause`).
        let is_var = matches!(n.node.as_ref(), Some(N::ColumnRef(_) | N::AConst(_)));
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
pub(crate) fn base_type(t: &str) -> String {
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
    /// The FROM items' relations, each as ruleutils names it: the level's
    /// range table (`set_rtable_names`) and, outside EXPLAIN's helpers, its
    /// columns' names (`set_deparse_for_query`).
    fn collect_rtes(&mut self, items: &[Node], out: &mut Vec<Rte>) -> Option<()> {
        let mut tops = Vec::new();
        for it in items {
            tops.push(self.collect_item(it, out)?);
        }
        if !self.legacy_joins {
            name_columns(out, &tops)?;
        }
        Some(())
    }

    /// One FROM item's entries, in range-table order; the index of its
    /// top entry (`None` for a legacy JOIN, which has none).
    fn collect_item(&mut self, it: &Node, out: &mut Vec<Rte>) -> Option<usize> {
        match it.node.as_ref()? {
            N::RangeVar(rv) => {
                if !rv.schemaname.is_empty() && rv.schemaname != "public" {
                    return None;
                }
                let alias = rv.alias.as_ref();
                let is_cte = self.ctes.iter().any(|(n, _)| *n == rv.relname);
                let mut columns = self.relation_columns(&rv.relname)?;
                let real: Vec<String> = columns.iter().map(|c| c.0.clone()).collect();
                let typmods = match (is_cte, (self.cat.lookup)(&rv.relname)) {
                    (false, Some(def)) if def.columns.len() == columns.len() => {
                        def.columns.iter().map(|c| Some(c.typmod)).collect()
                    }
                    _ => unknown_typmods(&columns),
                };
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
                    kind: if is_cte {
                        RteKind::Other
                    } else {
                        RteKind::Relation
                    },
                    real: if is_cte { Vec::new() } else { real },
                    typmods,
                    alias_cols: !renames.is_empty(),
                    ..Default::default()
                });
            }
            N::JoinExpr(j) if self.legacy_joins => {
                if j.alias.is_some() {
                    return None;
                }
                self.collect_item(j.larg.as_deref()?, out)?;
                self.collect_item(j.rarg.as_deref()?, out)?;
            }
            N::JoinExpr(j) => return self.collect_join(j, out),
            N::RangeFunction(rf) => {
                let (call, name, column, ty) = self.function_item(rf)?;
                let _ = call;
                let refname = self.unique_refname(&name, out);
                let columns = vec![(column, ty)];
                out.push(Rte {
                    refname,
                    name,
                    typmods: unknown_typmods(&columns),
                    columns,
                    kind: RteKind::Function,
                    ..Default::default()
                });
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
                    typmods: unknown_typmods(&columns),
                    columns,
                    kind: RteKind::Subquery,
                    alias_cols: !renames.is_empty(),
                    ..Default::default()
                });
            }
            _ => return None,
        }
        Some(out.len() - 1)
    }

    /// A JOIN's entry after its inputs' (`transformFromClauseItem`): its
    /// columns the merged USING (or NATURAL) ones first, then the left
    /// input's others, then the right's; an alias hides the inputs, and
    /// without one their columns are reached through the JOIN only.
    fn collect_join(
        &mut self,
        j: &pg_query::protobuf::JoinExpr,
        out: &mut Vec<Rte>,
    ) -> Option<usize> {
        let start = out.len();
        let left = self.collect_item(j.larg.as_deref()?, out)?;
        let right = self.collect_item(j.rarg.as_deref()?, out)?;
        let jointype = JoinType::try_from(j.jointype).ok()?;
        if !matches!(
            jointype,
            JoinType::JoinInner | JoinType::JoinLeft | JoinType::JoinRight | JoinType::JoinFull
        ) {
            return None;
        }
        let (lr, rr) = (&out[left], &out[right]);
        let using: Vec<String> = if j.is_natural {
            lr.columns
                .iter()
                .map(|c| c.0.clone())
                .filter(|n| rr.columns.iter().any(|c| c.0 == *n))
                .collect()
        } else {
            names(&j.using_clause)
        };
        // A USING column is named once on each side.
        let once = |r: &Rte, n: &str| -> Option<usize> {
            let mut hits = r.columns.iter().enumerate().filter(|(_, c)| c.0 == n);
            match (hits.next(), hits.next()) {
                (Some((i, _)), None) => Some(i),
                _ => None,
            }
        };
        let mut columns = Vec::new();
        let mut typmods = Vec::new();
        let mut sources = Vec::new();
        let mut merged_var = Vec::new();
        let (mut lmerged, mut rmerged) = (Vec::new(), Vec::new());
        for u in &using {
            let (li, ri) = (once(lr, u)?, once(rr, u)?);
            lmerged.push(li);
            rmerged.push(ri);
            let (lt, rt) = (&lr.columns[li].1, &rr.columns[ri].1);
            let (lm, rm) = (lr.typmods.get(li).copied()?, rr.typmods.get(ri).copied()?);
            // `select_common_type` and `select_common_typmod`.
            let ty = meet_types(lt, rt)?;
            let typmod = if *lt != ty || *rt != ty {
                Some(-1)
            } else {
                match (lm, rm) {
                    (Some(a), Some(b)) => Some(if a == b { a } else { -1 }),
                    _ => None,
                }
            };
            // Each side's column is the join's own as it is, or after a
            // coercion (`coerce_type`, or a RelabelType for the modifier).
            let plain = |t: &str, m: Option<i32>| -> Option<bool> {
                if t != ty {
                    return Some(false);
                }
                match (m, typmod) {
                    (Some(a), Some(b)) => Some(a == b),
                    _ => None,
                }
            };
            let (lp, rp) = (plain(lt, lm), plain(rt, rm));
            merged_var.push(match jointype {
                JoinType::JoinInner => match (lp?, rp?) {
                    (true, _) => Some(false),
                    (false, true) => Some(true),
                    _ => None,
                },
                JoinType::JoinLeft => lp?.then_some(false),
                JoinType::JoinRight => rp?.then_some(true),
                _ => None,
            });
            columns.push((u.clone(), ty));
            typmods.push(typmod);
            sources.push((Some(li), Some(ri)));
        }
        for (side, r, merged) in [(false, lr, &lmerged), (true, rr, &rmerged)] {
            for (i, c) in r.columns.iter().enumerate() {
                if merged.contains(&i) {
                    continue;
                }
                columns.push(c.clone());
                typmods.push(r.typmods.get(i).copied()?);
                sources.push(if side {
                    (None, Some(i))
                } else {
                    (Some(i), None)
                });
            }
        }
        let alias_cols = j
            .alias
            .as_ref()
            .map(|a| names(&a.colnames))
            .unwrap_or_default();
        if alias_cols.len() > columns.len() {
            return None;
        }
        for (c, n) in columns.iter_mut().zip(&alias_cols) {
            c.0 = n.clone();
        }
        let named = j.alias.is_some();
        let idx = out.len() + usize::from(j.join_using_alias.is_some());
        // The inputs leave the namespace under an alias, and keep only
        // their names without one.
        for r in &mut out[start..] {
            if r.hidden_by.is_some() {
                continue;
            }
            if named {
                r.hidden_by = Some(idx);
            } else if r.cols_hidden_by.is_none() {
                r.cols_hidden_by = Some(idx);
            }
        }
        if let Some(ua) = j.join_using_alias.as_ref() {
            out.push(Rte {
                name: ua.aliasname.clone(),
                columns: columns[..using.len()].to_vec(),
                typmods: typmods[..using.len()].to_vec(),
                qualified_only: true,
                kind: RteKind::Join,
                using_alias_of: Some(idx),
                ..Default::default()
            });
        }
        let (name, refname) = match j.alias.as_ref() {
            Some(a) => (a.aliasname.clone(), self.unique_refname(&a.aliasname, out)),
            None => (String::new(), String::new()),
        };
        out.push(Rte {
            refname,
            name,
            columns,
            typmods,
            kind: RteKind::Join,
            alias_cols: !alias_cols.is_empty(),
            join: Some(Box::new(JoinRte {
                start,
                left,
                right,
                sources,
                using,
                merged_var,
                named,
                alias_cols,
                using_names: Vec::new(),
            })),
            ..Default::default()
        });
        Some(idx)
    }
    /// A FROM item that is one plain call of a function returning a scalar
    /// (`generate_series(1, 10) g`): (the call, the name the query uses
    /// for it, its one column's name and type).
    fn function_item(
        &mut self,
        rf: &pg_query::protobuf::RangeFunction,
    ) -> Option<(Node, String, String, String)> {
        if rf.lateral || rf.ordinality || rf.is_rowsfrom || !rf.coldeflist.is_empty() {
            return None;
        }
        let [item] = rf.functions.as_slice() else {
            return None;
        };
        let Some(N::List(l)) = item.node.as_ref() else {
            return None;
        };
        let call = l.items.first()?.clone();
        if l.items.get(1).is_some_and(|c| {
            !matches!(c.node.as_ref(), Some(N::List(x)) if x.items.is_empty()) && c.node.is_some()
        }) {
            return None;
        }
        let Some(N::FuncCall(f)) = call.node.as_ref() else {
            return None;
        };
        let fname = crate::func_name(f)?;
        if f.agg_star || f.over.is_some() || !f.agg_order.is_empty() {
            return None;
        }
        let args = f
            .args
            .iter()
            .map(|a| self.typ(a))
            .collect::<Option<Vec<String>>>()?;
        // The set-returning built-ins `pg_proc`'s signature table leaves
        // out, for the argument types that need no coercion.
        let ty = match (
            fname.as_str(),
            args.iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice(),
        ) {
            ("generate_series", [a, b] | [a, b, _])
                if a == b && matches!(*a, "int4" | "int8" | "numeric") =>
            {
                if args.len() == 3 && args[2] != args[0] {
                    return None;
                }
                a.to_string()
            }
            ("unnest", [a]) if is_array(a) => a.trim_end_matches("[]").to_string(),
            ("generate_subscripts", [a, "int4"]) if is_array(a) => "int4".to_string(),
            ("regexp_split_to_table", ["text", "text"]) => "text".to_string(),
            _ => self.typ(&call)?,
        };
        if ty == "record" || crate::user_composite(&ty).is_some() || ty.is_empty() {
            return None;
        }
        let (name, column) = match rf.alias.as_ref() {
            Some(a) => {
                let cols = names(&a.colnames);
                if cols.len() > 1 {
                    return None;
                }
                (
                    a.aliasname.clone(),
                    cols.first().cloned().unwrap_or_else(|| a.aliasname.clone()),
                )
            }
            None => (fname.clone(), fname),
        };
        Some((call, name, column, ty))
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
                    // `ExpandAllTables` over the namespace items whose
                    // columns are visible, or the one item `rel.*` names.
                    let qual = names(&c.fields);
                    let scope = self.scopes.len().checked_sub(1)?;
                    for (ri, r) in rtes.iter().enumerate() {
                        let shown = if qual.is_empty() {
                            !r.qualified_only && r.hidden_by.is_none() && r.cols_hidden_by.is_none()
                        } else {
                            r.hidden_by.is_none()
                                && !r.name.is_empty()
                                && qual.last() == Some(&r.name)
                        };
                        if !shown {
                            continue;
                        }
                        for (ci, (col, _)) in r.columns.iter().enumerate() {
                            out.push(Target {
                                expr: var_ref(scope, ri, ci),
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
            inner.written_alias = std::rc::Rc::clone(&self.written_alias);
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
            inner.written_alias = std::rc::Rc::clone(&self.written_alias);
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
            // `get_target_list`: no AS where the column printed under its
            // output name.
            let plain_var = match t.expr.node.as_ref() {
                Some(N::ColumnRef(c)) => self
                    .resolve(&names(&c.fields))
                    .is_some_and(|(_, col, _)| col == name),
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
        let mut at = 0;
        for (i, it) in s.from_clause.iter().enumerate() {
            if i == 0 {
                self.keyword(" FROM ", -PRETTYINDENT_STD, PRETTYINDENT_STD, 2);
                self.print_from_item(it, &mut at)?;
            } else {
                self.buf.push_str(", ");
                let saved = std::mem::take(&mut self.buf);
                self.print_from_item(it, &mut at)?;
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
                if matches!(g.node.as_ref(), Some(N::GroupingSet(_))) {
                    self.grouping_set(g, &targets, true)?;
                    continue;
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

    /// `get_rule_groupingset`: GROUPING SETS / ROLLUP / CUBE and their
    /// member sets, an element printed as a GROUP BY item.
    fn grouping_set(&mut self, n: &Node, targets: &[Target], omit_parens: bool) -> Option<()> {
        use pg_query::protobuf::GroupingSetKind as K;
        let members: Vec<Node> = match n.node.as_ref()? {
            N::GroupingSet(gs) => {
                let (word, omit_child) = match K::try_from(gs.kind).ok()? {
                    K::GroupingSetEmpty => {
                        self.buf.push_str("()");
                        return Some(());
                    }
                    K::GroupingSetRollup => ("ROLLUP(", true),
                    K::GroupingSetCube => ("CUBE(", true),
                    K::GroupingSetSets => ("GROUPING SETS (", false),
                    _ => return None,
                };
                self.buf.push_str(word);
                for (i, m) in gs.content.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.grouping_set(m, targets, omit_child)?;
                }
                self.buf.push(')');
                return Some(());
            }
            // A parenthesised list of expressions is one simple set.
            N::RowExpr(r) => r.args.clone(),
            _ => vec![n.clone()],
        };
        let parens = !omit_parens || members.len() != 1;
        if parens {
            self.buf.push('(');
        }
        for (i, m) in members.iter().enumerate() {
            if i > 0 {
                self.buf.push_str(", ");
            }
            let e = self.group_target(m, targets)?;
            self.sort_expr(&e)?;
        }
        if parens {
            self.buf.push(')');
        }
        Some(())
    }

    /// This level's relation the query wrote as `name`.
    /// `(a, b, c)` after an item's alias.
    fn column_alias_list(&mut self, rte: &Rte) {
        let cols: Vec<String> = if rte.printed.is_empty() {
            rte.columns.iter().map(|(n, _)| q(n)).collect()
        } else {
            rte.printed.iter().map(|n| q(n)).collect()
        };
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
    /// `get_from_clause_item`: one FROM item, its entries read from the
    /// level in range-table order from `*at`.
    fn print_from_item(&mut self, it: &Node, at: &mut usize) -> Option<()> {
        let entry = |p: &Self, i: usize| p.scopes.last()?.get(i).cloned();
        match it.node.as_ref()? {
            N::RangeVar(rv) => {
                let rte = entry(self, *at)?;
                *at += 1;
                // A table of another schema is stored `schema.name`: printed
                // qualified while its schema is off the search path (as
                // `generate_relation_name` does), its alias compared with
                // the bare name. The session's own temporary schema is
                // searched first whether or not the path lists it.
                let bare = match rv.relname.split_once('.') {
                    Some((schema, bare)) => {
                        let on_path = crate::schemas::temp_schema().as_deref() == Some(schema)
                            || crate::schemas::search_path().iter().any(|s| s == schema);
                        if !on_path {
                            self.buf.push_str(&q(schema));
                            self.buf.push('.');
                        }
                        bare.to_string()
                    }
                    // A `public` table, likewise, while `public` is off the
                    // path (pg_dump sets it empty); `pg_catalog`'s are
                    // always found.
                    None => {
                        if matches!(rv.schemaname.as_str(), "" | "public")
                            && !crate::is_pg_catalog_relation(&rv.relname)
                            && !crate::schemas::search_path().iter().any(|s| s == "public")
                        {
                            self.buf.push_str("public.");
                        }
                        rv.relname.clone()
                    }
                };
                self.buf.push_str(&q(&bare));
                // `get_rte_alias`: a written alias, an alias the column
                // aliases need, or a name `set_rtable_names` changed.
                let shown = self.alias_written(rv)
                    || rte.print_aliases
                    || if rv.relname.contains('.') {
                        rte.refname != bare
                    } else {
                        rte.refname != rv.relname
                    };
                if shown {
                    self.buf.push(' ');
                    self.buf.push_str(&q(&rte.refname));
                }
                if rte.print_aliases {
                    self.column_alias_list(&rte);
                }
            }
            N::RangeFunction(rf) => {
                let rte = entry(self, *at)?;
                *at += 1;
                let (call, _, _, _) = self.function_item(rf)?;
                self.expr(&call)?;
                self.buf.push(' ');
                self.buf.push_str(&q(&rte.refname));
                self.column_alias_list(&rte);
            }
            N::RangeSubselect(rs) => {
                let rte = entry(self, *at)?;
                *at += 1;
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
                self.buf.push_str(&q(&rte.refname));
                if rte.print_aliases {
                    self.column_alias_list(&rte);
                }
            }
            N::JoinExpr(j) => {
                let open = !self.pretty || j.alias.is_some();
                let rarg = j.rarg.as_deref()?;
                let need_paren_on_right = self.pretty
                    && match rarg.node.as_ref()? {
                        N::JoinExpr(r) => r.alias.is_none(),
                        _ => false,
                    };
                if open {
                    self.buf.push('(');
                }
                self.print_from_item(j.larg.as_deref()?, at)?;
                // The JOIN's own entry follows both inputs' (and the USING
                // alias's, where there is one).
                let join_idx =
                    *at + from_item_entries(rarg)? + usize::from(j.join_using_alias.is_some());
                let rte = entry(self, join_idx)?;
                let info = rte.join.as_deref()?;
                let jt = JoinType::try_from(j.jointype).ok()?;
                // A USING (or NATURAL) join has quals once analysed.
                let has_quals = j.quals.is_some() || !info.using_names.is_empty();
                let word = match jt {
                    JoinType::JoinInner if has_quals => " JOIN ",
                    JoinType::JoinInner => " CROSS JOIN ",
                    JoinType::JoinLeft => " LEFT JOIN ",
                    JoinType::JoinFull => " FULL JOIN ",
                    JoinType::JoinRight => " RIGHT JOIN ",
                    _ => return None,
                };
                self.keyword(word, -PRETTYINDENT_STD, PRETTYINDENT_STD, PRETTYINDENT_JOIN);
                if need_paren_on_right {
                    self.buf.push('(');
                }
                self.print_from_item(rarg, at)?;
                if need_paren_on_right {
                    self.buf.push(')');
                }
                *at = join_idx + 1;
                if !info.using_names.is_empty() {
                    let cols: Vec<String> = info.using_names.iter().map(|c| q(c)).collect();
                    self.buf.push_str(&format!(" USING ({})", cols.join(", ")));
                    if let Some(ua) = j.join_using_alias.as_ref() {
                        self.buf.push_str(&format!(" AS {}", q(&ua.aliasname)));
                    }
                } else if let Some(qual) = j.quals.as_deref() {
                    self.buf.push_str(" ON ");
                    if !self.pretty {
                        self.buf.push('(');
                    }
                    // The ON clause sees the JOIN's inputs only, as they
                    // stand inside it.
                    let level = self.scopes.pop()?;
                    let mut inside = level.clone();
                    for (i, r) in inside.iter_mut().enumerate() {
                        let within =
                            |h: Option<usize>| h.is_some_and(|h| h >= info.start && h < join_idx);
                        if i < info.start || i >= join_idx {
                            r.hidden_by = Some(usize::MAX);
                            continue;
                        }
                        if !within(r.hidden_by) {
                            r.hidden_by = None;
                        }
                        if !within(r.cols_hidden_by) {
                            r.cols_hidden_by = None;
                        }
                    }
                    self.scopes.push(inside);
                    let printed = self.expr(qual);
                    self.scopes.pop();
                    self.scopes.push(level);
                    printed?;
                    if !self.pretty {
                        self.buf.push(')');
                    }
                } else if jt != JoinType::JoinInner {
                    // Without a CROSS JOIN, an ON is needed.
                    self.buf.push_str(" ON TRUE");
                }
                if open {
                    self.buf.push(')');
                }
                if j.alias.is_some() {
                    self.buf.push(' ');
                    self.buf.push_str(&q(&rte.refname));
                    if rte.print_aliases {
                        self.column_alias_list(&rte);
                    }
                }
            }
            _ => return None,
        }
        Some(())
    }
}

/// How many range-table entries a FROM item makes: one for a relation,
/// a subquery or a function, and a JOIN's inputs' plus its own (and its
/// USING alias's).
fn from_item_entries(n: &Node) -> Option<usize> {
    Some(match n.node.as_ref()? {
        N::JoinExpr(j) => {
            from_item_entries(j.larg.as_deref()?)?
                + from_item_entries(j.rarg.as_deref()?)?
                + usize::from(j.join_using_alias.is_some())
                + 1
        }
        _ => 1,
    })
}

/// A column's modifier where the source does not say: none for a type that
/// takes none, unknown for one that does.
fn unknown_typmods(columns: &[(String, String)]) -> Vec<Option<i32>> {
    columns
        .iter()
        .map(|(_, t)| (!has_typmod(t)).then_some(-1))
        .collect()
}

/// `select_common_type` over two column types, as [`Printer::common_type`]
/// resolves them.
fn meet_types(o: &str, t: &str) -> Option<String> {
    if o == t {
        return Some(o.to_string());
    }
    if let Some((meet, _)) = numeric_meet(o, t) {
        if !meet.is_empty() {
            return Some(meet.to_string());
        }
        // Integers meet at the widest.
        let w = |x: &str| match x {
            "int2" => 1,
            "int4" => 2,
            _ => 3,
        };
        return Some(if w(o) >= w(t) { o } else { t }.to_string());
    }
    (is_stringish(o) && is_stringish(t)).then(|| "text".to_string())
}

/// `set_deparse_for_query`'s column naming over one level: the USING
/// columns' names first, down the join tree (`set_using_names`), then every
/// entry's in range-table order (`set_relation_column_names`,
/// `set_join_column_names`), each made unique within its entry, against
/// its enclosing joins' USING names and, when an unnamed JOIN's merged
/// column is not a plain column (`has_dangerous_join_using`), against
/// every USING name of the level.
fn name_columns(level: &mut [Rte], tops: &[usize]) -> Option<()> {
    let unique_using = level.iter().any(|r| {
        r.join
            .as_deref()
            .is_some_and(|j| !j.named && j.merged_var.iter().any(Option::is_none))
    });
    let mut st = Naming {
        colnames: level.iter().map(|r| vec![None; r.columns.len()]).collect(),
        parent_using: vec![Vec::new(); level.len()],
        using_names: vec![Vec::new(); level.len()],
        global: Vec::new(),
        unique_using,
    };
    for &t in tops {
        st.set_using_names(level, t, &[])?;
    }
    for i in 0..level.len() {
        if level[i].using_alias_of.is_some() {
            continue;
        }
        let print_aliases = if level[i].join.is_some() {
            st.set_join_column_names(level, i)?
        } else {
            st.set_relation_column_names(level, i)
        };
        let r = &mut level[i];
        r.printed = st.colnames[i]
            .iter()
            .zip(&r.columns)
            .map(|(p, c)| p.clone().unwrap_or_else(|| c.0.clone()))
            .collect();
        r.print_aliases = print_aliases;
        if let Some(j) = r.join.as_deref_mut() {
            j.using_names = std::mem::take(&mut st.using_names[i]);
        }
    }
    Some(())
}

/// The `deparse_columns` being filled for one level.
struct Naming {
    colnames: Vec<Vec<Option<String>>>,
    parent_using: Vec<Vec<String>>,
    using_names: Vec<Vec<String>>,
    /// `dpns->using_names`.
    global: Vec<String>,
    unique_using: bool,
}

impl Naming {
    fn push_down(&mut self, idx: usize, col: usize, name: &str, level: &[Rte]) -> Option<()> {
        let j = level[idx].join.as_deref()?;
        let (l, r) = *j.sources.get(col)?;
        if let Some(c) = l {
            *self.colnames[j.left].get_mut(c)? = Some(name.to_string());
        }
        if let Some(c) = r {
            *self.colnames[j.right].get_mut(c)? = Some(name.to_string());
        }
        Some(())
    }

    fn set_using_names(&mut self, level: &[Rte], idx: usize, parent: &[String]) -> Option<()> {
        let Some(j) = level[idx].join.as_deref() else {
            return Some(());
        };
        // An unnamed JOIN cannot rename at its level: a name pushed down to
        // it goes on down to its inputs.
        if !j.named {
            for i in 0..self.colnames[idx].len() {
                if let Some(n) = self.colnames[idx][i].clone() {
                    self.push_down(idx, i, &n, level)?;
                }
            }
        }
        let mut parent_using = parent.to_vec();
        for i in 0..j.using.len() {
            let name = match self.colnames[idx][i].clone() {
                Some(n) => n,
                None => {
                    let written = j.alias_cols.get(i).unwrap_or(&j.using[i]);
                    let n = self.make_colname_unique(written, idx);
                    if self.unique_using {
                        self.global.push(n.clone());
                    }
                    self.colnames[idx][i] = Some(n.clone());
                    n
                }
            };
            self.using_names[idx].push(name.clone());
            parent_using.push(name.clone());
            self.push_down(idx, i, &name, level)?;
        }
        self.parent_using[j.left] = parent_using.clone();
        self.parent_using[j.right] = parent_using.clone();
        self.set_using_names(level, j.left, &parent_using)?;
        self.set_using_names(level, j.right, &parent_using)
    }

    /// The entry's `printaliases`.
    fn set_relation_column_names(&mut self, level: &[Rte], idx: usize) -> bool {
        let r = &level[idx];
        let mut changed = false;
        for i in 0..r.columns.len() {
            let name = match self.colnames[idx][i].clone() {
                Some(n) => n,
                None => {
                    let n = self.make_colname_unique(&r.columns[i].0, idx);
                    self.colnames[idx][i] = Some(n.clone());
                    n
                }
            };
            let real = r.real.get(i).unwrap_or(&r.columns[i].0);
            changed |= name != *real;
        }
        match r.kind {
            RteKind::Relation => changed,
            RteKind::Function => true,
            _ => r.alias_cols || changed,
        }
    }

    /// The JOIN's `printaliases`: a named JOIN prints its column aliases
    /// when one differs from its input column's name.
    fn set_join_column_names(&mut self, level: &[Rte], idx: usize) -> Option<bool> {
        let j = level[idx].join.as_deref()?;
        let mut changed = false;
        for i in self.using_names[idx].len()..level[idx].columns.len() {
            let real = match j.sources[i] {
                (Some(c), _) => self.colnames[j.left].get(c)?.clone()?,
                (None, Some(c)) => self.colnames[j.right].get(c)?.clone()?,
                (None, None) => return None,
            };
            if !j.named {
                self.colnames[idx][i] = Some(real);
                continue;
            }
            let name = match self.colnames[idx][i].clone() {
                Some(n) => n,
                None => {
                    let written = j.alias_cols.get(i).unwrap_or(&real).clone();
                    let n = self.make_colname_unique(&written, idx);
                    self.colnames[idx][i] = Some(n.clone());
                    n
                }
            };
            changed |= name != real;
        }
        Some(j.named && changed)
    }

    /// `colname_is_unique`.
    fn is_unique(&self, name: &str, idx: usize) -> bool {
        !self.colnames[idx].iter().flatten().any(|n| n == name)
            && !self.global.iter().any(|n| n == name)
            && !self.parent_using[idx].iter().any(|n| n == name)
    }

    /// `make_colname_unique`: `name_N` for the least N free, the name
    /// clipped to keep the digits within NAMEDATALEN.
    fn make_colname_unique(&self, name: &str, idx: usize) -> String {
        if self.is_unique(name, idx) {
            return name.to_string();
        }
        let mut base = name.to_string();
        let mut i = 0;
        loop {
            i += 1;
            let modname = loop {
                let m = format!("{base}_{i}");
                if m.len() <= MAX_IDENT || base.is_empty() {
                    break m;
                }
                base.pop();
            };
            if self.is_unique(&modname, idx) {
                return modname;
            }
        }
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
/// not reproduce. `written_aliases` is the definition's
/// [`crate::schemas::written_aliases`], when it was kept.
pub fn viewdef(sql: &str, written_aliases: Option<&[bool]>, cat: &Catalog<'_>) -> Option<String> {
    viewdef_with(sql, written_aliases, cat, false, false)
}

/// `pg_get_viewdef(view, true)`: parentheses only where precedence needs
/// them (PRETTYFLAG_PAREN).
pub fn viewdef_pretty(
    sql: &str,
    written_aliases: Option<&[bool]>,
    cat: &Catalog<'_>,
) -> Option<String> {
    viewdef_with(sql, written_aliases, cat, true, false)
}

fn viewdef_with(
    sql: &str,
    written_aliases: Option<&[bool]>,
    cat: &Catalog<'_>,
    pretty: bool,
    markers: bool,
) -> Option<String> {
    let node = parse_select(sql)?;
    let mut p = Printer::new(cat, 0);
    p.pretty = pretty;
    p.markers = markers;
    p.written_alias = std::rc::Rc::new(written_alias_at(&node, written_aliases));
    p.query(&node, None)?;
    Some(format!("{};", p.buf))
}

/// `written`, one flag per `RangeVar` of `node` in `nodes()` order, keyed
/// by each one's location in the text `node` was parsed from. Nothing when
/// the counts differ: the flags were not taken from this text's tree.
fn written_alias_at(node: &Node, written: Option<&[bool]>) -> std::collections::HashMap<i32, bool> {
    let (Some(written), Some(n)) = (written, node.node.as_ref()) else {
        return Default::default();
    };
    let locations: Vec<i32> = n
        .nodes()
        .into_iter()
        .filter_map(|(n, _, _, _)| match n {
            pg_query::NodeRef::RangeVar(r) => Some(r.location),
            _ => None,
        })
        .collect();
    if locations.len() != written.len() {
        return Default::default();
    }
    locations.into_iter().zip(written.iter().copied()).collect()
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
pub fn viewdef_marked(
    sql: &str,
    written_aliases: Option<&[bool]>,
    cat: &Catalog<'_>,
) -> Option<String> {
    viewdef_with(sql, written_aliases, cat, true, true)
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
                    ..Default::default()
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
                    ..Default::default()
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
            ..Default::default()
        });
    }
    if event != "INSERT" {
        pseudo.push(Rte {
            refname: "old".into(),
            name: "old".into(),
            columns,
            qualified_only: true,
            ..Default::default()
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

/// The clauses `transformAExprBetween` makes of `x BETWEEN lo AND hi`:
/// `x >= lo` and `x <= hi`, under an AND that the planner flattens into the
/// qual list beside the statement's other conjuncts, so EXPLAIN prints
/// them as two quals (RP-399). `None` for any other node: NOT BETWEEN and
/// the SYMMETRIC forms are an OR, which stays one clause.
pub fn between_clauses(n: &Node) -> Option<[Node; 2]> {
    let Some(N::AExpr(e)) = n.node.as_ref() else {
        return None;
    };
    if e.kind != AExprKind::AexprBetween as i32 {
        return None;
    }
    let Some(N::List(list)) = e.rexpr.as_deref().and_then(|r| r.node.as_ref()) else {
        return None;
    };
    let [lo, hi] = list.items.as_slice() else {
        return None;
    };
    let clause = |op: &str, bound: &Node| Node {
        node: Some(N::AExpr(Box::new(pg_query::protobuf::AExpr {
            kind: AExprKind::AexprOp as i32,
            name: vec![Node {
                node: Some(N::String(pg_query::protobuf::String {
                    sval: op.to_string(),
                })),
            }],
            lexpr: e.lexpr.clone(),
            rexpr: Some(Box::new(bound.clone())),
            location: e.location,
        }))),
    };
    Some([clause(">=", lo), clause("<=", hi)])
}

/// A qual's conjuncts as the planner lists them: nested ANDs flattened
/// (`make_ands_implicit` over an AND `eval_const_expressions` has
/// flattened) and a BETWEEN as its two clauses ([`between_clauses`]).
pub fn and_clauses<'n>(n: &'n Node, out: &mut Vec<std::borrow::Cow<'n, Node>>) {
    match n.node.as_ref() {
        Some(N::BoolExpr(b)) if b.boolop == BoolExprType::AndExpr as i32 => {
            for a in &b.args {
                and_clauses(a, out);
            }
        }
        _ => match between_clauses(n) {
            Some(halves) => out.extend(halves.map(std::borrow::Cow::Owned)),
            None => out.push(std::borrow::Cow::Borrowed(n)),
        },
    }
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
    and_clauses(where_clause, &mut parts);
    parts
        .iter()
        .map(|p| {
            let p: &Node = p;
            let text = expr_node_planned(p, def)?;
            Some((text, qual_column(p)))
        })
        .collect::<Option<Vec<_>>>()
        .map(|printed| qual_order(&parts, printed))
}

/// Each top-level `OR` conjunct of a WHERE, by the text
/// [`explain_conjuncts`] prints for it, with per arm that arm's conjuncts
/// as [`explain_conjuncts`] prints them (RP-378).
pub type OrArms = Vec<(String, Vec<Vec<(String, Option<String>)>>)>;

/// A single-relation WHERE's [`OrArms`]; `None` for a shape the printer
/// does not know.
pub fn explain_or_arms(where_clause: &Node, def: &TableDef) -> Option<OrArms> {
    let mut parts = Vec::new();
    and_clauses(where_clause, &mut parts);
    let mut out = Vec::new();
    for p in &parts {
        let p: &Node = p;
        let Some(N::BoolExpr(b)) = p.node.as_ref() else {
            continue;
        };
        if b.boolop != BoolExprType::OrExpr as i32 {
            continue;
        }
        let text = expr_node_planned(p, def)?;
        let arms = b
            .args
            .iter()
            .map(|a| explain_conjuncts(a, def))
            .collect::<Option<Vec<_>>>()?;
        out.push((text, arms));
    }
    Some(out)
}

/// The column an index-usable conjunct constrains: `column op constant`
/// (either way round) of a B-tree comparison operator, `column BETWEEN
/// constant AND constant` (two such clauses once the parser expands it)
/// and `column IN (constants)` (a `ScalarArrayOpExpr`, which a B-tree
/// index serves); `None` for any other conjunct.
fn qual_column(p: &Node) -> Option<String> {
    let column = |n: Option<&Node>| match n.and_then(|n| n.node.as_ref()) {
        Some(N::ColumnRef(c)) => names(&c.fields).pop(),
        _ => None,
    };
    let constant = |n: &Node| {
        matches!(
            n.node.as_ref(),
            Some(N::AConst(_) | N::TypeCast(_) | N::ParamRef(_))
        )
    };
    let constants = |n: Option<&Node>| match n.and_then(|n| n.node.as_ref()) {
        Some(N::List(l)) => !l.items.is_empty() && l.items.iter().all(constant),
        _ => false,
    };
    match p.node.as_ref() {
        Some(N::AExpr(e))
            if e.kind == AExprKind::AexprBetween as i32 && constants(e.rexpr.as_deref()) =>
        {
            column(e.lexpr.as_deref())
        }
        Some(N::AExpr(e))
            if e.kind == AExprKind::AexprIn as i32
                && crate::operator_name(e).ok() == Some("=")
                && constants(e.rexpr.as_deref()) =>
        {
            column(e.lexpr.as_deref())
        }
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
    }
}

/// The order PostgreSQL 15's planner leaves a scan's quals in (RP-348). A
/// `column = constant` is absorbed into an equivalence class
/// (`distribute_qual_to_rels`) and put back after the other clauses, one
/// class at a time (`generate_base_implied_equalities`); then
/// `order_qual_clauses` sorts the list by each clause's per-row cost
/// (`cost_qual_eval`), keeping the order of equal costs. Kept as written
/// when a clause's cost or class is not modelled: anything but a column
/// compared with a literal, an `IN` list of literals and a null test, or a
/// column equal to two constants (a class whose clauses are generated
/// otherwise).
fn qual_order(
    parts: &[std::borrow::Cow<'_, Node>],
    printed: Vec<(String, Option<String>)>,
) -> Vec<(String, Option<String>)> {
    use crate::costsize::CPU_OPERATOR_COST as OP;
    let literal = |n: Option<&Node>| match n.and_then(|n| n.node.as_ref()) {
        Some(N::AConst(_)) => true,
        Some(N::TypeCast(t)) => matches!(
            t.arg.as_deref().and_then(|a| a.node.as_ref()),
            Some(N::AConst(_))
        ),
        _ => false,
    };
    let column = |n: Option<&Node>| match n.and_then(|n| n.node.as_ref()) {
        Some(N::ColumnRef(c)) => names(&c.fields).pop(),
        _ => None,
    };
    // (cost, the column of an equivalence class it joins).
    let classify = |n: &Node, text: &str| -> Option<(f64, Option<String>)> {
        // A coercion of the column that is a function call (not varchar's
        // relabelling to text) costs an operator more: not modelled.
        if text.starts_with("((") && !text.contains(")::text ") {
            return None;
        }
        match n.node.as_ref()? {
            N::AExpr(e) if e.kind == AExprKind::AexprOp as i32 => {
                let op = crate::operator_name(e).ok()?;
                if !matches!(op, "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=") {
                    return None;
                }
                let col = column(e.lexpr.as_deref())?;
                if !literal(e.rexpr.as_deref()) {
                    return None;
                }
                Some((OP, (op == "=").then_some(col)))
            }
            N::AExpr(e) if e.kind == AExprKind::AexprIn as i32 => {
                let op = crate::operator_name(e).ok()?;
                let col = column(e.lexpr.as_deref())?;
                let Some(N::List(list)) = e.rexpr.as_deref().and_then(|r| r.node.as_ref()) else {
                    return None;
                };
                if !list.items.iter().all(|i| literal(Some(i))) {
                    return None;
                }
                match list.items.len() {
                    // One element is the plain operator.
                    1 => Some((OP, (op == "=").then_some(col))),
                    n if n >= 9 => Some((2.0 * OP, None)),
                    n => Some((OP * n as f64 * 0.5, None)),
                }
            }
            N::NullTest(t) => {
                column(t.arg.as_deref())?;
                Some((0.0, None))
            }
            _ => None,
        }
    };
    let mut keyed = Vec::with_capacity(printed.len());
    let mut classes: Vec<String> = Vec::new();
    for (i, (p, item)) in parts.iter().zip(printed.iter()).enumerate() {
        let Some((cost, class)) = classify(p, &item.0) else {
            return printed;
        };
        if let Some(c) = &class {
            if classes.contains(c) {
                return printed;
            }
            classes.push(c.clone());
        }
        keyed.push((class.is_some(), cost, i));
    }
    // The classes' clauses after the rest, then a stable sort by cost.
    keyed.sort_by_key(|k| k.0);
    keyed.sort_by(|a, b| a.1.total_cmp(&b.1));
    keyed
        .into_iter()
        .map(|(_, _, i)| printed[i].clone())
        .collect()
}

/// Each JOIN's ON condition in a SELECT's FROM, post-order (a left-deep
/// join's inner joins first), printed as EXPLAIN does (qualified).
pub fn join_conditions(
    s: &pg_query::protobuf::SelectStmt,
    cat: &Catalog<'_>,
) -> Option<Vec<String>> {
    Some(
        join_conditions_split(s, cat)?
            .into_iter()
            .map(|(c, _)| c)
            .collect(),
    )
}

/// [`join_conditions`], each with its split for a hash or merge join: an
/// ON that ANDs an equality between the join's two sides with conjuncts
/// that are not one gives `Some((join clauses, the rest))`. Only the
/// equalities are the hash (or merge) clauses (`hash_inner_and_outer`,
/// `select_mergejoin_clauses`); the rest is the join's `joinqual`, printed
/// as its `Join Filter` (RP-375). `None` where the ON is all one or the
/// other.
#[allow(clippy::type_complexity)]
pub fn join_conditions_split(
    s: &pg_query::protobuf::SelectStmt,
    cat: &Catalog<'_>,
) -> Option<Vec<(String, Option<(String, String)>)>> {
    let mut p = Printer::new(cat, 0);
    p.legacy_joins = true;
    let mut rtes = Vec::new();
    p.collect_rtes(&s.from_clause, &mut rtes)?;
    p.scopes.push(rtes);
    // The names a FROM item's relations go by.
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
    // The relations an expression's columns belong to; `None` when one
    // does not resolve.
    fn rels_of(p: &Printer<'_>, n: &Node) -> Option<Vec<String>> {
        let mut rels: Vec<String> = Vec::new();
        let mut ok = true;
        let mut copy = n.clone();
        crate::walk_expr(&mut copy, &mut |x| {
            if let Some(N::ColumnRef(c)) = x.node.as_ref() {
                match p.resolve(&names(&c.fields)) {
                    Some((rel, _, _)) => {
                        if !rels.contains(&rel) {
                            rels.push(rel);
                        }
                    }
                    None => ok = false,
                }
            }
            Ok(())
        })
        .ok()?;
        ok.then_some(rels)
    }
    // `left op right` with `=`, one operand over the join's left side
    // only and the other over its right side only: a hashable clause
    // (`clause_sides_match_join`).
    fn hashable(p: &Printer<'_>, c: &Node, left: &[String], right: &[String]) -> bool {
        let Some(N::AExpr(a)) = c.node.as_ref() else {
            return false;
        };
        let is_eq = a.kind == pg_query::protobuf::AExprKind::AexprOp as i32
            && matches!(
                a.name.as_slice(),
                [one] if matches!(one.node.as_ref(), Some(N::String(op)) if op.sval == "=")
            );
        if !is_eq {
            return false;
        }
        let (Some(l), Some(r)) = (a.lexpr.as_deref(), a.rexpr.as_deref()) else {
            return false;
        };
        let (Some(lr), Some(rr)) = (rels_of(p, l), rels_of(p, r)) else {
            return false;
        };
        let within = |rels: &[String], side: &[String]| {
            !rels.is_empty() && rels.iter().all(|x| side.contains(x))
        };
        (within(&lr, left) && within(&rr, right)) || (within(&lr, right) && within(&rr, left))
    }
    type Out = Vec<(String, Option<(String, String)>)>;
    fn walk(p: &mut Printer<'_>, n: &Node, out: &mut Out) -> Option<()> {
        if let Some(N::JoinExpr(j)) = n.node.as_ref() {
            walk(p, j.larg.as_deref()?, out)?;
            walk(p, j.rarg.as_deref()?, out)?;
            match j.quals.as_deref() {
                Some(q) => {
                    p.buf.clear();
                    p.expr(q)?;
                    let whole = std::mem::take(&mut p.buf);
                    let (mut left, mut right) = (Vec::new(), Vec::new());
                    refnames(j.larg.as_deref()?, &mut left);
                    refnames(j.rarg.as_deref()?, &mut right);
                    let mut parts = Vec::new();
                    and_clauses(q, &mut parts);
                    let mut hash = Vec::new();
                    let mut rest: Vec<(usize, String)> = Vec::new();
                    for part in &parts {
                        let part: &Node = part;
                        p.buf.clear();
                        p.expr(part)?;
                        let text = std::mem::take(&mut p.buf);
                        if hashable(p, part, &left, &right) {
                            hash.push(text);
                        } else {
                            // A cancel stops the walk; it stays pending
                            // for the statement's next cancellation point.
                            rest.push((qual_operators(part).ok()?, text));
                        }
                    }
                    // `order_qual_clauses`: the join filter cheapest first,
                    // a stable sort by each clause's operators.
                    rest.sort_by_key(|(ops, _)| *ops);
                    let rest: Vec<String> = rest.into_iter().map(|(_, t)| t).collect();
                    let and = |v: &[String]| match v {
                        [one] => one.clone(),
                        many => format!("({})", many.join(" AND ")),
                    };
                    let split =
                        (!hash.is_empty() && !rest.is_empty()).then(|| (and(&hash), and(&rest)));
                    out.push((whole, split));
                }
                None => out.push((String::new(), None)),
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
/// down to the relation's scan; any other stays on the join), and, for a
/// pushed-down `column op constant`, the column, which an index scan of
/// the relation may take as its `Index Cond` (RP-369).
pub fn join_where(s: &pg_query::protobuf::SelectStmt, cat: &Catalog<'_>) -> Option<Vec<JoinQual>> {
    let w = s.where_clause.as_deref()?;
    let mut p = Printer::new(cat, 0);
    p.legacy_joins = true;
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
    // `reduce_outer_joins`: a side some WHERE conjunct proves non-NULL is
    // not null-extended, its join an inner one (`find_nonnullable_rels`).
    fn nullable(n: &Node, nonnull: &[String], out: &mut Vec<String>) {
        if let Some(N::JoinExpr(j)) = n.node.as_ref() {
            let (l, r) = (j.larg.as_deref(), j.rarg.as_deref());
            let forced = |x: &Node| {
                let mut names = Vec::new();
                refnames(x, &mut names);
                names.iter().any(|n| nonnull.contains(n))
            };
            let side = |x: Option<&Node>, out: &mut Vec<String>| {
                if let Some(x) = x.filter(|x| !forced(x)) {
                    refnames(x, out);
                }
            };
            match JoinType::try_from(j.jointype) {
                Ok(JoinType::JoinLeft) => side(r, out),
                Ok(JoinType::JoinRight) => side(l, out),
                Ok(JoinType::JoinFull) => {
                    side(l, out);
                    side(r, out);
                }
                _ => {}
            }
            for side in [l, r].into_iter().flatten() {
                nullable(side, nonnull, out);
            }
        }
    }
    let mut parts = Vec::new();
    and_clauses(w, &mut parts);
    // The relations a conjunct proves non-NULL, as the planner's own
    // `reduce_outer_joins` finds them (`joins::strict_aliases`), so the
    // printed plan reduces the joins the executed one does: a comparison
    // over columns under a cast or arithmetic (`(s.a * 2) > 4`), or `IS
    // NOT NULL` of one (RP-405).
    let mut nonnull: Vec<String> = Vec::new();
    {
        let owner = |name: &str| p.resolve(&[name.to_string()]).map(|(rel, _, _)| rel);
        for part in &parts {
            for rel in crate::joins::strict_aliases(part, &owner) {
                if !nonnull.contains(&rel) {
                    nonnull.push(rel);
                }
            }
        }
    }
    let mut nulls = Vec::new();
    for item in &s.from_clause {
        nullable(item, &nonnull, &mut nulls);
    }
    let mut out = Vec::new();
    for part in &parts {
        let part: &Node = part;
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
        let column = home.as_ref().and_then(|_| qual_column(part));
        out.push((text, home, column));
    }
    Some(out)
}

/// A comma join of two relations (`FROM e, k`, RP-372): its WHERE's
/// equalities between a column of each, taken out of `quals` (as
/// [`join_where`] listed them, one per conjunct) and returned as the one
/// join's condition, ANDed. PostgreSQL makes an inner join's WHERE and ON
/// quals one list (`deconstruct_jointree`), so the equality is the hash or
/// merge clause an ON's would be. Empty -- `quals` untouched -- for any
/// other FROM, or when a conjunct over both relations is not such an
/// equality.
pub fn comma_join_conds(
    s: &pg_query::protobuf::SelectStmt,
    quals: &mut Vec<JoinQual>,
) -> crate::Result<Vec<String>> {
    let two = s.from_clause.len() == 2
        && s.from_clause
            .iter()
            .all(|n| matches!(n.node.as_ref(), Some(N::RangeVar(_))));
    let Some(w) = s.where_clause.as_deref().filter(|_| two) else {
        return Ok(Vec::new());
    };
    crate::cancel::check()?;
    let mut parts = Vec::new();
    and_clauses(w, &mut parts);
    if parts.len() != quals.len() {
        return Ok(Vec::new());
    }
    let column_equality = |n: &Node| {
        matches!(n.node.as_ref(), Some(N::AExpr(a))
        if a.kind == pg_query::protobuf::AExprKind::AexprOp as i32
            && matches!(a.name.as_slice(), [one]
                if matches!(one.node.as_ref(), Some(N::String(op)) if op.sval == "="))
            && [a.lexpr.as_deref(), a.rexpr.as_deref()].iter().all(|x| {
                matches!(x.and_then(|x| x.node.as_ref()), Some(N::ColumnRef(_)))
            }))
    };
    // The equalities between the two tables are the join clauses; any
    // other conjunct over both stays in `quals`, the join's `Join Filter`
    // (`hash_inner_and_outer` hashes only the equalities, RP-375). One
    // pass classifies every conjunct, so the work stays linear in their
    // number however the client interleaves them (RP-388).
    let spanning: Vec<bool> = parts
        .iter()
        .zip(quals.iter())
        .map(|(part, qual)| qual.1.is_none() && column_equality(part))
        .collect();
    if !spanning.contains(&true) {
        return Ok(Vec::new());
    }
    // `order_qual_clauses`: the conjuncts left on the join, its `Join
    // Filter`, cheapest first (a stable sort by their operators), back in
    // the positions they held.
    let slots: Vec<usize> = (0..parts.len())
        .filter(|i| quals[*i].1.is_none() && !spanning[*i])
        .collect();
    let mut rest: Vec<(usize, JoinQual)> = Vec::with_capacity(slots.len());
    for i in &slots {
        rest.push((qual_operators(&parts[*i])?, std::mem::take(&mut quals[*i])));
    }
    rest.sort_by_key(|(ops, _)| *ops);
    for (slot, (_, qual)) in slots.iter().zip(rest) {
        quals[*slot] = qual;
    }
    let mut texts = Vec::new();
    let mut kept = Vec::with_capacity(quals.len());
    for (qual, span) in std::mem::take(quals).into_iter().zip(spanning) {
        if span {
            texts.push(qual.0);
        } else {
            kept.push(qual);
        }
    }
    *quals = kept;
    Ok(match texts.as_slice() {
        [one] => vec![one.clone()],
        many => vec![format!("({})", many.join(" AND "))],
    })
}

/// The operators a clause evaluates (`cost_qual_eval`, each one
/// `cpu_operator_cost`).
fn qual_operators(n: &Node) -> crate::Result<usize> {
    let mut count = 0;
    let mut copy = n.clone();
    crate::walk_expr(&mut copy, &mut |x| {
        if let Some(N::AExpr(a)) = x.node.as_ref() {
            if a.kind == pg_query::protobuf::AExprKind::AexprOp as i32
                || a.kind == pg_query::protobuf::AExprKind::AexprDistinct as i32
            {
                count += 1;
            }
        }
        Ok(())
    })?;
    Ok(count)
}

/// A joined SELECT's WHERE conjunct for EXPLAIN: its text, the relation
/// whose scan it filters (`None`: the top join's), and the column an index
/// scan there could take it on (RP-369).
pub type JoinQual = (String, Option<String>, Option<String>);

/// An expression node over one table, printed as `pg_get_expr` does.
pub fn expr_node_def(n: &Node, def: &TableDef) -> Option<String> {
    expr_node_printed(n, def, false)
}

/// [`expr_node_def`] as EXPLAIN prints the planned expression, its
/// constants folded.
/// An `IN` list of constants of type `elem` as the array constant the
/// planner folds it to, quoted as a SQL literal: `array_out`'s text
/// (`'{1,2,3}'`, an element quoted when it is empty, `NULL`, or holds a
/// space, a brace, a comma, a quote or a backslash). `None` for an element
/// that is not a bare literal, or a type whose output is not reproduced
/// here.
/// The output text of an integer constant cast to `target`, as
/// `eval_const_expressions` folds it (RP-370). `None` for any other shape,
/// which then prints unfolded: a non-integer constant, a target this does
/// not reproduce, or a value out of the target's range (PostgreSQL raises
/// that while planning).
fn folded_numeric_cast(arg: &Node, target: &str) -> Option<String> {
    let Some(N::AConst(c)) = arg.node.as_ref() else {
        return None;
    };
    let v: i128 = match c.val.as_ref()? {
        Val::Ival(i) => i.ival.into(),
        Val::Fval(f) => f.fval.parse().ok()?,
        _ => return None,
    };
    let fits = match target {
        "int2" => i16::try_from(v).is_ok(),
        "int4" => i32::try_from(v).is_ok(),
        "int8" => i64::try_from(v).is_ok(),
        "numeric" | "text" => true,
        _ => false,
    };
    fits.then(|| v.to_string())
}

fn folded_array_literal(items: &[Node], elem: &str) -> Option<String> {
    let mut out = String::from("{");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let Some(N::AConst(c)) = item.node.as_ref() else {
            return None;
        };
        if c.isnull {
            out.push_str("NULL");
            continue;
        }
        let text = match (elem, c.val.as_ref()?) {
            ("int2" | "int4" | "int8" | "numeric", Val::Ival(v)) => v.ival.to_string(),
            ("int8", Val::Fval(f)) => f.fval.parse::<i64>().ok()?.to_string(),
            ("text", Val::Sval(v)) => {
                let s = &v.sval;
                let quote = s.is_empty()
                    || s.eq_ignore_ascii_case("NULL")
                    || s.chars().any(|ch| {
                        matches!(ch, '"' | '\\' | '{' | '}' | ',')
                            || matches!(ch, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
                    });
                if quote {
                    let mut q = String::from("\"");
                    for ch in s.chars() {
                        if matches!(ch, '"' | '\\') {
                            q.push('\\');
                        }
                        q.push(ch);
                    }
                    q.push('"');
                    q
                } else {
                    s.clone()
                }
            }
            _ => return None,
        };
        out.push_str(&text);
    }
    out.push('}');
    Some(format!("'{}'", out.replace('\'', "''")))
}

fn expr_node_planned(n: &Node, def: &TableDef) -> Option<String> {
    expr_node_printed(n, def, true)
}

/// A WHERE over one FROM-subquery as EXPLAIN prints its Subquery Scan's
/// `Filter` (RP-328): each AND conjunct, its columns qualified by the
/// subquery's alias (`def.name`), as ruleutils does once the plan's range
/// table holds more than one relation.
pub fn explain_subquery_conjuncts(where_clause: &Node, def: &TableDef) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    and_clauses(where_clause, &mut parts);
    let printed = parts
        .iter()
        .map(|p| Some((expr_node_printed_with(p, def, true, false)?, None)))
        .collect::<Option<Vec<_>>>()?;
    // In the planner's order, as a scan's (RP-348).
    Some(
        qual_order(&parts, printed)
            .into_iter()
            .map(|(text, _)| text)
            .collect(),
    )
}

fn expr_node_printed(n: &Node, def: &TableDef, fold_consts: bool) -> Option<String> {
    expr_node_printed_with(n, def, fold_consts, true)
}

fn expr_node_printed_with(
    n: &Node,
    def: &TableDef,
    fold_consts: bool,
    unqualified: bool,
) -> Option<String> {
    let none_lookup = |_: &str| None;
    let none_views = |_: &str| None;
    let cat = Catalog {
        lookup: &none_lookup,
        view_sql: &none_views,
    };
    let mut p = Printer::new(&cat, 0);
    p.unqualified = unqualified;
    p.fold_consts = fold_consts;
    p.scopes.push(vec![Rte {
        refname: def.name.clone(),
        name: def.name.clone(),
        columns: def
            .columns
            .iter()
            .map(|c| (c.name.clone(), base_type(&c.pg_type)))
            .collect(),
        qualified_only: false,
        ..Default::default()
    }]);
    p.expr(n)?;
    Some(p.buf)
}

/// A FROM-less statement's unfolded WHERE as EXPLAIN prints it as the
/// `Result`'s `One-Time Filter` (RP-342): no relation in scope, constants
/// folded. `None` for a shape the printer does not know.
pub fn explain_one_time_qual(n: &Node) -> Option<String> {
    let none_lookup = |_: &str| None;
    let none_views = |_: &str| None;
    let cat = Catalog {
        lookup: &none_lookup,
        view_sql: &none_views,
    };
    let mut p = Printer::new(&cat, 0);
    p.unqualified = true;
    p.fold_consts = true;
    p.scopes.push(Vec::new());
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
        ..Default::default()
    }]);
    p.expr(rt.val.as_deref()?)?;
    Some(p.buf)
}

/// A statistics object's expression as `pg_get_statisticsobjdef_expressions`
/// prints it: as `pg_get_expr` does, an operator expression in its own
/// parentheses (`(a + (b * (c)::integer))`), a call bare (`lower(c)`).
/// PostgreSQL 15.19 measured.
pub fn statistics_expr_def(sql: &str, def: &TableDef) -> Option<String> {
    expr_def(sql, def)
}

/// The same expression as `pg_get_statisticsobjdef` and its `_columns`
/// print it (`pg_get_statisticsobj_worker`): pretty, and in parentheses
/// unless it looks like a function call (`(a + b * c::integer)`,
/// `lower(c)`).
pub fn statistics_expr_def_pretty(sql: &str, def: &TableDef) -> Option<String> {
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
    let expr = rt.val.as_deref()?;
    let none_lookup = |_: &str| None;
    let none_views = |_: &str| None;
    let cat = Catalog {
        lookup: &none_lookup,
        view_sql: &none_views,
    };
    let mut p = Printer::new(&cat, 0);
    p.unqualified = true;
    p.pretty = true;
    p.scopes.push(vec![Rte {
        refname: def.name.clone(),
        name: def.name.clone(),
        columns: def
            .columns
            .iter()
            .map(|c| (c.name.clone(), base_type(&c.pg_type)))
            .collect(),
        qualified_only: false,
        ..Default::default()
    }]);
    p.expr(expr)?;
    let function_like = matches!(
        expr.node.as_ref(),
        Some(
            N::FuncCall(_)
                | N::CoalesceExpr(_)
                | N::MinMaxExpr(_)
                | N::SqlvalueFunction(_)
                | N::XmlExpr(_)
        )
    );
    Some(if function_like {
        p.buf
    } else {
        format!("({})", p.buf)
    })
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
            "vd_s.vd_t" => Some(TableDef::new(
                "vd_s.vd_t",
                vec![Column::new("id", "int4", false)],
            )),
            _ => None,
        }
    }

    fn render(sql: &str) -> Option<String> {
        render_written(sql, None)
    }

    fn render_written(sql: &str, written: Option<&[bool]>) -> Option<String> {
        let none = |_: &str| None;
        let cat = Catalog {
            lookup: &cat_lookup,
            view_sql: &none,
        };
        viewdef(sql, written, &cat)
    }

    // A reference stored under its `schema.name` key carries its bare name
    // as an alias, which prints only when the statement wrote it
    // (`get_rte_alias`); the definition's `written_aliases` say which.
    // Without them, an alias that is the bare name is the supplied one.
    // Measured on PostgreSQL 15.19 (`FROM b41a.t` / `FROM b41a.t t`).
    #[test]
    fn a_qualified_reference_prints_only_a_written_alias() {
        let sql = "SELECT vd_t.id FROM \"vd_s.vd_t\" vd_t";
        let bare = " SELECT vd_t.id\n   FROM vd_s.vd_t;";
        let aliased = " SELECT vd_t.id\n   FROM vd_s.vd_t vd_t;";
        assert_eq!(render_written(sql, Some(&[false])).as_deref(), Some(bare));
        assert_eq!(render_written(sql, Some(&[true])).as_deref(), Some(aliased));
        assert_eq!(render_written(sql, None).as_deref(), Some(bare));
        // Flags for another tree are not read.
        assert_eq!(
            render_written(sql, Some(&[true, true])).as_deref(),
            Some(bare)
        );
        // Any other alias is a written one.
        assert_eq!(
            render_written("SELECT x.id FROM \"vd_s.vd_t\" x", None).as_deref(),
            Some(" SELECT x.id\n   FROM vd_s.vd_t x;")
        );
    }

    #[test]
    fn a_simple_view_reads_as_ruleutils_prints_it() {
        assert_eq!(
            render("SELECT id, t FROM vd_t").as_deref(),
            Some(" SELECT vd_t.id,\n    vd_t.t\n   FROM vd_t;")
        );
    }

    // `set_deparse_for_query`'s column naming and `get_variable`'s join
    // expansion, each answer measured on PostgreSQL 15.19 (RP-467).
    #[test]
    fn a_join_names_its_columns_as_ruleutils_does() {
        let cases = [
            // An unnamed join's merged column is its left input's.
            (
                "SELECT id, w FROM vd_t JOIN vd_u USING (id)",
                " SELECT vd_t.id,\n    vd_u.w\n   FROM (vd_t\n     JOIN vd_u USING (id));",
            ),
            // NATURAL prints as the USING list it stands for, and `*` takes
            // the merged column once.
            (
                "SELECT * FROM vd_t NATURAL JOIN vd_u",
                " SELECT vd_t.id,\n    vd_t.v,\n    vd_t.t,\n    vd_t.n,\n    vd_t.d,\n    \
                 vd_t.b,\n    vd_u.t_id,\n    vd_u.w\n   FROM (vd_t\n     JOIN vd_u USING (id));",
            ),
            // An aliased join prints its alias, with a column list once a
            // name had to be made unique.
            (
                "SELECT j.w FROM (vd_t JOIN vd_u ON vd_t.id = vd_u.t_id) j",
                " SELECT j.w\n   FROM (vd_t\n     JOIN vd_u ON ((vd_t.id = vd_u.t_id))) \
                 j(id, v, t, n, d, b, id_1, t_id, w);",
            ),
            // A FULL join's merged column is a COALESCE: unqualified.
            (
                "SELECT id, w FROM vd_t FULL JOIN vd_u USING (id)",
                " SELECT id,\n    vd_u.w\n   FROM (vd_t\n     FULL JOIN vd_u USING (id));",
            ),
            // A RIGHT join's is its right input's, through a USING alias.
            (
                "SELECT u.id FROM vd_t RIGHT JOIN vd_u USING (id) AS u",
                " SELECT vd_u.id\n   FROM (vd_t\n     RIGHT JOIN vd_u USING (id) AS u);",
            ),
        ];
        for (sql, want) in cases {
            assert_eq!(render(sql).as_deref(), Some(want), "{sql}");
        }
    }
}
#[cfg(test)]
mod array_quals {
    use super::*;
    use secantus_pgcatalog::Column;

    /// An array operator against an untyped literal prints the literal as
    /// the column's array constant, in `array_out` form, as PostgreSQL
    /// 15.19's EXPLAIN does (RP-367).
    #[test]
    fn an_array_operators_literal_takes_the_columns_type() {
        let def = TableDef::new("ud", vec![Column::new("a", "int4[]", false)]);
        for (sql, want) in [
            ("a @> '{1}'", "(a @> '{1}'::integer[])"),
            ("a && '{1,2}'", "(a && '{1,2}'::integer[])"),
            ("a <@ '{ 1 , 2 }'", "(a <@ '{1,2}'::integer[])"),
            ("'{1}' <@ a", "('{1}'::integer[] <@ a)"),
        ] {
            let p = pg_query::parse(&format!("select * from ud where {sql}")).unwrap();
            let Some(N::SelectStmt(s)) = p.protobuf.stmts[0].stmt.as_ref().unwrap().node.as_ref()
            else {
                panic!()
            };
            let got = explain_conjuncts(s.where_clause.as_deref().unwrap(), &def).unwrap();
            assert_eq!(got, vec![(want.to_string(), None)], "{sql}");
        }
    }
}
#[cfg(test)]
mod index_qual_columns {
    use super::*;
    use secantus_pgcatalog::Column;

    /// A BETWEEN of constants and an IN list of constants constrain their
    /// column for an index, as PostgreSQL 15.19 prints them in an `Index
    /// Cond` (RP-339); a NOT IN or a symmetric BETWEEN does not.
    #[test]
    fn between_and_in_name_their_column() {
        let def = TableDef::new("k", vec![Column::new("a", "int4", false)]);
        for (sql, want) in [
            ("a between 10 and 20", Some("a")),
            ("a in (1, 2, 3)", Some("a")),
            ("a not in (1, 2)", None),
            ("a between symmetric 20 and 10", None),
            ("a < 3", Some("a")),
        ] {
            let p = pg_query::parse(&format!("select * from k where {sql}")).unwrap();
            let Some(N::SelectStmt(s)) = p.protobuf.stmts[0].stmt.as_ref().unwrap().node.as_ref()
            else {
                panic!()
            };
            let got = explain_conjuncts(s.where_clause.as_deref().unwrap(), &def).unwrap();
            assert_eq!(got[0].1.as_deref(), want, "{sql}");
        }
    }

    /// A BETWEEN is two quals of the scan's one list, each constraining its
    /// column, and a nested AND is flattened, as PostgreSQL 15.19 prints a
    /// Filter; NOT BETWEEN and BETWEEN SYMMETRIC are each one OR (RP-399).
    #[test]
    fn a_between_is_two_quals() {
        let def = TableDef::new(
            "e",
            vec![
                Column::new("a", "int4", false),
                Column::new("b", "text", false),
            ],
        );
        let a = || Some("a".to_string());
        for (sql, want) in [
            (
                "a between 15 and 55 and b <> 'v3'",
                vec![
                    ("(a >= 15)", a()),
                    ("(a <= 55)", a()),
                    ("(b <> 'v3'::text)", None),
                ],
            ),
            (
                "(a between 1 and 2 and a <> 9) and a < 5",
                vec![
                    ("(a >= 1)", a()),
                    ("(a <= 2)", a()),
                    ("(a <> 9)", None),
                    ("(a < 5)", a()),
                ],
            ),
            (
                "a not between 1 and 2",
                vec![("((a < 1) OR (a > 2))", None)],
            ),
            (
                "a between symmetric 2 and 1",
                vec![("(((a >= 2) AND (a <= 1)) OR ((a >= 1) AND (a <= 2)))", None)],
            ),
        ] {
            let p = pg_query::parse(&format!("select * from e where {sql}")).unwrap();
            let Some(N::SelectStmt(s)) = p.protobuf.stmts[0].stmt.as_ref().unwrap().node.as_ref()
            else {
                panic!()
            };
            let got = explain_conjuncts(s.where_clause.as_deref().unwrap(), &def).unwrap();
            let want: Vec<(String, Option<String>)> =
                want.into_iter().map(|(t, c)| (t.to_string(), c)).collect();
            assert_eq!(got, want, "{sql}");
        }
    }
}
#[cfg(test)]
mod folded_casts {
    use super::*;
    use secantus_pgcatalog::Column;

    /// A cast of an integer constant prints as the folded constant of the
    /// target type, as PostgreSQL 15.19's EXPLAIN does (RP-370).
    #[test]
    fn a_cast_constant_prints_folded() {
        let def = TableDef::new(
            "ud",
            vec![
                Column::new("a", "int4[]", false),
                Column::new("p", "int4", false),
                Column::new("n", "text", false),
            ],
        );
        for (sql, want) in [
            ("1::bigint = any(a)", "('1'::bigint = ANY (a))"),
            ("p = 1::int2", "(p = '1'::smallint)"),
            ("p::bigint = 1::bigint", "((p)::bigint = '1'::bigint)"),
            ("p::numeric = 1::numeric", "((p)::numeric = '1'::numeric)"),
            ("n = 1::text", "(n = '1'::text)"),
            ("p = 7::int4", "(p = 7)"),
        ] {
            let p = pg_query::parse(&format!("select * from ud where {sql}")).unwrap();
            let Some(N::SelectStmt(s)) = p.protobuf.stmts[0].stmt.as_ref().unwrap().node.as_ref()
            else {
                panic!()
            };
            let got = explain_conjuncts(s.where_clause.as_deref().unwrap(), &def).unwrap();
            assert_eq!(got[0].0, want, "{sql}");
        }
    }
}

#[cfg(test)]
mod comma_join_conds_tests {
    use super::*;

    fn stmt(sql: &str) -> pg_query::protobuf::SelectStmt {
        let p = pg_query::parse(sql).unwrap();
        let Some(N::SelectStmt(s)) = p.protobuf.stmts[0].stmt.as_ref().unwrap().node.as_ref()
        else {
            panic!("{sql}")
        };
        (**s).clone()
    }

    fn q(text: &str, home: Option<&str>) -> JoinQual {
        (text.to_string(), home.map(str::to_string), None)
    }

    #[test]
    fn a_comma_joins_equalities_become_its_join_condition() {
        // RP-372: `FROM e, k WHERE e.b = k.b AND k.a = 5` -- the equality
        // is the join's clause, the constant stays on k's scan.
        let s = stmt("select * from e, k where e.b = k.b and k.a = 5");
        let mut quals = vec![q("(e.b = k.b)", None), q("(a = 5)", Some("k"))];
        assert_eq!(
            comma_join_conds(&s, &mut quals).unwrap(),
            vec!["(e.b = k.b)"]
        );
        assert_eq!(quals, vec![q("(a = 5)", Some("k"))]);
        // Two equalities are one condition, ANDed, as an ON's prints.
        let s = stmt("select * from e, k where e.b = k.b and e.a = k.a");
        let mut quals = vec![q("(e.b = k.b)", None), q("(e.a = k.a)", None)];
        assert_eq!(
            comma_join_conds(&s, &mut quals).unwrap(),
            vec!["((e.b = k.b) AND (e.a = k.a))"]
        );
        assert!(quals.is_empty());
    }

    #[test]
    fn anything_else_is_left_as_the_join_filter() {
        // RP-375: an inequality over both tables stays the join's filter;
        // only the equality is its clause.
        let s = stmt("select * from e, k where e.b = k.b and e.a < k.a");
        let mut quals = vec![q("(e.b = k.b)", None), q("(e.a < k.a)", None)];
        assert_eq!(
            comma_join_conds(&s, &mut quals).unwrap(),
            vec!["(e.b = k.b)"]
        );
        assert_eq!(quals, vec![q("(e.a < k.a)", None)]);
        // The filter is ordered cheapest first (`order_qual_clauses`).
        let s = stmt("select * from e, k where e.b = k.b and e.a + 1 < k.a and e.a > k.a");
        let mut quals = vec![
            q("(e.b = k.b)", None),
            q("((e.a + 1) < k.a)", None),
            q("(e.a > k.a)", None),
        ];
        assert_eq!(
            comma_join_conds(&s, &mut quals).unwrap(),
            vec!["(e.b = k.b)"]
        );
        assert_eq!(
            quals,
            vec![q("(e.a > k.a)", None), q("((e.a + 1) < k.a)", None)]
        );
        // No equality over both tables: no clause is moved.
        let s = stmt("select * from e, k where e.a < k.a");
        let mut quals = vec![q("(e.a < k.a)", None)];
        let before = quals.clone();
        assert!(comma_join_conds(&s, &mut quals).unwrap().is_empty());
        assert_eq!(quals, before);
        // Three relations: more than one join node to place it on.
        let s = stmt("select * from e, k, m where e.b = k.b");
        let mut quals = vec![q("(e.b = k.b)", None)];
        assert!(comma_join_conds(&s, &mut quals).unwrap().is_empty());
        assert_eq!(quals.len(), 1);
    }
}

#[cfg(test)]
mod join_where_tests {
    use super::*;
    use secantus_pgcatalog::Column;

    fn lookup(name: &str) -> Option<TableDef> {
        matches!(name, "e" | "s").then(|| {
            TableDef::new(
                name,
                vec![
                    Column::new("a", "int4", false),
                    Column::new(if name == "e" { "b" } else { "c" }, "text", false),
                ],
            )
        })
    }

    fn homes(sql: &str) -> Vec<(String, Option<String>)> {
        let p = pg_query::parse(sql).unwrap();
        let Some(N::SelectStmt(s)) = p.protobuf.stmts[0].stmt.as_ref().unwrap().node.as_ref()
        else {
            panic!("{sql}")
        };
        let none = |_: &str| None;
        let cat = Catalog {
            lookup: &lookup,
            view_sql: &none,
        };
        join_where(s, &cat)
            .unwrap()
            .into_iter()
            .map(|(text, home, _)| (text, home))
            .collect()
    }

    #[test]
    fn a_where_over_an_expression_reduces_the_left_join() {
        // PostgreSQL 15.19 (RP-405): `(s.a * 2) > 4` is false or NULL for
        // a NULL-extended s, so the join is an inner one and the conjunct
        // filters s's scan, as the planner's `strict_aliases` decides.
        for (sql, want) in [
            (
                "select * from e left join s on s.a = e.a where s.a * 2 > 4",
                ("((a * 2) > 4)", Some("s")),
            ),
            (
                "select * from e left join s on s.a = e.a where s.a::numeric > 4",
                ("((a)::numeric > 4)", Some("s")),
            ),
        ] {
            let got = homes(sql);
            assert_eq!(got.len(), 1, "{sql}");
            assert_eq!(got[0].1.as_deref(), want.1, "{sql}");
        }
    }

    #[test]
    fn a_where_that_proves_nothing_keeps_the_side_nullable() {
        // `IS NULL` holds for a NULL-extended row: s stays null-extended,
        // its conjunct the join's.
        let got = homes("select * from e left join s on s.a = e.a where s.a is null");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, None);
    }
}
