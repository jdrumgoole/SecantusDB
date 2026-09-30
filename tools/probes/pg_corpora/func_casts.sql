# reference-version: 15
# typename(expr) is a cast; so is PostgreSQL 15's json(x). And 15's lexer
# refuses a numeric literal followed by identifier characters.
SELECT json('{"a":1}')
SELECT json('[1, 2]')::text
SELECT jsonb('{"b":1,"a":2}')
SELECT inet('1.2.3.4'), uuid('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'), bool('t')
SELECT int8(3) + 1, int4('5') * 2, float8('2.5'), text(12) || 'x'
SELECT date('2020-01-02') + 1
SELECT fc_mood('ok')
SELECT pg_typeof(int8(3)), pg_typeof(jsonb('1'))
SELECT id FROM fc WHERE int8(n) > 2 ORDER BY id
SELECT timestamp('2020-01-02 03:04:05')
SELECT upper('x'), length('abc'), abs(-3)
SELECT 1_000
SELECT 0x10
SELECT 0b101, 0o17
SELECT 1.5e1_0
SELECT 100abc
SELECT 1e10, 1.5, .5, 5., 2e-3
SELECT 'a1_000', "fc".id FROM fc WHERE id = 1
SELECT $$1_000$$, E'it\'s 1x'
CLUSTER fc USING fc_lower
SELECT id, s FROM fc
