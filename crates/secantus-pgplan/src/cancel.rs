//! A cancel that arrives while a statement is still being PARSED.
//!
//! PostgreSQL answers a `CancelRequest` at its next `CHECK_FOR_INTERRUPTS`,
//! which its raw parser reaches quickly: a cancel 0.5 s into a 1.2 MB
//! statement is 57014 at 0.515 s on 15.19. libpg_query's parser cannot be
//! interrupted from here, so a large statement is parsed on a thread of its
//! own while the planner's thread polls the backend's cancel flag, through
//! the hook the wire layer installs around planning.

use super::*;

/// Whether a cancel has arrived for this backend. It is only LOOKED at:
/// the statement's own cancellation point takes it, so a caller that
/// swallows the parse error (a lock or privilege pre-pass) cannot lose it.
pub type CancelHook<'a> = dyn Fn() -> bool + 'a;
type Hook = CancelHook<'static>;

thread_local! {
    static CANCEL_HOOK: std::cell::Cell<Option<*const Hook>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with `hook` as this thread's cancellation check.
pub fn with_cancel_hook<R>(hook: &CancelHook<'_>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<*const Hook>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CANCEL_HOOK.with(|r| r.set(self.0));
        }
    }
    // SAFETY: as `correlated::with_sequence_hook` -- only the lifetime is
    // erased, and `Restore` reinstates the previous pointer before the
    // borrow ends.
    let ptr: *const Hook =
        unsafe { std::mem::transmute::<*const CancelHook<'_>, *const Hook>(hook) };
    let _restore = Restore(CANCEL_HOOK.with(|r| r.replace(Some(ptr))));
    f()
}

/// Has a cancel arrived for the statement being planned?
pub(crate) fn pending() -> bool {
    CANCEL_HOOK.with(|h| h.get()).is_some_and(|p| {
        // SAFETY: installed by `with_cancel_hook`, whose borrow is live.
        unsafe { (*p)() }
    })
}

/// PostgreSQL's 57014 for a cancelled statement.
pub(crate) fn query_canceled() -> Error {
    Error::Sqlstate("57014", "canceling statement due to user request".into())
}

/// Run `work` on its own thread (with `stack` bytes of stack), polling for
/// a cancel while it runs; a cancel answers 57014 at once and leaves the
/// thread to finish on its own. With no hook installed, `work` just runs.
pub(crate) fn interruptible<T: Send + 'static>(
    stack: usize,
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    if CANCEL_HOOK.with(|h| h.get()).is_none() {
        return std::thread::Builder::new()
            .stack_size(stack)
            .spawn(work)
            .map_err(|e| Error::Sqlstate("54001", format!("stack depth limit exceeded: {e}")))?
            .join()
            .map_err(|_| Error::Sqlstate("XX000", "the parser thread panicked".into()))?;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .stack_size(stack)
        .spawn(move || {
            let _ = tx.send(work());
        })
        .map_err(|e| Error::Sqlstate("54001", format!("stack depth limit exceeded: {e}")))?;
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(5)) {
            Ok(out) => return out,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if pending() {
                    return Err(query_canceled());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::Sqlstate(
                    "XX000",
                    "the parser thread panicked".into(),
                ))
            }
        }
    }
}
