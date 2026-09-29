BEGIN
INSERT INTO sq_t (v) VALUES (1) RETURNING id
SELECT nextval('sq_s')
ROLLBACK
INSERT INTO sq_t (v) VALUES (2) RETURNING id
SELECT nextval('sq_s')
BEGIN
SELECT setval('sq_s', 50)
ROLLBACK
SELECT nextval('sq_s')
BEGIN
SELECT nextval('sq_s')
SELECT nextval('sq_s')
COMMIT
BEGIN
CREATE SEQUENCE sq_new
SELECT nextval('sq_new')
SELECT nextval('sq_new')
ROLLBACK
SELECT nextval('sq_new')
