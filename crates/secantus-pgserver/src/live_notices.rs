//! Notices sent while a statement is still running.
//!
//! PostgreSQL sends a `NoticeResponse` the moment it is raised, so a client
//! sees `RAISE NOTICE` output from a function that then waits on a lock --
//! and pgjdbc's StatementTest acts on it (it cancels the statement, or
//! releases the lock it waits on, once the notice arrives). This server
//! queues notices and sends them when the statement ends, which is fine for
//! a statement that ends; for one that WAITS, the queue is drained at the
//! wait's polls through the sink installed here.
//!
//! The statement runs synchronously (`block_in_place`) inside the handler
//! call that holds the client exclusively, so the sink is that client,
//! reached for the duration of the call and no longer.

use super::*;

/// Sends notices to the client of the running statement.
pub(crate) type SendNotices = Box<dyn FnMut(Vec<ErrorInfo>) + Send>;

/// Installs a [`SendNotices`] on the handler for one handler call; dropping
/// it removes it.
pub(crate) struct LiveNotices<'a> {
    h: &'a PgHandler,
}

impl<'a> LiveNotices<'a> {
    pub(crate) fn install<C>(h: &'a PgHandler, client: &mut C) -> Self
    where
        C: Sink<PgWireBackendMessage> + Unpin + Send,
    {
        let ptr = client as *mut C as usize;
        let send = move |notices: Vec<ErrorInfo>| {
            let Ok(handle) = tokio::runtime::Handle::try_current() else {
                return;
            };
            // SAFETY: the guard is dropped before the handler call that
            // lends `client` returns, and that call does not touch `client`
            // while the statement runs -- this is the only use meanwhile.
            let client = unsafe { &mut *(ptr as *mut C) };
            let deliver = async move {
                for info in notices {
                    if client
                        .send(PgWireBackendMessage::NoticeResponse(info.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            };
            tokio::task::block_in_place(|| handle.block_on(deliver));
        };
        let boxed: Box<dyn FnMut(Vec<ErrorInfo>) + Send + '_> = Box::new(send);
        // SAFETY: as above -- the closure never outlives this guard, which
        // `Drop` takes it back out of the handler.
        let boxed: SendNotices = unsafe { std::mem::transmute(boxed) };
        *h.live_notices.lock().unwrap_or_else(|e| e.into_inner()) = Some(boxed);
        Self { h }
    }
}

impl Drop for LiveNotices<'_> {
    fn drop(&mut self) {
        self.h
            .live_notices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}

impl PgHandler {
    /// Send the queued notices now, when a statement is running with a
    /// client to send them to. A no-op otherwise: they go at its end.
    pub(crate) fn send_live_notices(&self) {
        let mut live = self.live_notices.lock().unwrap_or_else(|e| e.into_inner());
        let Some(send) = live.as_mut() else {
            return;
        };
        let notices = std::mem::take(
            &mut *self
                .pending_notices
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        if !notices.is_empty() {
            send(notices);
        }
    }
}
