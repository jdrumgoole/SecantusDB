//! Spill tables: where a stage that spills to a storage-engine table, as
//! mongod 8.2's do, writes (RM-325).
//!
//! mongod 8.2 spills a `$setWindowFields`'s partition cache to a table in a
//! second WiredTiger instance it keeps for spilling (`<dbpath>/_tmp/spilldb`,
//! `featureFlagCreateSpillKVEngine`, on in 8.2): `SpillableDeque` writes the
//! documents it lets go as records keyed by record id, and reports the
//! table's size on disk (`SpillTable::storageSize`, WiredTiger's block
//! manager file size) as `spilledDataStorageSize`. The table lives in the
//! engine's cache, so a spill of a few hundred kilobytes leaves its file at
//! the 4096 bytes WiredTiger creates it with (measured 8.2.11, 2026-11-13).
//!
//! This crate has no storage engine, so the server hands one in for the
//! operation it runs ([`with_tables`]); a stage asks for a table with
//! [`create`]. With none handed in (the engine used on its own), a stage
//! keeps its own file instead.

use std::cell::RefCell;
use std::sync::Arc;

/// Something that makes spill tables: the server's spill engine.
pub trait SpillTables: Send + Sync {
    /// A new, empty table.
    fn create(&self) -> Result<Box<dyn SpillTable>, String>;
}

/// One spill table, dropped when the value is.
pub trait SpillTable {
    /// Write `records`, each its record id and its bytes, in one
    /// transaction.
    fn insert(&mut self, records: &[(i64, Vec<u8>)]) -> Result<(), String>;

    /// The table's size on disk: its file's size as WiredTiger's block
    /// manager counts it.
    fn storage_size(&self) -> Result<i64, String>;

    /// Drop the table, answering the error a drop on `Drop` could only
    /// print.
    fn drop_table(self: Box<Self>) -> Result<(), String>;
}

thread_local! {
    static TABLES: RefCell<Option<Arc<dyn SpillTables>>> = const { RefCell::new(None) };
}

/// Puts back the spill tables in force before [`with_tables`].
pub struct TablesScope(Option<Arc<dyn SpillTables>>);

impl Drop for TablesScope {
    fn drop(&mut self) {
        let before = self.0.take();
        TABLES.with(|t| *t.borrow_mut() = before);
    }
}

/// Spill what follows, on this thread, to `tables`.
pub fn with_tables(tables: Option<Arc<dyn SpillTables>>) -> TablesScope {
    TablesScope(TABLES.with(|t| t.replace(tables)))
}

/// A new spill table, or `None` when no spill engine is in force.
pub fn create() -> Option<Result<Box<dyn SpillTable>, String>> {
    let tables = TABLES.with(|t| t.borrow().clone())?;
    Some(tables.create())
}
