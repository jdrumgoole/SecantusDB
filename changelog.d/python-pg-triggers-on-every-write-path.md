### Python PostgreSQL server: triggers fire on ON CONFLICT, UPDATE FROM, DELETE USING and MERGE

These write paths used to refuse any table with a trigger. They now fire triggers in PostgreSQL's order, checked against PostgreSQL 15.

#### Added

- `INSERT ... ON CONFLICT` fires BEFORE INSERT ROW for every proposed row. `EXCLUDED` sees the row as that trigger returned it. A conflicting `DO UPDATE` then fires BEFORE UPDATE ROW.
- `UPDATE ... FROM` and `DELETE ... USING` fire their statement and row triggers, including `UPDATE OF`.
- `MERGE` fires BEFORE STATEMENT for each action its WHEN clauses name, row triggers per action, and the AFTER STATEMENT triggers in reverse.
- On every path, AFTER ROW events queue to the end of the statement. A BEFORE ROW trigger can change the row or skip it.
