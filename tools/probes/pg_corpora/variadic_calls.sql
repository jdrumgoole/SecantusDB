# reference-version: 15
# VARIADIC calls: an ARRAY[...] spread into the variadic parameter, and the json path functions / jsonb_delete as their operators over any array (RP-015).
SELECT jsonb_extract_path('{"a":{"b":1}}', variadic array['a','b'])
SELECT jsonb_extract_path('{"a":{"b":1}}'::jsonb, variadic array['a','b'])
SELECT jsonb_extract_path_text('{"a":{"b":1}}'::jsonb, variadic array['a','b']), json_extract_path('{"a":{"b":1}}'::json, variadic array['a'])
SELECT jsonb_delete('{"a":1,"b":2}'::jsonb, variadic array['a'])
SELECT concat(variadic array['a','b']), format('%s-%s', variadic array['x','y']), num_nulls(variadic array[1, null])
SELECT jsonb_extract_path('{"a":{"b":1}}'::jsonb, variadic null::text[])
SELECT jsonb_delete('{"a":1,"b":2}'::jsonb, 'a'), jsonb_delete('[1,2,3]'::jsonb, 1), jsonb_delete('{"a":1,"b":2,"c":3}'::jsonb, variadic array['a','c'])
SELECT json_extract_path_text('{"a":{"b":"x"}}'::json, variadic array['a','b']), jsonb_extract_path('{"a":1}'::jsonb, 'a')
SELECT jsonb_extract_path_text('{"a":{"b":1}}', variadic array['a','b'])
