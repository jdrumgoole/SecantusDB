# The clocks: now() / transaction_timestamp() / CURRENT_TIMESTAMP / 'now' are
# the TRANSACTION's start, statement_timestamp() the statement's, and only
# clock_timestamp() moves within a statement.
begin
select 'now'::timestamptz = now()
select pg_sleep(0.05)
select now() = transaction_timestamp(), now() < statement_timestamp(), statement_timestamp() <= clock_timestamp()
select current_timestamp = now(), localtimestamp = now()::timestamp, current_date = now()::date
create table nw_t (t timestamptz default now())
insert into nw_t values (default)
select count(*) from nw_t where t = now()
commit
select now() - statement_timestamp() = interval '0'
select clock_timestamp() > now()
drop table nw_t
