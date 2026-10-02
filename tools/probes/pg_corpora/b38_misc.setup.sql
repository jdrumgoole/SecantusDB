# reference-version: 15
DROP TABLE IF EXISTS b38_dt
DROP TABLE IF EXISTS b38_ns
DROP TABLE IF EXISTS b38_dom
DROP DOMAIN IF EXISTS b38_vb
CREATE TABLE b38_dt (t time, tz timetz)
CREATE TABLE b38_ns (n numeric(3,-2))
