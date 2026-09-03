# character(n) is blank-padded on OUTPUT but stripped by every conversion to
# text. The narrow set that sees the padded form (octet_length / concat /
# concat_ws / format / to_json / to_jsonb / ::bytea, and LIKE and friends) is
# MEASURED against PostgreSQL 14.13, not inferred from what looks similar.
SELECT c, v, t FROM c16 ORDER BY id
SELECT c || '|' FROM c16 ORDER BY id
SELECT length(c), octet_length(c), char_length(c) FROM c16 ORDER BY id
SELECT c = 'ab' FROM c16 ORDER BY id
SELECT c::text || '|' FROM c16 ORDER BY id
SELECT upper(c) || '|' FROM c16 ORDER BY id
SELECT c || v || '|' FROM c16 ORDER BY id
SELECT concat(c,'|') FROM c16 ORDER BY id
SELECT c IN ('ab','zz') FROM c16 ORDER BY id
SELECT replace(c,' ','_') FROM c16 ORDER BY id
SELECT to_json(c) FROM c16 ORDER BY id
SELECT c FROM c16 WHERE c = 'ab'
SELECT array_agg(c) FROM c16
SELECT min(c) || '|', max(c) || '|' FROM c16
SELECT c LIKE 'ab' FROM c16 ORDER BY id
SELECT rtrim(c) || '|' FROM c16 ORDER BY id
SELECT substring(c from 1 for 5) || '|' FROM c16 ORDER BY id
SELECT c::char(8) || '|' FROM c16 ORDER BY id
SELECT c::varchar(8) || '|' FROM c16 ORDER BY id
SELECT lpad(c, 7, '.') FROM c16 ORDER BY id
SELECT c || NULL FROM c16 ORDER BY id
SELECT count(DISTINCT c) FROM c16
SELECT c LIKE 'ab' FROM c16 ORDER BY id
SELECT c LIKE 'ab   ' FROM c16 ORDER BY id
SELECT c LIKE 'ab%' FROM c16 ORDER BY id
SELECT c LIKE 'ab_' FROM c16 ORDER BY id
SELECT id FROM c16 WHERE c LIKE 'ab' ORDER BY id
SELECT id FROM c16 WHERE c LIKE 'ab   ' ORDER BY id
SELECT id FROM c16 WHERE c LIKE 'ab%' ORDER BY id
SELECT id FROM c16 WHERE c NOT LIKE 'ab' ORDER BY id
SELECT c ILIKE 'AB' FROM c16 ORDER BY id
SELECT c ILIKE 'AB   ' FROM c16 ORDER BY id
SELECT c ~ '^ab$' FROM c16 ORDER BY id
SELECT c SIMILAR TO 'ab' FROM c16 ORDER BY id
SELECT v LIKE 'ab' FROM c16 ORDER BY id
SELECT t LIKE 'ab' FROM c16 ORDER BY id
SELECT id FROM c16 WHERE v LIKE 'ab' ORDER BY id
SELECT c = 'ab', c = 'ab   ' FROM c16 ORDER BY id
SELECT length(c), char_length(c), octet_length(c), bit_length(c) FROM c16 WHERE id=1
SELECT concat(c,'|'), concat_ws('-',c,'x') FROM c16 WHERE id=1
SELECT upper(c)||'|', lower(c)||'|', initcap(c)||'|' FROM c16 WHERE id=1
SELECT reverse(c)||'|', md5(c) FROM c16 WHERE id=1
SELECT to_json(c)::text, to_jsonb(c)::text FROM c16 WHERE id=1
SELECT c::text||'|', c::varchar||'|', c::char(9)||'|' FROM c16 WHERE id=1
SELECT position('b' in c), strpos(c,'b') FROM c16 WHERE id=1
SELECT split_part(c,' ',1)||'|' FROM c16 WHERE id=1
SELECT left(c,4)||'|', right(c,4)||'|' FROM c16 WHERE id=1
SELECT lpad(c,7,'.'), rpad(c,7,'.') FROM c16 WHERE id=1
SELECT c||''||'|' FROM c16 WHERE id=1
SELECT format('%s|', c) FROM c16 WHERE id=1
SELECT quote_literal(c), quote_ident(c) FROM c16 WHERE id=1
SELECT c::bytea FROM c16 WHERE id=1
SELECT ascii(c), chr(97)||'|' FROM c16 WHERE id=1
SELECT translate(c,' ','_') FROM c16 WHERE id=1
SELECT regexp_replace(c,' ','_') FROM c16 WHERE id=1
SELECT btrim(c)||'|' FROM c16 WHERE id=1
SELECT c > 'ab', c < 'ab   z' FROM c16 WHERE id=1
SELECT array_agg(c)::text FROM c16 WHERE id=1
SELECT row_to_json(x)::text FROM (SELECT c FROM c16 WHERE id=1) x
SELECT c::char(1)||'|' FROM c16 WHERE id=1
# --- non-string -> char(n)
SELECT 123::char(2), 123::char(5) || '|', 1.50::char(6) || '|'
SELECT true::char(2), false::char(2) || '|'
SELECT '2020-01-05'::date::char(4), '2020-01-05'::date::char(20) || '|'
SELECT 123::varchar(2), 1.50::varchar(2)
SELECT NULL::int::char(2)
SELECT 12345678901234::char(3)
SELECT (-5)::char(1)
# --- bpchar -> text conversions strip trailing blanks?
SELECT 'a'::char(3) || '|'
SELECT length('a'::char(3)), length('a'::char(3)::text), char_length('a'::char(3))
SELECT upper('a'::char(3)) || '|', trim('a'::char(3)) || '|'
SELECT ('a'::char(3))::text || '|'
SELECT ('a'::char(3))::varchar || '|'
SELECT concat('a'::char(3), '|')
SELECT 'a'::char(3) = 'a', 'a'::char(3) = 'a  '
SELECT ('a'::char(3) || 'b') = 'ab'
SELECT octet_length('a'::char(3)), octet_length('a'::char(3)::text)
SELECT replace('a'::char(3), ' ', '_')
SELECT 'x' || 'a'::char(3) || 'y'
SELECT substr('a'::char(3), 1, 3) || '|'
SELECT ('a'::char(3))::char(5) || '|'
SELECT to_json('a'::char(3))
