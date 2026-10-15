# reference-version: 15
# acldefault, makeaclitem and aclexplode (RP-015), with role oids no role has and with real roles.
SELECT acldefault('r', 99999), acldefault('f', 99999), acldefault('n', 99999), acldefault('s', 99999), acldefault('T', 99999), acldefault('l', 99999), acldefault('d', 99999), acldefault('c', 99999), acldefault('S', 99999), acldefault('p', 99999), acldefault('L', 99999), acldefault('t', 99999), acldefault('F', 99999)
SELECT acldefault('a', 99999)
SELECT makeaclitem(99999, 99998, 'select', false), makeaclitem(0, 99998, 'USAGE', true), makeaclitem(99999, 99998, 'Alter System', false), makeaclitem(99999, 99998, 'temporary', false)
SELECT makeaclitem(99999, 99998, 'SELECT, insert', true)
SELECT makeaclitem(99999, 99998, 'abc', true)
SELECT makeaclitem(null, 1, 'select', true), acldefault(null, 1), pg_typeof(acldefault('r', 1)), pg_typeof(makeaclitem(1, 1, 'select', true))
SELECT grantor::regrole::text, grantee::regrole::text, privilege_type, is_grantable from aclexplode(array[makeaclitem('acl_a'::regrole::oid, 'acl_b'::regrole::oid, 'select', true), makeaclitem(0, 'acl_b'::regrole::oid, 'usage', false)])
SELECT grantee::regrole::text, privilege_type from aclexplode('{acl_a=r*w/acl_b,=X/acl_b}'::aclitem[])
SELECT privilege_type from aclexplode(acldefault('d', 'acl_a'::regrole::oid))
SELECT count(*) from aclexplode('{}'::aclitem[])
SELECT acldefault('r', 'acl_a'::regrole::oid)
SELECT * FROM aclexplode(acldefault('d', 99999))
SELECT * FROM aclexplode(NULL)
DROP ROLE acl_a
DROP ROLE acl_b
