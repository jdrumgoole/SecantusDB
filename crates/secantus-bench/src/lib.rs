//! Three-droplet DigitalOcean benchmark harness for the SecantusDB Rust server.
//!
//! Two binaries share this library:
//!
//! * [`bin/do-cluster`] — the orchestrator. Provisions one server droplet and
//!   two client droplets, deploys the server binary and the load agent, runs a
//!   coordinated benchmark, collects the results, and parks the droplets.
//! * [`bin/do-client`] — the load agent that runs *on* each client droplet.
//!
//! Why three machines: every other harness in this repo shares one host with
//! the server, so the load generator competes with the database for the same
//! cores and page cache, and the "network" is loopback. That flatters latency
//! and caps throughput at whatever the client can drive on the leftover cores.
//!
//! The client speaks the MongoDB wire protocol directly ([`mongo`]) rather than
//! through a driver. For a *server* benchmark that is the right instrument:
//! driver overhead is the usual reason a client machine saturates before the
//! server does, and removing it moves the bottleneck back where it belongs.

pub mod argv;
pub mod cluster;
pub mod doapi;
pub mod engine;
pub mod histogram;
pub mod mongo;
pub mod opmix;
pub mod ops;
pub mod remote;
pub mod report;
pub mod timefmt;

/// Roles, which double as droplet-name suffixes and result-file keys.
pub const SERVER_ROLE: &str = "server";
pub const CLIENT_ROLES: [&str; 2] = ["client-1", "client-2"];
pub const ALL_ROLES: [&str; 3] = ["server", "client-1", "client-2"];

pub const SERVER_PORT: u16 = 27017;
pub const REMOTE_DIR: &str = "/opt/secantus-bench";

/// Where `cmd_perf` checks out and builds the repo on the server droplet.
/// Kept separate from [`REMOTE_DIR`]'s own `src/` checkout so a perf run and a
/// throughput run can share a droplet without clobbering each other's build.
pub const PERF_DIR: &str = "/opt/secantus-perf";
pub const SERVER_BIN: &str = "/usr/local/bin/secantusd-rs";
pub const SERVER_DATA: &str = "/var/lib/secantus-bench";
pub const SERVICE: &str = "secantus-bench";
pub const GITHUB_REPO: &str = "jdrumgoole/SecantusDB";

/// The error type every fallible harness step returns.
///
/// A benchmark that swallows an error reports a throughput number for a run
/// that did not happen, so every step propagates a message a human can act on
/// rather than a code.
pub type BenchError = String;
pub type BenchResult<T> = Result<T, BenchError>;
