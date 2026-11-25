### The PostgreSQL login comparison names a reference whose OpenSSL predates the delta-CRL fix

`secantusd-pg` never uses a delta revocation list as a certificate's list,
as PostgreSQL does on a current OpenSSL (RP-747). That answer is OpenSSL's,
and it came with a fix: before each line's 2026-08-25 release (3.0.22,
3.4.7, 3.5.8, 3.6.4, 4.0.2), `get_crl_score` reached its "don't process
deltas" check only with extended CRL support turned on, which PostgreSQL
never does, so a delta was read as a full list. A PostgreSQL reference
linking an older OpenSSL, such as Homebrew's `openssl@3` 3.6.3, made the TLS
login comparison report six differences that were the reference's, not the
server's. The comparison now reads which OpenSSL the reference links and
refuses an unfixed one by name.

#### Changed

- `tests/test_rust_pg_login_differential.py`: before the TLS probe runs, the
  test finds the `libcrypto` the reference `postgres` links (`otool -L` or
  `ldd`) and asks it for its version. One older than the fix on its line
  fails the test with that version and the releases to use, instead of
  showing up as six `crl scope delta` divergences. A new test,
  `test_a_reference_before_the_delta_fix_is_named`, pins the version rule.
  Measured on PostgreSQL 15.20: OpenSSL 3.6.3 differs in those six
  scenarios; 3.0.22, 3.5.9, 3.6.5 and 4.1.0 agree in all 547.
- `crl.rs`, the RP-013 scope-decision bullet and the
  `pg15-current-openssl` action now give that mechanism and the fix
  releases, where they cited only the versions measured.
