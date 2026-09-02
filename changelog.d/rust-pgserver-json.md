### json and jsonb, and the number that gives the difference away

`json` and `jsonb` look interchangeable and are not. `json` validates its input
and stores the text it was given, so whitespace, key order and even duplicate
keys all survive a round trip. `jsonb` stores a parsed structure, so what comes
back is normalised: keys sorted, the last of any duplicate pair kept, one
canonical spacing throughout.

Key order is the part worth knowing. `jsonb` sorts keys by **byte length**
first and only then bytewise, so `z` comes before `é` — one byte against two —
and `b` comes before `aa`. It is neither alphabetical nor by character count,
and no amount of reasoning gets you there; it was measured.

Numbers are where a shortcut would have shown. A `jsonb` number is a `numeric`,
and prints as one: an exponent expands, so `-1.5e10` comes back as
`-15000000000`, while a trailing zero written in the literal survives, so
`1.10` stays `1.10` rather than becoming `1.1`. That second half is what rules
out reading numbers into floating-point values, which every general-purpose JSON
parser does by default — it gets the exponent right and silently drops the zero.
Number text is therefore kept as written and normalised the way `numeric` is.

One related fix came out of testing this. A bound parameter whose type the client
leaves unspecified is guessed from its text, and the guess used to accept `01` as
the number 1 — so `'01'::json`, which PostgreSQL rejects, was quietly turned into
valid JSON before the cast ever ran. A guess made on the client's behalf must
never make a value more acceptable than the client wrote it, so a number is now
only inferred when it round-trips to the same text.

#### Added

- `json` and `jsonb` as cast targets, literals and bound parameters in both wire
  formats, with their own type oids.

#### Fixed

- An unspecified-type parameter was inferred as a number even when the text was
  not how that number is written, which could turn invalid input valid.
