### Rust PostgreSQL server: a database's encoding is its server encoding

A database created with `ENCODING 'LATIN1'` (or any other) behaves as
PostgreSQL 15.19's does in that encoding, where every database behaved as
UTF8 inside.

#### Fixed

- A session's server encoding is its database's (RP-064, Joe's decision of
  2026-10-22: emulate the encoding).
  - **The parameters.** `server_encoding` is reported at connection and by
    SHOW. It is `client_encoding`'s default when the client names none.
  - **The client's input.** Query text, bound parameters and COPY data are
    converted to the server encoding as they arrive. A character it has
    no byte for is 22P05, `character with byte sequence 0xe2 0x82 0xac in
    encoding "UTF8" has no equivalent in encoding "LATIN1"`.
  - **The byte functions** work in the server encoding's bytes:
    - `octet_length`, `bit_length`, `md5` and the text-to-bytea cast;
    - `pg_column_size`, and a tuple's size in the heap model, so a table's
      pages are PostgreSQL's;
    - `chr(n)` takes a byte of a single-byte encoding: `chr(128)` is that
      encoding's character, and past 255 is 54000. `chr(0)` is 54000 too;
    - `ascii` reads the first byte;
    - a multi-byte encoding refuses either past 127;
    - `convert_from` refuses a character the server encoding lacks.
  - **How it is held.** Text is still kept as Unicode inside. Its bytes in
    the server encoding are made when a function asks for them.

  Fixes RP-064 (tests/backlog/test_rp064_encoding.py::test_latin1_database_behaves_as_latin1,
  tests/backlog/test_rp_batch70.py::test_each_encoding_is_its_databases_server_encoding).
