### The Rust MongoDB server's startup log reports flow control

The startup log now carries mongod 8.2.11's `Flow Control is enabled on
this deployment` (20536) on `initandlisten`, right after `Dropping spill
idents`, as mongod writes it.

#### Fixed

- Startup writes mongod's flow-control line (RM-305 in part).

Fixes RM-305 in part (tests/backlog/test_rm_001_028.py::test_startup_reports_flow_control_after_the_spill_idents)
