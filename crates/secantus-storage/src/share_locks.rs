//! Shared row locks (`SELECT ... FOR SHARE` / `FOR KEY SHARE`).
//!
//! WiredTiger's only row lock is the exclusive one a write takes, so a
//! shared lock cannot be a write: two sharers of one row must both proceed.
//! A shared lock is therefore an entry in a process-wide table, keyed by the
//! row, naming the transaction (its [`HeldRows`](crate::HeldRows) set) that
//! holds it. A WRITE checks that table as it notes the row it is about to
//! write (`note_row`) and, when another transaction shares the row in a
//! conflicting mode, fails with the write conflict the server already waits
//! out (holder, deadlock check, `lock_timeout`) -- the same path a collision
//! on an exclusive lock takes.
//!
//! PostgreSQL's conflict table, as far as writes go: a `FOR SHARE` holder
//! blocks every write; a `FOR KEY SHARE` holder blocks only a write that
//! takes the KEY-strength lock (`DELETE`, `FOR UPDATE`, an UPDATE of a key
//! column) -- the thread marks such a write with [`with_key_write`]. The
//! taker's side (a sharer waiting for an exclusive holder) is the server's,
//! which sees every transaction's held rows.
//!
//! The race between a writer and a taker is closed Dekker-style: a writer
//! publishes its row (into its held set) BEFORE reading the table, and a
//! taker publishes its entry BEFORE reading the held sets, so at least one
//! of two simultaneous parties sees the other.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::WrittenRow;

/// The strength of a shared row lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ShareMode {
    /// `FOR KEY SHARE`: blocks only key-strength writes.
    KeyShare,
    /// `FOR SHARE`: blocks every write.
    Share,
}

/// `(store, row) -> [(holder, mode)]`, the holder being the address of a
/// transaction's `Held` (stable while the transaction lives). The table is
/// process-wide, so a row is keyed by its STORE too (`Held::store`): two
/// stores open in one process name rows alike (`postgres`, a collection, a
/// RecordId), and one's lock must not block the other's write.
type Table = Mutex<HashMap<(usize, WrittenRow), Vec<(usize, ShareMode)>>>;

fn table() -> &'static Table {
    static T: std::sync::OnceLock<Table> = std::sync::OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Entries in [`table`]: a writer reads the table only while this is
/// non-zero, so a store with no shared lock pays one atomic load a row.
static COUNT: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static KEY_WRITE: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` with this thread's writes taking the KEY-strength lock (they
/// conflict with `FOR KEY SHARE` too).
pub fn with_key_write<T>(f: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            KEY_WRITE.with(|k| k.set(self.0));
        }
    }
    let _reset = Reset(KEY_WRITE.with(|k| k.replace(true)));
    f()
}

/// Is this thread inside [`with_key_write`]?
pub fn key_write_now() -> bool {
    key_write()
}

/// Is this thread's current write a key-strength one?
pub(crate) fn key_write() -> bool {
    KEY_WRITE.with(Cell::get)
}

/// Does a transaction other than `me` share `row` in a mode a write of
/// this thread's strength conflicts with?
pub(crate) fn write_blocked(store: usize, row: &WrittenRow, me: usize) -> bool {
    if COUNT.load(Ordering::SeqCst) == 0 {
        return false;
    }
    let key = key_write();
    table()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(store, row.clone()))
        .is_some_and(|hs| {
            hs.iter()
                .any(|(h, m)| *h != me && *h != mover_of(me) && (key || *m == ShareMode::Share))
        })
}

/// The identity a transaction's rows are guarded under while it moves onto
/// a new WiredTiger transaction (`MoveGuard` in `lib.rs`): its own holder
/// with the low bit set -- a `Held` is word-aligned, so no real holder has
/// it -- which its own replay is not blocked by.
pub(crate) fn mover_of(holder: usize) -> usize {
    holder | 1
}

/// Record that `holder` shares `row` in `mode` (an existing entry is
/// strengthened, never weakened). `true` when it is a new entry.
pub(crate) fn add(store: usize, row: &WrittenRow, holder: usize, mode: ShareMode) -> bool {
    let mut t = table().lock().unwrap_or_else(|e| e.into_inner());
    let hs = t.entry((store, row.clone())).or_default();
    if let Some(e) = hs.iter_mut().find(|(h, _)| *h == holder) {
        e.1 = e.1.max(mode);
        return false;
    }
    hs.push((holder, mode));
    COUNT.fetch_add(1, Ordering::SeqCst);
    true
}

/// `holder` no longer shares `row`.
pub(crate) fn remove(store: usize, row: &WrittenRow, holder: usize) {
    let key = (store, row.clone());
    let mut t = table().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(hs) = t.get_mut(&key) {
        let before = hs.len();
        hs.retain(|(h, _)| *h != holder);
        COUNT.fetch_sub(before - hs.len(), Ordering::SeqCst);
        if hs.is_empty() {
            t.remove(&key);
        }
    }
}
