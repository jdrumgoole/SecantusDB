### A standalone Windows binary for the Rust server

The `secantusd-rs` standalone binary now ships for Windows alongside Linux and
macOS. Every `secantusdb-v*` release attaches an `x86_64-pc-windows-msvc` archive
— a `.zip` for Explorer, and a `.tar.gz` for anyone who'd rather use the same
command on all three platforms — each with a `.sha256` beside it. The Windows
build links the C runtime statically, so the `.exe` runs on a clean machine with
no Visual C++ redistributable installed.

Windows had been listed as blocked on the MSVC WiredTiger build "producing no
static library". That turned out to be a statement about a filename rather than a
capability: MSVC emits `wiredtiger.lib` where Unix emits `libwiredtiger.a`, and
`build.rs` grew the second name some time ago. CI had quietly been linking that
static library and building `secantusd-rs.exe` on every push ever since — the
note simply outlived its cause. Enabling the release lane was mostly packaging.

Two Windows-specific problems did surface, and both were worth finding. The
linker had been warning `LNK4098: defaultlib 'LIBCMT' conflicts`, which is not
cosmetic: WiredTiger's static library uses the static C runtime while Rust's MSVC
target defaults to the dynamic one, and two C runtimes in one process means two
heaps — memory allocated inside WiredTiger and freed on the Rust side is
undefined behaviour. Building with `+crt-static` matches them. Separately, the
binary's smoke test asserted a clean exit after SIGTERM, which can't work on
Windows at all: `send_signal` maps SIGTERM to `TerminateProcess`, an immediate
kill that exits 1 and runs no handler. It now sends `CTRL_BREAK_EVENT` to a
process-group child, which the binary's console handler turns into the same
graceful shutdown Unix gets — and that test now runs on Windows in CI on every
push, so the release binary is exercised continuously rather than only at tag
time.

#### Added

- `x86_64-pc-windows-msvc` in the `release-binaries` matrix, publishing
  `secantusdb-<version>-x86_64-pc-windows-msvc.zip` + `.tar.gz` (each with a
  `.sha256`). Built with `+crt-static`; no PGO on this target yet, so it is a few
  percent slower on write-heavy paths than the other two archives and otherwise
  identical.
- The wheel-bundled `secantusd-rs` smoke test now runs on Windows in `test.yml`'s
  `storage-engine` job (it was skipped on the stale grounds that the binary
  wasn't built there).

#### Fixed

- **CRT mismatch in the Windows binary** (`LNK4098`): WiredTiger's static CRT vs
  Rust's default dynamic CRT put two C runtimes, and two heaps, in one process.
  Now built with `-Ctarget-feature=+crt-static`.
- `tests/test_rust_binary_smoke.py` asserted a graceful SIGTERM exit that Windows
  cannot deliver; it now uses `CTRL_BREAK_EVENT` with `CREATE_NEW_PROCESS_GROUP`
  there, leaving the Unix path unchanged.

#### Changed

- `docs-rust/installation.md`, `docs-rust/releases.md`, the marketing site's
  Rust-server page, and the two "Windows is blocked" comments in
  `release-binaries.yml` / `test.yml` all corrected — they described a limitation
  that no longer existed.
