//! What `fmgr_sql_validator` checks in a `LANGUAGE sql` body at CREATE
//! beyond the names and types planning resolves (RP-500):
//!
//! - a string constant parse analysis coerces runs its type's input
//!   function there (`coerce_type` over an unknown-typed `Const`), so
//!   `select 'x'::int` is 22P02 at the literal;
//! - `transformCallStmt` resolves a CALL's procedure;
//! - `check_sql_fn_retval` holds the final statement's result to the
//!   declared return type (42P13).
//!
//! The parse-tree walks are here; the handler's side of each check is in
//! the `impl PgHandler` below, called from `check_sql_function_body`.

use bson::{Bson, Document};
use pg_query::protobuf::{self as pb, node::Node as N};
use pg_query::NodeRef;
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};

use crate::catalog_facts::{builtin, is_polymorphic, ANY, UNKNOWN};
use crate::PgHandler;

const RECORD: i64 = 2249;
const VOID: i64 = 2278;

/// A string constant of a statement: its byte offset and its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Literal {
    pub(crate) at: usize,
    pub(crate) value: String,
}

fn string_const(node: &pb::Node) -> Option<Literal> {
    match node.node.as_ref() {
        Some(N::AConst(c)) => match c.val.as_ref() {
            Some(pb::a_const::Val::Sval(s)) => Some(Literal {
                at: usize::try_from(c.location).ok()?,
                value: s.sval.clone(),
            }),
            _ => None,
        },
        _ => None,
    }
}

fn type_name_parts(tn: &pb::TypeName) -> Vec<String> {
    tn.names
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// `SELECT <expr>`, as libpg_query deparses it.
fn deparse_select(expr: pb::Node) -> Option<String> {
    let select = pb::Node {
        node: Some(N::SelectStmt(Box::new(pb::SelectStmt {
            target_list: vec![pb::Node {
                node: Some(N::ResTarget(Box::new(pb::ResTarget {
                    val: Some(Box::new(expr)),
                    location: -1,
                    ..Default::default()
                }))),
            }],
            limit_option: pb::LimitOption::Default as i32,
            op: pb::SetOperation::SetopNone as i32,
            ..Default::default()
        }))),
    };
    select.deparse().ok()
}

/// Each string constant a cast reads directly (`'x'::int`, `int4 'x'`,
/// `cast('x' as int)`), in the order written, with the SELECT that runs
/// the cast type's input function over it. `coerce_type` passes the input
/// function a typmod of -1 (but for `interval`, whose input obeys its
/// field restriction), and a domain's constraints are checked only when
/// the expression runs: so the probe drops the typmod, and its caller
/// counts only an error the input function raises.
pub(crate) fn literal_casts(stmt: &str) -> Vec<(Literal, String)> {
    let Ok(tree) = pg_query::parse(stmt) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (node, _, _, _) in tree.protobuf.nodes() {
        let NodeRef::TypeCast(tc) = node else {
            continue;
        };
        let (Some(arg), Some(tn)) = (tc.arg.as_deref(), tc.type_name.as_ref()) else {
            continue;
        };
        let Some(lit) = string_const(arg) else {
            continue;
        };
        if tn.pct_type || tn.setof {
            continue;
        }
        let mut tn = tn.clone();
        if type_name_parts(&tn).last().map(String::as_str) != Some("interval") {
            tn.typmods.clear();
        }
        tn.location = -1;
        let cast = pb::Node {
            node: Some(N::TypeCast(Box::new(pb::TypeCast {
                arg: Some(Box::new(arg.clone())),
                type_name: Some(tn),
                location: -1,
            }))),
        };
        if let Some(sql) = deparse_select(cast) {
            out.push((lit, sql));
        }
    }
    out.sort_by_key(|(l, _)| l.at);
    out
}

/// The string constants written as an operator's operand (`a = 'q'`, `'a'
/// = 1`, `1 in ('z')`), whose type the operator's resolution gives them.
pub(crate) fn operand_literals(stmt: &str) -> Vec<Literal> {
    let Ok(tree) = pg_query::parse(stmt) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (node, _, _, _) in tree.protobuf.nodes() {
        let NodeRef::AExpr(e) = node else {
            continue;
        };
        for side in [e.lexpr.as_deref(), e.rexpr.as_deref()]
            .into_iter()
            .flatten()
        {
            match side.node.as_ref() {
                Some(N::List(l)) => out.extend(l.items.iter().filter_map(string_const)),
                _ => out.extend(string_const(side)),
            }
        }
    }
    out.sort_by_key(|l| l.at);
    out
}

/// A `CALL`'s procedure as written: its name, where the name is, each
/// argument that is a string constant, and the name each named one gives.
pub(crate) struct CallTarget {
    pub(crate) name: String,
    pub(crate) at: usize,
    pub(crate) literals: Vec<Option<Literal>>,
    /// The name each `name => value` argument gives.
    pub(crate) names: Vec<Option<String>>,
}

pub(crate) fn call_target(stmt: &str) -> Option<CallTarget> {
    let tree = pg_query::parse(stmt).ok()?;
    let [raw] = tree.protobuf.stmts.as_slice() else {
        return None;
    };
    let Some(N::CallStmt(c)) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
        return None;
    };
    let f = c.funccall.as_ref()?;
    let name = f
        .funcname
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(".");
    Some(CallTarget {
        name,
        at: usize::try_from(f.location).ok()?,
        literals: f
            .args
            .iter()
            .map(|a| match a.node.as_ref() {
                Some(N::NamedArgExpr(na)) => na.arg.as_deref().and_then(string_const),
                _ => string_const(a),
            })
            .collect(),
        names: f
            .args
            .iter()
            .map(|a| match a.node.as_ref() {
                Some(N::NamedArgExpr(na)) => Some(na.name.clone()),
                _ => None,
            })
            .collect(),
    })
}

/// What the final statement of a body returns, for `check_sql_fn_retval`:
/// a SELECT's target list, or an INSERT / UPDATE / DELETE's RETURNING
/// list; `false` for anything else (a utility statement, a write without
/// RETURNING, MERGE).
pub(crate) fn returns_rows(stmt: &str) -> bool {
    let Ok(tree) = pg_query::parse(stmt) else {
        return false;
    };
    let Some(raw) = tree.protobuf.stmts.last() else {
        return false;
    };
    match raw.stmt.as_ref().and_then(|s| s.node.as_ref()) {
        Some(N::SelectStmt(_)) => true,
        Some(N::InsertStmt(s)) => !s.returning_list.is_empty(),
        Some(N::UpdateStmt(s)) => !s.returning_list.is_empty(),
        Some(N::DeleteStmt(s)) => !s.returning_list.is_empty(),
        _ => false,
    }
}

/// `ERRCODE_INVALID_FUNCTION_DEFINITION` as `check_sql_fn_retval` raises
/// it, framed by the routine (`sql_function_parse_error_callback`).
fn retval_mismatch(fname: &str, declared: &str, detail: String) -> PgWireError {
    let mut info = ErrorInfo::new(
        "ERROR".into(),
        "42P13".into(),
        format!("return type mismatch in function declared to return {declared}"),
    );
    info.detail = Some(detail);
    info.where_context = Some(format!("SQL function \"{fname}\""));
    PgWireError::UserError(Box::new(info))
}

/// A declared result as `get_func_result_type` gives it.
enum Declared {
    /// Not checked: `void`, a polymorphic or pseudo type, a `record`
    /// without OUT parameters, or a type this server cannot place.
    Unchecked,
    Scalar(i64),
    /// A composite: its type (`record` for OUT parameters) and each
    /// attribute's type.
    Composite(i64, Vec<i64>),
}

impl PgHandler {
    /// The type `name`'s oid, a domain's being its base type's (as
    /// `getBaseType` resolves it for a coercion).
    fn validator_type_oid(&self, name: &str) -> Option<i64> {
        let name = name.trim();
        if let Some(d) = secantus_pgplan::domains::user_domain(name) {
            return self.type_oid_by_name(&d.base);
        }
        self.type_oid_by_name(name)
    }

    /// Whether `t` is a composite type: a builtin one, a user composite or
    /// a table's row type.
    fn validator_is_complex(&self, t: i64) -> bool {
        if t == RECORD {
            return true;
        }
        if let Some(f) = builtin().types.get(&t) {
            return f.typtype == 'c';
        }
        self.composites()
            .is_ok_and(|cs| cs.iter().any(|(_, o, _)| *o == t))
    }

    /// `can_coerce_type` of one type at `COERCION_ASSIGNMENT`, as
    /// `coerce_fn_result_column` asks it. A user type it cannot place is
    /// taken as coercible, so that no function PostgreSQL accepts is
    /// refused.
    pub(crate) fn assignment_coercible(&self, src: i64, tgt: i64) -> bool {
        if src == tgt || tgt == ANY || is_polymorphic(tgt) || src == UNKNOWN {
            return true;
        }
        let b = builtin();
        let base = |t: i64| match b.types.get(&t) {
            Some(f) if f.typtype == 'd' && f.basetype != 0 => f.basetype,
            _ => t,
        };
        let (s, t) = (base(src), base(tgt));
        if s == t {
            return true;
        }
        if (t == RECORD && self.validator_is_complex(s))
            || (s == RECORD && self.validator_is_complex(t))
        {
            return true;
        }
        if let Some((_, context, _)) = b.casts.get(&(s, t)) {
            return matches!(context, 'i' | 'a');
        }
        if self
            .user_casts()
            .iter()
            .any(|c| c.source_oid == s && c.target_oid == t && matches!(c.context, 'i' | 'a'))
        {
            return true;
        }
        // A type this server cannot place (a user array, a base type) is
        // not judged.
        let placed = |o: i64| b.types.contains_key(&o) || self.placed_user_type(o);
        if !placed(s) || !placed(t) {
            return true;
        }
        match (b.types.get(&s), b.types.get(&t)) {
            // An array coerces element by element (`ArrayCoerceExpr`).
            (Some(sf), Some(tf))
                if sf.elem != 0 && tf.elem != 0 && sf.category == 'A' && tf.category == 'A' =>
            {
                self.assignment_coercible(sf.elem, tf.elem)
            }
            // Any value reaches a string type through its output function.
            (_, Some(tf)) => tf.category == 'S',
            // A user enum, composite or range is reached only by a cast.
            (_, None) => false,
        }
    }

    /// Whether `t` is a user type this server can name: a composite, an
    /// enum or a range, whose coercions are all in `pg_cast` or none.
    fn placed_user_type(&self, t: i64) -> bool {
        self.composites()
            .is_ok_and(|cs| cs.iter().any(|(_, o, _)| *o == t))
            || self
                .enums()
                .is_ok_and(|es| es.iter().any(|(_, o, _)| *o == t))
            || self
                .ranges()
                .is_ok_and(|rs| rs.iter().any(|(_, o, _)| *o == t))
    }

    /// `format_type_be` of a result column's type.
    fn validator_format_type(&self, oid: i64) -> String {
        let b = builtin();
        if let Some(f) = b.types.get(&oid) {
            if f.category == 'A' && f.elem != 0 {
                return format!("{}[]", self.validator_format_type(f.elem));
            }
            return secantus_pgplan::display_type(&f.name);
        }
        if let Ok(oid) = u32::try_from(oid) {
            if let Some(name) = self.user_type_name_for_oid(oid) {
                return name;
            }
        }
        oid.to_string()
    }

    /// The declared result of a routine document, as `get_func_result_type`
    /// resolves it: a single OUT parameter is its type, several are a
    /// `record` of them, and a named composite or table type is its
    /// attributes.
    fn declared_result(&self, doc: &Document) -> (Declared, String) {
        let tag = doc.get_str("return_tag").unwrap_or_default().trim();
        let shown = secantus_pgplan::display_type(tag);
        if doc.get_bool("returns_trigger").unwrap_or(false) {
            return (Declared::Unchecked, shown);
        }
        let columns: Vec<String> = doc
            .get_array("table_columns")
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.as_document())
                    .map(|c| c.get_str("type_tag").unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        if tag.eq_ignore_ascii_case("record") {
            if columns.len() < 2 {
                return (Declared::Unchecked, shown);
            }
            let attrs: Option<Vec<i64>> =
                columns.iter().map(|t| self.validator_type_oid(t)).collect();
            return match attrs {
                Some(attrs) => (Declared::Composite(RECORD, attrs), shown),
                None => (Declared::Unchecked, shown),
            };
        }
        let Some(oid) = self.validator_type_oid(tag) else {
            return (Declared::Unchecked, shown);
        };
        if oid == VOID || is_polymorphic(oid) || oid == ANY {
            return (Declared::Unchecked, shown);
        }
        if let Some(f) = builtin().types.get(&oid) {
            return match f.typtype {
                'b' | 'd' | 'e' | 'r' | 'm' => (Declared::Scalar(oid), shown),
                _ => (Declared::Unchecked, shown),
            };
        }
        // A user composite, or a table's row type.
        let fields: Option<Vec<String>> = self
            .composites()
            .ok()
            .and_then(|cs| cs.iter().find(|(_, o, _)| *o == oid).cloned())
            .map(|(_, _, fields)| fields.into_iter().map(|(_, t)| t).collect())
            .or_else(|| {
                self.lookup(tag)
                    .map(|t| t.columns.iter().map(|c| c.pg_type.clone()).collect())
            });
        match fields {
            Some(fields) => {
                let attrs: Option<Vec<i64>> =
                    fields.iter().map(|t| self.validator_type_oid(t)).collect();
                match attrs {
                    Some(attrs) => (Declared::Composite(oid, attrs), shown),
                    None => (Declared::Unchecked, shown),
                }
            }
            // An enum, a range or a domain is a scalar; a user type this
            // server cannot place is not checked.
            None if self.placed_user_type(oid) => (Declared::Scalar(oid), shown),
            None => (Declared::Unchecked, shown),
        }
    }

    /// `check_sql_fn_retval`: the final statement `last` (as written) and
    /// its result columns' types, `None` when it could not be described,
    /// against the routine's declared result.
    pub(crate) fn check_sql_fn_retval(
        &self,
        fname: &str,
        doc: &Document,
        last: Option<&str>,
        columns: Option<&[i64]>,
    ) -> PgWireResult<()> {
        let (declared, shown) = self.declared_result(doc);
        if matches!(declared, Declared::Unchecked) {
            return Ok(());
        }
        if !last.is_some_and(returns_rows) {
            return Err(retval_mismatch(
                fname,
                &shown,
                "Function's final statement must be SELECT or INSERT/UPDATE/DELETE RETURNING."
                    .into(),
            ));
        }
        let Some(columns) = columns else {
            return Ok(());
        };
        match declared {
            Declared::Unchecked => Ok(()),
            Declared::Scalar(rettype) => {
                let [one] = columns else {
                    return Err(retval_mismatch(
                        fname,
                        &shown,
                        "Final statement must return exactly one column.".into(),
                    ));
                };
                if self.assignment_coercible(*one, rettype) {
                    return Ok(());
                }
                Err(retval_mismatch(
                    fname,
                    &shown,
                    format!(
                        "Actual return type is {}.",
                        self.validator_format_type(*one)
                    ),
                ))
            }
            Declared::Composite(rettype, attrs) => {
                // One column of (or coercible to) the composite is the
                // whole result.
                if let [one] = columns {
                    if self.assignment_coercible(*one, rettype) {
                        return Ok(());
                    }
                }
                for (i, col) in columns.iter().enumerate() {
                    let Some(attr) = attrs.get(i) else {
                        return Err(retval_mismatch(
                            fname,
                            &shown,
                            "Final statement returns too many columns.".into(),
                        ));
                    };
                    if !self.assignment_coercible(*col, *attr) {
                        return Err(retval_mismatch(
                            fname,
                            &shown,
                            format!(
                                "Final statement returns {} instead of {} at column {}.",
                                self.validator_format_type(*col),
                                self.validator_format_type(*attr),
                                i + 1
                            ),
                        ));
                    }
                }
                if columns.len() < attrs.len() {
                    return Err(retval_mismatch(
                        fname,
                        &shown,
                        "Final statement returns too few columns.".into(),
                    ));
                }
                Ok(())
            }
        }
    }

    /// `transformCallStmt`'s resolution of a CALL in a body: the error the
    /// procedure's lookup raises ([`Self::resolve_call`]), at the name.
    /// Then each string constant passed for an input parameter is read as
    /// that parameter's type, as `coerce_type` does, its input error at the
    /// constant.
    pub(crate) fn check_call_in_body(
        &self,
        call: &CallTarget,
        arg_types: &[String],
    ) -> Option<(PgWireError, usize)> {
        let strings = |d: &Document, key: &str| -> Vec<String> {
            d.get_array(key)
                .map(|a| {
                    a.iter()
                        .map(|v| v.as_str().unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default()
        };
        let (found, numbers) = match self.resolve_call(&call.name, arg_types, &call.names) {
            Ok(found) => found,
            Err(PgWireError::UserError(mut info)) => {
                info.position = None;
                return Some((PgWireError::UserError(info), call.at));
            }
            Err(e) => return Some((e, call.at)),
        };
        let found = &found;
        let types = strings(found, "param_types");
        let modes = strings(found, "param_modes");
        let tz = self.session_timezone();
        for (k, lit) in call.literals.iter().enumerate() {
            let i = numbers.get(k).copied().unwrap_or(k);
            let (Some(lit), Some(ty)) = (lit, types.get(i)) else {
                continue;
            };
            if modes.get(i).is_some_and(|m| m == "o") {
                continue;
            }
            if let Err(e) =
                secantus_pgplan::cast_value_with_tz(Bson::String(lit.value.clone()), ty, &tz)
            {
                let err = Self::err(&e);
                if is_input_error(&err) {
                    return Some((err, lit.at));
                }
            }
        }
        None
    }
}

/// Whether `e` is an error a type's input function raises over a
/// constant: class 22 (invalid text, out of range) or, for the `reg*`
/// types that look a name up, class 42.
pub(crate) fn is_input_error(e: &PgWireError) -> bool {
    match e {
        PgWireError::UserError(info) => {
            (info.code.starts_with("22") || info.code.starts_with("42")) && info.code != "42501"
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cast_literal_probes_its_type_without_the_modifier() {
        let got =
            literal_casts("select 'x'::int, int4 'y', cast('z' as numeric(4,2)), 'w'::text::int");
        let probes: Vec<(usize, &str, &str)> = got
            .iter()
            .map(|(l, s)| (l.at, l.value.as_str(), s.as_str()))
            .collect();
        assert_eq!(probes.len(), 4, "{probes:?}");
        assert_eq!(probes[0].0, 7);
        assert_eq!(probes[0].1, "x");
        assert!(
            probes[0].2.to_ascii_lowercase().contains("'x'::int"),
            "{probes:?}"
        );
        assert_eq!(probes[1].1, "y");
        assert_eq!(probes[2].1, "z");
        assert!(!probes[2].2.contains('4'), "{probes:?}");
        // `'w'::text` is the cast that reads the constant; the outer cast
        // reads an expression.
        assert_eq!(probes[3].1, "w");
        assert!(
            probes[3].2.to_ascii_lowercase().contains("::text"),
            "{probes:?}"
        );
    }

    #[test]
    fn an_interval_keeps_its_field_restriction() {
        let got = literal_casts("select interval '1' hour");
        assert_eq!(got.len(), 1);
        assert!(got[0].1.to_ascii_lowercase().contains("hour"), "{got:?}");
    }

    #[test]
    fn operand_literals_are_an_operators_bare_constants() {
        let got =
            operand_literals("select a from fe where a = 'q' and 1 in ('z', 'y') and 'x'::int = 1");
        let values: Vec<&str> = got.iter().map(|l| l.value.as_str()).collect();
        assert_eq!(values, ["q", "z", "y"]);
    }

    #[test]
    fn a_call_target_is_its_name_and_arguments() {
        let c = call_target("call s.pp('a', 2)").unwrap();
        assert_eq!(c.name, "s.pp");
        assert_eq!(c.at, 5);
        assert_eq!(c.literals[0].as_ref().map(|l| l.value.as_str()), Some("a"));
        assert!(c.literals[1].is_none());
        assert_eq!(c.names, [None, None]);
        let named = call_target("call pp(1, x => 'b')").unwrap();
        assert_eq!(named.names, [None, Some("x".to_string())]);
        assert_eq!(
            named.literals[1].as_ref().map(|l| l.value.as_str()),
            Some("b")
        );
        assert!(call_target("select 1").is_none());
    }

    #[test]
    fn only_a_select_or_a_returning_write_returns_rows() {
        assert!(returns_rows("select 1"));
        assert!(returns_rows("values (1)"));
        assert!(returns_rows("with w as (select 1) select * from w"));
        assert!(returns_rows("insert into t values (1) returning a"));
        assert!(!returns_rows("insert into t values (1)"));
        assert!(!returns_rows("create table t (a int)"));
        assert!(!returns_rows("call pp(1)"));
        assert!(!returns_rows(""));
    }
}
