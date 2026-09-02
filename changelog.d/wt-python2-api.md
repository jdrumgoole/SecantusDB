### macOS builds broke when SWIG shipped 4.5.0

Building from source on macOS started failing partway through the vendored
WiredTiger step, with a wall of errors about calls to undeclared functions
named `PyInt_AsLong` and `PyString_InternFromString` — names from the Python 2 C
API, which have not existed since Python 3 was released.

The cause is upstream and outside this project. WiredTiger's own SWIG typemaps
still call those functions, and it has been getting away with it because SWIG
used to paper over them: every generated wrapper carried a block of
compatibility macros redefining the Python 2 spellings in terms of their Python
3 equivalents. SWIG 4.5.0 removed those macros, so the day Homebrew started
serving it, the same unchanged source stopped compiling.

It looked intermittent, which is worth explaining. The wrapper is only generated
when the WiredTiger build is not restored from cache, so a job with a warm cache
skipped the whole step and passed while a sibling with a cold one failed — the
same commit, green and red on adjacent machines, drifting redder as caches
expired.

The typemaps now call the Python 3 functions directly, applied to the vendored
tree at build time by the same mechanism as the project's other WiredTiger
patches. That removes the dependency on SWIG's compatibility layer entirely, so
it no longer matters which version generates the wrapper.

#### Fixed

- Source builds failed against SWIG 4.5.0 with undeclared Python 2 C API
  functions in WiredTiger's Python bindings.
