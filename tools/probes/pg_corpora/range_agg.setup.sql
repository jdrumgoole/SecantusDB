DROP TABLE IF EXISTS rga
CREATE TABLE rga (g int, r int4range, m int4multirange)
INSERT INTO rga VALUES (1, '[1,3)', '{[1,2)}'), (1, '[2,5)', '{[4,6)}'), (1, '[8,9)', NULL), (2, 'empty', '{}'), (2, NULL, NULL), (3, NULL, NULL)
