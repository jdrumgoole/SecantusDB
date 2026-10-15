DROP TABLE IF EXISTS fts_dt, fts_dg CASCADE
create table fts_dt (k int, v int, s text)
insert into fts_dt select i, case when i <= 12 then 1 else 2 end, chr(96 + (i % 3) + 1) from generate_series(1, 15) i
create table fts_dg (k int, g int)
insert into fts_dg select i, i % 4 from generate_series(1, 20) i
