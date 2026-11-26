//! The stages a `$lookup`'s foreign side is made of, run as SBE runs them:
//! the inner side of a `hash_lookup` is a scan of the foreign collection
//! whose every record goes through a small correlated program (`coscan`,
//! `limit`, `unwind`, `branch`, `union`, `nlj`, a `group` into a set) that
//! yields the record's join keys.
//!
//! Each stage is run for every row of the stage it is correlated with, so
//! its counters (`opens`, `closes`, `nReturned`, ...) add up over the runs.
//! Every consumer here pulls its inputs to the end, except a `limit` over a
//! `coscan`, which stops after its row, so a run is evaluated whole and the
//! counters follow from what each stage returned.

use super::expr::{Env, Expr, Value};
use super::stage::{Counted, Row, Stage};
use bson::{Bson, Document};
use std::cmp::Ordering;

/// The expression over the row it is evaluated for and the correlated
/// slots of the rows around it.
fn eval(e: &Expr, row: &Row, env: &Row) -> Value {
    let slots = |s: u32| row.get(&s).or_else(|| env.get(&s)).cloned().flatten();
    let mut scope = Env {
        slots: &slots,
        locals: Vec::new(),
    };
    e.eval(&mut scope)
}

fn kid(acc: &mut Counted, i: usize) -> &mut Counted {
    while acc.kids.len() <= i {
        acc.kids.push(Counted::default());
    }
    &mut acc.kids[i]
}

fn add_closes(acc: &mut Counted, n: i64) {
    *acc.closes.get_or_insert(0) += n;
}

/// The same for a stage and everything under it: a branch's side is closed
/// as a whole.
fn add_closes_deep(acc: &mut Counted, n: i64) {
    add_closes(acc, n);
    for below in &mut acc.kids {
        add_closes_deep(below, n);
    }
}

/// The inner side of a join is closed once for each run of the join that
/// opened it, not each time it is re-opened.
fn end_run(inner: &mut Counted) {
    add_closes(inner, 1);
    if let Some(side) = inner.side.take() {
        add_closes_deep(kid(inner, side), 1);
    }
}

fn bump(acc: &mut Counted, rows: usize) {
    *acc.opens.get_or_insert(0) += 1;
    *acc.closes.get_or_insert(0) += 1;
    acc.returned += rows as i64;
}

/// Run `stage` once, under the correlated slots of `env`, adding what it
/// counted to `acc`.
pub(super) fn exec(stage: &Stage, env: &Row, foreign: &[Document], acc: &mut Counted) -> Vec<Row> {
    match stage {
        Stage::Scan {
            record,
            record_id,
            fields,
            ..
        } => {
            let rows: Vec<Row> = foreign
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let mut row = Row::new();
                    row.insert(*record, Some(Bson::Document(d.clone())));
                    row.insert(*record_id, Some(Bson::Int64(i as i64 + 1)));
                    for (name, slot) in fields {
                        row.insert(*slot, d.get(name).cloned());
                    }
                    row
                })
                .collect();
            bump(acc, rows.len());
            acc.reads += rows.len() as i64;
            acc.eof = true;
            rows
        }
        // Never at EOF: the one row it hands over is all anything asks of
        // it.
        Stage::Coscan { .. } => {
            bump(acc, 1);
            vec![Row::new()]
        }
        Stage::LimitLit { limit, input, .. } => {
            let mut rows = exec(input, env, foreign, kid(acc, 0));
            rows.truncate((*limit).max(0) as usize);
            bump(acc, rows.len());
            acc.eof = true;
            rows
        }
        Stage::Project { binds, input, .. } => {
            let mut rows = exec(input, env, foreign, kid(acc, 0));
            for row in &mut rows {
                for (slot, e) in binds {
                    let v = eval(e, row, env);
                    row.insert(*slot, v);
                }
            }
            bump(acc, rows.len());
            acc.eof = true;
            rows
        }
        Stage::Unwind {
            input_slot,
            out,
            index,
            preserve,
            input,
            ..
        } => {
            let below = exec(input, env, foreign, kid(acc, 0));
            let mut rows = Vec::new();
            for row in below {
                let value = row.get(input_slot).or_else(|| env.get(input_slot)).cloned();
                let value = value.flatten();
                match value {
                    Some(Bson::Array(items)) if !items.is_empty() => {
                        for (i, item) in items.into_iter().enumerate() {
                            let mut r = row.clone();
                            r.insert(*out, Some(item));
                            r.insert(*index, Some(Bson::Int64(i as i64)));
                            rows.push(r);
                        }
                    }
                    other => {
                        if *preserve {
                            let mut r = row.clone();
                            let kept = match other {
                                Some(Bson::Array(_)) => None,
                                v => v,
                            };
                            r.insert(*out, kept);
                            r.insert(*index, None);
                            rows.push(r);
                        }
                    }
                }
            }
            bump(acc, rows.len());
            acc.eof = true;
            rows
        }
        Stage::Branch {
            predicate,
            then_slots,
            else_slots,
            outputs,
            then_,
            else_,
            ..
        } => {
            acc.tested += 1;
            // A predicate that is not a boolean (`Nothing`) opens neither
            // side: the stage returns nothing.
            let (side, slots, at) = match eval(predicate, &Row::new(), env) {
                Some(Bson::Boolean(true)) => {
                    acc.then_opens += 1;
                    (then_, then_slots, 0)
                }
                Some(Bson::Boolean(false)) => {
                    acc.else_opens += 1;
                    (else_, else_slots, 1)
                }
                _ => {
                    *acc.opens.get_or_insert(0) += 1;
                    *acc.closes.get_or_insert(0) += 1;
                    acc.eof = true;
                    return Vec::new();
                }
            };
            // A side stays open while the branch is re-opened onto it, and
            // closes when the other side opens or the join's run ends.
            if let Some(open) = acc.side.filter(|open| *open != at) {
                add_closes_deep(kid(acc, open), 1);
            }
            acc.side = Some(at);
            let below = exec(side, env, foreign, kid(acc, at));
            add_closes_deep(kid(acc, at), -1);
            let rows: Vec<Row> = below
                .into_iter()
                .map(|r| {
                    let mut out = Row::new();
                    for (from, to) in slots.iter().zip(outputs) {
                        let v = r.get(from).or_else(|| env.get(from)).cloned().flatten();
                        out.insert(*to, v);
                    }
                    out
                })
                .collect();
            bump(acc, rows.len());
            acc.eof = true;
            rows
        }
        Stage::Union {
            inputs, outputs, ..
        } => {
            let mut rows = Vec::new();
            for (i, (slots, input)) in inputs.iter().enumerate() {
                for r in exec(input, env, foreign, kid(acc, i)) {
                    let mut out = Row::new();
                    for (from, to) in slots.iter().zip(outputs) {
                        let v = r.get(from).or_else(|| env.get(from)).cloned().flatten();
                        out.insert(*to, v);
                    }
                    rows.push(out);
                }
            }
            bump(acc, rows.len());
            acc.eof = true;
            rows
        }
        Stage::Nlj { outer, inner, .. } => {
            let left = exec(outer, env, foreign, kid(acc, 0));
            let mut rows = Vec::new();
            let mut ran = false;
            for l in left {
                let mut around = env.clone();
                around.extend(l.clone());
                acc.inner_opens += 1;
                for r in exec(inner, &around, foreign, kid(acc, 1)) {
                    let mut joined = l.clone();
                    joined.extend(r);
                    rows.push(joined);
                }
                add_closes(kid(acc, 1), -1);
                ran = true;
            }
            if ran {
                end_run(kid(acc, 1));
            }
            // The inner side is closed as often as it closed itself.
            acc.inner_closes = acc.kids.get(1).and_then(|k| k.closes).unwrap_or(0);
            bump(acc, rows.len());
            acc.eof = true;
            rows
        }
        Stage::Group { aggs, input, .. } => {
            let below = exec(input, env, foreign, kid(acc, 0));
            let mut row = Row::new();
            for agg in aggs {
                let mut state: Value = None;
                for r in &below {
                    let v = agg.arg.as_ref().and_then(|a| eval(a, r, env));
                    state = agg.func.step(state.take(), v);
                }
                row.insert(agg.slot, state.or(Some(Bson::Array(vec![]))));
            }
            // With nothing to group, a group with no key returns no row.
            bump(acc, usize::from(!below.is_empty()));
            acc.eof = true;
            if below.is_empty() {
                Vec::new()
            } else {
                vec![row]
            }
        }
        // The rest run through `Stage::run`.
        other => {
            let (rows, counted) = other.run(&[], None);
            *acc = counted;
            rows
        }
    }
}

/// `hash_lookup`: build the table from the inner side, then give each outer
/// row the foreign records whose keys it shares.
pub(super) fn hash_lookup(
    stage: &Stage,
    docs: &[Document],
    demand: Option<usize>,
) -> (Vec<Row>, Counted) {
    let Stage::HashLookup {
        outer_key,
        inner_key,
        inner_value,
        out,
        outer,
        inner,
        foreign,
        ..
    } = stage
    else {
        return (Vec::new(), Counted::default());
    };
    // The inner side runs first, to the end, to build the table.
    let mut inner_counted = Counted::blank(inner);
    let built = exec(inner, &Row::new(), foreign, &mut inner_counted);
    let (rows, outer_counted) = outer.run(docs, demand);
    let keys_of = |v: &Value| -> Vec<Bson> {
        match v {
            Some(Bson::Array(a)) => a.clone(),
            _ => Vec::new(),
        }
    };
    let rows: Vec<Row> = rows
        .into_iter()
        .map(|mut row| {
            let wanted = keys_of(&row.get(outer_key).cloned().flatten());
            let matched: Vec<Bson> = built
                .iter()
                .filter(|b| {
                    keys_of(&b.get(inner_key).cloned().flatten())
                        .iter()
                        .any(|k| {
                            wanted
                                .iter()
                                .any(|w| secantus_core::order::cmp(w, k) == Ordering::Equal)
                        })
                })
                .filter_map(|b| b.get(inner_value).cloned().flatten())
                .collect();
            row.insert(*out, Some(Bson::Array(matched)));
            row
        })
        .collect();
    let n = rows.len() as i64;
    (
        rows,
        Counted {
            returned: n,
            eof: outer_counted.eof,
            kids: vec![outer_counted, inner_counted],
            ..Counted::default()
        },
    )
}
