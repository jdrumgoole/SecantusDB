DROP TABLE IF EXISTS sisl_t CASCADE
DROP TABLE IF EXISTS sisl_e CASCADE
DROP TABLE IF EXISTS sisl_ct CASCADE
DROP FUNCTION IF EXISTS sisl_pairs(int)
DROP FUNCTION IF EXISTS sisl_recs(int)
DROP TYPE IF EXISTS sisl_pair CASCADE
create table sisl_t (a int, j jsonb, js json, n int)
insert into sisl_t values (1, '{"x":1,"y":"q"}', '{"x":1}', 2), (2, '{}', '{"z":[1,2]}', 0), (3, null, null, null)
create table sisl_e (a int)
create type sisl_pair as (k text, v int)
create function sisl_pairs(n int) returns setof sisl_pair language sql as 'select ''k'' || g, g from generate_series(1, n) g'
create function sisl_recs(n int, out k text, out v int) returns setof record language sql as 'select ''r'' || g, g * 10 from generate_series(1, n) g'
create table sisl_ct (p sisl_pair)
insert into sisl_ct values (row('k', 1))
