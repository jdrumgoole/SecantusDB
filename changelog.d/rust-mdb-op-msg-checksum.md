### Rust MongoDB server: OP_MSG checksums and required flag bits

#### Fixed

- An OP_MSG whose CRC-32C checksum does not match, or that sets a required
  flag bit (one of the low 16) the server does not know, now ends the
  connection unanswered, as on mongod 8.2.11. Both used to get a normal
  reply: the checksum was trimmed without being checked. Optional high bits
  are still ignored.
- A reply to a request that carried a checksum now carries one too, as
  mongod's does.

Fixes RM-221
(`tests/backlog/test_rm_found_2026_10_20b.py::test_op_msg_bad_checksum_is_refused`,
`tests/backlog/test_rm_found_2026_10_20b.py::test_op_msg_unknown_required_flag_is_refused`).
