### Python PostgreSQL server: statement-level triggers on views

`BEFORE` / `AFTER ... FOR EACH STATEMENT` triggers on a view are now accepted, and they fire in PostgreSQL's order: around the view's `INSTEAD OF` rows, and not for a write through an automatically-updatable view, which fires the base table's triggers instead. Checked against PostgreSQL 15. This closes the last trigger shape the Python server refused.
