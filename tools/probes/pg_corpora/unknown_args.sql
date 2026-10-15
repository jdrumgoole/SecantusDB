# Built-ins given untyped string literals: each literal takes the type of the parameter of the overload PostgreSQL 15 resolves to.
SELECT abs('-1'), pg_typeof(abs('-1'))::text
SELECT abs('x')
SELECT abs('')
SELECT sqrt('4'), cbrt('27'), exp('0'), ln('1'), log('100'), log('2', '8')
SELECT ceil('1.5'), floor('-1.5'), sign('-3'), round('2.5'), trunc('2.7')
SELECT pg_typeof(round('2.5'))::text, pg_typeof(trunc('2.7'))::text
SELECT round('2.567', 2), pg_typeof(round('2.567', 2))::text
SELECT power('2', '3'), pg_typeof(power('2', '3'))::text
SELECT mod('7', '2'), pg_typeof(mod('7', '2'))::text
SELECT div('7', '2'), gcd('4', '6'), lcm('4', '6')
SELECT width_bucket('5', 0, 10, 5)
SELECT length('abc'), upper('x'), lower('X'), initcap('ab cd')
SELECT left('abc', 2), right('abc', 2), repeat('a', 3), reverse('abc')
SELECT substr('abcdef', 2), substr('abcdef', 2, 3), strpos('abc', 'b')
SELECT lpad('a', 3), rpad('a', 3, 'x'), btrim('  a  '), ltrim('xxa', 'x')
SELECT md5('a'), ascii('a'), chr('65'), bit_length('abc'), octet_length('abc')
SELECT encode('abc', 'hex'), decode('616263', 'hex'), encode('\x00ff', 'base64')
SELECT replace('abc', 'b', 'x'), split_part('a,b,c', ',', 2), translate('abc', 'ab', 'xy')
SELECT to_hex('255'), pg_typeof(to_hex('255'))::text
SELECT jsonb_typeof('{}'), json_typeof('[1]'), jsonb_array_length('[1,2]')
SELECT date_trunc('day', '2020-01-02 03:04:05'), pg_typeof(date_trunc('day', '2020-01-02 03:04:05'))::text
SELECT age('2020-01-01', '2019-01-01')
SELECT to_timestamp('0'), pg_typeof(to_timestamp('0'))::text
SELECT isfinite('2020-01-01'::date), isfinite('infinity'::timestamp)
SELECT host('10.0.0.1/8'), masklen('10.0.0.0/8'), family('::1')
SELECT abs('1e400')
SELECT sqrt('-1')
SELECT factorial('5')
SELECT num_nonnulls('a', NULL)
SELECT concat('a', 1, NULL), concat_ws('-', 'a', 'b')
SELECT starts_with('abc', 'a'), quote_literal('a''b'), quote_ident('A b')
SELECT to_char('2.5', '9.9')
SELECT int4larger('3', '5'), float8pl('1.5', '2')
