### PITR restore no longer writes 2 GB for a tiny database

Every point-in-time restore wrote 2 GB to disk regardless of how much data it
was restoring. WiredTiger preallocates its log file to `log_file_max` (2 GiB)
and that file is almost entirely zeros, so it compressed to nothing inside the
backup and expanded to full size on the way out: a database holding 100
documents archived to 2.0 MB and restored to 2.0 GB.

#### Fixed

- Backup extraction writes files sparsely, seeking past runs of zeros instead
  of writing them. The restored bytes are unchanged — holes read back as zeros
  — so this changes only how a restore reaches the disk, never what WiredTiger
  subsequently reads. A restored directory drops from **2.0 GB to 276 KB**, and
  a restore that took 858 seconds on a busy disk now takes seconds.
