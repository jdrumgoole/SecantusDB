//! The registry of running operations: what `currentOp` and `$currentOp`
//! list and what `killOp` acts on (RM-216).
//!
//! Every command `dispatch` runs is registered for its duration under an
//! `opid`, mongod's 32-bit operation id. `killOp` marks one killed; the
//! waits that check it (a tailable `getMore`'s await) end it with mongod's
//! 11601 "operation was interrupted". The running operation is also kept in
//! a thread-local, so code with no `CommandContext` -- the `$currentOp`
//! stage, the await loop -- can reach the registry and its own flag.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bson::{doc, Bson, Document};

/// One running operation.
pub struct RunningOp {
    pub opid: i64,
    pub connection_id: i64,
    /// The client's address, `host:port`.
    pub client: Option<String>,
    pub client_metadata: Option<Document>,
    /// mongod's `op`: `command`, `query`, `getmore`, `insert`, `update` or
    /// `remove`.
    pub op: &'static str,
    pub ns: String,
    pub command: Document,
    pub started: Instant,
    killed: AtomicBool,
}

impl RunningOp {
    /// Whether `killOp` has been sent for this operation.
    pub fn killed(&self) -> bool {
        self.killed.load(Ordering::Acquire)
    }

    /// The cursor a `getMore` reads.
    pub fn cursor_id(&self) -> Option<i64> {
        (self.op == "getmore")
            .then(|| self.command.get("getMore").and_then(crate::util::as_i64))
            .flatten()
    }

    /// The operation as `currentOp` lists it, in mongod's field order.
    pub fn describe(&self, host: &str) -> Document {
        let elapsed = self.started.elapsed();
        let mut out = doc! {
            "type": "op",
            "host": host,
            "desc": format!("conn{}", self.connection_id),
            "connectionId": Bson::Int64(self.connection_id),
        };
        if let Some(client) = &self.client {
            out.insert("client", client.clone());
        }
        if let Some(meta) = &self.client_metadata {
            if let Some(name) = meta
                .get_document("application")
                .ok()
                .and_then(|a| a.get_str("name").ok())
            {
                out.insert("appName", name.to_string());
            }
            out.insert("clientMetadata", meta.clone());
        }
        out.insert("active", true);
        out.insert("opid", Bson::Int64(self.opid));
        out.insert("secs_running", Bson::Int64(elapsed.as_secs() as i64));
        out.insert("microsecs_running", Bson::Int64(elapsed.as_micros() as i64));
        out.insert("op", self.op);
        out.insert("ns", self.ns.clone());
        out.insert("command", self.command.clone());
        if let Some(id) = self.cursor_id() {
            out.insert(
                "cursor",
                doc! {"cursorId": Bson::Int64(id), "operationUsingCursorId": Bson::Int64(self.opid)},
            );
        }
        if self.killed() {
            out.insert("killPending", true);
        }
        out
    }
}

/// The server's running operations.
pub struct OpRegistry {
    next_opid: AtomicI64,
    ops: Mutex<BTreeMap<i64, Arc<RunningOp>>>,
}

impl Default for OpRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl OpRegistry {
    pub fn new() -> Self {
        OpRegistry {
            next_opid: AtomicI64::new(1),
            ops: Mutex::new(BTreeMap::new()),
        }
    }

    /// Every running operation, by opid.
    pub fn snapshot(&self) -> Vec<Arc<RunningOp>> {
        self.ops
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    /// Mark the operation `opid` killed. `false` when none is running.
    pub fn kill(&self, opid: i64) -> bool {
        match self
            .ops
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&opid)
        {
            Some(op) => {
                op.killed.store(true, Ordering::Release);
                true
            }
            None => false,
        }
    }

    /// Whether a running `getMore` is reading the cursor `id`.
    pub fn cursor_in_use(&self, id: i64) -> bool {
        self.snapshot().iter().any(|op| op.cursor_id() == Some(id))
    }
}

/// What the registered operation on this thread needs beside its context.
struct Current {
    registry: Arc<OpRegistry>,
    op: Arc<RunningOp>,
    cursors: Option<Arc<crate::cursors::CursorRegistry>>,
    host: String,
}

thread_local! {
    static CURRENT: RefCell<Option<Current>> = const { RefCell::new(None) };
}

/// Registers an operation for as long as it lives.
#[must_use = "the operation is unregistered when this guard drops"]
pub struct OpGuard {
    registry: Arc<OpRegistry>,
    opid: i64,
    previous: Option<Current>,
}

impl Drop for OpGuard {
    fn drop(&mut self) {
        self.registry
            .ops
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.opid);
        let previous = self.previous.take();
        CURRENT.with(|c| *c.borrow_mut() = previous);
    }
}

/// mongod's `op` for a command.
fn op_kind(name: &str) -> &'static str {
    match name {
        "find" => "query",
        "getMore" => "getmore",
        "insert" => "insert",
        "update" => "update",
        "delete" => "remove",
        _ => "command",
    }
}

/// Register `doc`, about to run on `ctx`'s connection.
pub fn begin(registry: &Arc<OpRegistry>, doc: &Document, ctx: &crate::CommandContext) -> OpGuard {
    let name = doc.keys().next().map(String::as_str).unwrap_or("");
    let op = op_kind(name);
    let ns = match (op, doc.get(name)) {
        ("getmore", _) => doc
            .get_str("collection")
            .map(|c| format!("{}.{c}", ctx.db_name))
            .unwrap_or_else(|_| format!("{}.$cmd", ctx.db_name)),
        (_, Some(Bson::String(coll))) if op != "command" || name == "aggregate" => {
            format!("{}.{coll}", ctx.db_name)
        }
        _ => format!("{}.$cmd", ctx.db_name),
    };
    let (client, client_metadata) = ctx
        .conn_auth
        .as_ref()
        .and_then(|a| a.lock().ok())
        .map(|g| (g.peer_address.clone(), g.client_metadata.clone()))
        .unwrap_or_default();
    let opid = registry.next_opid.fetch_add(1, Ordering::Relaxed);
    let running = Arc::new(RunningOp {
        opid,
        connection_id: ctx.connection_id,
        client,
        client_metadata,
        op,
        ns,
        command: doc.clone(),
        started: Instant::now(),
        killed: AtomicBool::new(false),
    });
    registry
        .ops
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(opid, running.clone());
    let host = match &ctx.server_address {
        Some((h, p)) => format!("{h}:{p}"),
        None => "secantus".to_string(),
    };
    let previous = CURRENT.with(|c| {
        c.borrow_mut().replace(Current {
            registry: registry.clone(),
            op: running,
            cursors: ctx.cursors.clone(),
            host,
        })
    });
    OpGuard {
        registry: registry.clone(),
        opid,
        previous,
    }
}

/// Whether the operation running on this thread has been killed.
pub fn current_killed() -> bool {
    CURRENT.with(|c| c.borrow().as_ref().is_some_and(|cur| cur.op.killed()))
}

/// The rows `$currentOp` lists: every running operation and, asked for,
/// every idle cursor -- one no running `getMore` is reading. `None` when no
/// registry is in place (an off-server context).
pub fn current_op_rows(idle_cursors: bool) -> Option<Vec<Document>> {
    CURRENT.with(|c| {
        let c = c.borrow();
        let cur = c.as_ref()?;
        let ops = cur.registry.snapshot();
        let mut rows: Vec<Document> = ops.iter().map(|op| op.describe(&cur.host)).collect();
        if idle_cursors {
            if let Some(cursors) = &cur.cursors {
                for snap in cursors.snapshot() {
                    if ops.iter().any(|op| op.cursor_id() == Some(snap.cursor_id)) {
                        continue;
                    }
                    // The registry's clock is monotonic, so there is no
                    // date to give for the last access.
                    rows.push(doc! {
                        "type": "idleCursor",
                        "host": cur.host.clone(),
                        "ns": snap.namespace.clone(),
                        "cursor": {
                            "cursorId": Bson::Int64(snap.cursor_id),
                            "tailable": snap.tailable,
                            "awaitData": snap.await_data,
                        },
                    });
                }
            }
        }
        Some(rows)
    })
}
