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
    stage
        .as_document()
        .filter(|d| d.len() == 1)?
        .get_document("$sort")
        .ok()
}

fn as_match(stage: &Bson) -> Option<&Document> {
    stage
        .as_document()
        .filter(|d| d.len() == 1)?
        .get_document("$match")
        .ok()
}

/// The paths an `$addFields` / `$set` writes, or an `$unwind` replaces.
fn set_paths(stage: &Bson) -> Option<Vec<String>> {
    let d = stage.as_document().filter(|d| d.len() == 1)?;
    if let Some(unwind) = d.get("$unwind") {
        return unwind_paths(unwind);
    }
    let spec = d
        .get_document("$addFields")
        .or_else(|_| d.get_document("$set"))
        .ok()?;
    Some(spec.keys().cloned().collect())
}

/// The pipeline as mongod reorders its front: a `$match` behind an
/// `$addFields` / `$set` moves ahead of it, the clauses that read nothing
/// the stage writes, and the `$match`es that lead are joined into one, which
/// the read of the collection then takes (measured 8.2.11 by `explain`:
/// `[{$addFields: {z: 1}}, {$match: {b: 2, z: 1}}]` reads `{b: 2}` and leaves
/// `{z: 1}` behind the stage).
pub(crate) fn hoist_matches(pipeline: &[Bson]) -> Vec<Bson> {
    let mut out = pipeline.to_vec();
    let leading =
        |s: &Bson| as_match(s).is_some() || set_paths(s).is_some() || as_sort(s).is_some();
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
            let (Some(written), Some(m)) = (set_paths(&out[at - 1]), as_match(&out[at])) else {
                continue;
            };
            let mut moved = Vec::new();
            let mut kept = Vec::new();
            for (k, v) in m {
                let mut read = Vec::new();
                let free = clause_paths(k, v, &mut read).is_some()
                    && !read
                        .iter()
                        .any(|r| written.iter().any(|w| paths_overlap(r, w)));
                if free { &mut moved } else { &mut kept }.push(doc! { k.clone(): v.clone() });
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
                    doc! { "$match": if kept.is_empty() { m.clone() } else { joined(moved) } },
                ),
                out[at - 1].clone(),
            ];
            if !kept.is_empty() {
                replaced.push(Bson::Document(doc! { "$match": joined(kept) }));
            }
            out.splice(at - 1..=at, replaced);
            continue 'again;
        }
        break;
    }
    // The `$match`es at the front, as one.
    let lead = out.iter().take_while(|s| as_match(s).is_some()).count();
    if lead > 1 {
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
        out.splice(..lead, [Bson::Document(doc! { "$match": joined })]);
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
