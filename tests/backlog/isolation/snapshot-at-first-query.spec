# A REPEATABLE READ or SERIALIZABLE transaction takes its snapshot at the
# first statement that needs one (`PlannedStmtRequiresSnapshot`): SET,
# SHOW and LOCK need none, so a row committed after them is visible.
# Written for SecantusDB in the isolation-spec format.

setup
{
  CREATE TABLE st (id int);
}

teardown
{
  DROP TABLE st;
}

session s1
setup { BEGIN ISOLATION LEVEL REPEATABLE READ; }
step set1 { SET LOCAL work_mem = '8MB'; }
step show1 { SHOW work_mem; }
step lock1 { LOCK st IN ACCESS SHARE MODE; }
step sel1 { SELECT count(*) FROM st; }
step c1 { COMMIT; }

session s2
step ins2 { INSERT INTO st VALUES (1); }

permutation set1 ins2 sel1 c1
permutation show1 ins2 sel1 c1
permutation lock1 ins2 sel1 c1
permutation sel1 ins2 sel1 c1
