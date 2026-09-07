### A double in an error message was rendered the wrong way — in both servers

mongod has **two** renderings for a double inside an error message, and they
are not interchangeable. Measured one value at a time against 8.2.11:

| value | value form | spec form |
| --- | --- | --- |
| `-0.0` | `-0` | `-0.0` |
| `-1.0` | `-1` | `-1.0` |
| `1234567.0` | `1.23457e+06` | `1234567.0` |
| `-2147483648.0` | `-2.14748e+09` | `-2147483648.0` |
| `0.000123456789` | `0.000123457` | `0.000123456789` |

The **value** form (`Value::toString`) is C's `%g` at precision 6 and is what
`$mergeObjects`, `$replaceRoot`, `$ln`, `$log` and `$log10` echo. The **spec**
form is `%.16g` with a `.0` appended when that leaves no `.` or `e`, and is what
a stage's echoed specification uses — `$firstN`/`$lastN`/`$maxN`/`$median`'s
"specification must be an object" and `$graphLookup`'s missing-`from` message.

The spec form is **not** the shortest round-trip form, though the two agree for
every ordinary value, which is why it was written that way first and why the
unit tests — written from the same assumption — passed. They part company at the
bottom of the range, where the shortest string that round-trips is shorter than
sixteen significant digits: `1e-308` echoes as `9.999999999999999e-309` and
`5e-324` as `4.940656458412465e-324`. Only the differential gate against a real
mongod caught that, which is the argument for putting a finding there rather
than in a test that drives our own servers.

A single renderer was serving both, so every value message rendered a double
the spec way. Switching that renderer is not enough on its own: it fixes
`$mergeObjects` and `$replaceRoot` and silently **breaks** `$graphLookup`, which
is a spec echo that happened to share the function. `$graphLookup` now takes the
spec renderer explicitly.

The Rust server was wrong in a louder way. Its value renderer cast an integral
double to `i64`, which lost the sign of `-0.0` (printing `0`) and **saturated**
past `i64::MAX` — so `1e308` came back as `9223372036854775807`, a flatly wrong
number shown to the user. Its spec renderer used `{:.1}`, which expanded `1e308`
into its full 309-digit decimal value; Rust has no `%g` at all, so both
precisions are now reproduced by one routine.

Across the expression corpus this takes the Rust server's message-only
divergences from 13 to 1 and the Python server's from 173 to 9.

Note `$ln` renders its operand as a **double** whatever its BSON type: an Int32
`-2147483648` comes back as `-2.14748e+09`, not as itself.

`tests/test_mongod_differential.py` gains 52 cases covering both vocabularies,
so a future simplification that collapses the two renderers fails on one side or
the other. `tools/probes/double_error_rendering.py` is the three-server harness.

#### Fixed

- `secantus.bsontypes`: new `fmt_double_value` (C's `%g`); `fmt_double_parse`
  becomes `%.16g` rather than `repr`, which fixes the denormal end of the range;
  `bson_value_repr_stage` renders a double the value way.
- `secantus.aggregate`: `$graphLookup`'s missing-`from` echo uses the spec
  renderer, which also removes the manual brace re-spacing it needed.
- `secantus.expressions`: `$ln` / `$log` / `$log10` domain messages render the
  operand as a double in the value form.
- `secantus-core`: new `format_double_g` / `format_double_spec`;
  `render_value_compact` and the three log-family messages use the first, and
  the command layer's `render_stage_value` uses the second.
