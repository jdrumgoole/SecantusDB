### Rust MongoDB server: `rotateCertificates`

#### Added

- `rotateCertificates` re-reads the TLS certificate, key and CA files, and
  new connections get the new certificate. It used to be 59
  CommandNotFound. A file that does not load is 140
  InvalidSSLConfiguration ("Unable to load PEM from '<path>': ..."), and
  the certificate in use stays. With TLS off it answers `ok`, as mongod
  8.2.11 does; its `message` argument and admin-only rule are checked as
  on mongod.

RM-219 part (`tests/backlog/test_rm_219_rotate_certificates.py`).
