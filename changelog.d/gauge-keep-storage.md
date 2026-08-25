### The Go gauge can keep its data directory for debugging

`SECANTUS_GAUGE_KEEP_STORAGE=1 invoke validate-go` leaves the daemon's storage
behind instead of deleting it.

#### Added

- A driver-side assertion tells you a test failed but not what the server sent,
  and some gauge failures only reproduce under the *whole* run — so by the time
  there is a failure worth explaining, the oplog that produced it has already
  been removed. Keeping it is the difference between reading the offending
  entry and guessing at it.
