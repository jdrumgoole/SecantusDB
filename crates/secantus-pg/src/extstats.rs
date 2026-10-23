//! Extended statistics: what ANALYZE builds for each `CREATE STATISTICS`
//! object (`BuildRelationExtStatistics`, PostgreSQL 15.19), and the
//! catalogs that show it -- `pg_statistic_ext_data`, `pg_stats_ext`,
//! `pg_stats_ext_exprs` (RP-099).
//!
//! An object's dimensions are its columns in attnum order, then its
//! expressions, numbered -1, -2, ... as PostgreSQL numbers them. ANALYZE
//! reads every row (a table past its sample is not measured here, as for
//! `pg_statistic`), and builds what the object's kinds ask for:
//! - `ndistinct`: each combination of two or more dimensions, its distinct
//!   rows estimated by the Duj1 estimator (`statext_ndistinct_build`);
//! - `dependencies`: each functional dependency's degree, the share of
//!   rows in groups that agree on it (`statext_dependencies_build`);
//! - `mcv`: the most common combinations, ordered by frequency as
//!   PostgreSQL's quicksort leaves them (`statext_mcv_build`);
//! - an expression's own statistics, as a column's.
//!
//! Values compare as `ORDER BY` compares them, NULLs last; the text forms
//! and an MCV list's bytes are `secantus_pgplan::extstats`'.

use std::cmp::Ordering;

use super::*;
use secantus_pgplan::extstats::{self as ext, McvItem};

pub(crate) const STATEXT_DATA_COLLECTION: &str = "__sql_statext_data__";

/// `WIDTH_THRESHOLD`: a value wider than this is left out of the sorted
/// sample (`build_sorted_items`).
const WIDTH_THRESHOLD: usize = 1024;

/// One dimension of a statistics object over the sampled rows.
struct Dim {
    attnum: i16,
    values: Vec<Option<Bson>>,
    /// The row's value is too wide to sort (`build_sorted_items`).
    wide: Vec<bool>,
}

fn compare(a: &Option<Bson>, b: &Option<Bson>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => secantus_pgplan::compare_values(a, b).unwrap_or(Ordering::Equal),
    }
}

fn compare_rows(dims: &[&Dim], i: usize, j: usize) -> Ordering {
    for d in dims {
        let o = compare(&d.values[i], &d.values[j]);
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

/// The rows of the sample no dimension of `dims` is too wide in, sorted
/// by them (`build_sorted_items`, PostgreSQL's quicksort).
fn sorted_rows(dims: &[&Dim], numrows: usize) -> Vec<usize> {
    let mut rows: Vec<usize> = (0..numrows)
        .filter(|r| !dims.iter().any(|d| d.wide[*r]))
        .collect();
    crate::window_order::pg_qsort(&mut rows, &mut |a, b| compare_rows(dims, *a, *b));
    rows
}

/// `estimate_ndistinct`: the Duj1 estimator.
fn estimate_ndistinct(totalrows: f64, numrows: usize, d: usize, f1: usize) -> f64 {
    let n = numrows as f64;
    let numer = n * d as f64;
    let denom = (n - f1 as f64) + f1 as f64 * n / totalrows;
    let nd = (numer / denom).max(d as f64).min(totalrows);
    (nd + 0.5).floor()
}

/// `statext_ndistinct_build`.
fn ndistinct(dims: &[Dim], numrows: usize, totalrows: f64) -> Vec<(Vec<i16>, f64)> {
    let mut out = Vec::new();
    for k in 2..=dims.len() {
        for combo in combinations(dims.len(), k) {
            let sel: Vec<&Dim> = combo.iter().map(|i| &dims[*i]).collect();
            // Every row counts here, however wide.
            let mut rows: Vec<usize> = (0..numrows).collect();
            rows.sort_by(|a, b| compare_rows(&sel, *a, *b));
            let (mut f1, mut cnt, mut d) = (0, 1, 1);
            for w in rows.windows(2) {
                if compare_rows(&sel, w[1], w[0]) != Ordering::Equal {
                    if cnt == 1 {
                        f1 += 1;
                    }
                    d += 1;
                    cnt = 0;
                }
                cnt += 1;
            }
            if cnt == 1 {
                f1 += 1;
            }
            let attrs = combo.iter().map(|i| dims[*i].attnum).collect();
            out.push((attrs, estimate_ndistinct(totalrows, numrows, d, f1)));
        }
    }
    out
}

/// The `k`-combinations of `0..n` in `generate_combinations`' order.
fn combinations(n: usize, k: usize) -> Vec<Vec<usize>> {
    fn go(n: usize, k: usize, start: usize, cur: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if cur.len() == k {
            out.push(cur.clone());
            return;
        }
        for i in start..n {
            cur.push(i);
            go(n, k, i + 1, cur, out);
            cur.pop();
        }
    }
    let mut out = Vec::new();
    go(n, k, 0, &mut Vec::new(), &mut out);
    out
}

/// The dependencies of `k` of `n` dimensions in `generate_dependencies`'
/// order: an increasing combination of `k - 1`, then each other one.
fn dependency_orders(n: usize, k: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    for mut lead in combinations(n, k - 1) {
        for last in 0..n {
            if !lead.contains(&last) {
                lead.push(last);
                out.push(lead.clone());
                lead.pop();
            }
        }
    }
    out
}

/// `statext_dependencies_build`: each dependency's degree, those of 0 left
/// out.
fn dependencies(dims: &[Dim], numrows: usize) -> Vec<(Vec<i16>, f64)> {
    let mut out = Vec::new();
    for k in 2..=dims.len() {
        for order in dependency_orders(dims.len(), k) {
            let sel: Vec<&Dim> = order.iter().map(|i| &dims[*i]).collect();
            let rows = sorted_rows(&sel, numrows);
            let (lead, last) = sel.split_at(k - 1);
            let mut supporting = 0usize;
            let mut group = 1usize;
            let mut violations = 0usize;
            for i in 1..=rows.len() {
                if i == rows.len() || compare_rows(lead, rows[i - 1], rows[i]) != Ordering::Equal {
                    if violations == 0 {
                        supporting += group;
                    }
                    violations = 0;
                    group = 1;
                    continue;
                } else if compare_rows(last, rows[i - 1], rows[i]) != Ordering::Equal {
                    violations += 1;
                }
                group += 1;
            }
            if rows.is_empty() {
                supporting = 0;
            }
            let degree = supporting as f64 / numrows as f64;
            if degree == 0.0 {
                continue;
            }
            out.push((order.iter().map(|i| dims[*i].attnum).collect(), degree));
        }
    }
    out
}

/// `get_mincount_for_mcv_list`.
fn mincount(samplerows: f64, totalrows: f64) -> f64 {
    let (n, nn) = (samplerows, totalrows);
    let denom = nn - n + 0.04 * n * (nn - 1.0);
    if denom == 0.0 {
        return 0.0;
    }
    n * (nn - n) / denom
}

/// `statext_mcv_build`.
fn mcv(dims: &[Dim], numrows: usize, totalrows: f64, target: usize) -> Vec<McvItem> {
    let all: Vec<&Dim> = dims.iter().collect();
    let rows = sorted_rows(&all, numrows);
    if rows.is_empty() {
        return Vec::new();
    }
    // build_distinct_groups: (first row, count), then by count, largest
    // first, as PostgreSQL's quicksort leaves equal counts.
    let mut groups: Vec<(usize, usize)> = vec![(rows[0], 1)];
    for w in rows.windows(2) {
        if compare_rows(&all, w[1], w[0]) != Ordering::Equal {
            groups.push((w[1], 0));
        }
        groups.last_mut().expect("one group").1 += 1;
    }
    crate::window_order::pg_qsort(&mut groups, &mut |a, b| b.1.cmp(&a.1));
    let floor = mincount(numrows as f64, totalrows);
    let mut nitems = target.min(groups.len());
    if let Some(i) = groups[..nitems].iter().position(|g| (g.1 as f64) < floor) {
        nitems = i;
    }
    // build_column_frequencies: each value's count over every group.
    let freq = |dim: &Dim, v: &Option<Bson>| -> usize {
        groups
            .iter()
            .filter(|(r, _)| compare(&dim.values[*r], v) == Ordering::Equal)
            .map(|(_, c)| c)
            .sum()
    };
    groups[..nitems]
        .iter()
        .map(|(r, count)| {
            let values: Vec<Option<Bson>> = dims.iter().map(|d| d.values[*r].clone()).collect();
            let base = dims
                .iter()
                .zip(&values)
                .map(|(d, v)| freq(d, v) as f64 / numrows as f64)
                .product();
            McvItem {
                values,
                frequency: *count as f64 / numrows as f64,
                base_frequency: base,
            }
        })
        .collect()
}

fn too_wide(v: &Option<Bson>) -> bool {
    match v {
        Some(Bson::String(s)) => s.len() + if s.len() < 127 { 1 } else { 4 } > WIDTH_THRESHOLD,
        Some(Bson::Binary(b)) => b.bytes.len() + 4 > WIDTH_THRESHOLD,
        _ => false,
    }
}

fn strings(d: &Document, key: &str) -> Vec<String> {
    d.get_array(key)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

impl PgHandler {
    /// A statistics object's oid, as `pg_statistic_ext` gives it.
    fn statistics_oid(name: &str) -> i64 {
        Self::index_oid(&format!("stx:{name}"))
    }

    /// What ANALYZE built for the statistics object `name`: its own
    /// statistics, then its inherited ones, when it has them.
    fn statext_data(&self, name: &str) -> Vec<Document> {
        let mut out: Vec<Document> = self
            .storage
            .find_matching(
                self.db(),
                STATEXT_DATA_COLLECTION,
                &bson::doc! {"object": name},
            )
            .unwrap_or_default()
            .iter()
            .filter_map(|b| decode_doc(b).ok())
            .collect();
        out.sort_by_key(|d| d.get_bool("inherited").unwrap_or(false));
        out
    }

    /// Forget (or, with `to`, move) what ANALYZE built for `name`: the
    /// object was dropped or renamed.
    pub(crate) fn forget_statext_data(&self, name: &str, to: Option<&str>) -> PgWireResult<()> {
        for mut doc in self.statext_data(name) {
            let key = doc.get_str("_id").unwrap_or_default().to_string();
            self.put_comment_doc(STATEXT_DATA_COLLECTION, &key, None)?;
            if let Some(to) = to {
                let moved = key.replacen(name, to, 1);
                doc.insert("_id", &moved);
                doc.insert("object", to);
                self.put_comment_doc(STATEXT_DATA_COLLECTION, &moved, Some(doc))?;
            }
        }
        Ok(())
    }

    /// An object's dimensions' columns (attnum order) and expressions.
    fn statistics_dims(
        def: &TableDef,
        object: &Document,
    ) -> (Vec<(i16, secantus_pgcatalog::Column)>, Vec<String>) {
        let mut cols: Vec<(i16, secantus_pgcatalog::Column)> = strings(object, "columns")
            .iter()
            .filter_map(|n| {
                let i = def.columns.iter().position(|c| &c.name == n)?;
                Some((def.attnum(i), def.columns[i].clone()))
            })
            .collect();
        cols.sort_by_key(|(a, _)| *a);
        (cols, strings(object, "exprs"))
    }

    /// `BuildRelationExtStatistics` for `table`, over its rows `docs`:
    /// each statistics object on it built and recorded. With `columns`
    /// (`ANALYZE t (a, b)`), an object over a column not analysed is not
    /// built, with PostgreSQL's warning.
    pub(crate) fn build_extended_statistics(
        &self,
        table: &str,
        def: &TableDef,
        docs: &[Document],
        columns: &[String],
        target: usize,
    ) -> PgWireResult<()> {
        // In creation order, as PostgreSQL reads them off `pg_statistic_ext`.
        let mut objects: Vec<Document> = self
            .catalog_docs(catalog_objects::STATISTICS_COLLECTION)
            .into_iter()
            .filter(|d| d.get_str("table") == Ok(table))
            .collect();
        objects.sort_by_key(|d| d.get_i64("seq").unwrap_or(0));
        for object in objects {
            let name = object.get_str("_id").unwrap_or_default().to_string();
            let kinds = strings(&object, "kinds");
            let (cols, exprs) = Self::statistics_dims(def, &object);
            let planned = exprs
                .iter()
                .map(|e| secantus_pgplan::plan_check_expression(e, def).map_err(|e| Self::err(&e)))
                .collect::<PgWireResult<Vec<_>>>()?;
            // Every column an object reads -- its own, and its
            // expressions' -- must be among those analysed.
            if !columns.is_empty() {
                let mut read: Vec<String> = cols.iter().map(|(_, c)| c.name.clone()).collect();
                for e in &exprs {
                    for c in &def.columns {
                        if e.split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                            .any(|w| w.eq_ignore_ascii_case(&c.name))
                        {
                            read.push(c.name.clone());
                        }
                    }
                }
                if !read.iter().all(|c| columns.contains(c)) {
                    self.warning(
                        "01000",
                        format!(
                            "statistics object \"public.{name}\" could not be computed for relation \"public.{table}\""
                        ),
                    );
                    continue;
                }
            }
            // The table's own rows; for an inheritance parent also its
            // tree's, built as inherited statistics. A partitioned table has
            // no rows of its own: its statistics are its tree's.
            let partitioned = crate::partition::is_partitioned(def);
            let mut variants: Vec<(Vec<Document>, bool, String)> =
                vec![(docs.to_vec(), partitioned, name.clone())];
            let descendants = secantus_pgplan::inherit::descendants(table);
            if !partitioned && !descendants.is_empty() {
                let mut all = docs.to_vec();
                for child in &descendants {
                    let Some(cdef) = self.lookup(child) else {
                        continue;
                    };
                    for row in self.table_docs(child)? {
                        // The child's values under the parent's fields.
                        let mut mapped = Document::new();
                        for c in &def.columns {
                            if let Some(v) = cdef.field_of(&c.name).and_then(|f| row.get(&f)) {
                                mapped.insert(c.field(), v.clone());
                            }
                        }
                        all.push(mapped);
                    }
                }
                variants.push((all, true, format!("{name}\u{1}inh")));
            }
            for (rows, inherited, key) in variants {
                let docs: &[Document] = &rows;
                let numrows = docs.len();
                if numrows == 0 {
                    continue;
                }
                let mut dims: Vec<Dim> = cols
                    .iter()
                    .map(|(attnum, c)| {
                        let field = c.field();
                        let values: Vec<Option<Bson>> = docs
                            .iter()
                            .map(|d| d.get(&field).filter(|v| !matches!(v, Bson::Null)).cloned())
                            .collect();
                        Dim {
                            attnum: *attnum,
                            wide: values.iter().map(too_wide).collect(),
                            values,
                        }
                    })
                    .collect();
                let mut types: Vec<String> = cols.iter().map(|(_, c)| c.pg_type.clone()).collect();
                let mut expr_entries: Vec<Bson> = Vec::new();
                for (i, p) in planned.iter().enumerate() {
                    let values: Vec<Option<Bson>> = docs
                        .iter()
                        .map(|d| {
                            secantus_pgplan::apply_row_expr(p, d)
                                .map(|v| (!matches!(v, Bson::Null)).then_some(v))
                                .map_err(|e| Self::err(&e))
                        })
                        .collect::<PgWireResult<_>>()?;
                    let ty = secantus_pgplan::column_expr_type(p).to_string();
                    if kinds.iter().any(|k| k == "e") {
                        let col = secantus_pgcatalog::Column::new("expr", &ty, false);
                        let refs: Vec<Option<&Bson>> = values.iter().map(Option::as_ref).collect();
                        let mut entry = Self::stats_entry(&col, &refs, target, true);
                        entry.insert("type", &ty);
                        entry.insert("inherited", inherited);
                        expr_entries.push(Bson::Document(entry));
                    }
                    dims.push(Dim {
                        attnum: -(i as i16) - 1,
                        wide: values.iter().map(too_wide).collect(),
                        values,
                    });
                    types.push(ty);
                }
                let totalrows = numrows as f64;
                let mut data = bson::doc! {
                    "_id": &key,
                    "object": &name,
                    "table": table,
                    "inherited": inherited,
                };
                let pairs = |items: Vec<(Vec<i16>, f64)>| -> Bson {
                    Bson::Array(
                        items
                            .into_iter()
                            .map(|(attrs, v)| {
                                Bson::Document(bson::doc! {
                                    "attrs": attrs.into_iter().map(i32::from).collect::<Vec<_>>(),
                                    "value": v,
                                })
                            })
                            .collect(),
                    )
                };
                if dims.len() >= 2 && kinds.iter().any(|k| k == "d") {
                    data.insert("ndistinct", pairs(ndistinct(&dims, numrows, totalrows)));
                }
                if dims.len() >= 2 && kinds.iter().any(|k| k == "f") {
                    let deps = dependencies(&dims, numrows);
                    if !deps.is_empty() {
                        data.insert("dependencies", pairs(deps));
                    }
                }
                if dims.len() >= 2 && kinds.iter().any(|k| k == "m") {
                    let items = mcv(&dims, numrows, totalrows, target);
                    if !items.is_empty() {
                        let typed: Vec<(i64, String)> = types
                            .iter()
                            .map(|t| {
                                (
                                    secantus_pgplan::pgtypes::oid_of_name(t).unwrap_or(0),
                                    t.clone(),
                                )
                            })
                            .collect();
                        let cmp = |a: &Bson, b: &Bson| {
                            secantus_pgplan::compare_values(a, b).unwrap_or(Ordering::Equal)
                        };
                        if let Some(bytes) = ext::mcv_bytes(&items, &typed, &cmp) {
                            data.insert(
                                "mcv_bytes",
                                Bson::Binary(bson::Binary {
                                    subtype: bson::spec::BinarySubtype::Generic,
                                    bytes,
                                }),
                            );
                        }
                        let shown: Vec<Bson> = items
                            .iter()
                            .map(|it| {
                                let values: Vec<Bson> = it
                                    .values
                                    .iter()
                                    .zip(&types)
                                    .map(|(v, t)| match v {
                                        Some(v) => Bson::String(
                                            secantus_pgplan::typeout::output_text(v, t),
                                        ),
                                        None => Bson::Null,
                                    })
                                    .collect();
                                let nulls: Vec<Bson> = it
                                    .values
                                    .iter()
                                    .map(|v| Bson::Boolean(v.is_none()))
                                    .collect();
                                Bson::Document(bson::doc! {
                                    "values": values,
                                    "nulls": nulls,
                                    "frequency": it.frequency,
                                    "base_frequency": it.base_frequency,
                                })
                            })
                            .collect();
                        data.insert("mcv", shown);
                    }
                }
                if !expr_entries.is_empty() {
                    data.insert("exprs", expr_entries);
                }
                self.put_comment_doc(STATEXT_DATA_COLLECTION, &key, Some(data))?;
            }
        }
        Ok(())
    }

    /// Each statistics object's expressions as `pg_get_statisticsobjdef`
    /// prints them.
    fn statistics_expr_texts(def: Option<&TableDef>, exprs: &[String]) -> Vec<String> {
        exprs
            .iter()
            .map(|e| {
                def.and_then(|d| secantus_pgplan::ruleutils::statistics_expr_def(e, d))
                    .unwrap_or_else(|| format!("({e})"))
            })
            .collect()
    }

    /// What `pg_get_statisticsobjdef` and its siblings answer:
    /// `(oid, CREATE STATISTICS, column and expression list, expressions)`.
    pub(crate) fn statistics_defs(&self) -> Vec<secantus_pgplan::StatisticsDef> {
        self.catalog_docs(catalog_objects::STATISTICS_COLLECTION)
            .iter()
            .map(|d| {
                let name = d.get_str("_id").unwrap_or_default();
                let table = d.get_str("table").unwrap_or_default();
                let tdef = self.lookup(table);
                let (cols, exprs) = match &tdef {
                    Some(t) => Self::statistics_dims(t, d),
                    None => (Vec::new(), strings(d, "exprs")),
                };
                let texts = Self::statistics_expr_texts(tdef.as_ref(), &exprs);
                let mut items: Vec<String> = cols.iter().map(|(_, c)| c.name.clone()).collect();
                items.extend(exprs.iter().map(|e| {
                    tdef.as_ref()
                        .and_then(|d| secantus_pgplan::ruleutils::statistics_expr_def_pretty(e, d))
                        .unwrap_or_else(|| format!("({e})"))
                }));
                let list = items.join(", ");
                let kinds = strings(d, "kinds");
                let has = |k: &str| kinds.iter().any(|x| x == k);
                // The kinds are printed unless every one is built, or the
                // object is a single expression's.
                let only_expression = cols.is_empty() && exprs.len() == 1;
                let shown: Vec<&str> = [("d", "ndistinct"), ("f", "dependencies"), ("m", "mcv")]
                    .iter()
                    .filter(|(k, _)| has(k))
                    .map(|(_, n)| *n)
                    .collect();
                let kinds_text = if only_expression || shown.len() == 3 {
                    String::new()
                } else {
                    format!(" ({})", shown.join(", "))
                };
                let text =
                    format!("CREATE STATISTICS public.{name}{kinds_text} ON {list} FROM {table}");
                (Self::statistics_oid(name), text, list, texts)
            })
            .collect()
    }

    /// `pg_statistic_ext_data`: a row per object ANALYZE built.
    pub(crate) fn statistic_ext_data_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |n: &str| def.field_of(n).expect("column");
        self.statext_rows()
            .into_iter()
            .map(|(_, data)| {
                let mut r = Document::new();
                let name = data.get_str("_id").unwrap_or_default();
                let name = data.get_str("object").unwrap_or(name);
                r.insert(f("stxoid"), Bson::Int64(Self::statistics_oid(name)));
                r.insert(
                    f("stxdinherit"),
                    data.get_bool("inherited").unwrap_or(false),
                );
                r.insert(f("stxdndistinct"), Self::ndistinct_value(&data));
                r.insert(f("stxddependencies"), Self::dependencies_value(&data));
                r.insert(
                    f("stxdmcv"),
                    data.get("mcv_bytes").cloned().unwrap_or(Bson::Null),
                );
                r.insert(
                    f("stxdexpr"),
                    Self::stxdexpr_text(&data).map_or(Bson::Null, Bson::String),
                );
                r
            })
            .collect()
    }

    /// Each object ANALYZE built, with its definition: `(object, data)`.
    fn statext_rows(&self) -> Vec<(Document, Document)> {
        let data: Vec<Document> = self
            .storage
            .find_matching(self.db(), STATEXT_DATA_COLLECTION, &Document::new())
            .unwrap_or_default()
            .iter()
            .filter_map(|b| decode_doc(b).ok())
            .collect();
        let mut out: Vec<(Document, Document)> = self
            .catalog_docs(catalog_objects::STATISTICS_COLLECTION)
            .into_iter()
            .flat_map(|o| {
                data.iter()
                    .filter(|d| d.get_str("object") == o.get_str("_id"))
                    .map(|d| (o.clone(), d.clone()))
                    .collect::<Vec<_>>()
            })
            .collect();
        out.sort_by_key(|(o, d)| {
            (
                Self::statistics_oid(o.get_str("_id").unwrap_or_default()),
                d.get_bool("inherited").unwrap_or(false),
            )
        });
        out
    }

    /// `stxdexpr`: each expression's `pg_statistic` row, as the text of a
    /// `pg_statistic[]` (`record_out` inside `array_out`).
    fn stxdexpr_text(data: &Document) -> Option<String> {
        let entries = data.get_array("exprs").ok()?;
        // record_out's quoting, then array_out's.
        let field = |v: Option<String>| -> String {
            let Some(v) = v else {
                return String::new();
            };
            let quote = v.is_empty()
                || v.chars()
                    .any(|c| matches!(c, '"' | '\\' | '(' | ')' | ',') || c.is_whitespace());
            if !quote {
                return v;
            }
            let mut out = String::from("\"");
            for c in v.chars() {
                if matches!(c, '"' | '\\') {
                    out.push(c);
                }
                out.push(c);
            }
            out.push('"');
            out
        };
        let element = |v: String| -> String {
            let quote = v.is_empty()
                || v.eq_ignore_ascii_case("null")
                || v.chars()
                    .any(|c| matches!(c, '{' | '}' | ',' | '"' | '\\') || c.is_whitespace());
            if !quote {
                return v;
            }
            let mut out = String::from("\"");
            for c in v.chars() {
                if matches!(c, '"' | '\\') {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push('"');
            out
        };
        let text = |v: &Bson, ty: &str| secantus_pgplan::typeout::output_text(v, ty);
        let mut rows = Vec::new();
        for e in entries {
            let entry = e.as_document()?;
            if entry.get_bool("unmodelled").unwrap_or(false) {
                return None;
            }
            let ty = entry.get_str("type").unwrap_or("text");
            let col = secantus_pgcatalog::Column::new("expr", ty, false);
            let (eq, lt, collatable) = statistic::operators(ty)?;
            let coll = if collatable { 100 } else { 0 };
            // (kind, operator, numbers, values), compute_scalar_stats' order.
            let mut slots: Vec<(i32, i64, Option<String>, Option<String>)> = Vec::new();
            if let Some(sl) = Self::stored_slots(entry) {
                if !sl.mcv.is_empty() {
                    let values: Vec<Bson> = sl
                        .mcv
                        .iter()
                        .map(|(k, _)| statistic::value_of(k, &col))
                        .collect::<Option<_>>()?;
                    let freqs: Vec<Bson> = sl
                        .mcv
                        .iter()
                        .map(|(_, f)| Bson::Double(f64::from(*f)))
                        .collect();
                    slots.push((
                        1,
                        eq,
                        Some(text(&Bson::Array(freqs), "float4[]")),
                        Some(text(&Bson::Array(values), &format!("{ty}[]"))),
                    ));
                }
                if sl.histogram.len() >= 2 {
                    let values: Vec<Bson> = sl
                        .histogram
                        .iter()
                        .map(|k| statistic::value_of(k, &col))
                        .collect::<Option<_>>()?;
                    slots.push((
                        2,
                        lt,
                        None,
                        Some(text(&Bson::Array(values), &format!("{ty}[]"))),
                    ));
                }
                if let Some(c) = sl.correlation {
                    slots.push((
                        3,
                        lt,
                        Some(text(
                            &Bson::Array(vec![Bson::Double(f64::from(c))]),
                            "float4[]",
                        )),
                        None,
                    ));
                }
            }
            let mut fields: Vec<Option<String>> = vec![
                Some("0".into()),
                Some("0".into()),
                Some("f".into()),
                Some(text(
                    &Bson::Double(entry.get_f64("null_frac").unwrap_or(0.0)),
                    "float4",
                )),
                Some(entry.get_i32("width").unwrap_or(0).to_string()),
                Some(text(
                    &Bson::Double(entry.get_f64("distinct").unwrap_or(0.0)),
                    "float4",
                )),
            ];
            for n in 0..5 {
                fields.push(Some(slots.get(n).map_or(0, |s| s.0).to_string()));
            }
            for n in 0..5 {
                fields.push(Some(slots.get(n).map_or(0, |s| s.1).to_string()));
            }
            for n in 0..5 {
                fields.push(Some(
                    if slots.get(n).is_some() { coll } else { 0 }.to_string(),
                ));
            }
            for n in 0..5 {
                fields.push(slots.get(n).and_then(|s| s.2.clone()));
            }
            for n in 0..5 {
                fields.push(slots.get(n).and_then(|s| s.3.clone()));
            }
            let record = format!(
                "({})",
                fields.into_iter().map(field).collect::<Vec<_>>().join(",")
            );
            rows.push(element(record));
        }
        Some(format!("{{{}}}", rows.join(",")))
    }

    fn pairs_of(data: &Document, key: &str) -> Option<Vec<(Vec<i16>, f64)>> {
        Some(
            data.get_array(key)
                .ok()?
                .iter()
                .filter_map(|p| {
                    let p = p.as_document()?;
                    let attrs = p
                        .get_array("attrs")
                        .ok()?
                        .iter()
                        .filter_map(|a| a.as_i32().map(|a| a as i16))
                        .collect();
                    Some((attrs, p.get_f64("value").ok()?))
                })
                .collect(),
        )
    }

    fn ndistinct_value(data: &Document) -> Bson {
        Self::pairs_of(data, "ndistinct")
            .map_or(Bson::Null, |p| Bson::String(ext::ndistinct_text(&p)))
    }

    fn dependencies_value(data: &Document) -> Bson {
        Self::pairs_of(data, "dependencies")
            .map_or(Bson::Null, |p| Bson::String(ext::dependencies_text(&p)))
    }

    /// `pg_stats_ext`: a row per object ANALYZE built.
    pub(crate) fn stats_ext_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |n: &str| def.field_of(n).expect("column");
        self.statext_rows()
            .into_iter()
            .map(|(object, data)| {
                let table = object.get_str("table").unwrap_or_default();
                let tdef = self.lookup(table);
                let (cols, exprs) = match &tdef {
                    Some(t) => Self::statistics_dims(t, &object),
                    None => (Vec::new(), strings(&object, "exprs")),
                };
                let mut r = Document::new();
                r.insert(f("schemaname"), "public");
                r.insert(f("tablename"), table);
                r.insert(f("statistics_schemaname"), "public");
                r.insert(
                    f("statistics_name"),
                    object.get_str("_id").unwrap_or_default(),
                );
                r.insert(
                    f("statistics_owner"),
                    object.get_str("owner").unwrap_or_default(),
                );
                r.insert(
                    f("attnames"),
                    // `array_agg` over no columns is NULL.
                    if cols.is_empty() {
                        Bson::Null
                    } else {
                        Bson::Array(
                            cols.iter()
                                .map(|(_, c)| Bson::String(c.name.clone()))
                                .collect(),
                        )
                    },
                );
                r.insert(
                    f("exprs"),
                    if exprs.is_empty() {
                        Bson::Null
                    } else {
                        Bson::Array(
                            Self::statistics_expr_texts(tdef.as_ref(), &exprs)
                                .into_iter()
                                .map(Bson::String)
                                .collect(),
                        )
                    },
                );
                r.insert(
                    f("kinds"),
                    Bson::Array(
                        strings(&object, "kinds")
                            .into_iter()
                            .map(Bson::String)
                            .collect(),
                    ),
                );
                r.insert(f("inherited"), data.get_bool("inherited").unwrap_or(false));
                r.insert(f("n_distinct"), Self::ndistinct_value(&data));
                r.insert(f("dependencies"), Self::dependencies_value(&data));
                let items: Vec<Document> = data
                    .get_array("mcv")
                    .map(|a| a.iter().filter_map(|i| i.as_document().cloned()).collect())
                    .unwrap_or_default();
                let column = |key: &str| -> Bson {
                    if items.is_empty() {
                        return Bson::Null;
                    }
                    Bson::Array(
                        items
                            .iter()
                            .map(|i| i.get(key).cloned().unwrap_or(Bson::Null))
                            .collect(),
                    )
                };
                r.insert(f("most_common_vals"), column("values"));
                r.insert(f("most_common_val_nulls"), column("nulls"));
                r.insert(f("most_common_freqs"), column("frequency"));
                r.insert(f("most_common_base_freqs"), column("base_frequency"));
                r
            })
            .collect()
    }

    /// `pg_stats_ext_exprs`: a row per expression of every object, its
    /// statistics NULL until ANALYZE built them.
    pub(crate) fn stats_ext_exprs_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |n: &str| def.field_of(n).expect("column");
        let mut objects = self.catalog_docs(catalog_objects::STATISTICS_COLLECTION);
        objects.sort_by_key(|o| Self::statistics_oid(o.get_str("_id").unwrap_or_default()));
        let mut out = Vec::new();
        for object in objects {
            let name = object.get_str("_id").unwrap_or_default();
            let table = object.get_str("table").unwrap_or_default();
            let exprs = strings(&object, "exprs");
            if exprs.is_empty() {
                continue;
            }
            let tdef = self.lookup(table);
            let texts = Self::statistics_expr_texts(tdef.as_ref(), &exprs);
            // A row set per statistics ANALYZE built (its own, then its
            // inherited), or one of NULLs before it built any.
            let mut built: Vec<Vec<Document>> = self
                .statext_data(name)
                .iter()
                .map(|d| {
                    d.get_array("exprs")
                        .map(|a| a.iter().filter_map(|e| e.as_document().cloned()).collect())
                        .unwrap_or_default()
                })
                .collect();
            if built.is_empty() {
                built.push(Vec::new());
            }
            for entries in built {
                for (i, text) in texts.iter().cloned().enumerate() {
                    let mut r = Document::new();
                    r.insert(f("schemaname"), "public");
                    r.insert(f("tablename"), table);
                    r.insert(f("statistics_schemaname"), "public");
                    r.insert(f("statistics_name"), name);
                    r.insert(
                        f("statistics_owner"),
                        object.get_str("owner").unwrap_or_default(),
                    );
                    r.insert(f("expr"), text);
                    r.insert(f("inherited"), Bson::Null);
                    for c in [
                        "null_frac",
                        "avg_width",
                        "n_distinct",
                        "most_common_vals",
                        "most_common_freqs",
                        "histogram_bounds",
                        "correlation",
                        "most_common_elems",
                        "most_common_elem_freqs",
                        "elem_count_histogram",
                    ] {
                        r.insert(f(c), Bson::Null);
                    }
                    if let Some(entry) = entries
                        .get(i)
                        .filter(|e| !e.get_bool("unmodelled").unwrap_or(false))
                    {
                        let ty = entry.get_str("type").unwrap_or("text");
                        let col = secantus_pgcatalog::Column::new("expr", ty, false);
                        r.insert(f("inherited"), entry.get_bool("inherited").unwrap_or(false));
                        r.insert(
                            f("null_frac"),
                            Bson::Double(entry.get_f64("null_frac").unwrap_or(0.0)),
                        );
                        r.insert(
                            f("avg_width"),
                            Bson::Int32(entry.get_i32("width").unwrap_or(0)),
                        );
                        r.insert(
                            f("n_distinct"),
                            Bson::Double(entry.get_f64("distinct").unwrap_or(0.0)),
                        );
                        if let Some(s) = Self::stored_slots(entry) {
                            if !s.mcv.is_empty() {
                                let values: Option<Vec<Bson>> = s
                                    .mcv
                                    .iter()
                                    .map(|(k, _)| statistic::value_of(k, &col))
                                    .collect();
                                if let Some(values) = values {
                                    r.insert(f("most_common_vals"), Bson::Array(values));
                                    r.insert(
                                        f("most_common_freqs"),
                                        Bson::Array(
                                            s.mcv
                                                .iter()
                                                .map(|(_, fr)| Bson::Double(f64::from(*fr)))
                                                .collect(),
                                        ),
                                    );
                                }
                            }
                            if s.histogram.len() >= 2 {
                                let values: Option<Vec<Bson>> = s
                                    .histogram
                                    .iter()
                                    .map(|k| statistic::value_of(k, &col))
                                    .collect();
                                if let Some(values) = values {
                                    r.insert(f("histogram_bounds"), Bson::Array(values));
                                }
                            }
                            if let Some(c) = s.correlation {
                                r.insert(f("correlation"), Bson::Double(f64::from(c)));
                            }
                        }
                    }
                    out.push(r);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_order_is_postgresqls() {
        // `{"1 => 2", "1 => 3", "2 => 1", "2 => 3", "3 => 1", "3 => 2"}`,
        // then `1, 2 => 3`, `1, 3 => 2`, `2, 3 => 1`.
        assert_eq!(
            dependency_orders(3, 2),
            vec![
                vec![0, 1],
                vec![0, 2],
                vec![1, 0],
                vec![1, 2],
                vec![2, 0],
                vec![2, 1]
            ]
        );
        assert_eq!(
            dependency_orders(3, 3),
            vec![vec![0, 1, 2], vec![0, 2, 1], vec![1, 2, 0]]
        );
    }

    #[test]
    fn duj1() {
        assert_eq!(estimate_ndistinct(42.0, 42, 6, 0), 6.0);
    }
}
