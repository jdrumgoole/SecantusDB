DROP TABLE IF EXISTS ts2
CREATE TABLE ts2 (id int PRIMARY KEY, body text, tv tsvector)
INSERT INTO ts2 VALUES (1, 'The cat sat on the mat', to_tsvector('english', 'The cat sat on the mat')), (2, 'A dog chased the cat', to_tsvector('english', 'A dog chased the cat')), (3, 'Mice fear cats and dogs', to_tsvector('english', 'Mice fear cats and dogs')), (4, NULL, NULL)
