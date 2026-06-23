//! Shared real-WiredTiger test scaffolding for the command-layer integration
//! tests. Every command test now drives `dispatch` over a real `WtStorage`
//! (via `StorageAdapter`) — there is no in-memory `FakeStorage` any more.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use secantus_commands::{CommandContext, CursorRegistry, Storage as CmdStorage};
use secantus_storage::Storage as WtStorage;
use secantus_storage_adapter::StorageAdapter;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-cmdwt-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `body` with a `CommandContext` (db `t`) backed by a fresh real-WT store.
/// The WT connection is closed and the temp dir removed afterwards.
pub fn with_wt(body: impl FnOnce(&mut CommandContext)) {
    let dir = temp_home();
    let wt = Arc::new(WtStorage::open(dir.to_str().unwrap()).unwrap());
    let adapter: Arc<dyn CmdStorage> = Arc::new(StorageAdapter::new(wt));
    let mut ctx = CommandContext::new(1)
        .with_storage(adapter)
        .with_cursors(Arc::new(CursorRegistry::new()));
    ctx.db_name = "t".into();
    body(&mut ctx);
    drop(ctx); // release the Arc<WtStorage> so the WT connection closes
    let _ = std::fs::remove_dir_all(&dir);
}
