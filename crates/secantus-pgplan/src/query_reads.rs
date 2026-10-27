//! What a query reads: the names of the columns its references can read,
//! found by walking it, with an unknown node counting as "anything".
//! `joins::projection` uses it for the keys a join's rows must carry.

use super::*;

/// How a column reference counts toward what a query reads.
pub(crate) enum Use {
    /// It reads this name.
    Name(String),
    /// It may read anything (a whole row, `*`).
    All,
    /// It reads nothing that counts.
    Ignore,
}

/// What a query reads: the names its column references read, found by
/// walking it with `classify` deciding each reference (its name parts, `None`
/// for a `*`).
pub(crate) struct Reads<'a> {
    pub(crate) names: std::collections::HashSet<String>,
    /// A reference that may read anything, or a node the walk does not know.
    pub(crate) all: bool,
    classify: &'a dyn Fn(&[Option<&str>]) -> Use,
}

impl<'a> Reads<'a> {
    pub(crate) fn new(classify: &'a dyn Fn(&[Option<&str>]) -> Use) -> Self {
        Reads {
            names: std::collections::HashSet::new(),
            all: false,
            classify,
        }
    }

    fn column_ref(&mut self, c: &pg_query::protobuf::ColumnRef) {
        let parts: Vec<Option<&str>> = c
            .fields
            .iter()
            .map(|f| match f.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.as_str()),
                _ => None,
            })
            .collect();
        match (self.classify)(&parts) {
            Use::Name(n) => {
                self.names.insert(n);
            }
            Use::All => self.all = true,
            Use::Ignore => {}
        }
    }

    fn node(&mut self, node: &pg_query::protobuf::Node, skip: usize) {
        if self.all {
            return;
        }
        let Some(n) = node.node.as_ref() else {
            return;
        };
        match n {
            N::ColumnRef(c) => self.column_ref(c),
            N::AConst(_) | N::ParamRef(_) | N::SqlvalueFunction(_) | N::RangeVar(_) => {}
            N::String(s) => {
                // A `USING` column.
                self.names.insert(s.sval.clone());
            }
            N::TypeCast(tc) => self.opt(tc.arg.as_deref(), skip),
            N::AExpr(e) => {
                self.opt(e.lexpr.as_deref(), skip);
                self.opt(e.rexpr.as_deref(), skip);
            }
            N::FuncCall(f) => {
                self.all_of(&f.args, skip);
                self.all_of(&f.agg_order, skip);
                self.opt(f.agg_filter.as_deref(), skip);
                if let Some(w) = f.over.as_deref() {
                    self.window(w, skip);
                }
            }
            N::BoolExpr(b) => self.all_of(&b.args, skip),
            N::NullTest(t) => self.opt(t.arg.as_deref(), skip),
            N::BooleanTest(t) => self.opt(t.arg.as_deref(), skip),
            N::CaseExpr(c) => {
                self.opt(c.arg.as_deref(), skip);
                self.all_of(&c.args, skip);
                self.opt(c.defresult.as_deref(), skip);
            }
            N::CaseWhen(w) => {
                self.opt(w.expr.as_deref(), skip);
                self.opt(w.result.as_deref(), skip);
            }
            N::CoalesceExpr(c) => self.all_of(&c.args, skip),
            N::MinMaxExpr(m) => self.all_of(&m.args, skip),
            N::AArrayExpr(a) => self.all_of(&a.elements, skip),
            N::RowExpr(r) => self.all_of(&r.args, skip),
            N::List(l) => self.all_of(&l.items, skip),
            N::CollateClause(c) => self.opt(c.arg.as_deref(), skip),
            N::NamedArgExpr(a) => self.opt(a.arg.as_deref(), skip),
            N::ResTarget(rt) => self.opt(rt.val.as_deref(), skip),
            N::SortBy(sb) => self.opt(sb.node.as_deref(), skip),
            N::GroupingSet(g) => self.all_of(&g.content, skip),
            N::GroupingFunc(g) => self.all_of(&g.args, skip),
            N::AIndirection(a) => {
                self.opt(a.arg.as_deref(), skip);
                for ind in &a.indirection {
                    match ind.node.as_ref() {
                        Some(N::AIndices(idx)) => {
                            self.opt(idx.lidx.as_deref(), skip);
                            self.opt(idx.uidx.as_deref(), skip);
                        }
                        Some(N::String(_)) => {}
                        _ => self.all = true,
                    }
                }
            }
            N::SubLink(sl) => {
                self.opt(sl.testexpr.as_deref(), skip);
                self.opt(sl.subselect.as_deref(), skip);
            }
            N::SelectStmt(s) => self.select(s, skip),
            N::RangeSubselect(rs) => {
                // The subquery being pruned is the one place not walked.
                let body = rs.subquery.as_deref();
                if body.is_some_and(|b| std::ptr::from_ref(b) as usize == skip) {
                    return;
                }
                self.opt(body, skip);
            }
            N::RangeFunction(rf) => self.all_of(&rf.functions, skip),
            N::JoinExpr(j) => {
                if j.is_natural {
                    self.all = true;
                    return;
                }
                self.opt(j.larg.as_deref(), skip);
                self.opt(j.rarg.as_deref(), skip);
                self.all_of(&j.using_clause, skip);
                self.opt(j.quals.as_deref(), skip);
            }
            N::ColumnDef(_) | N::TypeName(_) => {}
            N::CommonTableExpr(c) => self.opt(c.ctequery.as_deref(), skip),
            N::WindowDef(w) => self.window(w, skip),
            N::LockingClause(_) => {}
            _ => self.all = true,
        }
    }

    fn opt(&mut self, node: Option<&pg_query::protobuf::Node>, skip: usize) {
        if let Some(n) = node {
            self.node(n, skip);
        }
    }

    fn all_of(&mut self, nodes: &[pg_query::protobuf::Node], skip: usize) {
        for n in nodes {
            self.node(n, skip);
        }
    }

    fn window(&mut self, w: &pg_query::protobuf::WindowDef, skip: usize) {
        self.all_of(&w.partition_clause, skip);
        self.all_of(&w.order_clause, skip);
        self.opt(w.start_offset.as_deref(), skip);
        self.opt(w.end_offset.as_deref(), skip);
    }

    pub(crate) fn select(&mut self, s: &pg_query::protobuf::SelectStmt, skip: usize) {
        self.all_of(&s.target_list, skip);
        self.all_of(&s.from_clause, skip);
        self.opt(s.where_clause.as_deref(), skip);
        self.all_of(&s.group_clause, skip);
        self.opt(s.having_clause.as_deref(), skip);
        self.all_of(&s.window_clause, skip);
        self.all_of(&s.values_lists, skip);
        self.all_of(&s.sort_clause, skip);
        self.all_of(&s.distinct_clause, skip);
        self.opt(s.limit_offset.as_deref(), skip);
        self.opt(s.limit_count.as_deref(), skip);
        self.all_of(&s.locking_clause, skip);
        if let Some(w) = s.with_clause.as_ref() {
            self.all_of(&w.ctes, skip);
        }
        if let Some(l) = s.larg.as_deref() {
            self.select(l, skip);
        }
        if let Some(r) = s.rarg.as_deref() {
            self.select(r, skip);
        }
    }
}
