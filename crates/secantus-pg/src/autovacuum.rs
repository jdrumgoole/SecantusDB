//! Autovacuum (RP-226), as PostgreSQL 15.19's launcher and workers run it
//! (`autovacuum.c`). The launcher wakes every `autovacuum_naptime`, spread
//! over the databases (`launcher_determine_sleep`), and starts a worker in
//! the next one due. The worker looks at each of the database's tables
//! (`do_autovacuum`), and vacuums and analyzes the ones whose cumulative
//! statistics pass `relation_needs_vacanalyze`'s thresholds: the base
//! threshold plus the scale factor times `reltuples`, for dead tuples,
//! tuples inserted since the last vacuum, and tuples changed since the last
//! analyze -- each from the GUCs, or the table's own storage parameters. A
//! table another session holds a conflicting lock on is skipped, as the
//! worker's conditional lock skips it. What it does is the server's own
//! VACUUM and ANALYZE, reported under the autovacuum counters
//! (`last_autovacuum`, `autoanalyze_count`, ...).

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use secantus_storage::Storage;

use crate::{DatabaseRegistry, PgHandler};

thread_local! {
    static IN_WORKER: Cell<bool> = const { Cell::new(false) };
}

/// Is this thread an autovacuum worker's? Its VACUUM and ANALYZE are
/// reported as autovacuum's.
pub(crate) fn in_worker() -> bool {
    IN_WORKER.with(Cell::get)
}

/// Start the launcher over `storage`. It runs until `stop` is set; the
/// handle is joined before the store closes.
pub(crate) fn launch(
    storage: &Arc<Storage>,
    databases: Arc<DatabaseRegistry>,
    stop: Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    let storage = Arc::downgrade(storage);
    std::thread::Builder::new()
        .name("secantus-pg-autovacuum".into())
        .stack_size(crate::server::WORKER_STACK_BYTES)
        .spawn(move || launcher(storage, databases, stop))
        .ok()
}

/// `AutoVacLauncherMain`: sleep, then a worker in the database due next,
/// round the databases in turn.
fn launcher(storage: Weak<Storage>, databases: Arc<DatabaseRegistry>, stop: Arc<AtomicBool>) {
    IN_WORKER.with(|w| w.set(true));
    let mut next_db = 0usize;
    loop {
        // The naptime and the database list are read afresh at each wake:
        // a reload reaches the launcher, and a new database is visited.
        let (naptime, names) = {
            let Some(store) = storage.upgrade() else {
                return;
            };
            let names: Vec<String> = databases
                .all(&store)
                .unwrap_or_default()
                .into_iter()
                .filter(|d| d.allow_conn)
                .map(|d| d.name)
                .collect();
            let naptime = names
                .first()
                .and_then(|db| setting(&store, &databases, db, "autovacuum_naptime"))
                .and_then(|v| parse_seconds(&v))
                .unwrap_or(60.0);
            (naptime, names)
        };
        // Each database once a naptime, the naptime shared among them. A
        // reload that shortens it reaches the sleep (the launcher's SIGHUP).
        let ndb = names.len().max(1) as f64;
        let started = Instant::now();
        let mut sleep = Duration::from_secs_f64(naptime / ndb);
        let mut checked = Instant::now();
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            if started.elapsed() >= sleep {
                break;
            }
            if checked.elapsed() >= Duration::from_secs(1) {
                checked = Instant::now();
                let Some(store) = storage.upgrade() else {
                    return;
                };
                if let Some(n) = names
                    .first()
                    .and_then(|db| setting(&store, &databases, db, "autovacuum_naptime"))
                    .and_then(|v| parse_seconds(&v))
                {
                    sleep = Duration::from_secs_f64(n / ndb);
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if names.is_empty() {
            continue;
        }
        let db = &names[next_db % names.len()];
        next_db = next_db.wrapping_add(1);
        let Some(store) = storage.upgrade() else {
            return;
        };
        if let Some(handler) = worker(&store, &databases, db) {
            handler.autovacuum_database(&stop);
        }
    }
}

/// A setting as a session of `db` would see it.
fn setting(
    store: &Arc<Storage>,
    databases: &Arc<DatabaseRegistry>,
    db: &str,
    name: &str,
) -> Option<String> {
    worker(store, databases, db)?.setting_value(name)
}

/// A worker's session of `db`: the server's configuration file and the
/// database's own settings in force, as a backend's start applies them.
fn worker(store: &Arc<Storage>, databases: &Arc<DatabaseRegistry>, db: &str) -> Option<PgHandler> {
    let handler = PgHandler::new(store.clone(), databases.clone());
    handler.select_database(db).ok()?;
    handler.start_config_layer();
    for (name, value, source) in handler.db_setting_layers() {
        let _ = handler.apply_setting_from(&name, &value, source);
    }
    Some(handler)
}

/// A setting's value in seconds (`1min`, `30s`, `500ms`, a bare number of
/// seconds).
fn parse_seconds(v: &str) -> Option<f64> {
    let v = v.trim();
    let split = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(v.len());
    let (n, unit) = v.split_at(split);
    let n: f64 = n.parse().ok()?;
    Some(match unit.trim() {
        "" | "s" => n,
        "ms" => n / 1000.0,
        "min" => n * 60.0,
        "h" => n * 3600.0,
        "d" => n * 86400.0,
        _ => return None,
    })
}

/// A relation key (`name`, or `schema.name` outside `public`) as SQL.
fn quoted_name(name: &str) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let (schema, bare) = secantus_pgplan::schemas::split_key(name);
    if schema == "public" {
        quote(&bare)
    } else {
        format!("{}.{}", quote(&schema), quote(&bare))
    }
}

/// A table's autovacuum parameters: its storage parameter, else the GUC.
struct Params {
    enabled: bool,
    vac_base: f64,
    vac_scale: f64,
    ins_base: f64,
    ins_scale: f64,
    anl_base: f64,
    anl_scale: f64,
}

impl PgHandler {
    /// `do_autovacuum` over this handler's database.
    pub(crate) fn autovacuum_database(&self, stop: &AtomicBool) {
        let on = |name: &str| {
            self.setting_value(name)
                .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "on" | "true" | "1"))
        };
        // Without `track_counts` there is nothing to judge by.
        if !on("autovacuum") || !on("track_counts") {
            return;
        }
        let gnum = |name: &str, default: f64| {
            self.setting_value(name)
                .and_then(|v| v.trim().parse::<f64>().ok())
                .unwrap_or(default)
        };
        let defaults = Params {
            enabled: true,
            vac_base: gnum("autovacuum_vacuum_threshold", 50.0),
            vac_scale: gnum("autovacuum_vacuum_scale_factor", 0.2),
            ins_base: gnum("autovacuum_vacuum_insert_threshold", 1000.0),
            ins_scale: gnum("autovacuum_vacuum_insert_scale_factor", 0.2),
            anl_base: gnum("autovacuum_analyze_threshold", 50.0),
            anl_scale: gnum("autovacuum_analyze_scale_factor", 0.1),
        };
        let Ok(mut tables) = self.all_table_defs() else {
            return;
        };
        // `pg_class` order: the tables' oids.
        tables.sort_by_key(|t| self.relation_oid(&t.name).unwrap_or(i64::MAX));
        for def in tables {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            // Its own session's temporary tables are no worker's; a
            // partitioned parent holds no rows of its own.
            if def.temp || crate::partition::is_partitioned(&def) || def.name.starts_with("__") {
                continue;
            }
            let Some(rel) = self.relation_oid(&def.name) else {
                continue;
            };
            let Some(entry) = self
                .stats()
                .rel_entry(crate::pgstat::Consistency::None, rel)
            else {
                continue;
            };
            let p = self.autovacuum_params(&def, &defaults);
            let reltuples = self
                .table_reltuples(&def.name)
                .filter(|t| *t >= 0.0)
                .unwrap_or(0.0);
            let vacthresh = p.vac_base + p.vac_scale * reltuples;
            let vacinsthresh = p.ins_base + p.ins_scale * reltuples;
            let anlthresh = p.anl_base + p.anl_scale * reltuples;
            let dovacuum = p.enabled
                && (entry.dead_tuples as f64 > vacthresh
                    || (p.ins_base >= 0.0 && entry.inserts_since_vacuum as f64 > vacinsthresh));
            let doanalyze = p.enabled && entry.changes_since_analyze as f64 > anlthresh;
            if !dovacuum && !doanalyze {
                continue;
            }
            // The worker's `ConditionalLockRelationOid` of SHARE UPDATE
            // EXCLUSIVE: a table another session holds is skipped.
            if !self.table_lock_free(&def.name, 4) {
                continue;
            }
            let target = quoted_name(&def.name);
            let sql = match (dovacuum, doanalyze) {
                (true, true) => format!("VACUUM (ANALYZE) {target}"),
                (true, false) => format!("VACUUM {target}"),
                _ => format!("ANALYZE {target}"),
            };
            if let Err(e) = self.run_internal_statement(&sql) {
                eprintln!("secantusd-pg: autovacuum of \"{}\" failed: {e}", def.name);
            }
        }
    }

    /// A setting's value as `SHOW` gives it.
    pub(crate) fn setting_value(&self, name: &str) -> Option<String> {
        self.read_setting(name).ok().flatten()
    }

    /// `pg_class.reltuples` of `table`: -1 when it was never measured.
    fn table_reltuples(&self, table: &str) -> Option<f64> {
        let d = self.relstats(table)?;
        match d.get("reltuples") {
            Some(bson::Bson::Int64(n)) => Some(*n as f64),
            Some(bson::Bson::Int32(n)) => Some(f64::from(*n)),
            Some(bson::Bson::Double(f)) => Some(*f),
            _ => None,
        }
    }

    /// Could this session take `mode` on `table` without waiting?
    fn table_lock_free(&self, table: &str, mode: i32) -> bool {
        let pid = self.backend_pid.load(Ordering::Relaxed);
        crate::table_locks::acquire::<()>(table, pid, mode, false, true, || Ok(()), || ())
            .unwrap_or(false)
    }

    /// Run one statement as this session, outside any block.
    fn run_internal_statement(&self, sql: &str) -> pgwire::error::PgWireResult<()> {
        let stmt = self.plan_internal(sql)?;
        self.execute(stmt, 0).map(|_| ())
    }

    /// `relation_needs_vacanalyze`'s parameters for `def`: its
    /// `autovacuum_*` storage parameters over the GUCs.
    fn autovacuum_params(&self, def: &secantus_pgcatalog::TableDef, d: &Params) -> Params {
        let options: Vec<String> = def
            .extra
            .get_array("reloptions")
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let opt = |name: &str| {
            options
                .iter()
                .find_map(|o| o.strip_prefix(&format!("{name}=")).map(str::to_string))
        };
        let num = |name: &str, fallback: f64| {
            opt(name)
                .and_then(|v| v.trim().parse::<f64>().ok())
                .unwrap_or(fallback)
        };
        Params {
            enabled: opt("autovacuum_enabled").map_or(d.enabled, |v| {
                matches!(
                    v.to_ascii_lowercase().as_str(),
                    "on" | "true" | "yes" | "1" | "t" | "y"
                )
            }),
            vac_base: num("autovacuum_vacuum_threshold", d.vac_base),
            vac_scale: num("autovacuum_vacuum_scale_factor", d.vac_scale),
            ins_base: num("autovacuum_vacuum_insert_threshold", d.ins_base),
            ins_scale: num("autovacuum_vacuum_insert_scale_factor", d.ins_scale),
            anl_base: num("autovacuum_analyze_threshold", d.anl_base),
            anl_scale: num("autovacuum_analyze_scale_factor", d.anl_scale),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_seconds;

    #[test]
    fn naptimes_read_as_postgres_writes_them() {
        assert_eq!(parse_seconds("1min"), Some(60.0));
        assert_eq!(parse_seconds("1s"), Some(1.0));
        assert_eq!(parse_seconds("30"), Some(30.0));
        assert_eq!(parse_seconds("500ms"), Some(0.5));
    }
}
