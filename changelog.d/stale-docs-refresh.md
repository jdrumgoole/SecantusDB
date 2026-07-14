### Docs: compatibility / authentication / index pages caught up with shipped features

Three docs pages still described the server as it was several releases ago.
`compatibility.md`'s stub table claimed `getLog` returns an empty array,
`hostInfo` / `whatsmyuri` / `buildInfo` are hardcoded, sessions are untracked,
and `serverStatus` is all zeros — all of those return real data now, so the
table shrinks to the honest remainder (`top`'s zero counters, `buildInfo`'s
deliberate `7.0.0` compatibility identity, `connectionStatus`'s empty
privileges expansion, `serverStatus`'s zeroed fallback for bare
`CommandContext` embedders). The `$lookup` stopgap section described the
pre-index-join hash-only implementation; the date-format section listed
ISO-week tokens as missing; the TTL-index row said there was no background
sweeper. All rewritten to match the code.

`authentication.md` and `index.md` both still said authorization (RBAC) is
not implemented and that an authenticated principal is fully privileged —
RBAC has been enforced for a while (built-in and custom roles, checked on
every command when `--auth` is on). `authentication.md` gains an
Authorization section documenting the enforcement model, the built-in role
list, and the custom-role / grant-revoke command set; both scope lists now
credit SCRAM-SHA-1 and MONGODB-X509 correctly.

#### Changed

- `docs/compatibility.md`: stub table rewritten to current behaviour;
  `$lookup` section describes the index-driven join (IXSCAN on a matching
  foreign-field index, hash-join fallback); date-format token list updated
  (`%G %V %j %U %u %w` all supported); TTL row documents the 60-second
  background sweeper; out-of-scope auth bullet updated (SCRAM-SHA-1
  implemented, RBAC enforced); Rust-server note updated to conformance
  parity with a pointer to the feature comparison.
- `docs/authentication.md`: RBAC documented as enforced (new Authorization
  section: built-in roles, custom roles, grant/revoke quartet, code-13
  behaviour); `createUser` example uses a real role binding.
- `docs/index.md`: in-scope and out-of-scope auth bullets updated to
  SCRAM (SHA-1/SHA-256) + MONGODB-X509 + enforced RBAC.
