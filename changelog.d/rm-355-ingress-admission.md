### Every command passes mongod's ingress admission control

mongod admits every command through an ingress admission controller before it runs it. The controller is a pool of tickets: the commands it subjects to the control (the CRUD, catalog and query commands) take one, and wait in a queue while none is free. Everything else, `ping` and `serverStatus` among them, is admitted without a ticket. The Rust MongoDB server now has the same controller, ported from mongod 8.2.11's `TicketHolder`. The pool holds 1,000,000 tickets by default, as on mongod, so it limits nothing until it is lowered, and it is reported in `serverStatus.queues.ingress`. Each command's `Slow query` line now says what the controller did with it: `queues: {ingress: {admissions: 1}}`, and how long it queued when it had to wait. A command waiting for a ticket ends as it does on mongod. A spent `maxTimeMS` gives 50. `killOp` gives 11601. A queue already as deep as allowed gives 433, labelled `SystemOverloadedError`. This is the first part of mongod's admission control. Execution control's read and write ticket pools come next.

#### Added

- Ingress admission control: the `ingressAdmissionControlEnabled`, `ingressAdmissionControllerTicketPoolSize` and `ingressAdmissionControllerMaxQueueDepth` parameters, settable at startup and at runtime, with mongod 8.2.11's defaults and bounds. `serverStatus.queues.ingress` reports the pool's tickets and each priority's queue statistics with mongod's fields and types, and leaves the section out while the control is off.
- A command is admitted where mongod admits it: after it has parsed and been authorized, and before its read concern is applied. Its `maxTimeMS` now counts from that point, so time spent queued is taken from its budget.
- Every `Slow query` line has `queues.ingress`, an update's and a delete's statement lines included. An index build's line has `ingress: {}`, as mongod's has.

#### Changed

- A write sent to a server inside its `replSetStepDown` window is refused with 10107 before its write concern is checked, as mongod refuses it.

#### Fixed

- Fixes RM-355 in part. Pinned by tests/mongod_live/test_ingress_admission.py (`test_the_commands_mongod_subjects_take_a_normal_ticket`, `test_an_empty_pool_queues_the_commands_it_subjects`, `test_a_line_says_what_ingress_admitted` and three more). RM-355's gap tests (`tests/backlog/test_rm_355_364.py::test_a_line_says_what_execution_control_admitted`, `tests/backlog/test_rm_001_028.py::test_slow_query_line_says_where_the_time_went`) wait on execution control.
