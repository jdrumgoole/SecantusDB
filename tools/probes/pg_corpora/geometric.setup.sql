DROP TABLE IF EXISTS gt
CREATE TABLE gt (id int PRIMARY KEY, p point, s lseg, l line, pa path, pg polygon, c circle, b box)
INSERT INTO gt VALUES (1, '(1,2)', '[(0,0),(3,4)]', '{1,-1,0}', '[(0,0),(3,4),(3,0)]', '((0,0),(4,0),(4,3))', '<(0,0),5>', '(2,2),(0,0)')
