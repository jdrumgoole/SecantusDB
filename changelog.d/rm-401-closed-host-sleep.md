### The crash-recovery read refused as too old was the Mac sleeping mid-test

RM-401 recorded a crash-recovery test that once found its first acknowledged commit 16 minutes older than the server's clock, so the snapshot read at that commit's time was refused as older than the oldest timestamp. `pmset -g log` gives the cause. The Mac entered sleep at 21:45:00 on 2026-11-07, the second that commit was minted, and stayed asleep until 22:01:12. The read came at 22:01:13, far outside the store's 300-second history window, and refusing it was correct: mongod refuses the same read. The fix-one batch scripts now keep the Mac awake while they run, and the test's checks for a fresh ack file and in-window acks stay in place.

#### Changed

- Closed RM-401. Its cause was the host sleeping, not the server.
