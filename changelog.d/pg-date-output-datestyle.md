### A date made text inside an expression follows the DateStyle

PostgreSQL prints a `date`, `timestamp` or `timestamptz` through its output
function wherever a value becomes text -- `concat`, `format`, `quote_literal`,
`||`, a record's text, an array's text -- and that function writes in the
session `DateStyle`: under `German`, `concat('2020-01-02'::date)` is
`02.01.2020`. The Rust PostgreSQL server wrote ISO on every one of those paths
whatever the setting, though a plain `::text` cast already followed it. Each
now writes in the session DateStyle, while JSON and XML keep writing ISO, as
PostgreSQL's do under every style.

#### Fixed

- `secantus-pgplan` `typeout`: `output_text` and `cast_text` print a `date`,
  `timestamp` or `timestamptz` in the session `DateStyle` (and a `timestamptz`
  in its zone label, `01.01.2020 12:00:00 UTC`), arrays' elements included;
  the new `iso_output_text` is the ISO form `to_json` and the XML functions
  use. `date` joins the types printed by their type, so `concat`, `format`
  (`%s`, `%L`, `%I`), `quote_literal`, `quote_nullable`, `||` and a `ROW(...)`
  field take it. `session_output_text` (RP-864's DEFAULT printer) folds into
  `output_text`. Measured against PostgreSQL 15.19 under German, ISO,
  `SQL, DMY`, `SQL, MDY` and `Postgres, MDY`.
- Fixes RP-894 (tests/backlog/test_rp_found_rp864.py::test_a_date_as_text_in_an_expression_follows_the_datestyle)
