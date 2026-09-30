//! Executing `MERGE` (see `secantus_pgplan::merge` for the plan's shape).
//!
//! Both reads run first, over the statement's starting snapshot; then each
//! chosen action runs as the UPDATE / DELETE / INSERT it is, by the target
//! row's identity, through the same internal path a PL/pgSQL body's
//! statements take -- so constraints, defaults, triggers, privileges and row
//! security all apply. The statement's own implicit transaction (an
//! autocommit MERGE is a row write) makes the whole of it atomic.

use bson::Bson;
use pgwire::api::results::{Response, Tag};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use secantus_pgplan::merge::{Merge, MergeAction};
use secantus_pgplan::scalar::quote_identifier as q;

use crate::{PgHandler, PlHost};

thread_local! {
    /// The MERGE target whose statement triggers the row actions must not
    /// fire: the MERGE fires them itself.
    static MERGING: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn statement_triggers_suppressed(table: &str) -> bool {
    MERGING.with(|m| m.borrow().as_deref() == Some(table))
}

impl PgHandler {
    /// MERGE's statement triggers: PostgreSQL fires BEFORE STATEMENT for
    /// every action type the command names (INSERT, then UPDATE, then
    /// DELETE) before it reads a row, and AFTER STATEMENT in the reverse
    /// order at the end -- whether or not any row took that action.
    pub(crate) fn execute_merge(&self, m: Merge) -> PgWireResult<Vec<Response>> {
        let has = |f: fn(&MergeAction) -> bool| m.clauses.iter().any(|(_, a)| f(a));
        let events: Vec<&str> = [
            ("INSERT", has(|a| matches!(a, MergeAction::Insert { .. }))),
            ("UPDATE", has(|a| matches!(a, MergeAction::Update(_)))),
            ("DELETE", has(|a| matches!(a, MergeAction::Delete))),
        ]
        .into_iter()
        .filter_map(|(e, on)| on.then_some(e))
        .collect();
        for e in &events {
            self.fire_statement_triggers(&m.target, "BEFORE", e)?;
        }
        struct Restore(Option<String>);
        impl Drop for Restore {
            fn drop(&mut self) {
                MERGING.with(|m| *m.borrow_mut() = self.0.take());
            }
        }
        let target = m.target.clone();
        let out = {
            let _restore = Restore(MERGING.with(|g| g.borrow_mut().replace(target)));
            self.merge_rows(m.clone())?
        };
        for e in events.iter().rev() {
            self.fire_statement_triggers(&m.target, "AFTER", e)?;
        }
        Ok(out)
    }

    fn merge_twice() -> PgWireError {
        let mut info = ErrorInfo::new(
            "ERROR".into(),
            "21000".into(),
            "MERGE command cannot affect row a second time".into(),
        );
        info.hint =
            Some("Ensure that not more than one source row matches any one target row.".into());
        PgWireError::UserError(Box::new(info))
    }

    fn merge_rows(&self, m: Merge) -> PgWireResult<Vec<Response>> {
        let def = self.lookup(&m.target).ok_or_else(|| {
            Self::user_error("42P01", format!("relation \"{}\" does not exist", m.target))
        })?;
        let matched = self.run_sql_rows(&m.matched_sql, &m.params)?;
        let not_matched = self.run_sql_rows(&m.not_matched_sql, &m.params)?;
        let host = PlHost { h: self };
        let run = |sql: &str, params: &[Bson]| -> PgWireResult<u64> {
            crate::plpgsql_fn::Host::execute(&host, sql, params, &[]).map_err(|e| {
                let mut info = ErrorInfo::new("ERROR".into(), e.sqlstate, e.message);
                info.detail = e.detail;
                PgWireError::UserError(Box::new(info))
            })
        };
        // Where each clause's computed values start in its query's row.
        let key_len = m.key.len();
        let mut update_at = vec![0usize; m.clauses.len()];
        let mut insert_at = vec![0usize; m.clauses.len()];
        let (mut u, mut i) = (key_len + 1, 1usize);
        for (n, (_, action)) in m.clauses.iter().enumerate() {
            match action {
                MergeAction::Update(cols) => {
                    update_at[n] = u;
                    u += cols.len();
                }
                MergeAction::Insert { defaults, .. } => {
                    insert_at[n] = i;
                    i += defaults.iter().filter(|d| !**d).count();
                }
                _ => {}
            }
        }
        let key_match = |first_param: usize| -> String {
            m.key
                .iter()
                .enumerate()
                .map(|(n, k)| {
                    let op = if m.key_is_pk {
                        "="
                    } else {
                        "IS NOT DISTINCT FROM"
                    };
                    format!("{} {op} ${}", q(k), first_param + n)
                })
                .collect::<Vec<_>>()
                .join(" AND ")
        };
        let clause_of = |v: &Bson| -> Option<usize> {
            let n = match v {
                Bson::Int32(n) => i64::from(*n),
                Bson::Int64(n) => *n,
                _ => -1,
            };
            usize::try_from(n).ok()
        };

        let mut affected: u64 = 0;
        let mut seen: Vec<Vec<Bson>> = Vec::new();
        for row in &matched {
            let Some(clause) = row.get(key_len).and_then(clause_of) else {
                continue;
            };
            let key: Vec<Bson> = row[..key_len].to_vec();
            let action = &m.clauses[clause].1;
            if matches!(action, MergeAction::Nothing) {
                continue;
            }
            // With no primary key a target row is known only by its values,
            // so IDENTICAL rows share one: the action runs once for all of
            // them, and a row was hit twice only when the key has more
            // acting source rows than target rows.
            if !m.key_is_pk {
                if seen.contains(&key) {
                    continue;
                }
                let acting = matched
                    .iter()
                    .filter(|r| r[..key_len] == key[..])
                    .filter(|r| {
                        r.get(key_len)
                            .and_then(clause_of)
                            .is_some_and(|c| !matches!(m.clauses[c].1, MergeAction::Nothing))
                    })
                    .count();
                let targets = self.run_sql_rows(
                    &format!(
                        "SELECT count(*) FROM {} WHERE {}",
                        q(&m.target),
                        key_match(1)
                    ),
                    &key,
                )?;
                let n = match targets.first().and_then(|r| r.first()) {
                    Some(Bson::Int64(n)) => *n as usize,
                    Some(Bson::Int32(n)) => *n as usize,
                    _ => 1,
                };
                if acting > n {
                    return Err(Self::merge_twice());
                }
            }
            // One target row joined by two source rows would be changed
            // twice: PostgreSQL refuses rather than pick one.
            if seen.contains(&key) {
                return Err(Self::merge_twice());
            }
            seen.push(key.clone());
            match action {
                MergeAction::Update(cols) => {
                    let values = &row[update_at[clause]..update_at[clause] + cols.len()];
                    let sets: Vec<String> = cols
                        .iter()
                        .enumerate()
                        .map(|(n, c)| format!("{} = ${}", q(c), n + 1))
                        .collect();
                    let sql = format!(
                        "UPDATE {} SET {} WHERE {}",
                        q(&m.target),
                        sets.join(", "),
                        key_match(cols.len() + 1)
                    );
                    let params: Vec<Bson> = values.iter().chain(&key).cloned().collect();
                    let n = run(&sql, &params)?;
                    affected += if m.key_is_pk { n.min(1) } else { n };
                }
                MergeAction::Delete => {
                    let sql = format!("DELETE FROM {} WHERE {}", q(&m.target), key_match(1));
                    let n = run(&sql, &key)?;
                    affected += if m.key_is_pk { n.min(1) } else { n };
                }
                _ => {}
            }
        }
        for row in &not_matched {
            let Some(clause) = row.first().and_then(clause_of) else {
                continue;
            };
            let MergeAction::Insert { columns, defaults } = &m.clauses[clause].1 else {
                continue;
            };
            let names: Vec<String> = if columns.is_empty() {
                def.columns.iter().map(|c| c.name.clone()).collect()
            } else {
                columns.clone()
            };
            // A DEFAULT is an omitted column, which takes its default.
            let mut cols = Vec::new();
            let mut params = Vec::new();
            let mut next = insert_at[clause];
            for (n, is_default) in defaults.iter().enumerate() {
                if *is_default {
                    continue;
                }
                cols.push(q(&names[n]));
                params.push(row.get(next).cloned().unwrap_or(Bson::Null));
                next += 1;
            }
            let sql = if cols.is_empty() {
                format!("INSERT INTO {} DEFAULT VALUES", q(&m.target))
            } else {
                let slots: Vec<String> = (1..=cols.len()).map(|n| format!("${n}")).collect();
                format!(
                    "INSERT INTO {} ({}) VALUES ({})",
                    q(&m.target),
                    cols.join(", "),
                    slots.join(", ")
                )
            };
            affected += run(&sql, &params)?;
        }
        Ok(vec![Response::Execution(
            Tag::new("MERGE").with_rows(affected as usize),
        )])
    }
}
