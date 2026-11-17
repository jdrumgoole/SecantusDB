### A retryable findAndModify counts the locks of its image

The Rust MongoDB server's slow-query line for a retryable `findAndModify`
now counts the locks mongod 8.2.11 takes to write the command's image in
`config.image_collection`, and a retryable remove takes mongod's single
write round.

#### Fixed

- A retryable `findAndModify` takes one more transition, global and
  collection IX for its image, and none on the database, as mongod does; a
  retryable remove no longer nests a second write round in its first.
  Fixes RM-357, in part (tests/backlog/test_rm_355_364.py::test_a_retryable_find_and_modify_writes_its_image_under_its_locks).
