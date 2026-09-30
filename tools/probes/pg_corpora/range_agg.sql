# range_agg / range_intersect_agg (PostgreSQL 14), against PostgreSQL 14.
SELECT g, range_agg(r)::text, range_intersect_agg(r)::text FROM rga GROUP BY g ORDER BY g
SELECT range_agg(r)::text FROM (VALUES (int4range(1,3)), (int4range(2,5)), (int4range(8,9))) v(r)
SELECT range_intersect_agg(r)::text FROM (VALUES (int4range(1,10)), (int4range(3,5))) v(r)
SELECT pg_typeof(range_agg(r)), pg_typeof(range_intersect_agg(r)) FROM rga
SELECT range_agg(x) FROM (VALUES (1)) v(x)
SELECT range_agg(r) FROM rga
