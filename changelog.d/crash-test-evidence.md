### A crash-recovery test records why it fails, the next time it does

`test_hard_kill_mid_transactions_keeps_every_acked_commit_and_its_history` failed once during a local gate. After the hard kill, a snapshot read at the first acknowledged commit's cluster time was refused as too old, because that time was 16 minutes before the run began. It has not been reproduced. The test now checks that its ack file is new and that every ack's cluster time falls inside the run's own wall-clock window. A repeat will show whether the ack file came from another run or the server minted a stale timestamp. The case is filed as RM-401.

#### Changed

- `tests/test_crash_recovery.py`: two checks that capture the cause of RM-401's failure. Filed RM-401.
