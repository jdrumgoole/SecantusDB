//! Command progress reporting (RP-100; decided by Joe, 2026-10-22: a
//! progress slot for every command): PostgreSQL 15.19's
//! `pgstat_progress_start_command` / `_update_param` / `_end_command`. Each
//! backend has one slot; a command that reports its progress fills it while
//! it runs, and `pg_stat_get_progress_info(cmdtype)` lists every backend's
//! slot of that type for the `pg_stat_progress_*` views, PostgreSQL's own
//! definitions over it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use bson::Bson;

/// `ProgressCommandType`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Command {
    Vacuum,
    Analyze,
    Cluster,
    CreateIndex,
    Copy,
}

/// `PGSTAT_NUM_PROGRESS_PARAM`.
const PARAMS: usize = 20;

#[derive(Debug, Clone)]
struct Slot {
    command: Command,
    datid: i64,
    relid: i64,
    params: [i64; PARAMS],
}

type Key = (usize, i32);

fn slots() -> &'static Mutex<HashMap<Key, Slot>> {
    static SLOTS: OnceLock<Mutex<HashMap<Key, Slot>>> = OnceLock::new();
    SLOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One backend's slot.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Progress {
    pub store: usize,
    pub pid: i32,
}

impl Progress {
    /// `pgstat_progress_start_command`: the slot is the command's, its
    /// parameters zero.
    pub(crate) fn start(&self, command: Command, datid: i64, relid: i64) {
        slots().lock().unwrap_or_else(|e| e.into_inner()).insert(
            (self.store, self.pid),
            Slot {
                command,
                datid,
                relid,
                params: [0; PARAMS],
            },
        );
    }

    /// `pgstat_progress_update_param`: parameter `index` (from 0) is
    /// `value`.
    pub(crate) fn set(&self, index: usize, value: i64) {
        if let Some(s) = slots()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&(self.store, self.pid))
        {
            if let Some(p) = s.params.get_mut(index) {
                *p = value;
            }
        }
    }

    /// `pgstat_progress_incr_param`.
    pub(crate) fn add(&self, index: usize, by: i64) {
        if let Some(s) = slots()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&(self.store, self.pid))
        {
            if let Some(p) = s.params.get_mut(index) {
                *p += by;
            }
        }
    }

    /// `pgstat_progress_end_command`.
    pub(crate) fn end(&self) {
        slots()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(self.store, self.pid));
    }
}

/// `pg_stat_get_progress_info(cmdtype)`: a record per backend whose slot
/// holds that command, `(pid, datid, relid, param1 .. param20)`, in pid
/// order. An unknown command type is 22023, as PostgreSQL's
/// `invalid command name`.
pub(crate) fn info(store: usize, cmdtype: &str) -> Result<Vec<Vec<Bson>>, String> {
    let wanted = match cmdtype.to_ascii_uppercase().as_str() {
        "VACUUM" => Command::Vacuum,
        "ANALYZE" => Command::Analyze,
        "CLUSTER" => Command::Cluster,
        "CREATE INDEX" => Command::CreateIndex,
        "COPY" => Command::Copy,
        // Base backups need replication, which this server does not serve:
        // no backend ever runs one.
        "BASEBACKUP" => return Ok(Vec::new()),
        _ => return Err(format!("invalid command name: \"{cmdtype}\"")),
    };
    let guard = slots().lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<(i32, Vec<Bson>)> = guard
        .iter()
        .filter(|((s, _), slot)| *s == store && slot.command == wanted)
        .map(|((_, pid), slot)| {
            let mut row = vec![
                Bson::Int32(*pid),
                Bson::Int64(slot.datid),
                Bson::Int64(slot.relid),
            ];
            row.extend(slot.params.iter().map(|p| Bson::Int64(*p)));
            (*pid, row)
        })
        .collect();
    rows.sort_by_key(|(p, _)| *p);
    Ok(rows.into_iter().map(|(_, r)| r).collect())
}

/// A command's slot for as long as it runs: cleared however it ends.
pub(crate) struct Running(Progress);

impl Running {
    pub(crate) fn start(slot: Progress, command: Command, datid: i64, relid: i64) -> Self {
        slot.start(command, datid, relid);
        Running(slot)
    }

    pub(crate) fn set(&self, index: usize, value: i64) {
        self.0.set(index, value);
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.0.end();
    }
}
