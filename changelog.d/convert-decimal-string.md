### `$toDecimal` of a long numeric string answers what mongod answers

Converting a numeric string with more than 34 significant digits to a decimal,
through `$toDecimal` or `$convert`, failed with an internal server error on both
SecantusDB servers, with or without `onError`. `mongod` accepts it: it keeps the
first 34 digits, dropping the rest without rounding, so
`"1.99999999999999999999999999999999999"` becomes
`1.999999999999999999999999999999999`. Both servers now do exactly that.

A string outside the range a decimal can hold now fails the way `mongod` fails,
as a conversion error that `onError` catches, with `mongod`'s own reason: "would
overflow" above about `1E+6144`, and "would underflow" for a value too small to
keep all its digits (`1E-6176` still converts; `1E-6177` does not).

A constant conversion that fails is now reported under the error name
`ConversionFailure`, as `mongod` reports it, instead of `Location241`.

#### Fixed

- `expressions.py`: `$convert` / `$toDecimal` of a string parse as `mongod` does:
  34 digits rounded toward zero, IEEE overflow and underflow as conversion
  failures.
- `crates/secantus-core`: the same for the Rust engine (`decimal128_from_str`),
  which relied on a parser that refuses a 35th digit.
- `commands.py`: error code 241 is named `ConversionFailure`.

#### Testing

- `tests/test_mongod_differential.py`: eleven cases against a real `mongod`
  8.2.11: truncation, both range failures, the boundaries, and `onError`.
- `crates/secantus-core`: unit tests with the same `mongod`-measured values.
- A probe of 66 cases across both engines found no differences from `mongod`.
