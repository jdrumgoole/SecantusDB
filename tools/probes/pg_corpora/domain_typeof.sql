# reference-version: 15
# pg_typeof over a column of a domain type names the domain, qualified or not, in a select list and a WHERE (RP-038).
select pg_typeof(c)::text from dt_t order by id
select pg_typeof(c), pg_typeof(n), pg_typeof(p) from dt_t order by id
select pg_typeof(t.c)::text from dt_t t order by id
select pg_typeof(c + 1)::text, pg_typeof(n || 'a')::text from dt_t order by id
select id from dt_t where pg_typeof(c) = 'dt_d'::regtype order by id
select pg_typeof(c)::oid = 'dt_d'::regtype::oid from dt_t limit 1
select c, n from dt_t order by id
