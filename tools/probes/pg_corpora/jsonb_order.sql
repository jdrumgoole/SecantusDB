# reference-version: 15
# jsonb orders as PostgreSQL's btree order (compareJsonbContainers), not as
# its text: null < string < number < boolean < array < object, element and
# pair counts before contents, an empty array below every scalar.
SELECT string_agg(j::text, ' | ' ORDER BY j) FROM (VALUES ('{}'::jsonb), ('[]'), ('null'), ('true'), ('false'), ('1'), ('2.5'), ('"a"'), ('"b"'), ('[1]'), ('[0,0]'), ('{"a":1}'), ('{"b":0}'), ('{"a":1,"b":2}'), ('["a"]'), ('[null]'), ('{"aa":1}'), ('[[1]]'), ('[{}]'), ('1.0'), ('-3'), ('""')) v(j)
SELECT string_agg(j::text, ' | ') FROM (SELECT j FROM (VALUES ('{}'::jsonb), ('[]'), ('null'), ('true'), ('false'), ('1'), ('2.5'), ('"a"'), ('"b"'), ('[1]'), ('[0,0]'), ('{"a":1}'), ('{"b":0}'), ('{"a":1,"b":2}'), ('["a"]'), ('[null]'), ('{"aa":1}'), ('[[1]]'), ('[{}]'), ('1.0'), ('-3'), ('""')) v(j) ORDER BY j DESC) s
SELECT string_agg(j::text, ' | ' ORDER BY r) FROM (SELECT j, row_number() OVER (ORDER BY j) r FROM (VALUES ('{}'::jsonb), ('[]'), ('null'), ('true'), ('false'), ('1'), ('2.5'), ('"a"'), ('"b"'), ('[1]'), ('[0,0]'), ('{"a":1}'), ('{"b":0}'), ('{"a":1,"b":2}'), ('["a"]'), ('[null]'), ('{"aa":1}'), ('[[1]]'), ('[{}]'), ('1.0'), ('-3'), ('""')) v(j)) s
SELECT count(*) FROM (VALUES ('{}'::jsonb), ('[]'), ('null'), ('true'), ('false'), ('1'), ('2.5'), ('"a"'), ('"b"'), ('[1]'), ('[0,0]'), ('{"a":1}'), ('{"b":0}'), ('{"a":1,"b":2}'), ('["a"]'), ('[null]'), ('{"aa":1}'), ('[[1]]'), ('[{}]'), ('1.0'), ('-3'), ('""')) v(j) WHERE j > '[1]'
SELECT count(*) FROM (VALUES ('{}'::jsonb), ('[]'), ('null'), ('true'), ('false'), ('1'), ('2.5'), ('"a"'), ('"b"'), ('[1]'), ('[0,0]'), ('{"a":1}'), ('{"b":0}'), ('{"a":1,"b":2}'), ('["a"]'), ('[null]'), ('{"aa":1}'), ('[[1]]'), ('[{}]'), ('1.0'), ('-3'), ('""')) v(j) WHERE j <= '"b"'
SELECT '[1]'::jsonb < '[0,0]'::jsonb, '"a"'::jsonb >= '1'::jsonb, '{}'::jsonb > '[1,2]'::jsonb, '[]'::jsonb < 'null'::jsonb
SELECT '{"b":1}'::jsonb > '{"aa":1}'::jsonb, '{"a":1,"a":2}'::jsonb > '{"a":1}'::jsonb, '1.0'::jsonb <= '1'::jsonb
SELECT jsonb_cmp('[1]', '{}'), jsonb_cmp('2', '1.0'), jsonb_cmp('[]', 'null'), jsonb_cmp('{"a":[1,2]}', '{"a":[1,3]}')
