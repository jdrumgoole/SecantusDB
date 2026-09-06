//! The one reason-an-engine-stopped type, shared by every engine in this crate.
//!
//! It used to be six separate unit structs — `expressions::Fallback`,
//! `query::Fallback`, and one each in `update` / `projection` / `diff` /
//! `aggregate` — that all meant the same thing and could carry nothing. Two
//! consequences, both visible to a user of the **standalone Rust server**:
//!
//! 1. Every seam between two engines had to write `.map_err(|_| Fallback::Defer)`,
//!    which erased whatever the inner engine knew.
//! 2. There was no way to say "mongod REFUSES this input, and here is its exact
//!    code and message". An operator that had to *raise* could only defer, and
//!    a defer on the Rust server is not a fallback — there is no Python behind
//!    it — so it surfaced as the generic `BadValue` "not supported by the Rust
//!    server". Probed against 8.2.11 (2026-09-01), roughly two dozen ordinary
//!    error shapes came back that way: `{$round: ["$n", 1.5]}`,
//!    `{$range: [0, 5, 0]}`, `{$ln: 0}`, `{$substrCP: ["abc", -1, 2]}` and so on
//!    all told the client the server could not do `$round` / `$range` / `$ln`,
//!    when in fact the server can and it was the *argument* that was bad.
//!
//! One type with a payload fixes both: seams propagate with plain `?`, and
//! [`Fallback::Mongo`] carries the real error to the wire.

/// Why an engine could not produce a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fallback {
    /// The construct is not reproducible in Rust and the caller should use the
    /// pure-Python engine instead. On the standalone Rust server there is no
    /// Python, so this is what surfaces as "not supported by the Rust server".
    Defer,
    /// mongod rejects this input, with exactly this code and message. Returned
    /// to the client verbatim rather than as a "not supported" error.
    ///
    /// `folded` records WHICH of mongod's two pipeline wrappers the message
    /// belongs under: `Some(true)` for a wholly constant expression, which
    /// mongod rejects at optimization time ("Failed to optimize pipeline"), and
    /// `Some(false)` for one that reads a field, which fails per document
    /// ("Executor error during aggregate command..."). `None` means nothing has
    /// decided yet; the expression evaluator stamps it at the operator that
    /// raised, so the verdict follows the offending sub-expression rather than
    /// the whole stage.
    Mongo {
        code: i32,
        message: String,
        folded: Option<bool>,
        /// Whether this is an EXECUTION-time update error -- one discoverable
        /// only while applying the update to a particular stored document, as
        /// opposed to a parse error readable from the update spec alone.
        /// mongod wraps exactly these in
        /// `Plan executor error during <command> :: caused by ::` and leaves
        /// parse errors bare (probed 8.2.11, 2026-09-06, for both `update` and
        /// `findAndModify`); the command layer reads this to decide. Mirrors
        /// `secantus.update.UpdateError.exec_error`.
        exec: bool,
    },
}

impl Fallback {
    /// A mongod error, ready to go to the wire. `code` is the `Location…`
    /// number; the message must be mongod's own text, probed, not paraphrased.
    pub fn mongo(code: i32, message: impl Into<String>) -> Self {
        Fallback::Mongo {
            code,
            message: message.into(),
            folded: None,
            exec: false,
        }
    }

    /// Mark this as an execution-time update error, so the command layer adds
    /// mongod's `Plan executor error during <command> :: caused by ::` wrapper.
    pub fn exec(mut self) -> Self {
        if let Fallback::Mongo { exec, .. } = &mut self {
            *exec = true;
        }
        self
    }

    /// Whether this error belongs under mongod's update-executor wrapper.
    pub fn is_exec(&self) -> bool {
        matches!(self, Fallback::Mongo { exec: true, .. })
    }

    /// Stamp which pipeline wrapper this error belongs under.
    pub fn with_folded(mut self, is_folded: bool) -> Self {
        if let Fallback::Mongo { folded, .. } = &mut self {
            *folded = Some(is_folded);
        }
        self
    }

    /// The `(code, message)` pair when this is a real server error.
    pub fn as_mongo(&self) -> Option<(i32, &str)> {
        match self {
            Fallback::Mongo { code, message, .. } => Some((*code, message.as_str())),
            Fallback::Defer => None,
        }
    }

    /// Whether this error belongs under mongod's constant-folding wrapper.
    /// Undecided counts as not folded — the executor prefix is the one that
    /// applies to anything reading a document.
    pub fn folded(&self) -> bool {
        matches!(
            self,
            Fallback::Mongo {
                folded: Some(true),
                ..
            }
        )
    }
}
