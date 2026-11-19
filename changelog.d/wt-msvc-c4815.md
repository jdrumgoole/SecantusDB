### WiredTiger builds again on MSVC 19.52

#### Fixed

- Every Windows WiredTiger build broke when the GitHub Windows image moved
  from MSVC 19.51 to 19.52 (image 20261111): `src/log/log.c(1026): error
  C2220` / `warning C4815: 'logrec': flexible array member in object will
  have no elements`, made an error by WiredTiger's strict-mode `/WX`. The
  code is correct. `__log_record_verify` copies a `WT_LOG_RECORD` header to
  the stack on purpose and never reads `record[]` through it.
- `cmake/patch_wt_strict.py` now keeps `/WX` and disables only C4815
  (`/wd4815`), so any other new MSVC warning still fails the build. It is
  narrower than the `-Werror` removal it already makes for GCC and Clang. The
  patcher is now idempotent per line, so a copy patched by the old script
  picks up the new edit. The `secantus-wiredtiger-sys` copy is refreshed.
