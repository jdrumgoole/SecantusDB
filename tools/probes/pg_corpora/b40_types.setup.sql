DROP TABLE IF EXISTS b40_mac
CREATE TABLE b40_mac (m macaddr, m8 macaddr8, l pg_lsn)
INSERT INTO b40_mac VALUES ('08:00:2b:01:02:03', '08:00:2b:01:02:03:04:05', '16/B374D848'), ('01-00-00-00-00-00', '0100.0000.0000.0000', 'A/0'), (NULL, NULL, '1/FFFFFFFF')
DROP TABLE IF EXISTS b40_lk
CREATE TABLE b40_lk (t text)
INSERT INTO b40_lk VALUES ('a'), ('b'), ('ab')
DROP TABLE IF EXISTS b40_ct
DROP TYPE IF EXISTS b40_custom
DROP TYPE IF EXISTS _b40_custom
CREATE TYPE b40_custom AS (i int)
CREATE TYPE _b40_custom AS (f float8)
CREATE TABLE b40_ct (c1 b40_custom, c2 _b40_custom, c3 b40_custom[], c4 _b40_custom[])
DROP TYPE IF EXISTS b40_flag
CREATE TYPE b40_flag AS ENUM ('duplicate', 'new', 'spike')
