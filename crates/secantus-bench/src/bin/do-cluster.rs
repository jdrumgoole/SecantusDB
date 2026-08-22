//! The orchestrator: provision three droplets, deploy, benchmark, tear down.
//!
//! See `bench/DO_CLUSTER.md` for the full operator guide.

use std::process::ExitCode;

use secantus_bench::argv::Args;
use secantus_bench::cluster::{
    Config, DEFAULT_CLIENT_SIZE, DEFAULT_IMAGE, DEFAULT_PREFIX, DEFAULT_REGION, DEFAULT_SERVER_SIZE,
};
use secantus_bench::doapi::{token_from_env, Api};
use secantus_bench::engine::Engine;
use secantus_bench::ops::{self, Opts};
use secantus_bench::remote::default_ssh_key;
use secantus_bench::{BenchResult, ALL_ROLES};

const BOOL_FLAGS: [&str; 7] = [
    "--fresh",
    "--sync-on-commit",
    "--standalone",
    "--keep-data",
    "--keep-server-running",
    "--purge-snapshots",
    "--no-suspend",
];

const VALUE_FLAGS: [&str; 29] = [
    "--prefix",
    "--region",
    "--server-size",
    "--client-size",
    "--image",
    "--ssh-key",
    "--ssh-cidr",
    "--server-build",
    "--server-version",
    "--server-ref",
    "--perf-n",
    "--perf-reps",
    "--perf-writers",
    "--agent-ref",
    "--engine",
    "--mongod-version",
    "--repeat",
    "--payload",
    "--duration",
    "--workers",
    "--op-mix",
    "--doc-bytes",
    "--batch-size",
    "--preload",
    "--cache-size",
    "--server-flags",
    "--start-delay",
    "--mode",
    "--deploy",
];

const USAGE: &str = r#"do-cluster — three-droplet DigitalOcean benchmark: SecantusDB vs MongoDB

USAGE
  do-cluster <command> [options]

COMMANDS
  up            Create the three droplets, or power on / restore ones that exist.
  resume        Alias for `up`.
  deploy        Install the server binary and build/distribute the load agent.
  run           Run the timed benchmark and collect the results.
  all           up -> deploy (only what's missing) -> run -> suspend.
  perf          Per-operation latency + concurrent-writer scaling, on the server
                droplet. Refreshes bench/results/{latency,concurrency}.json.
  suspend       Tear the cluster down (see --mode). Default: destroy.
  destroy       Alias for `suspend --mode destroy`.
  status        What exists, its power state, and the live hourly cost.
  ssh <role>    Open a shell on a droplet (server | client-1 | client-2).

CLUSTER OPTIONS (all commands)
  --prefix NAME        Resource name prefix and tag        [secantus-bench]
  --region SLUG        DigitalOcean region                 [lon1]
  --server-size SLUG   Server droplet plan                 [c-4]
  --client-size SLUG   Client droplet plan                 [c-2]
  --image SLUG         Base image                          [ubuntu-24-04-x64]
  --ssh-key PATH       Private key      [~/.ssh/secantus-bench, id_ed25519, id_rsa]
  --ssh-cidr CIDR      Who may SSH in           [this machine's public IP /32]

PROVISIONING (up, resume, all)
  --fresh              Ignore existing snapshots; provision from the base image.

DEPLOY (deploy, all)
  --server-build MODE  release | source                    [release]
  --server-version TAG Release tag for `release`           [latest secantusdb-v*]
  --server-ref REF     Pushed git ref for `source`         [HEAD]
  --agent-ref REF      Pushed git ref the load agent builds from     [HEAD]

PERF (perf)
  --perf-n N           Documents per latency workload             [10000]
  --perf-reps N        Reps to median over per workload           [5]
  --perf-writers LIST  Writer counts for the scaling sweep        [1,2,4,8]
  --server-ref REF     Pushed git ref to build and measure        [HEAD]
  (--duration and --repeat set the sweep's seconds-per-point and interleaved runs)

ENGINES (deploy, run, all)
  --engine WHICH       both (default) | secantus | mongod | a comma list.
                       `both` runs SecantusDB then MongoDB back-to-back on the
                       same droplets and prints a side-by-side comparison.
  --mongod-version V   MongoDB major version to install               [8.0]

WORKLOAD (run, all)
  --duration SECS      Timed seconds                       [120]
  --repeat N           Measurement passes; engines are interleaved within
                       each pass and the report gives medians + spread   [1]
  --workers N          Load threads per client droplet     [16]
  --op-mix SPEC        e.g. insert=100 or insert=70,find=20,update=10
  --doc-bytes N        Payload bytes per document          [8192]
  --payload KIND       repeat | random. Both engines compress, so `repeat`
                       measures the compressor; use `random` whenever
                       storage volume or cross-engine fairness matters. [repeat]
  --batch-size N       Documents per insert                [1]
  --preload N          Docs preloaded per worker           [10000]
  --cache-size SIZE    WiredTiger cache            [half the droplet's RAM]
  --sync-on-commit     Start the server with --sync-on-commit
  --standalone         Start the server with --standalone
  --server-flags STR   Extra flags appended to secantusd-rs
  --keep-data          Do not wipe the server data directory first
  --start-delay SECS   Lead time before the shared start barrier   [20]
  --keep-server-running  Leave the server up after the run

TEARDOWN (suspend, destroy, all)
  --mode MODE          destroy | snapshot | power-off       [destroy]
  --purge-snapshots    With --mode destroy, delete this cluster's snapshots too
  --no-suspend         (all) leave the droplets running afterwards
  --deploy WHEN        (all) auto | always | never          [auto]

BILLING
  DigitalOcean bills a droplet for EXISTING, not for running: a powered-off
  droplet costs exactly as much as a running one. That is why the default
  teardown is `destroy`. `--mode snapshot` keeps the installed software as a
  cheap image so the next run skips the deploy; `--mode power-off` resumes in
  seconds but keeps charging full price.

ENVIRONMENT
  DIGITALOCEAN_TOKEN   Required (also DO_TOKEN, DIGITALOCEAN_ACCESS_TOKEN, DO_API_TOKEN).
  GITHUB_TOKEN         Optional; only lifts GitHub's anonymous rate limit.
  SECANTUS_BENCH_RESULTS  Where run artifacts land   [bench/results/do]
  SECANTUS_BENCH_STATE    known_hosts + scratch      [bench/.do-state]
"#;

fn build_opts(args: &Args) -> BenchResult<Opts> {
    let ssh_key = match args.str_or("--ssh-key", "") {
        s if s.is_empty() => default_ssh_key(),
        s => std::path::PathBuf::from(shellexpand_home(&s)),
    };
    Ok(Opts {
        cfg: Config {
            prefix: args.str_or("--prefix", DEFAULT_PREFIX),
            region: args.str_or("--region", DEFAULT_REGION),
            server_size: args.str_or("--server-size", DEFAULT_SERVER_SIZE),
            client_size: args.str_or("--client-size", DEFAULT_CLIENT_SIZE),
            image: args.str_or("--image", DEFAULT_IMAGE),
            ssh_key,
            ssh_cidr: args.str_or("--ssh-cidr", ""),
        },
        fresh: args.has("--fresh"),
        server_build: args.str_or("--server-build", "release"),
        server_version: args.str_or("--server-version", "latest"),
        server_ref: args.str_or("--server-ref", ""),
        agent_ref: args.str_or("--agent-ref", ""),
        engines: Engine::parse_list(&args.str_or("--engine", "both"))?,
        mongod_version: args.str_or("--mongod-version", "8.0"),
        repeat: args.usize_or("--repeat", 1)?.max(1),
        payload: args.str_or("--payload", "repeat"),
        duration: args.f64_or("--duration", 120.0)?,
        workers: args.usize_or("--workers", 16)?,
        op_mix: args.str_or("--op-mix", "insert=70,find=20,update=10"),
        doc_bytes: args.usize_or("--doc-bytes", 8192)?,
        batch_size: args.usize_or("--batch-size", 1)?,
        preload: args.i64_or("--preload", 10_000)?,
        cache_size: args.str_or("--cache-size", ""),
        sync_on_commit: args.has("--sync-on-commit"),
        standalone: args.has("--standalone"),
        server_flags: args.str_or("--server-flags", ""),
        keep_data: args.has("--keep-data"),
        start_delay: args.f64_or("--start-delay", 20.0)?,
        keep_server_running: args.has("--keep-server-running"),
        // Destroy by default: a powered-off droplet still bills at full price,
        // so "park it until next time" has to mean "stop paying for it".
        mode: args.str_or("--mode", "destroy"),
        purge_snapshots: args.has("--purge-snapshots"),
        deploy: args.str_or("--deploy", "auto"),
        suspend_after: !args.has("--no-suspend"),
        perf_n: args.usize_or("--perf-n", 10_000)?,
        perf_reps: args.usize_or("--perf-reps", 5)?.max(1),
        perf_writers: args.str_or("--perf-writers", "1,2,4,8"),
    })
}

fn shellexpand_home(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => path.to_string(),
    }
}

fn dispatch(command: &str, args: &Args) -> BenchResult<()> {
    let mut opts = build_opts(args)?;
    if !["destroy", "snapshot", "power-off"].contains(&opts.mode.as_str()) {
        return Err(format!(
            "--mode {:?} is not one of destroy | snapshot | power-off",
            opts.mode
        ));
    }
    if !["release", "source"].contains(&opts.server_build.as_str()) {
        return Err(format!(
            "--server-build {:?} is not release or source",
            opts.server_build
        ));
    }
    let api = Api::new(token_from_env()?);
    match command {
        "up" | "resume" => cmd_up_only(&api, &mut opts),
        "deploy" => {
            let nodes = secantus_bench::cluster::discover(&api, &opts.cfg)?;
            ops::cmd_deploy(&api, &opts, &nodes)
        }
        "run" => {
            let nodes = secantus_bench::cluster::discover(&api, &opts.cfg)?;
            ops::cmd_run(&api, &opts, &nodes).map(|_| ())
        }
        "all" => ops::cmd_all(&api, &mut opts),
        "perf" => ops::cmd_perf(&api, &mut opts),
        "suspend" => ops::cmd_suspend(&api, &opts),
        "destroy" => {
            opts.mode = args.str_or("--mode", "destroy");
            ops::cmd_suspend(&api, &opts)
        }
        "status" => ops::cmd_status(&api, &opts),
        "ssh" => {
            let role = args.positional.first().cloned().unwrap_or_default();
            if !ALL_ROLES.contains(&role.as_str()) {
                return Err(format!("ssh needs a role: {}", ALL_ROLES.join(" | ")));
            }
            ops::cmd_ssh(&api, &opts, &role)
        }
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    }
}

fn cmd_up_only(api: &Api, opts: &mut Opts) -> BenchResult<()> {
    ops::cmd_up(api, opts).map(|_| ())
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() || argv[0] == "--help" || argv[0] == "-h" || argv[0] == "help" {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if argv[0] == "--version" {
        println!("do-cluster {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let args = match Args::parse(&argv, &BOOL_FLAGS, &VALUE_FLAGS) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let command = args.command.clone();
    match dispatch(&command, &args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            // Only worth saying for commands that can leave droplets behind —
            // and never for a failure that happened before the API was reached.
            let provisions = matches!(
                command.as_str(),
                "up" | "resume" | "deploy" | "run" | "all" | "perf"
            );
            if provisions && !e.starts_with("No DigitalOcean API token") {
                eprintln!(
                    "\nIf droplets were created before this failed they are still allocated and \
                     billing — run `do-cluster status` to see them, `do-cluster suspend` to remove \
                     them."
                );
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `--flag` documented in USAGE must be registered in BOOL_FLAGS or
    /// VALUE_FLAGS.
    ///
    /// The parser rejects unknown flags outright (accepting one would silently
    /// swallow the following flag as its value), so a flag that is documented
    /// but unregistered fails at run time with "unknown flag" -- after the
    /// operator has typed a command they had every reason to believe was
    /// valid. That is exactly how `--perf-n` shipped: added to USAGE and to
    /// Opts, but not to VALUE_FLAGS, and the gap only surfaced when a droplet
    /// run was invoked for real.
    #[test]
    fn every_documented_flag_is_registered() {
        let mut missing = Vec::new();
        for line in USAGE.lines() {
            for token in line.split_whitespace() {
                // Only the flag column, not prose mentions: USAGE indents flag
                // definitions, so a flag token starts the trimmed line.
                if !token.starts_with("--") || token.len() < 4 {
                    continue;
                }
                if line.trim_start() != line && line.trim_start().starts_with(token) {
                    let name = token.trim_end_matches(',');
                    if !BOOL_FLAGS.contains(&name) && !VALUE_FLAGS.contains(&name) {
                        missing.push(name.to_string());
                    }
                }
            }
        }
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "documented in USAGE but not registered as a flag: {missing:?}"
        );
    }

    /// The `perf` subcommand's own flags parse end to end.
    #[test]
    fn perf_flags_parse() {
        let argv: Vec<String> = [
            "perf",
            "--perf-n",
            "5000",
            "--perf-reps",
            "3",
            "--perf-writers",
            "1,2,4",
            "--mode",
            "destroy",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let args = Args::parse(&argv, &BOOL_FLAGS, &VALUE_FLAGS).expect("perf flags must parse");
        assert_eq!(args.command, "perf");
        assert_eq!(args.usize_or("--perf-n", 10_000).unwrap(), 5_000);
        assert_eq!(args.usize_or("--perf-reps", 5).unwrap(), 3);
        assert_eq!(args.str_or("--perf-writers", "1,2,4,8"), "1,2,4");
    }
}
