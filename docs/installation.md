# Installation

SecantusDB requires **Python 3.12** or newer.

## Native build prerequisites

`secantus` depends on the third-party
[`wiredtiger`](https://pypi.org/project/wiredtiger/) PyPI package, which
currently ships only as a source distribution. Until binary wheels are
available, `pip install secantus` triggers a from-source build of WiredTiger
that needs three native build tools on `PATH`:

- **`cmake`** (>= 3.21)
- **`ninja`**
- **`swig`** (>= 4.0)

Install the prerequisites first, then `secantus`.

### macOS

```bash
brew install cmake ninja swig
pip install secantus
```

If you use `uv`-managed Python, prefer:

```bash
uv tool install cmake
uv tool install ninja
brew install swig
uv pip install secantus
```

### Linux (Debian / Ubuntu)

```bash
sudo apt-get install -y cmake ninja-build swig
pip install secantus
```

### Linux (Fedora / RHEL)

```bash
sudo dnf install -y cmake ninja-build swig
pip install secantus
```

### Windows

WiredTiger's Python bindings have not been validated on Windows by the
SecantusDB project. macOS and Linux are the supported development targets.

## Development install

For working on SecantusDB itself, clone the repo and use `uv`:

```bash
git clone https://github.com/jdrumgoole/SecantusDB.git
cd SecantusDB
uv sync --extra dev
uv run python -m pytest    # full parallel suite
```

Common workflows:

```bash
uv run python -m invoke fmt    # ruff format
uv run python -m invoke lint   # ruff check
uv run python -m invoke test   # pytest, parallel
uv run python -m invoke docs   # build Sphinx docs
```

The test suite runs in parallel via `pytest-xdist`. Tests use `port=0` and
`:memory:` storage so they don't share state across workers.

## What's coming

Binary wheels for `secantus` (statically linking WiredTiger so the install is
a single `pip install secantus` with no native toolchain) are scaffolded on
the `wt-vendoring` branch and need CI iteration before they merge to `main`.
See `tasks/backlog.md` in the repo for status.
