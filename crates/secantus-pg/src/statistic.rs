//! `pg_statistic`: the rows ANALYZE recorded (`record_analyze`), laid out as
//! PostgreSQL's `compute_scalar_stats` fills its slots -- the most common
//! values first (`STATISTIC_KIND_MCV`, under the type's `=`), then the
//! histogram's bounds (`STATISTIC_KIND_HISTOGRAM`, under `<`) when at least
//! two remain, then the correlation (`STATISTIC_KIND_CORRELATION`, `<`).
//! `pg_stats` is PostgreSQL's own view over it.
//!
//! Only the columns whose type's equality ANALYZE models here have a row
//! (`analyze_sample`): integers, booleans, floats, `text` / `varchar`,
//! `char(n)`, `name`, `"char"` and the `reg*` types; a table larger than ANALYZE's sample has relation statistics
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
const MCELEM: i32 = 4;
const DECHIST: i32 = 5;
const DEFAULT_COLLATION: i64 = 100;
const C_COLLATION: i64 = 950;

/// The `RELSTATS_COLLECTION` row holding a database-wide ANALYZE's measure
/// of the built-in catalogs, by `"<relid>:<attnum>"` (RP-313).
pub(crate) const CATALOG_STATISTICS: &str = "\u{0}catalogs";

/// A type's `=` and `<` operators (its default B-tree opclass), and the
/// collation its slots are recorded under (`stacollN`; 0 for a type that
/// is not collatable): the database's default for `text` and `bpchar`, and
/// `C` (950) for `name`, whose `typcollation` it is.
pub(crate) fn operators(pg_type: &str) -> Option<(i64, i64, i64)> {
    // An array of a type whose `=` and `<` are modelled: `array_eq` and
    // `array_lt` (`array_typanalyze`'s scalar stats), under the element's
    // collation.
    if let Some(elem) = array_element(pg_type) {
        let (_, _, coll) = operators(elem)?;
        return Some((1070, 1072, coll));
    }
    Some(match pg_type.trim_start_matches("pg_catalog.") {
        "int2" => (94, 95, 0),
        "int4" => (96, 97, 0),
        "int8" => (410, 412, 0),
        "float4" => (620, 622, 0),
        "float8" => (670, 672, 0),
        "bool" => (91, 58, 0),
        "oid" => (607, 609, 0),
        // A `reg*` type has no opclass of its own: `GetDefaultOpClass` takes
        // `oid_ops`, as the type is binary-coercible to `oid` (so `initdb`'s
        // rows for `pg_type.typinput` carry `oideq` and `oidlt`).
        t if secantus_pgplan::regobj::is_kind(t) => (607, 609, 0),
        "text" | "varchar" | "character varying" => (98, 664, DEFAULT_COLLATION),
        "bpchar" | "character" => (1054, 1058, DEFAULT_COLLATION),
        "name" => (93, 660, C_COLLATION),
        secantus_pgplan::QUOTED_CHAR => (92, 631, 0),
        _ => return None,
    })
}

/// The `=` of a type with no `<` whose statistics are modelled
/// (`compute_distinct_stats`): `aclitem[]`'s `array_eq`, as `aclitem` has
/// `aclitemeq` and a hash opclass but no B-tree one.
pub(crate) fn equality_only(pg_type: &str) -> Option<i64> {
    match pg_type.trim_start_matches("pg_catalog.") {
        "aclitem[]" | "_aclitem" | "pg_catalog.aclitem[]" => Some(1070),
        _ => None,
    }
}

/// The element type of a one-dimensional array type (`text[]`, `_text`);
/// `None` for any other type.
pub(crate) fn array_element(pg_type: &str) -> Option<&str> {
    let t = pg_type.trim_start_matches("pg_catalog.");
    let elem = t
        .strip_suffix("[]")
        .or_else(|| t.strip_prefix('_'))?
        .trim_start_matches("pg_catalog.");
    (!elem.is_empty() && !elem.ends_with("[]")).then_some(elem)
}

/// An array's element as a column of its own, which ANALYZE samples it as.
pub(crate) fn element_column(c: &secantus_pgcatalog::Column) -> Option<secantus_pgcatalog::Column> {
    Some(secantus_pgcatalog::Column::new(
        &c.name,
        array_element(&c.pg_type)?,
        false,
    ))
}

/// An array's key, ordered as `array_cmp` orders arrays: element by
/// element (each its own type's key, NUL-escaped), a NULL element after
/// every value, and an array that is a prefix of another before it.
pub(crate) fn array_key(elements: &[Option<Vec<u8>>]) -> Vec<u8> {
    let mut key = Vec::new();
    for e in elements {
        match e {
            Some(k) => {
                key.push(1);
                for &b in k {
                    key.push(b);
                    if b == 0 {
                        key.push(0xFF);
                    }
                }
                key.extend([0, 0]);
            }
            None => key.push(2),
        }
    }
    key.push(0);
    key
}

/// The element keys `array_key` joined.
pub(crate) fn array_key_elements(key: &[u8]) -> Option<Vec<Option<Vec<u8>>>> {
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        match *key.get(i)? {
            0 => return (i + 1 == key.len()).then_some(out),
            2 => {
                out.push(None);
                i += 1;
            }
            1 => {
                i += 1;
                let mut k = Vec::new();
                loop {
                    let b = *key.get(i)?;
                    if b == 0 {
                        let next = *key.get(i + 1)?;
                        i += 2;
                        if next == 0 {
                            break;
                        }
                        k.push(0);
                    } else {
                        k.push(b);
                        i += 1;
                    }
                }
                out.push(Some(k));
            }
            _ => return None,
        }
    }
}

/// An array's datum size, its 4-byte header included (`VARSIZE` of a
/// one-dimensional array as `construct_md_array` builds it, whose
/// elements keep 4-byte headers): the header and dimensions, a null
/// bitmap when an element is NULL, `MAXALIGN`ed, then each element padded
/// to its `typalign`.
/// `sizes` holds each non-null element's size, `None` for a NULL.
pub(crate) fn array_datum_size(elem_type: &str, sizes: &[Option<usize>]) -> usize {
    if sizes.is_empty() {
        return 16;
    }
    let maxalign = |n: usize| n.div_ceil(8) * 8;
    let nulls = sizes.iter().any(Option::is_none);
    let overhead = if nulls {
        maxalign(16 + 8 + sizes.len().div_ceil(8))
    } else {
        maxalign(16 + 8)
    };
    let align = match elem_type {
        "int2" => 2,
        "int8" | "float8" => 8,
        "bool" | "name" | secantus_pgplan::QUOTED_CHAR => 1,
        _ => 4,
    };
    let mut off = 0usize;
    // `construct_md_array` aligns after each element, the last too.
    for s in sizes.iter().flatten() {
        off = (off + s).div_ceil(align) * align;
    }
    overhead + off
}

/// The most common elements and their frequencies, MCELEM's three extra
/// numbers (least and greatest frequency, NULL-element frequency; empty
/// with no common element), and DECHIST's numbers.
pub(crate) type ArrayElementStats = (Vec<(Vec<u8>, f32)>, Vec<f32>, Vec<f32>);

/// `compute_array_stats`' element slots of an array column: the most
/// common elements (`STATISTIC_KIND_MCELEM`, each element counted once
/// per array, by Lossy Counting) with their frequencies followed by the
/// least and greatest of them and the frequency of a NULL element, and
/// the histogram of distinct-element counts (`STATISTIC_KIND_DECHIST`)
/// followed by their average. `arrays` holds each non-null array's
/// elements' keys; `None` with no array to analyse.
pub(crate) fn array_element_stats(
    arrays: &[Vec<Option<Vec<u8>>>],
    target: usize,
) -> Option<ArrayElementStats> {
    if arrays.is_empty() {
        return None;
    }
    let mut num_mcelem = target * 10;
    let bucket_width = (num_mcelem * 1000 / 7).max(1) as i64;
    // element key -> (frequency, delta, last array)
    let mut tracked: HashMap<Vec<u8>, (i64, i64, usize)> = HashMap::new();
    let mut counts: HashMap<i64, i64> = HashMap::new();
    let (mut b_current, mut element_no, mut null_elem_cnt) = (1i64, 0i64, 0i64);
    for (array_no, elements) in arrays.iter().enumerate() {
        let prev = element_no;
        let mut null_present = false;
        for e in elements {
            let Some(k) = e else {
                null_present = true;
                continue;
            };
            match tracked.get_mut(k) {
                Some(item) => {
                    if item.2 == array_no {
                        continue;
                    }
                    item.0 += 1;
                    item.2 = array_no;
                }
                None => {
                    tracked.insert(k.clone(), (1, b_current - 1, array_no));
                }
            }
            element_no += 1;
            if element_no % bucket_width == 0 {
                // prune_element_hashtable
                tracked.retain(|_, item| item.0 + item.1 > b_current);
                b_current += 1;
            }
        }
        if null_present {
            null_elem_cnt += 1;
        }
        *counts.entry(element_no - prev).or_insert(0) += 1;
    }
    let nonnull = arrays.len() as f64;
    let cutoff = 9 * element_no / bucket_width;
    let mut items: Vec<(Vec<u8>, i64)> = tracked
        .into_iter()
        .filter(|(_, item)| item.0 > cutoff)
        .map(|(k, item)| (k, item.0))
        .collect();
    let mut minfreq = items.iter().map(|i| i.1).min().unwrap_or(element_no);
    let maxfreq = items.iter().map(|i| i.1).max().unwrap_or(0);
    if num_mcelem < items.len() {
        items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        items.truncate(num_mcelem);
        minfreq = items.last().map_or(minfreq, |i| i.1);
    } else {
        num_mcelem = items.len();
    }
    let mut mcelem = Vec::new();
    let mut tail = Vec::new();
    if num_mcelem > 0 {
        items.sort_by(|a, b| a.0.cmp(&b.0));
        mcelem = items
            .into_iter()
            .map(|(k, f)| (k, (f as f64 / nonnull) as f32))
            .collect();
        tail = vec![
            (minfreq as f64 / nonnull) as f32,
            (maxfreq as f64 / nonnull) as f32,
            (null_elem_cnt as f64 / nonnull) as f32,
        ];
    }
    // The distinct-element count histogram.
    let num_hist = target.max(2) as i64;
    let mut sorted: Vec<(i64, i64)> = counts.into_iter().collect();
    sorted.sort_unstable();
    let delta = arrays.len() as i64 - 1;
    let mut j = 0usize;
    let mut frac = sorted[0].1 * (num_hist - 1);
    let mut hist = Vec::with_capacity(num_hist as usize + 1);
    for _ in 0..num_hist {
        while frac <= 0 {
            j += 1;
            frac += sorted[j].1 * (num_hist - 1);
        }
        hist.push(sorted[j].0 as f32);
        frac -= delta;
    }
    hist.push((element_no as f64 / nonnull) as f32);
    Some((mcelem, tail, hist))
}

/// A key `analyze_sample` made, as the column's value again.
pub(crate) fn value_of(key: &[u8], c: &secantus_pgcatalog::Column) -> Option<Bson> {
    // An array prints as its text, which the slot's array quotes.
    if let Some(e) = element_column(c) {
        let items = array_key_elements(key)?
            .iter()
            .map(|k| match k {
                Some(k) => value_of(k, &e),
                None => Some(Bson::Null),
            })
            .collect::<Option<Vec<_>>>()?;
        return Some(Bson::String(secantus_pgplan::render_array(&items)));
    }
    Some(match c.pg_type.trim_start_matches("pg_catalog.") {
        "int2" | "int4" => Bson::Int32(secantus_pgplan::colstats::int_of_key(key)? as i32),
        "int8" | "oid" => Bson::Int64(secantus_pgplan::colstats::int_of_key(key)?),
        t if secantus_pgplan::regobj::is_kind(t) => {
            secantus_pgplan::regobj::value(t, secantus_pgplan::colstats::int_of_key(key)?)
        }
        "float4" | "float8" => {
            Bson::Double(f64::from_bits(u64::from_be_bytes(key.try_into().ok()?)))
        }
        "bool" => Bson::Boolean(*key.first()? != 0),
        "aclitem" => Bson::String(String::from_utf8(key.to_vec()).ok()?),
        "text" | "varchar" | "character varying" | "name" | secantus_pgplan::QUOTED_CHAR => {
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
                if operators(&c.pg_type).is_none() && equality_only(&c.pg_type).is_none() {
                    continue;
                }
                let f = c.field();
                let values: Vec<Option<&Bson>> = docs.iter().map(|d| d.get(&f)).collect();
                let mut entry = Self::stats_entry(c, &values, target, false, totalrows);
                entry.insert("catalog", true);
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
        let (eq, lt, coll) =
            operators(&c.pg_type).or_else(|| Some((equality_only(&c.pg_type)?, 0, 0)))?;
        // A catalog's collatable column is `C` (its `attcollation`).
        let coll = if coll != 0 && entry.get_bool("catalog").unwrap_or(false) {
            C_COLLATION
        } else {
            coll
        };
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
        // `compute_array_stats`' slots, under the element's `=`.
        if let (Some(e), Some((elem_eq, _, _))) = (
            element_column(c),
            array_element(&c.pg_type).and_then(operators),
        ) {
            let nums =
                |name: &str| -> Option<Vec<Bson>> { Some(entry.get_array(name).ok()?.to_vec()) };
            if let (Ok(mcelem), Some(tail)) = (entry.get_array("mcelem"), nums("mcelem_tail")) {
                if !mcelem.is_empty() {
                    let mut values = Vec::new();
                    let mut numbers = Vec::new();
                    for m in mcelem {
                        let pair = m.as_array()?;
                        let Some(Bson::Binary(k)) = pair.first() else {
                            return None;
                        };
                        values.push(value_of(&k.bytes, &e)?);
                        numbers.push(pair.get(1)?.clone());
                    }
                    numbers.extend(tail);
                    slots.push((MCELEM, elem_eq, Bson::Array(numbers), Bson::Array(values)));
                }
            }
            if let Some(hist) = nums("dechist") {
                slots.push((DECHIST, elem_eq, Bson::Array(hist), Bson::Null));
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

#[cfg(test)]
mod array_tests {
    use super::*;

    fn key(items: &[Option<&str>]) -> Vec<u8> {
        array_key(
            &items
                .iter()
                .map(|i| i.map(|s| s.as_bytes().to_vec()))
                .collect::<Vec<_>>(),
        )
    }

    /// `array_cmp`: element by element, a prefix first, NULL after values.
    #[test]
    fn array_keys_order_as_array_cmp() {
        let ordered = [
            key(&[]),
            key(&[Some("a")]),
            key(&[Some("a"), Some("")]),
            key(&[Some("a"), Some("b")]),
            key(&[Some("a"), None]),
            key(&[Some("a\0")]),
            key(&[Some("b")]),
            key(&[None]),
        ];
        for pair in ordered.windows(2) {
            assert!(pair[0] < pair[1], "{pair:?}");
        }
        for k in &ordered {
            assert_eq!(array_key(&array_key_elements(k).unwrap()), *k);
        }
    }

    /// `construct_md_array` pads after each element; a NULL adds a bitmap.
    #[test]
    fn array_datum_sizes() {
        assert_eq!(array_datum_size("text", &[]), 16);
        assert_eq!(array_datum_size("text", &[Some(4 + 13)]), 44);
        assert_eq!(array_datum_size("text", &[Some(5), None]), 40);
        assert_eq!(array_datum_size("int4", &[Some(4), Some(4)]), 32);
    }

    /// `compute_array_stats`: each element once per array, DECHIST over
    /// the distinct counts and their average.
    #[test]
    fn array_element_slots() {
        let arrays = vec![
            vec![
                Some(b"x".to_vec()),
                Some(b"y".to_vec()),
                Some(b"x".to_vec()),
            ],
            vec![Some(b"x".to_vec()), None],
            vec![],
            vec![Some(b"f".to_vec())],
        ];
        let (mcelem, tail, hist) = array_element_stats(&arrays, 100).unwrap();
        assert_eq!(
            mcelem,
            vec![
                (b"f".to_vec(), 0.25),
                (b"x".to_vec(), 0.5),
                (b"y".to_vec(), 0.25)
            ]
        );
        assert_eq!(tail, vec![0.25, 0.5, 0.25]);
        assert_eq!(hist.len(), 101);
        assert_eq!((hist[0], hist[99], hist[100]), (0.0, 2.0, 1.0));
    }
}
