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
        "text" | "varchar" | "character varying" => (98, 664, true),
        "bpchar" | "character" => (1054, 1058, true),
        _ => return None,
    })
}

/// A key `analyze_sample` made, as the column's value again.
pub(crate) fn value_of(key: &[u8], c: &secantus_pgcatalog::Column) -> Option<Bson> {
    Some(match c.pg_type.trim_start_matches("pg_catalog.") {
        "int2" | "int4" => Bson::Int32(secantus_pgplan::colstats::int_of_key(key)? as i32),
        "int8" => Bson::Int64(secantus_pgplan::colstats::int_of_key(key)?),
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
fn builtin_rows() -> &'static [Vec<(&'static str, Bson)>] {
    static ROWS: OnceLock<Vec<Vec<(&'static str, Bson)>>> = OnceLock::new();
    ROWS.get_or_init(|| {
        const KINDS: [&str; 5] = ["stakind1", "stakind2", "stakind3", "stakind4", "stakind5"];
        const OPS: [&str; 5] = ["staop1", "staop2", "staop3", "staop4", "staop5"];
        const COLLS: [&str; 5] = ["stacoll1", "stacoll2", "stacoll3", "stacoll4", "stacoll5"];
        const NUMBERS: [&str; 5] = [
            "stanumbers1",
            "stanumbers2",
            "stanumbers3",
            "stanumbers4",
            "stanumbers5",
        ];
        const VALUES: [&str; 5] = [
            "stavalues1",
            "stavalues2",
            "stavalues3",
            "stavalues4",
            "stavalues5",
        ];
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
                    ("starelid", Bson::Int64(int(0))),
                    ("staattnum", Bson::Int32(int(1) as i32)),
                    ("stainherit", Bson::Boolean(cell(2) == "t")),
                    ("stanullfrac", float4(cell(3)).expect("dumped real")),
                    ("stawidth", Bson::Int32(int(4) as i32)),
                    ("stadistinct", float4(cell(5)).expect("dumped real")),
                ];
                for n in 0..5 {
                    row.push((KINDS[n], Bson::Int32(int(6 + n) as i32)));
                }
                for n in 0..5 {
                    row.push((OPS[n], Bson::Int64(int(11 + n))));
                }
                for n in 0..5 {
                    row.push((COLLS[n], Bson::Int64(int(16 + n))));
                }
                for n in 0..5 {
                    let numbers = cells[21 + n].as_deref().map(|text| {
                        Bson::Array(
                            text.trim_start_matches('{')
                                .trim_end_matches('}')
                                .split(',')
                                .map(|x| float4(x).expect("dumped real"))
                                .collect(),
                        )
                    });
                    row.push((NUMBERS[n], numbers.unwrap_or(Bson::Null)));
                }
                for n in 0..5 {
                    let values = match (&cells[26 + 2 * n], &cells[27 + 2 * n]) {
                        (Some(elem), Some(json)) => {
                            builtin_values(elem.parse().expect("dumped type oid"), json)
                                .expect("dumped values")
                        }
                        _ => Bson::Null,
                    };
                    row.push((VALUES[n], values));
                }
                row
            })
            .collect()
    })
}

impl PgHandler {
    pub(crate) fn pg_statistic_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |n: &str| def.field_of(n).expect("column");
        let mut out: Vec<Document> = builtin_rows()
            .iter()
            .map(|row| row.iter().map(|(k, v)| (f(k), v.clone())).collect())
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
                if entry.get_bool("unmodelled").unwrap_or(false) {
                    continue;
                }
                let Some((eq, lt, collatable)) = operators(&c.pg_type) else {
                    continue;
                };
                let coll = if collatable { DEFAULT_COLLATION } else { 0 };
                let mut d = Document::new();
                d.insert(f("starelid"), Bson::Int64(rel));
                d.insert(f("staattnum"), Bson::Int32(i32::from(t.attnum(i))));
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
                let slots_of = Self::stored_slots(entry);
                if let Some(s) = &slots_of {
                    if !s.mcv.is_empty() {
                        let values: Option<Vec<Bson>> =
                            s.mcv.iter().map(|(k, _)| value_of(k, c)).collect();
                        let Some(values) = values else {
                            continue;
                        };
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
                        let values: Option<Vec<Bson>> =
                            s.histogram.iter().map(|k| value_of(k, c)).collect();
                        let Some(values) = values else {
                            continue;
                        };
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
                out.push(d);
            }
        }
        out
    }
}
