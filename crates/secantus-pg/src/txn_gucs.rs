//! Settings a transaction block changes, undone or kept when it ends, as
//! PostgreSQL's GUC stack does:
//!
//! - `SET LOCAL x` (and `set_config(x, v, true)`) lasts until the block ends,
//!   COMMIT or ROLLBACK alike, then `x` returns to its value before the block.
//! - a plain `SET x` inside a block is kept by COMMIT and undone by ROLLBACK.
//!
//! A reportable setting the end changes is reported again (`ParameterStatus`),
//! so a client tracking `application_name` sees it return.

use super::*;

/// Per key: the value before the block first changed it, and the session
/// value a COMMIT keeps (`None` when only `SET LOCAL` touched it).
pub(crate) type TxnGucs = HashMap<String, (Option<String>, Option<String>)>;

impl PgHandler {
    fn in_block(&self) -> bool {
        self.in_transaction
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Record a change to `key` the block is about to make. `local` for
    /// `SET LOCAL`; `value` is what a plain SET stores (kept on COMMIT).
    pub(crate) fn note_txn_guc(&self, key: &str, local: bool, value: &str) {
        // An extended-protocol statement group is a transaction too: what it
        // sets LOCAL ends at its `Sync`.
        if !self.in_block()
            && !self
                .implicit_extended
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        let before = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned();
        let mut map = self.txn_gucs.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(key.to_string()).or_insert((before, None));
        if !local {
            entry.1 = Some(value.to_string());
        }
    }

    /// `SET LOCAL` outside a block changes nothing, with PostgreSQL's
    /// warning. `true` when the caller should go on and apply it.
    pub(crate) fn set_local_allowed(&self) -> bool {
        if self.in_block() {
            return true;
        }
        self.warning(
            "25P01",
            "SET LOCAL can only be used in transaction blocks".into(),
        );
        false
    }

    /// Remember `key`'s value before a statement-scoped `set_config`.
    pub(crate) fn note_statement_guc(&self, key: &str) {
        let before = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned();
        self.statement_gucs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key.to_string())
            .or_insert(before);
    }

    /// The statement ended: put back what its `set_config(..., true)` set.
    pub(crate) fn end_statement_gucs(&self) {
        let changed: HashMap<String, Option<String>> = std::mem::take(
            &mut *self
                .statement_gucs
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        let mut settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        for (key, before) in changed {
            match before {
                Some(v) => {
                    settings.insert(key, v);
                }
                None => {
                    settings.remove(&key);
                }
            }
        }
    }

    /// The block ended: put back what it changed that it may not keep.
    pub(crate) fn end_txn_gucs(&self, commit: bool) {
        let changed: TxnGucs =
            std::mem::take(&mut *self.txn_gucs.lock().unwrap_or_else(|e| e.into_inner()));
        for (key, (before, kept)) in changed {
            let target = if commit { kept.or(before) } else { before };
            let mut settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
            let current = settings.get(&key).cloned();
            if current == target {
                continue;
            }
            match &target {
                Some(v) => {
                    settings.insert(key.clone(), v.clone());
                }
                None => {
                    settings.remove(&key);
                }
            }
            drop(settings);
            if let Some(v) = target {
                self.note_reportable_guc(&key, &v);
            }
        }
    }

    /// What a COMMIT keeps for `key` so far (outer `None`: not tracked).
    pub(crate) fn txn_guc_kept(&self, key: &str) -> Option<Option<String>> {
        self.txn_gucs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .map(|(_, kept)| kept.clone())
    }

    /// Put back what a COMMIT keeps for `key` after a LOCAL change ran
    /// through the plain SET path (which records its value as kept).
    pub(crate) fn restore_txn_guc_kept(&self, key: &str, kept: Option<Option<String>>) {
        if let Some(entry) = self
            .txn_gucs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(key)
        {
            entry.1 = kept.flatten();
        }
    }

    /// `ROLLBACK TO SAVEPOINT`: put the settings back as they were when the
    /// savepoint was established, reporting a reportable one that changes.
    pub(crate) fn restore_savepoint_settings(&self, saved: HashMap<String, String>, gucs: TxnGucs) {
        *self.txn_gucs.lock().unwrap_or_else(|e| e.into_inner()) = gucs;
        let changed: Vec<(String, String)> = {
            let mut settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
            let changed = saved
                .iter()
                .filter(|(k, v)| settings.get(*k) != Some(*v))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            *settings = saved.into();
            changed
        };
        for (k, v) in changed {
            self.note_reportable_guc(&k, &v);
        }
    }
}
