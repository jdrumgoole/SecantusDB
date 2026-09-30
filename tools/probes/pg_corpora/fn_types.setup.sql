DROP TABLE IF EXISTS ft_e
DROP TABLE IF EXISTS ft_r
CREATE TABLE ft_e (id int, s text, d date, v varchar(5))
CREATE TABLE ft_r (id int, s text)
INSERT INTO ft_r VALUES (1, 'a')
