### Rust MongoDB server: a date-part operator's operand written as an array of one

`{$year: ["$t"]}` -- any of `$year` ... `$isoDayOfWeek` with its operand in
an array of one -- now evaluates that one element, as mongod 8.2 does:
null for a missing field, the date part of a date. The server used to
evaluate the array itself and fail with 16006 "can't convert from BSON
type array to Date", even where every document held a date.

#### Fixed

- A literal array of one around a date-part operand is unwrapped before
  evaluation, so a field path in it is the operand and a `{date: ...}` in
  it is an object expression (16006 names an object), never the options
  form.
- An array VALUE is no longer unwrapped: `{$year: "$a"}`, `{$year: {date:
  "$a"}}` and `$dateToString` of a field holding `[date]` are 16006, as on
  mongod, where they answered the date.

Fixes RM-533 (tests/backlog/test_rm_found_2026_11_10_rm215.py::test_a_date_part_takes_an_array_of_one,
tests/mongod_live/test_date_part_operand.py::test_a_date_operand_array_is_unwrapped_as_mongod_unwraps_it)
