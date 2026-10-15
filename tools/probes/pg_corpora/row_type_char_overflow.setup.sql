DROP TABLE IF EXISTS ovf_t
DROP TYPE IF EXISTS ovf_ct
create table ovf_t (c char(3), v text)
create type ovf_ct as (c char(2))
