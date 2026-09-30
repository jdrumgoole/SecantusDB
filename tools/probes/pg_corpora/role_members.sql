SELECT r.rolname, m.rolname, am.admin_option FROM pg_auth_members am JOIN pg_roles r ON r.oid = am.roleid JOIN pg_roles m ON m.oid = am.member WHERE r.rolname LIKE 'rm_%' ORDER BY 1, 2
SELECT pg_has_role('rm_b', 'rm_a', 'member'), pg_has_role('rm_b', 'rm_a', 'usage'), pg_has_role('rm_b', 'rm_b', 'member')
SELECT pg_has_role('rm_e', 'rm_a', 'member'), pg_has_role('rm_b', 'rm_c', 'member')
SELECT pg_has_role('rm_b', 'rm_d', 'member with admin option'), pg_has_role('rm_b', 'rm_a', 'MEMBER WITH ADMIN OPTION')
SELECT pg_has_role('rm_a', 'member')
SELECT pg_has_role('nosuch', 'rm_a', 'member')
SELECT pg_has_role('rm_b', 'rm_a', 'bogus')
GRANT rm_a TO rm_e
SELECT pg_has_role('rm_e', 'rm_a', 'member')
REVOKE rm_a FROM rm_e
SELECT pg_has_role('rm_e', 'rm_a', 'member')
