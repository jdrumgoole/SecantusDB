### The Rust servers build on Windows again

`secantus-commands` stopped compiling on Windows when the execution
admission control ticket pools landed (RM-355, in part): `logical_cores()`
called `libc::sysconf`, and `libc` is a unix-only dependency of the crate.
Every Windows build of the storage-engine wheel failed with it, which took
the Rust PG server's psycopg gauge on Windows down on `main`.

#### Fixed

- `logical_cores()` reads `sysconf(_SC_NPROCESSORS_ONLN)` on unix as before
  and, elsewhere, `std::thread::available_parallelism`, which on Windows is
  `GetSystemInfo`'s `dwNumberOfProcessors`, the value mongod's
  `ProcessInfo::getNumLogicalCores` reports there.
