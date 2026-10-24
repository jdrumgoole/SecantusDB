//! The values extended statistics hold, as PostgreSQL 15.19 keeps them
//! (`src/backend/statistics/`): the text a `pg_ndistinct` and a
//! `pg_dependencies` print (`pg_ndistinct_out`, `pg_dependencies_out`), the
//! bytes a `pg_mcv_list` is (`statext_mcv_serialize`), and
//! `pg_mcv_list_items`, which reads them back. What ANALYZE computes is the
//! server's (`secantus-pg`'s `extstats.rs`), which sorts as PostgreSQL does.

use bson::Bson;

/// `pg_ndistinct_out`: `{"1, 2": 4, ...}`, each estimate as an integer.
pub fn ndistinct_text(items: &[(Vec<i16>, f64)]) -> String {
    let mut out = String::from("{");
    for (i, (attrs, n)) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        for (j, a) in attrs.iter().enumerate() {
            out.push_str(if j == 0 { "\"" } else { ", " });
            out.push_str(&a.to_string());
        }
        out.push_str(&format!("\": {}", *n as i32));
    }
    out.push('}');
    out
}

/// `pg_dependencies_out`: `{"1 => 2": 1.000000, ...}`, the last attribute
/// the one implied, each degree as `%f`.
pub fn dependencies_text(items: &[(Vec<i16>, f64)]) -> String {
    let mut out = String::from("{");
    for (i, (attrs, degree)) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push('"');
        for (j, a) in attrs.iter().enumerate() {
            if j + 1 == attrs.len() {
                out.push_str(" => ");
            } else if j > 0 {
                out.push_str(", ");
            }
            out.push_str(&a.to_string());
        }
        out.push_str(&format!("\": {degree:.6}"));
    }
    out.push('}');
    out
}

/// Days from 1970-01-01 to the civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date `days` after 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// 2000-01-01 in days from 1970-01-01.
const PG_EPOCH_DAYS: i64 = 10_957;

/// A stored date's `DateADT`: days since 2000-01-01, the infinities
/// `DATEVAL_NOEND` / `DATEVAL_NOBEGIN`.
fn date_days(text: &str) -> Option<i32> {
    match text.trim() {
        "infinity" => return Some(i32::MAX),
        "-infinity" => return Some(i32::MIN),
        _ => {}
    }
    let (body, bc) = match text.trim().strip_suffix(" BC") {
        Some(b) => (b, true),
        None => (text.trim(), false),
    };
    let mut parts = body.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    let y = if bc { 1 - y } else { y };
    i32::try_from(days_from_civil(y, m, d) - PG_EPOCH_DAYS).ok()
}

/// A `DateADT` as a stored date's text.
fn date_text(days: i32) -> String {
    match days {
        i32::MAX => return "infinity".into(),
        i32::MIN => return "-infinity".into(),
        _ => {}
    }
    let (y, m, d) = civil_from_days(i64::from(days) + PG_EPOCH_DAYS);
    if y <= 0 {
        format!("{:04}-{m:02}-{d:02} BC", 1 - y)
    } else {
        format!("{y:04}-{m:02}-{d:02}")
    }
}

/// `STATS_MCV_MAGIC` and `STATS_MCV_TYPE_BASIC`.
const MCV_MAGIC: u32 = 0xE1A6_51C2;
const MCV_TYPE_BASIC: u32 = 1;

/// One item of an MCV list: its values (`None` for a NULL), its frequency
/// and the frequency its values would have were the columns independent.
#[derive(Clone, Debug, PartialEq)]
pub struct McvItem {
    pub values: Vec<Option<Bson>>,
    pub frequency: f64,
    pub base_frequency: f64,
}

/// A type's `(typlen, typbyval)` for the types whose datums are written
/// here; `None` for any other.
fn datum_layout(pg_type: &str) -> Option<(i32, bool)> {
    Some(match pg_type.trim_start_matches("pg_catalog.") {
        "bool" => (1, true),
        "int2" => (2, true),
        "int4" | "oid" | "float4" | "date" => (4, true),
        "int8"
        | "float8"
        | "timestamp"
        | "timestamptz"
        | "timestamp without time zone"
        | "timestamp with time zone" => (8, true),
        "text" | "varchar" | "character varying" | "bpchar" | "character" | "numeric" => {
            (-1, false)
        }
        _ => return None,
    })
}

/// A `numeric`'s stored form (`numeric.c`'s `make_result`): its text's
/// base-10000 digits about the decimal point, leading and trailing zero
/// groups gone, under a short header (sign, display scale, weight) or,
/// when those do not fit one, a long one.
fn numeric_datum(text: &str) -> Option<Vec<u8>> {
    let t = text.trim();
    match t {
        "NaN" => return Some(0xC000u16.to_le_bytes().to_vec()),
        "Infinity" => return Some(0xD000u16.to_le_bytes().to_vec()),
        "-Infinity" => return Some(0xF000u16.to_le_bytes().to_vec()),
        _ => {}
    }
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, t.trim_start_matches('+')),
    };
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    if !int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let dscale = frac.len();
    let int = int.trim_start_matches('0');
    let pad_int = (4 - int.len() % 4) % 4;
    let mut digits_text = "0".repeat(pad_int) + int;
    let int_groups = digits_text.len() / 4;
    digits_text.push_str(frac);
    digits_text.push_str(&"0".repeat((4 - frac.len() % 4) % 4));
    let mut groups: Vec<i16> = digits_text
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap_or("0").parse().unwrap_or(0))
        .collect();
    let mut weight = int_groups as i32 - 1;
    while groups.first() == Some(&0) {
        groups.remove(0);
        weight -= 1;
    }
    while groups.last() == Some(&0) {
        groups.pop();
    }
    if groups.is_empty() {
        weight = 0;
    }
    let neg = neg && !groups.is_empty();
    let mut out = Vec::new();
    if dscale <= 63 && (-64..=63).contains(&weight) {
        let header: u16 = 0x8000
            | if neg { 0x2000 } else { 0 }
            | ((dscale as u16) << 7)
            | ((weight as i16 as u16) & 0x7F);
        out.extend_from_slice(&header.to_le_bytes());
    } else {
        let sign_dscale: u16 = if neg { 0x4000 } else { 0 } | (dscale as u16 & 0x3FFF);
        out.extend_from_slice(&sign_dscale.to_le_bytes());
        out.extend_from_slice(&(weight as i16).to_le_bytes());
    }
    for g in groups {
        out.extend_from_slice(&g.to_le_bytes());
    }
    Some(out)
}

/// A [`numeric_datum`] as its text again.
fn numeric_text(b: &[u8]) -> Option<String> {
    let header = u16::from_le_bytes(b.get(..2)?.try_into().ok()?);
    match header {
        0xC000 => return Some("NaN".into()),
        0xD000 => return Some("Infinity".into()),
        0xF000 => return Some("-Infinity".into()),
        _ => {}
    }
    let (neg, dscale, weight, rest) = if header & 0x8000 != 0 {
        let w = (header & 0x3F) as i32 - if header & 0x40 != 0 { 64 } else { 0 };
        (
            header & 0x2000 != 0,
            ((header >> 7) & 0x3F) as usize,
            w,
            &b[2..],
        )
    } else {
        let w = i16::from_le_bytes(b.get(2..4)?.try_into().ok()?) as i32;
        (header & 0x4000 != 0, (header & 0x3FFF) as usize, w, &b[4..])
    };
    let groups: Vec<i32> = rest
        .chunks(2)
        .map(|c| i16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]) as i32)
        .collect();
    // Digits from group `weight` down to the scale.
    let mut int = String::new();
    for w in (0..=weight.max(0)).rev() {
        let i = (weight - w) as usize;
        let g = if weight >= w {
            groups.get(i).copied().unwrap_or(0)
        } else {
            0
        };
        int.push_str(&format!("{g:04}"));
    }
    let int = int.trim_start_matches('0');
    let int = if int.is_empty() { "0" } else { int };
    let mut frac = String::new();
    let mut w = -1;
    while frac.len() < dscale {
        let i = weight - w;
        let g = if i >= 0 {
            groups.get(i as usize).copied().unwrap_or(0)
        } else {
            0
        };
        frac.push_str(&format!("{g:04}"));
        w -= 1;
    }
    frac.truncate(dscale);
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(int);
    if dscale > 0 {
        out.push('.');
        out.push_str(&frac);
    }
    Some(out)
}

/// A value's datum bytes: a by-value type's `store_att_byval` (little
/// endian), a varlena's data without its header.
fn datum_bytes(pg_type: &str, v: &Bson) -> Option<Vec<u8>> {
    let int = |v: &Bson| match v {
        Bson::Int32(n) => Some(i64::from(*n)),
        Bson::Int64(n) => Some(*n),
        Bson::Double(f) if f.fract() == 0.0 => Some(*f as i64),
        _ => None,
    };
    Some(match pg_type.trim_start_matches("pg_catalog.") {
        "bool" => vec![u8::from(matches!(v, Bson::Boolean(true)))],
        "int2" => (int(v)? as i16).to_le_bytes().to_vec(),
        "int4" | "oid" => (int(v)? as i32).to_le_bytes().to_vec(),
        "int8" => int(v)?.to_le_bytes().to_vec(),
        "float4" => (number(v)? as f32).to_le_bytes().to_vec(),
        "float8" => number(v)?.to_le_bytes().to_vec(),
        "text" | "varchar" | "character varying" | "bpchar" | "character" => match v {
            Bson::String(s) => s.as_bytes().to_vec(),
            _ => return None,
        },
        "numeric" => numeric_datum(&crate::typeout::output_text(v, "numeric"))?,
        // Days since 2000-01-01; a date is stored as its text.
        "date" => match v {
            Bson::String(t) => date_days(t)?.to_le_bytes().to_vec(),
            _ => return None,
        },
        // Microseconds since 2000-01-01 (`Timestamp`), from the stored
        // instant and its hidden microseconds.
        "timestamp"
        | "timestamptz"
        | "timestamp without time zone"
        | "timestamp with time zone" => (crate::instant_micros_pub(v)? - POSTGRES_EPOCH_MICROS)
            .to_le_bytes()
            .to_vec(),
        _ => return None,
    })
}

/// 2000-01-01 00:00:00 UTC in microseconds since the Unix epoch.
const POSTGRES_EPOCH_MICROS: i64 = 946_684_800_000_000;

/// A datum written by [`datum_bytes`], as the value again.
fn datum_value(pg_type: &str, b: &[u8]) -> Option<Bson> {
    Some(match pg_type.trim_start_matches("pg_catalog.") {
        "bool" => Bson::Boolean(*b.first()? != 0),
        "int2" => Bson::Int32(i32::from(i16::from_le_bytes(b.try_into().ok()?))),
        "int4" | "oid" => Bson::Int32(i32::from_le_bytes(b.try_into().ok()?)),
        "int8" => Bson::Int64(i64::from_le_bytes(b.try_into().ok()?)),
        "float4" => Bson::Double(f64::from(f32::from_le_bytes(b.try_into().ok()?))),
        "float8" => Bson::Double(f64::from_le_bytes(b.try_into().ok()?)),
        "numeric" => Bson::String(numeric_text(b)?),
        "date" => Bson::String(date_text(i32::from_le_bytes(b.try_into().ok()?))),
        "timestamp"
        | "timestamptz"
        | "timestamp without time zone"
        | "timestamp with time zone" => {
            crate::instant_value(i64::from_le_bytes(b.try_into().ok()?) + POSTGRES_EPOCH_MICROS)
        }
        _ => Bson::String(String::from_utf8(b.to_vec()).ok()?),
    })
}

/// A numeric value as a double.
fn number(v: &Bson) -> Option<f64> {
    match v {
        Bson::Double(f) => Some(*f),
        Bson::Int32(n) => Some(f64::from(*n)),
        Bson::Int64(n) => Some(*n as f64),
        _ => None,
    }
}

fn put_u32(out: &mut Vec<u8>, n: u32) {
    out.extend_from_slice(&n.to_le_bytes());
}

/// `statext_mcv_serialize`: the header, each dimension's
/// `DimensionInfo`, its distinct values deduplicated in the type's order
/// (`order`, the values' indexes sorted), then each item's NULL flags,
/// frequencies and value indexes. `None` when a dimension's type is not
/// one whose datums are written here.
pub fn mcv_bytes(
    items: &[McvItem],
    types: &[(i64, String)],
    compare: &dyn Fn(&Bson, &Bson) -> std::cmp::Ordering,
) -> Option<Vec<u8>> {
    let ndims = types.len();
    let mut out = Vec::new();
    put_u32(&mut out, MCV_MAGIC);
    put_u32(&mut out, MCV_TYPE_BASIC);
    put_u32(&mut out, items.len() as u32);
    out.extend_from_slice(&(ndims as i16).to_le_bytes());
    for (oid, _) in types {
        put_u32(&mut out, *oid as u32);
    }
    // Each dimension's distinct values, in the type's order.
    let mut distinct: Vec<Vec<Bson>> = Vec::with_capacity(ndims);
    let mut infos: Vec<(i32, i32, i32, i32, bool)> = Vec::with_capacity(ndims);
    let mut data: Vec<Vec<u8>> = Vec::with_capacity(ndims);
    for (dim, (_, ty)) in types.iter().enumerate() {
        let (typlen, byval) = datum_layout(ty)?;
        let mut vals: Vec<Bson> = items
            .iter()
            .filter_map(|it| it.values[dim].clone())
            .collect();
        vals.sort_by(|a, b| compare(a, b));
        vals.dedup_by(|a, b| compare(a, b) == std::cmp::Ordering::Equal);
        let mut bytes = Vec::new();
        let mut aligned = 0i32;
        for v in &vals {
            let d = datum_bytes(ty, v)?;
            if typlen > 0 {
                bytes.extend_from_slice(&d);
            } else {
                put_u32(&mut bytes, d.len() as u32);
                bytes.extend_from_slice(&d);
                aligned += ((4 + d.len() as i32) + 7) / 8 * 8;
            }
        }
        let nbytes = bytes.len() as i32;
        let nbytes_aligned = if byval { 0 } else { aligned };
        infos.push((vals.len() as i32, nbytes, nbytes_aligned, typlen, byval));
        distinct.push(vals);
        data.push(bytes);
    }
    // DimensionInfo: nvalues, nbytes, nbytes_aligned, typlen (ints), then
    // typbyval and three bytes of padding.
    for (nvalues, nbytes, aligned, typlen, byval) in &infos {
        for n in [*nvalues, *nbytes, *aligned, *typlen] {
            out.extend_from_slice(&n.to_le_bytes());
        }
        out.extend_from_slice(&[u8::from(*byval), 0, 0, 0]);
    }
    for d in &data {
        out.extend_from_slice(d);
    }
    for it in items {
        for v in &it.values {
            out.push(u8::from(v.is_none()));
        }
        out.extend_from_slice(&it.frequency.to_le_bytes());
        out.extend_from_slice(&it.base_frequency.to_le_bytes());
        for (dim, v) in it.values.iter().enumerate() {
            let index = match v {
                Some(v) => distinct[dim]
                    .iter()
                    .position(|d| compare(d, v) == std::cmp::Ordering::Equal)?
                    as u16,
                None => 0,
            };
            out.extend_from_slice(&index.to_le_bytes());
        }
    }
    Some(out)
}

/// `statext_mcv_deserialize` for the bytes [`mcv_bytes`] writes: the
/// items, with each dimension's type name.
pub fn mcv_items(bytes: &[u8]) -> Option<(Vec<String>, Vec<McvItem>)> {
    let mut at = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        let s = bytes.get(at..at + n)?;
        at += n;
        Some(s)
    };
    let u32_of = |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap_or([0; 4]));
    if u32_of(take(4)?) != MCV_MAGIC || u32_of(take(4)?) != MCV_TYPE_BASIC {
        return None;
    }
    let nitems = u32_of(take(4)?) as usize;
    let ndims = i16::from_le_bytes(take(2)?.try_into().ok()?) as usize;
    let mut types = Vec::with_capacity(ndims);
    for _ in 0..ndims {
        let oid = u32_of(take(4)?);
        types.push(crate::pgtypes::name_of_oid(i64::from(oid))?.to_string());
    }
    let mut infos = Vec::with_capacity(ndims);
    for _ in 0..ndims {
        let nvalues = u32_of(take(4)?) as usize;
        let _nbytes = take(4)?;
        let _aligned = take(4)?;
        let typlen = i32::from_le_bytes(take(4)?.try_into().ok()?);
        let _byval = take(4)?;
        infos.push((nvalues, typlen));
    }
    let mut values: Vec<Vec<Bson>> = Vec::with_capacity(ndims);
    for (dim, (nvalues, typlen)) in infos.iter().enumerate() {
        let mut vals = Vec::with_capacity(*nvalues);
        for _ in 0..*nvalues {
            let d = if *typlen > 0 {
                take(*typlen as usize)?
            } else {
                let len = u32_of(take(4)?) as usize;
                take(len)?
            };
            vals.push(datum_value(&types[dim], d)?);
        }
        values.push(vals);
    }
    let mut items = Vec::with_capacity(nitems);
    for _ in 0..nitems {
        let nulls: Vec<bool> = take(ndims)?.iter().map(|b| *b != 0).collect();
        let frequency = f64::from_le_bytes(take(8)?.try_into().ok()?);
        let base_frequency = f64::from_le_bytes(take(8)?.try_into().ok()?);
        let mut vals = Vec::with_capacity(ndims);
        for dim in 0..ndims {
            let index = u16::from_le_bytes(take(2)?.try_into().ok()?) as usize;
            vals.push(if nulls[dim] {
                None
            } else {
                Some(values[dim].get(index)?.clone())
            });
        }
        items.push(McvItem {
            values: vals,
            frequency,
            base_frequency,
        });
    }
    Some((types, items))
}

/// `pg_mcv_list_items(pg_mcv_list)`: `(index, values, nulls, frequency,
/// base_frequency)` per item, each value in its type's text form.
pub fn mcv_list_rows(arg: &Bson) -> crate::Result<Vec<Vec<Bson>>> {
    let bytes = match arg {
        Bson::Null => return Ok(Vec::new()),
        Bson::Binary(b) => b.bytes.clone(),
        _ => {
            return Err(crate::Error::Sqlstate(
                "42883",
                "function pg_mcv_list_items takes a pg_mcv_list".into(),
            ))
        }
    };
    let (types, items) = mcv_items(&bytes)
        .ok_or_else(|| crate::Error::Sqlstate("XX000", "invalid MCV list".into()))?;
    Ok(items
        .into_iter()
        .enumerate()
        .map(|(i, it)| {
            let values = it
                .values
                .iter()
                .zip(&types)
                .map(|(v, ty)| match v {
                    Some(v) => Bson::String(crate::typeout::output_text(v, ty)),
                    None => Bson::Null,
                })
                .collect();
            let nulls = it
                .values
                .iter()
                .map(|v| Bson::Boolean(v.is_none()))
                .collect();
            vec![
                Bson::Int32(i as i32),
                Bson::Array(values),
                Bson::Array(nulls),
                Bson::Double(it.frequency),
                Bson::Double(it.base_frequency),
            ]
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The datums PostgreSQL 15.19 wrote for these numerics.
    #[test]
    fn numeric_datums_are_postgresqls() {
        let hex = |t: &str| -> String {
            numeric_datum(t)
                .unwrap()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        assert_eq!(hex("-12345.678"), "81a1010029097c1a");
        assert_eq!(hex("0.0"), "8080");
        assert_eq!(hex("0.00012"), "ff820100d007");
        assert_eq!(hex("1.5"), "808001008813");
        assert_eq!(hex("100000"), "01800a00");
        for t in ["-12345.678", "0.0", "0.00012", "1.5", "100000", "7", "-0.5"] {
            assert_eq!(numeric_text(&numeric_datum(t).unwrap()).unwrap(), t);
        }
    }

    /// PostgreSQL 15.19's `pg_stats_ext` for `(a, b, c)` over 42 rows.
    #[test]
    fn text_forms() {
        assert_eq!(
            ndistinct_text(&[(vec![1, 2], 6.0), (vec![1, 2, 3], 14.0)]),
            r#"{"1, 2": 6, "1, 2, 3": 14}"#
        );
        assert_eq!(
            dependencies_text(&[(vec![1, 2], 0.952_380_952), (vec![1, 3, 2], 1.0)]),
            r#"{"1 => 2": 0.952381, "1, 3 => 2": 1.000000}"#
        );
    }

    /// The bytes PostgreSQL 15.19 wrote for an MCV list on `(a int, b int)`.
    #[test]
    fn mcv_bytes_are_postgresqls() {
        // 10 of 42 rows each; a's value in 10, b's in 21; one row each.
        let big = 10.0 / 42.0;
        let half = (10.0 / 42.0) * (21.0 / 42.0);
        let small = 1.0 / 42.0;
        let item = |a: Option<i32>, b: i32, fr: f64, base: f64| McvItem {
            values: vec![a.map(Bson::Int32), Some(Bson::Int32(b))],
            frequency: fr,
            base_frequency: base,
        };
        let items = vec![
            item(Some(0), 0, big, half),
            item(Some(1), 1, big, half),
            item(Some(2), 0, big, half),
            item(Some(3), 1, big, half),
            item(None, 0, small, small),
            item(None, 1, small, small),
        ];
        let types = vec![(23, "int4".to_string()), (23, "int4".to_string())];
        let cmp = |a: &Bson, b: &Bson| crate::compare_values(a, b).unwrap();
        let bytes = mcv_bytes(&items, &types, &cmp).unwrap();
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "c251a6e10100000006000000020017000000170000000400000010000000000000000400000001000000\
             020000000800000000000000040000000100000000000000010000000200000003000000000000000100\
             000000009ee7799ee779ce3f9ee7799ee779be3f0000000000009ee7799ee779ce3f9ee7799ee779be3f\
             0100010000009ee7799ee779ce3f9ee7799ee779be3f0200000000009ee7799ee779ce3f9ee7799ee779\
             be3f030001000100188661188661983f188661188661983f000000000100188661188661983f18866118\
             8661983f00000100"
        );
        let (tys, back) = mcv_items(&bytes).unwrap();
        assert_eq!(tys, vec!["int4", "int4"]);
        assert_eq!(back, items);
    }
}
