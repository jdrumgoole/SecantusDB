//! A statement does not see what the user code it called wrote (RP-473).
//!
//! PostgreSQL reads a statement's tables at the statement's snapshot and
//! command id: a volatile function it calls runs its own statements at
//! later command ids, so the rows they insert are invisible to the calling
//! statement and the rows they update or delete are still seen as they
//! were (`HeapTupleSatisfiesMVCC`'s `cmin >= curcid` test). Here the
//! transaction's writes are visible to all its reads at once, so the
//! storage keeps, per running statement, each row its calls wrote as it
//! was before them ([`secantus_storage::StatementViews`]) and lays those
//! rows over the statement's own reads.
//!
//! The levels follow the statements: a client statement starts with none
//! ([`PgHandler::reset_statement_views`]), each statement a function body
//! runs is one ([`StatementLevel`]), and each call of a volatile user
//! function records for the level calling it ([`ViewedCall`]).

use super::*;

impl PgHandler {
    /// A client statement starts: what an earlier one's calls wrote is no
    /// longer hidden from anything.
    pub(crate) fn reset_statement_views(&self) {
        if let Some(held) = self.undo_held() {
            held.lock().unwrap_or_else(|e| e.into_inner()).views.reset();
        }
    }
}

/// A statement a function body runs, for its duration: its reads see the
/// rows earlier statements of the body wrote, and only its own calls'
/// writes are hidden from it.
pub(crate) struct StatementLevel(Option<(secantus_storage::HeldRows, usize)>);

impl StatementLevel {
    pub(crate) fn enter(h: &PgHandler) -> Self {
        Self(h.undo_held().map(|held| {
            let token = held
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .views
                .enter_statement();
            (held, token)
        }))
    }
}

impl Drop for StatementLevel {
    fn drop(&mut self) {
        if let Some((held, token)) = self.0.take() {
            held.lock()
                .unwrap_or_else(|e| e.into_inner())
                .views
                .leave_statement(token);
        }
    }
}

/// A call of a volatile user function, for its duration: the rows it
/// writes are recorded as they were, for the calling statement's reads.
pub(crate) struct ViewedCall(Option<(secantus_storage::HeldRows, usize)>);

impl ViewedCall {
    pub(crate) fn enter(h: &PgHandler) -> Self {
        Self(h.undo_held().map(|held| {
            let token = held
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .views
                .enter_call();
            (held, token)
        }))
    }
}

impl Drop for ViewedCall {
    fn drop(&mut self) {
        if let Some((held, token)) = self.0.take() {
            held.lock()
                .unwrap_or_else(|e| e.into_inner())
                .views
                .leave_call(token);
        }
    }
}
