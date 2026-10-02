# Divergences the pgjdbc and pgx driver suites exposed on the Rust server
# (batch 37), each measured against PostgreSQL 15.
# current_schemas(bool): the effective path, implicit pg_catalog on request.
select current_schemas(true), current_schemas(false)
select pg_typeof(current_schemas(true))
# DISCARD: every form, and DISCARD ALL refused inside a block.
discard plans
discard sequences
discard temp
discard all
begin
discard all
rollback
# A DEFERRABLE UNIQUE constraint is judged at the end of the statement when
# immediate and at COMMIT when deferred -- it used to be judged nowhere.
create table b37_du (id text primary key, n int not null, unique (n) deferrable initially deferred)
insert into b37_du values ('a', 1), ('b', 2), ('c', 3)
update b37_du set n = n + 1 where id = 'b'
begin
update b37_du set n = n + 1 where id = 'b'
commit
begin
update b37_du set n = n + 1 where id = 'b'
update b37_du set n = n - 1 where id = 'c'
commit
select * from b37_du order by id
create table b37_dui (a int unique deferrable)
insert into b37_dui values (1), (1)
insert into b37_dui values (1), (2)
update b37_dui set a = a + 1
select * from b37_dui order by a
# nextval on a serial whose table the open block dropped and re-created: it
# advanced the OLD committed sequence and retried a write conflict with its
# own transaction forever.
create table b37_seq_t (k serial primary key, v text)
insert into b37_seq_t (v) values ('x')
begin
drop table b37_seq_t
create table b37_seq_t (k serial primary key, v text)
insert into b37_seq_t (v) values ('y') returning k
commit
select * from b37_seq_t
# Temporary tables live in the session's own pg_temp_N: they shadow a
# permanent table of the same name, pg_temp names them, and they vanish
# with the session (or a DISCARD TEMP).
create table b37_perm (a int)
create temp table b37_perm (z int)
insert into b37_perm values (5)
select * from b37_perm
select * from public.b37_perm
create temp table b37_tt (a int primary key)
insert into b37_tt values (1)
select * from pg_temp.b37_tt
select 'b37_tt'::regclass::text
create table pg_temp.b37_pt (a int)
select relname, relpersistence from pg_class where relname like 'b37_p%' order by 1, 2
drop table b37_perm
select * from b37_perm
discard temp
select * from b37_tt
# A session default set outside a block governs the next transaction at once.
set default_transaction_read_only = on
show transaction_read_only
create table b37_ro (a int)
set default_transaction_read_only = off
show transaction_read_only
# An expression over aggregates that fails is the statement's error, not a
# NULL (pgjdbc's BatchFailureTest relies on `select 0/count(*) where 1=2`).
select 0/count(*) where 1=2
create table b37_z (a int)
select 5/count(*) from b37_z
insert into b37_z values (1)
select 5/(count(*) - 1) from b37_z
# A row expression that fails while the rows are produced fails the
# statement: the writes before it in the same query roll back, and an
# explicit block is aborted.
update b37_z set a = a + 1; select 0/0 from b37_z
select a from b37_z
begin
update b37_z set a = a + 10
select 0/0 from b37_z
select 1
rollback
select a from b37_z
# A data-modifying WITH item writes once per EXECUTION -- not again for the
# Parse and the Describe of a prepared statement.
create table b37_cte (a int)
with x as (insert into b37_cte (a) values (%s) returning a) select * from x ||| [7]
select count(*) from b37_cte
# A regproc of a BUILT-IN function resolves to PostgreSQL's oid, and pg_type's
# I/O columns are regprocs (pgjdbc's type cache:
# `typinput = 'pg_catalog.array_in'::regproc`).
select 'pg_catalog.array_in'::regproc, 'array_in'::regproc::oid, 'int4in'::regproc::oid
select typname, typinput, typinput::oid, typinput = 'pg_catalog.array_in'::regproc, typreceive, typmodin from pg_type where typname in ('_int4', 'int4', 'numeric') order by 1
drop table b37_du, b37_dui, b37_seq_t, b37_perm, b37_z, b37_cte
