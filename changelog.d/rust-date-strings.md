### Rust MongoDB server: 12-hour times and zone names in date strings

`$toDate` and `$dateFromString` returned wrong times on the Rust MongoDB
server for every 12-hour time and every zone abbreviation, with no error. They
now match mongod 8.2.11 on all 123 probed strings; 61 differed before.

#### Fixed

- A 12-hour time came back hours out, often on another day. `"2024-01-01 10:00 PM"`
  was 01:00, and `"12:00 AM"` was 23:00 the previous day. Times are now read
  as mongod reads them: hours 1–12, with `am`/`pm` or `a.m.`/`p.m.`.
- A zone abbreviation came back hours out: `"… 22:30 UTC"` was twelve hours
  off, and `GMT`, `EST` and `PST` were wrong too. There are 51 abbreviations,
  each with the offset mongod gives it; some are timelib's historical values,
  such as `IST` +2. Abbreviations mongod does not know are refused.
- Second 60 (`"23:59:60"`) is refused, as mongod refuses it. It used to roll
  over into the next day.

#### Added

- These date-string forms now parse as on mongod:
  - an offset after a space (`"22:30 +02:00"`);
  - a time before the date;
  - a month-name date followed by a time;
  - ctime order (`"Jan 1 10:00:00 2024"`);
  - month and year (`"Jan 2024"`, `"Sept 2024"`);
  - a leading weekday, which moves the date forward to that weekday, as
    mongod does.
- `tools/probes/date_string_parsing.py`, a 123-string probe that compares
  against mongod.
