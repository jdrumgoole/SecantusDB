### An aggregate's cursor-stage buffers take execution tickets of their own

The Rust MongoDB server's `aggregate` read through its cursor stage now
counts mongod 8.2.11's execution admissions on its slow-query line: each
buffer the cursor stage fills takes the global IS with a ticket of its own,
as mongod's does once the command has let its collection lock go, rather
than riding the command's ticket.

#### Fixed

- An `$addFields` over 300 documents read 50 at a time counts 4 execution
  admissions, not 2.

Fixes RM-579 (tests/backlog/test_rm_found_2026_11_12_rm360.py::test_each_cursor_stage_buffer_takes_an_execution_ticket)
