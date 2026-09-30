DROP TABLE IF EXISTS dvp
DROP TABLE IF EXISTS dvs
DROP TABLE IF EXISTS dvc
CREATE TABLE dvp (id int primary key, v text)
CREATE TABLE dvs (id serial primary key, v text)
CREATE TABLE dvc (a int, b int, PRIMARY KEY (a, b))
