//! `parse_collate.c`: collation assignment.

use super::catalog::*;
use super::nodefuncs::{children_mut, expr_collation, expr_location, expr_type};
use super::nodes::*;
use super::parse::Analyzer;
use super::{pg_error, R};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Strength {
    None,
    Implicit,
    Conflict,
    Explicit,
}

#[derive(Clone, Copy, Debug)]
struct Ctx {
    collation: Oid,
    strength: Strength,
    location: i32,
    collation2: Oid,
    location2: i32,
}

impl Ctx {
    fn new() -> Self {
        Ctx {
            collation: 0,
            strength: Strength::None,
            location: -1,
            collation2: 0,
            location2: -1,
        }
    }
}

impl<'a> Analyzer<'a> {
    /// `assign_query_collations`.
    pub fn assign_query_collations(&self, q: &mut Query) -> R<()> {
        for t in q.target_list.iter_mut() {
            self.assign_tle_collations(t)?;
        }
        for t in q.returning_list.iter_mut() {
            self.assign_tle_collations(t)?;
        }
        if let Some(oc) = q.on_conflict.as_mut() {
            for e in oc.arbiter_elems.iter_mut() {
                self.assign_expr_collations(&mut e.expr)?;
            }
            if let Some(w) = oc.arbiter_where.as_mut() {
                self.assign_expr_collations(w)?;
            }
            for t in oc.on_conflict_set.iter_mut() {
                self.assign_tle_collations(t)?;
            }
            if let Some(w) = oc.on_conflict_where.as_mut() {
                self.assign_expr_collations(w)?;
            }
        }
        if let Some(j) = q.jointree.as_mut() {
            for n in j.fromlist.iter_mut() {
                self.assign_jointree_collations(n)?;
            }
            if let Some(qual) = j.quals.as_mut() {
                self.assign_expr_collations(qual)?;
            }
        }
        if let Some(h) = q.having_qual.as_mut() {
            self.assign_expr_collations(h)?;
        }
        if let Some(l) = q.limit_offset.as_mut() {
            self.assign_expr_collations(l)?;
        }
        if let Some(l) = q.limit_count.as_mut() {
            self.assign_expr_collations(l)?;
        }
        for w in q.window_clause.iter_mut() {
            if let Some(e) = w.start_offset.as_mut() {
                self.assign_expr_collations(e)?;
            }
            if let Some(e) = w.end_offset.as_mut() {
                self.assign_expr_collations(e)?;
            }
        }
        Ok(())
    }

    fn assign_tle_collations(&self, t: &mut TargetEntry) -> R<()> {
        let mut ctx = Ctx::new();
        let mut loc = Ctx::new();
        self.walk(&mut t.expr, &mut loc)?;
        if loc.strength == Strength::Conflict && t.ressortgroupref != 0 {
            return pg_error(
                "42P21",
                format!(
                    "collation mismatch between implicit collations \"{}\" and \"{}\"",
                    self.cat.collation_name(loc.collation),
                    self.cat.collation_name(loc.collation2)
                ),
            );
        }
        merge(
            self.cat,
            loc.collation,
            loc.strength,
            loc.location,
            loc.collation2,
            loc.location2,
            &mut ctx,
        )
    }

    fn assign_jointree_collations(&self, n: &mut JoinTreeNode) -> R<()> {
        if let JoinTreeNode::JoinExpr(j) = n {
            self.assign_jointree_collations(&mut j.larg)?;
            self.assign_jointree_collations(&mut j.rarg)?;
            if let Some(q) = j.quals.as_mut() {
                self.assign_expr_collations(q)?;
            }
        }
        Ok(())
    }

    /// `assign_expr_collations`.
    pub fn assign_expr_collations(&self, n: &mut Node) -> R<()> {
        let mut ctx = Ctx::new();
        self.walk(n, &mut ctx)
    }

    /// `select_common_collation`.
    pub fn select_common_collation(&self, exprs: &[Node], none_ok: bool) -> R<Oid> {
        let mut ctx = Ctx::new();
        for e in exprs {
            let mut e = e.clone();
            self.walk(&mut e, &mut ctx)?;
        }
        if ctx.strength == Strength::Conflict {
            if none_ok {
                return Ok(0);
            }
            return pg_error(
                "42P21",
                format!(
                    "collation mismatch between implicit collations \"{}\" and \"{}\"",
                    self.cat.collation_name(ctx.collation),
                    self.cat.collation_name(ctx.collation2)
                ),
            );
        }
        Ok(ctx.collation)
    }

    /// A TargetEntry walked as a node: its expression's state bubbles up.
    fn walk_tle(&self, t: &mut TargetEntry, context: &mut Ctx) -> R<()> {
        let mut wrapped = Node::TargetEntry(Box::new(t.clone()));
        self.walk(&mut wrapped, context)?;
        if let Node::TargetEntry(x) = wrapped {
            *t = *x;
        }
        Ok(())
    }

    /// `assign_ordered_set_collations` and `assign_hypothetical_collations`.
    fn assign_ordered_set_collations(&self, a: &mut Aggref, loc: &mut Ctx) -> R<()> {
        // Merge sort collations to the parent only if there can be only one.
        let merge_sort = a.args.len() == 1
            && self
                .cat
                .proc_row(a.aggfnoid)
                .is_some_and(|p| p.variadic == 0);
        if a.aggkind == 'o' {
            for d in a.aggdirectargs.iter_mut() {
                self.walk(d, loc)?;
            }
            for t in a.args.iter_mut() {
                if merge_sort {
                    self.walk_tle(t, loc)?;
                } else {
                    let mut own = Ctx::new();
                    self.walk_tle(t, &mut own)?;
                }
            }
            return Ok(());
        }
        // Hypothetical: the non-hypothetical direct args are ordinary
        // children; each hypothetical arg and its sort column share one
        // collation, forced into the sort column with a RelabelType.
        let extra = a.aggdirectargs.len().saturating_sub(a.args.len());
        for d in a.aggdirectargs.iter_mut().take(extra) {
            self.walk(d, loc)?;
        }
        for (h, s) in a
            .aggdirectargs
            .iter_mut()
            .skip(extra)
            .zip(a.args.iter_mut())
        {
            let mut pair = Ctx::new();
            self.walk(h, &mut pair)?;
            self.walk(&mut s.expr, &mut pair)?;
            if pair.strength == Strength::Conflict {
                return pg_error(
                    "42P21",
                    format!(
                        "collation mismatch between implicit collations \"{}\" and \"{}\"",
                        self.cat.collation_name(pair.collation),
                        self.cat.collation_name(pair.collation2)
                    ),
                );
            }
            if pair.collation != 0 && pair.collation != expr_collation(self.cat, &s.expr) {
                let e = s.expr.clone();
                s.expr = Node::RelabelType(Box::new(RelabelType {
                    resulttype: expr_type(self.cat, &e),
                    resulttypmod: super::nodefuncs::expr_typmod(self.cat, &e),
                    resultcollid: pair.collation,
                    relabelformat: CoercionForm::ImplicitCast,
                    location: -1,
                    arg: e,
                }));
            }
            if merge_sort {
                merge(
                    self.cat,
                    pair.collation,
                    pair.strength,
                    pair.location,
                    pair.collation2,
                    pair.location2,
                    loc,
                )?;
            }
        }
        Ok(())
    }

    /// `assign_collations_walker`.
    fn walk(&self, n: &mut Node, context: &mut Ctx) -> R<()> {
        let mut loc = Ctx::new();
        let (collation, strength, location);
        match n {
            Node::CollateExpr(c) => {
                self.walk(&mut c.arg, &mut loc)?;
                collation = c.coll_oid;
                strength = Strength::Explicit;
                location = c.location;
            }
            Node::FieldSelect(f) => {
                self.walk(&mut f.arg, &mut loc)?;
                if f.resultcollid != 0 {
                    collation = f.resultcollid;
                    strength = Strength::Implicit;
                    location = expr_location(&f.arg);
                } else {
                    collation = 0;
                    strength = Strength::None;
                    location = -1;
                }
            }
            Node::RowExpr(r) => {
                for a in r.args.iter_mut() {
                    self.assign_expr_collations(a)?;
                }
                return Ok(());
            }
            Node::RowCompareExpr(r) => {
                let mut colls = Vec::new();
                for (l, rr) in r.largs.iter_mut().zip(r.rargs.iter_mut()) {
                    let mut c = Ctx::new();
                    self.walk(l, &mut c)?;
                    self.walk(rr, &mut c)?;
                    colls.push(if c.strength == Strength::Conflict {
                        0
                    } else {
                        c.collation
                    });
                }
                r.inputcollids = colls;
                return Ok(());
            }
            Node::CoerceToDomain(c) => {
                let typcoll = self.cat.get_typcollation(c.resulttype);
                self.walk(&mut c.arg, &mut loc)?;
                let (co, st, lo) = if typcoll != 0 {
                    if typcoll == DEFAULT_COLLATION_OID {
                        (loc.collation, loc.strength, loc.location)
                    } else {
                        (typcoll, Strength::Implicit, -1)
                    }
                } else {
                    (0, Strength::None, -1)
                };
                c.resultcollid = if st == Strength::Conflict { 0 } else { co };
                collation = co;
                strength = st;
                location = if typcoll != 0 && typcoll != DEFAULT_COLLATION_OID {
                    expr_location(n)
                } else {
                    lo
                };
            }
            Node::TargetEntry(t) => {
                self.walk(&mut t.expr, &mut loc)?;
                if loc.strength == Strength::Conflict && t.ressortgroupref != 0 {
                    return pg_error(
                        "42P21",
                        format!(
                            "collation mismatch between implicit collations \"{}\" and \"{}\"",
                            self.cat.collation_name(loc.collation),
                            self.cat.collation_name(loc.collation2)
                        ),
                    );
                }
                collation = loc.collation;
                strength = loc.strength;
                location = loc.location;
            }
            Node::SubLink(s) => {
                // The general case: the test expression, then the Query as
                // its first output column.
                if let Some(t) = s.testexpr.as_mut() {
                    self.walk(t, &mut loc)?;
                }
                let qcoll = match s.subselect.target_list.first() {
                    Some(t) if !t.resjunk => {
                        Some((expr_collation(self.cat, &t.expr), expr_location(&t.expr)))
                    }
                    _ => None,
                };
                if let Some((c, l)) = qcoll {
                    merge(self.cat, c, Strength::Implicit, l, 0, -1, &mut loc)?;
                }
                let t = expr_type(self.cat, n);
                let tc = self.cat.get_typcollation(t);
                let (co, st, lo) = self.general_result(n, tc, &loc);
                collation = co;
                strength = st;
                location = lo;
            }
            Node::Var(_)
            | Node::Const(_)
            | Node::Param(_)
            | Node::CaseTestExpr(_)
            | Node::SetToDefault(_) => {
                collation = expr_collation(self.cat, n);
                strength = if collation != 0 {
                    Strength::Implicit
                } else {
                    Strength::None
                };
                location = expr_location(n);
            }
            _ => {
                match &mut *n {
                    Node::Aggref(a) if a.aggkind != 'n' => {
                        self.assign_ordered_set_collations(a, &mut loc)?;
                        if let Some(f) = a.aggfilter.as_mut() {
                            self.assign_expr_collations(f)?;
                        }
                    }
                    Node::Aggref(a) => {
                        for t in a.args.iter_mut() {
                            if t.resjunk {
                                let mut c = Ctx::new();
                                let mut wrapped = Node::TargetEntry(Box::new(t.clone()));
                                self.walk(&mut wrapped, &mut c)?;
                                if let Node::TargetEntry(x) = wrapped {
                                    *t = *x;
                                }
                            } else {
                                let mut wrapped = Node::TargetEntry(Box::new(t.clone()));
                                self.walk(&mut wrapped, &mut loc)?;
                                if let Node::TargetEntry(x) = wrapped {
                                    *t = *x;
                                }
                            }
                        }
                        if let Some(f) = a.aggfilter.as_mut() {
                            self.assign_expr_collations(f)?;
                        }
                    }
                    Node::WindowFunc(w) => {
                        for a in w.args.iter_mut() {
                            self.walk(a, &mut loc)?;
                        }
                        if let Some(f) = w.aggfilter.as_mut() {
                            self.assign_expr_collations(f)?;
                        }
                    }
                    Node::CaseExpr(c) => {
                        for w in c.args.iter_mut() {
                            self.walk(&mut w.expr, &mut loc)?;
                            self.walk(&mut w.result, &mut loc)?;
                        }
                        self.walk(&mut c.defresult, &mut loc)?;
                    }
                    Node::SubscriptingRef(r) => {
                        for e in r.refupperindexpr.iter_mut().flatten() {
                            self.assign_expr_collations(e)?;
                        }
                        if let Some(l) = r.reflowerindexpr.as_mut() {
                            for e in l.iter_mut().flatten() {
                                self.assign_expr_collations(e)?;
                            }
                        }
                        self.walk(&mut r.refexpr, &mut loc)?;
                        if let Some(a) = r.refassgnexpr.as_mut() {
                            self.walk(a, &mut loc)?;
                        }
                    }
                    other => {
                        for c in children_mut(other) {
                            self.walk(c, &mut loc)?;
                        }
                    }
                }
                let t = expr_type(self.cat, n);
                let tc = self.cat.get_typcollation(t);
                let (co, st, lo) = self.general_result(n, tc, &loc);
                collation = co;
                strength = st;
                location = lo;
                super::nodefuncs::set_collation(n, if st == Strength::Conflict { 0 } else { co });
                super::nodefuncs::set_input_collation(
                    n,
                    if loc.strength == Strength::Conflict {
                        0
                    } else {
                        loc.collation
                    },
                );
            }
        }
        merge(
            self.cat,
            collation,
            strength,
            location,
            loc.collation2,
            loc.location2,
            context,
        )
    }

    fn general_result(&self, n: &Node, typcoll: Oid, loc: &Ctx) -> (Oid, Strength, i32) {
        if typcoll != 0 {
            if loc.strength > Strength::None {
                (loc.collation, loc.strength, loc.location)
            } else {
                (typcoll, Strength::Implicit, expr_location(n))
            }
        } else {
            (0, Strength::None, -1)
        }
    }
}

/// `merge_collation_state`.
fn merge(
    cat: &Catalog,
    collation: Oid,
    strength: Strength,
    location: i32,
    collation2: Oid,
    location2: i32,
    ctx: &mut Ctx,
) -> R<()> {
    if strength > ctx.strength {
        ctx.collation = collation;
        ctx.strength = strength;
        ctx.location = location;
        if strength == Strength::Conflict {
            ctx.collation2 = collation2;
            ctx.location2 = location2;
        }
    } else if strength == ctx.strength {
        match strength {
            Strength::Implicit => {
                if collation != ctx.collation {
                    if ctx.collation == DEFAULT_COLLATION_OID {
                        ctx.collation = collation;
                        ctx.strength = strength;
                        ctx.location = location;
                    } else if collation != DEFAULT_COLLATION_OID {
                        ctx.strength = Strength::Conflict;
                        ctx.collation2 = collation;
                        ctx.location2 = location;
                    }
                }
            }
            Strength::Explicit => {
                if collation != ctx.collation {
                    return pg_error(
                        "42P21",
                        format!(
                            "collation mismatch between explicit collations \"{}\" and \"{}\"",
                            cat.collation_name(ctx.collation),
                            cat.collation_name(collation)
                        ),
                    );
                }
            }
            _ => {}
        }
    }
    Ok(())
}
