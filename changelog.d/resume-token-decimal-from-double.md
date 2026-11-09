### Resume tokens whose decimal mongod rebuilds from a double answer as mongod does

A resume token in mongod's own format is read on the Rust MongoDB server by a
transcription of mongod's KeyString decoder, so a client handing over a token
it cannot use gets the error mongod would give. One kind of number was still
out of reach: a decimal that mongod rebuilds from a double and a
continuation, in a small or large magnitude or a fraction. Such a token
answered 40647, where mongod answers the decoder's own error or reads the
token. The decoder now does the same Decimal128 arithmetic mongod does there --
the directed conversion from the double, the 15-digit conversion, the
multiply that recovers the tiniest values, the continuation added at 34
digits -- and then rescales the exponent to the stored type bits.

#### Fixed

- `secantus-commands` (`resume_keystring`): a decimal rebuilt from a double
  and a continuation, a huge decimal written out directly, and a NaN or
  infinity with a continuation now decode as mongod 8.2.11 decodes them
  (50815, 50829, 50830, 50850 and the rest of the exponent checks), instead
  of answering 40647. A differential sweep of 21,800 such tokens against
  mongod 8.2.11 found no difference in code or message.
  Fixes RM-075 (tests/backlog/test_rm_057_084.py::test_a_decimal_from_a_double_in_a_token);
  regression tests in `tests/mongod_live/test_resume_token_decimals.py`.
