#### Fixed

- Wheel builds: the embedded extension's PGO wiring passed cargo a fresh
  `RUSTFLAGS` (`-Cprofile-use=…`), clobbering the ambient flags cibuildwheel
  sets for the Linux containers — `-Ctarget-feature=-crt-static`, without which
  the musllinux target cannot produce a cdylib. Every manylinux/musllinux wheel
  build since the PGO change failed with "cannot produce cdylib … does not
  support these crate types". The PGO flags now append to the ambient
  `RUSTFLAGS` instead of replacing them (an empty ambient value composes to the
  previous behaviour, so macOS/Windows builds are unchanged).
