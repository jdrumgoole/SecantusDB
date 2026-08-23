//! The subcommands: `up`, `deploy`, `run`, `suspend`, `status`, `ssh`, `all`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

use crate::cluster::{
    auto_cache_size, detect_public_ip, discover, droplet_action, ensure_firewall, ensure_ssh_key,
    ensure_vpc, newest_snapshot, node_for, normalise_ssh_cidr, wait_action, wait_for_status,
    wait_ssh, Config, Node, CLOUD_INIT,
};
use crate::doapi::{github_json, resolve_release_asset, Api};
use crate::engine::Engine;
use crate::remote::{self, in_parallel, remote_script, scp_from, scp_to, ssh, ssh_raw};
use crate::report::{
    build_summary, render_comparison, render_summary, ClientReport, ClientsInfo, EngineRuns, Rtt,
    SampleSummary, ServerInfo, Summary, SummaryInputs, WorkloadInfo,
};
use crate::timefmt::{iso8601, now_epoch_secs, run_id};
use crate::BenchResult;
use crate::{ALL_ROLES, CLIENT_ROLES, GITHUB_REPO, PERF_DIR, REMOTE_DIR, SERVER_BIN};
use crate::{SERVER_PORT, SERVER_ROLE};

/// Everything the subcommands read, assembled from the CLI by the binary.
#[derive(Debug, Clone)]
pub struct Opts {
    pub cfg: Config,
    pub fresh: bool,
    pub server_build: String,
    pub server_version: String,
    pub server_ref: String,
    pub agent_ref: String,
    pub engines: Vec<Engine>,
    pub mongod_version: String,
    pub repeat: usize,
    pub payload: String,
    pub duration: f64,
    pub workers: usize,
    pub op_mix: String,
    pub doc_bytes: usize,
    pub batch_size: usize,
    pub preload: i64,
    pub cache_size: String,
    pub sync_on_commit: bool,
    pub standalone: bool,
    pub server_flags: String,
    pub keep_data: bool,
    pub start_delay: f64,
    pub keep_server_running: bool,
    pub mode: String,
    pub purge_snapshots: bool,
    pub deploy: String,
    pub suspend_after: bool,
    /// Documents per workload for `bench.compare_servers` (its `--n`).
    pub perf_n: usize,
    /// Reps to median over for `bench.compare_servers` (its `--reps`).
    pub perf_reps: usize,
    /// Writer counts for `bench.concurrency` (its `--writers`).
    pub perf_writers: String,
}

pub fn results_root() -> PathBuf {
    std::env::var("SECANTUS_BENCH_RESULTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| remote::repo_root().join("bench/results/do"))
}

// -- remote scripts ---------------------------------------------------------

const DEPLOY_SERVER_RELEASE: &str = r#"
set -euo pipefail
mkdir -p "$REMOTE_DIR"
curl -fsSL "$TARBALL_URL" -o /tmp/secantusdb.tar.gz
curl -fsSL "$SHA_URL" -o /tmp/secantusdb.sha256
expected=$(awk '{print $1}' /tmp/secantusdb.sha256)
actual=$(sha256sum /tmp/secantusdb.tar.gz | awk '{print $1}')
if [ "$expected" != "$actual" ]; then
  echo "checksum mismatch for $TARBALL_URL: expected $expected got $actual" >&2
  exit 1
fi
rm -rf /tmp/secantus-unpack && mkdir -p /tmp/secantus-unpack
tar -xzf /tmp/secantusdb.tar.gz -C /tmp/secantus-unpack
bin=$(find /tmp/secantus-unpack -maxdepth 3 -type f -name secantusd-rs | head -1)
if [ -z "$bin" ]; then echo "no secantusd-rs in the archive" >&2; exit 1; fi
install -m 0755 "$bin" "$SERVER_BIN"
"$SERVER_BIN" --version | tee "$REMOTE_DIR/VERSION"
echo "installed from $TAG"
"#;

const DEPLOY_SERVER_SOURCE: &str = r#"
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
# liblz4-dev is required: lz4 is the default block compressor, so the
# vendored WiredTiger build sets HAVE_BUILTIN_EXTENSION_LZ4 and fails
# configure without it.
apt-get install -y -qq build-essential cmake ninja-build swig clang libclang-dev llvm-dev \
  python3-dev git pkg-config liblz4-dev >/dev/null
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
. "$HOME/.cargo/env"
if ! command -v uv >/dev/null 2>&1; then
  curl -LsSf https://astral.sh/uv/install.sh | sh
fi
export PATH="$HOME/.local/bin:$PATH"
mkdir -p "$REMOTE_DIR"
rm -rf "$REMOTE_DIR/src"
git clone --quiet "https://github.com/$REPO.git" "$REMOTE_DIR/src"
cd "$REMOTE_DIR/src"
git checkout --quiet "$SERVER_REF"
git submodule update --init --depth 1 vendor/wiredtiger
# The same path CI proves green: the storage-engine wheel build drops
# libwiredtiger.a + headers under build/*/wt-build, which build.rs then links.
SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON uv build --wheel --out-dir dist-storage
wt=$(ls -d "$PWD"/build/*/wt-build 2>/dev/null | head -1)
if [ -z "$wt" ] || [ ! -f "$wt/include/wiredtiger.h" ]; then
  echo "vendored WiredTiger not found under build/*/wt-build" >&2
  exit 1
fi
export SECANTUS_WT_INCLUDE="$wt/include" SECANTUS_WT_LIB="$wt"
cargo build --release --locked --manifest-path crates/secantusdb/Cargo.toml
bin=$(find crates/secantusdb/target -maxdepth 3 -type f -name secantusd-rs | head -1)
install -m 0755 "$bin" "$SERVER_BIN"
"$SERVER_BIN" --version | tee "$REMOTE_DIR/VERSION"
echo "built from $SERVER_REF"
"#;

/// Build the load agent on one client droplet. The binary is then pulled back
/// and pushed to the other droplets, so only one machine pays the compile and
/// every machine runs a byte-identical agent.
const BUILD_AGENT: &str = r#"
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq build-essential git pkg-config >/dev/null
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
. "$HOME/.cargo/env"
mkdir -p "$REMOTE_DIR"
rm -rf "$REMOTE_DIR/src"
git clone --quiet --filter=blob:none --no-checkout "https://github.com/$REPO.git" "$REMOTE_DIR/src"
cd "$REMOTE_DIR/src"
git fetch --quiet --depth 1 origin "$AGENT_REF"
git checkout --quiet FETCH_HEAD
cd crates
cargo build --release --locked -p secantus-bench --bin do-client
install -m 0755 target/release/do-client "$REMOTE_DIR/do-client"
"$REMOTE_DIR/do-client" --version
"#;

/// Install MongoDB Community from the official apt repository.
///
/// The comparison is only worth anything against a real `mongod` from
/// MongoDB's own packages — not a distro fork and not a container image with
/// its own tuning. The version is pinned by `--mongod-version` so a rerun
/// months later compares against the same thing.
const INSTALL_MONGOD: &str = r#"
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
if command -v mongod >/dev/null 2>&1; then
  mongod --version | head -1
  echo "mongod already installed"
  exit 0
fi
apt-get install -y -qq gnupg curl ca-certificates >/dev/null
curl -fsSL "https://pgp.mongodb.com/server-${MONGOD_VERSION}.asc" \
  | gpg --dearmor -o "/usr/share/keyrings/mongodb-server-${MONGOD_VERSION}.gpg"
codename=$(. /etc/os-release && echo "$VERSION_CODENAME")
echo "deb [ arch=amd64,arm64 signed-by=/usr/share/keyrings/mongodb-server-${MONGOD_VERSION}.gpg ] \
https://repo.mongodb.org/apt/ubuntu ${codename}/mongodb-org/${MONGOD_VERSION} multiverse" \
  > "/etc/apt/sources.list.d/mongodb-org-${MONGOD_VERSION}.list"
apt-get update -qq
apt-get install -y -qq mongodb-org >/dev/null
# The distro unit would fight the one this harness installs.
systemctl stop mongod 2>/dev/null || true
systemctl disable mongod 2>/dev/null || true
mongod --version | head -1
"#;

/// Provision the server droplet to run the *Python* benchmark harnesses.
///
/// `docs/benchmark.md` and the website's performance page publish two figures
/// that this harness could not previously produce: per-operation latency
/// (`bench.compare_servers`) and concurrent-writer scaling
/// (`bench.concurrency`). Both were measured on a developer laptop, where a
/// Spotlight indexer or a parallel build silently moves every number -- one
/// such run made *mongod itself* 2.5x slower than its own baseline. A quiet,
/// dedicated droplet is the only place these are worth measuring.
///
/// Both harnesses drive all three engines over loopback on ONE machine, which
/// is what makes them per-operation engine measurements rather than network
/// measurements. So this runs entirely on the server droplet; the client
/// droplets are not involved.
const PERF_PROVISION: &str = r#"
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
# liblz4-dev is required: lz4 is the default block compressor, so the
# vendored WiredTiger build sets HAVE_BUILTIN_EXTENSION_LZ4 and fails
# configure without it.
apt-get install -y -qq build-essential cmake ninja-build swig clang libclang-dev llvm-dev \
  python3-dev git pkg-config liblz4-dev >/dev/null
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
. "$HOME/.cargo/env"
if ! command -v uv >/dev/null 2>&1; then
  curl -LsSf https://astral.sh/uv/install.sh | sh
fi
export PATH="$HOME/.local/bin:$PATH"
mkdir -p "$REMOTE_DIR"
rm -rf "$PERF_DIR"
git clone --quiet "https://github.com/$REPO.git" "$PERF_DIR"
cd "$PERF_DIR"
git checkout --quiet "$PERF_REF"
git submodule update --init --depth 1 vendor/wiredtiger
# One build produces both halves the harnesses need: the `secantus` Python
# package and the `_secantus_server` extension (the embedded Rust server).
SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON uv build --wheel --out-dir dist-perf
uv venv --python 3.12 .venv-perf
VIRTUAL_ENV="$PERF_DIR/.venv-perf" uv pip install --quiet "$(ls dist-perf/*.whl | head -1)" pymongo
# `bench.concurrency` drives the standalone binary, not the embedded handle.
wt=$(ls -d "$PWD"/build/*/wt-build 2>/dev/null | head -1)
if [ -z "$wt" ] || [ ! -f "$wt/include/wiredtiger.h" ]; then
  echo "vendored WiredTiger not found under build/*/wt-build" >&2
  exit 1
fi
export SECANTUS_WT_INCLUDE="$wt/include" SECANTUS_WT_LIB="$wt"
cargo build --release --locked --manifest-path crates/secantusdb/Cargo.toml
bin=$(find crates/secantusdb/target -maxdepth 3 -type f -name secantusd-rs | head -1)
install -m 0755 "$bin" /usr/local/bin/secantusd-rs
"$PERF_DIR/.venv-perf/bin/python" -c "import _secantus_server as s; print('embedded server', s.__version__)"
echo "perf environment ready at $PERF_REF"
"#;

/// Run both Python harnesses on the server droplet and leave machine-readable
/// results next to each other.
///
/// The systemd units this harness installs for the throughput benchmark bind
/// the same ports these harnesses want, so they are stopped first -- and a
/// running `secantusd-rs` or `mongod` would also be competing for CPU, which
/// is precisely the contamination the droplet exists to avoid.
const PERF_RUN: &str = r#"
set -euo pipefail
export PATH="$HOME/.local/bin:$PATH"
cd "$PERF_DIR"
systemctl stop secantusd-rs 2>/dev/null || true
systemctl stop mongod 2>/dev/null || true
sleep 2
mkdir -p bench/results
PY="$PERF_DIR/.venv-perf/bin/python"
echo "=== per-operation latency ==="
"$PY" -m bench.compare_servers --n "$PERF_N" --reps "$PERF_REPS" \
  --json bench/results/latency.json
echo "=== concurrent-writer scaling ==="
"$PY" -m bench.concurrency --server all --duration "$PERF_DURATION" \
  --writers "$PERF_WRITERS" --runs "$PERF_RUNS" --json bench/results/concurrency.json
echo "perf run complete"
"#;

const UNIT_TEMPLATE: &str = r#"[Unit]
Description=SecantusDB Rust server (benchmark)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=@EXEC_START@
# Restart=no is deliberate. A database that dies mid-benchmark must fail the
# run loudly; silently restarting would hand back a throughput number averaged
# over an outage.
Restart=no
LimitNOFILE=1048576
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
"#;

const START_SERVER: &str = r#"
set -euo pipefail
systemctl daemon-reload
systemctl stop $SERVICE 2>/dev/null || true
if [ "$WIPE" = "1" ]; then rm -rf "$SERVER_DATA"; fi
mkdir -p "$SERVER_DATA"
systemctl start $SERVICE
for _ in $(seq 1 120); do
  if (exec 3<>/dev/tcp/$BIND_HOST/$PORT) 2>/dev/null; then
    echo "server listening on $BIND_HOST:$PORT"
    exit 0
  fi
  if ! systemctl is-active --quiet $SERVICE; then
    echo "server exited during startup:" >&2
    journalctl -u $SERVICE -n 50 --no-pager >&2
    exit 1
  fi
  sleep 1
done
echo "server never accepted connections on $BIND_HOST:$PORT" >&2
journalctl -u $SERVICE -n 50 --no-pager >&2
exit 1
"#;

// -- up ---------------------------------------------------------------------

pub fn cmd_up(api: &Api, opts: &mut Opts) -> BenchResult<Vec<Node>> {
    cmd_up_roles(api, opts, &ALL_ROLES)
}

/// Provision (or wake) only `roles`.
///
/// `perf` needs the server droplet alone: both Python harnesses spawn every
/// engine themselves and drive them over loopback, so a client droplet
/// contributes nothing but its hourly cost. DigitalOcean bills a droplet for
/// existing rather than running, so two idle `c-2` clients are ~$0.13/hour of
/// pure waste on a run that takes about an hour -- every release.
pub fn cmd_up_roles(api: &Api, opts: &mut Opts, roles: &[&str]) -> BenchResult<Vec<Node>> {
    // Before anything is created: prove the key can actually authenticate.
    // Finding out afterwards means paying for droplets nobody can reach.
    remote::assert_key_usable(&opts.cfg.ssh_key)?;
    opts.cfg.ssh_cidr = normalise_ssh_cidr(&opts.cfg.ssh_cidr, &detect_public_ip)?;
    let cfg = opts.cfg.clone();
    let key_id = ensure_ssh_key(api, &cfg)?;
    let vpc_id = ensure_vpc(api, &cfg)?;
    let existing = discover(api, &cfg)?;
    let snapshots = if opts.fresh {
        Vec::new()
    } else {
        api.paged("/snapshots", "snapshots", "&resource_type=droplet")?
    };

    let mut nodes: Vec<Node> = Vec::new();
    // `created` droplets are bare and need a deploy; `woken` ones already have
    // their software on disk. Telling the user to deploy after a power-off
    // resume, or after a snapshot restore, is wasted work.
    let mut created: Vec<String> = Vec::new();
    let mut woken: Vec<String> = Vec::new();
    for role in roles {
        if let Some(node) = node_for(&existing, role) {
            if node.status() == "off" {
                println!("powering on {}", node.name());
                let action = droplet_action(api, node.id(), &json!({"type": "power_on"}))?;
                wait_action(api, action, Duration::from_secs(900))?;
                woken.push(role.to_string());
            } else {
                println!("{} already {}", node.name(), node.status());
            }
            nodes.push(node.clone());
            continue;
        }
        let snap = newest_snapshot(&snapshots, &cfg.snapshot_name(role));
        let image = match &snap {
            Some(s) => s.get("id").cloned().unwrap_or(json!(cfg.image)),
            None => json!(cfg.image),
        };
        let origin = match &snap {
            Some(_) => format!("snapshot {}", cfg.snapshot_name(role)),
            None => format!("image {}", cfg.image),
        };
        println!(
            "creating {} ({}, {}) from {origin}",
            cfg.droplet_name(role),
            cfg.size_for(role),
            cfg.region
        );
        let reply = api.request(
            "POST",
            "/droplets",
            Some(&json!({
                "name": cfg.droplet_name(role),
                "region": cfg.region,
                "size": cfg.size_for(role),
                "image": image,
                "ssh_keys": [key_id],
                "vpc_uuid": vpc_id,
                "tags": [cfg.prefix, format!("{}-{role}", cfg.prefix)],
                "user_data": CLOUD_INIT,
                "monitoring": true,
                "backups": false,
                "ipv6": false,
            })),
        )?;
        let droplet = reply
            .get("droplet")
            .cloned()
            .ok_or_else(|| format!("creating {} returned no droplet", cfg.droplet_name(role)))?;
        nodes.push(Node {
            role: role.to_string(),
            droplet,
        });
        // A droplet restored from a snapshot arrives with its software, so it
        // counts as woken rather than bare.
        if snap.is_some() {
            woken.push(role.to_string());
        } else {
            created.push(role.to_string());
        }
    }

    // Close the unauthenticated-wire-port window as early as possible: the
    // firewall binds by tag, so it covers droplets that are still booting.
    ensure_firewall(api, &cfg)?;

    let mut ready: Vec<Node> = Vec::new();
    for node in &nodes {
        let droplet = wait_for_status(api, node.id(), "active", Duration::from_secs(600))?;
        let node = Node {
            role: node.role.clone(),
            droplet,
        };
        if node.private().is_empty() {
            return Err(format!(
                "{} has no private IP — the VPC assignment failed, and client->server traffic \
                 would fall back to the public interface.",
                node.name()
            ));
        }
        ready.push(node);
    }
    for role in created.iter().chain(woken.iter()) {
        if let Some(node) = node_for(&ready, role) {
            remote::forget_host(&node.public());
        }
    }

    println!("waiting for ssh ...");
    for result in in_parallel(&ready, |node| {
        wait_ssh(&cfg, &node.public(), Duration::from_secs(420))
    }) {
        result?;
    }
    println!("waiting for cloud-init ...");
    for result in in_parallel(&ready, |node| {
        ssh_raw(
            &cfg.ssh_key,
            &node.public(),
            "cloud-init status --wait >/dev/null 2>&1 || true",
        )
        .map(|_| ())
    }) {
        result?;
    }

    println!();
    println!(
        "{:<10} {:<28} {:<16} {:<16} status",
        "role", "name", "public", "private"
    );
    for node in &ready {
        println!(
            "{:<10} {:<28} {:<16} {:<16} {}",
            node.role,
            node.name(),
            node.public(),
            node.private(),
            node.status()
        );
    }
    if !created.is_empty() {
        println!(
            "\nfreshly created: {} — run `deploy` before `run`.",
            created.join(", ")
        );
    }
    if !woken.is_empty() {
        println!(
            "\nwoken with their software intact: {} — `run` directly, no deploy needed.",
            woken.join(", ")
        );
    }
    Ok(ready)
}

// -- deploy -----------------------------------------------------------------

pub fn cmd_deploy(api: &Api, opts: &Opts, nodes: &[Node]) -> BenchResult<()> {
    let cfg = &opts.cfg;
    let server = node_for(nodes, SERVER_ROLE).ok_or("missing server droplet — run `up` first.")?;
    let server_ip = server.public();

    // The load agent is built from the harness's OWN source, never from the
    // server's release tag. The two are independent — the server is what is
    // being measured, the agent is the instrument — and the agent crate does
    // not exist at older server tags at all.
    let agent_ref = if opts.agent_ref.is_empty() {
        git_head()?
    } else {
        opts.agent_ref.clone()
    };
    assert_pushed(&agent_ref)?;

    if opts.server_build == "source" {
        let git_ref = if opts.server_ref.is_empty() {
            agent_ref.clone()
        } else {
            opts.server_ref.clone()
        };
        assert_pushed(&git_ref)?;
        println!("building the server from source at {git_ref} (this takes 15-25 minutes) ...");
        remote_script(
            &cfg.ssh_key,
            &server_ip,
            DEPLOY_SERVER_SOURCE,
            &[
                ("REMOTE_DIR", REMOTE_DIR.to_string()),
                ("SERVER_BIN", SERVER_BIN.to_string()),
                ("SERVER_REF", git_ref),
                ("REPO", GITHUB_REPO.to_string()),
            ],
            true,
        )?;
    } else {
        let (tag, tarball, sha) = resolve_release_asset(&opts.server_version, &|p| github_json(p))?;
        println!("installing the server binary from release {tag}");
        remote_script(
            &cfg.ssh_key,
            &server_ip,
            DEPLOY_SERVER_RELEASE,
            &[
                ("REMOTE_DIR", REMOTE_DIR.to_string()),
                ("SERVER_BIN", SERVER_BIN.to_string()),
                ("TARBALL_URL", tarball),
                ("SHA_URL", sha),
                ("TAG", tag),
            ],
            true,
        )?;
    }

    if opts.engines.contains(&Engine::Mongod) {
        println!(
            "installing MongoDB {} on the server droplet ...",
            opts.mongod_version
        );
        remote_script(
            &cfg.ssh_key,
            &server_ip,
            INSTALL_MONGOD,
            &[("MONGOD_VERSION", opts.mongod_version.clone())],
            true,
        )?;
    }

    // Build the agent once, on the first client, then distribute it. Compiling
    // it three times would be three times the wait for a byte-identical binary.
    let builder = node_for(nodes, CLIENT_ROLES[0]).ok_or("missing client-1 droplet")?;
    println!(
        "building the load agent on {} at {agent_ref} ...",
        builder.name()
    );
    remote_script(
        &cfg.ssh_key,
        &builder.public(),
        BUILD_AGENT,
        &[
            ("REMOTE_DIR", REMOTE_DIR.to_string()),
            ("AGENT_REF", agent_ref),
            ("REPO", GITHUB_REPO.to_string()),
        ],
        true,
    )?;

    let staged = remote::state_dir().join("do-client");
    scp_from(
        &cfg.ssh_key,
        &builder.public(),
        &format!("{REMOTE_DIR}/do-client"),
        &staged,
    )?;
    let targets: Vec<&Node> = nodes.iter().filter(|n| n.role != CLIENT_ROLES[0]).collect();
    for result in in_parallel(&targets, |node| {
        let ip = node.public();
        ssh(&cfg.ssh_key, &ip, &format!("mkdir -p {REMOTE_DIR}"), false)?;
        scp_to(
            &cfg.ssh_key,
            &ip,
            &staged,
            &format!("{REMOTE_DIR}/do-client"),
        )?;
        ssh(
            &cfg.ssh_key,
            &ip,
            &format!("chmod 0755 {REMOTE_DIR}/do-client"),
            false,
        )?;
        Ok(())
    }) {
        result?;
    }
    let _ = api;
    println!("deploy complete");
    Ok(())
}

fn git_head() -> BenchResult<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| format!("running git: {e}"))?;
    if !out.status.success() {
        return Err("git rev-parse HEAD failed — is this a checkout?".to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The droplet clones from GitHub, so uncommitted work is invisible to it (and
/// unreproducible as a benchmark anyway).
fn assert_pushed(git_ref: &str) -> BenchResult<()> {
    let out = std::process::Command::new("git")
        .args(["branch", "-r", "--contains", git_ref])
        .output()
        .map_err(|e| format!("running git: {e}"))?;
    if String::from_utf8_lossy(&out.stdout).trim().is_empty() {
        return Err(format!(
            "{git_ref} is not on any remote branch, so the droplet cannot clone it. \
             Push the branch first (or pass --server-ref <pushed-sha>)."
        ));
    }
    Ok(())
}

// -- run --------------------------------------------------------------------

fn client_load_args(opts: &Opts, addr: &str, role: &str) -> String {
    format!(
        "--addr {addr} --client-id {role} --workers {} --doc-bytes {} --preload {} \
         --op-mix {} --batch-size {} --payload {}",
        opts.workers,
        opts.doc_bytes,
        opts.preload,
        remote::shell_quote(&opts.op_mix),
        opts.batch_size,
        remote::shell_quote(&opts.payload)
    )
}

/// Run the benchmark against every selected engine and report the comparison.
///
/// The engines run **sequentially on the same droplets**: same cores, same
/// network, same clients, same workload, one after the other. That is the
/// whole value of the harness — the only variable left is the database.
pub fn cmd_run(api: &Api, opts: &Opts, nodes: &[Node]) -> BenchResult<Vec<EngineRuns>> {
    for role in ALL_ROLES {
        let node =
            node_for(nodes, role).ok_or(format!("missing droplet {role} — run `up` first."))?;
        if node.status() != "active" {
            return Err(format!(
                "droplet {role} is {} — run `up` to wake it.",
                node.status()
            ));
        }
    }

    let started = now_epoch_secs();
    let run = run_id(started);
    let outdir = results_root().join(&run);
    std::fs::create_dir_all(&outdir).map_err(|e| format!("creating {}: {e}", outdir.display()))?;

    let passes = opts.repeat.max(1);
    let mut collected: Vec<EngineRuns> = opts
        .engines
        .iter()
        .map(|e| EngineRuns {
            engine: *e,
            passes: Vec::new(),
        })
        .collect();

    // Engines are interleaved WITHIN each pass rather than run to completion
    // one at a time. Thermal drift, a noisy neighbour, or anything else that
    // changes over the run then lands on both engines roughly equally instead
    // of penalising whichever went last.
    for pass in 1..=passes {
        for (idx, engine) in opts.engines.iter().enumerate() {
            if passes > 1 {
                println!("\n=== {} (pass {pass}/{passes}) ===", engine.name());
            } else {
                println!("\n=== {} ===", engine.name());
            }
            let summary = run_one_engine(opts, nodes, *engine, &run, &outdir, pass)?;
            let text = render_summary(&summary);
            println!();
            println!("{text}");
            let stem = if passes > 1 {
                format!("{}-pass{pass}", engine.name())
            } else {
                engine.name().to_string()
            };
            std::fs::write(outdir.join(format!("{stem}-summary.md")), &text)
                .map_err(|e| format!("writing summary: {e}"))?;
            std::fs::write(
                outdir.join(format!("{stem}-summary.json")),
                serde_json::to_string_pretty(&summary)
                    .map_err(|e| format!("serialising summary: {e}"))?,
            )
            .map_err(|e| format!("writing summary: {e}"))?;
            collected[idx].passes.push(summary);
        }
    }

    if collected.len() > 1 || passes > 1 {
        let comparison = render_comparison(&run, &collected);
        std::fs::write(outdir.join("comparison.md"), &comparison)
            .map_err(|e| format!("writing comparison.md: {e}"))?;
        println!();
        println!("{comparison}");
    }
    println!("\nartifacts: {}", outdir.display());
    let _ = api;
    Ok(collected)
}

/// One engine's full measurement: restart it clean, preload, load, collect.
#[allow(clippy::too_many_arguments)]
fn run_one_engine(
    opts: &Opts,
    nodes: &[Node],
    engine: Engine,
    run: &str,
    outdir: &std::path::Path,
    pass: usize,
) -> BenchResult<Summary> {
    let stem = if opts.repeat.max(1) > 1 {
        format!("{}-pass{pass}", engine.name())
    } else {
        engine.name().to_string()
    };
    let cfg = &opts.cfg;
    let server = node_for(nodes, SERVER_ROLE).expect("checked by the caller");
    let server_ip = server.public();
    let addr = format!("{}:{}", server.private(), SERVER_PORT);

    let cache_size = if opts.cache_size.is_empty() {
        auto_cache_size(server.memory_mb())
    } else {
        opts.cache_size.clone()
    };
    let mut extra = String::new();
    if opts.sync_on_commit {
        extra.push_str(engine.sync_on_commit_flag());
    }
    if opts.standalone && engine == Engine::Secantus {
        extra.push_str(" --standalone");
    }
    if !opts.server_flags.is_empty() && engine == Engine::Secantus {
        extra.push(' ');
        extra.push_str(&opts.server_flags);
    }
    let exec_start = engine.exec_start(&server.private(), SERVER_PORT, &cache_size, extra.trim());

    let unit_path = remote::state_dir().join(format!("{}.service", engine.service()));
    std::fs::write(
        &unit_path,
        UNIT_TEMPLATE
            .replace("@EXEC_START@", &exec_start)
            .replace("@DESCRIPTION@", engine.name()),
    )
    .map_err(|e| format!("writing the unit file: {e}"))?;
    scp_to(
        &cfg.ssh_key,
        &server_ip,
        &unit_path,
        &format!("/etc/systemd/system/{}.service", engine.service()),
    )?;

    // Only one engine may hold the port. Stop both, then start this one.
    ssh(
        &cfg.ssh_key,
        &server_ip,
        &format!(
            "systemctl stop {} {} 2>/dev/null || true",
            Engine::Secantus.service(),
            Engine::Mongod.service()
        ),
        false,
    )?;

    println!("starting the server: {exec_start}");
    remote_script(
        &cfg.ssh_key,
        &server_ip,
        START_SERVER,
        &[
            ("SERVICE", engine.service().to_string()),
            ("SERVER_DATA", engine.data_dir().to_string()),
            ("BIND_HOST", server.private()),
            ("PORT", SERVER_PORT.to_string()),
            (
                "WIPE",
                if opts.keep_data {
                    "0".into()
                } else {
                    "1".to_string()
                },
            ),
        ],
        true,
    )?;
    let version = engine_version(cfg, &server_ip, engine);

    let mut network: BTreeMap<String, Rtt> = BTreeMap::new();
    for role in CLIENT_ROLES {
        let node = node_for(nodes, role).expect("checked by the caller");
        let out = ssh_raw(
            &cfg.ssh_key,
            &node.public(),
            &format!("ping -c 20 -i 0.2 -q {}", server.private()),
        )?;
        if let Some(rtt) = remote::parse_ping(&out.stdout) {
            println!("{role} -> server rtt {:.3} ms avg", rtt.avg_ms);
            network.insert(role.to_string(), rtt);
        }
    }

    println!(
        "preloading {} docs x {} workers on each client ...",
        opts.preload, opts.workers
    );
    let client_nodes: Vec<&Node> = CLIENT_ROLES
        .iter()
        .map(|r| node_for(nodes, r).expect("checked by the caller"))
        .collect();
    for result in in_parallel(&client_nodes, |node| {
        let cmd = format!(
            "{REMOTE_DIR}/do-client setup {}{}",
            client_load_args(opts, &addr, &node.role),
            if opts.keep_data { " --keep-data" } else { "" }
        );
        ssh(&cfg.ssh_key, &node.public(), &cmd, true).map(|_| ())
    }) {
        result?;
    }

    let start_at = now_epoch_secs() + opts.start_delay;
    println!(
        "running {:.0}s of load, starting in {:.0}s ...",
        opts.duration, opts.start_delay
    );

    let sample_out = "/tmp/server-sample.json";
    let sample_seconds = opts.duration + opts.start_delay + 2.0;
    ssh(
        &cfg.ssh_key,
        &server_ip,
        &format!(
            "rm -f {sample_out}; nohup {REMOTE_DIR}/do-client sample \
             --duration {sample_seconds:.0} --interval 1 --process {} --out {sample_out} \
             >/tmp/sample.log 2>&1 & echo started",
            engine.process()
        ),
        false,
    )?;

    let outcomes = in_parallel(&client_nodes, |node| {
        let cmd = format!(
            "{REMOTE_DIR}/do-client run {} --duration {:.3} --start-at {:.3} --out /tmp/result.json",
            client_load_args(opts, &addr, &node.role),
            opts.duration,
            start_at
        );
        ssh_raw(&cfg.ssh_key, &node.public(), &cmd)
    });

    let mut failures: Vec<String> = Vec::new();
    for (node, outcome) in client_nodes.iter().zip(outcomes.iter()) {
        match outcome {
            Ok(out) if out.status == 0 => {
                for line in out.stdout.trim().lines() {
                    println!("  [{}] {line}", node.role);
                }
            }
            Ok(out) => failures.push(format!(
                "{}: exit {}\n{}",
                node.role,
                out.status,
                out.stderr.trim()
            )),
            Err(e) => failures.push(format!("{}: {e}", node.role)),
        }
    }

    let mut results: BTreeMap<String, ClientReport> = BTreeMap::new();
    for node in &client_nodes {
        let local = outdir.join(format!("{stem}-{}.json", node.role));
        match scp_from(&cfg.ssh_key, &node.public(), "/tmp/result.json", &local)
            .and_then(|_| std::fs::read_to_string(&local).map_err(|e| e.to_string()))
            .and_then(|text| serde_json::from_str::<ClientReport>(&text).map_err(|e| e.to_string()))
        {
            Ok(report) => {
                results.insert(node.role.clone(), report);
            }
            Err(e) => failures.push(format!("{}: no usable result file ({e})", node.role)),
        }
    }

    let mut sample = SampleSummary::default();
    let landed = remote::wait_until(
        || {
            ssh_raw(
                &cfg.ssh_key,
                &server_ip,
                &format!("test -f {sample_out} && echo ok"),
            )
            .map(|o| o.stdout.trim() == "ok")
            .unwrap_or(false)
        },
        Duration::from_secs(90),
        Duration::from_secs(2),
    );
    if !landed {
        failures.push(format!(
            "server sampler never wrote {sample_out} (see /tmp/sample.log)"
        ));
    }
    let sample_local = outdir.join(format!("{stem}-server-sample.json"));
    match scp_from(&cfg.ssh_key, &server_ip, sample_out, &sample_local)
        .and_then(|_| std::fs::read_to_string(&sample_local).map_err(|e| e.to_string()))
        .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()))
    {
        Ok(value) => {
            if let Some(summary) = value.get("summary") {
                sample = serde_json::from_value(summary.clone()).unwrap_or_default();
            }
        }
        Err(e) => failures.push(format!("server sampler produced nothing ({e})")),
    }

    let alive = ssh_raw(
        &cfg.ssh_key,
        &server_ip,
        &format!("systemctl is-active {} || true", engine.service()),
    )
    .map(|o| o.stdout.trim().to_string())
    .unwrap_or_default();
    if let Ok(journal) = ssh_raw(
        &cfg.ssh_key,
        &server_ip,
        &format!("journalctl -u {} --no-pager -n 200", engine.service()),
    ) {
        let _ = std::fs::write(outdir.join(format!("{stem}-journal.log")), journal.stdout);
    }
    if alive != "active" {
        failures.push(format!(
            "{} service is {alive:?} after the run — see {stem}-journal.log",
            engine.name()
        ));
    }

    let summary = build_summary(SummaryInputs {
        run_id: run.to_string(),
        generated_at: iso8601(now_epoch_secs()),
        engine: engine.name().to_string(),
        server: ServerInfo {
            name: server.name(),
            size: cfg.server_size.clone(),
            region: cfg.region.clone(),
            vcpus: server.vcpus(),
            memory_mb: server.memory_mb(),
            private_ip: server.private(),
            version,
            cache_size,
            exec_start,
            sample,
        },
        clients: ClientsInfo {
            size: cfg.client_size.clone(),
            count: CLIENT_ROLES.len(),
            workers_each: opts.workers,
        },
        workload: WorkloadInfo {
            duration_s: opts.duration,
            op_mix: opts.op_mix.clone(),
            doc_bytes: opts.doc_bytes,
            batch_size: opts.batch_size,
            preload_per_worker: opts.preload,
            keep_data: opts.keep_data,
        },
        network,
        results,
        failures,
    });

    if !opts.keep_server_running {
        let _ = ssh_raw(
            &cfg.ssh_key,
            &server_ip,
            &format!("systemctl stop {} || true", engine.service()),
        );
    }
    Ok(summary)
}

/// The engine's own reported version, so the report names exactly what ran.
fn engine_version(cfg: &Config, server_ip: &str, engine: Engine) -> String {
    let cmd = match engine {
        Engine::Secantus => format!("cat {REMOTE_DIR}/VERSION 2>/dev/null || true"),
        Engine::Mongod => "mongod --version 2>/dev/null | head -1 || true".to_string(),
    };
    ssh_raw(&cfg.ssh_key, server_ip, &cmd)
        .map(|o| o.stdout.trim().to_string())
        .unwrap_or_default()
}

// -- suspend / status / ssh -------------------------------------------------

/// Measure per-operation latency and concurrent-writer scaling on the server
/// droplet, then pull both results files back into `bench/results/`.
///
/// This is the droplet counterpart of `invoke compare-servers` +
/// `invoke concurrency-refresh`. Those two run on whatever machine the
/// developer happens to be sitting at, which is where the published numbers
/// have historically gone wrong: a background build or an OS indexer moves
/// every column at once, and nothing in the output says so. Here the machine
/// is dedicated and idle, and `mongod` -- measured in the same run -- is the
/// control that proves it.
///
/// Only the server droplet is used. Both harnesses spawn all three engines
/// themselves and talk to them over loopback, so a client droplet would add
/// nothing but a NIC.
pub fn cmd_perf(api: &Api, opts: &mut Opts) -> BenchResult<()> {
    // Server droplet only -- see `cmd_up_roles`. Provisioning the clients too
    // would pay for two machines this command never connects to.
    let nodes = cmd_up_roles(api, opts, &[SERVER_ROLE])?;
    let cfg = &opts.cfg;
    let server = node_for(&nodes, SERVER_ROLE).ok_or("missing server droplet — run `up` first.")?;
    let server_ip = server.public();

    let perf_ref = if opts.server_ref.is_empty() {
        git_head()?
    } else {
        opts.server_ref.clone()
    };
    assert_pushed(&perf_ref)?;

    println!("provisioning the perf environment at {perf_ref} (this takes 15-25 minutes) ...");
    remote_script(
        &cfg.ssh_key,
        &server_ip,
        PERF_PROVISION,
        &[
            ("REMOTE_DIR", REMOTE_DIR.to_string()),
            ("PERF_DIR", PERF_DIR.to_string()),
            ("PERF_REF", perf_ref.clone()),
            ("REPO", GITHUB_REPO.to_string()),
        ],
        true,
    )?;

    println!("installing mongod (the control for both harnesses) ...");
    remote_script(
        &cfg.ssh_key,
        &server_ip,
        INSTALL_MONGOD,
        &[("MONGOD_VERSION", opts.mongod_version.clone())],
        true,
    )?;

    println!("running both harnesses (this takes 30-40 minutes) ...");
    remote_script(
        &cfg.ssh_key,
        &server_ip,
        PERF_RUN,
        &[
            ("PERF_DIR", PERF_DIR.to_string()),
            ("PERF_N", opts.perf_n.to_string()),
            ("PERF_REPS", opts.perf_reps.to_string()),
            ("PERF_DURATION", opts.duration.to_string()),
            ("PERF_WRITERS", opts.perf_writers.clone()),
            ("PERF_RUNS", opts.repeat.to_string()),
        ],
        true,
    )?;

    // Land them where the chart generators already look, so the follow-up is
    // just `python -m bench.latency_chart` / `bench.concurrency_chart`.
    let local_results = remote::repo_root().join("bench/results");
    std::fs::create_dir_all(&local_results)
        .map_err(|e| format!("could not create {}: {e}", local_results.display()))?;
    for name in ["latency.json", "concurrency.json"] {
        let remote_path = format!("{PERF_DIR}/bench/results/{name}");
        let local_path = local_results.join(name);
        remote::scp_from(&cfg.ssh_key, &server_ip, &remote_path, &local_path)?;
        println!("  fetched {} -> {}", name, local_path.display());
    }

    println!(
        "\nresults written. Regenerate the published charts with:\n  \
         uv run --no-sync python -m bench.latency_chart\n  \
         uv run --no-sync python -m bench.concurrency_chart --results bench/results/concurrency.json\n\
         then review the hand-maintained prose around each chart."
    );

    if opts.suspend_after {
        cmd_suspend(api, opts)?;
    }
    Ok(())
}

pub fn power_off(api: &Api, node: &Node) -> BenchResult<()> {
    if node.status() == "off" {
        println!("{} already off", node.name());
        return Ok(());
    }
    println!("shutting down {}", node.name());
    let action = droplet_action(api, node.id(), &json!({"type": "shutdown"}))?;
    if wait_action(api, action, Duration::from_secs(180)).is_err() {
        // A graceful ACPI shutdown can hang on a busy box; the hard power-off
        // is safe here because the benchmark data directory is disposable.
        println!(
            "  {}: graceful shutdown timed out, forcing power off",
            node.name()
        );
        let action = droplet_action(api, node.id(), &json!({"type": "power_off"}))?;
        wait_action(api, action, Duration::from_secs(300))?;
    }
    wait_for_status(api, node.id(), "off", Duration::from_secs(300))?;
    Ok(())
}

/// Delete every droplet snapshot belonging to this cluster's prefix.
///
/// Snapshots outlive the droplets they came from and keep billing as storage,
/// so cleaning them up must not depend on a droplet existing -- "no droplets"
/// is exactly the state you are in when you go looking for leftovers.
fn purge_snapshots(api: &Api, cfg: &Config) -> BenchResult<usize> {
    let mut deleted = 0;
    for snap in api.paged("/snapshots", "snapshots", "&resource_type=droplet")? {
        let name = snap.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.starts_with(&cfg.prefix) {
            println!("deleting snapshot {name}");
            let id = snap.get("id").and_then(|v| v.as_str()).unwrap_or("");
            api.request("DELETE", &format!("/snapshots/{id}"), None)?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

pub fn cmd_suspend(api: &Api, opts: &Opts) -> BenchResult<()> {
    let cfg = &opts.cfg;
    let nodes = discover(api, cfg)?;
    if nodes.is_empty() {
        println!("nothing to suspend — no droplets found");
        // --purge-snapshots must still run: snapshots outlive their droplets
        // and keep billing, so this is precisely when a user reaches for it.
        // Short-circuiting here reported success while deleting nothing.
        if opts.purge_snapshots {
            let n = purge_snapshots(api, cfg)?;
            println!(
                "{}",
                match n {
                    0 => "no snapshots matched this cluster's prefix.".to_string(),
                    1 => "1 snapshot deleted; nothing is billing.".to_string(),
                    n => format!("{n} snapshots deleted; nothing is billing."),
                }
            );
        }
        return Ok(());
    }

    if opts.mode == "power-off" || opts.mode == "snapshot" {
        for result in in_parallel(&nodes, |node| power_off(api, node)) {
            result?;
        }
    }

    match opts.mode.as_str() {
        "power-off" => {
            println!(
                "\ndroplets are powered off.\n\
                 NOTE: DigitalOcean bills a droplet for existing, not for running — a powered-off\n\
                 droplet costs the same as a running one. `suspend --mode destroy` (the default)\n\
                 or `--mode snapshot` are what actually stop the meter. `status` shows the total."
            );
            Ok(())
        }
        "snapshot" => {
            let existing = api.paged("/snapshots", "snapshots", "&resource_type=droplet")?;
            for node in &nodes {
                let name = cfg.snapshot_name(&node.role);
                println!(
                    "snapshotting {} as {name} (this takes a few minutes) ...",
                    node.name()
                );
                let action =
                    droplet_action(api, node.id(), &json!({"type": "snapshot", "name": name}))?;
                wait_action(api, action, Duration::from_secs(3600))?;
            }
            for node in &nodes {
                println!("destroying {}", node.name());
                api.request("DELETE", &format!("/droplets/{}", node.id()), None)?;
            }
            // Keep exactly one snapshot per role: without this every suspend
            // cycle leaves another paid-for image behind.
            for role in ALL_ROLES {
                let name = cfg.snapshot_name(role);
                for snap in existing
                    .iter()
                    .filter(|s| s.get("name").and_then(|v| v.as_str()) == Some(name.as_str()))
                {
                    let id = snap.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    println!("removing superseded snapshot {id} ({name})");
                    api.request("DELETE", &format!("/snapshots/{id}"), None)?;
                }
            }
            println!(
                "\nsnapshotted and destroyed. `up` restores from the snapshots with the software\n\
                 still installed; billing is now snapshot storage only."
            );
            Ok(())
        }
        _ => {
            for node in &nodes {
                println!("destroying {}", node.name());
                api.request("DELETE", &format!("/droplets/{}", node.id()), None)?;
            }
            if opts.purge_snapshots {
                purge_snapshots(api, cfg)?;
            }
            println!(
                "\ndroplets destroyed. Nothing is billing. The next `up` provisions bare droplets,\n\
                 so run `deploy` before `run`."
            );
            Ok(())
        }
    }
}

pub fn cmd_status(api: &Api, opts: &Opts) -> BenchResult<()> {
    let cfg = &opts.cfg;
    let nodes = discover(api, cfg)?;
    // Prices come from the API, never from a constant in this file: a hardcoded
    // price table goes stale silently and quietly lies about cost.
    let sizes = api.paged("/sizes", "sizes", "")?;

    println!(
        "{:<10} {:<28} {:<10} {:<8} {:<16} {:>8}",
        "role", "name", "size", "status", "public", "$/hr"
    );
    let mut hourly = 0.0f64;
    for role in ALL_ROLES {
        match node_for(&nodes, role) {
            None => println!("{role:<10} {:<28}", "(absent)"),
            Some(node) => {
                let slug = node.size_slug();
                let price = sizes
                    .iter()
                    .find(|s| s.get("slug").and_then(|v| v.as_str()) == Some(slug.as_str()))
                    .and_then(|s| s.get("price_hourly"))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                hourly += price;
                println!(
                    "{:<10} {:<28} {:<10} {:<8} {:<16} {:>8.4}",
                    role,
                    node.name(),
                    slug,
                    node.status(),
                    node.public(),
                    price
                );
            }
        }
    }
    if !nodes.is_empty() {
        println!(
            "\nallocated: ${hourly:.4}/hour = ${:.2}/day (powered on OR off)",
            hourly * 24.0
        );
    }

    let all_snaps = api.paged("/snapshots", "snapshots", "&resource_type=droplet")?;
    let mine: Vec<&Value> = all_snaps
        .iter()
        .filter(|s| {
            s.get("name")
                .and_then(|v| v.as_str())
                .map(|n| n.starts_with(&cfg.prefix))
                .unwrap_or(false)
        })
        .collect();
    if !mine.is_empty() {
        println!("\nsnapshots (billed as storage while they exist):");
        for snap in &mine {
            println!(
                "  {:<36} {:>6.1} GiB  {}",
                snap.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                snap.get("size_gigabytes")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
                snap.get("created_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            );
        }
    }
    if nodes.is_empty() && mine.is_empty() {
        println!("\nnothing allocated — this cluster costs nothing right now.");
    }
    Ok(())
}

pub fn cmd_ssh(api: &Api, opts: &Opts, role: &str) -> BenchResult<()> {
    let nodes = discover(api, &opts.cfg)?;
    let node = node_for(&nodes, role).ok_or(format!("no droplet for role {role:?}"))?;
    let status = std::process::Command::new("ssh")
        .arg("-i")
        .arg(&opts.cfg.ssh_key)
        .arg("-o")
        .arg(format!(
            "UserKnownHostsFile={}",
            remote::known_hosts().display()
        ))
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg(format!("root@{}", node.public()))
        .status()
        .map_err(|e| format!("spawning ssh failed: {e}"))?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Probe the droplets rather than trusting local state: a snapshot-restored
/// cluster already has everything, a freshly created one has nothing, and only
/// the droplets themselves know which they are.
pub fn needs_deploy(cfg: &Config, nodes: &[Node]) -> bool {
    let Some(server) = node_for(nodes, SERVER_ROLE) else {
        return true;
    };
    let probe = format!("test -x {SERVER_BIN} && test -x {REMOTE_DIR}/do-client && echo ok");
    if ssh_raw(&cfg.ssh_key, &server.public(), &probe).map(|o| o.stdout.trim().to_string())
        != Ok("ok".to_string())
    {
        return true;
    }
    for role in CLIENT_ROLES {
        let Some(node) = node_for(nodes, role) else {
            return true;
        };
        let probe = format!("test -x {REMOTE_DIR}/do-client && echo ok");
        if ssh_raw(&cfg.ssh_key, &node.public(), &probe).map(|o| o.stdout.trim().to_string())
            != Ok("ok".to_string())
        {
            return true;
        }
    }
    false
}

pub fn cmd_all(api: &Api, opts: &mut Opts) -> BenchResult<()> {
    let nodes = cmd_up(api, opts)?;
    let should_deploy = match opts.deploy.as_str() {
        "always" => true,
        "never" => false,
        _ => needs_deploy(&opts.cfg, &nodes),
    };
    if should_deploy {
        cmd_deploy(api, opts, &nodes)?;
    } else {
        println!("server binary and load agents already present — skipping deploy");
    }
    let run_result = cmd_run(api, opts, &nodes);
    if opts.suspend_after {
        // Teardown runs even when the benchmark failed: a failed run that
        // leaves three droplets billing is a worse outcome than the failure.
        if let Err(e) = cmd_suspend(api, opts) {
            eprintln!("warning: teardown failed: {e}");
        }
    }
    run_result.map(|_| ())
}
