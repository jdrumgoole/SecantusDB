# Two REPEATABLE READ transactions insert one key. The second waits for
# the first: when it commits, the second fails 23505; when it rolls back,
# the second's insert goes on. Written for SecantusDB in the
# isolation-spec format.

setup { CREATE TABLE rd (k int PRIMARY KEY); }
teardown { DROP TABLE rd; }

session s1
setup { BEGIN ISOLATION LEVEL REPEATABLE READ; }
step w1 { INSERT INTO rd VALUES (1); }
step c1 { COMMIT; }
step a1 { ROLLBACK; }

session s2
setup { BEGIN ISOLATION LEVEL REPEATABLE READ; }
step w2 { INSERT INTO rd VALUES (1); }
step c2 { COMMIT; }

permutation w1 w2 c1 c2
permutation w1 w2 a1 c2
