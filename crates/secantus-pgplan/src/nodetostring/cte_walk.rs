//! `parse_cte.c`'s raw-tree walks for WITH RECURSIVE: `makeDependencyGraph`
//! (with `WalkInnerWith`'s scoping of inner WITH names and
//! `TopologicalSort`) and `checkWellFormedRecursion`.
//!
//! Both are `raw_expression_tree_walker` walks, which visit every field of
//! a raw node. The raw tree is walked here as the JSON its serde derive
//! writes, so every field is reached: a tagged `{"node": {"Kind": ...}}` is
//! a `Node`, and the statement fields that hold a struct directly (a
//! SelectStmt's `larg` / `rarg`, a DML statement's `relation`, every
//! `with_clause`) are followed by name.

use super::{pg_error, AnalysisError, R};
use pg_query::protobuf as pb;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

fn tagged(v: &Value) -> Option<(&str, &Map<String, Value>)> {
    let o = v.as_object()?;
    if o.len() != 1 {
        return None;
    }
    let (k, inner) = o.get("node")?.as_object()?.iter().next()?;
    Some((k.as_str(), inner.as_object()?))
}

fn cte_list(with: &Value) -> Vec<(String, Value)> {
    with.get("ctes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let (k, o) = tagged(c)?;
                    (k == "CommonTableExpr").then(|| {
                        (
                            o.get("ctename")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            o.get("ctequery").cloned().unwrap_or(Value::Null),
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn is_recursive(with: &Value) -> bool {
    with.get("recursive")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn field_str<'a>(o: &'a Map<String, Value>, k: &str) -> &'a str {
    o.get(k).and_then(Value::as_str).unwrap_or("")
}

fn field_int(o: &Map<String, Value>, k: &str) -> i64 {
    o.get(k).and_then(Value::as_i64).unwrap_or(0)
}

fn to_json(v: &Option<Box<pb::Node>>) -> R<Value> {
    serde_json::to_value(v).map_err(|e| AnalysisError::new(format!("a raw tree: {e}")))
}

// ---------------------------------------------------------------------------
// makeDependencyGraph

struct Dep<'a> {
    names: &'a [String],
    cur: usize,
    inner: Vec<Vec<String>>,
    deps: Vec<BTreeSet<usize>>,
    recursive: Vec<bool>,
}

impl Dep<'_> {
    fn range_var(&mut self, o: &Map<String, Value>) {
        if !field_str(o, "schemaname").is_empty() {
            return;
        }
        let name = field_str(o, "relname");
        if self.inner.iter().any(|l| l.iter().any(|n| n == name)) {
            return;
        }
        if let Some(i) = self.names.iter().position(|n| n == name) {
            if i != self.cur {
                self.deps[self.cur].insert(i);
            } else {
                self.recursive[i] = true;
            }
        }
    }

    fn walk(&mut self, v: &Value) {
        if let Some((k, o)) = tagged(v) {
            self.node(k, o);
            return;
        }
        match v {
            Value::Array(a) => a.iter().for_each(|x| self.walk(x)),
            Value::Object(m) => m.values().for_each(|x| self.walk(x)),
            _ => {}
        }
    }

    fn node(&mut self, k: &str, o: &Map<String, Value>) {
        match k {
            "RangeVar" => self.range_var(o),
            "SelectStmt" => self.stmt(o, true),
            "InsertStmt" | "UpdateStmt" | "DeleteStmt" | "MergeStmt" => self.stmt(o, false),
            "WithClause" => {}
            _ => o.values().for_each(|x| self.walk(x)),
        }
    }

    /// A statement's fields, its WITH aside.
    fn fields(&mut self, o: &Map<String, Value>, is_select: bool) {
        for (key, val) in o {
            if key == "with_clause" || val.is_null() {
                continue;
            }
            if is_select && (key == "larg" || key == "rarg") {
                if let Some(s) = val.as_object() {
                    self.stmt(s, true);
                }
                continue;
            }
            if !is_select && key == "relation" {
                if let Some(rv) = val.as_object() {
                    self.range_var(rv);
                }
                continue;
            }
            self.walk(val);
        }
    }

    /// The statement node, with `WalkInnerWith` for its WITH.
    fn stmt(&mut self, o: &Map<String, Value>, is_select: bool) {
        let with = o.get("with_clause").filter(|w| !w.is_null()).cloned();
        let Some(with) = with else {
            self.fields(o, is_select);
            return;
        };
        let ctes = cte_list(&with);
        if is_recursive(&with) {
            self.inner
                .push(ctes.iter().map(|(n, _)| n.clone()).collect());
            for (_, q) in &ctes {
                self.walk(q);
            }
        } else {
            self.inner.push(Vec::new());
            for (n, q) in &ctes {
                self.walk(q);
                if let Some(l) = self.inner.last_mut() {
                    l.push(n.clone());
                }
            }
        }
        self.fields(o, is_select);
        self.inner.pop();
    }
}

/// `makeDependencyGraph` and `TopologicalSort`: the order to analyse the
/// CTEs of a WITH RECURSIVE in (positions in the clause), each with whether
/// it refers to itself.
pub(crate) fn dependency_order(ctes: &[&pb::CommonTableExpr]) -> R<Vec<(usize, bool)>> {
    let names: Vec<String> = ctes.iter().map(|c| c.ctename.clone()).collect();
    let mut st = Dep {
        names: &names,
        cur: 0,
        inner: Vec::new(),
        deps: vec![BTreeSet::new(); ctes.len()],
        recursive: vec![false; ctes.len()],
    };
    for (i, c) in ctes.iter().enumerate() {
        st.cur = i;
        st.inner.clear();
        let q = to_json(&c.ctequery)?;
        st.walk(&q);
    }
    let mut items: Vec<(usize, BTreeSet<usize>)> = st.deps.into_iter().enumerate().collect();
    for i in 0..items.len() {
        let j = (i..items.len())
            .find(|&j| items[j].1.is_empty())
            .ok_or_else(|| {
                AnalysisError::pg(
                    "0A000",
                    "mutual recursion between WITH items is not implemented",
                )
            })?;
        items.swap(i, j);
        let id = items[i].0;
        for item in items.iter_mut().skip(i + 1) {
            item.1.remove(&id);
        }
    }
    let recursive = st.recursive;
    Ok(items
        .into_iter()
        .map(|(id, _)| (id, recursive[id]))
        .collect())
}

// ---------------------------------------------------------------------------
// checkWellFormedRecursion

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Ok,
    NonRecursiveTerm,
    Sublink,
    OuterJoin,
    Intersect,
    Except,
}

struct Chk<'a> {
    name: &'a str,
    inner: Vec<Vec<String>>,
    selfrefs: u32,
    ctx: Ctx,
}

impl Chk<'_> {
    fn walk(&mut self, v: &Value) -> R<()> {
        if let Some((k, o)) = tagged(v) {
            return self.node(k, o);
        }
        match v {
            Value::Array(a) => {
                for x in a {
                    self.walk(x)?;
                }
            }
            Value::Object(m) => {
                for x in m.values() {
                    self.walk(x)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn walk_field(&mut self, o: &Map<String, Value>, k: &str) -> R<()> {
        match o.get(k) {
            Some(v) => self.walk(v),
            None => Ok(()),
        }
    }

    fn node(&mut self, k: &str, o: &Map<String, Value>) -> R<()> {
        let save = self.ctx;
        match k {
            "RangeVar" => self.range_var(o),
            "SelectStmt" => self.select_with(o),
            "WithClause" => Ok(()),
            "JoinExpr" => {
                // JoinType: the protobuf's numbering is PostgreSQL's plus one.
                match field_int(o, "jointype") - 1 {
                    0 => {
                        self.walk_field(o, "larg")?;
                        self.walk_field(o, "rarg")?;
                        self.walk_field(o, "quals")
                    }
                    1 => {
                        self.walk_field(o, "larg")?;
                        if save == Ctx::Ok {
                            self.ctx = Ctx::OuterJoin;
                        }
                        self.walk_field(o, "rarg")?;
                        self.ctx = save;
                        self.walk_field(o, "quals")
                    }
                    2 => {
                        if save == Ctx::Ok {
                            self.ctx = Ctx::OuterJoin;
                        }
                        self.walk_field(o, "larg")?;
                        self.walk_field(o, "rarg")?;
                        self.ctx = save;
                        self.walk_field(o, "quals")
                    }
                    3 => {
                        if save == Ctx::Ok {
                            self.ctx = Ctx::OuterJoin;
                        }
                        self.walk_field(o, "larg")?;
                        self.ctx = save;
                        self.walk_field(o, "rarg")?;
                        self.walk_field(o, "quals")
                    }
                    _ => Err(AnalysisError::new("unrecognized join type".into())),
                }
            }
            "SubLink" => {
                self.ctx = Ctx::Sublink;
                self.walk_field(o, "subselect")?;
                self.ctx = save;
                self.walk_field(o, "testexpr")
            }
            _ => {
                for v in o.values() {
                    self.walk(v)?;
                }
                Ok(())
            }
        }
    }

    fn range_var(&mut self, o: &Map<String, Value>) -> R<()> {
        if !field_str(o, "schemaname").is_empty() {
            return Ok(());
        }
        let rel = field_str(o, "relname");
        if self.inner.iter().any(|l| l.iter().any(|n| n == rel)) {
            return Ok(());
        }
        if rel == self.name {
            let msg = match self.ctx {
                Ctx::Ok => None,
                Ctx::NonRecursiveTerm => Some("must not appear within its non-recursive term"),
                Ctx::Sublink => Some("must not appear within a subquery"),
                Ctx::OuterJoin => Some("must not appear within an outer join"),
                Ctx::Intersect => Some("must not appear within INTERSECT"),
                Ctx::Except => Some("must not appear within EXCEPT"),
            };
            if let Some(m) = msg {
                return pg_error(
                    "42P19",
                    format!("recursive reference to query \"{}\" {m}", self.name),
                );
            }
            self.selfrefs += 1;
            if self.selfrefs > 1 {
                return pg_error(
                    "42P19",
                    format!(
                        "recursive reference to query \"{}\" must not appear more than once",
                        self.name
                    ),
                );
            }
        }
        Ok(())
    }

    /// The SelectStmt branch of `checkWellFormedRecursionWalker`.
    fn select_with(&mut self, o: &Map<String, Value>) -> R<()> {
        let with = o.get("with_clause").filter(|w| !w.is_null()).cloned();
        let Some(with) = with else {
            return self.select_stmt(o);
        };
        let ctes = cte_list(&with);
        if is_recursive(&with) {
            self.inner
                .push(ctes.iter().map(|(n, _)| n.clone()).collect());
            for (_, q) in &ctes {
                self.walk(q)?;
            }
        } else {
            self.inner.push(Vec::new());
            for (n, q) in &ctes {
                self.walk(q)?;
                if let Some(l) = self.inner.last_mut() {
                    l.push(n.clone());
                }
            }
        }
        let r = self.select_stmt(o);
        self.inner.pop();
        r
    }

    /// `raw_expression_tree_walker` over a SelectStmt's fields.
    fn select_fields(&mut self, o: &Map<String, Value>) -> R<()> {
        for (key, val) in o {
            if key == "with_clause" || val.is_null() {
                continue;
            }
            if key == "larg" || key == "rarg" {
                if let Some(s) = val.as_object() {
                    self.select_with(s)?;
                }
                continue;
            }
            self.walk(val)?;
        }
        Ok(())
    }

    /// `checkWellFormedSelectStmt`.
    fn select_stmt(&mut self, o: &Map<String, Value>) -> R<()> {
        let save = self.ctx;
        if save != Ctx::Ok {
            return self.select_fields(o);
        }
        let all = o.get("all").and_then(Value::as_bool).unwrap_or(false);
        let arm = |c: &mut Self, k: &str| -> R<()> {
            match o.get(k).and_then(Value::as_object) {
                Some(s) => c.select_with(s),
                None => Ok(()),
            }
        };
        // SetOperation: the protobuf's numbering is PostgreSQL's plus one.
        match field_int(o, "op") - 1 {
            2 => {
                if all {
                    self.ctx = Ctx::Intersect;
                }
                arm(self, "larg")?;
                arm(self, "rarg")?;
            }
            3 => {
                if all {
                    self.ctx = Ctx::Except;
                }
                arm(self, "larg")?;
                self.ctx = Ctx::Except;
                arm(self, "rarg")?;
            }
            _ => return self.select_fields(o),
        }
        self.ctx = save;
        for k in [
            "sort_clause",
            "limit_offset",
            "limit_count",
            "locking_clause",
        ] {
            self.walk_field(o, k)?;
        }
        Ok(())
    }
}

/// `checkWellFormedRecursion` for the recursive members of a WITH
/// RECURSIVE, in analysis order.
pub(crate) fn check_well_formed_recursion(
    ctes: &[&pb::CommonTableExpr],
    order: &[(usize, bool)],
) -> R<()> {
    for (i, recursive) in order {
        if !recursive {
            continue;
        }
        let c = ctes[*i];
        let name = c.ctename.as_str();
        let q = to_json(&c.ctequery)?;
        let Some(("SelectStmt", stmt)) = tagged(&q) else {
            return pg_error(
                "42P19",
                format!("recursive query \"{name}\" must not contain data-modifying statements"),
            );
        };
        if field_int(stmt, "op") - 1 != 1 {
            return pg_error(
                "42P19",
                format!(
                    "recursive query \"{name}\" does not have the form non-recursive-term UNION [ALL] recursive-term"
                ),
            );
        }
        let mut st = Chk {
            name,
            inner: Vec::new(),
            selfrefs: 0,
            ctx: Ctx::Sublink,
        };
        if let Some(with) = stmt.get("with_clause").filter(|w| !w.is_null()) {
            if let Some(ctes) = with.get("ctes") {
                st.walk(ctes)?;
            }
        }
        let nonempty = |k: &str| {
            stmt.get(k)
                .is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()))
        };
        if nonempty("sort_clause") {
            return pg_error("0A000", "ORDER BY in a recursive query is not implemented");
        }
        if nonempty("limit_offset") {
            return pg_error("0A000", "OFFSET in a recursive query is not implemented");
        }
        if nonempty("limit_count") {
            return pg_error("0A000", "LIMIT in a recursive query is not implemented");
        }
        if nonempty("locking_clause") {
            return pg_error(
                "0A000",
                "FOR UPDATE/SHARE in a recursive query is not implemented",
            );
        }
        st.inner.clear();
        st.selfrefs = 0;
        st.ctx = Ctx::NonRecursiveTerm;
        if let Some(l) = stmt.get("larg").and_then(Value::as_object) {
            st.select_with(l)?;
        }
        st.inner.clear();
        st.selfrefs = 0;
        st.ctx = Ctx::Ok;
        if let Some(r) = stmt.get("rarg").and_then(Value::as_object) {
            st.select_with(r)?;
        }
        if st.selfrefs != 1 {
            return Err(AnalysisError::new("missing recursive reference".into()));
        }
    }
    Ok(())
}
