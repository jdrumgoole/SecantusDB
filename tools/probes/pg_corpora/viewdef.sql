# pg_get_viewdef answers by regclass, oid or name, and NULL for a table.
SELECT pg_get_viewdef('vgd_v'::regclass) = (SELECT definition FROM pg_views WHERE viewname = 'vgd_v')
SELECT pg_get_viewdef('vgd_t'::regclass) IS NULL, pg_get_viewdef(0) IS NULL
SELECT pg_get_viewdef(c.oid) = pg_get_viewdef('vgd_v') FROM pg_class c WHERE c.relname = 'vgd_v'
DROP VIEW vgd_v
DROP TABLE vgd_t
