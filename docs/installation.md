# Installation

SecantusDB requires **Python 3.12** or newer.

```bash
pip install SecantusDB
```

Pre-built wheels are published for CPython **3.12** and **3.13** on:

- macOS arm64 (Apple Silicon)
- Linux x86_64 and aarch64 (manylinux2014 / glibc, and musllinux_1_2 / Alpine)
- Windows AMD64

WiredTiger is vendored inside the wheel — no separate package, no compile
step, no system build tools required.

macOS Intel (x86_64) is not in the wheel matrix; macOS Intel users fall
back to the [from-source path below](#building-from-source).

## PyPI name vs import name

The PyPI distribution is `SecantusDB`. The Python import is `secantus`:

```python
from secantus import SecantusDBServer       # import name: secantus
```

This split is normal Python convention — `pip install Pillow` /
`import PIL`, `pip install scikit-learn` / `import sklearn`. PyPI is
case-insensitive on lookup so `pip install secantusdb` works too.

(building-from-source)=
## Building from source (unsupported platforms only)

If your platform isn't in the wheel matrix, `pip install SecantusDB`
falls back to the sdist and compiles WiredTiger from source. That needs
three native build tools on `PATH`:

- **`cmake`** (>= 3.21)
- **`ninja`**
- **`swig`** (>= 4.0)

| Platform | Install prerequisites |
|---|---|
| macOS (Homebrew) | `brew install cmake ninja swig` |
| Debian / Ubuntu | `sudo apt-get install -y cmake ninja-build swig` |
| Fedora / RHEL | `sudo dnf install -y cmake ninja-build swig` |
| Alpine | `apk add --no-cache cmake ninja swig build-base` |

Then `pip install SecantusDB` triggers the build.

## Development install

For working on SecantusDB itself, clone the repo and use `uv`:

```bash
git clone https://github.com/jdrumgoole/SecantusDB.git
cd SecantusDB
git submodule update --init --recursive   # vendor/wiredtiger
uv sync --extra dev                       # also builds vendored WT
uv run python -m pytest                   # full parallel suite
```

A development clone always builds WiredTiger from source (the same
build cibuildwheel runs in CI), so the prerequisites in the previous
section apply: `cmake` / `ninja` / `swig` / a C/C++ compiler must be
on `PATH`.

Common workflows:

```bash
uv run python -m invoke fmt    # ruff format
uv run python -m invoke lint   # ruff check
uv run python -m invoke test   # pytest, parallel
uv run python -m invoke docs   # build Sphinx docs (warnings as errors)
```

The test suite runs in parallel via `pytest-xdist`. Tests use `port=0`
and `:memory:` storage so they don't share state across workers.
