//! A profile `filter`: checked, and read back, as mongod 8.2.11 does.
//!
//! mongod parses the filter as a match expression and keeps the parsed tree.
//! `profile: -1` reports that tree serialised WITHOUT the optimiser's
//! rewrites: `{op: "query"}` reads back as `{op: {$eq: "query"}}`, several
//! predicates become an `$and` in the order given (not sorted, unlike
//! `explain`'s `parsedQuery`), `$in` of one value stays an `$in`, and every
//! constant inside `$expr` is wrapped in `$const`. A filter may name only the
//! fields a profile entry has (4910200). Every rule here was measured.

use bson::{Bson, Document};
use secantus_core::sortkey::encode_value;

use crate::CommandError;

/// The fields a profile filter may name: mongod 8.2.11 accepts these and
/// refuses every other name with 4910200 (found by offering it every
/// identifier in its own binary).
const FIELDS: &[&str] = &[
    "allUsers",
    "appName",
    "authorization",
    "client",
    "command",
    "cpuNanos",
    "cursorExhausted",
    "cursorid",
    "dataThroughputAverage",
    "dataThroughputLastSecond",
    "docsExamined",
    "durationMillis",
    "errCode",
    "errMsg",
    "errName",
    "estimatedCardinality",
    "estimatedCost",
    "execStats",
    "exhaust",
    "flowControl",
    "fromMultiPlanner",
    "fromPlanCache",
    "hasSortStage",
    "keysDeleted",
    "keysExamined",
    "keysInserted",
    "locks",
    "millis",
    "mongot",
    "nBatches",
    "nMatched",
    "nModified",
    "nShards",
    "nUpserted",
    "ndeleted",
    "ninserted",
    "nreturned",
    "ns",
    "numYield",
    "ok",
    "op",
    "originatingCommand",
    "planCacheKey",
    "planCacheShapeHash",
    "planSummary",
    "planningTimeMicros",
    "prepareReadConflicts",
    "protocol",
    "queryFramework",
    "queryHash",
    "queryShapeHash",
    "remoteOpWaitMillis",
    "replanReason",
    "replanned",
    "responseLength",
    "spillStorage",
    "storage",
    "temporarilyUnavailableErrors",
    "totalOplogSlotDurationMicros",
    "ts",
    "usedDisk",
    "user",
    "workingMillis",
    "writeConcern",
    "writeConflicts",
];

/// A parsed profile filter: the query as given, which decides what is
/// profiled, and the form `profile` reports it in.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileFilter {
    /// What the entries are matched against: the filter as given, with
    /// `$sampleRate` (which the matcher does not have) in the form mongod
    /// itself rewrites it to.
    query: Document,
    serialized: Document,
}

impl ProfileFilter {
    /// Check `filter` as mongod does, or its refusal.
    pub fn parse(filter: &Document) -> Result<Self, CommandError> {
        let mut parser = Parser::default();
        let serialized = parser.and_of(filter)?;
        parser.check_fields()?;
        Ok(ProfileFilter {
            query: sampled(filter),
            serialized,
        })
    }

    /// The filter as `profile` reports it.
    pub fn serialized(&self) -> &Document {
        &self.serialized
    }

    /// Whether the profile entry `entry` passes the filter, or why the
    /// filter could not be evaluated against it.
    pub fn matches(&self, entry: &Document) -> Result<bool, String> {
        secantus_core::query::matches(entry, &self.query, &Document::new(), None)
            .map_err(|e| format!("{e:?}"))
    }
}

fn error(code: i32, msg: impl Into<String>) -> CommandError {
    let name = crate::mongod_codes::code_name(code)
        .map(str::to_string)
        .unwrap_or_else(|| format!("Location{code}"));
    CommandError::new(code, name, msg.into())
}

fn bad_value(msg: impl Into<String>) -> CommandError {
    CommandError::new(2, "BadValue", msg.into())
}

fn one(key: &str, value: Bson) -> Document {
    let mut d = Document::new();
    d.insert(key, value);
    d
}

fn wrap(key: &str, value: Bson) -> Bson {
    Bson::Document(one(key, value))
}

/// An operator document: its first key is an operator.
fn is_operator_doc(d: &Document) -> bool {
    d.keys().next().is_some_and(|k| k.starts_with('$'))
}

#[derive(Default)]
struct Parser {
    /// Every path the filter reads, as written, in order of appearance.
    paths: Vec<String>,
}

impl Parser {
    /// A filter document as one expression: its predicates, ANDed when
    /// there are several.
    fn and_of(&mut self, filter: &Document) -> Result<Document, CommandError> {
        let mut children = Vec::new();
        for (key, value) in filter {
            self.clause(key, value, &mut children)?;
        }
        Ok(match children.len() {
            0 => Document::new(),
            1 => children.remove(0),
            _ => one(
                "$and",
                Bson::Array(children.into_iter().map(Bson::Document).collect()),
            ),
        })
    }

    fn clause(
        &mut self,
        key: &str,
        value: &Bson,
        out: &mut Vec<Document>,
    ) -> Result<(), CommandError> {
        match key {
            "$and" | "$or" | "$nor" => {
                let Bson::Array(items) = value else {
                    return Err(bad_value(format!("{key} argument must be an array")));
                };
                if items.is_empty() {
                    return Err(bad_value(format!(
                        "{key} argument must be a non-empty array"
                    )));
                }
                let mut branches = Vec::with_capacity(items.len());
                for item in items {
                    let Bson::Document(d) = item else {
                        return Err(bad_value(format!(
                            "{key} argument's entries must be objects"
                        )));
                    };
                    branches.push(Bson::Document(self.and_of(d)?));
                }
                out.push(one(key, Bson::Array(branches)));
            }
            "$expr" => {
                // An unknown expression operator, or an undefined `$$var`,
                // with mongod's codes.
                let clause = one(key, value.clone());
                if let Some((code, msg)) =
                    crate::argtypes::expression_problem_in_filter(&clause, &[])
                {
                    return Err(error(code, msg));
                }
                let expr = self.expr(value, &[]);
                out.push(one("$expr", expr));
            }
            "$comment" => {}
            "$alwaysTrue" | "$alwaysFalse" => {
                self.leaf(key, value)?;
                out.push(one(key, Bson::Int32(1)));
            }
            "$sampleRate" => {
                let rate = match value {
                    Bson::Int32(n) => f64::from(*n),
                    Bson::Int64(n) => *n as f64,
                    Bson::Double(d) => *d,
                    _ => return Err(bad_value("argument to $sampleRate must be a numeric type")),
                };
                if !(0.0..=1.0).contains(&rate) {
                    return Err(bad_value(
                        "numeric argument to $sampleRate must be in [0, 1]",
                    ));
                }
                out.push(one("$expr", sample_expr(rate)));
            }
            "$jsonSchema" => {
                self.leaf(key, value)?;
                if let Bson::Document(schema) = value {
                    self.schema_paths(schema);
                    // mongod expands the schema into internal operators that
                    // this server does not have; it is reported as given.
                    if !schema.is_empty() {
                        out.push(one(key, value.clone()));
                    }
                }
            }
            "$where" | "$text" => {
                return Err(bad_value(format!("{key} is not allowed in this context")));
            }
            _ if key.starts_with('$') => {
                return Err(bad_value(format!(
                    "unknown top level operator: {key}. If you have a field name that starts \
                     with a '$' symbol, consider using $getField or $setField."
                )));
            }
            path => {
                self.paths.push(path.to_string());
                self.field(path, value, out)?;
            }
        }
        Ok(())
    }

    /// The predicates one `path: value` entry parses to, appended to `out`.
    fn field(
        &mut self,
        path: &str,
        value: &Bson,
        out: &mut Vec<Document>,
    ) -> Result<(), CommandError> {
        match value {
            Bson::RegularExpression(re) => {
                out.push(one(path, regex(&re.pattern, &re.options)));
                return Ok(());
            }
            Bson::Document(ops) if is_operator_doc(ops) => {}
            other => {
                out.push(one(path, wrap("$eq", other.clone())));
                return Ok(());
            }
        }
        let Bson::Document(ops) = value else {
            return Ok(());
        };
        if let (Some(Bson::RegularExpression(re)), Some(_)) =
            (ops.get("$regex"), ops.get("$options"))
        {
            if !re.options.is_empty() {
                return Err(error(51075, "options set in both $regex and $options"));
            }
        }
        for (op, arg) in ops {
            match op.as_str() {
                "$near" | "$nearSphere" | "$geoNear" => return Err(near_refused()),
                "$where" => return Err(bad_value("$where cannot be applied to a field")),
                known if FIELD_OPERATORS.contains(&known) => {}
                other => return Err(bad_value(format!("unknown operator: {other}"))),
            }
            // The operator's own argument checks, with mongod's messages.
            if op != "$options" {
                self.leaf(path, &wrap(op, arg.clone()))?;
            }
            let predicate = match op.as_str() {
                "$eq" | "$gt" | "$gte" | "$lt" | "$lte" | "$geoWithin" | "$geoIntersects" => {
                    wrap(op, arg.clone())
                }
                "$ne" => wrap("$not", wrap("$eq", arg.clone())),
                "$in" => wrap("$in", in_list(arg)?),
                "$nin" => wrap("$not", wrap("$in", in_list(arg)?)),
                "$exists" => {
                    if exists_arg(arg) {
                        wrap("$exists", Bson::Boolean(true))
                    } else {
                        wrap("$not", wrap("$exists", Bson::Boolean(true)))
                    }
                }
                "$type" => wrap("$type", type_codes(arg)),
                "$size" => wrap("$size", integral(arg, false)),
                "$mod" => match arg {
                    Bson::Array(items) => wrap(
                        "$mod",
                        Bson::Array(items.iter().map(|n| integral(n, true)).collect()),
                    ),
                    other => wrap("$mod", other.clone()),
                },
                "$bitsAllSet" | "$bitsAllClear" | "$bitsAnySet" | "$bitsAnyClear" => {
                    wrap(op, bit_positions(arg))
                }
                "$regex" => match arg {
                    Bson::RegularExpression(re) => regex(&re.pattern, &re.options),
                    Bson::String(pattern) => {
                        let options = ops.get_str("$options").unwrap_or("");
                        regex(pattern, options)
                    }
                    other => wrap("$regex", other.clone()),
                },
                "$options" => continue,
                "$all" => {
                    let Bson::Array(items) = arg else {
                        out.push(one(path, wrap("$all", arg.clone())));
                        continue;
                    };
                    if items.is_empty() {
                        out.push(one("$alwaysFalse", Bson::Int32(1)));
                        continue;
                    }
                    let mut members = Vec::with_capacity(items.len());
                    for item in items {
                        let member = match item {
                            Bson::Document(d) if d.len() == 1 && d.get("$elemMatch").is_some() => {
                                let inner = d.get("$elemMatch").cloned().unwrap_or(Bson::Null);
                                wrap("$elemMatch", self.elem_match(&inner)?)
                            }
                            other => wrap("$eq", other.clone()),
                        };
                        members.push(Bson::Document(one(path, member)));
                    }
                    out.push(one("$and", Bson::Array(members)));
                    continue;
                }
                "$elemMatch" => wrap("$elemMatch", self.elem_match(arg)?),
                "$not" => match arg {
                    Bson::RegularExpression(re) => wrap("$not", regex(&re.pattern, &re.options)),
                    Bson::Document(inner) => {
                        let mut negated = Vec::new();
                        self.field(path, &Bson::Document(inner.clone()), &mut negated)?;
                        if negated.len() == 1 {
                            let only = negated.remove(0);
                            match only.get(path) {
                                Some(pred) => wrap("$not", pred.clone()),
                                None => {
                                    out.push(one("$nor", Bson::Array(vec![Bson::Document(only)])));
                                    continue;
                                }
                            }
                        } else {
                            let all = one(
                                "$and",
                                Bson::Array(negated.into_iter().map(Bson::Document).collect()),
                            );
                            out.push(one("$nor", Bson::Array(vec![Bson::Document(all)])));
                            continue;
                        }
                    }
                    _ => return Err(bad_value("$not argument must be a regex or an object")),
                },
                other => return Err(bad_value(format!("unknown operator: {other}"))),
            };
            out.push(one(path, predicate));
        }
        Ok(())
    }

    /// `$elemMatch`'s argument: a sub-filter, or (with operator keys) the
    /// element's own predicates, which mongod leaves as given.
    fn elem_match(&mut self, arg: &Bson) -> Result<Bson, CommandError> {
        match arg {
            Bson::Document(d)
                if is_operator_doc(d)
                    && !matches!(
                        d.keys().next().map(String::as_str),
                        Some("$and" | "$or" | "$nor" | "$expr")
                    ) =>
            {
                Ok(arg.clone())
            }
            Bson::Document(d) => {
                // Paths inside are relative to the element: they name no
                // field of the profile entry.
                let saved = self.paths.len();
                let inner = self.and_of(d)?;
                self.paths.truncate(saved);
                Ok(Bson::Document(inner))
            }
            other => Ok(other.clone()),
        }
    }

    /// The matcher's own refusal of one clause, found without a document.
    fn leaf(&self, key: &str, value: &Bson) -> Result<(), CommandError> {
        let clause = one(key, value.clone());
        match secantus_core::query::matches(&Document::new(), &clause, &Document::new(), None) {
            Ok(_) => Ok(()),
            Err(refusal) => match refusal.as_mongo() {
                Some((code, msg)) => Err(error(code, msg)),
                None => Err(crate::find::query_filter_error(&clause)),
            },
        }
    }

    /// An aggregation expression as mongod serialises it, unoptimised.
    fn expr(&mut self, value: &Bson, bound: &[String]) -> Bson {
        match value {
            Bson::String(s) if s.starts_with("$$") => {
                let rest = &s[2..];
                let (var, path) = rest.split_once('.').unwrap_or((rest, ""));
                if matches!(var, "ROOT" | "CURRENT")
                    && !path.is_empty()
                    && !bound.iter().any(|b| b == var)
                {
                    self.paths.push(path.to_string());
                }
                value.clone()
            }
            Bson::String(s) if s.starts_with('$') => {
                self.paths.push(s[1..].to_string());
                value.clone()
            }
            Bson::Array(items) => Bson::Array(items.iter().map(|v| self.expr(v, bound)).collect()),
            Bson::Document(d) => match d.iter().next() {
                Some((op, arg)) if d.len() == 1 && op == "$literal" => wrap("$const", arg.clone()),
                Some((op, arg)) if d.len() == 1 && op.starts_with('$') => {
                    Bson::Document(one(op, self.operator(op, arg, bound)))
                }
                _ => Bson::Document(
                    d.iter()
                        .map(|(k, v)| (k.clone(), self.expr(v, bound)))
                        .collect(),
                ),
            },
            other => wrap("$const", other.clone()),
        }
    }

    /// One expression operator's argument.
    fn operator(&mut self, op: &str, arg: &Bson, bound: &[String]) -> Bson {
        match (op, arg) {
            ("$cond", Bson::Document(d)) => {
                let part = |k: &str| d.get(k).cloned().unwrap_or(Bson::Null);
                let parts = [part("if"), part("then"), part("else")];
                return Bson::Array(parts.iter().map(|v| self.expr(v, bound)).collect());
            }
            ("$getField", Bson::String(_)) => {
                let mut d = Document::new();
                d.insert("field", wrap("$const", arg.clone()));
                d.insert("input", "$$CURRENT");
                return Bson::Document(d);
            }
            ("$let", Bson::Document(d)) => {
                let mut inner = bound.to_vec();
                let mut out = Document::new();
                if let Ok(vars) = d.get_document("vars") {
                    let vars: Document = vars
                        .iter()
                        .map(|(k, v)| (k.clone(), self.expr(v, bound)))
                        .collect();
                    inner.extend(vars.keys().cloned());
                    out.insert("vars", vars);
                }
                if let Some(body) = d.get("in") {
                    out.insert("in", self.expr(body, &inner));
                }
                return Bson::Document(out);
            }
            _ => {}
        }
        match arg {
            // An operator of named arguments keeps its object.
            Bson::Document(d) if !is_operator_doc(d) => Bson::Document(
                d.iter()
                    .map(|(k, v)| (k.clone(), self.expr(v, bound)))
                    .collect(),
            ),
            Bson::Array(items) => Bson::Array(items.iter().map(|v| self.expr(v, bound)).collect()),
            // A single argument is a one-element list.
            other => Bson::Array(vec![self.expr(other, bound)]),
        }
    }

    /// The fields a `$jsonSchema` names at its top level.
    fn schema_paths(&mut self, schema: &Document) {
        if let Ok(props) = schema.get_document("properties") {
            self.paths.extend(props.keys().cloned());
        }
        if let Ok(required) = schema.get_array("required") {
            self.paths
                .extend(required.iter().filter_map(Bson::as_str).map(str::to_string));
        }
    }

    /// Every path must be a field path, and its first part a field of a
    /// profile entry.
    fn check_fields(&self) -> Result<(), CommandError> {
        let mut unknown: Vec<&str> = Vec::new();
        for path in &self.paths {
            for part in path.split('.') {
                if part.is_empty() {
                    return Err(error(
                        15998,
                        "Consider using $getField or $setField for a field path with '.' or \
                         '$'. :: caused by :: FieldPath field names may not be empty strings.",
                    ));
                }
                if part.starts_with('$') {
                    return Err(error(
                        16410,
                        format!(
                            "Consider using $getField or $setField for a field path with '.' \
                             or '$'. :: caused by :: FieldPath field names may not start with \
                             '$', given '{part}'."
                        ),
                    ));
                }
            }
            let head = path.split('.').next().unwrap_or("");
            if !FIELDS.contains(&head) && !unknown.contains(&head) {
                unknown.push(head);
            }
        }
        if unknown.is_empty() {
            return Ok(());
        }
        Err(error(
            4910200,
            format!(
                "No such field (or fields) available for profile filter: {}",
                unknown.join(", ")
            ),
        ))
    }
}

/// The field operators a profile filter may use.
const FIELD_OPERATORS: &[&str] = &[
    "$eq",
    "$gt",
    "$gte",
    "$lt",
    "$lte",
    "$ne",
    "$in",
    "$nin",
    "$exists",
    "$type",
    "$size",
    "$mod",
    "$regex",
    "$options",
    "$all",
    "$elemMatch",
    "$not",
    "$bitsAllSet",
    "$bitsAllClear",
    "$bitsAnySet",
    "$bitsAnyClear",
    "$geoWithin",
    "$geoIntersects",
];

fn near_refused() -> CommandError {
    error(
        5626500,
        "$geoNear, $near, and $nearSphere are not allowed in this context, as these operators \
         require sorting geospatial data. If you do not need sort, consider using $geoWithin \
         instead. Check out https://dochub.mongodb.org/core/near-sort-operation and \
         https://dochub.mongodb.org/core/nearSphere-sort-operationfor more details.",
    )
}

/// `$sampleRate: rate` as mongod parses it: `$rand` below the rate.
fn sample_expr(rate: f64) -> Bson {
    wrap(
        "$lt",
        Bson::Array(vec![
            wrap("$rand", Bson::Document(Document::new())),
            wrap("$const", Bson::Double(rate)),
        ]),
    )
}

/// `filter` with every `$sampleRate` in its `$expr` form, which the matcher
/// evaluates.
fn sampled(filter: &Document) -> Document {
    let mut clauses = Vec::new();
    let mut rewritten = false;
    for (key, value) in filter {
        let value = match (key.as_str(), value) {
            ("$sampleRate", rate) => {
                rewritten = true;
                let rate = match rate {
                    Bson::Int32(n) => f64::from(*n),
                    Bson::Int64(n) => *n as f64,
                    Bson::Double(d) => *d,
                    _ => 0.0,
                };
                clauses.push(one("$expr", sample_expr(rate)));
                continue;
            }
            ("$and" | "$or" | "$nor", Bson::Array(items)) => Bson::Array(
                items
                    .iter()
                    .map(|item| match item {
                        Bson::Document(d) => Bson::Document(sampled(d)),
                        other => other.clone(),
                    })
                    .collect(),
            ),
            (_, other) => other.clone(),
        };
        clauses.push(one(key, value));
    }
    if !rewritten {
        return clauses.into_iter().flatten().collect();
    }
    one(
        "$and",
        Bson::Array(clauses.into_iter().map(Bson::Document).collect()),
    )
}

/// How mongod reads `$exists`' argument: false, 0 and null are false,
/// anything else true.
fn exists_arg(arg: &Bson) -> bool {
    match arg {
        Bson::Boolean(b) => *b,
        Bson::Int32(n) => *n != 0,
        Bson::Int64(n) => *n != 0,
        Bson::Double(d) => *d != 0.0,
        Bson::Decimal128(d) => d.to_string().parse::<f64>() != Ok(0.0),
        Bson::Null | Bson::Undefined => false,
        _ => true,
    }
}

fn regex(pattern: &str, options: &str) -> Bson {
    let mut d = one("$regex", Bson::String(pattern.to_string()));
    if !options.is_empty() {
        d.insert("$options", options);
    }
    Bson::Document(d)
}

/// `$in`'s list: each value once, in BSON order.
fn in_list(arg: &Bson) -> Result<Bson, CommandError> {
    let Bson::Array(items) = arg else {
        return Err(bad_value("$in needs an array"));
    };
    let mut keyed: Vec<(Vec<u8>, Bson)> = items
        .iter()
        .map(|v| (encode_value(v, None).unwrap_or_default(), v.clone()))
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    keyed.dedup_by(|a, b| a.0 == b.0);
    Ok(Bson::Array(keyed.into_iter().map(|(_, v)| v).collect()))
}

/// A number mongod keeps as an integer: `$size` (32-bit) and `$mod`'s
/// divisor and remainder (64-bit), truncated.
fn integral(v: &Bson, wide: bool) -> Bson {
    let n = match v {
        Bson::Int32(n) => i64::from(*n),
        Bson::Int64(n) => *n,
        Bson::Double(d) if d.is_finite() => d.trunc() as i64,
        other => return other.clone(),
    };
    if wide {
        Bson::Int64(n)
    } else {
        i32::try_from(n).map_or(Bson::Int64(n), Bson::Int32)
    }
}

/// `$type`'s argument: a list of BSON type codes, ascending; `"number"`
/// stays a name.
fn type_codes(spec: &Bson) -> Bson {
    let items = match spec {
        Bson::Array(a) => a.clone(),
        other => vec![other.clone()],
    };
    let mut names = Vec::new();
    let mut codes = Vec::new();
    for item in items {
        match item {
            Bson::String(s) => match type_alias_code(&s) {
                Some(code) => codes.push(code),
                None => names.push(Bson::String(s)),
            },
            Bson::Int32(n) => codes.push(n),
            Bson::Int64(n) => codes.push(n as i32),
            Bson::Double(d) => codes.push(d as i32),
            other => names.push(other),
        }
    }
    codes.sort_unstable();
    codes.dedup();
    names.extend(codes.into_iter().map(Bson::Int32));
    Bson::Array(names)
}

fn type_alias_code(alias: &str) -> Option<i32> {
    Some(match alias {
        "double" => 1,
        "string" => 2,
        "object" => 3,
        "array" => 4,
        "binData" => 5,
        "undefined" => 6,
        "objectId" => 7,
        "bool" => 8,
        "date" => 9,
        "null" => 10,
        "regex" => 11,
        "dbPointer" => 12,
        "javascript" => 13,
        "symbol" => 14,
        "javascriptWithScope" => 15,
        "int" => 16,
        "timestamp" => 17,
        "long" => 18,
        "decimal" => 19,
        "minKey" => -1,
        "maxKey" => 127,
        _ => return None,
    })
}

/// A bitwise operator's argument: the positions of its set bits. A list
/// stays as given.
fn bit_positions(arg: &Bson) -> Bson {
    let n = match arg {
        Bson::Int32(v) => i64::from(*v),
        Bson::Int64(v) => *v,
        Bson::Double(d) if d.fract() == 0.0 => *d as i64,
        _ => return arg.clone(),
    };
    Bson::Array(
        (0..64)
            .filter(|i| n >> i & 1 == 1)
            .map(Bson::Int32)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn read_back(filter: Document) -> Result<Document, (i32, String)> {
        ProfileFilter::parse(&filter)
            .map(|f| f.serialized().clone())
            .map_err(|e| (e.code, e.errmsg))
    }

    /// Each pair was read back from mongod 8.2.11 (2026-10-15).
    #[test]
    fn read_back_is_mongods() {
        let cases = [
            (doc! {"op": "query"}, doc! {"op": {"$eq": "query"}}),
            (
                doc! {"millis": {"$gt": 5}, "ns": "a.b"},
                doc! {"$and": [{"millis": {"$gt": 5}}, {"ns": {"$eq": "a.b"}}]},
            ),
            (
                doc! {"op": {"$in": ["query"]}},
                doc! {"op": {"$in": ["query"]}},
            ),
            (
                doc! {"op": {"$in": ["b", "a", "b"]}},
                doc! {"op": {"$in": ["a", "b"]}},
            ),
            (doc! {}, doc! {}),
            (
                doc! {"op": {"$ne": "q"}},
                doc! {"op": {"$not": {"$eq": "q"}}},
            ),
            (
                doc! {"op": {"$nin": ["a", "b"]}},
                doc! {"op": {"$not": {"$in": ["a", "b"]}}},
            ),
            (
                doc! {"op": {"$exists": false}},
                doc! {"op": {"$not": {"$exists": true}}},
            ),
            (
                doc! {"op": {"$exists": "x"}},
                doc! {"op": {"$exists": true}},
            ),
            (
                doc! {"op": {"$options": "i", "$regex": "q"}},
                doc! {"op": {"$regex": "q", "$options": "i"}},
            ),
            (
                doc! {"millis": {"$type": ["int", "double"]}},
                doc! {"millis": {"$type": [1, 16]}},
            ),
            (
                doc! {"millis": {"$type": "number"}},
                doc! {"millis": {"$type": ["number"]}},
            ),
            (
                doc! {"millis": {"$bitsAllSet": 3}},
                doc! {"millis": {"$bitsAllSet": [0, 1]}},
            ),
            (
                doc! {"allUsers": {"$all": ["a", "b"]}},
                doc! {"$and": [{"allUsers": {"$eq": "a"}}, {"allUsers": {"$eq": "b"}}]},
            ),
            (doc! {"op": {"$all": []}}, doc! {"$alwaysFalse": 1}),
            (
                doc! {"op": {"$not": {"$gt": 1, "$lt": 3}}},
                doc! {"$nor": [{"$and": [{"op": {"$gt": 1}}, {"op": {"$lt": 3}}]}]},
            ),
            (
                doc! {"$and": [{"op": "query"}, {"millis": {"$gt": 1}}], "ns": "x"},
                doc! {"$and": [
                    {"$and": [{"op": {"$eq": "query"}}, {"millis": {"$gt": 1}}]},
                    {"ns": {"$eq": "x"}},
                ]},
            ),
            (
                doc! {"$or": [{"op": 1, "ns": 2}]},
                doc! {"$or": [{"$and": [{"op": {"$eq": 1}}, {"ns": {"$eq": 2}}]}]},
            ),
            (
                doc! {"$comment": "hi", "op": "query"},
                doc! {"op": {"$eq": "query"}},
            ),
            (
                doc! {"command": {"find": "t"}},
                doc! {"command": {"$eq": {"find": "t"}}},
            ),
            (
                doc! {"$expr": {"$eq": ["$op", "query"]}},
                doc! {"$expr": {"$eq": ["$op", {"$const": "query"}]}},
            ),
            (
                doc! {"$expr": {"$add": [1, {"$multiply": [2, "$millis"]}]}},
                doc! {"$expr": {"$add": [{"$const": 1}, {"$multiply": [{"$const": 2}, "$millis"]}]}},
            ),
            (
                doc! {"$expr": {"$size": "$allUsers"}},
                doc! {"$expr": {"$size": ["$allUsers"]}},
            ),
            (
                doc! {"$expr": {"$cond": {"if": "$ok", "then": 1, "else": 0}}},
                doc! {"$expr": {"$cond": ["$ok", {"$const": 1}, {"$const": 0}]}},
            ),
            (
                doc! {"$expr": {"$let": {"vars": {"x": 1}, "in": "$$x"}}},
                doc! {"$expr": {"$let": {"vars": {"x": {"$const": 1}}, "in": "$$x"}}},
            ),
            (
                doc! {"$expr": {"$getField": "op"}},
                doc! {"$expr": {"$getField": {"field": {"$const": "op"}, "input": "$$CURRENT"}}},
            ),
            (
                doc! {"$expr": {"$literal": 5}},
                doc! {"$expr": {"$const": 5}},
            ),
            (doc! {"$expr": 1}, doc! {"$expr": {"$const": 1}}),
            (
                doc! {"$sampleRate": 0.5},
                doc! {"$expr": {"$lt": [{"$rand": {}}, {"$const": 0.5}]}},
            ),
            (doc! {"$alwaysTrue": 1}, doc! {"$alwaysTrue": 1}),
        ];
        for (given, want) in cases {
            assert_eq!(read_back(given.clone()), Ok(want), "{given}");
        }
    }

    #[test]
    fn refusals_are_mongods() {
        let unknown_top = "unknown top level operator: $foo. If you have a field name that \
                           starts with a '$' symbol, consider using $getField or $setField.";
        let cases = [
            (doc! {"$foo": 1}, 2, unknown_top.to_string()),
            (doc! {"zz": 1, "$foo": 1}, 2, unknown_top.to_string()),
            (
                doc! {"op": {"$where": "1"}},
                2,
                "$where cannot be applied to a field".into(),
            ),
            (
                doc! {"$where": "1"},
                2,
                "$where is not allowed in this context".into(),
            ),
            (
                doc! {"$text": {"$search": "x"}},
                2,
                "$text is not allowed in this context".into(),
            ),
            (
                doc! {"$and": []},
                2,
                "$and argument must be a non-empty array".into(),
            ),
            (
                doc! {"$and": [1]},
                2,
                "$and argument's entries must be objects".into(),
            ),
            (doc! {"op": {"$in": 1}}, 2, "$in needs an array".into()),
            (
                doc! {"op": {"$not": 1}},
                2,
                "$not argument must be a regex or an object".into(),
            ),
            (
                doc! {"a": 1},
                4910200,
                "No such field (or fields) available for profile filter: a".into(),
            ),
            (
                doc! {"$expr": {"$gt": ["$zz", 5]}},
                4910200,
                "No such field (or fields) available for profile filter: zz".into(),
            ),
            (
                doc! {"$expr": {"$eq": ["$$ROOT.zz", 1]}},
                4910200,
                "No such field (or fields) available for profile filter: zz".into(),
            ),
            (
                doc! {"$expr": {"$foo": 1}},
                168,
                "Unrecognized expression '$foo'".into(),
            ),
            (
                doc! {"$sampleRate": 2},
                2,
                "numeric argument to $sampleRate must be in [0, 1]".into(),
            ),
        ];
        for (given, code, msg) in cases {
            assert_eq!(read_back(given.clone()), Err((code, msg)), "{given}");
        }
        assert_eq!(
            read_back(doc! {"a": {"$near": [0, 0]}}).unwrap_err().0,
            5626500
        );
        assert_eq!(read_back(doc! {"op.$": 1}).unwrap_err().0, 16410);
    }

    #[test]
    fn the_query_as_given_decides() {
        let all = ProfileFilter::parse(&doc! {"$sampleRate": 1.0, "op": "query"}).unwrap();
        assert_eq!(all.matches(&doc! {"op": "query"}), Ok(true));
        let none = ProfileFilter::parse(&doc! {"$or": [{"$sampleRate": 0}]}).unwrap();
        assert_eq!(none.matches(&doc! {"op": "query"}), Ok(false));
        let f = ProfileFilter::parse(&doc! {"op": "query", "millis": {"$gte": 3}}).unwrap();
        assert_eq!(f.matches(&doc! {"op": "query", "millis": 5}), Ok(true));
        assert_eq!(f.matches(&doc! {"op": "insert", "millis": 5}), Ok(false));
        assert_eq!(f.matches(&doc! {"op": "query", "millis": 1}), Ok(false));
    }
}
