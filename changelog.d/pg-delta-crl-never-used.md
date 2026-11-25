### The PostgreSQL server never uses a delta revocation list, as current OpenSSL does not

PostgreSQL leaves client-certificate revocation checking to the OpenSSL it
links, so its answer is that OpenSSL's. Current releases (OpenSSL 3.5.9 and
4.1.0) never use a delta revocation list as a certificate's list, because
PostgreSQL does not turn on delta support. A certificate whose issuer has only
a delta list is refused with "unknown ca", even when the list does not name it.
`secantusd-pg` read a delta list as a full one, the answer of the older
OpenSSL 3.0 that CI's reference happened to link, so it let such a
certificate in. It now refuses it, as PostgreSQL on a current OpenSSL does.

The login comparisons now run against a reference on a current OpenSSL as
well. CI builds a PostgreSQL 15.20 against OpenSSL 3.5.9 once and caches it.
Before this, every local run of the comparison differed from CI's in the same
six scenarios.

#### Fixed

- `secantus-pg` (`crl.rs`): a list carrying a `DeltaCRLIndicator` is never a
  candidate, like a reasons-only or indirect list. OpenSSL's `get_crl_score`
  passes deltas over without `X509_V_FLAG_USE_DELTAS`, so an issuer with only
  a delta has no list (`X509_V_ERR_UNABLE_TO_GET_CRL`, "unknown ca"). Decided
  by Joe, 2026-11-25: where PostgreSQL's answer is its OpenSSL's, the current
  OpenSSL's is kept. Fixes RP-747
  (`tests/test_rust_pg_login_differential.py`, now 547 of 547 against
  PostgreSQL 15.20 on OpenSSL 3.5.9 and on 4.1.0).

#### Changed

- CI (`pg-oracle`, `server` shard): the login comparisons run against
  `.github/actions/pg15-current-openssl`, a PostgreSQL 15.20 built from
  checksum-verified source against OpenSSL 3.5.9 and kept in the Actions
  cache. The runner's PGDG `postgresql-15` links Ubuntu's OpenSSL 3.0, which
  gives the older answer.
