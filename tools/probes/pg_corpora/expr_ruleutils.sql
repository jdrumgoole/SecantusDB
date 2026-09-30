# reference-version: 15
# pg_get_expr / pg_get_constraintdef / information_schema.columns as
# ruleutils prints a stored expression: generated columns (CASE, a coercion
# under ||), defaults, and CHECK constraints (BETWEEN, IN), plus a policy.
create table ge_t (a int, b text, v varchar(5), n numeric, g1 int generated always as (a * 2 + 1) stored, g2 text generated always as (case when a > 0 then 'pos' else 'neg' end) stored, g3 text generated always as (upper(b::text) || coalesce(v, 'z')) stored, d1 text default lower('ABC'), d2 int default (1 + 2) * 3, d3 numeric default 1.5, d4 text default 'x' || 'y', c1 int check (c1 between 1 and 10), c2 text check (c2 in ('a', 'b')), c3 int check (c3 > 0 and (c3 < 100 or c3 = 1000)))
select attname, pg_get_expr(adbin, adrelid) from pg_attrdef d join pg_attribute a on a.attrelid = d.adrelid and a.attnum = d.adnum where adrelid = 'ge_t'::regclass order by attnum
select conname, pg_get_constraintdef(oid) from pg_constraint where conrelid = 'ge_t'::regclass order by conname
select column_name, generation_expression, column_default from information_schema.columns where table_name = 'ge_t' order by ordinal_position
create policy ge_p on ge_t using (case when a > 0 then b = 'x' else v in ('a', 'b') end)
select policyname, qual from pg_policies where tablename = 'ge_t'
select polname, pg_get_expr(polqual, polrelid) from pg_policy where polname = 'ge_p'
drop policy ge_p on ge_t
drop table ge_t
