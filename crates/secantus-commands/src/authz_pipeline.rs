//! The privileges a pipeline, a `mapReduce` and a `bulkWrite` need, stage
//! by stage and op by op, as mongod's `LiteParsed` stages declare them
//! (measured on 8.2.11 with `tools/probes/auth_enforcement.py`, 2026-10-14).

use bson::{Bson, Document};

use crate::privileges::Resource;

/// Actions needed on one resource.
pub(crate) type Need = (Resource, Vec<&'static str>);

pub(crate) fn ns(db: &str, collection: &str) -> Resource {
    Resource::Ns {
        db: db.to_string(),
        collection: collection.to_string(),
    }
}

/// `{db: "", collection: ""}`: every normal collection of every normal
/// database, the target of a check over the whole deployment.
pub(crate) fn any_normal() -> Resource {
    ns("", "")
}

fn need(needs: &mut Vec<Need>, resource: Resource, actions: &[&'static str]) {
    needs.push((resource, actions.to_vec()));
}

/// A stage's target namespace: a string names a collection of `db`, a
/// document names `{db, coll}` (`$out` / `$merge.into`).
fn target_of(spec: &Bson, db: &str) -> Option<Resource> {
    match spec {
        Bson::String(coll) => Some(ns(db, coll)),
        Bson::Document(d) => Some(ns(d.get_str("db").unwrap_or(db), d.get_str("coll").ok()?)),
        _ => None,
    }
}

fn truthy(value: Option<&Bson>) -> bool {
    value
        .and_then(crate::util::truthy_number_or_bool)
        .unwrap_or(false)
}

/// What the stages of `pipeline` need beyond the read of the collection
/// they run on: `$out` and `$merge` write their target, the `$lookup`
/// family reads its foreign collection (and whatever its own pipeline
/// needs), `$collStats` / `$indexStats` / `$planCacheStats` read the
/// collection's own statistics, `$currentOp` and `$listLocalSessions` need
/// the cluster only to see other users', and `$changeStream` needs
/// `changeStream` and `find` on what it watches.
pub(crate) fn walk(
    pipeline: &[Bson],
    db: &str,
    collection: Option<&str>,
    bypass: bool,
    needs: &mut Vec<Need>,
) {
    let own = || collection.map(|c| ns(db, c));
    for stage in pipeline.iter().filter_map(Bson::as_document) {
        let Some((op, spec)) = stage.iter().next() else {
            continue;
        };
        match op.as_str() {
            "$out" => {
                if let Some(target) = target_of(spec, db) {
                    let mut actions = vec!["insert", "remove"];
                    if bypass {
                        actions.push("bypassDocumentValidation");
                    }
                    need(needs, target, &actions);
                }
            }
            "$merge" => {
                let into = match spec {
                    Bson::Document(d) => d.get("into"),
                    other => Some(other),
                };
                if let Some(target) = into.and_then(|i| target_of(i, db)) {
                    let mut actions = vec!["insert", "update"];
                    if bypass {
                        actions.push("bypassDocumentValidation");
                    }
                    need(needs, target, &actions);
                }
            }
            "$lookup" | "$graphLookup" => {
                if let Bson::Document(d) = spec {
                    if let Some(from) = d.get("from").and_then(|f| target_of(f, db)) {
                        need(needs, from, &["find"]);
                    }
                    if let Some(Bson::Array(sub)) = d.get("pipeline") {
                        walk(sub, db, collection, bypass, needs);
                    }
                }
            }
            "$unionWith" => match spec {
                Bson::String(coll) => need(needs, ns(db, coll), &["find"]),
                Bson::Document(d) => {
                    if let Ok(coll) = d.get_str("coll") {
                        need(needs, ns(db, coll), &["find"]);
                    }
                    if let Some(Bson::Array(sub)) = d.get("pipeline") {
                        walk(sub, db, collection, bypass, needs);
                    }
                }
                _ => {}
            },
            "$facet" => {
                if let Bson::Document(d) = spec {
                    for (_, branch) in d {
                        if let Bson::Array(sub) = branch {
                            walk(sub, db, collection, bypass, needs);
                        }
                    }
                }
            }
            "$collStats" => {
                if let Some(target) = own() {
                    need(needs, target, &["collStats"]);
                }
            }
            "$indexStats" => {
                if let Some(target) = own() {
                    need(needs, target, &["indexStats"]);
                }
            }
            "$planCacheStats" => {
                if let Some(target) = own() {
                    need(needs, target, &["planCacheRead"]);
                }
            }
            "$listSearchIndexes" => {
                if let Some(target) = own() {
                    need(needs, target, &["listSearchIndexes"]);
                }
            }
            "$currentOp" => {
                let all = spec.as_document().and_then(|d| d.get("allUsers"));
                if truthy(all) {
                    need(needs, Resource::Cluster, &["inprog"]);
                }
            }
            "$listSampledQueries" => need(needs, Resource::Cluster, &["listSampledQueries"]),
            "$listLocalSessions" | "$listSessions" => {
                let all = spec.as_document().and_then(|d| d.get("allUsers"));
                if truthy(all) {
                    need(needs, Resource::Cluster, &["listSessions"]);
                }
            }
            "$changeStream" => {
                let whole_cluster = spec
                    .as_document()
                    .and_then(|d| d.get("allChangesForCluster"));
                let target = match own() {
                    Some(target) => target,
                    None if truthy(whole_cluster) => any_normal(),
                    None => ns(db, ""),
                };
                need(needs, target, &["changeStream", "find"]);
            }
            _ => {}
        }
    }
}

/// What an `aggregate` needs: `find` on its collection unless the first
/// stage reads something else (`$collStats`, `$indexStats`), nothing for
/// the collection-less `aggregate: 1`, and then what every stage needs.
pub(crate) fn aggregate(doc: &Document, db: &str) -> Vec<Need> {
    let collection = doc.get_str("aggregate").ok();
    let bypass = truthy(doc.get("bypassDocumentValidation"));
    let pipeline: &[Bson] = match doc.get("pipeline") {
        Some(Bson::Array(stages)) => stages,
        _ => &[],
    };
    let mut needs = Vec::new();
    if let Some(coll) = collection {
        let first = pipeline
            .first()
            .and_then(Bson::as_document)
            .and_then(|s| s.keys().next())
            .map(String::as_str);
        let action = match first {
            Some("$collStats") => "collStats",
            Some("$indexStats") => "indexStats",
            _ => "find",
        };
        need(&mut needs, ns(db, coll), &[action]);
    }
    walk(pipeline, db, collection, bypass, &mut needs);
    needs
}

/// What a `mapReduce` needs: `find` on its input, and on an output
/// collection `insert` with `remove` (replace) or `update` (merge, reduce).
pub(crate) fn map_reduce(doc: &Document, db: &str) -> Vec<Need> {
    let mut needs = Vec::new();
    let input = doc
        .get_str("mapReduce")
        .or_else(|_| doc.get_str("mapreduce"));
    if let Ok(input) = input {
        need(&mut needs, ns(db, input), &["find"]);
    }
    let bypass = truthy(doc.get("bypassDocumentValidation"));
    let output = match doc.get("out") {
        Some(Bson::String(coll)) => Some((ns(db, coll), "remove")),
        Some(Bson::Document(out)) => {
            let out_db = out.get_str("db").unwrap_or(db);
            ["replace", "merge", "reduce"].iter().find_map(|mode| {
                out.get_str(mode).ok().map(|coll| {
                    (
                        ns(out_db, coll),
                        if *mode == "replace" {
                            "remove"
                        } else {
                            "update"
                        },
                    )
                })
            })
        }
        _ => None,
    };
    if let Some((target, action)) = output {
        let mut actions = vec!["insert", action];
        if bypass {
            actions.push("bypassDocumentValidation");
        }
        need(&mut needs, target, &actions);
    }
    needs
}

/// A `db.coll` namespace string's resource.
pub(crate) fn namespace(text: &str) -> Option<Resource> {
    let (db, coll) = text.split_once('.')?;
    Some(ns(db, coll))
}

/// What a `bulkWrite` needs: for each op, `insert` / `update` (with `insert`
/// for an upsert) / `remove` on the namespace it names, plus
/// `bypassDocumentValidation` on each when the command sets it.
pub(crate) fn bulk_write(doc: &Document) -> Vec<Need> {
    let bypass = truthy(doc.get("bypassDocumentValidation"));
    let namespaces: Vec<Option<Resource>> = match doc.get("nsInfo") {
        Some(Bson::Array(entries)) => entries
            .iter()
            .map(|e| e.as_document()?.get_str("ns").ok().and_then(namespace))
            .collect(),
        _ => Vec::new(),
    };
    let mut needs = Vec::new();
    let ops: &[Bson] = match doc.get("ops") {
        Some(Bson::Array(ops)) => ops,
        _ => &[],
    };
    for op in ops.iter().filter_map(Bson::as_document) {
        let Some((kind, index)) = op.iter().next() else {
            continue;
        };
        let Some(target) = crate::util::as_i64(index)
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| namespaces.get(i).cloned().flatten())
        else {
            continue;
        };
        let mut actions = match kind.as_str() {
            "insert" => vec!["insert"],
            "update" if truthy(op.get("upsert")) => vec!["update", "insert"],
            "update" => vec!["update"],
            "delete" => vec!["remove"],
            _ => continue,
        };
        if bypass {
            actions.push("bypassDocumentValidation");
        }
        need(&mut needs, target, &actions);
    }
    needs
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn has(needs: &[Need], resource: Resource, actions: &[&str]) -> bool {
        needs
            .iter()
            .any(|(r, a)| *r == resource && actions.iter().all(|x| a.contains(x)))
    }

    #[test]
    fn stages_name_their_own_namespaces() {
        let needs = aggregate(
            &doc! {"aggregate": "orders", "pipeline": [
                {"$match": {"v": 1}},
                {"$out": {"db": "warehouse", "coll": "t"}},
                {"$merge": {"into": "t2"}},
                {"$unionWith": {"coll": "c2", "pipeline": [
                    {"$merge": {"into": {"db": "x", "coll": "y"}}},
                ]}},
                {"$facet": {"branch": [
                    {"$lookup": {"from": "f", "localField": "a", "foreignField": "b", "as": "z"}},
                ]}},
            ]},
            "shop",
        );
        assert!(has(&needs, ns("shop", "orders"), &["find"]));
        assert!(has(&needs, ns("warehouse", "t"), &["insert", "remove"]));
        assert!(has(&needs, ns("shop", "t2"), &["insert", "update"]));
        assert!(has(&needs, ns("shop", "c2"), &["find"]));
        assert!(has(&needs, ns("x", "y"), &["insert", "update"]));
        assert!(has(&needs, ns("shop", "f"), &["find"]));
    }

    #[test]
    fn first_stage_replaces_find_and_collectionless_needs_none() {
        let stats = aggregate(
            &doc! {"aggregate": "c", "pipeline": [{"$collStats": {}}], "cursor": {}},
            "t",
        );
        assert!(has(&stats, ns("t", "c"), &["collStats"]));
        assert!(!has(&stats, ns("t", "c"), &["find"]));
        let own = aggregate(
            &doc! {"aggregate": 1, "pipeline": [{"$currentOp": {}}], "cursor": {}},
            "admin",
        );
        assert!(own.is_empty());
        let all = aggregate(
            &doc! {"aggregate": 1, "pipeline": [{"$currentOp": {"allUsers": true}}], "cursor": {}},
            "admin",
        );
        assert_eq!(all, vec![(Resource::Cluster, vec!["inprog"])]);
        let stream = aggregate(
            &doc! {"aggregate": 1, "pipeline": [{"$changeStream": {"allChangesForCluster": true}}]},
            "admin",
        );
        assert_eq!(stream, vec![(any_normal(), vec!["changeStream", "find"])]);
    }

    #[test]
    fn map_reduce_output_modes() {
        let replace = map_reduce(&doc! {"mapReduce": "c", "out": "o"}, "t");
        assert!(has(&replace, ns("t", "o"), &["insert", "remove"]));
        let merge = map_reduce(
            &doc! {"mapReduce": "c", "out": {"merge": "o", "db": "u"}},
            "t",
        );
        assert!(has(&merge, ns("u", "o"), &["insert", "update"]));
        assert!(!has(&merge, ns("u", "o"), &["remove"]));
        let inline = map_reduce(&doc! {"mapReduce": "c", "out": {"inline": 1}}, "t");
        assert_eq!(inline, vec![(ns("t", "c"), vec!["find"])]);
    }

    #[test]
    fn bulk_write_ops_resolve_their_namespace() {
        let needs = bulk_write(&doc! {
            "bulkWrite": 1,
            "ops": [
                {"insert": 0, "document": {}},
                {"update": 1, "filter": {}, "updateMods": {}, "upsert": true},
                {"delete": 0, "filter": {}},
            ],
            "nsInfo": [{"ns": "a.x"}, {"ns": "b.y"}],
            "bypassDocumentValidation": true,
        });
        assert!(has(
            &needs,
            ns("a", "x"),
            &["insert", "bypassDocumentValidation"]
        ));
        assert!(has(&needs, ns("b", "y"), &["update", "insert"]));
        assert!(has(&needs, ns("a", "x"), &["remove"]));
    }
}
