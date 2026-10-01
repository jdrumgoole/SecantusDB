# reference-version: 15
# CREATE AGGREGATE's state and final functions resolved against PostgreSQL's
# own signatures (any built-in, a C function's name included).
create aggregate aggs_a(int) (sfunc = int4larger, stype = int)
select aggs_a(x) from (values (3), (7), (5)) v(x)
create aggregate aggs_b(text) (sfunc = int4larger, stype = int)
create aggregate aggs_c(int) (sfunc = nosuchfn, stype = int)
create aggregate aggs_d(int) (sfunc = int4larger, stype = text)
create aggregate aggs_e(float8) (sfunc = float8larger, stype = float8)
select aggs_e(x) from (values (1.5::float8), (2.5)) v(x)
create aggregate aggs_f(int) (sfunc = int4pl, stype = int, finalfunc = int4abs)
select aggs_f(x) from (values (-3), (-4)) v(x)
create aggregate aggs_g(text) (sfunc = textcat, stype = text, finalfunc = upper)
select aggs_g(x) from (values ('a'), ('b')) v(x)
create aggregate aggs_h(int) (sfunc = int4pl, stype = int, finalfunc = lower)
drop aggregate aggs_a(int)
drop aggregate aggs_e(float8)
drop aggregate aggs_g(text)
drop aggregate aggs_f(int)
