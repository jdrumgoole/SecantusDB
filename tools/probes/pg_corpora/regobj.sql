SELECT 'public'::regnamespace, 'public'::regnamespace::oid, 'pg_catalog'::regnamespace::oid
SELECT 'ro_s'::regnamespace::text
SELECT 'nosuch'::regnamespace
SELECT nspname FROM pg_namespace WHERE nspname IN ('public', 'ro_s', 'pg_catalog') ORDER BY 1
SELECT relname FROM pg_class WHERE relnamespace = 'public'::regnamespace AND relname = 'ro_t'
SELECT 'nosuch_role'::regrole
SELECT 'ro_f'::regproc::text, 'ro_f(integer, text)'::regprocedure::text, 'ro_f(int4, text)'::regprocedure::text
SELECT 'nosuch_fn'::regproc
SELECT proname FROM pg_proc WHERE oid = 'ro_f'::regproc
SELECT proname, pronamespace::regnamespace FROM pg_proc WHERE proname = 'ro_f'
SELECT pg_typeof('public'::regnamespace), pg_typeof('ro_f'::regproc)
SELECT 0::regnamespace, 12345::regproc
