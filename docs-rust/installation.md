# Installation

Three ways to get the Rust server. It is not in the `SecantusDB` Python wheel,
which carries only the Python servers.

## From crates.io (recommended)

The server is the `secantus-mdb` crate. `cargo install` builds it, WiredTiger
included, and puts `secantusd-rs` on `PATH`. It needs a Rust toolchain, CMake
and a C compiler:

```bash
cargo install secantus-mdb --version 0.5.3-beta.173
secantusd-rs --version
secantusd-rs --port 27017 --storage-path ./secantus-data
```

The crate is a pre-release, so `cargo install` needs the `--version`; without
it cargo stops at "nothing to install". The current version is on
[crates.io](https://crates.io/crates/secantus-mdb).

To run the server inside a Rust test instead, add the crate as a
dev-dependency and start it in-process (see [Embedded](embedded.md)):

```toml
[dev-dependencies]
secantus-mdb = "0.5.3-beta.173"
```

## Prebuilt binary (no Rust toolchain)

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
