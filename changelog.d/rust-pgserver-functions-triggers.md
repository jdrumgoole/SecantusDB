### The Rust PostgreSQL server runs user-defined functions and triggers

`CREATE FUNCTION ... LANGUAGE sql` and `LANGUAGE plpgsql` now define functions
the Rust PostgreSQL server can call — in the select list, in `WHERE`, in
`FROM` as a set-returning function, and from each other (recursion included).
PL/pgSQL runs on a new interpreter over the real PostgreSQL grammar
(`libpg_query`'s PL/pgSQL parser), so a body the reference server rejects is
rejected here at `CREATE` time too. Functions are stored in the Python
server's `__sql_functions__` shape, so either server can call the other's.

`CREATE TRIGGER` works: `BEFORE` and `AFTER`, `FOR EACH ROW` and
`FOR EACH STATEMENT`, over `INSERT`, `UPDATE`, `DELETE` and (statement-level)
`TRUNCATE`, with `UPDATE OF`, `WHEN (...)`, `TG_ARGV` and every `TG_`
variable. A BEFORE row trigger can rewrite the row (`NEW.x := ...`) or skip it
(`RETURN NULL`); an AFTER trigger can write elsewhere; an error raised in any
trigger aborts the whole statement. `pg_trigger` lists them.

`DO` blocks outside the small subset they already supported — `DECLARE`,
`IF`, loops, `EXCEPTION` handlers, `SELECT ... INTO` — now run on the same
interpreter instead of being refused.

#### Added

- `CREATE [OR REPLACE] FUNCTION` in `LANGUAGE sql` / `plpgsql`: scalar,
  `SETOF`, `RETURNS TABLE` and OUT parameters.
- A PL/pgSQL interpreter: blocks and `EXCEPTION` handlers (condition names and
  SQLSTATEs), assignment, `IF` / `CASE`, `LOOP` / `WHILE` / integer `FOR` /
  query `FOR` / `FOREACH`, `EXIT` / `CONTINUE`, `RETURN` / `RETURN NEXT` /
  `RETURN QUERY`, `RAISE` with `USING`, `PERFORM`, `SELECT ... INTO`,
  `EXECUTE`, `GET DIAGNOSTICS`, `FOUND`, `ASSERT`.
- `CREATE [OR REPLACE] TRIGGER`, `DROP TRIGGER [IF EXISTS]`, `pg_trigger`.

#### Fixed

- `DROP FUNCTION` refuses (2BP01) while a trigger calls the function, and
  `CASCADE` drops the trigger; `DROP TABLE` drops the table's triggers.
- A statement that runs user code is atomic outside a block too: a `DO` block
  or trigger that raises after writing takes those writes with it (a `DO`
  that ran `EXECUTE 'insert ...'` and then raised used to leave the row).
- `ROLLBACK TO SAVEPOINT` undoes what a trigger or function wrote to tables
  other than the statement's own target.
- A call at an arity no user function has answers PostgreSQL's
  `42883 function f(integer) does not exist`.
