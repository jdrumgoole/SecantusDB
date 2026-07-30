# Installation

Three ways to get the Rust server, from lightest to heaviest.

## Prebuilt binary (recommended)

Prebuilt archives with WiredTiger statically linked are published on GitHub
Releases under `secantusdb-v<version>` tags — no Python, no shared
libraries, no system MongoDB:

- `secantusdb-<version>-x86_64-unknown-linux-gnu.tar.gz`
- `secantusdb-<version>-aarch64-apple-darwin.tar.gz`
- `secantusdb-<version>-x86_64-pc-windows-msvc.zip` — and a `.tar.gz` of the
  same contents, if you'd rather use the same command on every platform

Each archive ships with a `.sha256` checksum file. Download, verify,
extract, run:

```bash
tag="secantusdb-v0.5.3-beta.147"
base="https://github.com/jdrumgoole/SecantusDB/releases/download/$tag"
curl -LO "$base/secantusdb-0.5.3-beta.147-x86_64-unknown-linux-gnu.tar.gz"
curl -LO "$base/secantusdb-0.5.3-beta.147-x86_64-unknown-linux-gnu.tar.gz.sha256"
shasum -a 256 -c secantusdb-0.5.3-beta.147-x86_64-unknown-linux-gnu.tar.gz.sha256
tar xzf secantusdb-0.5.3-beta.147-x86_64-unknown-linux-gnu.tar.gz
./secantusd-rs --version
```

Every archive is smoke-tested in CI before release: the workflow boots the
binary and runs a full `pymongo` CRUD round-trip against it.

On **Windows**, unpack the `.zip` in Explorer (or `tar xzf` the `.tar.gz` from
any modern shell) and run `secantusd-rs.exe`:

```powershell
$tag = "secantusdb-v0.5.3-beta.147"
$base = "https://github.com/jdrumgoole/SecantusDB/releases/download/$tag"
$zip = "secantusdb-0.5.3-beta.147-x86_64-pc-windows-msvc.zip"
Invoke-WebRequest "$base/$zip" -OutFile $zip
# The .sha256 is in the `<hash>  <filename>` format shasum/sha256sum print:
(Get-FileHash $zip -Algorithm SHA256).Hash -eq `
  ((Get-Content "$zip.sha256") -split '\s+')[0].ToUpper()
Expand-Archive $zip -DestinationPath .
.\secantusd-rs.exe --version
```

The Windows build links the C runtime statically, so it needs no Visual C++
redistributable — the `.exe` runs on a clean machine. It is currently built
without PGO, so it is a few percent slower on write-heavy paths than the Linux
and macOS archives; it is otherwise identical.

## Bundled in the Python wheel

A storage-engine build of the `SecantusDB` wheel installs `secantusd-rs` on
`PATH` next to the pure-Python `secantusd-py`, plus the embedded
[`RustServer` handle](embedded.md):

```bash
SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON uv sync --extra dev
secantusd-rs --port 27017 --storage-path ./secantus-data
```

## Build from source

The binary lives in `crates/secantusdb` (its own Cargo workspace, since it
links WiredTiger). Building needs a Rust toolchain, CMake/Ninja, and
libclang (for the WiredTiger FFI bindgen):

```bash
git clone --recurse-submodules https://github.com/jdrumgoole/SecantusDB
cd SecantusDB
cargo build --release --manifest-path crates/secantusdb/Cargo.toml
# → crates/secantusdb/target/release/secantusd-rs
```

The build compiles the vendored WiredTiger (`vendor/wiredtiger`,
mongodb-7.0 line) and statically links it — the resulting binary is
self-contained.
