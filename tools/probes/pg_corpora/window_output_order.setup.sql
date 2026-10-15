DROP TABLE IF EXISTS wo_t CASCADE
DROP TABLE IF EXISTS wo_big CASCADE
CREATE TABLE wo_t (id int, g int, v int, s text)
INSERT INTO wo_t VALUES (1,1,5,'b'),(2,2,3,'a'),(3,1,NULL,'c'),(4,2,3,'b'),(5,1,1,NULL),(6,3,7,'a'),(7,2,NULL,'c'),(8,3,2,'b')
CREATE TABLE wo_big (id int, g int, v int)
INSERT INTO wo_big SELECT i, i % 4, (i * 7) % 5 FROM generate_series(1, 60) i
