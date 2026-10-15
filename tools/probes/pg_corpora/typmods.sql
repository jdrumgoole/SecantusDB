# reference-version: 15
# Type modifiers: time / timetz / interval precision and interval fields are applied (casts and columns), the length-coercion functions by name, format_type, information_schema.columns' 44 columns, and identity sequence options.
SELECT '04:05:06.789'::time(1)::text, '04:05:06.789+00'::timetz(2)::text, '04:05:06.25'::time(1)::text, '23:59:59.99'::time(1)::text, '23:59:59.96+05:30'::timetz(1)::text, '04:05:06.123456'::time(3)::text
SELECT cast('04:05:06.789' AS time(0))::text, '04:05:06.5'::time(0)::text
SELECT '1 day 02:03:04.5678'::interval(2)::text, '1 day 02:03:04.5678'::interval second(1)::text, '-1.55 seconds'::interval(1)::text, '1 year 2 months 3 days 04:05:06.7'::interval day to second(0)::text
SELECT '1 year 2 months 3 days 04:05:06.7'::interval year to month::text, '1 year 2 months 3 days 04:05:06.7'::interval hour::text, '3 days 04:05:06'::interval minute::text, '-3 days -04:05:06.7'::interval hour::text, '1 year 13 months'::interval year::text
INSERT INTO tm_t (a, b, c, z) VALUES ('1.26 seconds', '1 day 02:03:04', '04:05:06.789', 1) RETURNING a::text, b::text, c::text
UPDATE tm_t SET a = '2.25 seconds', b = '3 days 01:02:03', c = '01:02:03.456' RETURNING a::text, b::text, c::text
SELECT pg_catalog."bit"(2::int4, 2::int4), pg_catalog."bit"(B'101', 2::int4, true), pg_catalog."bit"(5::int8, 2::int4)
SELECT pg_catalog."bpchar"('abc'::bpchar, 2::int4, true), pg_catalog."bpchar"('abc'::bpchar, 6, true), pg_catalog."bpchar"('abcdef'::bpchar, 6, true)
SELECT pg_catalog."numeric"(1.5::numeric, 2::int4), pg_catalog."numeric"(1.2345, 458758), pg_catalog."numeric"(12345.678, 327678)
SELECT pg_catalog."numeric"(99.5, 131078)
SELECT pg_catalog."time"('04:05:06.789'::time, 1)::text, pg_catalog."timestamp"('2020-02-03 04:05:06.789'::timestamp, 0)::text
SELECT pg_catalog."varbit"(B'101'::varbit, 2::int4, true), pg_catalog."varchar"('abc'::varchar, 2::int4, true), pg_catalog."varchar"('abcdef'::varchar, 7, true)
SELECT pg_catalog."interval"('1 day 02:03:04.5678'::interval, 2147418114)::text, pg_catalog."interval"('1 day 02:03:04.5678'::interval, 470286338)::text
SELECT pg_typeof(pg_catalog."numeric"(1.5, 458758)), pg_typeof(pg_catalog."varchar"('a'::varchar, 7, true))
SELECT format_type('timestamp'::regtype, 3), format_type('timestamptz'::regtype, 0), format_type('time'::regtype, 1), format_type('timetz'::regtype, 6), format_type('bit'::regtype, 1), format_type('varbit'::regtype, 5)
SELECT format_type('interval'::regtype, (32767 << 16) | 1), format_type('interval'::regtype, (3080 << 16) | 65535), format_type('interval'::regtype, (4096 << 16) | 2), format_type('interval'::regtype, (6 << 16) | 65535)
SELECT format_type('numeric'::regtype, (5 << 16 | (-2 & 2047)) + 4), format_type('bpchar'::regtype, 5), format_type('timestamp'::regtype, -1), format_type('_timestamp'::regtype, 2), format_type('_interval'::regtype, (8 << 16) | 65535), format_type('bit'::regtype, 0)
SELECT format_type(1007, -1), format_type(1007, 3), format_type(25, NULL), format_type(NULL, 1), format_type(16, 7), format_type(1043, 10), '_int4'::regtype, '_text'::regtype::oid
SELECT attname, format_type(atttypid, atttypmod) FROM pg_attribute WHERE attrelid = 'tm_t'::regclass AND attnum > 0 ORDER BY attnum
SELECT table_schema, table_name, column_name, ordinal_position, column_default, is_nullable, data_type, character_maximum_length, character_octet_length, numeric_precision, numeric_precision_radix, numeric_scale, datetime_precision, interval_type, interval_precision, character_set_name, collation_schema, collation_name, domain_schema, domain_name, udt_schema, udt_name, scope_name, maximum_cardinality, dtd_identifier, is_self_referencing, is_identity, identity_generation, identity_start, identity_increment, identity_maximum, identity_minimum, identity_cycle, is_generated, generation_expression, is_updatable FROM information_schema.columns WHERE table_name IN ('tm_t', 'tm_id', 'tm_id2') ORDER BY table_name, ordinal_position
SELECT table_schema, table_name, column_name, ordinal_position, column_default, is_nullable, data_type, character_maximum_length, character_octet_length, numeric_precision, numeric_precision_radix, numeric_scale, datetime_precision, interval_type, interval_precision, character_set_name, collation_schema, collation_name, domain_schema, domain_name, udt_schema, udt_name, scope_name, maximum_cardinality, dtd_identifier, is_self_referencing, is_identity, identity_generation, identity_start, identity_increment, identity_maximum, identity_minimum, identity_cycle, is_generated, generation_expression, is_updatable FROM information_schema.columns WHERE table_schema = 'pg_catalog' AND table_name = 'pg_class' AND column_name IN ('oid', 'relname', 'relacl', 'reltuples') ORDER BY ordinal_position
INSERT INTO tm_id (v) VALUES (1), (2) RETURNING id
INSERT INTO tm_id2 (v) VALUES (1), (2), (3) RETURNING id
