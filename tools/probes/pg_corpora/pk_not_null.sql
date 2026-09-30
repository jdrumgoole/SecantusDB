# A PRIMARY KEY column is NOT NULL -- the single-column key too.
INSERT INTO dvp DEFAULT VALUES
INSERT INTO dvp (v) VALUES ('x')
INSERT INTO dvp VALUES (NULL, 'y')
INSERT INTO dvp VALUES (1, 'y')
UPDATE dvp SET id = NULL
INSERT INTO dvs (v) VALUES ('a')
INSERT INTO dvc (a) VALUES (1)
SELECT * FROM dvp
SELECT * FROM dvs
SELECT count(*) FROM dvc
