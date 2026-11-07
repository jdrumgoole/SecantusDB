### The MongoDB gate rebuilds the core extension too

`scripts/gate_rm.py` rebuilt the two Rust MongoDB server artifacts from HEAD, but not the `_secantus_core` extension that the test suites import. When a commit was amended or rebased after the lane's last `./inv sync`, the suite's provenance check found that extension stale. Every pytest step then stopped before running a test. The gate now runs `./inv sync` in its build phase.

#### Fixed

- `scripts/gate_rm.py`: a `core-sync` step (`./inv sync`) before the server builds.
