//! `nodeFuncs.c`: `exprType`, `exprTypmod`, `exprCollation`,
//! `exprLocation` and the tree walks the parser leans on, plus
//! `rewriteManip.c`'s `OffsetVarNodes` and `rangeTableEntry_used`.

use super::catalog::*;
use super::nodes::*;

pub(crate) fn expr_type(cat: &Catalog, n: &Node) -> Oid {
    match n {
        Node::Var(v) => v.vartype,
        Node::Const(c) => c.consttype,
        Node::Param(p) => p.paramtype,
        Node::Aggref(a) => a.aggtype,
        Node::GroupingFunc(_) => INT4OID,
        Node::WindowFunc(w) => w.wintype,
        Node::SubscriptingRef(r) => r.refrestype,
        Node::FuncExpr(f) => f.funcresulttype,
        Node::NamedArgExpr(na) => expr_type(cat, &na.arg),
        Node::OpExpr(o) => o.opresulttype,
        Node::ScalarArrayOpExpr(_) | Node::BoolExpr(_) => BOOLOID,
        Node::SubLink(s) => match s.sub_link_type {
            SubLinkType::Expr | SubLinkType::Array => {
                let t = s
                    .subselect
                    .target_list
                    .first()
                    .map_or(0, |t| expr_type(cat, &t.expr));
                if s.sub_link_type == SubLinkType::Array {
                    cat.promoted_array_type(t)
                } else {
                    t
                }
            }
            SubLinkType::MultiExpr => RECORDOID,
            _ => BOOLOID,
        },
        Node::FieldSelect(f) => f.resulttype,
        Node::RelabelType(r) => r.resulttype,
        Node::CoerceViaIO(c) => c.resulttype,
        Node::ArrayCoerceExpr(c) => c.resulttype,
        Node::ConvertRowtypeExpr(c) => c.resulttype,
        Node::CollateExpr(c) => expr_type(cat, &c.arg),
        Node::CaseExpr(c) => c.casetype,
        Node::CaseTestExpr(c) => c.type_id,
        Node::ArrayExpr(a) => a.array_typeid,
        Node::RowExpr(r) => r.row_typeid,
        Node::RowCompareExpr(_) => BOOLOID,
        Node::CoalesceExpr(c) => c.coalescetype,
        Node::MinMaxExpr(m) => m.minmaxtype,
        Node::SqlValueFunction(f) => f.type_,
        Node::NullTest(_) | Node::BooleanTest(_) => BOOLOID,
        Node::CoerceToDomain(c) => c.resulttype,
        Node::SetToDefault(d) => d.type_id,
        Node::TargetEntry(t) => expr_type(cat, &t.expr),
    }
}

/// `exprIsLengthCoercion`.
fn is_length_coercion(n: &Node) -> Option<i32> {
    match n {
        Node::FuncExpr(f) => {
            if f.funcformat != CoercionForm::ExplicitCast
                && f.funcformat != CoercionForm::ImplicitCast
            {
                return None;
            }
            if f.args.len() < 2 || f.args.len() > 3 {
                return None;
            }
            match &f.args[1] {
                Node::Const(Const {
                    consttype: INT4OID,
                    constvalue: Some(Datum::ByVal { bytes, .. }),
                    ..
                }) => Some(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
                _ => None,
            }
        }
        Node::ArrayCoerceExpr(c) if c.resulttypmod >= 0 => Some(c.resulttypmod),
        _ => None,
    }
}

pub(crate) fn expr_typmod(cat: &Catalog, n: &Node) -> i32 {
    match n {
        Node::Var(v) => v.vartypmod,
        Node::Const(c) => c.consttypmod,
        Node::Param(p) => p.paramtypmod,
        Node::SubscriptingRef(r) => r.reftypmod,
        Node::FuncExpr(_) => is_length_coercion(n).unwrap_or(-1),
        Node::NamedArgExpr(na) => expr_typmod(cat, &na.arg),
        Node::OpExpr(o) if o.kind == OpKind::NullIf => expr_typmod(cat, &o.args[0]),
        Node::SubLink(s) => match s.sub_link_type {
            SubLinkType::Expr | SubLinkType::Array => s
                .subselect
                .target_list
                .first()
                .map_or(-1, |t| expr_typmod(cat, &t.expr)),
            _ => -1,
        },
        Node::FieldSelect(f) => f.resulttypmod,
        Node::RelabelType(r) => r.resulttypmod,
        Node::ArrayCoerceExpr(c) => c.resulttypmod,
        Node::CollateExpr(c) => expr_typmod(cat, &c.arg),
        Node::CaseExpr(c) => {
            if expr_type(cat, &c.defresult) != c.casetype {
                return -1;
            }
            let m = expr_typmod(cat, &c.defresult);
            if m < 0 {
                return -1;
            }
            for w in &c.args {
                if expr_type(cat, &w.result) != c.casetype || expr_typmod(cat, &w.result) != m {
                    return -1;
                }
            }
            m
        }
        Node::CaseTestExpr(c) => c.type_mod,
        Node::ArrayExpr(a) => {
            let Some(first) = a.elements.first() else {
                return -1;
            };
            let m = expr_typmod(cat, first);
            if m < 0 {
                return -1;
            }
            let common = if a.multidims {
                a.array_typeid
            } else {
                a.element_typeid
            };
            for e in &a.elements {
                if expr_type(cat, e) != common || expr_typmod(cat, e) != m {
                    return -1;
                }
            }
            m
        }
        Node::CoalesceExpr(c) => same_typmod(cat, &c.args, c.coalescetype),
        Node::MinMaxExpr(m) => same_typmod(cat, &m.args, m.minmaxtype),
        Node::SqlValueFunction(f) => f.typmod,
        Node::CoerceToDomain(c) => c.resulttypmod,
        Node::SetToDefault(d) => d.type_mod,
        Node::TargetEntry(t) => expr_typmod(cat, &t.expr),
        _ => -1,
    }
}

fn same_typmod(cat: &Catalog, args: &[Node], t: Oid) -> i32 {
    let Some(first) = args.first() else {
        return -1;
    };
    if expr_type(cat, first) != t {
        return -1;
    }
    let m = expr_typmod(cat, first);
    if m < 0 {
        return -1;
    }
    for e in &args[1..] {
        if expr_type(cat, e) != t || expr_typmod(cat, e) != m {
            return -1;
        }
    }
    m
}

#[allow(clippy::only_used_in_recursion)]
pub(crate) fn expr_collation(cat: &Catalog, n: &Node) -> Oid {
    match n {
        Node::Var(v) => v.varcollid,
        Node::Const(c) => c.constcollid,
        Node::Param(p) => p.paramcollid,
        Node::Aggref(a) => a.aggcollid,
        Node::GroupingFunc(_) => 0,
        Node::WindowFunc(w) => w.wincollid,
        Node::SubscriptingRef(r) => r.refcollid,
        Node::FuncExpr(f) => f.funccollid,
        Node::NamedArgExpr(na) => expr_collation(cat, &na.arg),
        Node::OpExpr(o) => o.opcollid,
        Node::ScalarArrayOpExpr(_) | Node::BoolExpr(_) => 0,
        Node::SubLink(s) => match s.sub_link_type {
            SubLinkType::Expr | SubLinkType::Array => s
                .subselect
                .target_list
                .first()
                .map_or(0, |t| expr_collation(cat, &t.expr)),
            _ => 0,
        },
        Node::FieldSelect(f) => f.resultcollid,
        Node::RelabelType(r) => r.resultcollid,
        Node::CoerceViaIO(c) => c.resultcollid,
        Node::ArrayCoerceExpr(c) => c.resultcollid,
        Node::ConvertRowtypeExpr(_) => 0,
        Node::CollateExpr(c) => c.coll_oid,
        Node::CaseExpr(c) => c.casecollid,
        Node::CaseTestExpr(c) => c.collation,
        Node::ArrayExpr(a) => a.array_collid,
        Node::RowExpr(_) | Node::RowCompareExpr(_) => 0,
        Node::CoalesceExpr(c) => c.coalescecollid,
        Node::MinMaxExpr(m) => m.minmaxcollid,
        Node::SqlValueFunction(f) => {
            if f.type_ == NAMEOID {
                C_COLLATION_OID
            } else {
                0
            }
        }
        Node::NullTest(_) | Node::BooleanTest(_) => 0,
        Node::CoerceToDomain(c) => c.resultcollid,
        Node::SetToDefault(d) => d.collation,
        Node::TargetEntry(t) => expr_collation(cat, &t.expr),
    }
}

/// `exprSetCollation`.
pub(crate) fn set_collation(n: &mut Node, c: Oid) {
    match n {
        Node::Var(v) => v.varcollid = c,
        Node::Const(k) => k.constcollid = c,
        Node::Param(p) => p.paramcollid = c,
        Node::Aggref(a) => a.aggcollid = c,
        Node::WindowFunc(w) => w.wincollid = c,
        Node::SubscriptingRef(r) => r.refcollid = c,
        Node::FuncExpr(f) => f.funccollid = c,
        Node::OpExpr(o) => o.opcollid = c,
        Node::FieldSelect(f) => f.resultcollid = c,
        Node::RelabelType(r) => r.resultcollid = c,
        Node::CoerceViaIO(k) => k.resultcollid = c,
        Node::ArrayCoerceExpr(k) => k.resultcollid = c,
        Node::CaseExpr(k) => k.casecollid = c,
        Node::ArrayExpr(a) => a.array_collid = c,
        Node::CoalesceExpr(k) => k.coalescecollid = c,
        Node::MinMaxExpr(m) => m.minmaxcollid = c,
        Node::CoerceToDomain(k) => k.resultcollid = c,
        Node::SetToDefault(d) => d.collation = c,
        _ => {}
    }
}

/// `exprSetInputCollation`.
pub(crate) fn set_input_collation(n: &mut Node, c: Oid) {
    match n {
        Node::Aggref(a) => a.inputcollid = c,
        Node::WindowFunc(w) => w.inputcollid = c,
        Node::FuncExpr(f) => f.inputcollid = c,
        Node::OpExpr(o) => o.inputcollid = c,
        Node::ScalarArrayOpExpr(o) => o.inputcollid = c,
        Node::MinMaxExpr(m) => m.inputcollid = c,
        _ => {}
    }
}

fn leftmost(a: i32, b: i32) -> i32 {
    if a < 0 {
        b
    } else if b < 0 {
        a
    } else {
        a.min(b)
    }
}

fn list_location(l: &[Node]) -> i32 {
    for e in l {
        let x = expr_location(e);
        if x >= 0 {
            return x;
        }
    }
    -1
}

/// `exprLocation`.
pub(crate) fn expr_location(n: &Node) -> i32 {
    match n {
        Node::Var(v) => v.location,
        Node::Const(c) => c.location,
        Node::Param(p) => p.location,
        Node::Aggref(a) => a.location,
        Node::GroupingFunc(g) => g.location,
        Node::WindowFunc(w) => w.location,
        Node::SubscriptingRef(r) => expr_location(&r.refexpr),
        Node::FuncExpr(f) => leftmost(f.location, list_location(&f.args)),
        Node::NamedArgExpr(na) => leftmost(na.location, expr_location(&na.arg)),
        Node::OpExpr(o) => leftmost(o.location, list_location(&o.args)),
        Node::ScalarArrayOpExpr(o) => leftmost(o.location, list_location(&o.args)),
        Node::BoolExpr(b) => leftmost(b.location, list_location(&b.args)),
        Node::SubLink(s) => leftmost(s.testexpr.as_ref().map_or(-1, expr_location), s.location),
        Node::FieldSelect(f) => expr_location(&f.arg),
        Node::RelabelType(r) => leftmost(r.location, expr_location(&r.arg)),
        Node::CoerceViaIO(c) => leftmost(c.location, expr_location(&c.arg)),
        Node::ArrayCoerceExpr(c) => leftmost(c.location, expr_location(&c.arg)),
        Node::ConvertRowtypeExpr(c) => leftmost(c.location, expr_location(&c.arg)),
        Node::CollateExpr(c) => expr_location(&c.arg),
        Node::CaseExpr(c) => c.location,
        Node::CaseTestExpr(_) => -1,
        Node::ArrayExpr(a) => a.location,
        Node::RowExpr(r) => r.location,
        Node::RowCompareExpr(r) => list_location(&r.largs),
        Node::CoalesceExpr(c) => c.location,
        Node::MinMaxExpr(m) => m.location,
        Node::SqlValueFunction(f) => f.location,
        Node::NullTest(t) => leftmost(t.location, expr_location(&t.arg)),
        Node::BooleanTest(b) => leftmost(b.location, expr_location(&b.arg)),
        Node::CoerceToDomain(c) => leftmost(c.location, expr_location(&c.arg)),
        Node::SetToDefault(d) => d.location,
        Node::TargetEntry(t) => expr_location(&t.expr),
    }
}

/// `strip_implicit_coercions`.
pub(crate) fn strip_implicit_coercions(n: &Node) -> &Node {
    match n {
        Node::FuncExpr(f) if f.funcformat == CoercionForm::ImplicitCast && !f.args.is_empty() => {
            strip_implicit_coercions(&f.args[0])
        }
        Node::RelabelType(r) if r.relabelformat == CoercionForm::ImplicitCast => {
            strip_implicit_coercions(&r.arg)
        }
        Node::CoerceViaIO(c) if c.coerceformat == CoercionForm::ImplicitCast => {
            strip_implicit_coercions(&c.arg)
        }
        Node::ArrayCoerceExpr(c) if c.coerceformat == CoercionForm::ImplicitCast => {
            strip_implicit_coercions(&c.arg)
        }
        Node::ConvertRowtypeExpr(c) if c.convertformat == CoercionForm::ImplicitCast => {
            strip_implicit_coercions(&c.arg)
        }
        Node::CoerceToDomain(c) if c.coercionformat == CoercionForm::ImplicitCast => {
            strip_implicit_coercions(&c.arg)
        }
        other => other,
    }
}

/// The direct children `expression_tree_walker` visits (not into
/// sub-queries).
pub(crate) fn children(n: &Node) -> Vec<&Node> {
    let mut v: Vec<&Node> = Vec::new();
    match n {
        Node::Var(_)
        | Node::Const(_)
        | Node::Param(_)
        | Node::CaseTestExpr(_)
        | Node::SqlValueFunction(_)
        | Node::SetToDefault(_) => {}
        Node::Aggref(a) => {
            v.extend(a.aggdirectargs.iter());
            for t in &a.args {
                v.push(&t.expr);
            }
            if let Some(f) = &a.aggfilter {
                v.push(f);
            }
        }
        Node::GroupingFunc(g) => v.extend(g.args.iter()),
        Node::WindowFunc(w) => {
            v.extend(w.args.iter());
            if let Some(f) = &w.aggfilter {
                v.push(f);
            }
        }
        Node::SubscriptingRef(r) => {
            v.extend(r.refupperindexpr.iter().flatten());
            if let Some(l) = &r.reflowerindexpr {
                v.extend(l.iter().flatten());
            }
            v.push(&r.refexpr);
            if let Some(a) = &r.refassgnexpr {
                v.push(a);
            }
        }
        Node::FuncExpr(f) => v.extend(f.args.iter()),
        Node::NamedArgExpr(na) => v.push(&na.arg),
        Node::OpExpr(o) => v.extend(o.args.iter()),
        Node::ScalarArrayOpExpr(o) => v.extend(o.args.iter()),
        Node::BoolExpr(b) => v.extend(b.args.iter()),
        Node::SubLink(s) => {
            if let Some(t) = &s.testexpr {
                v.push(t);
            }
        }
        Node::FieldSelect(f) => v.push(&f.arg),
        Node::RelabelType(r) => v.push(&r.arg),
        Node::CoerceViaIO(c) => v.push(&c.arg),
        Node::ArrayCoerceExpr(c) => {
            v.push(&c.arg);
            v.push(&c.elemexpr);
        }
        Node::ConvertRowtypeExpr(c) => v.push(&c.arg),
        Node::CollateExpr(c) => v.push(&c.arg),
        Node::CaseExpr(c) => {
            if let Some(a) = &c.arg {
                v.push(a);
            }
            for w in &c.args {
                v.push(&w.expr);
                v.push(&w.result);
            }
            v.push(&c.defresult);
        }
        Node::ArrayExpr(a) => v.extend(a.elements.iter()),
        Node::RowExpr(r) => v.extend(r.args.iter()),
        Node::RowCompareExpr(r) => {
            v.extend(r.largs.iter());
            v.extend(r.rargs.iter());
        }
        Node::CoalesceExpr(c) => v.extend(c.args.iter()),
        Node::MinMaxExpr(m) => v.extend(m.args.iter()),
        Node::NullTest(t) => v.push(&t.arg),
        Node::BooleanTest(b) => v.push(&b.arg),
        Node::CoerceToDomain(c) => v.push(&c.arg),
        Node::TargetEntry(t) => v.push(&t.expr),
    }
    v
}

/// `expression_returns_set`.
pub(crate) fn expression_returns_set(n: &Node) -> bool {
    match n {
        Node::FuncExpr(f) if f.funcretset => return true,
        Node::OpExpr(o) if o.opretset => return true,
        Node::Aggref(_) | Node::GroupingFunc(_) | Node::WindowFunc(_) => return false,
        Node::SubLink(_) => return false,
        _ => {}
    }
    children(n).into_iter().any(expression_returns_set)
}

/// `contain_vars_of_level` on an expression.
pub(crate) fn contain_vars_of_level(n: &Node, level: u32) -> bool {
    match n {
        Node::Var(v) => return v.varlevelsup == level,
        Node::SubLink(s) => {
            if s.testexpr
                .as_ref()
                .is_some_and(|t| contain_vars_of_level(t, level))
            {
                return true;
            }
            return query_contains_vars_of_level(&s.subselect, level + 1);
        }
        Node::Aggref(a) if a.agglevelsup == level => return true,
        _ => {}
    }
    children(n)
        .into_iter()
        .any(|c| contain_vars_of_level(c, level))
}

/// `contain_vars_of_level` on a query.
pub(crate) fn query_contains_vars_of_level(q: &Query, level: u32) -> bool {
    let mut found = false;
    walk_query_exprs(q, &mut |e| {
        if !found && contain_vars_of_level(e, level) {
            found = true;
        }
    });
    if found {
        return true;
    }
    for r in &q.rtable {
        match &r.kind {
            RteKind::Subquery { subquery, .. } => {
                if query_contains_vars_of_level(subquery, level + 1) {
                    return true;
                }
            }
            RteKind::Function { functions, .. } => {
                if functions
                    .iter()
                    .any(|f| contain_vars_of_level(&f.funcexpr, level))
                {
                    return true;
                }
            }
            RteKind::Values { values_lists, .. } => {
                if values_lists
                    .iter()
                    .flatten()
                    .any(|e| contain_vars_of_level(e, level))
                {
                    return true;
                }
            }
            RteKind::Join { joinaliasvars, .. } => {
                if joinaliasvars
                    .iter()
                    .any(|e| contain_vars_of_level(e, level))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    q.cte_list
        .iter()
        .any(|c| query_contains_vars_of_level(&c.ctequery, level + 1))
}

/// Every top-level expression of a query (as `query_tree_walker` visits
/// them, without the range table).
pub(crate) fn walk_query_exprs(q: &Query, f: &mut dyn FnMut(&Node)) {
    for t in &q.target_list {
        f(&t.expr);
    }
    for t in &q.returning_list {
        f(&t.expr);
    }
    if let Some(j) = &q.jointree {
        walk_fromexpr(j, f);
    }
    if let Some(h) = &q.having_qual {
        f(h);
    }
    if let Some(l) = &q.limit_offset {
        f(l);
    }
    if let Some(l) = &q.limit_count {
        f(l);
    }
}

fn walk_fromexpr(j: &FromExpr, f: &mut dyn FnMut(&Node)) {
    for n in &j.fromlist {
        walk_jointree(n, f);
    }
    if let Some(q) = &j.quals {
        f(q);
    }
}

fn walk_jointree(n: &JoinTreeNode, f: &mut dyn FnMut(&Node)) {
    if let JoinTreeNode::JoinExpr(j) = n {
        walk_jointree(&j.larg, f);
        walk_jointree(&j.rarg, f);
        if let Some(q) = &j.quals {
            f(q);
        }
    }
}

/// `rangeTableEntry_used` on an expression.
pub(crate) fn range_table_entry_used(n: &Node, rt_index: i32, sublevels_up: u32) -> bool {
    match n {
        Node::Var(v) => return v.varlevelsup == sublevels_up && v.varno == rt_index,
        Node::SubLink(s) => {
            if s.testexpr
                .as_ref()
                .is_some_and(|t| range_table_entry_used(t, rt_index, sublevels_up))
            {
                return true;
            }
            return range_table_entry_used_query(&s.subselect, rt_index, sublevels_up + 1);
        }
        _ => {}
    }
    children(n)
        .into_iter()
        .any(|c| range_table_entry_used(c, rt_index, sublevels_up))
}

/// `rangeTableEntry_used` on a query.
pub(crate) fn range_table_entry_used_query(q: &Query, rt_index: i32, sublevels_up: u32) -> bool {
    let mut found = false;
    walk_query_exprs(q, &mut |e| {
        if !found && range_table_entry_used(e, rt_index, sublevels_up) {
            found = true;
        }
    });
    if found {
        return true;
    }
    if sublevels_up == 0 {
        if let Some(j) = &q.jointree {
            if jointree_mentions(j, rt_index) {
                return true;
            }
        }
    }
    for r in &q.rtable {
        match &r.kind {
            RteKind::Subquery { subquery, .. } => {
                if range_table_entry_used_query(subquery, rt_index, sublevels_up + 1) {
                    return true;
                }
            }
            RteKind::Function { functions, .. } => {
                if functions
                    .iter()
                    .any(|f| range_table_entry_used(&f.funcexpr, rt_index, sublevels_up))
                {
                    return true;
                }
            }
            RteKind::Values { values_lists, .. } => {
                if values_lists
                    .iter()
                    .flatten()
                    .any(|e| range_table_entry_used(e, rt_index, sublevels_up))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn jointree_mentions(j: &FromExpr, rt: i32) -> bool {
    fn node(n: &JoinTreeNode, rt: i32) -> bool {
        match n {
            JoinTreeNode::RangeTblRef(i) => *i == rt,
            JoinTreeNode::JoinExpr(j) => j.rtindex == rt || node(&j.larg, rt) || node(&j.rarg, rt),
        }
    }
    j.fromlist.iter().any(|n| node(n, rt))
}

/// `OffsetVarNodes(query, offset, sublevels_up)`.
pub(crate) fn offset_query(q: &mut Query, offset: i32, sublevels_up: u32) {
    if sublevels_up == 0 {
        if q.result_relation != 0 {
            q.result_relation += offset;
        }
        for r in q.row_marks.iter_mut() {
            r.rti = (r.rti as i32 + offset) as u32;
        }
        if let Some(j) = q.jointree.as_mut() {
            for n in j.fromlist.iter_mut() {
                offset_jointree(n, offset);
            }
        }
        if let Some(s) = q.set_operations.as_mut() {
            offset_setop(s, offset);
        }
    }
    let visit = |e: &mut Node| offset_expr(e, offset, sublevels_up);
    for t in q.target_list.iter_mut() {
        visit(&mut t.expr);
    }
    for t in q.returning_list.iter_mut() {
        visit(&mut t.expr);
    }
    if let Some(j) = q.jointree.as_mut() {
        offset_fromexpr_quals(j, offset, sublevels_up);
    }
    if let Some(h) = q.having_qual.as_mut() {
        visit(h);
    }
    if let Some(l) = q.limit_offset.as_mut() {
        visit(l);
    }
    if let Some(l) = q.limit_count.as_mut() {
        visit(l);
    }
    for w in q.window_clause.iter_mut() {
        if let Some(e) = w.start_offset.as_mut() {
            offset_expr(e, offset, sublevels_up);
        }
        if let Some(e) = w.end_offset.as_mut() {
            offset_expr(e, offset, sublevels_up);
        }
    }
    for c in q.cte_list.iter_mut() {
        offset_query(&mut c.ctequery, offset, sublevels_up + 1);
    }
    for r in q.rtable.iter_mut() {
        match &mut r.kind {
            RteKind::Subquery { subquery, .. } => offset_query(subquery, offset, sublevels_up + 1),
            RteKind::Function { functions, .. } => {
                for f in functions.iter_mut() {
                    offset_expr(&mut f.funcexpr, offset, sublevels_up);
                }
            }
            RteKind::Values { values_lists, .. } => {
                for e in values_lists.iter_mut().flatten() {
                    offset_expr(e, offset, sublevels_up);
                }
            }
            RteKind::Join { joinaliasvars, .. } => {
                for e in joinaliasvars.iter_mut() {
                    offset_expr(e, offset, sublevels_up);
                }
            }
            _ => {}
        }
    }
}

fn offset_jointree(n: &mut JoinTreeNode, offset: i32) {
    match n {
        JoinTreeNode::RangeTblRef(i) => *i += offset,
        JoinTreeNode::JoinExpr(j) => {
            offset_jointree(&mut j.larg, offset);
            offset_jointree(&mut j.rarg, offset);
            j.rtindex += offset;
        }
    }
}

fn offset_setop(s: &mut SetOperationStmt, offset: i32) {
    for side in [&mut s.larg, &mut s.rarg] {
        match side {
            SetOpNode::RangeTblRef(i) => *i += offset,
            SetOpNode::SetOp(x) => offset_setop(x, offset),
        }
    }
}

fn offset_fromexpr_quals(j: &mut FromExpr, offset: i32, up: u32) {
    fn jt(n: &mut JoinTreeNode, offset: i32, up: u32) {
        if let JoinTreeNode::JoinExpr(j) = n {
            jt(&mut j.larg, offset, up);
            jt(&mut j.rarg, offset, up);
            if let Some(q) = j.quals.as_mut() {
                offset_expr(q, offset, up);
            }
        }
    }
    for n in j.fromlist.iter_mut() {
        jt(n, offset, up);
    }
    if let Some(q) = j.quals.as_mut() {
        offset_expr(q, offset, up);
    }
}

/// `OffsetVarNodes_walker` on an expression.
pub(crate) fn offset_expr(n: &mut Node, offset: i32, up: u32) {
    match n {
        Node::Var(v) => {
            if v.varlevelsup == up {
                v.varno += offset;
                if v.varnosyn > 0 {
                    v.varnosyn = (v.varnosyn as i32 + offset) as u32;
                }
            }
            return;
        }
        Node::SubLink(s) => {
            if let Some(t) = s.testexpr.as_mut() {
                offset_expr(t, offset, up);
            }
            offset_query(&mut s.subselect, offset, up + 1);
            return;
        }
        _ => {}
    }
    for c in children_mut(n) {
        offset_expr(c, offset, up);
    }
}

/// The mutable children of an expression.
pub(crate) fn children_mut(n: &mut Node) -> Vec<&mut Node> {
    let mut v: Vec<&mut Node> = Vec::new();
    match n {
        Node::Var(_)
        | Node::Const(_)
        | Node::Param(_)
        | Node::CaseTestExpr(_)
        | Node::SqlValueFunction(_)
        | Node::SetToDefault(_) => {}
        Node::Aggref(a) => {
            v.extend(a.aggdirectargs.iter_mut());
            for t in a.args.iter_mut() {
                v.push(&mut t.expr);
            }
            if let Some(f) = a.aggfilter.as_mut() {
                v.push(f);
            }
        }
        Node::GroupingFunc(g) => v.extend(g.args.iter_mut()),
        Node::WindowFunc(w) => {
            v.extend(w.args.iter_mut());
            if let Some(f) = w.aggfilter.as_mut() {
                v.push(f);
            }
        }
        Node::SubscriptingRef(r) => {
            let r = &mut **r;
            v.extend(r.refupperindexpr.iter_mut().flatten());
            if let Some(l) = r.reflowerindexpr.as_mut() {
                v.extend(l.iter_mut().flatten());
            }
            v.push(&mut r.refexpr);
            if let Some(a) = r.refassgnexpr.as_mut() {
                v.push(a);
            }
        }
        Node::FuncExpr(f) => v.extend(f.args.iter_mut()),
        Node::NamedArgExpr(na) => v.push(&mut na.arg),
        Node::OpExpr(o) => v.extend(o.args.iter_mut()),
        Node::ScalarArrayOpExpr(o) => v.extend(o.args.iter_mut()),
        Node::BoolExpr(b) => v.extend(b.args.iter_mut()),
        Node::SubLink(s) => {
            if let Some(t) = s.testexpr.as_mut() {
                v.push(t);
            }
        }
        Node::FieldSelect(f) => v.push(&mut f.arg),
        Node::RelabelType(r) => v.push(&mut r.arg),
        Node::CoerceViaIO(c) => v.push(&mut c.arg),
        Node::ArrayCoerceExpr(c) => {
            let c = &mut **c;
            v.push(&mut c.arg);
            v.push(&mut c.elemexpr);
        }
        Node::ConvertRowtypeExpr(c) => v.push(&mut c.arg),
        Node::CollateExpr(c) => v.push(&mut c.arg),
        Node::CaseExpr(c) => {
            let c = &mut **c;
            if let Some(a) = c.arg.as_mut() {
                v.push(a);
            }
            for w in c.args.iter_mut() {
                v.push(&mut w.expr);
                v.push(&mut w.result);
            }
            v.push(&mut c.defresult);
        }
        Node::ArrayExpr(a) => v.extend(a.elements.iter_mut()),
        Node::RowExpr(r) => v.extend(r.args.iter_mut()),
        Node::RowCompareExpr(r) => {
            let r = &mut **r;
            v.extend(r.largs.iter_mut());
            v.extend(r.rargs.iter_mut());
        }
        Node::CoalesceExpr(c) => v.extend(c.args.iter_mut()),
        Node::MinMaxExpr(m) => v.extend(m.args.iter_mut()),
        Node::NullTest(t) => v.push(&mut t.arg),
        Node::BooleanTest(b) => v.push(&mut b.arg),
        Node::CoerceToDomain(c) => v.push(&mut c.arg),
        Node::TargetEntry(t) => v.push(&mut t.expr),
    }
    v
}

/// `check_agg_arguments_walker`'s minimum Var and Aggref levels, relative
/// to the aggregate's own level.
pub(crate) fn agg_levels(
    n: &Node,
    sublevels: u32,
    min_var: &mut Option<u32>,
    min_agg: &mut Option<u32>,
) {
    match n {
        Node::Var(v) => {
            if v.varlevelsup >= sublevels {
                let l = v.varlevelsup - sublevels;
                if min_var.is_none_or(|m| m > l) {
                    *min_var = Some(l);
                }
            }
            return;
        }
        Node::Aggref(a) => {
            if a.agglevelsup >= sublevels {
                let l = a.agglevelsup - sublevels;
                if min_agg.is_none_or(|m| m > l) {
                    *min_agg = Some(l);
                }
            }
        }
        Node::GroupingFunc(g) => {
            if g.agglevelsup >= sublevels {
                let l = g.agglevelsup - sublevels;
                if min_agg.is_none_or(|m| m > l) {
                    *min_agg = Some(l);
                }
            }
        }
        Node::SubLink(s) => {
            if let Some(t) = &s.testexpr {
                agg_levels(t, sublevels, min_var, min_agg);
            }
            let q = &s.subselect;
            walk_query_exprs(q, &mut |e| agg_levels(e, sublevels + 1, min_var, min_agg));
            return;
        }
        _ => {}
    }
    for c in children(n) {
        agg_levels(c, sublevels, min_var, min_agg);
    }
}
