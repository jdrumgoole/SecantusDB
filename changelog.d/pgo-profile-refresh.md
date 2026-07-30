### Refreshed PGO profile for the reworked write path

The committed profile-guided-optimization profile
(`crates/pgo/_secantus_server.profdata.tar.gz`) is regenerated against the
current hot paths — the oplog visibility point, the key-only prune, and the
new routing defaults all reshaped the write path since the profile was last
trained, and a stale profile silently forfeits PGO's gains (measured: the
refresh recovered `update_many` 1.2×→1.1×, `$group` 1.3×→1.0×,
`delete_many` 1.5×→0.9× of mongod on the six-workload benchmark).

#### Changed

- `crates/pgo/_secantus_server.profdata.tar.gz` retrained on the post-#702
  write path (wheel builds consume it; a stale profile is safe but slower).
