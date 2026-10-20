//! Time-series collections: what `mongod` accepts as a definition and what it
//! then refuses to do to one.
//!
//! `mongod` stores a time-series collection as buckets in
//! `system.buckets.<name>` and presents the collection as a view over them.
//! This server stores the measurements as ordinary documents and derives the
//! buckets when they are asked for (see `ts_buckets`). This module is the
//! behaviour a client meets when it defines and writes one: the option checks, the stored and listed definition, the rule that every
//! measurement carries a date in the time field, the meta-only update rule,
//! and the index and `collMod` restrictions. Every code and message here was
//! measured against `mongod` 8.2.11 on 2026-10-11
//! (`tools/probes/time_series.py`).

use bson::{doc, Bson, Document};

use crate::CommandError;

/// A time-series collection's two field names.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Fields {
    pub time: String,
    /// `None` when the collection was created without a `metaField`.
    pub meta: Option<String>,
}

impl Fields {
    /// From a collection's stored options; `None` for any other collection.
    pub(crate) fn of(options: &Document) -> Option<Fields> {
        let ts = options.get_document("timeseries").ok()?;
        Some(Fields {
            time: ts.get_str("timeField").ok()?.to_string(),
            meta: ts.get_str("metaField").ok().map(str::to_string),
        })
    }

    /// The default index a collection with a meta field is created with:
    /// `(name, key)`.
    pub(crate) fn default_index(&self) -> Option<(String, Document)> {
        let meta = self.meta.as_ref()?;
        let mut key = Document::new();
        key.insert(meta.clone(), 1i32);
        key.insert(self.time.clone(), 1i32);
        Some((format!("{meta}_1_{}_1", self.time), key))
    }
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(72, "InvalidOptions", message)
}

fn type_name(v: &Bson) -> &'static str {
    secantus_core::query::bson_type_name(v)
}

/// The bucket span `mongod` gives each granularity.
fn default_span(granularity: &str) -> i32 {
    match granularity {
        "minutes" => 86400,
        "hours" => 2592000,
        _ => 3600,
    }
}

const GRANULARITIES: &[&str] = &["seconds", "minutes", "hours"];

/// A whole number of seconds from a numeric option, or its type error.
fn seconds(path: &str, field: &str, v: &Bson) -> Result<i64, CommandError> {
    let n = match v {
        Bson::Int32(n) => i64::from(*n),
        Bson::Int64(n) => *n,
        Bson::Double(d) => *d as i64,
        Bson::Decimal128(d) => d.to_string().parse::<f64>().unwrap_or(0.0) as i64,
        _ => {
            return Err(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field '{path}.{field}' is the wrong type '{}', expected types \
                     '[decimal, double, int, long]'",
                    type_name(v)
                ),
            ))
        }
    };
    for (bad, bound, sign) in [(n < 1, 1i64, ">="), (n > 31536000, 31536000, "<=")] {
        if bad {
            return Err(CommandError::new(
                2,
                "BadValue",
                format!("BSON field '{field}' value must be {sign} {bound}, actual value '{n}'"),
            ));
        }
    }
    Ok(n)
}

fn string_field(path: &str, field: &str, v: &Bson) -> Result<Option<String>, CommandError> {
    match v {
        Bson::Null => Ok(None),
        Bson::String(s) => Ok(Some(s.clone())),
        _ => Err(CommandError::new(
            14,
            "TypeMismatch",
            format!(
                "BSON field '{path}.{field}' is the wrong type '{}', expected type 'string'",
                type_name(v)
            ),
        )),
    }
}

fn granularity(path: &str, v: &Bson) -> Result<Option<String>, CommandError> {
    let Some(g) = string_field(path, "granularity", v)? else {
        return Ok(None);
    };
    if !GRANULARITIES.contains(&g.as_str()) {
        return Err(CommandError::new(
            2,
            "BadValue",
            format!("Enumeration value '{g}' for field '{path}.granularity' is not a valid value."),
        ));
    }
    Ok(Some(g))
}

fn unknown(path: &str, field: &str) -> CommandError {
    CommandError::new(
        40415,
        "IDLUnknownField",
        format!("BSON field '{path}.{field}' is an unknown field."),
    )
}

/// The definition as `mongod` stores and lists it: the two field names, then
/// either the granularity with its span or an explicit rounding and span.
fn stored(time: &str, meta: Option<&str>, gran: Option<&str>, span: Option<i64>) -> Document {
    let mut out = doc! { "timeField": time };
    if let Some(meta) = meta {
        out.insert("metaField", meta);
    }
    match span {
        Some(span) if gran.is_none() => {
            out.insert("bucketRoundingSeconds", span as i32);
            out.insert("bucketMaxSpanSeconds", span as i32);
        }
        _ => {
            let g = gran.unwrap_or("seconds");
            out.insert("granularity", g);
            out.insert("bucketMaxSpanSeconds", default_span(g));
        }
    }
    out
}

/// Validate `create`'s `timeseries` document and the options beside it.
/// `Ok(None)` when the command defines no time-series collection.
pub(crate) fn definition(command: &Document, ns: &str) -> Result<Option<Document>, CommandError> {
    let Some(Bson::Document(spec)) = command.get("timeseries") else {
        return Ok(None);
    };
    const PATH: &str = "create.timeseries";
    let (mut time, mut meta, mut gran) = (None, None, None);
    let (mut span, mut rounding) = (None, None);
    for (k, v) in spec {
        match k.as_str() {
            "timeField" => time = string_field(PATH, k, v)?,
            "metaField" => meta = string_field(PATH, k, v)?,
            "granularity" => gran = granularity(PATH, v)?,
            "bucketMaxSpanSeconds" if !matches!(v, Bson::Null) => span = Some(seconds(PATH, k, v)?),
            "bucketRoundingSeconds" if !matches!(v, Bson::Null) => {
                rounding = Some(seconds(PATH, k, v)?)
            }
            "bucketMaxSpanSeconds" | "bucketRoundingSeconds" => {}
            _ => return Err(unknown(PATH, k)),
        }
    }
    let Some(time) = time else {
        return Err(CommandError::new(
            40414,
            "IDLFailedToParse",
            format!("BSON field '{PATH}.timeField' is missing but a required field"),
        ));
    };
    for other in [
        "capped",
        "validator",
        "viewOn",
        "clusteredIndex",
        "changeStreamPreAndPostImages",
    ] {
        let given = match command.get(other) {
            None | Some(Bson::Null) | Some(Bson::Boolean(false)) => false,
            Some(Bson::Document(d)) if other == "validator" => !d.is_empty(),
            Some(_) => true,
        };
        if given {
            return Err(invalid(format!(
                "{ns}: 'timeseries' is not allowed with '{other}'"
            )));
        }
    }
    if time.contains('.') {
        return Err(invalid(format!(
            "{ns}: 'timeField' must be a top-level field and not contain a '.'"
        )));
    }
    if let Some(meta) = &meta {
        if meta == "_id" {
            return Err(invalid("'metaField' cannot be \"_id\""));
        }
        if meta == &time {
            return Err(invalid("'metaField' cannot be the same as 'timeField'"));
        }
        if meta.contains('.') {
            return Err(invalid(format!(
                "{ns}: 'metaField' must be a top-level field and not contain a '.'"
            )));
        }
    }
    if span != rounding {
        return Err(invalid(
            "Time-series 'bucketRoundingSeconds' must be equal to 'bucketMaxSpanSeconds'",
        ));
    }
    if let (Some(span), Some(g)) = (span, &gran) {
        let default = default_span(g);
        if span != i64::from(default) {
            return Err(invalid(format!(
                "Time-series 'bucketMaxSpanSeconds' cannot be set to a value other than the \
                 default of {default} for the provided granularity"
            )));
        }
    }
    Ok(Some(stored(&time, meta.as_deref(), gran.as_deref(), span)))
}

/// A collection-level `expireAfterSeconds` as `mongod` stores it (a long), or
/// its refusal. `path` is `create` or `collMod`.
pub(crate) fn expire_after(path: &str, v: &Bson) -> Result<i64, CommandError> {
    let n = match v {
        Bson::Int32(n) => i64::from(*n),
        Bson::Int64(n) => *n,
        Bson::Double(d) => *d as i64,
        Bson::Decimal128(d) => d.to_string().parse::<f64>().unwrap_or(0.0) as i64,
        _ => {
            let expected = if path == "collMod" {
                "[string, long]"
            } else {
                "[decimal, double, int, long]"
            };
            return Err(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field '{path}.expireAfterSeconds' is the wrong type '{}', expected \
                     types '{expected}'",
                    type_name(v)
                ),
            ));
        }
    };
    if n < 0 {
        return Err(invalid(
            "TTL index 'expireAfterSeconds' option cannot be less than 0",
        ));
    }
    Ok(n)
}

/// Why a document cannot be inserted as a measurement: its time field must
/// hold a date.
pub(crate) fn measurement_problem(doc: &Document, fields: &Fields) -> Option<String> {
    match doc.get(&fields.time) {
        Some(Bson::DateTime(_)) => None,
        _ => Some(format!(
            "'{}' must be present and contain a valid BSON UTC datetime value",
            fields.time
        )),
    }
}

/// Whether `path` is the meta field or a path inside it.
fn in_meta(path: &str, meta: &str) -> bool {
    path == meta
        || path
            .strip_prefix(meta)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Whether a query names only the meta field. `Err` is the `$expr` refusal.
fn query_is_meta_only(query: &Document, meta: &str) -> Result<bool, &'static str> {
    for (k, v) in query {
        match k.as_str() {
            "$expr" => {
                return Err(
                    "Cannot use $expr when performing an update or delete on a time-series \
                     collection",
                )
            }
            "$and" | "$or" | "$nor" => {
                for part in v.as_array().into_iter().flatten() {
                    if let Bson::Document(d) = part {
                        if !query_is_meta_only(d, meta)? {
                            return Ok(false);
                        }
                    }
                }
            }
            "$comment" => {}
            _ if !in_meta(k, meta) => return Ok(false),
            _ => {}
        }
    }
    Ok(true)
}

/// Why `mongod` refuses one `update` statement on a time-series collection,
/// in the order it checks.
pub(crate) fn update_problem(statement: &Document, fields: &Fields) -> Option<String> {
    let flag = |k: &str| matches!(statement.get(k), Some(Bson::Boolean(true)));
    if !flag("multi") {
        return Some("Cannot perform a non-multi update on a time-series collection".into());
    }
    if flag("upsert") {
        return Some("Cannot perform an upsert on a time-series collection".into());
    }
    let Some(meta) = &fields.meta else {
        return Some(
            "Cannot perform an update on a time-series collection that does not have a metaField"
                .into(),
        );
    };
    let update = match statement.get("u") {
        Some(Bson::Array(_)) => {
            return Some(
                "Cannot perform an update on a time-series collection using a pipeline update"
                    .into(),
            )
        }
        Some(Bson::Document(u)) => u,
        _ => return None,
    };
    if let Some(Bson::Document(q)) = statement.get("q") {
        match query_is_meta_only(q, meta) {
            Err(message) => return Some(message.into()),
            Ok(false) => {
                return Some(format!(
                    "Cannot perform an update or delete on a time-series collection when \
                     querying on a field that is not the metaField '{meta}'"
                ))
            }
            Ok(true) => {}
        }
    }
    if !update.keys().all(|k| k.starts_with('$')) || update.is_empty() {
        return Some(
            "Cannot perform an update on a time-series collection using a replacement document"
                .into(),
        );
    }
    let not_meta = format!(
        "Cannot perform an update on a time-series collection which updates a field that is not \
         the metaField '{meta}'"
    );
    for (op, targets) in update {
        let Bson::Document(targets) = targets else {
            continue;
        };
        for (path, value) in targets {
            if !in_meta(path, meta) {
                return Some(not_meta);
            }
            // `$rename` moves a value: its destination must be in the meta
            // field as well.
            if op == "$rename" && !matches!(value, Bson::String(to) if in_meta(to, meta)) {
                return Some(not_meta);
            }
        }
    }
    None
}

/// Why `mongod` refuses a `findAndModify` on a time-series collection.
pub(crate) fn find_and_modify_problem(command: &Document) -> Option<String> {
    let removing =
        matches!(command.get("remove"), Some(v) if secantus_core::expressions::truthy(v));
    if !removing {
        return Some("Cannot perform a non-multi update on a time-series collection".into());
    }
    let sorted = matches!(command.get("sort"), Some(Bson::Document(s)) if !s.is_empty());
    sorted.then(|| {
        "Cannot perform a findAndModify with a query and sort on a time-series collection."
            .to_string()
    })
}

/// Why an index cannot be built on a time-series collection.
pub(crate) fn index_problem(spec: &Document, fields: &Fields) -> Option<CommandError> {
    let truthy = |k: &str| spec.get(k).is_some_and(secantus_core::expressions::truthy);
    if truthy("unique") {
        return Some(invalid(
            "Unique indexes are not supported on time-series collections",
        ));
    }
    let key = spec.get_document("key").ok()?;
    // A wildcard key becomes two on the buckets (the lower and the upper
    // bound), and an index takes one wildcard field.
    if key.keys().any(|k| k == "$**" || k.ends_with(".$**")) {
        if let Some(layout) = crate::ts_buckets::Layout::of(&doc! {"timeseries": {
            "timeField": fields.time.clone(),
            "metaField": fields.meta.clone(),
        }}) {
            let mut shown = Document::new();
            if let Some(name) = spec.get("name") {
                shown.insert("name", name.clone());
            }
            shown.insert("key", crate::ts_buckets::bucket_key(key, &layout));
            shown.insert("originalSpec", spec.clone());
            return Some(CommandError::new(
                7246201,
                "Location7246201",
                format!(
                    "Error in specification {} :: caused by :: Index Key can contain only one \
                     wildcard index field",
                    secantus_core::query::bson_value_repr(&Bson::Document(shown))
                ),
            ));
        }
    }
    if key.values().any(|v| v.as_str() == Some("text")) {
        return Some(invalid(
            "Text indexes are not supported on time-series collections",
        ));
    }
    if spec.contains_key("expireAfterSeconds") {
        let on_meta = |filter: &Document| {
            fields
                .meta
                .as_ref()
                .is_some_and(|meta| !filter.is_empty() && filter.keys().all(|k| in_meta(k, meta)))
        };
        if !spec
            .get_document("partialFilterExpression")
            .is_ok_and(on_meta)
        {
            return Some(invalid(
                "TTL indexes on time-series collections require a partialFilterExpression on \
                 the metaField",
            ));
        }
        if key.len() != 1 || !key.contains_key(&fields.time) {
            return Some(invalid(
                "TTL indexes on non-time fields are not supported on time-series collections",
            ));
        }
    }
    None
}

/// Apply `collMod`'s `timeseries` document to a stored definition.
pub(crate) fn modified(current: &Document, change: &Document) -> Result<Document, CommandError> {
    const PATH: &str = "collMod.timeseries";
    let (mut gran, mut span, mut rounding) = (None, None, None);
    for (k, v) in change {
        match k.as_str() {
            "granularity" => gran = granularity(PATH, v)?,
            "bucketMaxSpanSeconds" => span = Some(seconds(PATH, k, v)?),
            "bucketRoundingSeconds" => rounding = Some(seconds(PATH, k, v)?),
            _ => return Err(unknown(PATH, k)),
        }
    }
    let time = current.get_str("timeField").unwrap_or_default();
    let meta = current.get_str("metaField").ok();
    let was_gran = current.get_str("granularity").ok();
    let was_span = i64::from(current.get_i32("bucketMaxSpanSeconds").unwrap_or(3600));
    if span.is_some() != rounding.is_some() {
        return Err(invalid(
            "Timeseries 'bucketMaxSpanSeconds' and 'bucketRoundingSeconds' need to be set \
             alongside each other",
        ));
    }
    if span != rounding {
        return Err(invalid(
            "Time-series 'bucketRoundingSeconds' must be equal to 'bucketMaxSpanSeconds'",
        ));
    }
    if let (Some(g), Some(span)) = (&gran, span) {
        if span != i64::from(default_span(g)) {
            return Err(invalid(
                "Timeseries 'bucketMaxSpanSeconds' is not configurable to a value other than \
                 the default for the provided granularity",
            ));
        }
    }
    if let Some(g) = &gran {
        let rank = |g: &str| GRANULARITIES.iter().position(|x| *x == g).unwrap_or(0);
        if was_gran.is_some_and(|was| rank(g) < rank(was)) {
            return Err(invalid(
                "Invalid transition for timeseries.granularity. Can only transition from \
                 'seconds' to 'minutes' or 'minutes' to 'hours'.",
            ));
        }
        return Ok(stored(time, meta, Some(g), None));
    }
    if let Some(span) = span {
        if span < was_span {
            return Err(invalid(
                "Timeseries 'bucketMaxSpanSeconds' needs to be equal or greater to transition",
            ));
        }
        return Ok(stored(time, meta, None, Some(span)));
    }
    Ok(current.clone())
}

/// `mongod` presents a time-series collection as a view, and says so when a
/// command needs a real collection.
pub(crate) fn is_a_view(ns: &str) -> CommandError {
    CommandError::new(
        166,
        "CommandNotSupportedOnView",
        format!("Namespace {ns} is a view, not a collection"),
    )
}

/// The bucket-level predicates mongod derives from a query on a time-series
/// collection, as its planner holds them over `system.buckets.<name>`
/// (measured 8.2.11, 2026-10-20): a measurement's comparison bounds the
/// bucket's `control.min` / `control.max`, the time field's also bounds the
/// bucket `_id` (an ObjectId whose time is the bucket's), offset by the
/// bucket span; a predicate on the meta field is kept, renamed `meta`. A
/// predicate it can derive nothing from (null, an array or a document, a
/// `$ne`, another type on the time field) bounds nothing and is left out.
pub(crate) fn bucket_filter(
    filter: &Document,
    time: &str,
    meta: Option<&str>,
    span_secs: i64,
) -> Document {
    let fields = BucketFields {
        time,
        meta,
        span_ms: span_secs * 1000,
    };
    and_of(fields.clauses(filter))
}

struct BucketFields<'a> {
    time: &'a str,
    meta: Option<&'a str>,
    span_ms: i64,
}

fn and_of(clauses: Vec<Document>) -> Document {
    match clauses.len() {
        0 => Document::new(),
        1 => clauses.into_iter().next().expect("one"),
        _ => doc! { "$and": clauses.into_iter().map(Bson::Document).collect::<Vec<_>>() },
    }
}

/// `{path: {op: value}}`.
fn pred(path: &str, op: &str, value: Bson) -> Document {
    let mut inner = Document::new();
    inner.insert(op, value);
    let mut out = Document::new();
    out.insert(path, inner);
    out
}

impl BucketFields<'_> {
    fn clauses(&self, filter: &Document) -> Vec<Document> {
        let mut out = Vec::new();
        for (key, value) in filter {
            match (key.as_str(), value) {
                ("$and", Bson::Array(items)) => {
                    for item in items.iter().filter_map(Bson::as_document) {
                        out.extend(self.clauses(item));
                    }
                }
                ("$or", Bson::Array(items)) => {
                    let branches: Vec<Document> = items
                        .iter()
                        .filter_map(Bson::as_document)
                        .map(|b| and_of(self.clauses(b)))
                        .collect();
                    // A branch that bounds nothing leaves the `$or` bounding
                    // nothing.
                    if !branches.is_empty() && branches.iter().all(|b| !b.is_empty()) {
                        let list: Vec<Bson> = branches.into_iter().map(Bson::Document).collect();
                        out.push(doc! { "$or": list });
                    }
                }
                (k, _) if k.starts_with('$') => {}
                (path, value) => out.extend(self.field(path, value)),
            }
        }
        out
    }

    fn field(&self, path: &str, value: &Bson) -> Vec<Document> {
        if let Some(meta) = self.meta {
            if let Some(rest) = path
                .strip_prefix(meta)
                .filter(|r| r.is_empty() || r.starts_with('.'))
            {
                let mut out = Document::new();
                out.insert(format!("meta{rest}"), value.clone());
                return vec![out];
            }
        }
        match value {
            Bson::Document(ops) if ops.keys().next().is_some_and(|k| k.starts_with('$')) => ops
                .iter()
                .flat_map(|(op, arg)| self.operator(path, op, arg))
                .collect(),
            other => self.operator(path, "$eq", other),
        }
    }

    fn operator(&self, path: &str, op: &str, arg: &Bson) -> Vec<Document> {
        match op {
            "$eq" | "$gt" | "$gte" | "$lt" | "$lte" => self.comparison(path, op, arg),
            "$in" => {
                let Some(items) = arg.as_array() else {
                    return Vec::new();
                };
                let branches: Vec<Document> = items
                    .iter()
                    .map(|v| and_of(self.comparison(path, "$eq", v)))
                    .collect();
                if branches.is_empty() || branches.iter().any(Document::is_empty) {
                    return Vec::new();
                }
                if branches.len() == 1 {
                    return branches;
                }
                let list: Vec<Bson> = branches.into_iter().map(Bson::Document).collect();
                vec![doc! { "$or": list }]
            }
            "$exists" if crate::util::truthy(arg) => vec![
                pred(
                    &format!("control.max.{path}"),
                    "$exists",
                    Bson::Boolean(true),
                ),
                pred(
                    &format!("control.min.{path}"),
                    "$exists",
                    Bson::Boolean(true),
                ),
            ],
            _ => Vec::new(),
        }
    }

    fn comparison(&self, path: &str, op: &str, arg: &Bson) -> Vec<Document> {
        if matches!(
            arg,
            Bson::Null
                | Bson::Undefined
                | Bson::Array(_)
                | Bson::Document(_)
                | Bson::RegularExpression(_)
                | Bson::MinKey
                | Bson::MaxKey
        ) {
            return Vec::new();
        }
        let max = format!("control.max.{path}");
        let min = format!("control.min.{path}");
        if path == self.time {
            let Bson::DateTime(at) = arg else {
                return Vec::new();
            };
            return self.time_bounds(op, at.timestamp_millis(), &max, &min);
        }
        let v = || arg.clone();
        match op {
            "$gt" => vec![pred(&max, "$_internalExprGt", v())],
            "$gte" => vec![pred(&max, "$_internalExprGte", v())],
            "$lt" => vec![pred(&min, "$_internalExprLt", v())],
            "$lte" => vec![pred(&min, "$_internalExprLte", v())],
            _ => vec![
                pred(&max, "$_internalExprGte", v()),
                pred(&min, "$_internalExprLte", v()),
            ],
        }
    }

    /// The time field's: the bucket's own bounds, which lie within one span
    /// of the measurement's, and its `_id`, whose ObjectId carries the time
    /// the bucket starts at.
    fn time_bounds(&self, op: &str, at: i64, max: &str, min: &str) -> Vec<Document> {
        let date = |ms: i64| Bson::DateTime(bson::DateTime::from_millis(ms));
        let id = |ms: i64, fill: u8| {
            let mut bytes = [fill; 12];
            let secs = ms.div_euclid(1000) as u32;
            bytes[..4].copy_from_slice(&secs.to_be_bytes());
            Bson::ObjectId(bson::oid::ObjectId::from_bytes(bytes))
        };
        let (at_low, at_high) = (at - self.span_ms, at + self.span_ms);
        match op {
            "$gt" => vec![
                pred("_id", "$gt", id(at_low, 0xff)),
                pred(max, "$_internalExprGt", date(at)),
                pred(min, "$_internalExprGt", date(at_low)),
            ],
            "$gte" => vec![
                pred("_id", "$gte", id(at_low, 0)),
                pred(max, "$_internalExprGte", date(at)),
                pred(min, "$_internalExprGte", date(at_low)),
            ],
            "$lt" => vec![
                pred("_id", "$lt", id(at, 0)),
                pred(max, "$_internalExprLt", date(at_high)),
                pred(min, "$_internalExprLt", date(at)),
            ],
            "$lte" => vec![
                pred("_id", "$lte", id(at, 0xff)),
                pred(max, "$_internalExprLte", date(at_high)),
                pred(min, "$_internalExprLte", date(at)),
            ],
            _ => vec![
                pred("_id", "$lte", id(at, 0xff)),
                pred("_id", "$gte", id(at_low, 0)),
                pred(max, "$_internalExprGte", date(at)),
                pred(min, "$_internalExprGte", date(at_low)),
                pred(max, "$_internalExprLte", date(at_high)),
                pred(min, "$_internalExprLte", date(at)),
            ],
        }
    }
}

/// A hint on a read of the time-series collection `db.coll` that names
/// none of its indexes: refused as mongod's planner refuses it over the
/// buckets, with the bucket-level query it planned and the simple
/// collation, whatever the read's sort, projection or paging (mongod
/// 8.2.11, 2026-10-20). The buckets are clustered, so `{_id: 1}` names no
/// index (the name `_id_` does).
pub(crate) fn check_hint(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    filter: &Document,
    hint: Option<&Bson>,
) -> Result<(), CommandError> {
    let Ok(options) = storage.get_collection_options(db, coll) else {
        return Ok(());
    };
    let Ok(ts) = options.get_document("timeseries") else {
        return Ok(());
    };
    let id_spec = matches!(hint, Some(Bson::Document(d)) if d.len() == 1 && d.contains_key("_id"));
    if !id_spec && crate::util::hint_is_known(storage, db, coll, hint) {
        return Ok(());
    }
    let buckets = bucket_filter(
        filter,
        ts.get_str("timeField").unwrap_or_default(),
        ts.get_str("metaField").ok(),
        crate::util::as_i64(ts.get("bucketMaxSpanSeconds").unwrap_or(&Bson::Int32(3600)))
            .unwrap_or(3600),
    );
    let simple = doc! {"locale": "simple"};
    let query = crate::explain::CanonicalQuery {
        collation: Some(&simple),
        ..crate::explain::CanonicalQuery::plain(db, &format!("system.buckets.{coll}"), &buckets)
    };
    Err(CommandError::new(
        2,
        "BadValue",
        query.planner_error("hint provided does not correspond to an existing index"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn define(ts: Document) -> Result<Option<Document>, CommandError> {
        definition(&doc! {"create": "w", "timeseries": ts}, "db.w")
    }

    fn refusal(ts: Document) -> (i32, String) {
        let e = define(ts).unwrap_err();
        (e.code, e.errmsg)
    }

    #[test]
    fn a_definition_is_stored_with_its_defaults() {
        assert_eq!(
            define(doc! {"timeField": "t"}).unwrap().unwrap(),
            doc! {"timeField": "t", "granularity": "seconds", "bucketMaxSpanSeconds": 3600}
        );
        assert_eq!(
            define(doc! {"timeField": "t", "metaField": "m", "granularity": "hours"})
                .unwrap()
                .unwrap(),
            doc! {"timeField": "t", "metaField": "m", "granularity": "hours",
            "bucketMaxSpanSeconds": 2592000}
        );
        assert_eq!(
            define(
                doc! {"timeField": "t", "bucketMaxSpanSeconds": 600, "bucketRoundingSeconds": 600}
            )
            .unwrap()
            .unwrap(),
            doc! {"timeField": "t", "bucketRoundingSeconds": 600, "bucketMaxSpanSeconds": 600}
        );
        assert_eq!(
            define(doc! {"timeField": "t", "metaField": Bson::Null})
                .unwrap()
                .unwrap(),
            doc! {"timeField": "t", "granularity": "seconds", "bucketMaxSpanSeconds": 3600}
        );
    }

    #[test]
    fn a_bad_definition_is_refused_as_mongod_refuses_it() {
        assert_eq!(refusal(doc! {"metaField": "m"}).0, 40414);
        assert_eq!(
            refusal(doc! {"timeField": "t", "granularity": "days"}),
            (
                2,
                "Enumeration value 'days' for field 'create.timeseries.granularity' is not a \
                 valid value."
                    .into()
            )
        );
        assert_eq!(
            refusal(doc! {"timeField": "t", "metaField": "t"}),
            (72, "'metaField' cannot be the same as 'timeField'".into())
        );
        assert_eq!(
            refusal(doc! {"timeField": "a.t"}).1,
            "db.w: 'timeField' must be a top-level field and not contain a '.'"
        );
        assert_eq!(
            refusal(doc! {"timeField": "t", "bucketMaxSpanSeconds": 600}).1,
            "Time-series 'bucketRoundingSeconds' must be equal to 'bucketMaxSpanSeconds'"
        );
        assert_eq!(
            refusal(doc! {"timeField": "t", "bucketMaxSpanSeconds": 0, "bucketRoundingSeconds": 0})
                .1,
            "BSON field 'bucketMaxSpanSeconds' value must be >= 1, actual value '0'"
        );
        assert_eq!(refusal(doc! {"timeField": "t", "bogus": 1}).0, 40415);
        let with_capped = definition(
            &doc! {"timeseries": {"timeField": "t"}, "capped": true},
            "db.w",
        );
        assert_eq!(
            with_capped.unwrap_err().errmsg,
            "db.w: 'timeseries' is not allowed with 'capped'"
        );
    }

    #[test]
    fn an_update_may_only_touch_the_meta_field() {
        let fields = Fields {
            time: "t".into(),
            meta: Some("m".into()),
        };
        let check = |statement: Document| update_problem(&statement, &fields);
        let multi = |q: Document, u: Document| doc! {"q": q, "u": u, "multi": true};
        assert_eq!(check(multi(doc! {}, doc! {"$set": {"m.k": 1}})), None);
        assert_eq!(
            check(multi(doc! {"m.k": 1}, doc! {"$unset": {"m": 1}})),
            None
        );
        assert_eq!(
            check(multi(doc! {}, doc! {"$rename": {"m.k": "m.z"}})),
            None
        );
        assert!(check(doc! {"q": {}, "u": {"$set": {"m": 1}}})
            .unwrap()
            .contains("non-multi"));
        assert!(check(multi(doc! {"v": 1}, doc! {"$set": {"m": 1}}))
            .unwrap()
            .contains("querying on a field that is not the metaField 'm'"));
        assert!(check(multi(doc! {"mx": 1}, doc! {"$set": {"m": 1}}))
            .unwrap()
            .contains("querying"));
        assert!(check(multi(doc! {}, doc! {"$set": {"m.k": 1, "v": 2}}))
            .unwrap()
            .contains("updates a field that is not the metaField 'm'"));
        assert!(check(multi(doc! {}, doc! {"$rename": {"m": "m2"}}))
            .unwrap()
            .contains("updates a field"));
        assert!(check(multi(doc! {}, doc! {"m": 1}))
            .unwrap()
            .contains("replacement document"));
        assert!(check(multi(
            doc! {"$expr": {"$eq": ["$m", 1]}},
            doc! {"$set": {"m": 1}}
        ))
        .unwrap()
        .starts_with("Cannot use $expr"));
        let no_meta = Fields {
            time: "t".into(),
            meta: None,
        };
        assert!(
            update_problem(&multi(doc! {}, doc! {"$set": {"x": 1}}), &no_meta)
                .unwrap()
                .ends_with("does not have a metaField")
        );
    }

    #[test]
    fn collmod_only_widens_the_buckets() {
        let seconds =
            doc! {"timeField": "t", "granularity": "seconds", "bucketMaxSpanSeconds": 3600};
        let up = modified(&seconds, &doc! {"granularity": "minutes"}).unwrap();
        assert_eq!(up.get_i32("bucketMaxSpanSeconds").ok(), Some(86400));
        assert!(modified(&up, &doc! {"granularity": "seconds"}).is_err());
        let custom = modified(
            &seconds,
            &doc! {"bucketMaxSpanSeconds": 7200, "bucketRoundingSeconds": 7200},
        )
        .unwrap();
        assert_eq!(
            custom,
            doc! {"timeField": "t", "bucketRoundingSeconds": 7200, "bucketMaxSpanSeconds": 7200}
        );
        assert!(modified(
            &seconds,
            &doc! {"bucketMaxSpanSeconds": 60, "bucketRoundingSeconds": 60}
        )
        .is_err());
        assert_eq!(
            modified(&seconds, &doc! {"timeField": "x"})
                .unwrap_err()
                .code,
            40415
        );
        assert_eq!(modified(&seconds, &doc! {}).unwrap(), seconds);
    }

    #[test]
    fn index_rules() {
        let fields = Fields {
            time: "t".into(),
            meta: Some("m".into()),
        };
        let problem = |spec: Document| index_problem(&spec, &fields).map(|e| e.errmsg);
        assert_eq!(problem(doc! {"key": {"v": 1}}), None);
        assert!(problem(doc! {"key": {"m": 1}, "unique": true})
            .unwrap()
            .starts_with("Unique"));
        assert!(problem(doc! {"key": {"v": "text"}})
            .unwrap()
            .starts_with("Text"));
        assert!(problem(doc! {"key": {"t": 1}, "expireAfterSeconds": 5})
            .unwrap()
            .contains("require a partialFilterExpression on the metaField"));
        assert_eq!(
            problem(doc! {"key": {"t": 1}, "expireAfterSeconds": 5,
            "partialFilterExpression": {"m": "a"}}),
            None
        );
        assert!(problem(doc! {"key": {"m": 1}, "expireAfterSeconds": 5,
        "partialFilterExpression": {"m": "a"}})
        .unwrap()
        .contains("non-time fields"));
        assert_eq!(fields.default_index().unwrap().0, "m_1_t_1");
    }

    /// What mongod 8.2.11 printed over the buckets (2026-10-20).
    #[test]
    fn bucket_filters_print_as_mongod_planned_them() {
        use crate::explain::debug_string::debug_string;
        let at = bson::DateTime::from_millis(1672531200000);
        let tree = |f: Document, span: i64| debug_string(&bucket_filter(&f, "t", Some("m"), span));
        assert_eq!(tree(doc! {}, 3600), "$and\n");
        assert_eq!(
            tree(doc! {"t": {"$gt": at}}, 3600),
            "$and\n    _id $gt ObjectId('63b0bef0ffffffffffffffff')\n    \
             control.max.t $_internalExprGt new Date(1672531200000)\n    \
             control.min.t $_internalExprGt new Date(1672527600000)\n"
        );
        assert_eq!(
            tree(doc! {"t": at}, 3600),
            "$and\n    _id $lte ObjectId('63b0cd00ffffffffffffffff')\n    \
             _id $gte ObjectId('63b0bef00000000000000000')\n    \
             control.max.t $_internalExprGte new Date(1672531200000)\n    \
             control.min.t $_internalExprGte new Date(1672527600000)\n    \
             control.max.t $_internalExprLte new Date(1672534800000)\n    \
             control.min.t $_internalExprLte new Date(1672531200000)\n"
        );
        assert_eq!(
            tree(doc! {"t": {"$lt": at}}, 2592000),
            "$and\n    _id $lt ObjectId('63b0cd000000000000000000')\n    \
             control.max.t $_internalExprLt new Date(1675123200000)\n    \
             control.min.t $_internalExprLt new Date(1672531200000)\n"
        );
        assert_eq!(
            tree(doc! {"v": {"$gt": 1}}, 3600),
            "control.max.v $_internalExprGt 1\n"
        );
        assert_eq!(
            tree(doc! {"v": 1}, 3600),
            "$and\n    control.max.v $_internalExprGte 1\n    control.min.v $_internalExprLte 1\n"
        );
        for nothing in [
            doc! {"v": Bson::Null},
            doc! {"v": {"$ne": 1}},
            doc! {"v": [1]},
            doc! {"t": {"$gt": "x"}},
        ] {
            assert_eq!(tree(nothing, 3600), "$and\n");
        }
        assert_eq!(
            tree(doc! {"v": {"$in": [1, 2]}}, 3600),
            "$or\n    $and\n        control.max.v $_internalExprGte 1\n        \
             control.min.v $_internalExprLte 1\n    $and\n        \
             control.max.v $_internalExprGte 2\n        control.min.v $_internalExprLte 2\n"
        );
        assert_eq!(
            tree(doc! {"v": {"$exists": true}}, 3600),
            "$and\n    control.max.v exists\n    control.min.v exists\n"
        );
        assert_eq!(tree(doc! {"m.k": {"$gt": 1}}, 3600), "meta.k $gt 1\n");
        assert_eq!(tree(doc! {"m": {"$in": [1, 2]}}, 3600), "meta $in [ 1 2]\n");
        assert_eq!(
            tree(doc! {"$or": [{"v": 1}, {"m.k": 2}]}, 3600),
            "$or\n    $and\n        control.max.v $_internalExprGte 1\n        \
             control.min.v $_internalExprLte 1\n    meta.k $eq 2\n"
        );
        assert_eq!(
            tree(doc! {"m.k": 1, "v": 2}, 3600),
            "$and\n    meta.k $eq 1\n    control.max.v $_internalExprGte 2\n    \
             control.min.v $_internalExprLte 2\n"
        );
    }
}
