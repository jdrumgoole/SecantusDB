//! Moving a pipeline's `$match` stages to its front, as mongod's pipeline
//! optimizer does before it plans the read of the collection.

use bson::{doc, Bson, Document};

/// The field paths a filter clause reads, or `None` when it may read the
/// whole document (`$where`, `$jsonSchema`, `$$ROOT`, an operator this does
/// not know).
/// The field paths an aggregation expression reads, or `None` when it reads
/// the whole document (`$$ROOT`, `$$CURRENT`).
pub(crate) fn expr_paths(v: &Bson, out: &mut Vec<String>) -> Option<()> {
    match v {
        Bson::String(s) if s.starts_with("$$") => {
            let mut parts = s[2..].splitn(2, '.');
            let var = parts.next().unwrap_or("");
            if var == "ROOT" || var == "CURRENT" {
                // `$$CURRENT.x` reads `x`; the bare variable, everything.
                out.push(parts.next()?.to_string());
            }
            Some(())
        }
        Bson::String(s) if s.starts_with('$') => {
            out.push(s[1..].to_string());
            Some(())
        }
        Bson::Array(items) => items.iter().try_for_each(|i| expr_paths(i, out)),
        Bson::Document(d) if d.contains_key("$literal") => Some(()),
        // `distinct` through a view reads its key by name.
        Bson::Document(d) if d.len() == 1 && d.contains_key("$_internalFindAllValuesAtPath") => {
            match d.get("$_internalFindAllValuesAtPath") {
                Some(Bson::String(path)) => {
                    out.push(path.clone());
                    Some(())
                }
                _ => None,
            }
        }
        Bson::Document(d) => d.values().try_for_each(|i| expr_paths(i, out)),
        _ => Some(()),
    }
}

fn clause_paths(key: &str, value: &Bson, out: &mut Vec<String>) -> Option<()> {
    match key {
        "$and" | "$or" | "$nor" => value
            .as_array()?
            .iter()
            .try_for_each(|c| filter_paths(c.as_document()?, out)),
        "$expr" => expr_paths(value, out),
        "$comment" | "$alwaysTrue" | "$alwaysFalse" => Some(()),
        k if k.starts_with('$') => None,
        k => {
            out.push(k.to_string());
            Some(())
        }
    }
}

pub(crate) fn filter_paths(filter: &Document, out: &mut Vec<String>) -> Option<()> {
    filter.iter().try_for_each(|(k, v)| clause_paths(k, v, out))
}

/// Whether two dotted paths name the same field or one inside the other.
fn paths_overlap(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    long == short || long.starts_with(&format!("{short}."))
}

fn as_sort(stage: &Bson) -> Option<&Document> {
    let d = stage.as_document().filter(|d| d.len() == 1)?;
    d.get_document("$sort")
        .or_else(|_| d.get_document(super::shape::WINDOW_SORT))
        .ok()
}

pub(crate) fn as_match(stage: &Bson) -> Option<&Document> {
    let d = stage.as_document().filter(|d| d.len() == 1)?;
    d.get_document("$match")
        .or_else(|_| d.get_document(PUSHED_MATCH))
        .ok()
}

/// A `$match` mongod moved ahead of a `$replaceRoot` and rewrote, which
/// `explain` lists as it stands -- its clauses in the order the rewrite left
/// them, not sorted again -- and which runs as the `$match` it is.
pub(crate) const PUSHED_MATCH: &str = "$_secantusPushedMatch";

/// Whether a stage is a [`PUSHED_MATCH`].
fn is_pushed(stage: &Bson) -> bool {
    stage
        .as_document()
        .is_some_and(|d| d.contains_key(PUSHED_MATCH))
}

/// The field a `$replaceRoot` / `$replaceWith` makes the document, when its
/// new root is a plain field path (`"$x.y"`; not `$$ROOT`, not a computed
/// document).
fn new_root_path(stage: &Bson) -> Option<&str> {
    let d = stage.as_document().filter(|d| d.len() == 1)?;
    let root = match (d.get_document("$replaceRoot"), d.get("$replaceWith")) {
        (Ok(spec), _) => spec.get_str("newRoot").ok()?,
        (_, Some(Bson::String(root))) => root.as_str(),
        _ => return None,
    };
    root.strip_prefix('$')
        .filter(|p| !p.is_empty() && !p.starts_with('$'))
}

/// A stage that turns each document into one other (mongod's
/// `DocumentSourceSingleDocumentTransformation`), which a `$skip` behind it
/// moves ahead of.
fn transforms_one_by_one(stage: &Bson) -> bool {
    stage
        .as_document()
        .filter(|d| d.len() == 1)
        .and_then(|d| d.keys().next())
        .is_some_and(|k| {
            matches!(
                k.as_str(),
                "$project" | "$addFields" | "$set" | "$unset" | "$replaceRoot" | "$replaceWith"
            )
        })
}

fn is_skip(stage: &Bson) -> bool {
    stage
        .as_document()
        .is_some_and(|d| d.len() == 1 && d.contains_key("$skip"))
}

/// Whether mongod renames a parsed filter onto the field a `$replaceRoot`
/// reads (measured 8.2.11): not one that is only `$alwaysTrue` or
/// `$alwaysFalse`, nor one that holds an `$expr`, an `$elemMatch` or a
/// clause that is not on a path.
fn renameable(parsed: &Document) -> bool {
    fn clause(key: &str, value: &Bson) -> bool {
        match key {
            "$and" | "$or" | "$nor" => value.as_array().is_some_and(|cs| {
                cs.iter().all(|c| {
                    c.as_document()
                        .is_some_and(|d| d.iter().all(|(k, v)| clause(k, v)))
                })
            }),
            "$alwaysTrue" | "$alwaysFalse" => true,
            k if k.starts_with('$') => false,
            _ => !holds_elem_match(value),
        }
    }
    fn holds_elem_match(value: &Bson) -> bool {
        match value {
            Bson::Document(d) => d
                .iter()
                .any(|(k, v)| k == "$elemMatch" || (k == "$not" && holds_elem_match(v))),
            _ => false,
        }
    }
    let lone_constant = parsed.len() == 1
        && (parsed.contains_key("$alwaysTrue") || parsed.contains_key("$alwaysFalse"));
    !lone_constant && parsed.iter().all(|(k, v)| clause(k, v))
}

/// `filter` with each path under `root`.
fn under_root(filter: &Document, root: &str) -> Document {
    filter
        .iter()
        .map(|(k, v)| match (k.as_str(), v) {
            ("$and" | "$or" | "$nor", Bson::Array(cs)) => (
                k.clone(),
                Bson::Array(
                    cs.iter()
                        .map(|c| match c {
                            Bson::Document(d) => Bson::Document(under_root(d, root)),
                            other => other.clone(),
                        })
                        .collect(),
                ),
            ),
            (k, _) if k.starts_with('$') => (k.to_string(), v.clone()),
            (k, _) => (format!("{root}.{k}"), v.clone()),
        })
        .collect()
}

/// A `$match` behind a `$replaceRoot` whose new root is the field `root`,
/// as mongod 8.2 moves it ahead (`ReplaceRootTransformation`, measured
/// 8.2.11): the filter, parsed, reads its paths under `root`, in an `$or`
/// with the tests that let through what the `$replaceRoot` then refuses (an
/// array, or not a document) so that the error still comes. The `$or` takes
/// in the branches of a renamed `$or`, and its equalities and regexes go
/// last (mongod's `$or` optimisation keeps them for an `$in`). An empty
/// filter moves as it is; `None`: it stays behind. `pushed`: the filter is
/// one an earlier `$replaceRoot` already rewrote.
fn through_replace_root(filter: &Document, pushed: bool, root: &str) -> Option<Document> {
    let filter = if pushed {
        filter.clone()
    } else {
        super::parsed::parsed_query(filter)
    };
    if filter.is_empty() {
        return Some(filter);
    }
    if !pushed && !renameable(&filter) {
        return None;
    }
    let renamed = under_root(&filter, root);
    let mut branches: Vec<Document> = match renamed.get_array("$or") {
        Ok(cs) if renamed.len() == 1 => cs.iter().filter_map(Bson::as_document).cloned().collect(),
        _ => vec![renamed],
    };
    branches.push(doc! { root: { "$type": [4] } });
    branches.push(doc! { root: { "$not": { "$type": [3] } } });
    let (equalities, mut out): (Vec<Document>, Vec<Document>) = branches
        .into_iter()
        .partition(super::parsed::is_or_equality);
    out.extend(equalities);
    Some(doc! { "$or": out.into_iter().map(Bson::Document).collect::<Vec<_>>() })
}

/// What a stage a `$match` can move ahead of does to the fields.
enum Reshape {
    /// It writes or removes these paths and leaves the rest.
    Writes(Vec<String>),
    /// It keeps only these paths (an inclusion `$project`), and names these
    /// fields for others (`{w: "$x"}`: `w` is `x`).
    Keeps {
        kept: Vec<String>,
        renamed: Vec<(String, String)>,
    },
    /// A `$group`: it keeps no field, its key's plain field paths are
    /// renames, and `id_fields` are the key's own paths (`_id`, or `_id.k`
    /// for each field of a document key).
    Groups {
        renamed: Vec<(String, String)>,
        id_fields: Vec<String>,
    },
}

/// The renames a `$group` key makes (mongod 8.2
/// `ExpressionFieldPath::getComputedPaths`): a field path of one component
/// (`$x`, `$$ROOT.x`, `$$CURRENT.x`) names the field it reads, inside a
/// document key too; a dotted `$a.b` can reshape arrays and is no rename.
fn key_renames(path: &str, key: &Bson, out: &mut Vec<(String, String)>) {
    match key {
        Bson::String(s) => {
            let field = s
                .strip_prefix("$$CURRENT.")
                .or_else(|| s.strip_prefix("$$ROOT."))
                .or_else(|| s.strip_prefix('$').filter(|f| !f.starts_with('$')));
            if let Some(f) = field.filter(|f| !f.is_empty() && !f.contains('.')) {
                out.push((path.to_string(), f.to_string()));
            }
        }
        Bson::Document(d) if !is_operator(d) => {
            for (k, v) in d {
                key_renames(&format!("{path}.{k}"), v, out);
            }
        }
        _ => {}
    }
}

/// Whether a document is an operator expression rather than a document of
/// fields: its first field names an operator.
fn is_operator(d: &Document) -> bool {
    d.keys().next().is_some_and(|k| k.starts_with('$'))
}

/// A `$group`'s effect on the fields (mongod 8.2
/// `DocumentSourceGroupBase::getModifiedPaths`).
fn groups(stage: &Bson) -> Option<Reshape> {
    let id = stage
        .as_document()
        .filter(|d| d.len() == 1)?
        .get_document("$group")
        .ok()?
        .get("_id")?;
    let mut renamed = Vec::new();
    key_renames("_id", id, &mut renamed);
    let id_fields = match id {
        Bson::Document(d) if !d.is_empty() && !is_operator(d) => {
            d.keys().map(|k| format!("_id.{k}")).collect()
        }
        _ => vec!["_id".to_string()],
    };
    Some(Reshape::Groups { renamed, id_fields })
}

/// Whether a filter holds one of `ops` on a path at or under `under`'s
/// (mongod 8.2 `expression::hasPredicateOnPaths`: an `$elemMatch` carries
/// its path down to the predicates inside it).
fn has_predicate(
    filter: &Document,
    prefix: Option<&str>,
    ops: &[&str],
    under: &dyn Fn(&str) -> bool,
) -> bool {
    filter.iter().any(|(k, v)| match k.as_str() {
        "$and" | "$or" | "$nor" => v.as_array().is_some_and(|cs| {
            cs.iter()
                .filter_map(Bson::as_document)
                .any(|c| has_predicate(c, prefix, ops, under))
        }),
        k if k.starts_with('$') => false,
        k => {
            let path = prefix.map_or_else(|| k.to_string(), |p| format!("{p}.{k}"));
            value_has_predicate(&path, v, ops, under)
        }
    })
}

fn value_has_predicate(path: &str, v: &Bson, ops: &[&str], under: &dyn Fn(&str) -> bool) -> bool {
    let Some(d) = v.as_document().filter(|d| is_operator(d)) else {
        return false;
    };
    d.iter().any(|(op, arg)| match op.as_str() {
        "$not" => value_has_predicate(path, arg, ops, under),
        "$elemMatch" => match arg.as_document() {
            Some(inner) if is_operator(inner) => value_has_predicate(path, arg, ops, under),
            Some(inner) => has_predicate(inner, Some(path), ops, under),
            None => false,
        },
        op => ops.contains(&op) && under(path),
    })
}

/// Whether mongod refuses to move any of a `$match` ahead of a `$group`
/// (8.2 `groupMatchSwapVerified`): a `$type` on the key can tell apart
/// values the group takes as one, and an `$exists` on a key of one field
/// cannot see the null a missing field groups as.
fn group_refuses(filter: &Document, id_fields: &[String]) -> bool {
    let under = |p: &str| {
        id_fields
            .iter()
            .any(|f| p == f || p.starts_with(&format!("{f}.")))
    };
    let ops: &[&str] = if id_fields.len() == 1 {
        &["$type", "$exists"]
    } else {
        &["$type"]
    };
    has_predicate(filter, None, ops, &under)
}

/// Clauses as one filter: one document where their keys differ, else an
/// `$and` of them.
fn conjoin(parts: Vec<Document>) -> Document {
    let mut one = Document::new();
    for (k, v) in parts.iter().flatten() {
        if one.insert(k.clone(), v.clone()).is_some() {
            return doc! { "$and": parts.into_iter().map(Bson::Document).collect::<Vec<_>>() };
        }
    }
    one
}

/// A whole filter as `movable` moves it, or `None` when any clause stays.
fn move_whole(
    filter: &Document,
    movable: &dyn Fn(&str, &Bson) -> Option<Document>,
) -> Option<Document> {
    let parts = filter
        .iter()
        .map(|(k, v)| movable(k, v))
        .collect::<Option<Vec<_>>>()?;
    Some(conjoin(parts))
}

/// A `$match` clause split into what moves ahead of a stage and what stays
/// (mongod 8.2 `splitMatchExpressionByFunction`): an `$and`'s children
/// split one by one, a `$nor`'s wholly movable children move as a `$nor`
/// of their own, and anything else moves whole or not at all.
fn split_clause(
    key: &str,
    value: &Bson,
    movable: &dyn Fn(&str, &Bson) -> Option<Document>,
    moved: &mut Vec<Document>,
    kept: &mut Vec<Document>,
) {
    let children = value
        .as_array()
        .and_then(|cs| cs.iter().map(Bson::as_document).collect::<Option<Vec<_>>>());
    match (key, children) {
        ("$and", Some(cs)) => {
            for c in cs {
                for (k, v) in c {
                    split_clause(k, v, movable, moved, kept);
                }
            }
        }
        ("$nor", Some(cs)) => {
            let (mut go, mut stay) = (Vec::new(), Vec::new());
            for c in cs {
                match move_whole(c, movable) {
                    Some(m) => go.push(Bson::Document(m)),
                    None => stay.push(Bson::Document(c.clone())),
                }
            }
            if !go.is_empty() {
                moved.push(doc! { "$nor": go });
            }
            if !stay.is_empty() {
                kept.push(doc! { "$nor": stay });
            }
        }
        _ => match movable(key, value) {
            Some(clause) => moved.push(clause),
            None => kept.push(doc! { key: value.clone() }),
        },
    }
}

/// An aggregation expression as it reads ahead of a stage that keeps `kept`
/// and renames `renamed`, or `None` when it reads something the stage does
/// not keep (mongod 8.2 `ExpressionFieldPath::copyWithSubstitution`).
fn expr_through_projection(
    v: &Bson,
    kept: &[String],
    renamed: &[(String, String)],
) -> Option<Bson> {
    let within = |r: &str, p: &str| r == p || r.starts_with(&format!("{p}."));
    let path_to = |p: &str| -> Option<String> {
        if kept.iter().any(|k| within(p, k)) {
            return Some(p.to_string());
        }
        let (new, old) = renamed.iter().find(|(new, _)| within(p, new))?;
        Some(format!("{old}{}", &p[new.len()..]))
    };
    match v {
        Bson::String(s) if s.starts_with("$$") => {
            let rest = &s[2..];
            let (var, path) = match rest.split_once('.') {
                Some((var, path)) => (var, Some(path)),
                None => (rest, None),
            };
            if var != "ROOT" && var != "CURRENT" {
                return Some(v.clone());
            }
            let path = path?;
            let to = path_to(path)?;
            Some(if to == path {
                v.clone()
            } else {
                Bson::String(format!("${to}"))
            })
        }
        Bson::String(s) if s.starts_with('$') => {
            Some(Bson::String(format!("${}", path_to(&s[1..])?)))
        }
        Bson::Array(items) => items
            .iter()
            .map(|i| expr_through_projection(i, kept, renamed))
            .collect::<Option<Vec<_>>>()
            .map(Bson::Array),
        Bson::Document(d) if d.contains_key("$literal") => Some(v.clone()),
        Bson::Document(d) if d.contains_key("$_internalFindAllValuesAtPath") => {
            let mut read = Vec::new();
            expr_paths(v, &mut read)?;
            read.iter()
                .all(|r| kept.iter().any(|k| within(r, k)))
                .then(|| v.clone())
        }
        Bson::Document(d) => {
            let mut out = Document::new();
            for (k, i) in d {
                out.insert(k.clone(), expr_through_projection(i, kept, renamed)?);
            }
            Some(Bson::Document(out))
        }
        _ => Some(v.clone()),
    }
}

fn reshapes(stage: &Bson) -> Option<Reshape> {
    let d = stage.as_document().filter(|d| d.len() == 1)?;
    if let Some(unwind) = d.get("$unwind") {
        return unwind_paths(unwind).map(Reshape::Writes);
    }
    if let Ok(spec) = d
        .get_document("$addFields")
        .or_else(|_| d.get_document("$set"))
    {
        return Some(Reshape::Writes(spec.keys().cloned().collect()));
    }
    match d.get("$unset")? {
        Bson::String(p) => return Some(Reshape::Writes(vec![p.clone()])),
        Bson::Array(ps) => {
            return ps
                .iter()
                .map(|p| p.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .map(Reshape::Writes)
        }
        _ => {}
    }
    None
}

/// A `$project`'s effect on the fields (measured 8.2.11 by `explain`: a
/// `$match` moves ahead of one that keeps every field it reads, and onto
/// the field a rename names).
fn projects(stage: &Bson) -> Option<Reshape> {
    let spec = stage
        .as_document()
        .filter(|d| d.len() == 1)?
        .get_document("$project")
        .ok()?;
    let flag = |v: &Bson| match v {
        Bson::Boolean(b) => Some(*b),
        Bson::Int32(n) => Some(*n != 0),
        Bson::Int64(n) => Some(*n != 0),
        Bson::Double(n) => Some(*n != 0.0),
        _ => None,
    };
    let excluding = spec
        .iter()
        .filter(|(k, _)| k.as_str() != "_id")
        .all(|(_, v)| flag(v) == Some(false));
    if excluding {
        let removed = spec
            .iter()
            .filter(|(_, v)| flag(v) == Some(false))
            .map(|(k, _)| k.clone())
            .collect();
        return Some(Reshape::Writes(removed));
    }
    let mut kept = Vec::new();
    let mut renamed = Vec::new();
    // `_id` is kept unless the projection drops it.
    if !spec.contains_key("_id") {
        kept.push("_id".to_string());
    }
    for (k, v) in spec {
        match (flag(v), v) {
            (Some(true), _) => kept.push(k.clone()),
            (Some(false), _) => {}
            (None, Bson::String(path)) if path.starts_with('$') && !path.starts_with("$$") => {
                renamed.push((k.clone(), path[1..].to_string()));
            }
            // A computed field is the stage's own.
            _ => {}
        }
    }
    Some(Reshape::Keeps { kept, renamed })
}

/// `clause` as it reads ahead of a stage that keeps `kept` and renames
/// `renamed`, or `None` when it reads something the stage does not keep.
fn through_projection(
    key: &str,
    value: &Bson,
    kept: &[String],
    renamed: &[(String, String)],
) -> Option<Document> {
    let within = |r: &str, p: &str| r == p || r.starts_with(&format!("{p}."));
    if !key.starts_with('$') {
        if kept.iter().any(|p| within(key, p)) {
            return Some(doc! { key: value.clone() });
        }
        // A rename: the clause reads the field it names.
        let (new, old) = renamed.iter().find(|(new, _)| within(key, new))?;
        return Some(doc! { format!("{old}{}", &key[new.len()..]): value.clone() });
    }
    // A logical clause moves whole, its children renamed (mongod 8.2
    // `applyRenamesToExpression`); so does an `$expr`.
    match key {
        "$and" | "$or" | "$nor" => {
            let children = value
                .as_array()?
                .iter()
                .map(|c| {
                    move_whole(c.as_document()?, &|k, v| {
                        through_projection(k, v, kept, renamed)
                    })
                    .map(Bson::Document)
                })
                .collect::<Option<Vec<_>>>()?;
            return Some(doc! { key: children });
        }
        "$expr" => {
            return expr_through_projection(value, kept, renamed).map(|e| doc! { "$expr": e });
        }
        _ => {}
    }
    let mut read = Vec::new();
    clause_paths(key, value, &mut read)?;
    read.iter()
        .all(|r| kept.iter().any(|p| within(r, p)))
        .then(|| doc! { key: value.clone() })
}

/// The pipeline as mongod reorders its front: a `$match` behind an
/// `$addFields` / `$set` moves ahead of it, the clauses that read nothing
/// the stage writes, and the `$match`es that lead are joined into one, which
/// the read of the collection then takes (measured 8.2.11 by `explain`:
/// `[{$addFields: {z: 1}}, {$match: {b: 2, z: 1}}]` reads `{b: 2}` and leaves
/// `{z: 1}` behind the stage), and one behind a `$group` moves ahead of it
/// onto the fields its key renames (RM-311).
pub(crate) fn hoist_matches(pipeline: &[Bson]) -> Vec<Bson> {
    hoist(pipeline, false)
}

/// The stages after a pipeline's own source (`$documents`, which is a
/// `$replaceRoot` from `_tempDocumentsField` by then, `$queryStats`, ...)
/// as mongod reorders them: as [`hoist_matches`] does, and besides, a
/// `$match` moves ahead of a `$replaceRoot` of a field path, rewritten
/// ([`through_replace_root`]), and a `$skip` ahead of a stage that
/// transforms each document into one (measured 8.2.11, RM-414).
pub(crate) fn hoist_after_source(pipeline: &[Bson]) -> Vec<Bson> {
    hoist(pipeline, true)
}

fn hoist(pipeline: &[Bson], after_source: bool) -> Vec<Bson> {
    let mut out = pipeline.to_vec();
    // A `$skip` / `$limit` is a wall a `$match` never crosses, but mongod
    // still moves one behind it ahead of the stages after it (8.2.11:
    // `[{$limit: 3}, {$project: {x: 1}}, {$match: {x: 1}}]` lists `$match`
    // then `$project`; RM-308).
    let paging = |s: &Bson| {
        s.as_document()
            .filter(|d| d.len() == 1)
            .is_some_and(|d| d.contains_key("$skip") || d.contains_key("$limit"))
    };
    let leading = |s: &Bson| {
        paging(s)
            || as_match(s).is_some()
            || reshapes(s).is_some()
            || projects(s).is_some()
            || groups(s).is_some()
            || as_sort(s).is_some()
            || (after_source && transforms_one_by_one(s))
    };
    'again: loop {
        for at in 1..out.len() {
            if !out[..at].iter().all(leading) {
                break;
            }
            // A `$sort` on fields an `$unwind` leaves alone goes ahead of
            // it (measured 8.2.11: `[{$unwind: "$t"}, {$sort: {a: 1}}]`
            // reads `a_1` and lists only the `$unwind`).
            let unwound = out[at - 1]
                .as_document()
                .filter(|d| d.len() == 1)
                .and_then(|d| d.get("$unwind"))
                .and_then(unwind_paths);
            if let (Some(unwound), Some(sort)) = (unwound, as_sort(&out[at])) {
                let free = sort
                    .keys()
                    .all(|k| !unwound.iter().any(|w| paths_overlap(k, w)));
                if free {
                    out.swap(at - 1, at);
                    continue 'again;
                }
            }
            if after_source && is_skip(&out[at]) && transforms_one_by_one(&out[at - 1]) {
                out.swap(at - 1, at);
                continue 'again;
            }
            let Some(m) = as_match(&out[at]) else {
                continue;
            };
            let pushed = is_pushed(&out[at]);
            if let Some(root) = new_root_path(&out[at - 1]).filter(|_| after_source) {
                let Some(moved) = through_replace_root(m, pushed, root) else {
                    continue;
                };
                out[at] = out[at - 1].clone();
                out[at - 1] = Bson::Document(doc! { PUSHED_MATCH: moved });
                continue 'again;
            }
            // A rewritten `$match` stays one as it moves on.
            let key = if pushed { PUSHED_MATCH } else { "$match" };
            // A `$match` behind a `$sort` goes ahead of it (measured 8.2.11:
            // `[{$project: {x: 1}}, {$sort: {x: 1}}, {$match: {x: 1}}]`
            // reads `{x: 1}`).
            if as_sort(&out[at - 1]).is_some() {
                out.swap(at - 1, at);
                continue 'again;
            }
            let Some(reshape) = reshapes(&out[at - 1])
                .or_else(|| projects(&out[at - 1]))
                .or_else(|| groups(&out[at - 1]))
            else {
                continue;
            };
            if let Reshape::Groups { id_fields, .. } = &reshape {
                if group_refuses(m, id_fields) {
                    continue;
                }
            }
            let movable = |k: &str, v: &Bson| match &reshape {
                Reshape::Writes(written) => {
                    let mut read = Vec::new();
                    (clause_paths(k, v, &mut read).is_some()
                        && !read
                            .iter()
                            .any(|r| written.iter().any(|w| paths_overlap(r, w))))
                    .then(|| doc! { k: v.clone() })
                }
                Reshape::Keeps {
                    kept: names,
                    renamed,
                } => through_projection(k, v, names, renamed),
                Reshape::Groups { renamed, .. } => through_projection(k, v, &[], renamed),
            };
            let mut moved = Vec::new();
            let mut kept = Vec::new();
            for (k, v) in m {
                split_clause(k, v, &movable, &mut moved, &mut kept);
            }
            if moved.is_empty() {
                continue;
            }
            let joined = |mut parts: Vec<Document>| match parts.len() {
                1 => parts.remove(0),
                _ => doc! { "$and": parts.into_iter().map(Bson::Document).collect::<Vec<_>>() },
            };
            let mut replaced = vec![
                Bson::Document(
                    // Every clause moved: one filter, as the `$match` wrote
                    // it (a rename that names a field twice stays a
                    // conjunction).
                    doc! { key: if kept.is_empty() { conjoin(moved) } else { joined(moved) } },
                ),
                out[at - 1].clone(),
            ];
            if !kept.is_empty() {
                replaced.push(Bson::Document(doc! { key: joined(kept) }));
            }
            out.splice(at - 1..=at, replaced);
            continue 'again;
        }
        break;
    }
    // The `$match`es at the front, as one.
    let lead = out.iter().take_while(|s| as_match(s).is_some()).count();
    if lead > 1 {
        let key = if out[..lead].iter().any(is_pushed) {
            PUSHED_MATCH
        } else {
            "$match"
        };
        let parts: Vec<Document> = out[..lead]
            .iter()
            .filter_map(as_match)
            .filter(|m| !m.is_empty())
            .cloned()
            .collect();
        let joined = match parts.len() {
            0 => Document::new(),
            1 => parts.into_iter().next().unwrap_or_default(),
            _ => doc! { "$and": parts.into_iter().map(Bson::Document).collect::<Vec<_>>() },
        };
        out.splice(..lead, [Bson::Document(doc! { key: joined })]);
    }
    out
}

/// The paths an `$unwind` writes: its array, and the index field it names.
pub(crate) fn unwind_paths(unwind: &Bson) -> Option<Vec<String>> {
    let (path, index) = match unwind {
        Bson::String(p) => (p.as_str(), None),
        Bson::Document(spec) => (
            spec.get_str("path").ok()?,
            spec.get_str("includeArrayIndex").ok(),
        ),
        _ => return None,
    };
    let mut out = vec![path.strip_prefix('$')?.to_string()];
    out.extend(index.map(str::to_string));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{hoist_after_source, hoist_matches, PUSHED_MATCH};
    use bson::{doc, Bson};

    fn root() -> Bson {
        doc! { "$replaceRoot": { "newRoot": "$_tempDocumentsField" } }.into()
    }

    fn guarded(clauses: Vec<Bson>) -> Bson {
        doc! { PUSHED_MATCH: { "$or": clauses } }.into()
    }

    /// The order mongod 8.2.11 lists (RM-414): the renamed filter, the two
    /// tests, and equalities last.
    #[test]
    fn a_match_moves_ahead_of_a_replace_root_renamed() {
        let t4 = Bson::from(doc! { "_tempDocumentsField": { "$type": [4] } });
        let n3 = Bson::from(doc! { "_tempDocumentsField": { "$not": { "$type": [3] } } });
        let eq =
            |f: &str, v: i32| Bson::from(doc! { format!("_tempDocumentsField.{f}"): { "$eq": v } });
        assert_eq!(
            hoist_after_source(&[root(), doc! { "$match": { "a": 1 } }.into()]),
            vec![guarded(vec![t4.clone(), n3.clone(), eq("a", 1)]), root()]
        );
        assert_eq!(
            hoist_after_source(&[root(), doc! { "$match": { "a": { "$gte": 1 } } }.into()]),
            vec![
                guarded(vec![
                    doc! { "_tempDocumentsField.a": { "$gte": 1 } }.into(),
                    t4.clone(),
                    n3.clone(),
                ]),
                root()
            ]
        );
        // Two `$match`es each move, then join.
        assert_eq!(
            hoist_after_source(&[
                root(),
                doc! { "$match": { "a": 1 } }.into(),
                doc! { "$match": { "b": 2 } }.into(),
            ]),
            vec![
                doc! { PUSHED_MATCH: { "$and": [
                    { "$or": [t4.clone(), n3.clone(), eq("a", 1)] },
                    { "$or": [t4, n3, eq("b", 2)] },
                ] } }
                .into(),
                root()
            ]
        );
    }

    #[test]
    fn what_stays_behind_a_replace_root() {
        for filter in [
            doc! { "$expr": { "$eq": ["$a", 1] } },
            doc! { "a": { "$elemMatch": { "$gt": 1 } } },
            doc! { "$alwaysFalse": 1 },
        ] {
            let pipeline = vec![root(), doc! { "$match": filter.clone() }.into()];
            assert_eq!(hoist_after_source(&pipeline), pipeline, "{filter}");
        }
        // A new root that is not a field path takes no `$match`.
        let pipeline = vec![
            doc! { "$replaceRoot": { "newRoot": { "z": "$a" } } }.into(),
            doc! { "$match": { "z": 1 } }.into(),
        ];
        assert_eq!(hoist_after_source(&pipeline), pipeline);
    }

    #[test]
    fn a_skip_moves_ahead_of_a_transform_but_not_a_match() {
        let skip = Bson::from(doc! { "$skip": 1 });
        let project = Bson::from(doc! { "$project": { "a": 1 } });
        assert_eq!(
            hoist_after_source(&[root(), project.clone(), skip.clone()]),
            vec![skip.clone(), root(), project.clone()]
        );
        // Over a collection the pipeline keeps its order.
        assert_eq!(
            hoist_matches(&[root(), project.clone(), skip.clone()]),
            vec![root(), project, skip]
        );
    }
}
