DROP TABLE IF EXISTS sv_o CASCADE
DROP TABLE IF EXISTS sv_c CASCADE
DROP TABLE IF EXISTS sv_p CASCADE
DROP TABLE IF EXISTS sv_m CASCADE
DROP MATERIALIZED VIEW IF EXISTS sv_mv
DROP DOMAIN IF EXISTS sv_pos
CREATE TABLE sv_c (id int PRIMARY KEY, name text, tier text)
CREATE TABLE sv_o (id int PRIMARY KEY, cid int REFERENCES sv_c(id), amt numeric(10,2), at timestamptz, tags text[], meta jsonb)
INSERT INTO sv_c VALUES (1,'ann','gold'),(2,'bob','silver'),(3,'cy',NULL)
INSERT INTO sv_o VALUES (1,1,10.50,'2024-01-01 10:00+00','{a,b}','{"k":1}'),(2,1,20,'2024-01-02 11:00+00','{b}','{"k":2}'),(3,2,5,'2024-02-01 09:00+00','{}','{}')
