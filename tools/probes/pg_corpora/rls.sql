ALTER TABLE rl ENABLE ROW LEVEL SECURITY
SELECT relrowsecurity, relforcerowsecurity FROM pg_class WHERE relname = 'rl'
ALTER TABLE rl FORCE ROW LEVEL SECURITY
SELECT relrowsecurity, relforcerowsecurity FROM pg_class WHERE relname = 'rl'
ALTER TABLE rl NO FORCE ROW LEVEL SECURITY
CREATE POLICY p1 ON rl USING (owner = current_user)
CREATE POLICY p1 ON rl USING (true)
CREATE POLICY p2 ON rl FOR INSERT WITH CHECK (id > 0)
CREATE POLICY p3 ON nosuch USING (true)
SELECT policyname, cmd, permissive, roles, qual, with_check FROM pg_policies WHERE tablename = 'rl' ORDER BY 1
ALTER POLICY p2 ON rl RENAME TO p2b
ALTER POLICY p1 ON rl USING (id < 10)
SELECT policyname, cmd, qual, with_check FROM pg_policies WHERE tablename = 'rl' ORDER BY 1
ALTER POLICY nosuch ON rl USING (true)
DROP POLICY nosuch ON rl
DROP POLICY IF EXISTS nosuch ON rl
DROP POLICY p1 ON rl
DROP POLICY p2b ON rl
SELECT count(*) FROM pg_policies WHERE tablename = 'rl'
ALTER TABLE rl DISABLE ROW LEVEL SECURITY
SELECT relrowsecurity, relforcerowsecurity FROM pg_class WHERE relname = 'rl'
