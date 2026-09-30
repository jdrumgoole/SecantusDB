DROP TABLE IF EXISTS fc
DROP TYPE IF EXISTS fc_mood
CREATE TYPE fc_mood AS ENUM ('sad', 'ok')
CREATE TABLE fc (id int, s text, n int)
INSERT INTO fc VALUES (3, 'B', 1), (1, 'c', 2), (2, 'a', 3), (4, NULL, 4)
CREATE INDEX fc_lower ON fc (lower(s))
