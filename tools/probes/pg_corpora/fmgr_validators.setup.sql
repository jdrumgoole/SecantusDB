DROP FUNCTION IF EXISTS fm_sql(int)
DROP FUNCTION IF EXISTS fm_pl()
create function fm_sql(int) returns int language sql as 'select $1'
create function fm_pl() returns int language plpgsql as 'begin return 1; end'
