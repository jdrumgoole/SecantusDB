INSERT INTO dx DEFAULT VALUES RETURNING id, n, s
INSERT INTO dx (n) VALUES (5), (6) RETURNING id, n
SELECT count(*), count(DISTINCT u), count(DISTINCT id) FROM dx
SELECT id FROM dx WHERE created > now() - interval '1 minute' AND day = current_date ORDER BY id
INSERT INTO dx (id, n) VALUES (DEFAULT, 7) RETURNING id
SELECT column_name, column_default FROM information_schema.columns WHERE table_name = 'dx' ORDER BY ordinal_position
ALTER TABLE dx ALTER COLUMN n SET DEFAULT currval('dxs')
INSERT INTO dx (id) VALUES (500) RETURNING n
ALTER TABLE dx ADD COLUMN t2 timestamptz DEFAULT now()
UPDATE dx SET n = DEFAULT WHERE id = 500 RETURNING n
UPDATE dx SET n = nextval('dxs') WHERE id < 200 RETURNING n
SELECT current_date = now()::date, localtimestamp IS NOT NULL, current_time IS NOT NULL
ALTER TABLE dx ADD COLUMN u2 uuid DEFAULT gen_random_uuid()
SELECT count(DISTINCT u2) = count(*), count(t2) = count(*) FROM dx
INSERT INTO dx (id) VALUES (900) RETURNING t2 IS NOT NULL, u2 IS NOT NULL
