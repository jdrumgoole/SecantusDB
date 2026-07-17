### $size validates its argument like mongod

`$size` accepted or silently ignored arguments mongod rejects: a negative size
returned no match instead of erroring, a bool was accepted as `1` (Python's
`bool` is an `int`), and an integer-valued float like `2.0` was wrongly
rejected even though mongod accepts it as `2`. Both engines now validate the
argument the way mongod 7.0.12 does — it must be a number, integer-valued, and
non-negative — raising the corresponding parse error (code 2) otherwise, and
accepting an integer-valued float. Found while triaging the driver-gauge
results; three-way mongod-verified.

#### Fixed

- `$size` errors on a negative, non-integer, string, or bool argument (code 2)
  instead of silently matching nothing or accepting a bool, and accepts an
  integer-valued float — on both servers.
