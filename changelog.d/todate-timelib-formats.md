### `$toDate` parses the dates MongoDB parses

`{$toDate: "12/31/2020"}` is an ordinary call. Both servers answered `241
ConversionFailure`, because both implemented a small ISO-8601 subset while
MongoDB runs **timelib** — a parser with a much larger format table. 15 of 19
measured shapes diverged.

Three of its rules are worth knowing, because none is guessable:

- **The slash form is US-first by rule, not by ambiguity-resolution.**
  `31/12/2020` is refused outright, so `MM/DD/YYYY` wins and day-first is not a
  fallback.
- **A trailing letter is a military timezone, not the ISO separator.**
  `"2020-01-01T"` is `07:00:00`, because `T` is UTC−7. This is deterministic,
  not host-local — a `TZ=UTC` server answers the same. `J` is the one letter
  timelib rejects.
- **An out-of-range component is a parse failure, not a rollover.**
  `13/01/2020` and `12/32/2020` are both refused.

#### Added

Both servers now accept, and agree with MongoDB on:

| form | example |
| --- | --- |
| US slash, padded or not, optional time | `12/31/2020`, `1/2/2020`, `12/31/2020 10:30` |
| year-first slash | `2020/12/31` |
| non-padded ISO | `2020-1-1`, `2020-1-1 10:30` |
| month names, either order | `Dec 31 2020`, `31 December 2020`, `Dec 31, 2020` |
| Unix seconds | `@1577836800`, `@-1`, `@1577836800.5` |
| compact | `20200101`, `20200101T120000` |
| ISO week date | `2020-W01-1` |
| hour with no minutes | `2020-01-01T00` |
| military timezone suffix | `2020-01-01T`, `…A`, `…Z` |
| surrounding whitespace | `"  2020-01-01"`, `"2020-01-01 "` |

Measured against mongod 8.2.11 over 45 shapes — including the refusals, which
matter as much as the acceptances: a parser that takes too much is as wrong as
one that takes too little. 0 divergent on both servers.

#### Still not reproduced

MongoDB's per-position diagnostic for a string its scanner got partway through
(`'abc'` names the offending character and where it stopped) needs timelib's own
lexer and timezone-abbreviation tables. Both servers give the same error code
and a general message rather than inventing a position.
