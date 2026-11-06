//! The HINT of a missing column's 42703: PostgreSQL's `errorMissingColumn`
//! over `searchRangeTableForCol` (`parse_relation.c`, 15.19).
//!
//! PostgreSQL searches every range table entry of the query level the
//! reference is in, then of each level enclosing it, for the column whose
//! name is closest to the one written (a Levenshtein distance, plus one for
//! each edit between the qualifier written and the entry's alias), and names
//! the closest one, or the two closest when they tie. An entry is in the
//! range table once its FROM item has been analysed, whether or not the
//! reference may see it: `select * from e, (select b) x` hints that `e`'s
//! `b` cannot be referenced from there.
//!
//! This server finds the missing column late, in the lowering, where the
//! range table is gone, so the search is run again over the statement as
//! written: the reference is the column reference at the error's position,
//! and each level's range table is rebuilt from its FROM clause in the order
//! PostgreSQL's parse analysis adds the entries. An entry whose columns are
//! not known here (a function in FROM, RP-413) makes the search give no hint rather
//! than a different one.

use super::*;
use pg_query::protobuf as pb;

/// `MAX_FUZZY_DISTANCE`.
const MAX_FUZZY_DISTANCE: usize = 3;

/// One range table entry: the name it goes by (`eref->aliasname`) and its
/// columns in order (`eref->colnames`); `cols` is `None` when this server
/// cannot tell them.
#[derive(Clone, Debug)]
struct Rte {
    alias: String,
    cols: Option<Vec<String>>,
}

/// The range tables of the query levels a reference sits in, outermost
/// first.
type Levels = Vec<Vec<Rte>>;

/// The column reference at the error's location, with the range tables in
/// force there.
struct Found {
    levels: Levels,
    parts: Vec<String>,
}

/// Why a walk stopped early.
enum Stop {
    Found(Found),
}

type Walk<T> = std::result::Result<T, Stop>;

struct Finder<'a> {
    location: i32,
    /// A relation's columns by name.
    columns: &'a dyn Fn(&str) -> Option<Vec<String>>,
    /// The CTEs in scope, innermost WITH last: `(name, columns)`.
    ctes: Vec<Vec<(String, Option<Vec<String>>)>>,
}

/// The HINT PostgreSQL sends with the 42703 `message` this server raised
/// for `sql`, at `position` (1-based, in characters, as the error's `P`
/// field). `columns` gives a relation's columns by name (a table's or a
/// view's). `None` when PostgreSQL sends none, or when the reference or its
/// range tables cannot be told.
pub fn missing_column_hint(
    sql: &str,
    position: usize,
    message: &str,
    columns: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Option<String> {
    let byte = sql.char_indices().nth(position.checked_sub(1)?)?.0;
    let mut node = pb::Node {
        node: Some(parse_one_as_sent(sql).ok()?),
    };
    let mut finder = Finder {
        location: i32::try_from(byte).ok()?,
        columns,
        ctes: Vec::new(),
    };
    let found = match finder.statement(&mut node) {
        Err(Stop::Found(f)) => f,
        Ok(()) => return None,
    };
    let (alias, name) = match found.parts.as_slice() {
        [name] => (None, name.as_str()),
        [.., alias, name] => (Some(alias.as_str()), name.as_str()),
        [] => return None,
    };
    // The reference found must be the one the message is about.
    let expected = match alias {
        Some(a) => format!("column {a}.{name} does not exist"),
        None => format!("column \"{name}\" does not exist"),
    };
    if message != expected {
        return None;
    }
    hint_for(&found.levels, alias, name)
}

/// `errorMissingColumn`'s hint, from `searchRangeTableForCol`'s answer.
fn hint_for(levels: &Levels, alias: Option<&str>, colname: &str) -> Option<String> {
    let state = search_range_tables(levels, alias, colname)?;
    let first = state.first.as_ref();
    match (state.rfirst.as_ref(), state.rsecond.as_ref()) {
        (Some(rfirst), None) => Some(match first {
            Some(closest) => {
                format!("Perhaps you meant to reference the column \"{rfirst}.{closest}\".")
            }
            None => format!(
                "There is a column named \"{colname}\" in table \"{rfirst}\", but it cannot be referenced from this part of the query."
            ),
        }),
        (Some(rfirst), Some(rsecond)) => Some(format!(
            "Perhaps you meant to reference the column \"{rfirst}.{}\" or the column \"{rsecond}.{}\".",
            first?,
            state.second.as_ref()?
        )),
        _ => None,
    }
}

/// `FuzzyAttrMatchState`, the entries named by alias and the columns by
/// name (`None`: `InvalidAttrNumber`).
struct FuzzyState {
    distance: usize,
    rfirst: Option<String>,
    first: Option<String>,
    rsecond: Option<String>,
    second: Option<String>,
}

/// `searchRangeTableForCol`. `None` when an entry the search reaches has
/// columns this server cannot tell.
fn search_range_tables(levels: &Levels, alias: Option<&str>, colname: &str) -> Option<FuzzyState> {
    let mut state = FuzzyState {
        distance: MAX_FUZZY_DISTANCE + 1,
        rfirst: None,
        first: None,
        rsecond: None,
        second: None,
    };
    for level in levels.iter().rev() {
        for rte in level {
            let cols = rte.cols.as_ref()?;
            let penalty = match alias {
                Some(a) => levenshtein(a, &rte.alias),
                None => 0,
            };
            // `scanRTEForColumn`: every column updates the fuzzy state, and
            // an exact match in an entry the qualifier names exactly ends
            // the search.
            let mut exact = false;
            for col in cols {
                if col == colname {
                    exact = true;
                }
                update_fuzzy_state(penalty, &mut state, &rte.alias, col, colname);
            }
            if exact && penalty == 0 {
                state.rfirst = Some(rte.alias.clone());
                state.first = None;
                state.rsecond = None;
                state.second = None;
                return Some(state);
            }
        }
    }
    Some(state)
}

/// `updateFuzzyAttrMatchState`.
fn update_fuzzy_state(
    penalty: usize,
    state: &mut FuzzyState,
    rte: &str,
    actual: &str,
    wanted: &str,
) {
    if penalty > state.distance {
        return;
    }
    // A dropped column has an empty name.
    if actual.is_empty() {
        return;
    }
    // `varstr_levenshtein_less_equal` gives up past its bound with a value
    // past it, which the comparisons below reject as the exact distance
    // would be.
    let distance = levenshtein(actual, wanted);
    // More than half the characters different is no match (bytes, as
    // `strlen`).
    if distance > wanted.len() / 2 {
        return;
    }
    let distance = distance + penalty;
    if distance < state.distance {
        state.distance = distance;
        state.rfirst = Some(rte.to_string());
        state.first = Some(actual.to_string());
        state.rsecond = None;
        state.second = None;
    } else if distance == state.distance {
        if state.second.is_some() {
            // Too many matches at this distance.
            state.rfirst = None;
            state.first = None;
            state.rsecond = None;
            state.second = None;
        } else if state.first.is_some() {
            state.rsecond = Some(rte.to_string());
            state.second = Some(actual.to_string());
        } else if state.distance <= MAX_FUZZY_DISTANCE {
            // The first match was invalidated (or was exact): this one
            // takes its place.
            state.rfirst = Some(rte.to_string());
            state.first = Some(actual.to_string());
            state.rsecond = None;
            state.second = None;
        }
    }
}

/// `varstr_levenshtein` with every edit costing one, over characters.
fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn strings(nodes: &[pb::Node]) -> Vec<String> {
    nodes
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// `cols` renamed by an alias's column list, which names the leading ones.
fn renamed(cols: Option<Vec<String>>, alias: Option<&pb::Alias>) -> Option<Vec<String>> {
    let mut cols = cols?;
    if let Some(a) = alias {
        for (i, name) in strings(&a.colnames).into_iter().enumerate() {
            if let Some(c) = cols.get_mut(i) {
                *c = name;
            }
        }
    }
    Some(cols)
}

fn with_level(levels: &Levels, rtable: &[Rte]) -> Levels {
    let mut out = levels.clone();
    out.push(rtable.to_vec());
    out
}

impl Finder<'_> {
    fn statement(&mut self, node: &mut pb::Node) -> Walk<()> {
        match node.node.as_mut() {
            Some(N::SelectStmt(s)) => self.select(s, &Vec::new()).map(|_| ()),
            Some(N::InsertStmt(i)) => self.insert(i),
            Some(N::UpdateStmt(u)) => self.update(u),
            Some(N::DeleteStmt(d)) => self.delete(d),
            Some(N::ExplainStmt(e)) => match e.query.as_deref_mut() {
                Some(q) => self.statement(q),
                None => Ok(()),
            },
            Some(N::DeclareCursorStmt(d)) => match d.query.as_deref_mut() {
                Some(q) => self.statement(q),
                None => Ok(()),
            },
            Some(N::CreateTableAsStmt(c)) => match c.query.as_deref_mut() {
                Some(q) => self.statement(q),
                None => Ok(()),
            },
            Some(N::ViewStmt(v)) => match v.query.as_deref_mut() {
                Some(q) => self.statement(q),
                None => Ok(()),
            },
            _ => Ok(()),
        }
    }

    /// The expressions of `node` at `levels`, and the subqueries inside it
    /// one level down.
    fn expr(&mut self, node: &mut pb::Node, levels: &Levels) -> Walk<()> {
        let mut stop: Option<Stop> = None;
        let location = self.location;
        let _ = walk_expr(node, &mut |n| {
            match n.node.as_mut() {
                Some(N::ColumnRef(c)) if c.location == location => {
                    stop = Some(Stop::Found(Found {
                        levels: levels.clone(),
                        parts: strings(&c.fields),
                    }));
                }
                Some(N::SubLink(sl)) => {
                    if let Some(N::SelectStmt(body)) =
                        sl.subselect.as_deref_mut().and_then(|q| q.node.as_mut())
                    {
                        if let Err(s) = self.select(body, levels) {
                            stop = Some(s);
                        }
                    }
                }
                _ => {}
            }
            match stop {
                // Any error ends the walk; the stop says why.
                Some(_) => Err(Error::Internal(String::new())),
                None => Ok(()),
            }
        });
        match stop {
            Some(s) => Err(s),
            None => Ok(()),
        }
    }

    fn exprs<'n>(
        &mut self,
        nodes: impl IntoIterator<Item = &'n mut pb::Node>,
        levels: &Levels,
    ) -> Walk<()> {
        for n in nodes {
            self.expr(n, levels)?;
        }
        Ok(())
    }

    /// The CTEs of a WITH, each analysed at `levels` plus the empty range
    /// table of the query that owns it, and then in scope.
    fn with(&mut self, with: &mut pb::WithClause, levels: &Levels) -> Walk<()> {
        self.ctes.push(Vec::new());
        let inner = with_level(levels, &[]);
        for cte in &mut with.ctes {
            let Some(N::CommonTableExpr(c)) = cte.node.as_mut() else {
                continue;
            };
            let cols = match c.ctequery.as_deref_mut().and_then(|q| q.node.as_mut()) {
                Some(N::SelectStmt(body)) => self.select(body, &inner)?,
                _ => None,
            };
            let names = strings(&c.aliascolnames);
            let cols = if names.is_empty() {
                cols
            } else {
                cols.map(|mut cols| {
                    for (i, n) in names.into_iter().enumerate() {
                        if let Some(c) = cols.get_mut(i) {
                            *c = n;
                        }
                    }
                    cols
                })
            };
            if let Some(scope) = self.ctes.last_mut() {
                scope.push((c.ctename.clone(), cols));
            }
        }
        Ok(())
    }

    fn cte(&self, name: &str) -> Option<Option<Vec<String>>> {
        self.ctes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .find(|(n, _)| n == name)
            .map(|(_, cols)| cols.clone())
    }

    fn relation(&self, r: &pb::RangeVar) -> Rte {
        let alias = r
            .alias
            .as_ref()
            .map(|a| a.aliasname.clone())
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| r.relname.clone());
        let cols = match self.cte(&r.relname).filter(|_| r.schemaname.is_empty()) {
            Some(cols) => cols,
            None => (self.columns)(&relation_name(r)),
        };
        Rte {
            alias,
            cols: renamed(cols, r.alias.as_ref()),
        }
    }

    /// One FROM item, analysed with the entries before it in `rtable`, which
    /// it then adds its own entries to (a join adds its sides', not its own:
    /// the search skips join entries).
    fn range_item(
        &mut self,
        item: &mut pb::Node,
        levels: &Levels,
        rtable: &mut Vec<Rte>,
    ) -> Walk<()> {
        match item.node.as_mut() {
            Some(N::RangeVar(r)) => rtable.push(self.relation(r)),
            Some(N::JoinExpr(j)) => {
                if let Some(l) = j.larg.as_deref_mut() {
                    self.range_item(l, levels, rtable)?;
                }
                if let Some(r) = j.rarg.as_deref_mut() {
                    self.range_item(r, levels, rtable)?;
                }
                if let Some(q) = j.quals.as_deref_mut() {
                    self.expr(q, &with_level(levels, rtable))?;
                }
            }
            Some(N::RangeSubselect(rs)) => {
                let cols = match rs.subquery.as_deref_mut().and_then(|q| q.node.as_mut()) {
                    Some(N::SelectStmt(body)) => self.select(body, &with_level(levels, rtable))?,
                    _ => None,
                };
                rtable.push(Rte {
                    alias: rs
                        .alias
                        .as_ref()
                        .map(|a| a.aliasname.clone())
                        .unwrap_or_default(),
                    cols: renamed(cols, rs.alias.as_ref()),
                });
            }
            Some(N::RangeFunction(rf)) => {
                let inner = with_level(levels, rtable);
                for f in &mut rf.functions {
                    self.expr(f, &inner)?;
                }
                // Its columns depend on the function's result type.
                rtable.push(Rte {
                    alias: String::new(),
                    cols: None,
                });
            }
            Some(N::RangeTableSample(ts)) => {
                match ts.relation.as_deref().and_then(|r| r.node.as_ref()) {
                    Some(N::RangeVar(r)) => rtable.push(self.relation(r)),
                    _ => rtable.push(Rte {
                        alias: String::new(),
                        cols: None,
                    }),
                }
            }
            _ => rtable.push(Rte {
                alias: String::new(),
                cols: None,
            }),
        }
        Ok(())
    }

    /// A SELECT at `levels` (the levels enclosing it); its output column
    /// names, when they can be told.
    fn select(&mut self, s: &mut pb::SelectStmt, levels: &Levels) -> Walk<Option<Vec<String>>> {
        let scoped = s.with_clause.is_some();
        if let Some(with) = s.with_clause.as_mut() {
            self.with(with, levels)?;
        }
        let out = if s.op != pb::SetOperation::SetopNone as i32 {
            self.set_operation(s, levels)
        } else {
            self.plain_select(s, levels)
        };
        if scoped {
            self.ctes.pop();
        }
        out
    }

    /// `transformSetOperationTree`: each leaf is analysed as a subquery of
    /// the set operation, whose range table gains a `*SELECT* n` entry per
    /// leaf as it goes.
    fn set_operation(
        &mut self,
        s: &mut pb::SelectStmt,
        levels: &Levels,
    ) -> Walk<Option<Vec<String>>> {
        let mut rtable = Vec::new();
        let out = self.set_operation_tree(s, levels, &mut rtable)?;
        // Its ORDER BY and LIMIT read the result's columns, which this
        // search does not model: a reference there is not found.
        Ok(out)
    }

    fn set_operation_tree(
        &mut self,
        s: &mut pb::SelectStmt,
        levels: &Levels,
        rtable: &mut Vec<Rte>,
    ) -> Walk<Option<Vec<String>>> {
        let mut first = None;
        for (k, side) in [s.larg.as_deref_mut(), s.rarg.as_deref_mut()]
            .into_iter()
            .flatten()
            .enumerate()
        {
            // A parenthesised set operation with its own ORDER BY, LIMIT or
            // WITH is a leaf like any SELECT.
            let leaf = side.op == pb::SetOperation::SetopNone as i32
                || !side.sort_clause.is_empty()
                || side.limit_count.is_some()
                || side.limit_offset.is_some()
                || side.with_clause.is_some()
                || !side.locking_clause.is_empty();
            let cols = if leaf {
                let cols = self.select(side, &with_level(levels, rtable))?;
                rtable.push(Rte {
                    alias: format!("*SELECT* {}", rtable.len() + 1),
                    cols: cols.clone(),
                });
                cols
            } else {
                self.set_operation_tree(side, levels, rtable)?
            };
            if k == 0 {
                first = cols;
            }
        }
        Ok(first)
    }

    fn plain_select(
        &mut self,
        s: &mut pb::SelectStmt,
        levels: &Levels,
    ) -> Walk<Option<Vec<String>>> {
        if !s.values_lists.is_empty() {
            let here = with_level(levels, &[]);
            let width = match s.values_lists.first().and_then(|l| l.node.as_ref()) {
                Some(N::List(l)) => l.items.len(),
                _ => 0,
            };
            self.exprs(s.values_lists.iter_mut(), &here)?;
            return Ok(Some((1..=width).map(|i| format!("column{i}")).collect()));
        }
        let mut rtable = Vec::new();
        for item in &mut s.from_clause {
            self.range_item(item, levels, &mut rtable)?;
        }
        let here = with_level(levels, &rtable);
        self.exprs(s.target_list.iter_mut(), &here)?;
        self.exprs(s.where_clause.as_deref_mut(), &here)?;
        self.exprs(s.group_clause.iter_mut(), &here)?;
        self.exprs(s.having_clause.as_deref_mut(), &here)?;
        self.exprs(s.window_clause.iter_mut(), &here)?;
        self.exprs(s.sort_clause.iter_mut(), &here)?;
        self.exprs(s.distinct_clause.iter_mut(), &here)?;
        self.exprs(s.limit_offset.as_deref_mut(), &here)?;
        self.exprs(s.limit_count.as_deref_mut(), &here)?;
        Ok(output_columns(s, &rtable))
    }

    fn insert(&mut self, i: &mut pb::InsertStmt) -> Walk<()> {
        let scoped = i.with_clause.is_some();
        if let Some(with) = i.with_clause.as_mut() {
            self.with(with, &Vec::new())?;
        }
        let Some(target) = i.relation.as_ref().map(|r| self.relation(r)) else {
            return Ok(());
        };
        let mut here: Levels = vec![vec![target.clone()]];
        if let Some(N::SelectStmt(body)) =
            i.select_stmt.as_deref_mut().and_then(|q| q.node.as_mut())
        {
            // A VALUES list is analysed in the INSERT's own level; any other
            // query as a subquery under it.
            if body.values_lists.is_empty() {
                self.select(body, &here)?;
            } else {
                self.exprs(body.values_lists.iter_mut(), &here)?;
            }
        }
        // `transformOnConflictClause` adds `excluded` to the range table for
        // DO UPDATE, where its SET and WHERE and then RETURNING see it.
        if let Some(oc) = i.on_conflict_clause.as_deref_mut() {
            if oc.action == pb::OnConflictAction::OnconflictUpdate as i32 {
                here[0].push(Rte {
                    alias: "excluded".into(),
                    cols: target.cols.clone(),
                });
                self.exprs(oc.target_list.iter_mut(), &here)?;
                self.exprs(oc.where_clause.as_deref_mut(), &here)?;
            }
        }
        self.exprs(i.returning_list.iter_mut(), &here)?;
        if scoped {
            self.ctes.pop();
        }
        Ok(())
    }

    fn update(&mut self, u: &mut pb::UpdateStmt) -> Walk<()> {
        let scoped = u.with_clause.is_some();
        if let Some(with) = u.with_clause.as_mut() {
            self.with(with, &Vec::new())?;
        }
        let Some(target) = u.relation.as_ref().map(|r| self.relation(r)) else {
            return Ok(());
        };
        let mut rtable = vec![target];
        for item in &mut u.from_clause {
            self.range_item(item, &Vec::new(), &mut rtable)?;
        }
        let here: Levels = vec![rtable];
        self.exprs(u.target_list.iter_mut(), &here)?;
        self.exprs(u.where_clause.as_deref_mut(), &here)?;
        self.exprs(u.returning_list.iter_mut(), &here)?;
        if scoped {
            self.ctes.pop();
        }
        Ok(())
    }

    fn delete(&mut self, d: &mut pb::DeleteStmt) -> Walk<()> {
        let scoped = d.with_clause.is_some();
        if let Some(with) = d.with_clause.as_mut() {
            self.with(with, &Vec::new())?;
        }
        let Some(target) = d.relation.as_ref().map(|r| self.relation(r)) else {
            return Ok(());
        };
        let mut rtable = vec![target];
        for item in &mut d.using_clause {
            self.range_item(item, &Vec::new(), &mut rtable)?;
        }
        let here: Levels = vec![rtable];
        self.exprs(d.where_clause.as_deref_mut(), &here)?;
        self.exprs(d.returning_list.iter_mut(), &here)?;
        if scoped {
            self.ctes.pop();
        }
        Ok(())
    }
}

/// A plain SELECT's output column names (`FigureColname`, a star expanded
/// over the entries it reads), when they can be told.
fn output_columns(s: &pb::SelectStmt, rtable: &[Rte]) -> Option<Vec<String>> {
    // A star over a USING or NATURAL join reads the join's merged columns
    // first, which the entries alone do not give.
    let merged = |n: &pb::Node| -> bool {
        fn any(n: &pb::Node) -> bool {
            match n.node.as_ref() {
                Some(N::JoinExpr(j)) => {
                    j.is_natural
                        || !j.using_clause.is_empty()
                        || j.larg.as_deref().is_some_and(any)
                        || j.rarg.as_deref().is_some_and(any)
                }
                _ => false,
            }
        }
        any(n)
    };
    let mut out = Vec::new();
    for t in &s.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return None;
        };
        if !rt.name.is_empty() {
            out.push(rt.name.clone());
            continue;
        }
        let val = rt.val.as_deref()?;
        if let Some(N::ColumnRef(c)) = val.node.as_ref() {
            if matches!(
                c.fields.last().and_then(|f| f.node.as_ref()),
                Some(N::AStar(_))
            ) {
                let parts = strings(&c.fields);
                match parts.last() {
                    None => {
                        if s.from_clause.iter().any(merged) {
                            return None;
                        }
                        for rte in rtable {
                            out.extend(rte.cols.clone()?);
                        }
                    }
                    Some(alias) => {
                        let rte = rtable.iter().find(|r| &r.alias == alias)?;
                        out.extend(rte.cols.clone()?);
                    }
                }
                continue;
            }
        }
        out.push(nodetostring::target::figure_colname(val));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rte(alias: &str, cols: &[&str]) -> Rte {
        Rte {
            alias: alias.into(),
            cols: Some(cols.iter().map(|c| c.to_string()).collect()),
        }
    }

    #[test]
    fn levenshtein_counts_single_edits() {
        assert_eq!(levenshtein("bb", "b"), 1);
        assert_eq!(levenshtein("gama", "gamma"), 1);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("é", "e"), 1);
    }

    // Each answer below is PostgreSQL 15.19's, measured on a private
    // cluster with `e(a int, b text)`, `s(a int, c text)`,
    // `k(a int, b text, bc int)` and `w(alpha int, beta int, gamma int)`.
    #[test]
    fn a_qualifier_naming_the_wrong_table_points_at_the_other() {
        let levels = vec![vec![rte("e", &["a", "b"]), rte("s", &["a", "c"])]];
        assert_eq!(
            hint_for(&levels, Some("s"), "b").as_deref(),
            Some("Perhaps you meant to reference the column \"e.b\".")
        );
    }

    #[test]
    fn two_columns_at_one_distance_are_both_named() {
        let levels = vec![vec![rte("e", &["a", "b"]), rte("s", &["a", "c"])]];
        assert_eq!(
            hint_for(&levels, None, "bc").as_deref(),
            Some("Perhaps you meant to reference the column \"e.b\" or the column \"s.c\".")
        );
    }

    #[test]
    fn three_columns_at_one_distance_name_none() {
        let levels = vec![vec![rte("e", &["a", "b"]), rte("k", &["a", "b", "bc"])]];
        assert_eq!(hint_for(&levels, None, "bx"), None);
    }

    #[test]
    fn more_than_half_the_name_different_is_no_match() {
        let levels = vec![vec![rte("w", &["alpha", "beta", "gamma"])]];
        assert_eq!(hint_for(&levels, None, "alp"), None);
        assert_eq!(
            hint_for(&levels, None, "alphx").as_deref(),
            Some("Perhaps you meant to reference the column \"w.alpha\".")
        );
    }

    #[test]
    fn an_exact_match_out_of_reach_is_named_as_such() {
        let levels = vec![vec![rte("e", &["a", "b"])], vec![]];
        assert_eq!(
            hint_for(&levels, None, "b").as_deref(),
            Some("There is a column named \"b\" in table \"e\", but it cannot be referenced from this part of the query.")
        );
    }

    #[test]
    fn the_inner_level_is_searched_first() {
        let levels = vec![vec![rte("e", &["a", "b"])], vec![rte("s", &["a", "c"])]];
        assert_eq!(
            hint_for(&levels, None, "aa").as_deref(),
            Some("Perhaps you meant to reference the column \"s.a\" or the column \"e.a\".")
        );
    }

    #[test]
    fn an_entry_of_unknown_columns_gives_no_hint() {
        let levels = vec![vec![
            rte("e", &["a", "b"]),
            Rte {
                alias: "g".into(),
                cols: None,
            },
        ]];
        assert_eq!(hint_for(&levels, None, "bb"), None);
    }
}
