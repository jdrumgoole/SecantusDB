### Log-family domain errors now match mongod

An out-of-domain argument to the log family — `$ln` or `$log10` of a
non-positive number, `$log` with a non-positive argument or a base that is
non-positive or 1, `$sqrt` of a negative number — now raises mongod's exact
Location error (28766 / 28761 / 28758 / 28759 / 28714, messages verbatim from
a mongod 7.0.12 probe) on both servers, instead of silently returning null.
NaN inputs now propagate as NaN (IEEE, matching mongod); null and missing
still yield null.

#### Fixed

- `$ln` / `$log` / `$log10` / `$sqrt` out-of-domain arguments error exactly
  as mongod does, on both servers (the Rust engine defers those cases so the
  Python error surfaces).
