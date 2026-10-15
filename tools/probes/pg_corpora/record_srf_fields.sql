# reference-version: 15
# A built-in setof-record function in a select list: (f()).field, (f()).* and f() alone -- aclexplode, pg_options_to_table, ts_parse, ts_token_type, ts_debug, pg_get_keywords (RP-015).
select (aclexplode('{postgres=ar/postgres}'::aclitem[])).privilege_type order by 1
select (aclexplode(acl)).privilege_type, (aclexplode(acl)).is_grantable from r15_t order by 1
select aclexplode(acl) is not null from r15_t limit 1
select (pg_options_to_table(array['a=1', 'b=x'])).option_name order by 1
select (pg_options_to_table(array['a=1'])).*
select (ts_parse('default', 'foo bar')).token
select (ts_parse('default', 'foo bar')).*
select (ts_token_type('default')).alias order by 1 limit 5
select (ts_debug('english', 'The cats')).lexemes
select (pg_get_keywords()).word order by 1 limit 3
select (aclexplode(acl)).nosuch from r15_t
select (aclexplode(acl)).privilege_type, (aclexplode(acl)).grantee = (select oid from pg_roles where rolname = 'postgres') from r15_t order by 1
