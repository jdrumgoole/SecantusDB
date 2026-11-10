### The Rust MongoDB server's diagnostic sample carries Linux's system metrics

On Linux, mongod's full-time diagnostic data capture adds a `systemMetrics`
collector to every sample: the machine's CPU, memory, network, socket, disk,
mount, virtual-memory, file-handle and pressure counters, read from `/proc`.
The Rust MongoDB server's `getDiagnosticData` sample now carries the same
collector on Linux, read from the same files with mongod 8.2's keys, units and
number types, stamped with its own `start` and `end` just before the sample's
`end`. On macOS, where mongod has no such collector, the sample is unchanged.

#### Fixed

- `secantus-commands` (`system_metrics`, `ftdc`): on Linux the FTDC sample
  gains `systemMetrics`, a port of mongod's `ftdc_system_stats_linux.cpp` and
  `procparser.cpp` -- `cpu` (`/proc/stat`, tick counts in milliseconds, plus
  `num_logical_cores` and `num_cores_available_to_process`), `memory`
  (`/proc/meminfo`, `_kb`), `status` (the process's THP opt-in), `netstat`
  (`/proc/net/netstat` and `/proc/net/snmp`), `sockstat`, `disks` (physical
  disks with activity), `mounts` (`statvfs` space per mount point), `vmstat`,
  `files` and `pressure`; a file that cannot be read leaves mongod's `error`
  string in its section.
- Fixes RM-424 (tests/backlog/test_rm_219_diag_query_settings.py::test_linux_diagnostic_data_samples_system_metrics)
