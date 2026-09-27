### The admin screenshots regenerate correctly on Windows

The 22 admin-UI screenshots had not been regenerated since `0.6.0b11`
(2026-08-15), so the published images still showed a `0.6.0b10` version
badge six releases later. `tests/test_docs_screenshots.py` cannot catch
this — it checks that every documented page *has* an image, and nothing
checks what is in one.

Regenerating on Windows then surfaced two faults in the capture script
itself, both of which would have shipped a worse image than the stale one
they replaced.

#### Fixed

- `scripts/admin_screenshots.py` scrubbed machine-specific paths by
  rewriting only their PREFIX, so on Windows every segment after the
  placeholder kept its backslash and the docs would have shown
  `/var/lib/secantus\backups` and `/home/user\.secantus\embedded-data`.
  The scrubber now re-slashes the run of path characters it has just
  written, and leaves backslashes anywhere else on the page alone.

#### Changed

- Regenerated all 22 admin screenshots at `0.6.0b17`.
