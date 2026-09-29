DROP TABLE IF EXISTS ei_t
CREATE TABLE ei_t (id int PRIMARY KEY, email text, a int, b int)
INSERT INTO ei_t VALUES (1, 'A@x.com', 1, 2), (2, 'b@x.com', 3, 4)
