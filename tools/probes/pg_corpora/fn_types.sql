# Built-in functions resolve by type at plan time: an unknown function, or a
# text function given an integer, is 42883 even over an empty table.
SELECT upper(1)
SELECT lower(true)
SELECT upper(NULL), upper('x'), upper('x'::varchar), upper('x'::name)
SELECT upper(id) FROM ft_e
SELECT upper(id) FROM ft_r
SELECT upper(d) FROM ft_e
SELECT upper(v), lower(s) FROM ft_e
SELECT repeat(id, 2) FROM ft_e
SELECT repeat(s, id) FROM ft_r
SELECT replace(s, 'a', id::text) FROM ft_r
SELECT nosuch(id) FROM ft_e
SELECT nosuch(id) FROM ft_r
SELECT s FROM ft_e WHERE upper(id) = 'X'
SELECT length(id) FROM ft_e
SELECT left(s, 1), right(s, 1), lpad(s, 3), reverse(s), initcap(s), ascii(s) FROM ft_r
SELECT strpos(s, 'a'), split_part(s, 'a', 1), starts_with(s, 'a') FROM ft_r
SELECT upper(s || id) FROM ft_r
