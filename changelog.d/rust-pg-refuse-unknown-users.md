### Rust PostgreSQL server: `--refuse-unknown-users`

The Rust PostgreSQL server lets a user name it has no role for connect
without a password. `--refuse-unknown-users` stops that. The default is
unchanged, because test fixtures connect as roles nobody created.

#### Added

- `secantusd-pg --refuse-unknown-users`, `PgBuilder::refuse_unknown_users`
  and `ServerOptions` with `bind_with` for a caller that binds the server
  itself. A user there is no role for is asked for a password and the login
  fails as a wrong password does, which is what PostgreSQL answers under a
  `scram-sha-256` line. Under `--tls-client-cert cert` the certificate is
  checked and the role is then reported missing. A role with no password
  still connects without one.

Run against PostgreSQL 15.19 in the five login configurations of the TLS
comparison, with 65 unknown-user scenarios added: 219 of 219 answer the
same with the flag, and 171 without it.
