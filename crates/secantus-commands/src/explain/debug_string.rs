//! mongod's query-tree text: the `Tree: ...` its planner prints when it
//! cannot plan a query (an unknown hint, `notablescan`), and the canonical
//! query around it.
//!
//! Transcribed from what mongod 8.2.11 prints (`MatchExpression::debugString`
//! and `CanonicalQuery::toString`), measured 2026-10-20 over about 130
//! shapes. The tree is the normalised match expression `parsedQuery`
//! reports, with each `$in` as written: one line per node, indented four
//! spaces a level, a node's children under it, and each value in
//! `BSONElement::toString`'s form (a string quoted and not escaped, a double
//! with `.0` when it prints as a whole number, `new Date(ms)`).

use bson::spec::BinarySubtype;
use bson::{Bson, Document};

/// What mongod's planner says it was asked: the namespace, the paging, the
/// query tree, the sort and projection, and the collation when one was
/// given.
pub(crate) struct CanonicalQuery<'a> {
    pub ns: String,
    pub filter: &'a Document,
    pub sort: Option<&'a Document>,
    pub projection: Option<&'a Document>,
    pub skip: Option<i64>,
    pub limit: Option<i64>,
    pub collation: Option<&'a Document>,
}

impl<'a> CanonicalQuery<'a> {
    /// The query of `filter` alone on `db.coll`.
    pub(crate) fn plain(db: &str, coll: &str, filter: &'a Document) -> CanonicalQuery<'a> {
        CanonicalQuery {
            ns: format!("{db}.{coll}"),
            filter,
            sort: None,
            projection: None,
            skip: None,
            limit: None,
            collation: None,
        }
    }

    /// `error processing query: <this> planner returned error :: caused by
    /// :: <reason>`, as mongod words a query it could not plan.
    pub(crate) fn planner_error(&self, reason: &str) -> String {
        format!(
            "error processing query: {} planner returned error :: caused by :: {reason}",
            self.render()
        )
    }

    fn render(&self) -> String {
        let mut out = format!("ns={}", self.ns);
        if let Some(limit) = self.limit.filter(|n| *n != 0) {
            out.push_str(&format!(" limit={}", limit.unsigned_abs()));
        }
        if let Some(skip) = self.skip.filter(|n| *n != 0) {
            out.push_str(&format!(" skip={skip}"));
        }
        out.push_str("Tree: ");
        out.push_str(&debug_string(self.filter));
        // A `$natural` sort is a scan direction, not a sort.
        let sort = self
            .sort
            .filter(|s| !s.contains_key("$natural"))
            .map_or_else(|| "{}".to_string(), |s| value(&Bson::Document(s.clone())));
        out.push_str(&format!("Sort: {sort}\n"));
        let proj = self
            .projection
            .map_or_else(|| "{}".to_string(), |p| value(&Bson::Document(p.clone())));
        out.push_str(&format!("Proj: {proj}\n"));
        if let Some(collation) = self.collation {
            out.push_str(&format!(
                "Collation: {}\n",
                value(&Bson::Document(collation.clone()))
            ));
        }
        out
    }
}

/// The query tree of `filter`, one `\n`-terminated line per node.
pub(crate) fn debug_string(filter: &Document) -> String {
    let parsed = super::parsed::parsed_query_as_written(filter);
    let mut out = String::new();
    node(&parsed, 0, &mut out);
    out
}

fn indent(level: usize, out: &mut String) {
    for _ in 0..level {
        out.push_str("    ");
    }
}

/// One node of the normalised tree: `{}` (match everything), a logical
/// operator over children, or one predicate on a path.
fn node(d: &Document, level: usize, out: &mut String) {
    if d.len() != 1 {
        // Several clauses side by side are an `$and` (an empty one matches
        // everything).
        indent(level, out);
        out.push_str("$and\n");
        for (k, v) in d {
            let mut one = Document::new();
            one.insert(k.clone(), v.clone());
            node(&one, level + 1, out);
        }
        return;
    }
    let (key, val) = d.iter().next().expect("one key");
    match key.as_str() {
        "$and" | "$or" | "$nor" => {
            let children: Vec<&Document> = val
                .as_array()
                .map(|a| a.iter().filter_map(Bson::as_document).collect())
                .unwrap_or_default();
            // A `$nor` of one is the `$not` mongod optimises it to.
            let name = if key == "$nor" && children.len() == 1 {
                "$not"
            } else {
                key.as_str()
            };
            indent(level, out);
            out.push_str(name);
            out.push('\n');
            for child in children {
                node(child, level + 1, out);
            }
        }
        "$alwaysFalse" | "$alwaysTrue" => {
            indent(level, out);
            out.push_str(&format!("{key}: 1\n"));
        }
        "$expr" => {
            indent(level, out);
            out.push_str(&format!("$expr {}\n", expression(val)));
        }
        path => match val {
            Bson::Document(ops) if ops.keys().next().is_some_and(|k| k.starts_with('$')) => {
                predicate(path, ops, level, out)
            }
            other => leaf(path, "$eq", other, level, out),
        },
    }
}

/// The predicates on one path. A geo operator prints the clause it was
/// given; each other operator is a node of its own.
fn predicate(path: &str, ops: &Document, level: usize, out: &mut String) {
    if ops.contains_key("$regex") {
        indent(level, out);
        out.push_str(&format!(
            "{path} regex /{}/{}\n",
            ops.get_str("$regex").unwrap_or_default(),
            ops.get_str("$options").unwrap_or_default()
        ));
        return;
    }
    if ops
        .keys()
        .any(|k| matches!(k.as_str(), "$geoWithin" | "$geoIntersects" | "$within"))
    {
        let mut raw = Document::new();
        raw.insert(path, Bson::Document(ops.clone()));
        indent(level, out);
        out.push_str(&format!("GEO raw = {}\n", value(&Bson::Document(raw))));
        return;
    }
    if ops.len() > 1 {
        indent(level, out);
        out.push_str("$and\n");
        for (op, arg) in ops {
            let mut one = Document::new();
            one.insert(op.clone(), arg.clone());
            predicate(path, &one, level + 1, out);
        }
        return;
    }
    let (op, arg) = ops.iter().next().expect("one operator");
    match op.as_str() {
        "$not" => {
            indent(level, out);
            out.push_str("$not\n");
            match arg {
                Bson::Document(inner) => predicate(path, inner, level + 1, out),
                other => leaf(path, "$eq", other, level + 1, out),
            }
        }
        "$exists" => {
            indent(level, out);
            out.push_str(&format!("{path} exists\n"));
        }
        "$in" => {
            let items = arg.as_array().cloned().unwrap_or_default();
            let (regexes, values): (Vec<&Bson>, Vec<&Bson>) = items
                .iter()
                .partition(|v| matches!(v, Bson::RegularExpression(_)));
            let values: Vec<String> = values.into_iter().map(value).collect();
            let regexes: String = regexes
                .into_iter()
                .map(|r| format!("{} ", value(r)))
                .collect();
            indent(level, out);
            out.push_str(&format!("{path} $in [ {}{regexes}]\n", values.join(" ")));
        }
        "$size" => {
            indent(level, out);
            out.push_str(&format!("{path} $size : {}\n", value(arg)));
        }
        "$type" => {
            let codes = match arg {
                Bson::Array(a) => a.clone(),
                other => vec![other.clone()],
            };
            let listed: Vec<String> = codes
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{i}: {}", value(c)))
                .collect();
            indent(level, out);
            out.push_str(&format!("{path} $type: {{ {} }}\n", listed.join(", ")));
        }
        "$mod" => {
            let parts = arg.as_array().cloned().unwrap_or_default();
            let n = |i: usize| parts.get(i).map(value).unwrap_or_default();
            indent(level, out);
            out.push_str(&format!("{path} mod {} % x == {}\n", n(0), n(1)));
        }
        "$bitsAllSet" | "$bitsAllClear" | "$bitsAnySet" | "$bitsAnyClear" => {
            let positions: Vec<String> = arg
                .as_array()
                .map(|a| a.iter().map(value).collect())
                .unwrap_or_default();
            indent(level, out);
            out.push_str(&format!("{path} {op}: [{}]\n", positions.join(", ")));
        }
        "$elemMatch" => {
            let Bson::Document(inner) = arg else {
                return;
            };
            indent(level, out);
            if inner.keys().next().is_some_and(|k| k.starts_with('$'))
                && !matches!(
                    inner.keys().next().map(String::as_str),
                    Some("$and" | "$or" | "$nor" | "$expr")
                )
            {
                out.push_str(&format!("{path} $elemMatch (value)\n"));
                for (op, arg) in inner {
                    let mut one = Document::new();
                    one.insert(op.clone(), arg.clone());
                    predicate("", &one, level + 1, out);
                }
            } else {
                out.push_str(&format!("{path} $elemMatch (obj)\n"));
                node(inner, level + 1, out);
            }
        }
        _ => leaf(path, op, arg, level, out),
    }
}

/// `<path> <op> <value>`: the comparisons, and `$_internalExpr*`.
fn leaf(path: &str, op: &str, arg: &Bson, level: usize, out: &mut String) {
    indent(level, out);
    out.push_str(&format!("{path} {op} {}\n", value(arg)));
}

/// A value as `BSONElement::toString` prints it without its name.
pub(crate) fn value(v: &Bson) -> String {
    match v {
        Bson::Double(d) => double(*d),
        Bson::String(s) => format!("\"{s}\""),
        Bson::Document(d) if d.is_empty() => "{}".to_string(),
        Bson::Document(d) => {
            let fields: Vec<String> = d
                .iter()
                .map(|(k, v)| format!("{k}: {}", value(v)))
                .collect();
            format!("{{ {} }}", fields.join(", "))
        }
        Bson::Array(a) if a.is_empty() => "[]".to_string(),
        Bson::Array(a) => {
            let items: Vec<String> = a.iter().map(value).collect();
            format!("[ {} ]", items.join(", "))
        }
        Bson::Boolean(b) => b.to_string(),
        Bson::Null => "null".to_string(),
        Bson::Undefined => "undefined".to_string(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Decimal128(d) => d.to_string(),
        Bson::DateTime(dt) => format!("new Date({})", dt.timestamp_millis()),
        Bson::ObjectId(oid) => format!("ObjectId('{}')", oid.to_hex()),
        Bson::Timestamp(ts) => format!("Timestamp({}, {})", ts.time, ts.increment),
        Bson::Binary(b) => {
            let hex: String = b.bytes.iter().map(|byte| format!("{byte:02X}")).collect();
            format!("BinData({}, {hex})", u8::from(b.subtype))
        }
        Bson::RegularExpression(re) => format!("/{}/{}", re.pattern, re.options),
        Bson::JavaScriptCode(code) => code.clone(),
        Bson::JavaScriptCodeWithScope(c) => c.code.clone(),
        Bson::Symbol(s) => s.clone(),
        Bson::MinKey => "MinKey".to_string(),
        Bson::MaxKey => "MaxKey".to_string(),
        other => format!("{other}"),
    }
}

/// A double as mongod prints one: sixteen significant digits (`%.16g`),
/// with `.0` added when that reads as a whole number.
fn double(d: f64) -> String {
    let s = if d.is_nan() {
        "nan".to_string()
    } else if d.is_infinite() {
        if d > 0.0 { "inf" } else { "-inf" }.to_string()
    } else {
        g16(d)
    };
    if s.bytes().all(|b| b == b'-' || b.is_ascii_digit()) {
        format!("{s}.0")
    } else {
        s
    }
}

/// C's `%.16g`.
fn g16(d: f64) -> String {
    g(d, 16)
}

/// C's `%.<precision>g` (C++'s default stream format at precision 6).
pub(crate) fn g(d: f64, precision: i32) -> String {
    if d.is_nan() {
        return "nan".to_string();
    }
    if d.is_infinite() {
        return if d > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    let p = precision;
    if d == 0.0 {
        return if d.is_sign_negative() { "-0" } else { "0" }.to_string();
    }
    let sci = format!("{:.*e}", (p - 1) as usize, d);
    let (mantissa, exp) = sci.split_once('e').expect("exponent");
    let exp: i32 = exp.parse().expect("exponent digits");
    if !(-4..p).contains(&exp) {
        let mantissa = trim_zeros(mantissa);
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exp.abs())
    } else {
        let decimals = (p - 1 - exp).max(0) as usize;
        trim_zeros(&format!("{d:.decimals$}")).to_string()
    }
}

fn trim_zeros(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

/// An aggregation expression as mongod's `Value` prints it inside `$expr`:
/// `{$gt: ["$a", {$const: 1}]}`, keys bare, no spaces inside the braces.
fn expression(v: &Bson) -> String {
    match v {
        Bson::Document(d) => {
            let fields: Vec<String> = d
                .iter()
                .map(|(k, v)| format!("{k}: {}", expression(v)))
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
        Bson::Array(a) => {
            let items: Vec<String> = a.iter().map(expression).collect();
            format!("[{}]", items.join(", "))
        }
        Bson::String(s) => format!("\"{s}\""),
        Bson::Double(d) if d.fract() == 0.0 && d.is_finite() && d.abs() < 1e15 => {
            format!("{}", *d as i64)
        }
        Bson::Double(d) => g16(*d),
        Bson::DateTime(dt) => dt
            .try_to_rfc3339_string()
            .map(|s| {
                // `2020-01-01T00:00:00.000Z`: milliseconds, always.
                let ms = dt.timestamp_millis().rem_euclid(1000);
                match s.split_once('.') {
                    Some((head, _)) => format!("{head}.{ms:03}Z"),
                    None => format!("{}.{ms:03}Z", s.trim_end_matches('Z')),
                }
            })
            .unwrap_or_default(),
        Bson::Binary(b) if b.subtype == BinarySubtype::Generic => value(v),
        other => value(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::{doc, oid::ObjectId, Binary, DateTime, Decimal128, Regex, Timestamp};

    fn tree(filter: Document) -> String {
        debug_string(&filter)
    }

    /// Each line as mongod 8.2.11 printed it (2026-10-20).
    #[test]
    fn leaves_print_as_mongod_prints_them() {
        let cases: Vec<(Document, &str)> = vec![
            (doc! {"a": 1}, "a $eq 1\n"),
            (doc! {"a": 1.5}, "a $eq 1.5\n"),
            (doc! {"a": 2.0}, "a $eq 2.0\n"),
            (doc! {"a": 7_i64}, "a $eq 7\n"),
            (doc! {"a": -0.0}, "a $eq -0.0\n"),
            (doc! {"a": f64::NAN}, "a $eq nan\n"),
            (doc! {"a": f64::INFINITY}, "a $eq inf\n"),
            (doc! {"a": 1e20}, "a $eq 1e+20\n"),
            (doc! {"a": 1e-7}, "a $eq 1e-07\n"),
            (doc! {"a": 123456789.125}, "a $eq 123456789.125\n"),
            (doc! {"a": 1e15}, "a $eq 1000000000000000.0\n"),
            (doc! {"a": 1e16}, "a $eq 1e+16\n"),
            (
                doc! {"a": 123456789012345678.0},
                "a $eq 1.234567890123457e+17\n",
            ),
            (doc! {"a": 1.0 / 3.0}, "a $eq 0.3333333333333333\n"),
            (doc! {"a": -1.25e-5}, "a $eq -1.25e-05\n"),
            (doc! {"a": "x\"y"}, "a $eq \"x\"y\"\n"),
            (doc! {"a": true}, "a $eq true\n"),
            (doc! {"a": Bson::Null}, "a $eq null\n"),
            (
                doc! {"a": DateTime::from_millis(1577934245678)},
                "a $eq new Date(1577934245678)\n",
            ),
            (
                doc! {"a": ObjectId::parse_str("5f1a2b3c4d5e6f7a8b9c0d1e").unwrap()},
                "a $eq ObjectId('5f1a2b3c4d5e6f7a8b9c0d1e')\n",
            ),
            (doc! {"a": Bson::MinKey}, "a $eq MinKey\n"),
            (
                doc! {"a": Timestamp {time: 5, increment: 6}},
                "a $eq Timestamp(5, 6)\n",
            ),
            (
                doc! {"a": Binary {subtype: BinarySubtype::Uuid, bytes: vec![0xab, 0xcd]}},
                "a $eq BinData(4, ABCD)\n",
            ),
            (
                doc! {"a": Decimal128::from_bytes([0; 16])},
                "a $eq 0E-6176\n",
            ),
            (
                doc! {"a": {"x": 1, "y": [1, {"z": "q"}]}},
                "a $eq { x: 1, y: [ 1, { z: \"q\" } ] }\n",
            ),
            (doc! {"a": []}, "a $eq []\n"),
            (doc! {"a": [[1], []]}, "a $eq [ [ 1 ], [] ]\n"),
            (doc! {"a": {"$gt": 1}}, "a $gt 1\n"),
            (
                doc! {"a": {"$in": ["b", "a", 3, Bson::Null]}},
                "a $in [ \"b\" \"a\" 3 null]\n",
            ),
            (doc! {"a": {"$in": [3, 1, 3]}}, "a $in [ 3 1 3]\n"),
            (
                doc! {"a": {"$in": [Regex {pattern: "b".into(), options: String::new()}, 2,
                Regex {pattern: "a".into(), options: "i".into()}, 1]}},
                "a $in [ 2 1/b/ /a/i ]\n",
            ),
            (doc! {"a": {"$exists": true}}, "a exists\n"),
            (
                doc! {"a": Regex {pattern: "a.b".into(), options: "msx".into()}},
                "a regex /a.b/msx\n",
            ),
            (doc! {"a": {"$size": 0}}, "a $size : 0\n"),
            (
                doc! {"a": {"$type": ["string", "int"]}},
                "a $type: { 0: 2, 1: 16 }\n",
            ),
            (
                doc! {"a": {"$type": "number"}},
                "a $type: { 0: \"number\" }\n",
            ),
            (doc! {"a": {"$mod": [3, -1]}}, "a mod 3 % x == -1\n"),
            (doc! {"a": {"$bitsAnySet": 6}}, "a $bitsAnySet: [1, 2]\n"),
            (
                doc! {"a": {"$bitsAnyClear": Binary {subtype: BinarySubtype::Generic, bytes: vec![3]}}},
                "a $bitsAnyClear: [0, 1]\n",
            ),
            (doc! {"a.0.b": 1}, "a.0.b $eq 1\n"),
        ];
        for (filter, want) in cases {
            assert_eq!(tree(filter.clone()), want, "{filter:?}");
        }
    }

    #[test]
    fn trees_indent_their_children() {
        let cases: Vec<(Document, &str)> = vec![
            (doc! {}, "$and\n"),
            (doc! {"a": {"$gt": 1}, "b": "x"}, "$and\n    b $eq \"x\"\n    a $gt 1\n"),
            (doc! {"a": {"$exists": false}}, "$not\n    a exists\n"),
            (doc! {"a": {"$ne": 3}}, "$not\n    a $eq 3\n"),
            (doc! {"a": {"$nin": []}}, "$not\n    $alwaysFalse: 1\n"),
            (doc! {"a": {"$not": {"$in": [1]}}}, "$not\n    a $eq 1\n"),
            (doc! {"a": {"$not": Regex {pattern: "x".into(), options: String::new()}}}, "$not\n    a regex /x/\n"),
            (doc! {"$nor": [{"a": 1}]}, "$not\n    a $eq 1\n"),
            (doc! {"$nor": [{"a": 1}, {"b": 2}]}, "$nor\n    a $eq 1\n    b $eq 2\n"),
            (doc! {"$or": [{"a": 1}, {"a": 2}]}, "a $in [ 1 2]\n"),
            (
                doc! {"$and": [{"a": 1}, {"$or": [{"b": 1}, {"c": {"$nin": [1, 2]}}]}]},
                "$and\n    $or\n        b $eq 1\n        $not\n            c $in [ 1 2]\n    a $eq 1\n",
            ),
            (doc! {"a": {"$lte": 5, "$gte": 1}}, "$and\n    a $lte 5\n    a $gte 1\n"),
            (doc! {"a.b": {"$elemMatch": {"c": 1}}}, "a.b $elemMatch (obj)\n    c $eq 1\n"),
            (
                doc! {"a": {"$elemMatch": {"b": {"$gt": 1}, "c": 2}}},
                "a $elemMatch (obj)\n    $and\n        c $eq 2\n        b $gt 1\n",
            ),
            (doc! {"a": {"$elemMatch": {}}}, "a $elemMatch (obj)\n    $and\n"),
            (
                doc! {"a": {"$elemMatch": {"$gt": 1, "$lt": 5}}},
                "a $elemMatch (value)\n     $lt 5\n     $gt 1\n",
            ),
            (
                doc! {"a": {"$elemMatch": {"$elemMatch": {"$gt": 1}}}},
                "a $elemMatch (value)\n     $elemMatch (value)\n         $gt 1\n",
            ),
            (doc! {"a": {"$elemMatch": {"$not": {"$gt": 1}}}}, "a $elemMatch (value)\n    $not\n         $gt 1\n"),
            (doc! {"a": {"$all": [1]}}, "a $eq 1\n"),
            (doc! {"a": {"$all": [{"$elemMatch": {"b": 1}}]}}, "a $elemMatch (obj)\n    b $eq 1\n"),
            (doc! {"a": {"$not": {"$size": 1}}}, "$not\n    a $size : 1\n"),
            (
                doc! {"a": {"$exists": true, "$ne": Bson::Null}},
                "$and\n    a exists\n    $not\n        a $eq null\n",
            ),
            (doc! {"$alwaysTrue": 1, "a": 1}, "a $eq 1\n"),
            (
                doc! {"a": {"$geoWithin": {"$box": [[0, 0], [1, 1]]}}},
                "GEO raw = { a: { $geoWithin: { $box: [ [ 0, 0 ], [ 1, 1 ] ] } } }\n",
            ),
        ];
        for (filter, want) in cases {
            assert_eq!(tree(filter.clone()), want, "{filter:?}");
        }
    }

    #[test]
    fn expr_prints_its_expression_and_the_predicates_it_adds() {
        assert_eq!(
            tree(doc! {"$expr": {"$gt": ["$a", "$b"]}}),
            "$expr {$gt: [\"$a\", \"$b\"]}\n"
        );
        assert_eq!(
            tree(doc! {"$expr": {"$gt": ["$a", 2.0]}}),
            "$and\n    $expr {$gt: [\"$a\", {$const: 2}]}\n    a $_internalExprGt 2.0\n"
        );
        assert_eq!(
            tree(doc! {"$expr": {"$eq": ["$a", DateTime::from_millis(1577836800000)]}}),
            "$and\n    $expr {$eq: [\"$a\", {$const: 2020-01-01T00:00:00.000Z}]}\n    \
             a $_internalExprEq new Date(1577836800000)\n"
        );
        assert_eq!(tree(doc! {"$expr": true}), "$and\n");
    }

    #[test]
    fn the_canonical_query_names_its_paging_sort_projection_and_collation() {
        let filter = doc! {"a": 1};
        let sort = doc! {"b": -1, "a": 1};
        let q = CanonicalQuery {
            ns: "ds.c".into(),
            filter: &filter,
            sort: Some(&sort),
            projection: None,
            skip: Some(2),
            limit: Some(3),
            collation: None,
        };
        assert_eq!(
            q.planner_error("x"),
            "error processing query: ns=ds.c limit=3 skip=2Tree: a $eq 1\nSort: { b: -1, a: 1 }\n\
             Proj: {}\n planner returned error :: caused by :: x"
        );
        let simple = doc! {"locale": "simple"};
        let q = CanonicalQuery {
            ns: "ds.c".into(),
            filter: &filter,
            sort: None,
            projection: None,
            skip: None,
            limit: None,
            collation: Some(&simple),
        };
        assert!(q
            .planner_error("x")
            .contains("Proj: {}\nCollation: { locale: \"simple\" }\n planner"));
    }
}
