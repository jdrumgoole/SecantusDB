//! `secantus-wt` — safe Rust bindings over the vendored WiredTiger C library.
//!
//! This is the storage foundation for SecantusDB's Rust engine (Phase 4 of the
//! Python→Rust rewrite). It wraps the bits of the WiredTiger C API that
//! `secantus.storage` uses — connection / session / cursor lifecycle, the key
//! formats of SecantusDB's tables (`SS`, `SSu`, `SSS`, `SSSu`, `q`, `S`, `u`),
//! error-code translation, and transactions — behind a small safe surface.
//!
//! Design notes:
//! * WiredTiger "objects" are C structs of function pointers; every call goes
//!   through `(*ptr).method.unwrap()`.
//! * Key/value packing is delegated to WiredTiger's own `set_key`/`get_key`
//!   (the variadic C entry points) rather than re-implemented in Rust — passing
//!   native typed args lets WiredTiger pack in C, avoiding the per-op packing
//!   cost that the old SWIG-in-Python path paid.
//! * `u` columns are `WT_ITEM` (raw bytes) and `S` columns are NUL-terminated
//!   strings. WiredTiger references the caller's memory for these until the next
//!   cursor operation, so the `Cursor` **owns** the key/value buffers it hands to
//!   WiredTiger (see its `*_hold` fields) — callers pass borrowed slices/strings
//!   and don't have to manage lifetimes. On `get_*` the returned bytes/strings
//!   are copied out (the WiredTiger-owned buffer is valid only until the next op).

#![allow(clippy::missing_safety_doc)]

mod sys {
    #![allow(
        non_upper_case_globals,
        non_camel_case_types,
        non_snake_case,
        dead_code
    )]
    include!(concat!(env!("OUT_DIR"), "/wt_sys.rs"));
}

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::ptr;

/// A WiredTiger error: the raw return code plus its `wiredtiger_strerror` text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WtError {
    pub code: i32,
    pub message: String,
}

impl WtError {
    fn from_code(code: i32) -> Self {
        // wiredtiger_strerror is always safe to call and returns a static string.
        let msg = unsafe { CStr::from_ptr(sys::wiredtiger_strerror(code)) }
            .to_string_lossy()
            .into_owned();
        WtError { code, message: msg }
    }

    /// The row / key was not found (`WT_NOTFOUND`).
    pub fn is_not_found(&self) -> bool {
        self.code == sys::WT_NOTFOUND
    }
    /// The table / file does not exist (`ENOENT`) — e.g. opening a cursor on a
    /// lazily-created shard table that has never been written. Distinct from
    /// [`is_not_found`](Self::is_not_found), which is a missing *row*. Read /
    /// scan / merge paths use this to treat an absent shard as empty.
    pub fn is_missing_table(&self) -> bool {
        self.code == 2 // ENOENT (POSIX): WT surfaces a missing table's file this way
    }
    /// A unique-key conflict (`WT_DUPLICATE_KEY`).
    pub fn is_duplicate_key(&self) -> bool {
        self.code == sys::WT_DUPLICATE_KEY
    }
    /// The transaction must be retried (`WT_ROLLBACK`).
    pub fn is_rollback(&self) -> bool {
        self.code == sys::WT_ROLLBACK
    }
}

impl std::fmt::Display for WtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WiredTiger error {}: {}", self.code, self.message)
    }
}
impl std::error::Error for WtError {}

pub type Result<T> = std::result::Result<T, WtError>;

fn check(code: c_int) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(WtError::from_code(code))
    }
}

fn item(bytes: &[u8]) -> sys::WT_ITEM {
    let mut it: sys::WT_ITEM = unsafe { std::mem::zeroed() };
    it.data = bytes.as_ptr() as *const std::os::raw::c_void;
    it.size = bytes.len();
    it
}

unsafe fn item_bytes(it: &sys::WT_ITEM) -> Vec<u8> {
    if it.data.is_null() || it.size == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(it.data as *const u8, it.size).to_vec()
    }
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// A WiredTiger database connection. One per process (sessions are per-thread).
pub struct Connection {
    ptr: *mut sys::WT_CONNECTION,
}

// The connection handle is safe to share across threads; WiredTiger's own MVCC
// and per-session affinity provide the actual thread-safety.
unsafe impl Send for Connection {}
unsafe impl Sync for Connection {}

impl Connection {
    /// Open (creating if needed) a WiredTiger database rooted at `home` with the
    /// given config string (e.g. `"create,cache_size=256M,log=(enabled=true)"`).
    pub fn open(home: &str, config: &str) -> Result<Connection> {
        let home_c = CString::new(home).map_err(nul_err)?;
        let cfg_c = CString::new(config).map_err(nul_err)?;
        let mut ptr: *mut sys::WT_CONNECTION = ptr::null_mut();
        check(unsafe {
            sys::wiredtiger_open(home_c.as_ptr(), ptr::null_mut(), cfg_c.as_ptr(), &mut ptr)
        })?;
        Ok(Connection { ptr })
    }

    /// Open a new session (thread-affine — open one per thread).
    pub fn open_session(&self) -> Result<Session> {
        let open = unsafe { (*self.ptr).open_session }.unwrap();
        let mut sptr: *mut sys::WT_SESSION = ptr::null_mut();
        check(unsafe { open(self.ptr, ptr::null_mut(), ptr::null(), &mut sptr) })?;
        Ok(Session { ptr: sptr })
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            if let Some(close) = unsafe { (*self.ptr).close } {
                unsafe { close(self.ptr, ptr::null()) };
            }
            self.ptr = ptr::null_mut();
        }
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A WiredTiger session: the unit of table creation, cursors and transactions.
/// Sessions are not `Sync` — keep one per thread.
pub struct Session {
    ptr: *mut sys::WT_SESSION,
}

// A `Session` can be **moved** between threads (it owns its `WT_SESSION`), but
// WiredTiger forbids *concurrent* use from two threads. We mark it `Send` (not
// `Sync`) so a multi-document transaction's dedicated session can travel across
// the connection threads that carry the transaction's statements + its
// retryable commit — the per-transaction mutex in the command layer's
// `TransactionRegistry` guarantees only one thread ever touches it at a time.
unsafe impl Send for Session {}

impl Session {
    /// Create a table / index object, e.g.
    /// `create("table:secantus_documents", "key_format=SSu,value_format=u")`.
    pub fn create(&self, name: &str, config: &str) -> Result<()> {
        let name_c = CString::new(name).map_err(nul_err)?;
        let cfg_c = CString::new(config).map_err(nul_err)?;
        let create = unsafe { (*self.ptr).create }.unwrap();
        check(unsafe { create(self.ptr, name_c.as_ptr(), cfg_c.as_ptr()) })
    }

    /// Open a cursor over `uri` (e.g. `"table:secantus_documents"`).
    pub fn open_cursor(&self, uri: &str, config: Option<&str>) -> Result<Cursor> {
        let uri_c = CString::new(uri).map_err(nul_err)?;
        let cfg_c = match config {
            Some(c) => Some(CString::new(c).map_err(nul_err)?),
            None => None,
        };
        let cfg_ptr = cfg_c.as_ref().map_or(ptr::null(), |c| c.as_ptr());
        let open = unsafe { (*self.ptr).open_cursor }.unwrap();
        let mut cptr: *mut sys::WT_CURSOR = ptr::null_mut();
        check(unsafe {
            open(
                self.ptr,
                uri_c.as_ptr(),
                ptr::null_mut(),
                cfg_ptr,
                &mut cptr,
            )
        })?;
        Ok(Cursor {
            ptr: cptr,
            key_strs_hold: RefCell::new(Vec::new()),
            key_bytes_hold: RefCell::new(Vec::new()),
            val_bytes_hold: RefCell::new(Vec::new()),
        })
    }

    pub fn begin_transaction(&self, config: Option<&str>) -> Result<()> {
        self.txn(unsafe { (*self.ptr).begin_transaction }, config)
    }
    pub fn commit_transaction(&self, config: Option<&str>) -> Result<()> {
        self.txn(unsafe { (*self.ptr).commit_transaction }, config)
    }
    pub fn rollback_transaction(&self, config: Option<&str>) -> Result<()> {
        self.txn(unsafe { (*self.ptr).rollback_transaction }, config)
    }
    pub fn checkpoint(&self, config: Option<&str>) -> Result<()> {
        self.txn(unsafe { (*self.ptr).checkpoint }, config)
    }

    /// Why WiredTiger rolled this session's transaction back
    /// (`WT_SESSION::get_rollback_reason`), or None when it has no reason to
    /// report.
    ///
    /// A `WT_ROLLBACK` is not one condition: it is *either* a genuine
    /// write-write conflict with a concurrent operation *or* the engine
    /// abandoning a transaction whose own dirty content it cannot evict. Only
    /// the reason string separates them, and they deserve different errors —
    /// the first is retryable, the second is not (retrying it just rebuilds the
    /// same unevictable pile). This is the same call mongod uses to raise
    /// `TransactionTooLargeForCache`.
    ///
    /// Valid only immediately after the failing call, before any further use of
    /// the session: WiredTiger overwrites the buffer on the next operation.
    pub fn rollback_reason(&self) -> Option<String> {
        let f = unsafe { (*self.ptr).get_rollback_reason }?;
        let raw = unsafe { f(self.ptr) };
        if raw.is_null() {
            return None;
        }
        let text = unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    fn txn(
        &self,
        f: Option<unsafe extern "C" fn(*mut sys::WT_SESSION, *const c_char) -> c_int>,
        config: Option<&str>,
    ) -> Result<()> {
        let cfg_c = match config {
            Some(c) => Some(CString::new(c).map_err(nul_err)?),
            None => None,
        };
        let cfg_ptr = cfg_c.as_ref().map_or(ptr::null(), |c| c.as_ptr());
        check(unsafe { f.unwrap()(self.ptr, cfg_ptr) })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            if let Some(close) = unsafe { (*self.ptr).close } {
                unsafe { close(self.ptr, ptr::null()) };
            }
            self.ptr = ptr::null_mut();
        }
    }
}

// ---------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------

/// A WiredTiger cursor. Key/value setters mirror the table's `key_format`; it's
/// the caller's job to use the variant matching the table.
///
/// WiredTiger references the memory behind string (`S`) and `WT_ITEM` (`u`)
/// columns passed to `set_key`/`set_value` until the next cursor operation, so
/// the cursor **owns** those buffers (the `*_hold` fields) and keeps them alive
/// across `set_* -> insert/search/...`. Callers pass borrowed slices and need
/// not worry about lifetimes.
pub struct Cursor {
    ptr: *mut sys::WT_CURSOR,
    key_strs_hold: RefCell<Vec<CString>>,
    key_bytes_hold: RefCell<Vec<u8>>,
    val_bytes_hold: RefCell<Vec<u8>>,
}

/// Fetch a cursor method function pointer. Always used inside an `unsafe` block
/// (the pointer deref is the unsafe part).
macro_rules! cur_fn {
    ($self:ident, $field:ident) => {
        (*$self.ptr).$field.unwrap()
    };
}

impl Cursor {
    // --- key setters (one per table format SecantusDB uses) ---

    // Each setter calls WiredTiger's `set_key`/`set_value` with pointers into
    // freshly-owned buffers, then stows those buffers in the cursor's `*_hold`
    // cells so they outlive the operation (moving a `CString`/`Vec` keeps its
    // heap allocation at a stable address, so the pointers handed to WiredTiger
    // stay valid).

    /// `key_format=u` — a single raw byte string.
    pub fn set_key_u(&self, k: &[u8]) {
        let owned = k.to_vec();
        let it = item(&owned);
        unsafe { cur_fn!(self, set_key)(self.ptr, &it as *const sys::WT_ITEM) };
        *self.key_bytes_hold.borrow_mut() = owned;
    }
    /// `key_format=q` — a signed 64-bit integer (the oplog `seq`). Scalars are
    /// packed by value, so no buffer needs holding.
    pub fn set_key_q(&self, k: i64) {
        unsafe { cur_fn!(self, set_key)(self.ptr, k) };
    }
    /// `key_format=S` — a single NUL-terminated string (the oplog-meta key).
    pub fn set_key_s(&self, k: &str) {
        let c = cstr(k);
        unsafe { cur_fn!(self, set_key)(self.ptr, c.as_ptr()) };
        *self.key_strs_hold.borrow_mut() = vec![c];
    }
    /// `key_format=SS` — `(db, coll)` (the collections registry).
    pub fn set_key_ss(&self, a: &str, b: &str) {
        let (ca, cb) = (cstr(a), cstr(b));
        unsafe { cur_fn!(self, set_key)(self.ptr, ca.as_ptr(), cb.as_ptr()) };
        *self.key_strs_hold.borrow_mut() = vec![ca, cb];
    }
    /// `key_format=SSu` — `(db, coll, id_key_bytes)` (the documents table).
    pub fn set_key_ssu(&self, a: &str, b: &str, c: &[u8]) {
        let (ca, cb) = (cstr(a), cstr(b));
        let owned = c.to_vec();
        let it = item(&owned);
        unsafe {
            cur_fn!(self, set_key)(
                self.ptr,
                ca.as_ptr(),
                cb.as_ptr(),
                &it as *const sys::WT_ITEM,
            )
        };
        *self.key_strs_hold.borrow_mut() = vec![ca, cb];
        *self.key_bytes_hold.borrow_mut() = owned;
    }
    /// `key_format=SSq` — `(db, coll, seq)` (the natural-order index).
    pub fn set_key_ssq(&self, a: &str, b: &str, c: i64) {
        let (ca, cb) = (cstr(a), cstr(b));
        unsafe { cur_fn!(self, set_key)(self.ptr, ca.as_ptr(), cb.as_ptr(), c) };
        *self.key_strs_hold.borrow_mut() = vec![ca, cb];
    }
    /// `key_format=SSS` — `(db, coll, index_name)` (the indexes registry).
    pub fn set_key_sss(&self, a: &str, b: &str, c: &str) {
        let (ca, cb, cc) = (cstr(a), cstr(b), cstr(c));
        unsafe { cur_fn!(self, set_key)(self.ptr, ca.as_ptr(), cb.as_ptr(), cc.as_ptr()) };
        *self.key_strs_hold.borrow_mut() = vec![ca, cb, cc];
    }
    /// `key_format=SSSu` — `(db, coll, index_name, packed_bytes)` (index entries).
    pub fn set_key_sssu(&self, a: &str, b: &str, c: &str, d: &[u8]) {
        let (ca, cb, cc) = (cstr(a), cstr(b), cstr(c));
        let owned = d.to_vec();
        let it = item(&owned);
        unsafe {
            cur_fn!(self, set_key)(
                self.ptr,
                ca.as_ptr(),
                cb.as_ptr(),
                cc.as_ptr(),
                &it as *const sys::WT_ITEM,
            )
        };
        *self.key_strs_hold.borrow_mut() = vec![ca, cb, cc];
        *self.key_bytes_hold.borrow_mut() = owned;
    }

    // --- value (most SecantusDB tables use value_format=u; the natural-order
    // reverse index uses value_format=q) ---

    pub fn set_value_u(&self, v: &[u8]) {
        let owned = v.to_vec();
        let it = item(&owned);
        unsafe { cur_fn!(self, set_value)(self.ptr, &it as *const sys::WT_ITEM) };
        *self.val_bytes_hold.borrow_mut() = owned;
    }

    /// `value_format=q` — a signed 64-bit integer (the natural-order reverse
    /// index's `seq`). Scalars are packed by value, so no buffer needs holding.
    pub fn set_value_q(&self, v: i64) {
        unsafe { cur_fn!(self, set_value)(self.ptr, v) };
    }

    // --- key getters (for scans) ---

    pub fn get_key_u(&self) -> Result<Vec<u8>> {
        let mut it: sys::WT_ITEM = unsafe { std::mem::zeroed() };
        check(unsafe { cur_fn!(self, get_key)(self.ptr, &mut it as *mut sys::WT_ITEM) })?;
        Ok(unsafe { item_bytes(&it) })
    }
    pub fn get_key_q(&self) -> Result<i64> {
        let mut v: i64 = 0;
        check(unsafe { cur_fn!(self, get_key)(self.ptr, &mut v as *mut i64) })?;
        Ok(v)
    }
    pub fn get_key_ss(&self) -> Result<(String, String)> {
        let (mut a, mut b): (*const c_char, *const c_char) = (ptr::null(), ptr::null());
        check(unsafe { cur_fn!(self, get_key)(self.ptr, &mut a, &mut b) })?;
        Ok((owned(a), owned(b)))
    }
    /// `key_format=S` — a single NUL-terminated string. Used by the `backup:`
    /// cursor, whose key is each file in the consistent snapshot.
    pub fn get_key_s(&self) -> Result<String> {
        let mut a: *const c_char = ptr::null();
        check(unsafe { cur_fn!(self, get_key)(self.ptr, &mut a) })?;
        Ok(owned(a))
    }
    pub fn get_key_ssu(&self) -> Result<(String, String, Vec<u8>)> {
        let (mut a, mut b): (*const c_char, *const c_char) = (ptr::null(), ptr::null());
        let mut it: sys::WT_ITEM = unsafe { std::mem::zeroed() };
        check(unsafe {
            cur_fn!(self, get_key)(self.ptr, &mut a, &mut b, &mut it as *mut sys::WT_ITEM)
        })?;
        Ok((owned(a), owned(b), unsafe { item_bytes(&it) }))
    }
    /// `key_format=SSq` — `(db, coll, seq)` (the natural-order index).
    pub fn get_key_ssq(&self) -> Result<(String, String, i64)> {
        let (mut a, mut b): (*const c_char, *const c_char) = (ptr::null(), ptr::null());
        let mut c: i64 = 0;
        check(unsafe { cur_fn!(self, get_key)(self.ptr, &mut a, &mut b, &mut c as *mut i64) })?;
        Ok((owned(a), owned(b), c))
    }
    /// `key_format=SSS` — `(db, coll, index_name)` (the indexes registry).
    pub fn get_key_sss(&self) -> Result<(String, String, String)> {
        let (mut a, mut b, mut c): (*const c_char, *const c_char, *const c_char) =
            (ptr::null(), ptr::null(), ptr::null());
        check(unsafe { cur_fn!(self, get_key)(self.ptr, &mut a, &mut b, &mut c) })?;
        Ok((owned(a), owned(b), owned(c)))
    }
    /// `key_format=SSSu` — `(db, coll, index_name, packed_bytes)` (index entries).
    pub fn get_key_sssu(&self) -> Result<(String, String, String, Vec<u8>)> {
        let (mut a, mut b, mut c): (*const c_char, *const c_char, *const c_char) =
            (ptr::null(), ptr::null(), ptr::null());
        let mut it: sys::WT_ITEM = unsafe { std::mem::zeroed() };
        check(unsafe {
            cur_fn!(self, get_key)(
                self.ptr,
                &mut a,
                &mut b,
                &mut c,
                &mut it as *mut sys::WT_ITEM,
            )
        })?;
        Ok((owned(a), owned(b), owned(c), unsafe { item_bytes(&it) }))
    }

    pub fn get_value_u(&self) -> Result<Vec<u8>> {
        let mut it: sys::WT_ITEM = unsafe { std::mem::zeroed() };
        check(unsafe { cur_fn!(self, get_value)(self.ptr, &mut it as *mut sys::WT_ITEM) })?;
        Ok(unsafe { item_bytes(&it) })
    }

    /// `value_format=q` — the natural-order reverse index's `seq`.
    pub fn get_value_q(&self) -> Result<i64> {
        let mut v: i64 = 0;
        check(unsafe { cur_fn!(self, get_value)(self.ptr, &mut v as *mut i64) })?;
        Ok(v)
    }

    /// `value_format=S` — a single NUL-terminated string. Used by the `metadata:`
    /// cursor, whose value is each object's schema/config string (so a caller can
    /// read an on-disk table's `key_format`).
    pub fn get_value_s(&self) -> Result<String> {
        let mut a: *const c_char = ptr::null();
        check(unsafe { cur_fn!(self, get_value)(self.ptr, &mut a) })?;
        Ok(owned(a))
    }

    // --- operations ---

    pub fn insert(&self) -> Result<()> {
        check(unsafe { cur_fn!(self, insert)(self.ptr) })
    }
    pub fn update(&self) -> Result<()> {
        check(unsafe { cur_fn!(self, update)(self.ptr) })
    }
    pub fn remove(&self) -> Result<()> {
        check(unsafe { cur_fn!(self, remove)(self.ptr) })
    }
    pub fn search(&self) -> Result<()> {
        check(unsafe { cur_fn!(self, search)(self.ptr) })
    }
    /// Position at the smallest key >= the set key. Returns the comparison: <0 if
    /// the found key is less than the search key, 0 if exact, >0 if greater.
    pub fn search_near(&self) -> Result<i32> {
        let mut exact: c_int = 0;
        check(unsafe { cur_fn!(self, search_near)(self.ptr, &mut exact) })?;
        Ok(exact as i32)
    }
    /// Advance to the next row. Returns `false` at end-of-table (`WT_NOTFOUND`).
    pub fn next(&self) -> Result<bool> {
        step(unsafe { cur_fn!(self, next)(self.ptr) })
    }
    /// Step to the previous row. Returns `false` at start-of-table.
    pub fn prev(&self) -> Result<bool> {
        step(unsafe { cur_fn!(self, prev)(self.ptr) })
    }
    pub fn reset(&self) -> Result<()> {
        check(unsafe { cur_fn!(self, reset)(self.ptr) })
    }
}

impl Drop for Cursor {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            if let Some(close) = unsafe { (*self.ptr).close } {
                unsafe { close(self.ptr) };
            }
            self.ptr = ptr::null_mut();
        }
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn cstr(s: &str) -> CString {
    CString::new(s).expect("string has interior NUL")
}

fn owned(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

fn nul_err(_: std::ffi::NulError) -> WtError {
    WtError {
        code: 22, // EINVAL
        message: "argument contains an interior NUL byte".to_string(),
    }
}

/// Translate a `next`/`prev` return into a "more rows?" bool, mapping
/// `WT_NOTFOUND` to `false` and any other non-zero code to an error.
fn step(code: c_int) -> Result<bool> {
    if code == 0 {
        Ok(true)
    } else if code == sys::WT_NOTFOUND {
        Ok(false)
    } else {
        Err(WtError::from_code(code))
    }
}
