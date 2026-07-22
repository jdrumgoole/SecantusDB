"""Cross-driver smoke tests for recently-added features.

The in-tree pytest suite drives these features through pymongo only.
Wire-protocol bugs that surface only with a different driver's BSON
serialization, command shape, or error mapping wouldn't be caught.
This file plugs that gap with one canonical workload per feature, run
through each driver:

  * RBAC denial — `read`-bound user, find succeeds, insert rejected
    with code 13 / Unauthorized.
  * DDL change events — createIndex + dropIndex emit
    ``operationType: createIndexes`` / ``dropIndexes``.
  * ``updateUser`` rotation — old password rejected, new password
    accepted.
  * ``connectionStatus`` — authenticated user roles surfaced via
    ``authInfo.authenticatedUserRoles``.
  * BSON type fidelity — ObjectId / int32 / int64 / double / Decimal128
    / Date / Binary round-trip through SecantusDB without type collapse.
  * Bulk write — mixed insert / update / replace / upsert / delete in
    one ``bulkWrite``; counts and final state match per-driver.
  * Change-stream resume — ``resumeAfter`` and
    ``startAtOperationTime`` round-trip; the resume token format is
    opaque to drivers, but each driver must re-present it verbatim
    and have the server replay the right next events.
  * ``listDatabases`` filter — ``{filter: {name: <x>}}`` returns
    only the matching db; ``nameOnly: true`` strips size/empty.
  * ``batchSize: 0`` — open a cursor with empty firstBatch, fetch
    docs via the follow-up getMore.
  * ``dropAllUsersFromDatabase`` — drops only target-db users,
    returns ``n`` = removed count.
  * SCRAM-SHA-1 — user with ``mechanisms: [SCRAM-SHA-1]`` round-
    trips through the mongo-driver's legacy MD5-prepass + SHA-1
    PBKDF2 path.
  * ``postBatchResumeToken`` — change-stream cursor reply carries
    a resume token that advances on empty getMores.
  * Tailable cursor on capped collection — open with
    ``tailable: true``, see seeded docs, then see follow-up
    inserts via the polling cursor.
  * Custom roles — ``createRole`` + bind to user; verify granted
    action succeeds and ungranted action gets ``Unauthorized``;
    then ``grantPrivilegesToRole`` adds an action and a fresh
    connection picks it up.

Each test self-skips if its driver tooling isn't on PATH. Java
coverage uses ``tests/cross_driver/java/`` — a single Gradle project
that builds an uber-jar containing every smoke main class once at
first-test time and reuses it across the run, so the per-smoke cost
is just ``java -cp <jar> <FQN>`` (no Gradle re-invocation per test).
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from secantus import SecantusDBServer
from secantus.auth import SCRAM_SHA_256, derive_credentials

# Cross-driver tooling-presence skipif gates assume a POSIX environment
# (the node / java / ruby / go skipif checks pass on Windows runners
# because npm / java / ruby ARE preinstalled, but the actual smoke
# scripts use ``fcntl.flock`` for cross-xdist coordination and shell
# scripts the runner can't exec). Skip the whole module on Windows
# rather than try to port the coordination primitive — none of the
# Windows-specific bugs the cross-driver gauges would catch are
# different from what the POSIX runs already cover.
pytestmark = [
    pytest.mark.skipif(
        sys.platform == "win32",
        reason="Cross-driver smokes use fcntl.flock + POSIX shell scripts; "
        "POSIX runners (Linux/macOS) cover the wire-protocol gaps these tests "
        "are meant to catch.",
    ),
    # These smokes shell out to toolchain builds — `npm install`, a Maven/Gradle
    # jar, `bundle install`, `composer install`, `cargo build --release` — whose
    # own budgets are 300-600s. CI runs pytest with `--timeout=120
    # --timeout-method=thread`, so on a COLD cache whichever test happens to draw
    # the build blows the pytest deadline first. That method dumps to the
    # worker's stderr (discarded by xdist) and then calls os._exit(), so the
    # symptom is an anonymous "node down: Not properly terminated" naming a
    # random smoke test — the same mechanism as the ws-changes worker death
    # (tasks/backlog.md). The pytest budget must therefore exceed the build
    # budget it wraps, not sit under it.
    #
    # Trade-off accepted deliberately: a genuinely wedged test in this file now
    # takes 10 min to fail instead of 2. Every test here is subprocess-driven,
    # so wall-clock is dominated by the toolchain anyway, and a slow honest
    # failure beats a fast anonymous one.
    pytest.mark.timeout(600),
]

_HERE = Path(__file__).parent
_CROSS_DRIVER = _HERE / "cross_driver"
_NODE_SMOKE_DIR = _CROSS_DRIVER / "node"
_GO_SMOKE_DIR = _CROSS_DRIVER / "go"
_JAVA_SMOKE_DIR = _CROSS_DRIVER / "java"
_JAVA_SMOKES_JAR = _JAVA_SMOKE_DIR / "build" / "libs" / "secantus-java-smokes-all.jar"
_RUBY_SMOKE_DIR = _CROSS_DRIVER / "ruby"
_PHP_SMOKE_DIR = _CROSS_DRIVER / "php"
_RUST_SMOKE_DIR = _CROSS_DRIVER / "rust"

_MONGOSH = shutil.which("mongosh")
_NODE = shutil.which("node")
_NPM = shutil.which("npm")
_GO = shutil.which("go")
_JAVA = shutil.which("java")
_GRADLE = shutil.which("gradle")
_CARGO = shutil.which("cargo")


# Ruby + PHP toolchains. macOS's system Ruby (2.6.x at /usr/bin/ruby)
# is too old for mongo-ruby-driver, so prefer Homebrew's modern Ruby
# at /opt/homebrew/opt/ruby/bin/ruby when present. PHP follows the
# same pattern.
def _resolve_ruby() -> str | None:
    for cand in ("/opt/homebrew/opt/ruby/bin/ruby", shutil.which("ruby")):
        if cand and Path(cand).is_file():
            check = subprocess.run(
                [cand, "-e", "exit(RUBY_VERSION.to_f >= 3.0 ? 0 : 1)"],
                capture_output=True,
            )
            if check.returncode == 0:
                return cand
    return None


def _resolve_bundle(ruby_path: str | None) -> str | None:
    if ruby_path is None:
        return None
    cand = Path(ruby_path).parent / "bundle"
    if cand.is_file():
        return str(cand)
    return shutil.which("bundle")


def _resolve_php() -> str | None:
    for cand in ("/opt/homebrew/bin/php", shutil.which("php")):
        if cand and Path(cand).is_file():
            check = subprocess.run(
                [cand, "-r", "exit(extension_loaded('mongodb') ? 0 : 1);"],
                capture_output=True,
            )
            if check.returncode == 0:
                return cand
    return None


_RUBY = _resolve_ruby()
_BUNDLE = _resolve_bundle(_RUBY)
_PHP = _resolve_php()
_COMPOSER = shutil.which("composer") or "/opt/homebrew/bin/composer"
_RUBY_AVAILABLE = _RUBY is not None and _BUNDLE is not None
_PHP_AVAILABLE = _PHP is not None and Path(_COMPOSER).is_file()
_RUST_AVAILABLE = _CARGO is not None


def _run(
    cmd: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    timeout: float = 120.0,
) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout)


def _run_mongosh(
    cmd: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    timeout: float = 60.0,
    retries: int = 2,
) -> subprocess.CompletedProcess:
    """Run a mongosh subprocess with retry-on-rc1.

    Mongosh launches a full Node-based shell with its own SCRAM-SHA-256
    handshake and serverSelectionTimeoutMS=30s. Under heavy parallel
    test load the PBKDF2 round-trip can race the timeout — we see
    intermittent rc=1 with a server-side ``ConnectionResetError`` that
    a single re-attempt almost always clears. Two retries with a small
    backoff is the empirical sweet spot: enough headroom for the
    pathological cold-CPU case, not so many that a real auth failure
    spins for minutes.

    Genuine failures (auth, syntax) reproduce on the retry and still
    return rc=1 — the assertion in the caller surfaces them after the
    bounded retries elapse.
    """
    import time as _time

    last: subprocess.CompletedProcess | None = None
    for attempt in range(retries + 1):
        last = subprocess.run(
            cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout
        )
        if last.returncode == 0:
            return last
        if attempt < retries:
            _time.sleep(0.5 * (attempt + 1))
    assert last is not None
    return last


def _ensure_node_modules() -> bool:
    """Install mongodb npm package once; cached in dir for re-runs.

    Same parallel-test race as the Java jar build: multiple xdist
    workers see no ``node_modules`` on a cold checkout, all kick off
    ``npm install`` simultaneously, and the directory ends up with
    partial state (the top-level ``node_modules/`` exists from
    whichever worker finished last, but the ``mongodb`` / ``bson``
    package directories may be missing because npm clobbered them
    mid-write). Tests then fail with ``Cannot find module 'mongodb'``.
    Serialise via ``fcntl.flock`` so workers 2..N wait for the first
    to finish, then see the populated directory.
    """
    nm = _NODE_SMOKE_DIR / "node_modules"
    if nm.is_dir() and (nm / "mongodb").is_dir():
        return True
    if _NPM is None:
        return False
    import fcntl

    lock_path = _NODE_SMOKE_DIR / ".node_modules.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with open(lock_path, "w") as lock_fp:
        fcntl.flock(lock_fp.fileno(), fcntl.LOCK_EX)
        # Re-check after acquiring the lock — a sibling worker may
        # have populated it while we were blocked.
        if (nm / "mongodb").is_dir():
            return True
        result = _run([_NPM, "install", "--silent"], cwd=_NODE_SMOKE_DIR, timeout=300.0)
        return result.returncode == 0 and (nm / "mongodb").is_dir()


def _ensure_ruby_bundle() -> bool:
    """Install the mongo-ruby-driver gem set once via ``bundle install``.

    Probes the runtime by asking ``bundle exec ruby`` to require the
    ``mongo`` gem; if that succeeds, the bundle is good. Same flock
    pattern as ``_ensure_node_modules`` — multiple xdist workers
    racing on a cold ``Gemfile.lock`` would corrupt each other's gem
    extraction.
    """
    if not _RUBY_AVAILABLE:
        return False
    env = {**os.environ, "PATH": f"{Path(_RUBY).parent}:{os.environ.get('PATH', '')}"}

    def _probe() -> bool:
        return (
            _run(
                [_BUNDLE, "exec", "ruby", "-e", "require 'mongo'"],
                cwd=_RUBY_SMOKE_DIR,
                env=env,
                timeout=30.0,
            ).returncode
            == 0
        )

    if _probe():
        return True
    import fcntl

    lock_path = _RUBY_SMOKE_DIR / ".bundle.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with open(lock_path, "w") as lock_fp:
        fcntl.flock(lock_fp.fileno(), fcntl.LOCK_EX)
        if _probe():
            return True
        result = _run(
            [_BUNDLE, "install", "--quiet"],
            cwd=_RUBY_SMOKE_DIR,
            env=env,
            timeout=300.0,
        )
        return result.returncode == 0 and _probe()


def _ensure_php_vendor() -> bool:
    """``composer install`` once so ``mongodb/mongodb`` is available."""
    if not _PHP_AVAILABLE:
        return False
    nm = _PHP_SMOKE_DIR / "vendor" / "mongodb" / "mongodb"
    if nm.is_dir():
        return True
    import fcntl

    lock_path = _PHP_SMOKE_DIR / ".vendor.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with open(lock_path, "w") as lock_fp:
        fcntl.flock(lock_fp.fileno(), fcntl.LOCK_EX)
        if nm.is_dir():
            return True
        result = _run(
            [_COMPOSER, "install", "--no-interaction", "--quiet"],
            cwd=_PHP_SMOKE_DIR,
            timeout=300.0,
        )
        return result.returncode == 0 and nm.is_dir()


def _java_smokes_jar_is_fresh() -> bool:
    """True if the cached uber-jar exists AND is newer than every
    ``.java`` source under ``tests/cross_driver/java/src/``.

    A stale jar is the trap: existence alone isn't enough because
    adding a new smoke source without rebuilding leaves the file in
    place but missing the new class, so ``java -cp <jar> <FQN>``
    fails with ``ClassNotFoundException`` on a class whose source
    the developer just wrote.
    """
    if not _JAVA_SMOKES_JAR.is_file():
        return False
    jar_mtime = _JAVA_SMOKES_JAR.stat().st_mtime
    src_root = _JAVA_SMOKE_DIR / "src"
    return all(path.stat().st_mtime <= jar_mtime for path in src_root.rglob("*.java"))


def _ensure_java_smokes_jar() -> bool:
    """Build the Java smokes uber-jar; cached in build/ for re-runs.

    Under pytest-xdist parallel workers, multiple workers will hit
    this helper simultaneously on a fresh checkout. We serialise the
    build with ``fcntl.flock`` on a sentinel file so workers 2..N
    wait for worker 1 to finish, then see the cached jar. Without the
    lock, racing ``gradle smokesJar`` invocations clobber each other
    and leave a partial jar missing some smoke classes — failing tests
    with ``ClassNotFoundException`` on a class the source clearly
    defines.

    Freshness check: the jar is also rebuilt when any ``.java`` source
    under ``src/`` is newer than the jar. Without this, adding a new
    smoke source in a slice would silently fail with
    ``ClassNotFoundException`` on the not-yet-rebuilt class.

    Skipping conditions: ``java`` or ``gradle`` not on PATH, or the
    Gradle build itself fails. Returns True only when the jar is on
    disk, fresh, and runnable by ``java -cp <jar> <FQN>``.
    """
    if _java_smokes_jar_is_fresh():
        return True
    if _GRADLE is None or _JAVA is None:
        return False
    import fcntl

    lock_path = _JAVA_SMOKE_DIR / ".smokesjar.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with open(lock_path, "w") as lock_fp:
        fcntl.flock(lock_fp.fileno(), fcntl.LOCK_EX)
        # Re-check after acquiring the lock — a sibling worker may
        # have built it while we were blocked.
        if _java_smokes_jar_is_fresh():
            return True
        # Gradle 9.5 needs a JDK >= 17 toolchain; macOS dev boxes
        # usually have multiple JDKs and Gradle's default scan picks
        # the highest, so we rely on JAVA_HOME / sourceCompatibility=17
        # in build.gradle rather than mandating a toolchain block.
        result = _run(
            [_GRADLE, "smokesJar", "--no-daemon", "-q"],
            cwd=_JAVA_SMOKE_DIR,
            timeout=600.0,
        )
        return result.returncode == 0 and _JAVA_SMOKES_JAR.is_file()


def _run_java_smoke(
    fqn: str, env: dict[str, str], *, timeout: float = 120.0
) -> subprocess.CompletedProcess:
    """Invoke a Java smoke main class via the prebuilt uber-jar.

    Caller is responsible for the surrounding skipif gates and
    ``_ensure_java_smokes_jar()`` call.
    """
    return _run(
        [_JAVA, "-cp", str(_JAVA_SMOKES_JAR), fqn],
        env=env,
        timeout=timeout,
    )


# ---------------------------------------------------------------------------
# Server fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def server(tmp_path):
    """Plain (no-auth) server — used by DDL smoke."""
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        yield srv


@pytest.fixture
def server_with_noop(tmp_path):
    """Server with periodic noop oplog heartbeats enabled, mirroring mongod.

    Real mongod advances a change stream's ``postBatchResumeToken`` on a quiet
    collection through the oplog's periodic noop writes
    (``periodicNoopIntervalSecs``, 10s by default) — not through a per-getMore
    clock tick. SecantusDB's server-side PBRT only advances on genuine oplog
    movement (so an immediately-exhausted stream keeps its last event's token,
    per the change-streams spec), so the PBRT smokes — which assert the token
    advances across empty getMores on an otherwise-quiet collection — need that
    background activity. A fast 0.2s interval so the ~600 ms smoke window sees
    several heartbeats."""
    with SecantusDBServer(
        port=0, storage_path=str(tmp_path / "wt"), noop_heartbeat_seconds=0.2
    ) as srv:
        yield srv


_ADMIN_USER = "root"
_ADMIN_PWD = "rootpw"


@pytest.fixture
def server_with_auth(tmp_path):
    """Auth-enabled server with a bootstrap root/`rootpw` admin user."""
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "wt"), require_auth=True)
    srv.start()
    creds = derive_credentials(_ADMIN_PWD)
    record = {
        "_id": "admin.root",
        "user": _ADMIN_USER,
        "db": "admin",
        "credentials": creds.to_doc(),
        "roles": [{"role": "root", "db": "admin"}],
        "mechanisms": [SCRAM_SHA_256],
    }
    srv.storage.add_user("admin", _ADMIN_USER, record)
    try:
        yield srv
    finally:
        srv.stop()


# ---------------------------------------------------------------------------
# RBAC denial
# ---------------------------------------------------------------------------


_RBAC_MONGOSH_SCRIPT = """
db.getSiblingDB("shop").runCommand({
  createUser: "viewer",
  pwd: "vp",
  roles: [{ role: "read", db: "shop" }],
});
print("PROVISIONED");
"""

_RBAC_MONGOSH_VIEWER_SCRIPT = """
const items = db.getSiblingDB("shop").items;
const findRes = items.find({}).toArray();
let inserted = false;
let errCode = null;
let errMsg = null;
try {
  items.insertOne({ x: 1 });
  inserted = true;
} catch (e) {
  errCode = e.code;
  errMsg = e.errmsg || e.message || String(e);
}
print(JSON.stringify({ findCount: findRes.length, inserted, errCode, errMsg }));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
@pytest.mark.xdist_group(name="mongosh_smokes")
def test_rbac_smoke_via_mongosh(server_with_auth: SecantusDBServer) -> None:
    """RBAC denial round-trip via mongosh.

    First mongosh invocation provisions the viewer with admin creds;
    second invocation re-connects as the viewer and asserts find works
    while insert is rejected.
    """
    admin_uri = (
        f"mongodb://{_ADMIN_USER}:{_ADMIN_PWD}@127.0.0.1:{server_with_auth.port}/"
        "?authSource=admin&authMechanism=SCRAM-SHA-256"
    )
    result = _run_mongosh(
        [_MONGOSH, "--quiet", admin_uri, "--eval", _RBAC_MONGOSH_SCRIPT], timeout=60.0
    )
    assert result.returncode == 0 and "PROVISIONED" in result.stdout, (
        f"provision: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )

    viewer_uri = (
        f"mongodb://viewer:vp@127.0.0.1:{server_with_auth.port}/shop"
        "?authSource=shop&authMechanism=SCRAM-SHA-256"
    )
    result = _run_mongosh(
        [_MONGOSH, "--quiet", viewer_uri, "--eval", _RBAC_MONGOSH_VIEWER_SCRIPT],
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"viewer: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["inserted"] is False
    assert payload["errCode"] == 13 or "Unauthorized" in (payload["errMsg"] or "")


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_rbac_smoke_via_node_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "rbac_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node rbac smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_rbac_smoke_via_go_driver(server_with_auth: SecantusDBServer) -> None:
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_GO, "run", "./rbac"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go rbac smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_rbac_smoke_via_java_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run_java_smoke("com.secantus.smokes.RbacSmoke", env)
    assert result.returncode == 0, (
        f"java rbac smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# DDL change events
# ---------------------------------------------------------------------------


_DDL_MONGOSH_SCRIPT = """
const coll = db.getSiblingDB("ddl_xd").c;
coll.drop();
coll.insertOne({ _id: 1 });

const cs = coll.watch([], { maxAwaitTimeMS: 2000, showExpandedEvents: true });
sleep(300);

coll.createIndex({ x: 1 });
coll.dropIndex("x_1");

const events = [];
const deadline = Date.now() + 8000;
while (Date.now() < deadline && events.length < 2) {
  if (cs.hasNext()) {
    events.push(cs.next().operationType);
  } else {
    sleep(200);
  }
}
cs.close();
print(JSON.stringify({ events }));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
@pytest.mark.xdist_group(name="mongosh_smokes")
def test_ddl_smoke_via_mongosh(server: SecantusDBServer) -> None:
    """DDL change-stream events via mongosh."""
    result = _run_mongosh(
        [_MONGOSH, "--quiet", f"{server.uri}ddl_xd", "--eval", _DDL_MONGOSH_SCRIPT],
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"mongosh: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["events"] == ["createIndexes", "dropIndexes"]


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_ddl_smoke_via_node_driver(server: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "ddl_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node ddl smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_ddl_smoke_via_go_driver(server: SecantusDBServer) -> None:
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_GO, "run", "./ddl"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go ddl smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_ddl_smoke_via_java_driver(server: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run_java_smoke("com.secantus.smokes.DdlSmoke", env)
    assert result.returncode == 0, (
        f"java ddl smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# updateUser rotation
# ---------------------------------------------------------------------------


_UPDATEUSER_MONGOSH_PROVISION_SCRIPT = """
db.getSiblingDB("admin").runCommand({
  createUser: "alice_xd",
  pwd: "orig",
  roles: [{ role: "read", db: "admin" }],
});
db.getSiblingDB("admin").runCommand({ updateUser: "alice_xd", pwd: "rotated" });
print("PROVISIONED");
"""

_UPDATEUSER_MONGOSH_PING_SCRIPT = (
    'print(JSON.stringify({ ok: db.getSiblingDB("admin").runCommand({ ping: 1 }).ok }));'
)


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
@pytest.mark.xdist_group(name="mongosh_smokes")
def test_updateuser_smoke_via_mongosh(server_with_auth: SecantusDBServer) -> None:
    """updateUser rotation via mongosh."""
    admin_uri = (
        f"mongodb://{_ADMIN_USER}:{_ADMIN_PWD}@127.0.0.1:{server_with_auth.port}/"
        "?authSource=admin&authMechanism=SCRAM-SHA-256"
    )
    result = _run_mongosh(
        [_MONGOSH, "--quiet", admin_uri, "--eval", _UPDATEUSER_MONGOSH_PROVISION_SCRIPT],
        timeout=60.0,
    )
    assert result.returncode == 0 and "PROVISIONED" in result.stdout, (
        f"provision: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )

    # Old password — must fail.
    old_uri = (
        f"mongodb://alice_xd:orig@127.0.0.1:{server_with_auth.port}/"
        "?authSource=admin&authMechanism=SCRAM-SHA-256"
    )
    result = _run_mongosh(
        [_MONGOSH, "--quiet", old_uri, "--eval", _UPDATEUSER_MONGOSH_PING_SCRIPT],
        timeout=30.0,
    )
    assert result.returncode != 0 or '"ok":1' not in result.stdout, (
        f"old password should not authenticate: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )

    # New password — must succeed.
    new_uri = (
        f"mongodb://alice_xd:rotated@127.0.0.1:{server_with_auth.port}/"
        "?authSource=admin&authMechanism=SCRAM-SHA-256"
    )
    result = _run_mongosh(
        [_MONGOSH, "--quiet", new_uri, "--eval", _UPDATEUSER_MONGOSH_PING_SCRIPT],
        timeout=30.0,
    )
    assert result.returncode == 0, (
        f"new password: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["ok"] == 1


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_updateuser_smoke_via_node_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "updateuser_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node updateuser smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_updateuser_smoke_via_go_driver(server_with_auth: SecantusDBServer) -> None:
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_GO, "run", "./updateuser"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go updateuser smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_updateuser_smoke_via_java_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run_java_smoke("com.secantus.smokes.UpdateUserSmoke", env)
    assert result.returncode == 0, (
        f"java updateuser smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# connectionStatus / authenticatedUserRoles
# ---------------------------------------------------------------------------


_CONNSTATUS_MONGOSH_SCRIPT = """
const r = db.getSiblingDB("admin").runCommand({ connectionStatus: 1 });
print(JSON.stringify({
  users: r.authInfo.authenticatedUsers,
  roles: r.authInfo.authenticatedUserRoles,
}));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
@pytest.mark.xdist_group(name="mongosh_smokes")
def test_connstatus_smoke_via_mongosh(server_with_auth: SecantusDBServer) -> None:
    """connectionStatus surfaces authenticatedUserRoles via mongosh."""
    admin_uri = (
        f"mongodb://{_ADMIN_USER}:{_ADMIN_PWD}@127.0.0.1:{server_with_auth.port}/"
        "?authSource=admin&authMechanism=SCRAM-SHA-256"
    )
    result = _run_mongosh(
        [_MONGOSH, "--quiet", admin_uri, "--eval", _CONNSTATUS_MONGOSH_SCRIPT],
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"mongosh: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["users"] and payload["users"][0]["user"] == _ADMIN_USER
    assert payload["roles"] and payload["roles"][0]["role"] == "root"


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_connstatus_smoke_via_node_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "connstatus_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node connstatus smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_connstatus_smoke_via_go_driver(server_with_auth: SecantusDBServer) -> None:
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_GO, "run", "./connstatus"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go connstatus smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_connstatus_smoke_via_java_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run_java_smoke("com.secantus.smokes.ConnStatusSmoke", env)
    assert result.returncode == 0, (
        f"java connstatus smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# BSON type fidelity
# ---------------------------------------------------------------------------


# mongosh prints an extended-JSON representation per field; we parse it
# Python-side and assert each field's type tag (`$oid`, `$numberInt`,
# `$numberLong`, `$numberDouble`, `$numberDecimal`, `$date`, `$binary`).
# Anything missing the expected tag means SecantusDB collapsed the type
# at the wire — the exact bug class this smoke is built to catch.
_TYPES_MONGOSH_SCRIPT = """
const objID = new ObjectId();
const dec = NumberDecimal("3.141592653589793238");
const when = ISODate("2026-05-06T12:34:56.789Z");
const bin = BinData(0, "aGVsbG8=");
db.c.drop();
db.c.insertOne({
  _id: objID,
  i32: NumberInt(2147483647),
  i64: NumberLong("9223372036854775807"),
  f64: 2.5,
  dec: dec,
  dt: when,
  bin: bin,
  b: true,
  n: null,
});
const got = db.c.findOne({ _id: objID });
print(EJSON.stringify(got, { relaxed: false }));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
@pytest.mark.xdist_group(name="mongosh_smokes")
def test_types_smoke_via_mongosh(server: SecantusDBServer) -> None:
    result = _run_mongosh(
        [_MONGOSH, "--quiet", f"{server.uri}types_xd", "--eval", _TYPES_MONGOSH_SCRIPT],
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"mongosh: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)

    # Strict extJSON tags: collapsing at the server would drop these.
    assert "$oid" in payload["_id"], f"_id: {payload['_id']}"
    assert payload["i32"] == {"$numberInt": "2147483647"}
    assert payload["i64"] == {"$numberLong": "9223372036854775807"}
    assert payload["f64"] == {"$numberDouble": "2.5"}
    assert payload["dec"] == {"$numberDecimal": "3.141592653589793238"}
    assert payload["dt"] == {"$date": {"$numberLong": "1778070896789"}}
    assert payload["bin"]["$binary"]["subType"] == "00"
    assert payload["bin"]["$binary"]["base64"] == "aGVsbG8="
    assert payload["b"] is True
    assert payload["n"] is None


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_types_smoke_via_node_driver(server: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "types_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node types smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_types_smoke_via_go_driver(server: SecantusDBServer) -> None:
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_GO, "run", "./types"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go types smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_types_smoke_via_java_driver(server: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run_java_smoke("com.secantus.smokes.TypesSmoke", env)
    assert result.returncode == 0, (
        f"java types smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# Bulk write
# ---------------------------------------------------------------------------


# One mixed bulk: insert + updateOne + updateMany + replaceOne +
# upsert + deleteOne. Each driver folds the heterogeneous slice into
# one OP_MSG with a kind-1 documentSequence; this smoke proves the
# server's command dispatcher reconstructs the shape and runs the
# right per-op handler in each case. The mongosh script prints the
# result counters + the final doc list for Python-side assertion.
_BULK_MONGOSH_SCRIPT = """
db.c.drop();
db.c.insertMany([{ _id: 1, kind: "old" }, { _id: 2, kind: "old" }]);
const res = db.c.bulkWrite([
  { insertOne: { document: { _id: 3, kind: "fresh" } } },
  { updateOne: { filter: { _id: 1 }, update: { $set: { kind: "new" } } } },
  { updateMany: { filter: { kind: "old" }, update: { $set: { kind: "new" } } } },
  { replaceOne: { filter: { _id: 3 }, replacement: { _id: 3, kind: "replaced" } } },
  { updateOne: {
      filter: { _id: 99 },
      update: { $set: { kind: "upserted" } },
      upsert: true,
  } },
  { deleteOne: { filter: { _id: 2 } } },
]);
const docs = db.c.find({}).sort({ _id: 1 }).toArray();
print(JSON.stringify({
  insertedCount: res.insertedCount,
  matchedCount: res.matchedCount,
  modifiedCount: res.modifiedCount,
  upsertedCount: res.upsertedCount,
  deletedCount: res.deletedCount,
  docs: docs.map((d) => ({ _id: d._id, kind: d.kind })),
}));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
@pytest.mark.xdist_group(name="mongosh_smokes")
def test_bulk_smoke_via_mongosh(server: SecantusDBServer) -> None:
    result = _run_mongosh(
        [_MONGOSH, "--quiet", f"{server.uri}bulk_xd", "--eval", _BULK_MONGOSH_SCRIPT],
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"mongosh: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["insertedCount"] == 1
    assert payload["matchedCount"] == 3
    assert payload["modifiedCount"] == 3
    assert payload["upsertedCount"] == 1
    assert payload["deletedCount"] == 1
    assert payload["docs"] == [
        {"_id": 1, "kind": "new"},
        {"_id": 3, "kind": "replaced"},
        {"_id": 99, "kind": "upserted"},
    ]


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_bulk_smoke_via_node_driver(server: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "bulk_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node bulk smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_bulk_smoke_via_go_driver(server: SecantusDBServer) -> None:
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_GO, "run", "./bulk"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go bulk smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_bulk_smoke_via_java_driver(server: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run_java_smoke("com.secantus.smokes.BulkSmoke", env)
    assert result.returncode == 0, (
        f"java bulk smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# Change-stream resume
# ---------------------------------------------------------------------------


# Workload: open a watch, drive three inserts, capture the resume
# token at event 1, close, reopen with `resumeAfter` → must replay
# events 2 + 3 in order. Then reopen with `startAtOperationTime`
# anchored to a pre-insert timestamp → must replay all three.
#
# Resume tokens are opaque to drivers; the server-side layout is
# `{s, t, n, k}` BSON-encoded as a hex string (`secantus.changestreams`).
# Any wire-shape divergence in token round-trip surfaces here as a
# wrong starting position or a "resume token not found" error.
_CS_RESUME_MONGOSH_SCRIPT = """
db.c.drop();
const startTs = db.runCommand({ hello: 1 }).lastWrite.opTime.ts;

const cs1 = db.c.watch([], { maxAwaitTimeMS: 1000 });
sleep(200);
db.c.insertMany([{ _id: 1 }, { _id: 2 }, { _id: 3 }]);

function nextEvent(cs, deadline) {
  while (Date.now() < deadline) {
    if (cs.hasNext()) return cs.next();
    sleep(150);
  }
  return null;
}

const e1 = nextEvent(cs1, Date.now() + 8000);
const resumeAfter = e1._id;
cs1.close();

const cs2 = db.c.watch([], { resumeAfter, maxAwaitTimeMS: 1000 });
const e2 = nextEvent(cs2, Date.now() + 8000);
const e3 = nextEvent(cs2, Date.now() + 8000);
cs2.close();

const cs3 = db.c.watch([], { startAtOperationTime: startTs, maxAwaitTimeMS: 1000 });
const got = [];
const deadline = Date.now() + 8000;
while (got.length < 3 && Date.now() < deadline) {
  if (cs3.hasNext()) got.push(cs3.next().documentKey._id);
  else sleep(150);
}
cs3.close();

print(JSON.stringify({
  e1: e1.documentKey._id,
  e2: e2.documentKey._id,
  e3: e3.documentKey._id,
  startAt: got,
}));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
@pytest.mark.xdist_group(name="mongosh_smokes")
def test_cs_resume_smoke_via_mongosh(server: SecantusDBServer) -> None:
    result = _run(
        [
            _MONGOSH,
            "--quiet",
            f"{server.uri}cs_resume_xd",
            "--eval",
            _CS_RESUME_MONGOSH_SCRIPT,
        ],
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"mongosh: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["e1"] == 1
    assert payload["e2"] == 2
    assert payload["e3"] == 3
    assert payload["startAt"] == [1, 2, 3]


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_cs_resume_smoke_via_node_driver(server: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "cs_resume_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node cs-resume smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_cs_resume_smoke_via_go_driver(server: SecantusDBServer) -> None:
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_GO, "run", "./cs_resume"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go cs-resume smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_cs_resume_smoke_via_java_driver(server: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run_java_smoke("com.secantus.smokes.CsResumeSmoke", env)
    assert result.returncode == 0, (
        f"java cs-resume smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# listDatabases filter
# ---------------------------------------------------------------------------


_LISTDB_MONGOSH_SCRIPT = """
['alpha', 'beta', 'gamma'].forEach((d) => {
  db.getSiblingDB(d).c.insertOne({_id: 1});
});
const filtered = db.adminCommand({listDatabases: 1, filter: {name: 'alpha'}});
const nameOnly = db.adminCommand({listDatabases: 1, nameOnly: true});
print(JSON.stringify({
  filteredNames: filtered.databases.map((d) => d.name),
  nameOnlyCount: nameOnly.databases.length,
  nameOnlyHasSize: nameOnly.databases.some((d) => 'sizeOnDisk' in d),
}));
['alpha', 'beta', 'gamma'].forEach((d) => db.getSiblingDB(d).dropDatabase());
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
@pytest.mark.xdist_group(name="mongosh_smokes")
def test_listdb_filter_smoke_via_mongosh(server: SecantusDBServer) -> None:
    result = _run_mongosh(
        [_MONGOSH, "--quiet", f"{server.uri}admin", "--eval", _LISTDB_MONGOSH_SCRIPT],
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"mongosh: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["filteredNames"] == ["alpha"]
    assert payload["nameOnlyCount"] >= 3
    assert payload["nameOnlyHasSize"] is False


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_listdb_filter_smoke_via_node_driver(server: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "listdb_filter_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node listdb-filter smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_listdb_filter_smoke_via_go_driver(server: SecantusDBServer) -> None:
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_GO, "run", "./listdb_filter"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go listdb-filter smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_listdb_filter_smoke_via_java_driver(server: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run_java_smoke("com.secantus.smokes.ListDbFilterSmoke", env)
    assert result.returncode == 0, (
        f"java listdb-filter smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# batchSize: 0
# ---------------------------------------------------------------------------


_BATCHSIZE_MONGOSH_SCRIPT = """
db.c.drop();
db.c.insertMany([0,1,2,3,4].map((i) => ({_id: i})));
const cur = db.runCommand({find: 'c', filter: {}, batchSize: 0});
const cursorId = cur.cursor.id;
const firstBatch = cur.cursor.firstBatch;
// Issue an explicit getMore so we can assert the docs flow through it.
const more = db.runCommand({getMore: cursorId, collection: 'c', batchSize: 5});
print(JSON.stringify({
  firstBatchLen: firstBatch.length,
  cursorIdNonZero: cursorId.toString() !== '0',
  fromGetMore: more.cursor.nextBatch.map((d) => d._id),
}));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
@pytest.mark.xdist_group(name="mongosh_smokes")
def test_batchsize_zero_smoke_via_mongosh(server: SecantusDBServer) -> None:
    result = _run_mongosh(
        [_MONGOSH, "--quiet", f"{server.uri}batch_zero_xd", "--eval", _BATCHSIZE_MONGOSH_SCRIPT],
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"mongosh: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["firstBatchLen"] == 0
    assert payload["cursorIdNonZero"] is True
    assert payload["fromGetMore"] == [0, 1, 2, 3, 4]


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_batchsize_zero_smoke_via_node_driver(server: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "batchsize_zero_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node batchsize-zero smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_batchsize_zero_smoke_via_go_driver(server: SecantusDBServer) -> None:
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_GO, "run", "./batchsize_zero"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go batchsize-zero smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_batchsize_zero_smoke_via_java_driver(server: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run_java_smoke("com.secantus.smokes.BatchSizeZeroSmoke", env)
    assert result.returncode == 0, (
        f"java batchsize-zero smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# dropAllUsersFromDatabase
# ---------------------------------------------------------------------------


_DROP_ALL_USERS_PROVISION_SCRIPT = """
['alice', 'bob'].forEach((u) => {
  db.getSiblingDB('shop').runCommand({
    createUser: u, pwd: 'p',
    roles: [{role: 'read', db: 'shop'}],
  });
});
db.getSiblingDB('other').runCommand({
  createUser: 'carol', pwd: 'p',
  roles: [{role: 'read', db: 'other'}],
});
print('PROVISIONED');
"""

_DROP_ALL_USERS_OBSERVE_SCRIPT = """
const r = db.getSiblingDB('shop').runCommand({dropAllUsersFromDatabase: 1});
const shopUsers = db.getSiblingDB('shop').runCommand({usersInfo: 1}).users;
const otherUsers = db.getSiblingDB('other').runCommand({usersInfo: 1}).users;
print(JSON.stringify({
  n: r.n,
  shopUsers: shopUsers.map((u) => u.user),
  otherUsers: otherUsers.map((u) => u.user),
}));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
@pytest.mark.xdist_group(name="mongosh_smokes")
def test_drop_all_users_smoke_via_mongosh(server_with_auth: SecantusDBServer) -> None:
    admin_uri = (
        f"mongodb://{_ADMIN_USER}:{_ADMIN_PWD}@127.0.0.1:{server_with_auth.port}/"
        "?authSource=admin&authMechanism=SCRAM-SHA-256"
    )
    r = _run_mongosh(
        [_MONGOSH, "--quiet", admin_uri, "--eval", _DROP_ALL_USERS_PROVISION_SCRIPT],
        timeout=60.0,
    )
    assert r.returncode == 0 and "PROVISIONED" in r.stdout, (
        f"provision: rc={r.returncode}\nstdout: {r.stdout}\nstderr: {r.stderr}"
    )
    r = _run_mongosh(
        [_MONGOSH, "--quiet", admin_uri, "--eval", _DROP_ALL_USERS_OBSERVE_SCRIPT],
        timeout=60.0,
    )
    assert r.returncode == 0, f"observe: rc={r.returncode}\nstdout: {r.stdout}\nstderr: {r.stderr}"
    out_line = next(
        (ln for ln in reversed(r.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {r.stdout!r}"
    payload = json.loads(out_line)
    assert payload["n"] == 2
    assert payload["shopUsers"] == []
    assert payload["otherUsers"] == ["carol"]


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_drop_all_users_smoke_via_node_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {**os.environ, "MONGODB_URI": server_with_auth.uri, "ADMIN_PASSWORD": _ADMIN_PWD}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "drop_all_users_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node drop-all-users smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_drop_all_users_smoke_via_go_driver(server_with_auth: SecantusDBServer) -> None:
    env = {**os.environ, "MONGODB_URI": server_with_auth.uri, "ADMIN_PASSWORD": _ADMIN_PWD}
    result = _run(
        [_GO, "run", "./drop_all_users"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go drop-all-users smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_drop_all_users_smoke_via_java_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {**os.environ, "MONGODB_URI": server_with_auth.uri, "ADMIN_PASSWORD": _ADMIN_PWD}
    result = _run_java_smoke("com.secantus.smokes.DropAllUsersSmoke", env)
    assert result.returncode == 0, (
        f"java drop-all-users smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# SCRAM-SHA-1
# ---------------------------------------------------------------------------


_SCRAM_SHA1_PROVISION_SCRIPT = """
db.getSiblingDB('admin').runCommand({
  createUser: 'legacy_sh',
  pwd: 'pass',
  roles: [],
  mechanisms: ['SCRAM-SHA-1'],
});
print('PROVISIONED');
"""

_SCRAM_SHA1_PING_SCRIPT = """
print(JSON.stringify({
  ok: db.getSiblingDB('admin').runCommand({ping: 1}).ok,
  user: db.getSiblingDB('admin').runCommand({connectionStatus: 1})
    .authInfo.authenticatedUsers[0].user,
}));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
@pytest.mark.xdist_group(name="mongosh_smokes")
def test_scram_sha1_smoke_via_mongosh(server_with_auth: SecantusDBServer) -> None:
    admin_uri = (
        f"mongodb://{_ADMIN_USER}:{_ADMIN_PWD}@127.0.0.1:{server_with_auth.port}/"
        "?authSource=admin&authMechanism=SCRAM-SHA-256"
    )
    r = _run_mongosh(
        [_MONGOSH, "--quiet", admin_uri, "--eval", _SCRAM_SHA1_PROVISION_SCRIPT],
        timeout=60.0,
    )
    assert r.returncode == 0 and "PROVISIONED" in r.stdout, (
        f"provision: rc={r.returncode}\nstdout: {r.stdout}\nstderr: {r.stderr}"
    )
    legacy_uri = (
        f"mongodb://legacy_sh:pass@127.0.0.1:{server_with_auth.port}/"
        "?authSource=admin&authMechanism=SCRAM-SHA-1"
    )
    r = _run_mongosh(
        [_MONGOSH, "--quiet", legacy_uri, "--eval", _SCRAM_SHA1_PING_SCRIPT], timeout=60.0
    )
    assert r.returncode == 0, f"ping: rc={r.returncode}\nstdout: {r.stdout}\nstderr: {r.stderr}"
    out_line = next(
        (ln for ln in reversed(r.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {r.stdout!r}"
    payload = json.loads(out_line)
    assert payload["ok"] == 1
    assert payload["user"] == "legacy_sh"


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_scram_sha1_smoke_via_node_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {**os.environ, "MONGODB_URI": server_with_auth.uri, "ADMIN_PASSWORD": _ADMIN_PWD}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "scram_sha1_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node scram-sha1 smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_scram_sha1_smoke_via_go_driver(server_with_auth: SecantusDBServer) -> None:
    env = {**os.environ, "MONGODB_URI": server_with_auth.uri, "ADMIN_PASSWORD": _ADMIN_PWD}
    result = _run(
        [_GO, "run", "./scram_sha1"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go scram-sha1 smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_scram_sha1_smoke_via_java_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {**os.environ, "MONGODB_URI": server_with_auth.uri, "ADMIN_PASSWORD": _ADMIN_PWD}
    result = _run_java_smoke("com.secantus.smokes.ScramSha1Smoke", env)
    assert result.returncode == 0, (
        f"java scram-sha1 smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# postBatchResumeToken
# ---------------------------------------------------------------------------


_PBRT_MONGOSH_SCRIPT = """
db.c.drop();
const cs = db.c.watch([], {maxAwaitTimeMS: 500});
const initial = cs.getResumeToken();
sleep(200);
cs.tryNext();
sleep(200);
cs.tryNext();
sleep(200);
const after = cs.getResumeToken();
cs.close();
print(JSON.stringify({
  initialPresent: !!initial,
  afterPresent: !!after,
  // Resume tokens are {_data: <hex>}; advance check is on _data string.
  initialData: initial ? initial._data : null,
  afterData: after ? after._data : null,
}));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
@pytest.mark.xdist_group(name="mongosh_smokes")
def test_pbrt_smoke_via_mongosh(server_with_noop: SecantusDBServer) -> None:
    result = _run_mongosh(
        [_MONGOSH, "--quiet", f"{server_with_noop.uri}pbrt_xd", "--eval", _PBRT_MONGOSH_SCRIPT],
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"mongosh: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["afterPresent"] is True, "expected resume token after empty getMores"
    if payload["initialData"]:
        assert payload["initialData"] != payload["afterData"], (
            "resume token did not advance across empty getMores"
        )


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_pbrt_smoke_via_node_driver(server_with_noop: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {**os.environ, "MONGODB_URI": server_with_noop.uri}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "pbrt_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node pbrt smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_pbrt_smoke_via_go_driver(server_with_noop: SecantusDBServer) -> None:
    env = {**os.environ, "MONGODB_URI": server_with_noop.uri}
    result = _run(
        [_GO, "run", "./pbrt"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go pbrt smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_pbrt_smoke_via_java_driver(server_with_noop: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {**os.environ, "MONGODB_URI": server_with_noop.uri}
    result = _run_java_smoke("com.secantus.smokes.PbrtSmoke", env)
    assert result.returncode == 0, (
        f"java pbrt smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# Tailable cursor on capped collection
# ---------------------------------------------------------------------------


_TAILABLE_MONGOSH_SCRIPT = """
db.dropDatabase();
db.createCollection('logs', {capped: true, size: 64 * 1024});
db.logs.insertOne({_id: 1});
const cur = db.logs.find({}).addOption(2);  // 2 = tailable cursor flag
const first = cur.next();
db.logs.insertOne({_id: 2});
let got = null;
const deadline = Date.now() + 5000;
while (Date.now() < deadline && got === null) {
  if (cur.hasNext()) got = cur.next();
  else sleep(100);
}
cur.close();
print(JSON.stringify({first: first._id, second: got ? got._id : null}));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
@pytest.mark.xdist_group(name="mongosh_smokes")
def test_tailable_smoke_via_mongosh(server: SecantusDBServer) -> None:
    result = _run_mongosh(
        [_MONGOSH, "--quiet", f"{server.uri}tailable_xd", "--eval", _TAILABLE_MONGOSH_SCRIPT],
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"mongosh: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["first"] == 1
    assert payload["second"] == 2


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_tailable_smoke_via_node_driver(server: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "tailable_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node tailable smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_tailable_smoke_via_go_driver(server: SecantusDBServer) -> None:
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_GO, "run", "./tailable"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go tailable smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_tailable_smoke_via_java_driver(server: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run_java_smoke("com.secantus.smokes.TailableSmoke", env)
    assert result.returncode == 0, (
        f"java tailable smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# Custom roles
# ---------------------------------------------------------------------------


_CUSTOM_ROLES_PROVISION_SCRIPT = """
db.getSiblingDB('shop').runCommand({
  createRole: 'shopAuditor',
  privileges: [
    {resource: {db: 'shop', collection: ''}, actions: ['find']},
  ],
  roles: [],
});
db.getSiblingDB('shop').runCommand({
  createUser: 'auditor_msh',
  pwd: 'p',
  roles: [{role: 'shopAuditor', db: 'shop'}],
});
print('PROVISIONED');
"""

_CUSTOM_ROLES_AUDITOR_SCRIPT = """
const items = db.getSiblingDB('shop').items;
const findOk = items.find({}).toArray();
let inserted = false;
let errCode = null;
try {
  items.insertOne({x: 1});
  inserted = true;
} catch (e) {
  errCode = e.code;
}
print(JSON.stringify({findCount: findOk.length, inserted, errCode}));
"""

_CUSTOM_ROLES_GRANT_SCRIPT = """
db.getSiblingDB('shop').runCommand({
  grantPrivilegesToRole: 'shopAuditor',
  privileges: [
    {resource: {db: 'shop', collection: ''}, actions: ['insert']},
  ],
});
print('GRANTED');
"""

_CUSTOM_ROLES_AUDITOR_INSERT_SCRIPT = """
const items = db.getSiblingDB('shop').items;
let inserted = false;
try {
  items.insertOne({x: 2});
  inserted = true;
} catch (e) {}
print(JSON.stringify({inserted}));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
@pytest.mark.xdist_group(name="mongosh_smokes")
def test_custom_roles_smoke_via_mongosh(server_with_auth: SecantusDBServer) -> None:
    admin_uri = (
        f"mongodb://{_ADMIN_USER}:{_ADMIN_PWD}@127.0.0.1:{server_with_auth.port}/"
        "?authSource=admin&authMechanism=SCRAM-SHA-256"
    )
    r = _run_mongosh(
        [_MONGOSH, "--quiet", admin_uri, "--eval", _CUSTOM_ROLES_PROVISION_SCRIPT],
        timeout=60.0,
    )
    assert r.returncode == 0 and "PROVISIONED" in r.stdout, (
        f"provision: rc={r.returncode}\nstdout: {r.stdout}\nstderr: {r.stderr}"
    )

    auditor_uri = (
        f"mongodb://auditor_msh:p@127.0.0.1:{server_with_auth.port}/shop"
        "?authSource=shop&authMechanism=SCRAM-SHA-256"
    )
    r = _run_mongosh(
        [_MONGOSH, "--quiet", auditor_uri, "--eval", _CUSTOM_ROLES_AUDITOR_SCRIPT],
        timeout=60.0,
    )
    assert r.returncode == 0, f"auditor: rc={r.returncode}\nstdout: {r.stdout}\nstderr: {r.stderr}"
    out_line = next(
        (ln for ln in reversed(r.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {r.stdout!r}"
    payload = json.loads(out_line)
    assert payload["inserted"] is False
    assert payload["errCode"] == 13

    # Grant insert; reconnect to pick up the new privilege.
    r = _run_mongosh(
        [_MONGOSH, "--quiet", admin_uri, "--eval", _CUSTOM_ROLES_GRANT_SCRIPT],
        timeout=60.0,
    )
    assert r.returncode == 0 and "GRANTED" in r.stdout, (
        f"grant: rc={r.returncode}\nstdout: {r.stdout}\nstderr: {r.stderr}"
    )
    r = _run_mongosh(
        [_MONGOSH, "--quiet", auditor_uri, "--eval", _CUSTOM_ROLES_AUDITOR_INSERT_SCRIPT],
        timeout=60.0,
    )
    assert r.returncode == 0, (
        f"auditor-insert: rc={r.returncode}\nstdout: {r.stdout}\nstderr: {r.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(r.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    payload = json.loads(out_line)
    assert payload["inserted"] is True


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_custom_roles_smoke_via_node_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "custom_roles_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node custom-roles smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_custom_roles_smoke_via_go_driver(server_with_auth: SecantusDBServer) -> None:
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_GO, "run", "./custom_roles"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go custom-roles smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_custom_roles_smoke_via_java_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_JAVA, "-cp", str(_JAVA_SMOKES_JAR), "com.secantus.smokes.CustomRolesSmoke"],
        env=env,
        timeout=120.0,
    )
    assert result.returncode == 0, (
        f"java custom-roles smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# Logical sessions (startSession / endSessions / refreshSessions)
# ---------------------------------------------------------------------------


_SESSIONS_MONGOSH_SCRIPT = """
const started = db.getSiblingDB("admin").runCommand({ startSession: 1 });
// `BinData(4, ...)` second argument is base64-decoded by mongosh and
// must produce exactly 16 bytes for subtype 4 (UUID). 24 base64 chars
// → 16 raw bytes; passing 16 ASCII chars literally gives 12 bytes
// after base64 decode and produces malformed BSON on the wire.
const refreshRes = db.getSiblingDB("admin").runCommand({
  refreshSessions: [{ id: BinData(4, "AAAAAAAAAAAAAAAAAAAAAA==") }],
});
const endRes = db.getSiblingDB("admin").runCommand({ endSessions: [started.id] });
print(JSON.stringify({
  startedOk: started.ok,
  hasId: !!started.id && !!started.id.id,
  timeoutMinutes: started.timeoutMinutes,
  refreshOk: refreshRes.ok,
  endOk: endRes.ok,
}));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
@pytest.mark.xdist_group(name="mongosh_smokes")
def test_sessions_smoke_via_mongosh(server: SecantusDBServer) -> None:
    result = _run_mongosh(
        [_MONGOSH, "--quiet", server.uri, "--eval", _SESSIONS_MONGOSH_SCRIPT],
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"mongosh sessions: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["startedOk"] == 1
    assert payload["hasId"] is True
    assert payload["timeoutMinutes"] == 30
    assert payload["refreshOk"] == 1
    assert payload["endOk"] == 1


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_sessions_smoke_via_node_driver(server: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "sessions_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node sessions smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_sessions_smoke_via_go_driver(server: SecantusDBServer) -> None:
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_GO, "run", "./sessions"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go sessions smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_sessions_smoke_via_java_driver(server: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_JAVA, "-cp", str(_JAVA_SMOKES_JAR), "com.secantus.smokes.SessionsSmoke"],
        env=env,
        timeout=120.0,
    )
    assert result.returncode == 0, (
        f"java sessions smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# Cluster role bundles (clusterMonitor / backup)
# ---------------------------------------------------------------------------


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
@pytest.mark.xdist_group(name="node_smokes")
def test_cluster_roles_smoke_via_node_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "cluster_roles_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node cluster-roles smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_cluster_roles_smoke_via_go_driver(server_with_auth: SecantusDBServer) -> None:
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_GO, "run", "./cluster_roles"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go cluster-roles smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_JAVA is None, reason="java not on PATH")
@pytest.mark.skipif(_GRADLE is None, reason="gradle not on PATH")
@pytest.mark.xdist_group(name="java_smokes")
def test_cluster_roles_smoke_via_java_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_java_smokes_jar():
        pytest.skip("could not build secantus-java-smokes-all.jar")
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_JAVA, "-cp", str(_JAVA_SMOKES_JAR), "com.secantus.smokes.ClusterRolesSmoke"],
        env=env,
        timeout=120.0,
    )
    assert result.returncode == 0, (
        f"java cluster-roles smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# Ruby (mongo-ruby-driver) — BSON type fidelity
# ---------------------------------------------------------------------------


def _run_ruby_smoke(
    smoke_filename: str,
    server_uri: str,
    *,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess:
    env = {
        **os.environ,
        "MONGODB_URI": server_uri,
        "PATH": f"{Path(_RUBY).parent}:{os.environ.get('PATH', '')}",
    }
    if extra_env:
        env.update(extra_env)
    return _run(
        [_BUNDLE, "exec", "ruby", str(_RUBY_SMOKE_DIR / smoke_filename)],
        cwd=_RUBY_SMOKE_DIR,
        env=env,
        timeout=120.0,
    )


@pytest.mark.skipif(not _RUBY_AVAILABLE, reason="ruby >= 3.0 with bundle not on PATH")
@pytest.mark.xdist_group(name="ruby_smokes")
def test_types_smoke_via_ruby_driver(server: SecantusDBServer) -> None:
    if not _ensure_ruby_bundle():
        pytest.skip("could not bundle install mongo-ruby-driver")
    result = _run_ruby_smoke("types_smoke.rb", server.uri)
    assert result.returncode == 0, (
        f"ruby types smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(not _RUBY_AVAILABLE, reason="ruby >= 3.0 with bundle not on PATH")
@pytest.mark.xdist_group(name="ruby_smokes")
def test_rbac_smoke_via_ruby_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_ruby_bundle():
        pytest.skip("could not bundle install mongo-ruby-driver")
    result = _run_ruby_smoke(
        "rbac_smoke.rb",
        server_with_auth.uri,
        extra_env={"ADMIN_PASSWORD": _ADMIN_PWD},
    )
    assert result.returncode == 0, (
        f"ruby rbac smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(not _RUBY_AVAILABLE, reason="ruby >= 3.0 with bundle not on PATH")
@pytest.mark.xdist_group(name="ruby_smokes")
def test_bulk_smoke_via_ruby_driver(server: SecantusDBServer) -> None:
    if not _ensure_ruby_bundle():
        pytest.skip("could not bundle install mongo-ruby-driver")
    result = _run_ruby_smoke("bulk_smoke.rb", server.uri)
    assert result.returncode == 0, (
        f"ruby bulk smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# PHP (mongo-php-library + ext-mongodb) — feature matrix
# ---------------------------------------------------------------------------


def _run_php_smoke(
    smoke_filename: str,
    server_uri: str,
    *,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess:
    env = {**os.environ, "MONGODB_URI": server_uri}
    if extra_env:
        env.update(extra_env)
    return _run(
        [_PHP, str(_PHP_SMOKE_DIR / smoke_filename)],
        env=env,
        timeout=60.0,
    )


@pytest.mark.skipif(not _PHP_AVAILABLE, reason="php with ext-mongodb + composer not on PATH")
@pytest.mark.xdist_group(name="php_smokes")
def test_types_smoke_via_php_driver(server: SecantusDBServer) -> None:
    if not _ensure_php_vendor():
        pytest.skip("could not composer install mongodb/mongodb")
    result = _run_php_smoke("types_smoke.php", server.uri)
    assert result.returncode == 0, (
        f"php types smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(not _PHP_AVAILABLE, reason="php with ext-mongodb + composer not on PATH")
@pytest.mark.xdist_group(name="php_smokes")
def test_rbac_smoke_via_php_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_php_vendor():
        pytest.skip("could not composer install mongodb/mongodb")
    result = _run_php_smoke(
        "rbac_smoke.php",
        server_with_auth.uri,
        extra_env={"ADMIN_PASSWORD": _ADMIN_PWD},
    )
    assert result.returncode == 0, (
        f"php rbac smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(not _PHP_AVAILABLE, reason="php with ext-mongodb + composer not on PATH")
@pytest.mark.xdist_group(name="php_smokes")
def test_bulk_smoke_via_php_driver(server: SecantusDBServer) -> None:
    if not _ensure_php_vendor():
        pytest.skip("could not composer install mongodb/mongodb")
    result = _run_php_smoke("bulk_smoke.php", server.uri)
    assert result.returncode == 0, (
        f"php bulk smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# Ruby + PHP — listdb_filter / sessions / cluster_roles
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not _RUBY_AVAILABLE, reason="ruby >= 3.0 with bundle not on PATH")
@pytest.mark.xdist_group(name="ruby_smokes")
def test_listdb_filter_smoke_via_ruby_driver(server: SecantusDBServer) -> None:
    if not _ensure_ruby_bundle():
        pytest.skip("could not bundle install mongo-ruby-driver")
    result = _run_ruby_smoke("listdb_filter_smoke.rb", server.uri)
    assert result.returncode == 0, (
        f"ruby listdb-filter smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(not _RUBY_AVAILABLE, reason="ruby >= 3.0 with bundle not on PATH")
@pytest.mark.xdist_group(name="ruby_smokes")
def test_sessions_smoke_via_ruby_driver(server: SecantusDBServer) -> None:
    if not _ensure_ruby_bundle():
        pytest.skip("could not bundle install mongo-ruby-driver")
    result = _run_ruby_smoke("sessions_smoke.rb", server.uri)
    assert result.returncode == 0, (
        f"ruby sessions smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(not _RUBY_AVAILABLE, reason="ruby >= 3.0 with bundle not on PATH")
@pytest.mark.xdist_group(name="ruby_smokes")
def test_cluster_roles_smoke_via_ruby_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_ruby_bundle():
        pytest.skip("could not bundle install mongo-ruby-driver")
    result = _run_ruby_smoke(
        "cluster_roles_smoke.rb",
        server_with_auth.uri,
        extra_env={"ADMIN_PASSWORD": _ADMIN_PWD},
    )
    assert result.returncode == 0, (
        f"ruby cluster-roles smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(not _PHP_AVAILABLE, reason="php with ext-mongodb + composer not on PATH")
@pytest.mark.xdist_group(name="php_smokes")
def test_listdb_filter_smoke_via_php_driver(server: SecantusDBServer) -> None:
    if not _ensure_php_vendor():
        pytest.skip("could not composer install mongodb/mongodb")
    result = _run_php_smoke("listdb_filter_smoke.php", server.uri)
    assert result.returncode == 0, (
        f"php listdb-filter smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(not _PHP_AVAILABLE, reason="php with ext-mongodb + composer not on PATH")
@pytest.mark.xdist_group(name="php_smokes")
def test_sessions_smoke_via_php_driver(server: SecantusDBServer) -> None:
    if not _ensure_php_vendor():
        pytest.skip("could not composer install mongodb/mongodb")
    result = _run_php_smoke("sessions_smoke.php", server.uri)
    assert result.returncode == 0, (
        f"php sessions smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(not _PHP_AVAILABLE, reason="php with ext-mongodb + composer not on PATH")
@pytest.mark.xdist_group(name="php_smokes")
def test_cluster_roles_smoke_via_php_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_php_vendor():
        pytest.skip("could not composer install mongodb/mongodb")
    result = _run_php_smoke(
        "cluster_roles_smoke.php",
        server_with_auth.uri,
        extra_env={"ADMIN_PASSWORD": _ADMIN_PWD},
    )
    assert result.returncode == 0, (
        f"php cluster-roles smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# Rust (mongo-rust-driver) — BSON type fidelity
# ---------------------------------------------------------------------------
#
# The Rust smoke (``tests/cross_driver/rust/src/bin/types_smoke.rs``)
# is the same shape as the Ruby / Java / Node / Go / PHP types tests:
# insert one document touching every BSON variant, find it back, assert
# round-trip parity. Skips cleanly if ``cargo`` isn't on PATH; xfail-
# marked because mongo-rust-driver upstream has had recurrent
# Rust-version / tokio / Decimal128 compatibility wobbles that are out
# of scope to track in this repo. The xfail tag is ``strict=False`` so
# unexpected passes don't fail the suite — they just surface in test
# logs as "this stopped being broken, time to retire the xfail".


_RUST_BIN_TYPES = _RUST_SMOKE_DIR / "target" / "release" / "types_smoke"


def _ensure_rust_bin(bin_name: str) -> bool:
    """Build a Rust smoke binary once via ``cargo build --release``.

    Same xdist coordination pattern as ``_ensure_ruby_bundle`` /
    ``_ensure_node_modules``: workers race on a cold ``target/`` dir,
    so flock a sentinel file and let only one worker run cargo. The
    others wait then observe the populated ``target/release/<bin>``.

    Returns False if cargo is missing OR the build fails (the upstream
    driver / toolchain churn the Cargo.toml header documents).
    """
    if not _RUST_AVAILABLE:
        return False
    target = _RUST_SMOKE_DIR / "target" / "release" / bin_name
    if target.is_file():
        return True
    import fcntl

    lock_path = _RUST_SMOKE_DIR / ".cargo.lock-build"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with open(lock_path, "w") as lock_fp:
        fcntl.flock(lock_fp.fileno(), fcntl.LOCK_EX)
        if target.is_file():
            return True
        # cargo build can take 60–120s on a cold target/. Give it
        # enough room without making the harness hostage to a wedge.
        result = _run(
            [_CARGO, "build", "--release", "--bin", bin_name],
            cwd=_RUST_SMOKE_DIR,
            timeout=300.0,
        )
        return result.returncode == 0 and target.is_file()


def _run_rust_smoke(
    bin_path: Path,
    server_uri: str,
    *,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess:
    env = {
        **os.environ,
        "MONGODB_URI": server_uri,
    }
    if extra_env:
        env.update(extra_env)
    return _run([str(bin_path)], env=env, timeout=60.0)


@pytest.mark.skipif(not _RUST_AVAILABLE, reason="cargo not on PATH")
@pytest.mark.xdist_group(name="rust_smokes")
@pytest.mark.xfail(
    reason="mongo-rust-driver upstream has recurrent toolchain compat "
    "issues (tokio / Decimal128 / Rust version drift). Tracked in "
    "tests/cross_driver/rust/Cargo.toml header.",
    strict=False,
)
def test_types_smoke_via_rust_driver(server: SecantusDBServer) -> None:
    if not _ensure_rust_bin("types_smoke"):
        pytest.skip("cargo build --release of types_smoke failed; see Cargo.toml header")
    result = _run_rust_smoke(_RUST_BIN_TYPES, server.uri)
    assert result.returncode == 0, (
        f"rust types smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout
