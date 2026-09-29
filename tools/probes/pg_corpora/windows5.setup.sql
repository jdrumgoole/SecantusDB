DROP TABLE IF EXISTS wv5
DROP TABLE IF EXISTS wv5k
CREATE TABLE wv5 (id int PRIMARY KEY, g text, h int, v int)
CREATE TABLE wv5k (g text PRIMARY KEY, label text)
INSERT INTO wv5 VALUES (1,'a',1,10),(2,'a',2,20),(3,'b',1,5),(4,'b',1,NULL),(5,'c',2,40),(6,'c',2,1)
INSERT INTO wv5k VALUES ('a','alpha'),('b','beta'),('c','gamma')
