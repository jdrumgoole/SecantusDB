# reference-version: 15
# The language validators (fmgr_internal / c / sql_validator, plpgsql_validator) and postgresql_fdw_validator: void for a function of the validator's language, 42501 naming the language's validator otherwise, 42883 for an oid naming no function; the libpq options per catalog (RP-015). A plpgsql function's language oid is per cluster, so no case names one.
select fmgr_internal_validator('abs(int4)'::regprocedure)
select pg_typeof(fmgr_internal_validator('abs(int4)'::regprocedure))::text
select fmgr_c_validator('abs(int4)'::regprocedure)
select fmgr_sql_validator('abs(int4)'::regprocedure)
select fmgr_internal_validator(0)
select fmgr_internal_validator(null) is null
select fmgr_sql_validator('fm_sql(int)'::regprocedure)
select fmgr_internal_validator('fm_sql(int)'::regprocedure)
select postgresql_fdw_validator(array['host=x'], 0)
select postgresql_fdw_validator(array['nosuch=x'], 1418)
select postgresql_fdw_validator(array['host=x'], 1417)
select postgresql_fdw_validator(array['user=x'], 1418)
select postgresql_fdw_validator(array['user=x'], 1417)
select postgresql_fdw_validator(null, 1417)
select fmgr_sql_validator(12345678)
select plpgsql_validator('fm_pl()'::regprocedure)
select fmgr_c_validator('plpgsql_call_handler()'::regprocedure)
select fmgr_sql_validator('information_schema._pg_truetypid(pg_attribute, pg_type)'::regprocedure)
select pg_typeof(postgresql_fdw_validator(array['user=x'], 1418))::text
select fmgr_internal_validator('abs(int4)'::regprocedure) is null
