#!/usr/bin/env bash
# Build the `secantusd-pg` release binary: link-time optimisation across the
# whole program (the crate's `[profile.release]`) and a profile collected from
# the binary itself, on the machine that builds it.
#
#     SECANTUS_WT_INCLUDE=... SECANTUS_WT_LIB=... scripts/build_pg_release.sh <target>
#
# Three stages: build with instrumentation, run `bench/pgo_workload_pg.py`
# against that binary, build again with the profile it wrote. The path of the
# finished binary is the last line printed. `SECANTUS_PG_NO_PGO=1` skips the
# first two stages; the profile is a hint to the optimiser, so the binary
# without it answers the same and is slower.
#
# Called by `.github/workflows/release-pg-binaries.yml`. It is a script and
# not workflow steps so it can be run, and timed, anywhere a release is built.
#
# Measured 2026-10-13 on a 16-vCPU droplet, one pinned client, selects by
# primary key a second: 12,052 as released before this, 14,402 with the
# link-time optimisation, 17,855 with the profile as well
# (`bench/results/pg-droplet/2026-10-13-single-connection-cost/`).
set -euo pipefail

target=${1:?usage: build_pg_release.sh <target triple>}
repo=$(cd "$(dirname "$0")/.." && pwd)
manifest="$repo/crates/secantus-pg/Cargo.toml"
bin="$repo/crates/secantus-pg/target/$target/release/secantusd-pg"
build() { cargo build --release --locked --target "$target" --manifest-path "$manifest" "$@"; }

if [ "${SECANTUS_PG_NO_PGO:-}" = "1" ]; then
  echo "=== release build, no profile ===" >&2
  build >&2
  echo "$bin"
  exit 0
fi

work=$(mktemp -d)
raw="$work/raw"
instrument=()
mkdir -p "$raw"
server=""
cleanup() {
  [ -n "$server" ] && kill -TERM "$server" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

profdata=$(find "$(rustc --print sysroot)" -name llvm-profdata -type f | head -1)
if [ -z "$profdata" ]; then
  echo "error: no llvm-profdata in the Rust toolchain (rustup component add llvm-tools)" >&2
  exit 1
fi

case "$target" in
  *-apple-darwin)
    # The `cc` crate copies rustc's -Cprofile-generate and -Cprofile-use onto
    # the C compiler, so every C dependency would be instrumented by Apple
    # clang, in a profile format the Rust toolchain's runtime does not read.
    # With Rust 1.99 the instrumented binary then died of a segmentation
    # fault before it listened (measured here 2026-10-13; the MongoDB binary's
    # build met the same thing). This wrapper drops those flags, so only Rust
    # is profiled.
    cat > "$work/cc-noprofile" <<'WRAP'
#!/bin/bash
args=()
for a in "$@"; do
  case "$a" in -fprofile-generate*|-fprofile-use*|-fprofile-instr-*) ;; *) args+=("$a") ;; esac
done
exec cc "${args[@]}"
WRAP
    chmod +x "$work/cc-noprofile"
    export "CC_${target//-/_}=$work/cc-noprofile"
    # On the macOS release runner the profile is not written as the process
    # exits, so there the binary writes it itself (the `pgo-instrument`
    # feature, to SECANTUS_PGO_OUT). Both are set: where the exit-time write
    # works too, the two files are merged.
    instrument=(--features pgo-instrument)
    export SECANTUS_PGO_OUT="$raw/pg-self.profraw"
    ;;
esac

echo "=== stage 1: instrumented build ===" >&2
RUSTFLAGS="-Cprofile-generate=$raw" build ${instrument[@]+"${instrument[@]}"} >&2

echo "=== stage 2: run the workload against the instrumented binary ===" >&2
LLVM_PROFILE_FILE="$raw/pg-%p-%m.profraw" \
  "$bin" "$work/store" 127.0.0.1:0 > "$work/server.log" 2>&1 &
server=$!
addr=""
for _ in $(seq 1 120); do
  addr=$(sed -n 's/.*listening on \([0-9.]*:[0-9]*\).*/\1/p' "$work/server.log" | head -1)
  [ -n "$addr" ] && break
  kill -0 "$server" 2>/dev/null || break
  sleep 0.5
done
if [ -z "$addr" ]; then
  echo "error: the instrumented binary never printed a listening address" >&2
  cat "$work/server.log" >&2
  exit 1
fi
(cd "$repo" && uv run --no-project --with "psycopg[binary]" python -m bench.pgo_workload_pg \
  --dsn "host=${addr%:*} port=${addr##*:} dbname=postgres user=postgres sslmode=disable") >&2
kill -TERM "$server"
# The profile is written as the process exits, so its exit is waited for and
# its status checked: a crash here would leave a partial profile behind.
status=0
wait "$server" || status=$?
server=""
if [ "$status" != 0 ]; then
  echo "error: the instrumented binary exited with status $status" >&2
  cat "$work/server.log" >&2
  exit 1
fi

count=$(find "$raw" -name '*.profraw' | wc -l | tr -d ' ')
if [ "$count" = 0 ]; then
  echo "error: the instrumented binary wrote no profile" >&2
  cat "$work/server.log" >&2
  exit 1
fi
"$profdata" merge -o "$work/merged.profdata" "$raw"
echo "profile: $count raw file(s), $(du -h "$work/merged.profdata" | cut -f1) merged" >&2

echo "=== stage 3: build with the profile ===" >&2
RUSTFLAGS="-Cprofile-use=$work/merged.profdata" build >&2
echo "$bin"
