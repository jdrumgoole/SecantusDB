### LISTEN connections no longer desync on a fragmented client write

A connection holding an active `LISTEN` waits for its next command in short
0.25-second slices so queued notifications flush promptly. That wait was a
socket read *timeout*, which could fire in the middle of reading a frame —
after the type byte or partway through the length or payload — and the bytes
already read were silently discarded. The next read then re-synchronized on
the wrong byte offset and misread the rest of the stream, so a legitimate but
slightly slow or fragmented client write (ordinary network jitter, not just a
malicious client) got the connection dropped with a spurious protocol error.

The idle-poll wait now uses `select` to wait for readability and only reads a
complete frame once the socket has data, with a blocking recv — so a poll
wakeup can never truncate a frame. A frame whose tail is delayed simply blocks
the recv until it arrives, exactly as the non-listening default path already
did. Async NOTIFY delivery to an idle listener is unchanged.

#### Fixed

- A `LISTEN`-holding connection could desync its PostgreSQL wire stream when a
  frame's bytes straddled the 0.25s notification-poll window, dropping the
  connection on ordinary network jitter (#882). The poll now waits with
  `select` and reads whole frames only.
