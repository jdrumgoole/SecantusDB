"""Line coverage of the Rust MongoDB server, from its test suites and its driver gauges.

Builds every crate of the server with LLVM source-based coverage
(``-C instrument-coverage``) and records two sets of profiles:

- **suites**: the cargo tests of the clean workspace and of the four
  WiredTiger-linked crates, then the Python suites CI runs against the
  ``secantusd-rs`` binary (the backlog's gap and regression tests,
  ``tests/mongod_live``, the binary smoke and crash-durability tests);
- **gauges**: the eleven driver gauges that run the binary as a daemon.

It merges each set, and both together, and writes ``docs-rust/coverage.md``:
lines, regions and functions for each set, per crate, and the files where the
gauges reach code the suites do not.

Run it from a CLEAN checkout with the gauge toolchains and submodules, which in
practice is the main one (``./inv rust-mdb-coverage``): the gauges run there,
and they refuse a binary built from a different tree. It takes about 45 minutes
on the 12-core Mac and needs ~20 GB free; go through ``scripts/detached_run.py``.
``mongod`` 8.2 must be on ``PATH`` (the backlog and ``mongod_live`` compare
against it), and the ``llvm-tools`` rustup component must be installed.

The gauges overwrite ``.validation/``, which the website's driver panels are
generated from, so it is copied aside first and put back afterwards. A gauge's
pass rate here means nothing: this is a debug, instrumented build. The run
records each gauge's exit and summary line only to show it ran.

Not counted: the two pymongo gauges and the Python files that use the embedded
``_secantus_server`` extension (instrumenting it needs the CMake wheel build),
and branch coverage (needs a nightly compiler). ``#[cfg(test)]`` modules inside
``src/`` count as source.

Usage: ``python scripts/rust_mdb_coverage.py [--out DIR] [--gauges LIST|none]
[--report PATH] [--keep]``
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import time

REPO = pathlib.Path(__file__).resolve().parent.parent

# The crates that make up secantusd-rs. The PG server's crates, the PyO3
# bindings, the benches and the bindgen crate are not part of it.
CRATES = (
    "secantus-auth",
    "secantus-commands",
    "secantus-core",
    "secantus-server",
    "secantus-storage",
    "secantus-storage-adapter",
    "secantus-wire",
    "secantus-wt",
    "secantusdb",
)
# Excluded from crates/Cargo.toml, so the workspace `cargo test` skips them.
WT_CRATES = ("secantus-storage", "secantus-storage-adapter", "secantusdb", "secantus-wt")
GAUGES = (
    "go",
    "java",
    "kotlin",
    "node",
    "ruby",
    "rust",
    "php_lib",
    "php_ext",
    "c",
    "cxx",
    "dotnet",
)
SETS = ("suites", "gauges", "all")
# A line in a gauge's log that carries its counts; the runners differ.
GAUGE_COUNT = r"\d+ passed|Tests passed|^Tests: \d+|Passed: +\d+|merged \d+ results"
SUITE_PY = ("pymongo==4.17.0", "pytest", "pytest-xdist", "pytest-timeout", "psycopg[binary]==3.3.4")


def log(out: pathlib.Path, msg: str) -> None:
    line = f"{time.strftime('%H:%M:%S')} {msg}"
    print(line, flush=True)
    with open(out / "status.txt", "a") as f:
        f.write(line + "\n")


def run(cmd: list[str], logfile: pathlib.Path, *, cwd: pathlib.Path, env: dict[str, str]) -> int:
    with open(logfile, "w") as f:
        return subprocess.run(cmd, cwd=cwd, env=env, stdout=f, stderr=subprocess.STDOUT).returncode


def llvm_tools() -> pathlib.Path:
    sysroot = subprocess.run(
        ["rustc", "--print", "sysroot"], capture_output=True, text=True, check=True
    ).stdout.strip()
    host = re.search(
        r"host: (\S+)", subprocess.run(["rustc", "-vV"], capture_output=True, text=True).stdout
    )
    bindir = pathlib.Path(sysroot) / "lib" / "rustlib" / (host.group(1) if host else "") / "bin"
    if not (bindir / "llvm-profdata").exists():
        sys.exit(f"no llvm-profdata in {bindir}: rustup component add llvm-tools")
    return bindir


def summary_line(logfile: pathlib.Path, pattern: str) -> str:
    """The last line of a log matching ``pattern``, for the run table."""
    try:
        lines = logfile.read_text(errors="replace").splitlines()
    except OSError:
        return "(no log)"
    hits = [ln.strip() for ln in lines if re.search(pattern, ln)]
    return hits[-1] if hits else "(no count in its log)"


def cargo_summary(logfile: pathlib.Path) -> str:
    """The sum of every ``test result:`` line: one per test binary and doctest run."""
    try:
        text = logfile.read_text(errors="replace")
    except OSError:
        return "(no log)"
    sums = {"passed": 0, "failed": 0, "ignored": 0}
    results = re.findall(
        r"^test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored", text, re.M
    )
    for passed, failed, ignored in results:
        sums["passed"] += int(passed)
        sums["failed"] += int(failed)
        sums["ignored"] += int(ignored)
    return f"{len(results)} test binaries: " + ", ".join(f"{n} {k}" for k, n in sums.items())


def junit_summary(validation: pathlib.Path, since: float) -> str | None:
    """Counts from a JUnit XML the gauge wrote in ``.validation/`` during its run,
    for a gauge whose log carries none (the C++ one)."""
    for xml in sorted(validation.glob("*.xml")):
        if xml.stat().st_mtime < since:
            continue
        attrs = re.search(r"<testsuites?\b([^>]*)>", xml.read_text(errors="replace"))
        if not attrs:
            continue
        got = dict(re.findall(r'(\w+)="(\d+)"', attrs.group(1)))
        if "tests" in got:
            keys = ("tests", "failures", "errors", "skipped")
            return f"{xml.name}: " + ", ".join(f"{got[k]} {k}" for k in keys if k in got)
    return None


# --- report ------------------------------------------------------------------


def file_summaries(export: dict, root: pathlib.Path) -> dict[str, dict[str, tuple[int, int]]]:
    """``{crate-relative path: {lines|regions|functions: (count, covered)}}`` from
    ``llvm-cov export -summary-only`` JSON, keeping only the server crates."""
    out: dict[str, dict[str, tuple[int, int]]] = {}
    for data in export["data"]:
        for f in data["files"]:
            try:
                rel = pathlib.Path(f["filename"]).resolve().relative_to(root / "crates")
            except ValueError:
                continue
            if rel.parts[0] not in CRATES or len(rel.parts) < 2 or rel.parts[1] != "src":
                continue
            s = f["summary"]
            out[rel.as_posix()] = {
                k: (s[k]["count"], s[k]["covered"]) for k in ("lines", "regions", "functions")
            }
    return out


def totals(
    files: dict[str, dict[str, tuple[int, int]]], crate: str | None = None
) -> dict[str, tuple[int, int]]:
    t = {k: [0, 0] for k in ("lines", "regions", "functions")}
    for path, s in files.items():
        if crate is None or path.split("/", 1)[0] == crate:
            for k, (n, c) in s.items():
                t[k][0] += n
                t[k][1] += c
    return {k: (n, c) for k, (n, c) in t.items()}


def pct(n: int, c: int) -> str:
    return f"{100.0 * c / n:.1f}%" if n else "-"


def render(
    sets: dict[str, dict], meta: dict, runs: list[tuple[str, int, str]], top: int = 15
) -> str:
    """The Markdown report. ``sets`` maps suites/gauges/all to ``file_summaries``."""
    have_gauges = bool(sets["gauges"])
    shown = SETS if have_gauges else ("suites",)
    names = {"suites": "Test suites", "gauges": "Driver gauges", "all": "Both"}
    lines = [
        "# Test coverage",
        "",
        "How much of the Rust MongoDB server's source its tests execute, measured with LLVM",
        "source-based coverage over an instrumented debug build. Regenerate it with",
        "`./inv rust-mdb-coverage` from a clean main checkout (about 45 minutes); the",
        "numbers describe the commit below, not the current tree.",
        "",
        f"- **Commit:** `{meta['commit']}` ({meta['date']})",
        f"- **Toolchain:** {meta['rustc']}; reference server {meta['mongod']}",
        f"- **Platform:** {meta['platform']}",
        "",
        "## Totals",
        "",
        "| | Lines | Regions | Functions |",
        "| --- | --- | --- | --- |",
    ]
    for s in shown:
        t = totals(sets[s])
        label = f"**{names[s]}**" if s == "all" else names[s]
        lines.append(
            f"| {label} | "
            + " | ".join(pct(*t[k]) for k in ("lines", "regions", "functions"))
            + " |"
        )
    n, _ = totals(sets["suites"])["lines"]
    lines += ["", f"Over {n:,} lines in {len(CRATES)} crates.", "", "## By crate (lines)", ""]
    if have_gauges:
        lines += [
            "| Crate | Lines | Suites | Gauges | Both | Added by the gauges |",
            "| --- | --- | --- | --- | --- | --- |",
        ]
    else:
        lines += ["| Crate | Lines | Suites |", "| --- | --- | --- |"]
    by_size = sorted(CRATES, key=lambda c: -totals(sets["suites"], c)["lines"][0])
    for crate in by_size:
        su = totals(sets["suites"], crate)["lines"]
        if not su[0]:
            continue
        row = f"| {crate} | {su[0]:,} | {pct(*su)} |"
        if have_gauges:
            ga = totals(sets["gauges"], crate)["lines"]
            al = totals(sets["all"], crate)["lines"]
            row += f" {pct(*ga)} | {pct(*al)} | +{al[1] - su[1]:,} |"
        lines.append(row)
    if have_gauges:
        added = []
        for path, s in sets["all"].items():
            su = sets["suites"].get(path, {"lines": (s["lines"][0], 0)})["lines"]
            gain = s["lines"][1] - su[1]
            if gain > 0:
                added.append((gain, path, su, s["lines"]))
        added.sort(key=lambda a: (-a[0], a[1]))
        lines += [
            "",
            "## Where the gauges reach code the suites do not",
            "",
            "| File | Lines added | Suites | Both |",
            "| --- | --- | --- | --- |",
        ]
        for gain, path, su, al in added[:top]:
            lines.append(f"| `{path}` | +{gain} | {pct(*su)} | {pct(*al)} |")
    best = "all" if have_gauges else "suites"
    missed = sorted(
        ((s["lines"][0] - s["lines"][1], p, s["lines"]) for p, s in sets[best].items()),
        key=lambda m: (-m[0], m[1]),
    )
    lines += [
        "",
        "## Most lines not executed",
        "",
        "| File | Lines not executed | Coverage |",
        "| --- | --- | --- |",
    ]
    for miss, path, ln in missed[:top]:
        lines.append(f"| `{path}` | {miss:,} | {pct(*ln)} |")
    lines += [
        "",
        "## What ran",
        "",
        "Exit status and summary line of each run. A gauge's pass count here is not its",
        "published rate: this is an instrumented debug build.",
        "",
        "| Run | Exit | Summary |",
        "| --- | --- | --- |",
    ]
    for name, rc, summary in runs:
        lines.append(f"| {name} | {rc} | {summary.replace('|', '/')} |")
    lines += [
        "",
        "## What is not counted",
        "",
        "- The two pymongo gauges, and the Python test files that run the server through the",
        "  embedded `_secantus_server` extension: instrumenting it needs the CMake wheel build.",
        "- Branch coverage, which needs a nightly compiler.",
        "- `#[cfg(test)]` modules inside `src/` count as source, which raises the figures",
        "  slightly.",
        "",
    ]
    return "\n".join(lines)


# --- measurement -------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument(
        "--out",
        type=pathlib.Path,
        help="work directory, outside the repository (default: a temp dir)",
    )
    ap.add_argument("--gauges", default=",".join(GAUGES), help="comma-separated gauges, or 'none'")
    ap.add_argument(
        "--repo",
        type=pathlib.Path,
        default=REPO,
        help="the checkout to measure (default: this one)",
    )
    ap.add_argument(
        "--report", type=pathlib.Path, help="default: docs-rust/coverage.md in this checkout"
    )
    ap.add_argument(
        "--keep", action="store_true", help="keep the instrumented build and the profiles"
    )
    args = ap.parse_args()
    repo = args.repo.resolve()
    report = args.report or REPO / "docs-rust" / "coverage.md"
    gauges = [] if args.gauges == "none" else [g for g in args.gauges.split(",") if g]
    unknown = sorted(set(gauges) - set(GAUGES))
    if unknown:
        sys.exit(f"unknown gauge(s): {', '.join(unknown)}")

    commit = subprocess.run(
        ["git", "rev-parse", "--short", "HEAD"], cwd=repo, capture_output=True, text=True
    ).stdout.strip()
    dirty = subprocess.run(
        ["git", "status", "--porcelain", "--untracked-files=no", "--ignore-submodules"],
        cwd=repo,
        capture_output=True,
        text=True,
    ).stdout
    if dirty:
        sys.exit(f"uncommitted changes; the report would not describe {commit}:\n{dirty}")
    if not shutil.which("mongod"):
        sys.exit("mongod 8.2 must be on PATH")
    out = (
        args.out or pathlib.Path(tempfile.gettempdir()) / f"secantus-mdb-coverage-{commit}"
    ).resolve()
    if repo in out.parents or out == repo:
        sys.exit(f"{out} is inside the repository; build output must not land there")
    out.mkdir(parents=True, exist_ok=True)
    tools = llvm_tools()
    prof = {s: out / f"prof-{s}" for s in ("suites", "gauges")}
    for p in prof.values():
        shutil.rmtree(p, ignore_errors=True)
        p.mkdir()

    env = dict(os.environ)
    env.update(
        CARGO_BUILD_JOBS=env.get("CARGO_BUILD_JOBS", "3"), CARGO_TARGET_DIR=str(out / "target")
    )
    env["RUSTFLAGS"] = (env.get("RUSTFLAGS", "") + " -C instrument-coverage").strip()
    ruby = pathlib.Path(
        "/opt/homebrew/opt/ruby/bin"
    )  # the Ruby gauge needs a newer Ruby than macOS ships
    if ruby.is_dir():
        env["PATH"] = f"{ruby}{os.pathsep}{env['PATH']}"
    binary = out / "target" / "debug" / "secantusd-rs"
    runs: list[tuple[str, int, str]] = []
    log(out, f"start at {commit}, work directory {out}")

    # The suites.
    env["LLVM_PROFILE_FILE"] = str(prof["suites"] / "%p-%m.profraw")
    crates = repo / "crates"
    rc = run(
        ["cargo", "test", "--no-fail-fast", "--", "--test-threads=4"],
        out / "t-workspace.log",
        cwd=crates,
        env=env,
    )
    runs.append(("cargo test (workspace)", rc, cargo_summary(out / "t-workspace.log")))
    for c in WT_CRATES:
        logfile = out / f"t-{c}.log"
        rc = run(
            ["cargo", "test", "--no-fail-fast", "--", "--test-threads=4"],
            logfile,
            cwd=crates / c,
            env=env,
        )
        runs.append((f"cargo test ({c})", rc, cargo_summary(logfile)))
    log(out, "cargo tests done")
    rc = run(["cargo", "build"], out / "b-binary.log", cwd=crates / "secantusdb", env=env)
    if rc or not binary.exists():
        log(out, f"instrumented binary build failed ({rc}); see {out / 'b-binary.log'}")
        return 1
    env["SECANTUSDB_BIN"] = str(binary)

    venv = out / "venv"
    py = str(venv / "bin" / "python")
    subprocess.run(["uv", "venv", "-q", "--python", "3.12", str(venv)], check=True)
    subprocess.run(["uv", "pip", "install", "-q", "--python", py, *SUITE_PY], check=True)
    # The suites start $SECANTUSDB_BIN; the override only stops the conftest
    # refusing the checkout's own, unused, debug binary and extension.
    penv = dict(env, SECANTUS_ALLOW_STALE_ARTIFACT="1")
    py_suites = [
        (
            "backlog verify --regressions",
            [
                py,
                "tools/backlog.py",
                "verify",
                "--file",
                "backlog-rust-mongodb.md",
                "--regressions",
            ],
            r"^=== ",
        ),
        (
            "tests/mongod_live",
            [
                py,
                "-m",
                "pytest",
                "tests/mongod_live",
                "-m",
                "mongod_live",
                "-o",
                "addopts=",
                "-n",
                "4",
                "-p",
                "no:cacheprovider",
                "-q",
            ],
            r" in [\d.]+s",
        ),
        (
            "binary smoke + crash durability",
            [
                py,
                "-m",
                "pytest",
                "tests/test_rust_binary_smoke.py",
                "tests/test_rust_binary_crash_durability.py",
            ]
            + ["-o", "addopts=", "-p", "no:cacheprovider", "-q"],
            r" in [\d.]+s",
        ),
    ]
    for i, (name, cmd, pattern) in enumerate(py_suites):
        logfile = out / f"p-{i}.log"
        rc = run(cmd, logfile, cwd=repo, env=penv)
        runs.append((name, rc, summary_line(logfile, pattern)))
        log(out, f"{name}: {rc}")

    # The gauges, one at a time (Go and Java are load-sensitive), on the
    # checkout's own venv, as the invoke tasks run them.
    if gauges:
        backup = out / "validation-backup"
        shutil.rmtree(backup, ignore_errors=True)
        if (repo / ".validation").exists():
            shutil.copytree(repo / ".validation", backup, symlinks=True)
        genv = dict(
            env,
            LLVM_PROFILE_FILE=str(prof["gauges"] / "%p-%m.profraw"),
            SECANTUS_GAUGE_SERVER="rust",
        )
        genv["PYTHONPATH"] = str(repo)
        try:
            for g in gauges:
                logfile = out / f"g-{g}.log"
                start = time.monotonic()
                start_wall = time.time()
                rc = run(
                    [str(repo / ".venv" / "bin" / "python"), "-m", f"{g}_validation.runner"],
                    logfile,
                    cwd=repo,
                    env=genv,
                )
                summary = summary_line(logfile, GAUGE_COUNT)
                if summary.startswith("("):
                    summary = junit_summary(repo / ".validation", start_wall) or summary
                runs.append((f"gauge {g}", rc, summary))
                log(out, f"gauge {g}: {rc} in {time.monotonic() - start:.0f}s")
        finally:
            if backup.exists():
                shutil.rmtree(repo / ".validation", ignore_errors=True)
                shutil.copytree(backup, repo / ".validation", symlinks=True)
                log(out, "restored .validation")

    # Merge and export.
    profdata = {s: out / f"{s}.profdata" for s in SETS}
    for s in ("suites", "gauges"):
        raws = sorted(str(p) for p in prof[s].glob("*.profraw"))
        if not raws:
            profdata[s] = None  # type: ignore[assignment]
            continue
        subprocess.run(
            [str(tools / "llvm-profdata"), "merge", "-sparse", *raws, "-o", str(profdata[s])],
            check=True,
        )
    if profdata["suites"] is None:
        log(out, "no suite profiles were written")
        return 1
    parts = [str(p) for p in (profdata["suites"], profdata["gauges"]) if p]
    subprocess.run(
        [str(tools / "llvm-profdata"), "merge", "-sparse", *parts, "-o", str(profdata["all"])],
        check=True,
    )
    objects = [str(binary)]
    for f in sorted((out / "target" / "debug" / "deps").iterdir()):
        if (
            f.is_file()
            and os.access(f, os.X_OK)
            and f.suffix not in (".d", ".dylib", ".rlib", ".so")
        ):
            objects += ["-object", str(f)]
    sources = [str(crates / c / "src") for c in CRATES]
    sets: dict[str, dict] = {}
    for s in SETS:
        if profdata[s] is None:
            sets[s] = {}
            continue
        flags = [f"-instr-profile={profdata[s]}"]
        with open(out / f"report-{s}.txt", "w") as f:
            subprocess.run(
                [str(tools / "llvm-cov"), "report", *objects, *flags, *sources],
                stdout=f,
                check=True,
            )
        export = subprocess.run(
            [str(tools / "llvm-cov"), "export", *objects, *flags, "-summary-only", *sources],
            capture_output=True,
            check=True,
        ).stdout
        sets[s] = file_summaries(json.loads(export), repo)
    if not gauges:
        sets["gauges"] = {}

    def version(cmd: list[str]) -> str:
        return subprocess.run(cmd, capture_output=True, text=True).stdout.splitlines()[0].strip()

    meta = {
        "commit": commit,
        "date": datetime.date.today().isoformat(),
        "rustc": version(["rustc", "--version"]),
        "mongod": version(["mongod", "--version"]),
        "platform": f"{os.uname().sysname} {os.uname().machine}",
    }
    # Summary lines name local paths; keep them relative to the checkout.
    runs = [(name, rc, s.replace(f"{repo}/", "")) for name, rc, s in runs]
    report.write_text(render(sets, meta, runs))
    log(out, f"wrote {report}")
    if not args.keep:
        for p in (
            out / "target",
            out / "venv",
            *prof.values(),
            *(p for p in profdata.values() if p),
        ):
            if p.is_dir():
                shutil.rmtree(p, ignore_errors=True)
            elif p.exists():
                p.unlink()
        log(out, "removed the instrumented build and the profiles")
    return 0


if __name__ == "__main__":
    sys.exit(main())
