//! A cancel that arrives while a statement is still being PARSED.
//!
//! PostgreSQL answers a `CancelRequest` at its next `CHECK_FOR_INTERRUPTS`,
//! which its raw parser reaches quickly: a cancel 0.5 s into a 1.2 MB
//! statement is 57014 at 0.515 s on 15.19. libpg_query's parser cannot be
//! interrupted from here, so a large statement is parsed on a thread of its
//! own while the planner's thread polls the backend's cancel flag, through
//! the hook the wire layer installs around planning.

use super::*;

/// Whether the statement must stop -- a cancel arrived for this backend, or
/// its `statement_timeout` passed -- as the message its 57014 carries. It
/// is only LOOKED at: the statement's own cancellation point takes a
/// cancel, so a caller that swallows the parse error (a lock or privilege
/// pre-pass) cannot lose it.
pub type CancelHook<'a> = dyn Fn() -> Option<&'static str> + 'a;
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

/// Why the statement being planned must stop, if it must.
fn reason() -> Option<&'static str> {
    CANCEL_HOOK.with(|h| h.get()).and_then(|p| {
        // SAFETY: installed by `with_cancel_hook`, whose borrow is live.
        unsafe { (*p)() }
    })
}

/// Has a cancel arrived for the statement being planned (or its timeout
/// passed)?
pub(crate) fn pending() -> bool {
    reason().is_some()
}

/// A cancellation point between planning passes, as PostgreSQL's
/// `CHECK_FOR_INTERRUPTS` is one through parse analysis and planning. Each
/// pass walks the whole statement, so a large one (a 200,000-item `IN`
/// list) plans for seconds; without these a cancel that missed the parse
/// waited for all of them, and every repeat of a statement misses it,
/// because the parse is memoised.
#[track_caller]
pub(crate) fn check() -> Result<()> {
    let why = reason();
    if trace_on() {
        // A check that stops is always traced; one that lets the plan go on
        // only after a gap of 20 ms or more since this thread's last check,
        // which is the stretch a cancel could not interrupt. Every check
        // would be tens of thousands of lines a statement.
        type Last = Option<(std::time::Instant, &'static std::panic::Location<'static>)>;
        thread_local! {
            static LAST_CHECK: std::cell::Cell<Last> = const { std::cell::Cell::new(None) };
        }
        let at = std::panic::Location::caller();
        let now = std::time::Instant::now();
        let last = LAST_CHECK.with(|c| c.replace(Some((now, at))));
        let gap = last.map(|(t, _)| now - t);
        let long = gap.is_some_and(|g| g >= std::time::Duration::from_millis(20));
        if why.is_some() || long {
            let short = |l: &std::panic::Location<'_>| {
                format!(
                    "{}:{}",
                    l.file().rsplit("crates/").next().unwrap_or_default(),
                    l.line()
                )
            };
            trace(format_args!(
                "plan check {} {} ({} ms since the check at {})",
                short(at),
                if why.is_some() { "STOP" } else { "go" },
                gap.map_or(0, |g| g.as_millis()),
                last.map_or_else(|| "-".to_string(), |(_, l)| short(l))
            ));
        }
    }
    match why {
        Some(message) => Err(Error::Sqlstate("57014", message.into())),
        None => Ok(()),
    }
}

thread_local! {
    /// Whether the expression walks are cancellation points too
    /// (`with_walk_checks`), and how many nodes they have visited.
    static WALK_CHECKS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static WALK_NODES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Run `f` -- a top-level plan or Describe, which the caller follows with
/// its own cancellation point -- with every expression walk a cancellation
/// point too, one in each 2,048 nodes. Each rewrite pass walks the whole
/// statement, and over a 200,000-item `IN` list one pass was 50 to 100 ms
/// in which no cancel was seen (RP-202). A walk a cancel cuts short leaves
/// its pass working from part of the tree; nothing it decides survives,
/// because the caller's own check raises the cancel (which a walk only looks
/// at) before the plan is used. Never on while a statement EXECUTES, where a
/// value computed from part of a tree could be sent.
pub fn with_walk_checks<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            WALK_CHECKS.with(|w| w.set(self.0));
        }
    }
    let _restore = Restore(WALK_CHECKS.with(|w| w.replace(true)));
    f()
}

/// A walk's cancellation point (`with_walk_checks`).
#[track_caller]
pub(crate) fn walk_point() -> Result<()> {
    if !WALK_CHECKS.with(|w| w.get()) {
        return Ok(());
    }
    let n = WALK_NODES.with(|c| {
        let n = c.get().wrapping_add(1);
        c.set(n);
        n
    });
    if n.is_multiple_of(2048) {
        check()
    } else {
        Ok(())
    }
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
    trace(format_args!("parse thread spawning"));
    std::thread::Builder::new()
        .stack_size(stack)
        .spawn(move || {
            trace(format_args!("parse thread started"));
            let out = work();
            trace(format_args!("parse thread finished"));
            let _ = tx.send(out);
            trace(format_args!("parse thread answered"));
        })
        .map_err(|e| Error::Sqlstate("54001", format!("stack depth limit exceeded: {e}")))?;
    let mut last = std::time::Instant::now();
    loop {
        let got = rx.recv_timeout(std::time::Duration::from_millis(5));
        if trace_on() {
            let gap = last.elapsed();
            if gap > std::time::Duration::from_millis(50) {
                trace(format_args!(
                    "parse wait polled after {} ms",
                    gap.as_millis()
                ));
            }
            last = std::time::Instant::now();
        }
        match got {
            Ok(out) => {
                trace(format_args!("parse waited out"));
                return out;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(message) = reason() {
                    trace(format_args!("parse abandoned on a cancel"));
                    return Err(Error::Sqlstate("57014", message.into()));
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

/// The cancel trace: with `SECANTUS_PG_CANCEL_TRACE` naming a file, every
/// cancellation point, the parse thread, the cancel request and the 57014
/// append a line to it -- microseconds since the Unix epoch, the thread,
/// the event -- so a cancel honoured late shows what ran between the
/// request and the answer. Off (one relaxed load) without the variable.
/// The timing tests turn it on and print it when a budget is missed
/// (RP-571, RP-748): those failures come from Linux CI and do not happen
/// on demand.
static TRACE: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
    std::sync::OnceLock::new();

fn trace_file() -> Option<&'static std::sync::Mutex<std::fs::File>> {
    TRACE
        .get_or_init(|| {
            let path = std::env::var_os("SECANTUS_PG_CANCEL_TRACE")?;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
                .map(std::sync::Mutex::new)
        })
        .as_ref()
}

/// Whether the cancel trace is on.
pub fn trace_on() -> bool {
    trace_file().is_some()
}

/// One line of the cancel trace (`trace_on`); nothing when it is off.
pub fn trace(event: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let Some(file) = trace_file() else {
        return;
    };
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or_default();
    let thread = std::thread::current();
    let line = format!(
        "{micros} {}/{:?} {event}\n",
        thread.name().unwrap_or("-"),
        thread.id()
    );
    // One write per line, so lines from several threads do not interleave.
    let mut f = file.lock().unwrap_or_else(|e| e.into_inner());
    let _ = f.write_all(line.as_bytes());
}
