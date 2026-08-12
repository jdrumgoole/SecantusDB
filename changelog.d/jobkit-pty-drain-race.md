### A finished job no longer shows an empty log

The opsboard runs each job as a detached child on a pseudo-terminal and
tees everything it prints to a logfile the UI tails. That tee loop asked
the pty whether it had anything to read and, on a quiet answer, left as
soon as the child had exited. A child that wrote its output *and* exited
inside that window left its bytes sitting in the pty buffer, and leaving
discarded them — so the job finished with exit 0 and a completely empty
log. The shorter the job, the likelier it was to lose everything it said.

The loop now drains the buffer before it leaves. The comment that used to
justify the old behaviour ("a timed-out select with the child reaped means
everything has been drained") was simply untrue, and is gone.

#### Fixed

- `jobkit`'s pty tee no longer discards output written in the window
  between polling the terminal for readability and observing that the
  child has exited. This is the second race of its kind on this path; the
  regression test forces the losing interleaving deterministically rather
  than relying on timing.
