### Rust pgserver: uuid binary parameters

A `uuid` sent as a BINARY parameter (16 raw bytes, oid 2950 — psycopg's default
for a Python `UUID`) is now decoded to its canonical lowercase text, the same
value the text path stores. Previously only the text form was accepted, so a
bound `UUID` failed with `binary parameters of type oid Some(2950) are not
supported`.

#### Added
- Binary decode of `uuid` (oid 2950) parameters.
