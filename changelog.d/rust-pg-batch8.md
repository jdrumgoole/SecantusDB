### Rust PostgreSQL server: backlog batch 8

#### Added

- A key encrypted in OpenSSL's legacy PEM form (`Proc-Type: 4,ENCRYPTED`,
  `DEK-Info: AES-256-CBC,...`, as `openssl genrsa -aes256` writes it) is
  decrypted with `--tls-key-passphrase-command`, as OpenSSL reads it for
  PostgreSQL (`EVP_BytesToKey` over MD5; AES-128/192/256 or DES-EDE3 in CBC
  mode). It was refused. Closes the last code item of RP-013 (its
  remaining bullets are a decision and two probe-tooling notes), pinned by
  the `passphrase, legacy PEM` group in `tools/probes/pg_login_tls_files.py`.
