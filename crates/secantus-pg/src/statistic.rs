//! `pg_statistic`: the rows ANALYZE recorded (`record_analyze`), laid out as
//! PostgreSQL's `compute_scalar_stats` fills its slots -- the most common
//! values first (`STATISTIC_KIND_MCV`, under the type's `=`), then the
//! histogram's bounds (`STATISTIC_KIND_HISTOGRAM`, under `<`) when at least
//! two remain, then the correlation (`STATISTIC_KIND_CORRELATION`, `<`).
//! `pg_stats` is PostgreSQL's own view over it.
//!
//! Only the columns whose type's equality ANALYZE models here have a row
//! (`analyze_sample`): integers, booleans, floats, `text` / `varchar` and
//! `char(n)`; a table larger than ANALYZE's sample has relation statistics
//! only.
//!
//! The built-in catalogs' rows are the ones PostgreSQL 15.19's `initdb`
//! leaves (RP-311): its ANALYZE of the catalogs, copied into every database
//! from `template1`, dumped into `pg15_catalog_statistics.txt` by
//! `tools/probes/pg15_system_catalog_dump.py` in heap order, and listed
//! before any table's.

use super::*;

const MCV: i32 = 1;
const HISTOGRAM: i32 = 2;
const CORRELATION: i32 = 3;
const DEFAULT_COLLATION: i64 = 100;

/// The `RELSTATS_COLLECTION` row holding a database-wide ANALYZE's measure
/// of the built-in catalogs, by `"<relid>:<attnum>"` (RP-313).
pub(crate) const CATALOG_STATISTICS: &str = "\u{0}catalogs";

/// A type's `=` and `<` operators (its default B-tree opclass), and
/// whether it is collatable.
pub(crate) fn operators(pg_type: &str) -> Option<(i64, i64, bool)> {
    Some(match pg_type.trim_start_matches("pg_catalog.") {
        "int2" => (94, 95, false),
        "int4" => (96, 97, false),
        "int8" => (410, 412, false),
        "float4" => (620, 622, false),
        "float8" => (670, 672, false),
        "bool" => (91, 58, false),
        "oid" => (607, 609, false),
        "text" | "varchar" | "character varying" => (98, 664, true),
        "bpchar" | "character" => (1054, 1058, true),
        _ => return None,
    })
}

/// A key `analyze_sample` made, as the column's value again.
pub(crate) fn value_of(key: &[u8], c: &secantus_pgcatalog::Column) -> Option<Bson> {
    Some(match c.pg_type.trim_start_matches("pg_catalog.") {
        "int2" | "int4" => Bson::Int32(secantus_pgplan::colstats::int_of_key(key)? as i32),
        "int8" | "oid" => Bson::Int64(secantus_pgplan::colstats::int_of_key(key)?),
        "float4" | "float8" => {
            Bson::Double(f64::from_bits(u64::from_be_bytes(key.try_into().ok()?)))
        }
        "bool" => Bson::Boolean(*key.first()? != 0),
        "text" | "varchar" | "character varying" => {
            Bson::String(String::from_utf8(key.to_vec()).ok()?)
        }
        // Compared trimmed, shown padded to the declared length.
        "bpchar" | "character" => {
            let text = String::from_utf8(key.to_vec()).ok()?;
            let width = if c.typmod >= 4 {
                c.typmod as usize - 4
            } else {
                0
            };
            let pad = width.saturating_sub(text.chars().count());
            Bson::String(format!("{text}{}", " ".repeat(pad)))
        }
        _ => return None,
    })
}

/// A dumped `real`: the double the `float4` is carried as.
fn float4(text: &str) -> Option<Bson> {
    Some(Bson::Double(f64::from(text.parse::<f32>().ok()?)))
}

/// One dumped slot's values (`array_to_json` of the `anyarray`), each as its
/// element type is carried here; a type with no wider form keeps
/// PostgreSQL's text for it, which is what the array prints. An array
/// column's values are arrays themselves, each kept as its text.
fn builtin_values(elem: i64, json: &str) -> Option<Bson> {
    fn value(elem: i64, v: &serde_json::Value) -> Option<Bson> {
        let text = match v {
            serde_json::Value::Null => return Some(Bson::Null),
            serde_json::Value::Array(items) => {
                // The element type of the array (and vector) types an
                // ANALYZE of the catalogs reaches.
                let inner = match elem {
                    1005 | 22 => 21,
                    1028 | 30 => 26,
                    _ => 0,
                };
                let items = items
                    .iter()
                    .map(|v| value(inner, v))
                    .collect::<Option<Vec<_>>>()?;
                // Its own text output, which the slot's array quotes:
                // `{"{1,2}"}`, and a vector's `{"1981 1978"}`.
                return Some(Bson::String(if matches!(elem, 22 | 30) {
                    items
                        .iter()
                        .map(secantus_pgplan::value_text)
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    secantus_pgplan::render_array(&items)
                }));
            }
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        Some(match elem {
            16 => Bson::Boolean(text == "true" || text == "t"),
            21 | 23 => Bson::Int32(text.parse().ok()?),
            20 | 26 => Bson::Int64(text.parse().ok()?),
            _ => Bson::String(text),
        })
    }
    let serde_json::Value::Array(items) = serde_json::from_str(json).ok()? else {
        return None;
    };
    items
        .iter()
        .map(|v| value(elem, v))
        .collect::<Option<Vec<_>>>()
        .map(Bson::Array)
}

/// `initdb`'s `pg_statistic` rows, parsed once: each the column names'
/// values, in the order the dump wrote them.
fn builtin_rows() -> &'static [Vec<(String, Bson)>] {
    static ROWS: OnceLock<Vec<Vec<(String, Bson)>>> = OnceLock::new();
    ROWS.get_or_init(|| {
        include_str!("pg15_catalog_statistics.txt")
            .lines()
            .map(|line| {
                let cells: Vec<Option<String>> = line
                    .split('\t')
                    .map(|c| (c != "\\N").then(|| unescape_copy_text(c)))
                    .collect();
                assert_eq!(cells.len(), 36, "pg15_catalog_statistics.txt: {line}");
                let cell = |i: usize| cells[i].as_deref().expect("dumped NOT NULL column");
                let int = |i: usize| cell(i).parse::<i64>().expect("dumped integer");
                let mut row = vec![
                    ("starelid".to_string(), Bson::Int64(int(0))),
                    ("staattnum".to_string(), Bson::Int32(int(1) as i32)),
                    ("stainherit".to_string(), Bson::Boolean(cell(2) == "t")),
                    (
                        "stanullfrac".to_string(),
                        float4(cell(3)).expect("dumped real"),
                    ),
                    ("stawidth".to_string(), Bson::Int32(int(4) as i32)),
                    (
                        "stadistinct".to_string(),
                        float4(cell(5)).expect("dumped real"),
                    ),
                ];
                for n in 0..5 {
                    let slot = n + 1;
                    let numbers = cells[21 + n].as_deref().map(|text| {
                        Bson::Array(
                            text.trim_start_matches('{')
                                .trim_end_matches('}')
                                .split(',')
                                .map(|x| float4(x).expect("dumped real"))
                                .collect(),
                        )
                    });
                    let values = match (&cells[26 + 2 * n], &cells[27 + 2 * n]) {
                        (Some(elem), Some(json)) => {
                            builtin_values(elem.parse().expect("dumped type oid"), json)
                                .expect("dumped values")
                        }
                        _ => Bson::Null,
                    };
                    row.extend([
                        (format!("stakind{slot}"), Bson::Int32(int(6 + n) as i32)),
                        (format!("staop{slot}"), Bson::Int64(int(11 + n))),
                        (format!("stacoll{slot}"), Bson::Int64(int(16 + n))),
                        (format!("stanumbers{slot}"), numbers.unwrap_or(Bson::Null)),
                        (format!("stavalues{slot}"), values),
                    ]);
                }
                row
            })
            .collect()
    })
}

/// `initdb`'s `pg_statistic` rows of the built-in catalog `starelid`, as
/// the planner reads them (RP-314).
pub(crate) fn builtin_stats(starelid: i64) -> impl Iterator<Item = &'static [(String, Bson)]> {
    builtin_rows()
        .iter()
        .filter(move |row| row.first().map(|(_, v)| v) == Some(&Bson::Int64(starelid)))
        .map(Vec::as_slice)
}

impl PgHandler {
    /// ANALYZE of the built-in catalogs, as a database-wide ANALYZE takes
    /// them (`get_all_vacuum_rels`; RP-313): each catalog column whose type's
    /// statistics are modelled here, over the catalog's rows as they stand
    /// (all of them -- no catalog outgrows ANALYZE's sample), replaces
    /// `initdb`'s row for it. A column of another type keeps `initdb`'s.
    pub(crate) fn analyze_catalogs(&self) -> PgWireResult<()> {
        let target = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get("default_statistics_target")
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(100);
        let int = |b: Option<&Bson>| match b {
            Some(Bson::Int32(n)) => Some(i64::from(*n)),
            Some(Bson::Int64(n)) => Some(*n),
            _ => None,
        };
        let (Some(class), Some(attribute)) = (
            Self::virtual_table("pg_class"),
            Self::virtual_table("pg_attribute"),
        ) else {
            return Ok(());
        };
        let field = |d: &TableDef, n: &str| d.field_of(n).unwrap_or_default();
        let (oid_f, relname_f) = (field(&class, "oid"), field(&class, "relname"));
        let names: HashMap<i64, String> = self
            .table_docs("pg_class")?
            .iter()
            .filter_map(|d| Some((int(d.get(&oid_f))?, d.get_str(&relname_f).ok()?.to_string())))
            .collect();
        let (relid_f, attnum_f, attname_f) = (
            field(&attribute, "attrelid"),
            field(&attribute, "attnum"),
            field(&attribute, "attname"),
        );
        let attnames: HashMap<(i64, i64), String> = self
            .table_docs("pg_attribute")?
            .iter()
            .filter_map(|d| {
                Some((
                    (int(d.get(&relid_f))?, int(d.get(&attnum_f))?),
                    d.get_str(&attname_f).ok()?.to_string(),
                ))
            })
            .collect();
        let mut columns = Document::new();
        let mut relids: Vec<i64> = builtin_rows()
            .iter()
            .filter_map(|r| int(Some(&r[0].1)))
            .collect();
        relids.sort_unstable();
        relids.dedup();
        for rel in relids {
            let Some(name) = names.get(&rel) else {
                continue;
            };
            let Some(def) = Self::virtual_table(name) else {
                continue;
            };
            let docs = self.table_docs(name)?;
            // ANALYZE computes nothing from no rows.
            if docs.is_empty() {
                continue;
            }
            let totalrows = docs.len() as f64;
            for row in builtin_rows()
                .iter()
                .filter(|r| int(Some(&r[0].1)) == Some(rel))
            {
                let Some(attnum) = int(Some(&row[1].1)) else {
                    continue;
                };
                let Some(c) = attnames.get(&(rel, attnum)).and_then(|n| def.column(n)) else {
                    continue;
                };
                if operators(&c.pg_type).is_none() {
                    continue;
                }
                let f = c.field();
                let values: Vec<Option<&Bson>> = docs.iter().map(|d| d.get(&f)).collect();
                let entry = Self::stats_entry(c, &values, target, false, totalrows);
                if !entry.get_bool("unmodelled").unwrap_or(false) {
                    columns.insert(format!("{rel}:{attnum}"), entry);
                }
            }
        }
        self.put_comment_doc(
            Self::RELSTATS_COLLECTION,
            CATALOG_STATISTICS,
            Some(bson::doc! {"_id": CATALOG_STATISTICS, "catalogs": columns}),
        )
    }

    /// One `pg_statistic` row of column `c` (attnum `attnum`) of relation
    /// `rel`, from what ANALYZE recorded of it; `None` when its values are
    /// not shown here.
    fn statistic_row(
        def: &TableDef,
        rel: i64,
        attnum: i16,
        c: &secantus_pgcatalog::Column,
        entry: &Document,
    ) -> Option<Document> {
        let f = |n: &str| def.field_of(n).expect("column");
        if entry.get_bool("unmodelled").unwrap_or(false) {
            return None;
        }
        let (eq, lt, collatable) = operators(&c.pg_type)?;
        let coll = if collatable { DEFAULT_COLLATION } else { 0 };
        let mut d = Document::new();
        d.insert(f("starelid"), Bson::Int64(rel));
        d.insert(f("staattnum"), Bson::Int32(i32::from(attnum)));
        d.insert(f("stainherit"), false);
        d.insert(
            f("stanullfrac"),
            Bson::Double(entry.get_f64("null_frac").unwrap_or(0.0)),
        );
        d.insert(
            f("stawidth"),
            Bson::Int32(entry.get_i32("width").unwrap_or(0)),
        );
        d.insert(
            f("stadistinct"),
            Bson::Double(entry.get_f64("distinct").unwrap_or(0.0)),
        );
        // (kind, operator, numbers, values), in compute_scalar_stats'
        // order.
        let mut slots: Vec<(i32, i64, Bson, Bson)> = Vec::new();
        if let Some(s) = Self::stored_slots(entry) {
            if !s.mcv.is_empty() {
                let values: Vec<Bson> = s
                    .mcv
                    .iter()
                    .map(|(k, _)| value_of(k, c))
                    .collect::<Option<_>>()?;
                slots.push((
                    MCV,
                    eq,
                    Bson::Array(
                        s.mcv
                            .iter()
                            .map(|(_, freq)| Bson::Double(f64::from(*freq)))
                            .collect(),
                    ),
                    Bson::Array(values),
                ));
            }
            if s.histogram.len() >= 2 {
                let values: Vec<Bson> = s
                    .histogram
                    .iter()
                    .map(|k| value_of(k, c))
                    .collect::<Option<_>>()?;
                slots.push((HISTOGRAM, lt, Bson::Null, Bson::Array(values)));
            }
            if let Some(corr) = s.correlation {
                slots.push((
                    CORRELATION,
                    lt,
                    Bson::Array(vec![Bson::Double(f64::from(corr))]),
                    Bson::Null,
                ));
            }
        }
        for n in 1..=5usize {
            let slot = slots.get(n - 1);
            d.insert(
                f(&format!("stakind{n}")),
                Bson::Int32(slot.map_or(0, |s| s.0)),
            );
            d.insert(
                f(&format!("staop{n}")),
                Bson::Int64(slot.map_or(0, |s| s.1)),
            );
            d.insert(
                f(&format!("stacoll{n}")),
                Bson::Int64(if slot.is_some() { coll } else { 0 }),
            );
            d.insert(
                f(&format!("stanumbers{n}")),
                slot.map_or(Bson::Null, |s| s.2.clone()),
            );
            d.insert(
                f(&format!("stavalues{n}")),
                slot.map_or(Bson::Null, |s| s.3.clone()),
            );
        }
        Some(d)
    }

    pub(crate) fn pg_statistic_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |n: &str| def.field_of(n).expect("column");
        // A database-wide ANALYZE's measure of the catalogs replaces
        // `initdb`'s rows it re-measured (RP-313).
        let measured = self
            .relstats(CATALOG_STATISTICS)
            .and_then(|d| d.get_document("catalogs").ok().cloned())
            .unwrap_or_default();
        let catalog = |rel: i64, attnum: i32| -> Option<Document> {
            let entry = measured.get_document(format!("{rel}:{attnum}")).ok()?;
            let ty = entry.get_str("type").ok()?;
            let c = secantus_pgcatalog::Column::new(entry.get_str("name").ok()?, ty, false);
            Self::statistic_row(def, rel, attnum as i16, &c, entry)
        };
        let mut out: Vec<Document> = builtin_rows()
            .iter()
            .map(|row| {
                let rel = match &row[0].1 {
                    Bson::Int64(n) => *n,
                    _ => 0,
                };
                let attnum = match &row[1].1 {
                    Bson::Int32(n) => *n,
                    _ => 0,
                };
                catalog(rel, attnum)
                    .unwrap_or_else(|| row.iter().map(|(k, v)| (f(k), v.clone())).collect())
            })
            .collect();
        for t in self.all_table_defs().unwrap_or_default() {
            let Some(rel) = self.relation_oid(&t.name) else {
                continue;
            };
            let Some(doc) = self.relstats(&t.name) else {
                continue;
            };
            let Ok(columns) = doc.get_document("columns") else {
                continue;
            };
            for (i, c) in t.columns.iter().enumerate() {
                let Ok(entry) = columns.get_document(c.field()) else {
                    continue;
                };
                if let Some(d) = Self::statistic_row(def, rel, t.attnum(i), c, entry) {
                    out.push(d);
                }
            }
        }
        out
    }
}
