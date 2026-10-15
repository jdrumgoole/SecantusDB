### Rust PostgreSQL server: backlog batch 8

#### Added

- A key encrypted in OpenSSL's legacy PEM form (`Proc-Type: 4,ENCRYPTED`,
  `DEK-Info: AES-256-CBC,...`, as `openssl genrsa -aes256` writes it) is
  decrypted with `--tls-key-passphrase-command`, as OpenSSL reads it for
  PostgreSQL (`EVP_BytesToKey` over MD5; AES-128/192/256 or DES-EDE3 in CBC
  mode). It was refused. Closes the last code item of RP-013 (its
  remaining bullets are a decision and two probe-tooling notes), pinned by
  the `passphrase, legacy PEM` group in `tools/probes/pg_login_tls_files.py`.
- The comparison functions whose answer is a byte comparison's --
  `byteacmp`, `bitcmp`, `varbitcmp`, `btnamecmp`, `btnametextcmp`,
  `bttextnamecmp`, `bttext_pattern_cmp`, `btbpchar_pattern_cmp`,
  `network_cmp`, `gin_cmp_tslexeme`, `gin_compare_jsonb` -- answer
  PostgreSQL 15.19's numbers (the first differing bytes' difference, then
  each function's rule for an equal prefix). Part of RP-015
  (`tests/backlog/test_rust_postgres_gaps.py::test_builtin_coverage_fixed[byte-compare]`).
