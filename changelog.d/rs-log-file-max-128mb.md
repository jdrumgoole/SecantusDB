### `secantusd-rs` starts with a 128 MB store, not a 2 GB one

WiredTiger allocates a log file in full when it creates one, and
`secantusd-rs` asked for 2 GB files. A server that had taken no write had a
2 GB store.

#### Changed

- `--log-file-max` (and `[storage] log_file_max`) defaults to `128MB`, as
  the embedded handle already did. Pass `--log-file-max 2GB` for the old
  behaviour. An existing store opens as before.
- Measured on a 16-vCPU droplet with the clients on two others, half
  inserts and half updates of 1 KiB documents: 22,701 operations a second
  at 2 GB and 22,422 at 128 MB with 16 writers, 16,010 and 16,102 with 64.
  The three trials of one size differ by more than that.
