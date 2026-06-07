#!/usr/bin/env bash
# Phase 0 spike runner — builds and runs all three de-risking spikes for the
# Python -> Rust rewrite (see tasks/rust-rewrite-plan.md §6 Phase 0 and
# tasks/rust-rewrite-spike-findings.md for results).
#
#   Spike 1  bson-crate <-> pymongo byte-fidelity
#   Spike 2  WiredTiger FFI smoke (open/session/create/insert/search/scan)
#   Spike 3  byte-exact reproduction of secantus.sortkey
#
# Requirements: rust toolchain, uv, cmake + ninja + clang (libclang for
# bindgen). Spike 2 builds the vendored WiredTiger static lib on first run
# (~minutes); set WT_BUILD_DIR to a prebuilt WT cmake build dir to reuse it.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
cd "$here"

# bindgen needs libclang; auto-locate if LIBCLANG_PATH isn't already set.
if [[ -z "${LIBCLANG_PATH:-}" ]]; then
    cand="$(dirname "$(find /usr/lib /usr/lib64 -name 'libclang*.so*' 2>/dev/null | head -1)" 2>/dev/null || true)"
    [[ -n "$cand" ]] && export LIBCLANG_PATH="$cand"
fi

echo "==> building spikes"
cargo build --release

echo; echo "==> Spike 1: BSON fidelity (pymongo <-> bson crate)"
uv run --no-project --with pymongo python "$here/harness/spike_bson_harness.py" \
    "$here/target/release/roundtrip"

echo; echo "==> Spike 3: sortkey golden vectors"
golden="$(mktemp /tmp/sortkey-golden.XXXXXX.bson)"
uv run --no-project --with pymongo python "$here/harness/spike_sortkey_golden.py" "$golden"
"$here/target/release/goldencheck" < "$golden"
rm -f "$golden"

echo; echo "==> Spike 2: WiredTiger FFI smoke"
"$here/target/release/wt_smoke"

echo; echo "==> all spikes passed"
