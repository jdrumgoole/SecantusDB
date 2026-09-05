### A change-stream test asserted which 1-second window an event arrived in

`test_absent_max_time_ms_still_waits_and_delivers` inserted after 150 ms and
asserted that **one** `getMore` came back with the event. That holds only while
the inserting thread is scheduled inside the server's 1 s default tailable
wait. On a loaded runner it isn't: the window closes empty and the test fails
with `[] == ['insert']`. Seen on macOS CI, and reproduced here by delaying the
insert past 1 s.

Delivering the event on the **next** `getMore` is correct behaviour, not a bug —
so the test was asserting something the server never promised. The property it
exists to protect is "an absent `maxTimeMS` waits for an event and delivers it",
and that is now pinned as two separate assertions instead of one race:

1. **It waits.** With nothing pending, an absent `maxTimeMS` must block rather
   than poll once — the inverse of the explicit `maxTimeMS: 0` pinned by the
   test above it.
2. **It delivers.** An insert issued while a `getMore` is blocking comes back,
   in that window or the one after it.

**The guard was checked both ways**, because a flake "fixed" by loosening an
assertion is worse than the flake:

- with the insert delayed to 1.4 s — the exact CI failure — the test now passes;
- with the server not blocking (simulated by sending `maxTimeMS: 0`), it still
  **fails**: `an absent maxTimeMS returned after 0ms; it must WAIT`.

Its siblings were checked for the same shape: `test_change_streams.py` already
polls to a deadline, so this single-window assumption was unique to this test.

#### Fixed

- `tests/test_getmore_maxtimems_zero.py`: assert the blocking property directly
  and let delivery span more than one window.
