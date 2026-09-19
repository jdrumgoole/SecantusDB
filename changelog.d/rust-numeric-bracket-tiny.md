### Rust server: numeric filters near the smallest exponent select the right rows

On the Rust PostgreSQL server, comparing a `numeric` column against a constant
with more than 34 significant digits and a very small exponent (around
`1E-6150`) could return the wrong rows. For `n > 1.2345678901234567890123456789012345E-6150`,
a stored `5E-6160` came back even though it is smaller, and `n <` missed it.

To compare a wide constant against the ordinary stored values, the server finds
the nearest values on either side that the storage format can hold. Near the
bottom of that format's exponent range, that calculation gave up and answered
"between 0 and 1E-6176", which does not contain the constant at all. It now
finds the true neighbours. Found while porting the same code to the Python
server, which already had the corrected version.

#### Fixed

- `crates/secantus-pgplan/src/numeric.rs`: `decimal128_bracket` keeps only the
  digits that still fit above Decimal128's exponent floor.

#### Testing

- `crates/secantus-pgplan/src/tests.rs`: the bracket contains its value for a
  35-digit value at every exponent from -6180 to -6100 (and around 0 and the
  top), both signs, plus short values at the floor.
- `tests/test_rust_pgserver_differential.py`: every comparison operator on
  such a constant, against a real PostgreSQL.
