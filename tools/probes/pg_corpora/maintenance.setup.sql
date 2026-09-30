ROLLBACK
DROP TABLE IF EXISTS mt
CREATE TABLE mt (id int primary key, v text)
INSERT INTO mt VALUES (3,'c'),(1,'a'),(2,'b')
CREATE INDEX mt_v ON mt (v DESC)
