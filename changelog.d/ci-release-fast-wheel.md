### Release binaries: build the throwaway wheel in fast mode

The release workflow builds the storage-engine wheel only to get the static
WiredTiger library, then discards the wheel. It now builds that wheel without
PGO or LTO, the way test.yml's fast lanes do. On Windows this removes about
50 minutes of linking an extension nothing used. The release binaries are
unchanged: they are built in their own step with their own profile settings.

#### Changed

- `release-binaries.yml`: the storage-engine wheel step sets
  `SECANTUS_PGO_DISABLE`, `CARGO_PROFILE_RELEASE_LTO=off` and
  `CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16` on that step only.
