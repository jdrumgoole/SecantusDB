DROP TABLE IF EXISTS xt
CREATE TABLE xt (id int PRIMARY KEY, name text, doc xml, n int)
INSERT INTO xt VALUES (1, 'a&b', '<r><i>1</i></r>', 5), (2, 'c', '<r/>', NULL), (3, NULL, NULL, 7)
