# reference-version: 15
# Built-in functions found by tools/probes/pg_builtin_coverage.py: each was
# refused, or answered wrongly, until the batch that added this file.
#
# bytea is a type of its own: these are not the text functions of the name.
SELECT substr('\x0102030405'::bytea, 2), substr('\x0102030405'::bytea, 2, 2), substring('\x0102030405'::bytea, 4, 10)
SELECT substr('\x0102030405'::bytea, 0), substr('\x0102030405'::bytea, -1, 3), substr('\x0102030405'::bytea, 9), substr('\x0102030405'::bytea, 2, 0)
SELECT substr('\x0102030405'::bytea, 2, -1)
SELECT substring('\x0102030405'::bytea from 2 for 3), substring('\x0102030405'::bytea from 3), substring('\x0102030405'::bytea for 2)
SELECT id, substr(b, 2, 3), substring(b, 3) FROM bc_t ORDER BY id
SELECT overlay('\x0102030405'::bytea placing '\xffff'::bytea from 2), overlay('\x0102030405'::bytea placing '\xff'::bytea from 2 for 3), overlay('\x0102'::bytea placing '\xff'::bytea from 9)
SELECT overlay('\x0102030405'::bytea placing '\xff'::bytea from 0)
SELECT overlay('\x0102030405'::bytea placing '\xff'::bytea from 2 for 0)
SELECT md5('\x616263'::bytea), md5('abc'), md5(''::bytea), md5('\x00'::bytea)
SELECT id, md5(b) FROM bc_t ORDER BY id
SELECT btrim('\x0000616200'::bytea, '\x00'::bytea), ltrim('\x0000616200'::bytea, '\x00'::bytea), rtrim('\x0000616200'::bytea, '\x00'::bytea), btrim('\x6162'::bytea, '\x6261'::bytea), btrim('\x6162'::bytea, '\x'::bytea)
SELECT trim('\x00'::bytea from '\x0000616200'::bytea), trim(leading '\x00'::bytea from '\x00616200'::bytea), trim(trailing '\x00'::bytea from '\x00616200'::bytea)
SELECT id, btrim(b, '\x00ff'::bytea) FROM bc_t ORDER BY id
SELECT length('\xc3a9e282ac'::bytea, 'UTF8'), length('\xc3a9e282ac'::bytea, 'LATIN1'), length('\x'::bytea, 'utf-8'), length('\x616263'::bytea, 'SQL_ASCII')
SELECT length('\xc3'::bytea, 'UTF8')
SELECT length('\x61'::bytea, 'nosuch')
SELECT get_bit('\x61'::bytea, 0), get_bit('\x61'::bytea, 1), get_bit('\x61'::bytea, 5), get_bit('\x6180'::bytea, 15)
SELECT get_bit('\x61'::bytea, 8)
SELECT get_bit('\x61'::bytea, -1)
SELECT set_bit('\x61'::bytea, 1, 1), set_bit('\x61'::bytea, 0, 0), set_bit('\x6100'::bytea, 15, 1)
SELECT set_bit('\x61'::bytea, 1, 2)
SELECT set_bit('\x61'::bytea, 9, 1)
SELECT bit_count('\x616263'::bytea), bit_count('\x'::bytea), bit_count('\xffff'::bytea)
SELECT id, bit_count(b), get_bit(b, 8) FROM bc_t WHERE id IN (1, 4) ORDER BY id
SELECT pg_typeof(substr('\x01'::bytea, 1)), pg_typeof(md5('\x01'::bytea)), pg_typeof(bit_count('\x01'::bytea)), pg_typeof(get_bit('\x01'::bytea, 1)), pg_typeof(btrim('\x01'::bytea, '\x01'::bytea)), pg_typeof(length('\x01'::bytea, 'UTF8'))
# regexp_match: the first match's groups.
SELECT regexp_match('foobarbequebaz', 'bar.*que'), regexp_match('foobarbequebaz', '(bar)(beque)'), regexp_match('abc', 'x'), regexp_match('ABC', 'b', 'i')
SELECT regexp_match('abc', '(a)|(x)'), regexp_match('', ''), regexp_match(NULL, 'a'), regexp_match('abc', NULL)
SELECT regexp_match('abc', 'b', 'g')
SELECT regexp_match('abc', 'b', 'z')
SELECT (regexp_match('k=v', '(\w+)=(\w+)'))[2], pg_typeof(regexp_match('a', 'a'))
SELECT id, regexp_match(s, '(\w+) (\w+)'), (regexp_match(s, '[a-z]+'))[1] FROM bc_t ORDER BY id
SELECT id FROM bc_t WHERE regexp_match(s, 'World') IS NOT NULL ORDER BY id
# The options every regular-expression function takes are checked.
SELECT regexp_like('abc', 'b', 'z')
SELECT regexp_count('abc', 'b', 1, 'z')
SELECT regexp_substr('abc', 'b', 1, 1, 'z')
SELECT regexp_instr('abc', 'b', 1, 1, 0, 'z')
SELECT regexp_replace('abc', 'b', 'x', 'z')
SELECT regexp_split_to_array('abc', 'b', 'z')
SELECT regexp_matches('abc', 'b', 'z')
SELECT regexp_split_to_table('abc', 'b', 'z')
SELECT regexp_like('abc', 'B', 'i'), regexp_count('abcabc', 'B', 1, 'i'), regexp_replace('abc', 'B', 'x', 'i')
# A number out of range is reported before a bad option.
SELECT regexp_count('abc', 'b', 0, 'z')
SELECT regexp_substr('abc', 'b', 0, 1, 'z')
SELECT regexp_substr('abc', 'b', 1, 0, 'z')
SELECT regexp_substr('abc', 'b', 1, 1, 'z', -1)
SELECT regexp_instr('abc', 'b', 0, 1, 0, 'z')
SELECT regexp_instr('abc', 'b', 1, 0, 0, 'z')
SELECT regexp_instr('abc', 'b', 1, 1, 2, 'z')
SELECT regexp_instr('abc', 'b', 1, 1, 0, 'z', -1)
SELECT regexp_replace('abc', 'b', 'X', 0, 1, 'z')
SELECT regexp_replace('abc', 'b', 'X', 1, -1, 'z')
SELECT regexp_replace('abc', 'b', 'X', 1, 1, 'z')
SELECT regexp_count('abc', 'b', 1, 'g')
SELECT regexp_match('abc', 'b', 'gz')
# regexp_replace from a start position, the Nth match.
SELECT regexp_replace('abcabcabc', 'b', 'X', 1), regexp_replace('abcabcabc', 'b', 'X', 3), regexp_replace('abcabcabc', 'b', 'X', 20)
SELECT regexp_replace('abcabcabc', 'b', 'X', 1, 0), regexp_replace('abcabcabc', 'b', 'X', 1, 2), regexp_replace('abcabcabc', 'b', 'X', 1, 9), regexp_replace('abcabcabc', 'b', 'X', 4, 1)
SELECT regexp_replace('abcabcabc', 'B', 'X', 1, 2, 'i'), regexp_replace('abcabcabc', 'B', 'X', 1, 0, 'i'), regexp_replace('abcabcabc', 'b', 'X', 1, 1, 'g'), regexp_replace('abcabcabc', '(b)(c)', '\2\1', 1, 2)
SELECT regexp_replace('abc', 'b', 'X', 0)
SELECT regexp_replace('abc', 'b', 'X', 1, -1)
SELECT id, regexp_replace(s, 'o', '0', 1, 2), regexp_replace(s, 'l', 'L', 4) FROM bc_t ORDER BY id
# parse_ident
SELECT parse_ident('a.b'), parse_ident('"A b".C'), parse_ident('SomeSchema.someTable'), parse_ident(' x . y '), parse_ident('"a""b"')
SELECT parse_ident('a.b()', false), parse_ident('a b', false), parse_ident('a.b[1]', false)
SELECT parse_ident('a.b()')
SELECT parse_ident('a..b')
SELECT parse_ident('')
SELECT parse_ident('1a')
SELECT parse_ident('"unclosed')
SELECT parse_ident('a.')
SELECT parse_ident('""')
SELECT id, parse_ident(s) FROM bc_t WHERE id = 4
# convert_to / convert
SELECT convert_to('abc', 'UTF8'), convert_to('é', 'LATIN1'), convert_to('é', 'UTF8'), convert_to('', 'utf-8')
SELECT convert_to('€', 'LATIN1')
SELECT convert_to('a', 'nosuch')
SELECT convert('\xe9'::bytea, 'LATIN1', 'UTF8'), convert('\xc3a9'::bytea, 'UTF8', 'LATIN1'), convert('\x616263'::bytea, 'UTF8', 'UTF8')
SELECT convert('\xff'::bytea, 'UTF8', 'LATIN1')
SELECT convert_from(convert_to('héllo', 'LATIN1'), 'LATIN1')
# num_nulls / num_nonnulls
SELECT num_nulls(1, NULL, 2, NULL), num_nonnulls(1, NULL, 2, NULL), num_nulls(NULL), num_nonnulls(1), num_nulls('a', 1, now())
SELECT num_nulls(VARIADIC ARRAY[1, NULL, 3]), num_nonnulls(VARIADIC ARRAY[1, NULL, 3]), num_nulls(VARIADIC NULL::int[]), num_nonnulls(VARIADIC '{}'::int[])
SELECT id, num_nulls(b, s, n), num_nonnulls(b, s, n) FROM bc_t ORDER BY id
SELECT id FROM bc_t WHERE num_nonnulls(b, s) = 2 ORDER BY id
# macaddr
SELECT trunc('08:00:2b:01:02:03'::macaddr), trunc('08:00:2b:01:02:03:04:05'::macaddr8), macaddr8_set7bit('08:00:2b:01:02:03:04:05'::macaddr8), pg_typeof(trunc('08:00:2b:01:02:03'::macaddr))
SELECT id, trunc(m) FROM bc_t ORDER BY id
# multirange(range)
SELECT multirange('[1,5)'::int4range), multirange('empty'::int4range), multirange(numrange(1.5, 2.5)), pg_typeof(multirange('[1,5)'::int4range)), pg_typeof(multirange(daterange('2020-01-01', '2020-02-01')))
SELECT id, multirange(r) FROM bc_t ORDER BY id
# Aggregates. sum(interval) answered 0 and avg(interval) NULL.
SELECT sum(iv), avg(iv), min(iv), max(iv) FROM bc_a
SELECT g, sum(iv), avg(iv), count(iv) FROM bc_a GROUP BY g ORDER BY g
SELECT sum(iv), avg(iv) FROM bc_a WHERE id = 3
SELECT sum(x), avg(x) FROM (VALUES ('1 day'::interval), ('2 days'), ('00:00:01')) v(x)
SELECT avg(x) FROM (VALUES ('1 mon'::interval), ('2 mons'), ('1 day')) v(x)
SELECT bit_and(b), bit_or(b), bit_xor(b) FROM bc_a
SELECT bit_and(n), bit_or(n), bit_xor(n), bit_xor(s2), bit_and(s2), bit_or(s2) FROM bc_a
SELECT g, bit_xor(n), bit_and(b), bit_xor(b) FROM bc_a GROUP BY g ORDER BY g
SELECT bit_xor(x) FROM (VALUES (1), (3), (7)) v(x)
SELECT bit_xor(n) FROM bc_a WHERE id = 3
SELECT bit_and(x) FROM (VALUES (B'101'), (B'11')) v(x)
SELECT string_agg(by, '\x2c'::bytea), string_agg(by, '\x2c'::bytea ORDER BY id DESC), string_agg(by, NULL::bytea) FROM bc_a
SELECT g, string_agg(by, '\x2d2d'::bytea ORDER BY id) FROM bc_a GROUP BY g ORDER BY g
SELECT array_agg(a ORDER BY id) FROM bc_a WHERE a IS NOT NULL
SELECT pg_typeof(sum(iv)), pg_typeof(avg(iv)), pg_typeof(bit_and(b)), pg_typeof(bit_xor(n)), pg_typeof(bit_xor(s2)), pg_typeof(string_agg(by, '\x2c'::bytea)) FROM bc_a
SELECT pg_typeof(array_agg(a)) FROM bc_a WHERE a IS NOT NULL
# smallint and real arithmetic keep their type, and smallint overflows.
SELECT pg_typeof(3::int2 + 3::int2), pg_typeof(3::int2 * 3::int2), pg_typeof(3::int2 + 4), pg_typeof(3::int2 + 4::int8), pg_typeof(3::int2 % 2::int2), pg_typeof(3::int2 << 2), pg_typeof(~(3::int2)), pg_typeof(-(3::int2))
SELECT pg_typeof(-(1.5::float4)), pg_typeof(abs(1.5::float4)), pg_typeof(abs(3::int2)), pg_typeof(mod(3::int2, 2::int2)), pg_typeof(1.5::float4 + 1.5::float4), pg_typeof(1.5::float4 + 1)
SELECT 3::int2 + 3::int2, 3::int2 * 3::int2, 7::int2 / 2::int2, 7::int2 % 2::int2, 3::int2 << 2, ~(3::int2), -(3::int2), abs((-3)::int2), -(1.5::float4), abs((-1.5)::float4)
SELECT 32767::int2 + 1::int2
SELECT 100::int2 * 400::int2
SELECT (-32768)::int2 - 1::int2
SELECT 32767::int2 + 1, 32767::int2 * 2::int4
SELECT id, s2 + s2, pg_typeof(s2 + s2), s2 * 2::int2 FROM bc_a ORDER BY id
# mod and div over numerics keep the fraction.
SELECT mod(7.5, 2), mod(1.5::numeric, 1.5::numeric), mod(10::numeric, 3), mod(-7.5, 2), mod(7, 2.5), mod(7, 2), mod(7::int8, 2), mod(7::int2, 2::int2)
SELECT div(7.5, 2), div(9.9, 0.3), div(10, 3), div(-7.5, 2), div(1.5::numeric, 0.5), pg_typeof(div(10, 3))
SELECT mod(7.5, 0)
SELECT div(7.5, 0)
SELECT id, mod(n, 4), mod(n::numeric, 2.5), div(n::numeric, 2.5) FROM bc_a ORDER BY id
# OVERLAPS
SELECT (date '2020-01-01', date '2020-02-01') OVERLAPS (date '2020-01-15', date '2020-03-01'), (date '2020-01-01', date '2020-01-10') OVERLAPS (date '2020-01-10', date '2020-03-01'), (date '2020-01-01', date '2020-01-01') OVERLAPS (date '2020-01-01', date '2020-01-01')
SELECT (timestamp '2020-01-01 00:00', interval '1 day') OVERLAPS (timestamp '2020-01-01 12:00', interval '1 hour'), (timestamp '2020-01-02', timestamp '2020-01-01') OVERLAPS (timestamp '2020-01-01 12:00', timestamp '2020-01-01 13:00')
SELECT (time '10:00', time '11:00') OVERLAPS (time '10:30', time '12:00'), (time '10:00', interval '30 minutes') OVERLAPS (time '10:30', interval '1 hour'), (time '10:00', time '10:30') OVERLAPS (time '10:30', time '11:00')
SELECT (timestamptz '2020-01-01 00:00+00', timestamptz '2020-01-02 00:00+00') OVERLAPS (timestamptz '2020-01-01 23:00+00', interval '2 hours')
SELECT (date '2020-01-01', NULL) OVERLAPS (date '2020-01-15', date '2020-03-01'), (NULL::date, NULL) OVERLAPS (date '2020-01-15', date '2020-03-01'), (date '2020-01-20', NULL) OVERLAPS (date '2020-01-15', date '2020-03-01'), (date '2020-01-01', date '2020-02-01') OVERLAPS (date '2020-01-15', NULL)
SELECT (date '2020-01-01', interval '1 month') OVERLAPS (date '2020-01-15', date '2020-03-01'), (date '2020-01-01', date '2020-01-05') OVERLAPS (timestamp '2020-01-04 12:00', timestamp '2020-01-06')
SELECT id FROM bc_a WHERE (date '2020-01-01', iv) OVERLAPS (date '2020-01-02', date '2020-01-03') ORDER BY id
# date + time
SELECT date '2020-02-03' + time '04:05:06', time '04:05:06' + date '2020-02-03', pg_typeof(date '2020-02-03' + time '04:05:06')
# A built-in called by the name of the function behind its operator.
SELECT int4pl(1, 2), int4mi(5, 2), int4mul(3, 4), int4div(7, 2), int8pl(1, 2), float8mul(1.5, 2), numeric_add(1.5, 2.5), numeric_sub(1.5, 2.5), int48pl(1, 2::int8)
SELECT int4eq(1, 1), int4ne(1, 1), int4lt(1, 2), int4le(2, 2), int4gt(1, 2), int4ge(2, 2), texteq('a', 'a'), textne('a', 'b'), text_lt('a', 'b'), int48eq(1, 1::int8), booleq(true, false)
SELECT textcat('a', 'b'), textlike('abc', 'a%'), textnlike('abc', 'a%'), texticlike('ABC', 'a%'), like('abc', 'a_c'), notlike('abc', 'a_c'), textregexeq('abc', '^a')
SELECT int4larger(1, 2), int4smaller(1, 2), text_larger('a', 'b'), text_smaller('a', 'b'), int4larger(1, NULL), float8larger(1.5, 2.5), numeric_smaller(1.5, 1.25), date_larger('2020-01-01', '2020-02-01')
SELECT btint4cmp(1, 2), btint4cmp(2, 2), btint4cmp(3, 2), numeric_cmp(1.5, 1.50), btint48cmp(1, 2::int8), btfloat8cmp(2.5, 1.5), btint4cmp(1, NULL)
SELECT textlen('abc'), dexp(1), dlog1(1), dlog10(100), dround(2.5), dtrunc(2.7), dsqrt(16), dcbrt(27), dpow(2, 10), numeric_inc(1.5), int4inc(1), nameconcatoid('abc', 5)
SELECT numeric_exp(1.0), numeric_ln(2.0), numeric_log(2.0, 8.0), numeric_sqrt(2.0), numeric_power(2.0, 3.0), numeric_abs(-1.5), numeric_div_trunc(7.5, 2), int4abs(-3), float8abs(-1.5)
SELECT getpgusername() = current_user
SELECT pg_typeof(int4pl(1, 2)), pg_typeof(int4eq(1, 1)), pg_typeof(textcat('a', 'b')), pg_typeof(textlen('abc')), pg_typeof(int4larger(1, 2)), pg_typeof(btint4cmp(1, 2))
SELECT id, int4pl(n, 1), int4larger(n, 6), textcat(g, '!'), btint4cmp(n, 6) FROM bc_a ORDER BY id
SELECT id FROM bc_a WHERE int4gt(n, 5) ORDER BY id
SELECT int4pl(1, 2) AS named, int4pl(1, 2)
SELECT int8pl(1, 2), pg_typeof(int8pl(1, 2)), float8mul(1.5, 2), pg_typeof(float8mul(1.5, 2)), pg_typeof(float8larger(1.5, 2.5)), date_larger('2020-01-01', '2020-02-01'), pg_typeof(date_larger('2020-01-01', '2020-02-01'))
# bytea LIKE bytea matches bytes; it answered NULL. `\x25` is `%`, `\x5f` is `_`.
SELECT '\x616263'::bytea LIKE '\x616263'::bytea, '\x616263'::bytea LIKE '\x6125'::bytea, '\x616263'::bytea LIKE '\x615f63'::bytea, '\x616263'::bytea LIKE '\x615f'::bytea, '\x616263'::bytea NOT LIKE '\x6125'::bytea, '\x00ff01'::bytea LIKE '\x002501'::bytea
SELECT '\x612563'::bytea LIKE '\x615c2563'::bytea, '\x616263'::bytea LIKE '\x615c2563'::bytea, '\x'::bytea LIKE '\x25'::bytea, '\x'::bytea LIKE '\x'::bytea, NULL::bytea LIKE '\x25'::bytea
SELECT '\x61'::bytea LIKE '\x615c'::bytea
SELECT like('\x6162'::bytea, '\x6125'::bytea), notlike('\x6162'::bytea, '\x6125'::bytea), bytealike('\x6162'::bytea, '\x6125'::bytea), byteanlike('\x6162'::bytea, '\x6125'::bytea)
SELECT id, by LIKE '\x6125'::bytea, by LIKE '\x5f'::bytea FROM bc_a ORDER BY id
SELECT id FROM bc_a WHERE by LIKE '\x25'::bytea ORDER BY id
# to_tsvector over a JSON document indexes its string values only.
SELECT to_tsvector('english', '{"a": "The Fat Rats", "b": "dog", "c": [1, "cats and dogs", {"d": "x y"}], "e": null, "f": true}'::json)
SELECT to_tsvector('english', '{"a": "The Fat Rats", "b": "dog", "c": [1, "cats and dogs", {"d": "x y"}], "e": null, "f": true}'::jsonb)
SELECT to_tsvector('{"b": "dog", "a": "The Fat Rats"}'::jsonb), to_tsvector('"just a string"'::jsonb), to_tsvector('5'::jsonb), to_tsvector('{}'::json), to_tsvector(NULL::jsonb), pg_typeof(to_tsvector('{}'::json))
SELECT to_tsvector('simple', '{"a": "", "b": "x"}'::json), to_tsvector('simple', '["a b", "", "c"]'::jsonb)
SELECT to_tsvector('english', '{"title": "Fat Cats"}'::jsonb) @@ to_tsquery('english', 'cat'), to_tsvector('english', '{"title": "Fat Cats"}'::jsonb) @@ to_tsquery('english', 'title')
# A box's diagonal runs from its upper right corner.
SELECT diagonal('((0,0),(1,1))'::box), lseg('((0,0),(1,1))'::box), diagonal('((3,1),(0,5))'::box), '((3,1),(0,5))'::box::lseg
# Error codes and texts.
SELECT int4range(1, 2, 'abc')
SELECT is_normalized('abc', 'abc')
SELECT pg_size_bytes('abc')
SELECT ts_rank('{1.5,2.5}'::float4[], 'a b'::tsvector, 'a & b'::tsquery)
SELECT json_array_elements('{"a":1}'::json)
SELECT jsonb_array_elements('{"a":1}'::jsonb)
SELECT json_array_elements_text('5'::json)
