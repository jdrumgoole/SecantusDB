### A change stream could silently skip a write it should have delivered

When a change-stream poll found nothing for the watched namespace, the cursor
skipped forward to the end of the oplog so that a quiet collection's resume
token keeps moving past unrelated activity. The skip was bounded by the oplog's
tail — and that is not the same as what the poll had actually looked at.

Two writes could fall through the gap. One committing between the scan and the
tail read is counted by the tail while never having been examined. And the tail
is the highest sequence number *handed out*, which a writer takes before it
commits its batch, so the tail can name a write no reader can see yet. Either
way the cursor stepped over that write, and because a change stream only ever
moves forward, no later poll could return it: the event was gone, not late.

The skip is now bounded by the highest position the poll actually examined,
rejected entries included. Anything committed after that necessarily sorts
later and is delivered on the next poll. The window was microseconds wide, which
is why it surfaced only as an occasional CI failure — a test that opened a
stream, wrote a document, and waited for an event that was never coming.

#### Fixed

- Python server: a change stream no longer skips past a write that commits
  while it is polling, or one whose sequence number has been assigned but not
  yet committed. The Rust server was never affected — its poll advances only
  over entries it has scanned.
