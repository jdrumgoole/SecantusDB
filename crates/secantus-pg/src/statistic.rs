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

use super::*;

const MCV: i32 = 1;
const HISTOGRAM: i32 = 2;
const CORRELATION: i32 = 3;
const DEFAULT_COLLATION: i64 = 100;

/// A type's `=` and `<` operators (its default B-tree opclass), and
/// whether it is collatable.
fn operators(pg_type: &str) -> Option<(i64, i64, bool)> {
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
fn value_of(key: &[u8], c: &secantus_pgcatalog::Column) -> Option<Bson> {
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

impl PgHandler {
    pub(crate) fn pg_statistic_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |n: &str| def.field_of(n).expect("column");
        let mut out = Vec::new();
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
