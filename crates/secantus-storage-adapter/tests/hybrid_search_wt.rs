//! `$score`, `$rankFusion` and `$scoreFusion` (RM-198), each answer as
//! mongod 8.2.11 gave it (measured 2026-10-20 over the same six documents).

mod common;

use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::{dispatch, CommandContext};

fn seed(c: &mut CommandContext) {
    let docs: Vec<Document> = (0..6)
        .map(|i| doc! {"_id": i, "x": i, "y": (i * 7) % 5})
        .collect();
    dispatch(&doc! {"insert": "s", "documents": docs}, c);
}

/// `(_id, score)` of each document `pipeline` returns, in order.
fn scored(c: &mut CommandContext, pipeline: Vec<Document>) -> Vec<(i32, f64)> {
    let mut pipeline: Vec<Bson> = pipeline.into_iter().map(Bson::Document).collect();
    pipeline.push(Bson::Document(
        doc! {"$project": {"_id": 1, "s": {"$meta": "score"}}},
    ));
    let reply = dispatch_full(
        &doc! {"aggregate": "s", "pipeline": pipeline, "cursor": {}},
        c,
    );
    reply
        .get_document("cursor")
        .and_then(|cur| cur.get_array("firstBatch"))
        .unwrap_or_else(|_| panic!("no batch in {reply}"))
        .iter()
        .map(|d| {
            let d = d.as_document().unwrap();
            (d.get_i32("_id").unwrap(), d.get_f64("s").unwrap())
        })
        .collect()
}

fn error(c: &mut CommandContext, pipeline: Vec<Document>) -> (i32, String) {
    let reply = dispatch(
        &doc! {"aggregate": "s", "pipeline": pipeline, "cursor": {}},
        c,
    );
    (
        reply.get_i32("code").unwrap_or(0),
        reply.get_str("errmsg").unwrap_or_default().to_string(),
    )
}

#[test]
fn score_normalises_and_weights_without_sorting() {
    with_wt(|c| {
        seed(c);
        let got = scored(
            c,
            vec![doc! {"$score": {"score": "$x", "normalization": "minMaxScaler", "weight": 0.5}}],
        );
        let want: Vec<(i32, f64)> = (0..6).map(|i| (i, f64::from(i) * 0.2 * 0.5)).collect();
        for ((gi, gs), (wi, ws)) in got.iter().zip(&want) {
            assert_eq!(gi, wi);
            assert!((gs - ws).abs() < 1e-12, "{got:?}");
        }
        let got = scored(
            c,
            vec![doc! {"$score": {"score": "$x", "normalization": "sigmoid"}}],
        );
        assert_eq!(got[1], (1, 0.7310585786300049));
    });
}

#[test]
fn rank_fusion_sums_reciprocal_ranks_ties_sharing_one() {
    with_wt(|c| {
        seed(c);
        let got = scored(
            c,
            vec![doc! {"$rankFusion": {"input": {"pipelines": {
                "a": [{"$sort": {"x": 1}}],
                "b": [{"$sort": {"y": -1}}],
            }}}}],
        );
        assert_eq!(
            got,
            vec![
                (2, 0.032266458495966696),
                (1, 0.03200204813108039),
                (0, 0.03177805800756621),
                (4, 0.0315136476426799),
                (3, 0.03125),
                // `y` ties at 0 for 0 and 5: both rank 5 in `b`.
                (5, 0.030536130536130537),
            ]
        );
    });
}

#[test]
fn score_fusion_averages_and_sorts_by_score_then_id() {
    with_wt(|c| {
        seed(c);
        let got = scored(
            c,
            vec![doc! {"$scoreFusion": {"input": {
                "pipelines": {
                    "a": [{"$score": {"score": "$x"}}],
                    "b": [{"$score": {"score": "$y"}}],
                },
                "normalization": "none",
            }}}],
        );
        assert_eq!(
            got,
            vec![(4, 3.5), (2, 3.0), (5, 2.5), (3, 2.0), (1, 1.5), (0, 0.0)]
        );
    });
}

#[test]
fn hybrid_stages_are_refused_as_mongod_refuses_them() {
    with_wt(|c| {
        seed(c);
        let cases: Vec<(Vec<Document>, i32)> = vec![
            (vec![doc! {"$project": {"s": {"$meta": "score"}}}], 40218),
            (
                vec![
                    doc! {"$match": {}},
                    doc! {"$rankFusion": {"input": {"pipelines": {"a": [{"$sort": {"x": 1}}]}}}},
                ],
                10170100,
            ),
            (
                vec![doc! {"$rankFusion": {"input": {"pipelines": {"a": [{"$match": {}}]}}}}],
                9191100,
            ),
            (
                vec![
                    doc! {"$scoreFusion": {"input": {"pipelines": {"a": [{"$score": {"score": "$x"}}]}}}},
                ],
                40414,
            ),
            (vec![doc! {"$score": {"score": "$x", "weight": 2}}], 2),
            (vec![doc! {"$score": {"score": "abc"}}], 14),
        ];
        for (pipeline, code) in cases {
            let (got, msg) = error(c, pipeline.clone());
            assert_eq!(got, code, "{pipeline:?}: {msg}");
        }
    });
}

#[test]
fn hybrid_stages_run_in_lookup_and_union_sub_pipelines() {
    with_wt(|c| {
        seed(c);
        // mongod 8.2.11: the `$lookup`'s own `$rankFusion` ranks 5 then 4.
        let reply = dispatch_full(
            &doc! {"aggregate": "s", "cursor": {}, "pipeline": [
                {"$limit": 1},
                {"$lookup": {"from": "s", "as": "q", "pipeline": [
                    {"$rankFusion": {"input": {"pipelines": {"a": [{"$sort": {"x": -1}}]}}}},
                    {"$limit": 2},
                    {"$project": {"s": {"$meta": "score"}}},
                ]}},
            ]},
            c,
        );
        let q = reply
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()[0]
            .as_document()
            .unwrap()
            .get_array("q")
            .unwrap()
            .clone();
        assert_eq!(
            q,
            vec![
                Bson::Document(doc! {"_id": 5, "s": 0.01639344262295082}),
                Bson::Document(doc! {"_id": 4, "s": 0.016129032258064516}),
            ]
        );
        // A `$score` inside a `$facet` is 40218, as on mongod.
        let (code, _) = error(
            c,
            vec![doc! {"$facet": {"f": [{"$score": {"score": "$x"}}]}}],
        );
        assert_eq!(code, 40218);
    });
}
