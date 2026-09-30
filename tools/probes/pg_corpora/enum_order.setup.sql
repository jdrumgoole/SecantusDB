DROP TABLE IF EXISTS eo_t
DROP TYPE IF EXISTS eo_m CASCADE
CREATE TYPE eo_m AS ENUM ('zeta', 'alpha', 'mid')
CREATE TABLE eo_t (id int, x eo_m)
INSERT INTO eo_t VALUES (1, 'alpha'), (2, 'zeta'), (3, 'mid')
