### Date extractors error on a non-date input (both servers)

All thirteen date-component extractors — `$year` / `$month` / `$dayOfMonth` /
`$hour` / `$minute` / `$second` / `$dayOfWeek` / `$dayOfYear` / `$week` /
`$isoWeek` / `$isoDayOfWeek` / `$isoWeekYear` / `$millisecond` — now raise
mongod's `Location16006` ("can't convert from BSON type … to Date") when given a
present non-date value (a string, a number, a bool, …), instead of silently
returning `null`. A `null` or a missing field still yields `null`, as before.

#### Fixed

- `expressions.py` / `secantus-core`: the shared date-operand resolver
  (`_date_operand` / `date_operand_millis`) distinguishes a null / missing operand
  (→ null) from a present non-date value. The Python server raises `Location16006`;
  the Rust server surfaces a generic `BadValue` on that path (the documented
  error-code gap). Verified three-way vs real `mongod` 6.0 (Python zero
  divergences).
