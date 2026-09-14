### `rustls` bumped past RUSTSEC-2026-0285

#### Fixed

- `rustls` moved to 0.23.45 (from 0.23.40 / 0.23.43, depending on the lockfile)
  across all four Cargo lockfiles that carry it, clearing RUSTSEC-2026-0285 —
  TLS 1.3 handshake messages incorrectly accepted across encryption level
  boundaries, medium severity, fixed in 0.23.45. `rustls-webpki` came along to
  0.103.15.
