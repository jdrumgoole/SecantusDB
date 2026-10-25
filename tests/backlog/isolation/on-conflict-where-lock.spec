# INSERT ... ON CONFLICT DO UPDATE ... WHERE locks the conflicting row
# before it tests the WHERE (`ExecOnConflictUpdate`), so a row the WHERE
# rejects is still held: a concurrent UPDATE of it waits for the inserter's
# transaction, then goes on (READ COMMITTED).

setup
{
  CREATE TABLE ocw (key int PRIMARY KEY, val int);
  INSERT INTO ocw VALUES (1, 0);
}

teardown
{
  DROP TABLE ocw;
}

session s1
setup { BEGIN; }
step ins1 { INSERT INTO ocw VALUES (1, 99) ON CONFLICT (key) DO UPDATE SET val = 99 WHERE ocw.val > 100; }
step c1 { COMMIT; }
step a1 { ROLLBACK; }

session s2
step upd2 { UPDATE ocw SET val = val + 1 WHERE key = 1; }
step sel2 { SELECT * FROM ocw; }

permutation ins1 upd2 c1 sel2
permutation ins1 upd2 a1 sel2
