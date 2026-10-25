# Two transactions insert one value of a UNIQUE column (not the primary
# key). The second waits for the first: when it commits, the second fails
# 23505; when it rolls back, the second's insert goes on. At READ COMMITTED
# and at REPEATABLE READ. Written for SecantusDB in the isolation-spec
# format.

setup { CREATE TABLE ucw (id serial PRIMARY KEY, k int UNIQUE); }
teardown { DROP TABLE ucw; }

session s1
setup { BEGIN ISOLATION LEVEL REPEATABLE READ; }
step w1 { INSERT INTO ucw (k) VALUES (1); }
step c1 { COMMIT; }
step a1 { ROLLBACK; }

session s2
setup { BEGIN ISOLATION LEVEL REPEATABLE READ; }
step w2 { INSERT INTO ucw (k) VALUES (1); }
step c2 { COMMIT; }

session s3
setup { BEGIN; }
step w3 { INSERT INTO ucw (k) VALUES (1); }
step c3 { COMMIT; }

permutation w1 w2 c1 c2
permutation w1 w2 a1 c2
permutation w1 w3 c1 c3
permutation w1 w3 a1 c3
