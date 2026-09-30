# View privileges are the view's; its base tables are the owner's.
# A subquery anywhere in the statement is checked with it.
SET ROLE pu
SELECT id FROM pvv ORDER BY id
SELECT id FROM pbase
SELECT (SELECT count(*) FROM pother)
SELECT id FROM pvv WHERE EXISTS (SELECT 1 FROM pother o WHERE o.id = pvv.id)
SELECT id FROM pvv WHERE id IN (SELECT id FROM pother)
SELECT id, (SELECT count(*) FROM pother o WHERE o.id = pvv.id) FROM pvv ORDER BY id
UPDATE pvv SET v = 'x'
RESET ROLE
GRANT UPDATE ON pvv TO pu
SET ROLE pu
UPDATE pvv SET v = 'x' WHERE id = 1
SELECT v FROM pvv ORDER BY id
RESET ROLE
SET ROLE pu
SELECT id FROM pvv WHERE v IN (SELECT v FROM pvv) ORDER BY id
WITH c AS (SELECT id FROM pvv) SELECT count(*) FROM c
WITH pother AS (SELECT 1 AS id) SELECT id FROM pother
RESET ROLE
