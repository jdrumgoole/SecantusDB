//! The checks mongod's expression PARSER makes, in the order it makes them.
//!
//! mongod parses every expression in a command before it reads a document, so
//! a malformed one fails on an empty collection too. The evaluator in
//! `expressions.rs` reads an expression only when a document reaches it, and
//! reads an object it cannot place as a literal value. This module walks an
//! expression the way `Expression::parseOperand` does (mongod 8.2.11,
//! `src/mongo/db/pipeline/expression.cpp`) and reports the first error the
//! parser would raise:
//!
//! - an object whose first field names an operator is that one operator and
//!   has no other field (15983), and the operator must exist (168);
//! - any other object is an object literal, whose field names follow
//!   `FieldPath::validateFieldName` (15998, 16410, 16411, 16412) and may not
//!   repeat (16406);
//! - an operator's own missing-argument errors
//!   ([`crate::expressions::check_required_fields`]).
//!
//! Errors are bare: the caller adds the stage's envelope where mongod has one
//! (`Invalid $addFields :: caused by :: ...`).

use bson::{Bson, Document};

use crate::expressions::{check_required_fields, KNOWN_EXPR_OPS};
use crate::fallback::Fallback;

type R = Result<(), Fallback>;

/// mongod's `FieldPath::validateFieldName` context, for a field name inside
/// an object literal or a projection.
const FIELD_NAME_CONTEXT: &str =
    "Consider using $getField or $setField for a field path with '.' or '$'.";

/// The `$`-prefixed names `FieldPath` accepts (DBRef and metadata fields).
const ALLOWED_DOLLAR_FIELDS: &[&str] = &[
    "$id",
    "$ref",
    "$db",
    "$sortKey",
    "$recordId",
    "$searchSortValues",
    "$searchScore",
];

/// Operators whose document argument holds NAMED arguments rather than an
/// object literal: their keys are not field names, and a sort spec among them
/// is not an expression. Each value that is an expression is walked.
const NAMED_ARGUMENT_OPS: &[&str] = &[
    "$bottom",
    "$bottomN",
    "$cond",
    "$convert",
    "$dateAdd",
    "$dateDiff",
    "$dateFromParts",
    "$dateFromString",
    "$dateSubtract",
    "$dateToParts",
    "$dateToString",
    "$dateTrunc",
    "$filter",
    "$firstN",
    "$function",
    "$getField",
    "$lastN",
    "$let",
    "$ltrim",
    "$map",
    "$maxN",
    "$median",
    "$minN",
    "$percentile",
    "$reduce",
    "$regexFind",
    "$regexFindAll",
    "$regexMatch",
    "$replaceAll",
    "$replaceOne",
    "$rtrim",
    "$setField",
    "$sigmoid",
    "$sortArray",
    "$switch",
    "$top",
    "$topN",
    "$trim",
    "$unsetField",
    "$zip",
];

/// `FieldPath::validateFieldName`: the bare status message for one field name.
pub fn field_name_problem(name: &str) -> Option<(i32, String)> {
    name_problem(name, false)
}

/// [`field_name_problem`] for a name that came from a document KEY, which may
/// be the escape of a name that is not UTF-8 (`bsondoc::escape_key`). mongod
/// accepts such a name; a NUL that is not that escape is still refused. A
/// name from a string VALUE never takes this: a string can hold a NUL and
/// two hex digits, and mongod refuses that.
fn name_problem(name: &str, key: bool) -> Option<(i32, String)> {
    if name.is_empty() {
        return Some((
            15998,
            "FieldPath field names may not be empty strings.".to_string(),
        ));
    }
    if name.starts_with('$') && !ALLOWED_DOLLAR_FIELDS.contains(&name) {
        return Some((
            16410,
            format!("FieldPath field names may not start with '$', given '{name}'."),
        ));
    }
    if name.contains('\0') && !(key && crate::bsondoc::key_bytes(name).is_some()) {
        return Some((
            16411,
            format!("FieldPath field names may not contain '\0', given '{name}'."),
        ));
    }
    if name.contains('.') {
        return Some((
            16412,
            format!("FieldPath field names may not contain '.', given '{name}'."),
        ));
    }
    None
}

/// A field name refused, with the context mongod's `FieldPath` adds.
pub fn field_name_error(name: &str) -> Option<Fallback> {
    name_error(name, false)
}

fn name_error(name: &str, key: bool) -> Option<Fallback> {
    name_problem(name, key).map(|(code, msg)| {
        Fallback::mongo(code, format!("{FIELD_NAME_CONTEXT} :: caused by :: {msg}")).bare()
    })
}

/// [`field_path_error`] for a path that is a document KEY (a projection's or
/// an `$addFields`' field, an object literal's name): an escaped name that is
/// not UTF-8 is accepted (RM-222).
pub fn key_path_error(path: &str) -> Option<Fallback> {
    path_error(path, true)
}

/// The `FieldPath` constructor over a dotted path: 40352 for an empty path,
/// 40353 for a trailing dot, then each component's name.
pub fn field_path_error(path: &str) -> Option<Fallback> {
    path_error(path, false)
}

fn path_error(path: &str, key: bool) -> Option<Fallback> {
    if path.is_empty() {
        return Some(
            Fallback::mongo(40352, "FieldPath cannot be constructed with empty string").bare(),
        );
    }
    if path.ends_with('.') {
        return Some(Fallback::mongo(40353, "FieldPath must not end with a '.'.").bare());
    }
    path.split('.').find_map(|name| name_error(name, key))
}

/// `Expression::parseOperand`: any value in expression position.
pub fn operand(v: &Bson) -> R {
    match v {
        Bson::Document(d) => object(d),
        Bson::Array(items) => items.iter().try_for_each(operand),
        _ => Ok(()),
    }
}

/// `Expression::parseObject`: an operator when the first field names one,
/// an object literal otherwise. The empty object is a constant.
pub fn object(d: &Document) -> R {
    match d.keys().next() {
        None => Ok(()),
        Some(first) if first.starts_with('$') => operator(d),
        Some(_) => object_literal(d),
    }
}

/// `Expression::parseExpression`: exactly one field, a known operator, then
/// the operator's own parse of its argument.
pub fn operator(d: &Document) -> R {
    if d.len() != 1 {
        return Err(Fallback::mongo(
            15983,
            format!(
                "An object representing an expression must have exactly one field: {}",
                crate::query::bson_value_repr(&Bson::Document(d.clone()))
            ),
        )
        .bare());
    }
    let (op, arg) = d.iter().next().expect("one field");
    if !KNOWN_EXPR_OPS.contains(&op.as_str()) {
        return Err(Fallback::mongo(168, format!("Unrecognized expression '{op}'")).bare());
    }
    if op == "$literal" || op == "$const" {
        return Ok(());
    }
    check_required_fields(op, arg)?;
    argument(op, arg)
}

/// An operator's argument: named arguments are walked value by value (a sort
/// spec is not an expression); anything else is an operand.
fn argument(op: &str, arg: &Bson) -> R {
    match arg {
        Bson::Document(named) if NAMED_ARGUMENT_OPS.contains(&op) => named
            .iter()
            .filter(|(k, _)| k.as_str() != "sortBy")
            .try_for_each(|(_, v)| operand(v)),
        _ => operand(arg),
    }
}

/// `ExpressionObject::parse`: each field name is a valid `FieldPath` name and
/// appears once; each value is an operand.
pub fn object_literal(d: &Document) -> R {
    // A string that is not UTF-8 is a constant, not an object (RM-222).
    if crate::bsondoc::carrier_bytes(d).is_some() {
        return Ok(());
    }
    let mut seen = std::collections::HashSet::new();
    for (name, value) in d {
        if let Some(e) = name_error(name, true) {
            return Err(e);
        }
        if !seen.insert(name.as_str()) {
            return Err(Fallback::mongo(
                16406,
                format!(
                    "duplicate field name specified in object literal: {}",
                    crate::query::bson_value_repr(&Bson::Document(d.clone()))
                ),
            )
            .bare());
        }
        operand(value)?;
    }
    Ok(())
}

/// `$addFields` / `$set`: mongod's `ProjectionSpecValidator` over the whole
/// spec (conflicting paths 40176, a multi-field expression object 40181, a
/// dotted name in a sub-object 40183), then the parse of each field
/// (`AddFieldsProjectionExecutor::parse`). mongod 8.2.11,
/// `add_fields_projection_executor.cpp`.
pub fn add_fields_spec(spec: &Document) -> R {
    let raw = crate::query::bson_value_repr(&Bson::Document(spec.clone()));
    let mut seen = PathSet::default();
    for (name, value) in spec {
        if let Some(e) = key_path_error(name) {
            return Err(e);
        }
        add_fields_element(value, name, &mut seen, &raw)?;
    }
    for (_, value) in spec {
        match value {
            Bson::Document(d) if crate::bsondoc::carrier_bytes(d).is_none() => {
                add_fields_object(d)?
            }
            other => operand(other)?,
        }
    }
    Ok(())
}

fn add_fields_element(value: &Bson, path: &str, seen: &mut PathSet, raw: &str) -> R {
    let Some(level) = value
        .as_document()
        .filter(|d| crate::bsondoc::carrier_bytes(d).is_none())
    else {
        return seen.insert(path, raw);
    };
    for (name, child) in level {
        if name.starts_with('$') {
            if level.len() != 1 {
                return Err(Fallback::mongo(
                    40181,
                    format!(
                        "an expression specification must contain exactly one field, the name \
                         of the expression. Found {} fields in {}, while parsing object {raw}",
                        level.len(),
                        crate::query::bson_value_repr(value)
                    ),
                )
                .bare());
            }
            seen.insert(path, raw)?;
            continue;
        }
        if name.contains('.') {
            return Err(Fallback::mongo(
                40183,
                format!("cannot use dotted field name '{name}' in a sub object: {raw}"),
            )
            .bare());
        }
        add_fields_element(child, &format!("{path}.{name}"), seen, raw)?;
    }
    Ok(())
}

/// An object value in `$addFields`: an expression when its first field names
/// an operator (the validator has made it the only field), otherwise a nested
/// specification whose leaves are operands.
fn add_fields_object(d: &Document) -> R {
    match d.keys().next() {
        Some(first) if first.starts_with('$') => operator(d),
        _ => d.values().try_for_each(|v| match v {
            Bson::Document(sub) => add_fields_object(sub),
            other => operand(other),
        }),
    }
}

/// mongod's `OrderedPathSet` with `isPathPrefixOf`: no path may repeat or be
/// a prefix of another. Ordered so a path sorts just after its prefix.
#[derive(Default)]
struct PathSet(std::collections::BTreeMap<String, String>);

impl PathSet {
    fn insert(&mut self, path: &str, raw: &str) -> R {
        let key = path.replace('.', "\0");
        let prefixes = |first: &str, second: &str| {
            second.len() > first.len()
                && second.starts_with(first)
                && second.as_bytes()[first.len()] == b'.'
        };
        let conflict = if self.0.contains_key(&key) {
            Some(path.to_string())
        } else {
            let before = self.0.range(..key.clone()).next_back().map(|(_, p)| p);
            let after = self.0.range(key.clone()..).next().map(|(_, p)| p);
            before
                .filter(|p| prefixes(p, path))
                .or(after.filter(|p| prefixes(path, p)))
                .cloned()
        };
        if let Some(other) = conflict {
            return Err(Fallback::mongo(
                40176,
                format!(
                    "specification contains two conflicting paths. Cannot specify both \
                     '{path}' and '{other}': {raw}"
                ),
            )
            .bare());
        }
        self.0.insert(key, path.to_string());
        Ok(())
    }
}

/// Which projection grammar: an aggregation `$project`, or a `find` /
/// `findAndModify` projection (positional `.$`, find's `$slice` and
/// `$elemMatch`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProjectionKind {
    Aggregation,
    Find,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProjectType {
    Inclusion,
    Exclusion,
}

/// A projection tree node, as `ProjectionPathASTNode` builds it.
enum Node {
    Inner(Vec<(String, Node)>),
    Leaf,
}

impl Node {
    fn children(&mut self) -> Option<&mut Vec<(String, Node)>> {
        match self {
            Node::Inner(c) => Some(c),
            Node::Leaf => None,
        }
    }
}

struct ProjectionParse {
    kind: ProjectionKind,
    ty: Option<ProjectType>,
}

/// mongod's projection parser (`projection_parser.cpp`, 8.2.11): field names,
/// path collisions (31249 / 31250), inclusion against exclusion (31252,
/// 31253, 31254, 31310), empty sub-projections (51270), unknown operators
/// (31325) and every expression inside, in the order the parser meets them.
/// What it does not model -- positional and find-only operators, `$meta`
/// availability -- it leaves to the checks that follow.
pub fn projection_spec(spec: &Document, kind: ProjectionKind) -> R {
    let mut ctx = ProjectionParse { kind, ty: None };
    let mut root = Node::Inner(Vec::new());
    for (name, value) in spec {
        ctx.element(name, value, &mut root, true)?;
    }
    Ok(())
}

fn add_node(parent: &mut Node, path: &str, new: Node) -> R {
    let parts: Vec<&str> = path.split('.').collect();
    let mut node = parent;
    for (i, part) in parts.iter().enumerate() {
        let children = node.children().expect("inner node");
        let at = children.iter().position(|(k, _)| k == part);
        if i + 1 == parts.len() {
            if at.is_some() {
                return Err(Fallback::mongo(31250, format!("Path collision at {path}")).bare());
            }
            children.push((part.to_string(), new));
            return Ok(());
        }
        let at = match at {
            Some(at) => at,
            None => {
                children.push((part.to_string(), Node::Inner(Vec::new())));
                children.len() - 1
            }
        };
        if matches!(children[at].1, Node::Leaf) {
            return Err(Fallback::mongo(
                31249,
                format!(
                    "Path collision at {path} remaining portion {}",
                    parts[1..].join(".")
                ),
            )
            .bare());
        }
        node = &mut children[at].1;
    }
    Ok(())
}

/// `findLastInnerNodeOnPath`: the inner node at `path`, if there is one.
fn inner_at<'a>(parent: &'a mut Node, path: &str) -> Option<&'a mut Node> {
    let mut node = parent;
    for part in path.split('.') {
        let children = node.children()?;
        let at = children.iter().position(|(k, _)| k == part)?;
        node = &mut children[at].1;
    }
    node.children().is_some().then_some(node)
}

impl ProjectionParse {
    fn element(&mut self, name: &str, value: &Bson, parent: &mut Node, at_root: bool) -> R {
        // Positional projection has rules of its own; the find path checks it.
        if name.ends_with(".$") || name.contains(".$.") {
            if self.kind == ProjectionKind::Aggregation && name.ends_with(".$") {
                return Err(Fallback::mongo(
                    31324,
                    "Cannot use positional projection in aggregation projection",
                )
                .bare());
            }
            return Ok(());
        }
        match value {
            // A string that is not UTF-8 is an expression, not a sub-object.
            Bson::Document(d) if crate::bsondoc::carrier_bytes(d).is_none() => {
                self.sub_object(name, d, parent)
            }
            Bson::Int32(_)
            | Bson::Int64(_)
            | Bson::Double(_)
            | Bson::Decimal128(_)
            | Bson::Boolean(_) => {
                if let Some(e) = key_path_error(name) {
                    return Err(e);
                }
                add_node(parent, name, Node::Leaf)?;
                if at_root && name == "_id" {
                    return Ok(());
                }
                if crate::expressions::truthy(value) {
                    if self.ty == Some(ProjectType::Exclusion) {
                        return Err(Fallback::mongo(
                            31253,
                            format!("Cannot do inclusion on field {name} in exclusion projection"),
                        )
                        .bare());
                    }
                    self.ty = Some(ProjectType::Inclusion);
                } else {
                    if self.ty == Some(ProjectType::Inclusion) {
                        return Err(Fallback::mongo(
                            31254,
                            format!("Cannot do exclusion on field {name} in inclusion projection"),
                        )
                        .bare());
                    }
                    self.ty = Some(ProjectType::Exclusion);
                }
                Ok(())
            }
            other => {
                operand(other)?;
                if let Some(e) = key_path_error(name) {
                    return Err(e);
                }
                add_node(parent, name, Node::Leaf)?;
                if self.ty == Some(ProjectType::Exclusion) {
                    return Err(Fallback::mongo(
                        31310,
                        format!(
                            "Cannot use an expression {name}: {} in an exclusion projection",
                            crate::query::bson_value_repr(other)
                        ),
                    )
                    .bare());
                }
                self.ty = Some(ProjectType::Inclusion);
                Ok(())
            }
        }
    }

    /// A `$slice` in a find projection: find's own `[skip, limit]` syntax if
    /// it is that, else the aggregation expression, a computed field. When
    /// it is neither, mongod's error names both reasons (measured 8.2.11,
    /// 2026-10-24).
    fn find_slice(&mut self, name: &str, d: &Document, parent: &mut Node) -> R {
        let arg = d.get("$slice").unwrap_or(&Bson::Null);
        let Some((code, why)) = crate::projection::find_slice_problem(arg) else {
            return add_node(parent, name, Node::Leaf);
        };
        let passed = match arg {
            Bson::Array(a) => a.len(),
            _ => 1,
        };
        let as_expression = if self.ty == Some(ProjectType::Exclusion) {
            Err(Fallback::mongo(
                31252,
                "Cannot use expression other than $meta in exclusion projection",
            ))
        } else if !(2..=3).contains(&passed) {
            Err(Fallback::mongo(
                28667,
                format!(
                    "Expression $slice takes at least 2 arguments, and at most 3, but {passed} \
                     were passed in."
                ),
            ))
        } else {
            operator(d)
        };
        if let Err(e) = as_expression {
            let Some((inner, message)) = e.as_mongo() else {
                return Err(e);
            };
            return Err(Fallback::mongo(
                inner,
                format!(
                    "Invalid $slice syntax. The given syntax {} did not match the find() \
                     syntax because :: Location{code}: {why} :: The given syntax did not \
                     match the expression $slice syntax. :: caused by :: {message}",
                    crate::query::bson_value_repr(&Bson::Document(d.clone()))
                ),
            )
            .bare());
        }
        if self.ty.is_none() {
            self.ty = Some(ProjectType::Inclusion);
        }
        add_node(parent, name, Node::Leaf)
    }

    fn sub_object(&mut self, name: &str, d: &Document, parent: &mut Node) -> R {
        if d.is_empty() {
            return Err(
                Fallback::mongo(51270, format!("Invalid empty sub-projection: {name}")).bare(),
            );
        }
        if let Some(e) = key_path_error(name) {
            return Err(e);
        }
        if let (1, Some(first)) = (d.len(), d.keys().next()) {
            if first.starts_with('$') {
                match (self.kind, first.as_str()) {
                    (ProjectionKind::Find, "$slice") => return self.find_slice(name, d, parent),
                    // find's own operator, checked by the find path.
                    (ProjectionKind::Find, "$elemMatch") => {
                        return add_node(parent, name, Node::Leaf)
                    }
                    (ProjectionKind::Aggregation, "$elemMatch") => {
                        return Err(
                            Fallback::mongo(168, "Cannot use $elemMatch in this context").bare(),
                        )
                    }
                    _ => {}
                }
                if KNOWN_EXPR_OPS.contains(&first.as_str()) {
                    let is_meta = first == "$meta";
                    if !is_meta && self.ty == Some(ProjectType::Exclusion) {
                        return Err(Fallback::mongo(
                            31252,
                            "Cannot use expression other than $meta in exclusion projection",
                        )
                        .bare());
                    }
                    if !is_meta && self.ty.is_none() {
                        self.ty = Some(ProjectType::Inclusion);
                    }
                    operator(d)?;
                    return add_node(parent, name, Node::Leaf);
                }
                if field_path_error(first).is_some() {
                    return Err(
                        Fallback::mongo(31325, format!("Unknown expression {first}")).bare(),
                    );
                }
            }
        }
        if inner_at(parent, name).is_none() {
            add_node(parent, name, Node::Inner(Vec::new()))?;
        }
        let node = inner_at(parent, name).expect("added above");
        for (child, value) in d {
            self.element(child, value, node, false)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::bson;

    fn code(v: Bson) -> Option<i32> {
        operand(&v).err().and_then(|f| f.as_mongo().map(|(c, _)| c))
    }

    #[test]
    fn mongod_parse_order() {
        // Measured 8.2.11, 2026-10-23.
        assert_eq!(code(bson!({"$add": [1], "$b": 2})), Some(15983));
        assert_eq!(code(bson!({"$nosuch": 1, "$b": 2})), Some(15983));
        assert_eq!(
            code(bson!({"$and": [{"$nosuch": 1}, {"$add": [1], "$b": 2}]})),
            Some(168)
        );
        assert_eq!(
            code(bson!({"$and": [{"$add": [1], "$b": 2}, {"$nosuch": 1}]})),
            Some(15983)
        );
        assert_eq!(code(bson!({"$eq": [{"a": 1, "$b": 2}, 1]})), Some(16410));
        assert_eq!(code(bson!({"$eq": [{"a.b": 1}, 1]})), Some(16412));
        assert_eq!(code(bson!({"$literal": {"$a": 1, "$b": 2}})), None);
        assert_eq!(
            code(bson!({"$sortArray": {"input": [1], "sortBy": {"a.b": 1}}})),
            None
        );
        // A DBRef name is a field only after the first field: first, it reads
        // as an operator.
        assert_eq!(code(bson!({"a": {"$id": 1, "$ref": "c"}})), Some(15983));
        assert_eq!(code(bson!({"a": {"x": 1, "$id": 1}})), None);
        assert_eq!(code(bson!({})), None);
    }
}
