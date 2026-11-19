# Installation

Three ways to get the Rust MongoDB server (`secantusd-mdb`). The
[main Installation page](https://secantusdb.com/docs/installation.html)
covers all three channels for both servers, including `secantusd-pg`.

## From PyPI

`pip install SecantusDB` installs prebuilt `secantusd-mdb` and `secantusd-pg`
binaries on `PATH` (Linux x86_64, macOS arm64) and the `secantusdb` Python
launcher (`from secantusdb import MongoServer, PostgresServer`).

## From crates.io

The server is the `secantus-mdb` crate. `cargo install` builds it, WiredTiger
included, and puts `secantusd-mdb` on `PATH`. It needs a Rust toolchain, CMake
and a C compiler:

```bash
cargo install secantus-mdb --version 0.5.3-beta.199
secantusd-mdb --version
secantusd-mdb --port 27017 --storage-path ./secantus-data
```

The crate is a pre-release, so `cargo install` needs the `--version`; without
it cargo stops at "nothing to install". Take the current version from
[crates.io](https://crates.io/crates/secantus-mdb).

To run the server inside a Rust program instead, add the crate as a
dependency and start it in-process (see [Embedded](embedded.md)):

```toml
[dependencies]
secantus-mdb = "0.5.3-beta.199"
```

## Prebuilt binary (no Rust toolchain)

Prebuilt archives with WiredTiger statically linked are published on GitHub
Releases under `secantusdb-v<version>` tags — no Python, no shared
libraries, no system MongoDB:

- `secantusdb-<version>-x86_64-unknown-linux-gnu.tar.gz`
- `secantusdb-<version>-aarch64-apple-darwin.tar.gz`
- `secantusdb-<version>-x86_64-pc-windows-msvc.tar.gz` — plus a `.zip` of the
  same contents for Explorer, from the first release after 0.5.3-beta.197
  (earlier releases built the `.zip` but never attached it)

Each archive ships with a `.sha256` checksum file. Download, verify,
extract, run:

```bash
tag="secantusdb-v0.5.3-beta.197"
stem="secantusdb-0.5.3-beta.197-x86_64-unknown-linux-gnu"   # macOS: aarch64-apple-darwin
base="https://github.com/jdrumgoole/SecantusDB/releases/download/$tag"
curl -LO "$base/$stem.tar.gz"
curl -LO "$base/$stem.tar.gz.sha256"
shasum -a 256 -c "$stem.tar.gz.sha256"
tar xzf "$stem.tar.gz"                 # unpacks into ./$stem/
"./$stem/secantusd-mdb" --version
```

Every archive is smoke-tested in CI before release: the workflow boots the
binary and runs a full `pymongo` CRUD round-trip against it.

On **Windows**, `tar` ships with Windows 10 and later, so the `.tar.gz`
unpacks from PowerShell (on releases that carry the `.zip`, Explorer opens
that too) and you run `secantusd-mdb.exe`:

```powershell
$tag = "secantusdb-v0.5.3-beta.197"
$stem = "secantusdb-0.5.3-beta.197-x86_64-pc-windows-msvc"
$base = "https://github.com/jdrumgoole/SecantusDB/releases/download/$tag"
Invoke-WebRequest "$base/$stem.tar.gz" -OutFile "$stem.tar.gz"
Invoke-WebRequest "$base/$stem.tar.gz.sha256" -OutFile "$stem.tar.gz.sha256"
# The .sha256 is in the `<hash>  <filename>` format shasum/sha256sum print:
(Get-FileHash "$stem.tar.gz" -Algorithm SHA256).Hash -eq `
  ((Get-Content "$stem.tar.gz.sha256") -split '\s+')[0].ToUpper()
tar xzf "$stem.tar.gz"                 # unpacks into .\$stem\
& ".\$stem\secantusd-mdb.exe" --version
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
# → crates/secantusdb/target/release/secantusd-mdb
```

The build compiles the vendored WiredTiger (`vendor/wiredtiger`,
mongodb-7.0 line) and statically links it — the resulting binary is
self-contained.
