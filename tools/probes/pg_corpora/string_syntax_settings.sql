# reference-version: 15
# standard_conforming_strings = off reads N'...' like a plain literal, and backslash_quote (safe_encoding / on / off, with on/off's hidden spellings) refuses \' in a literal read with backslash escapes: 22P06 at the literal (RP-003). Ends with every setting back at its default.
show backslash_quote
select current_setting('backslash_quote')
set standard_conforming_strings = off
select N'a\\b', pg_typeof(N'a\\b')
select N'a\'b'
select n'x\ty'
select N'it''s', N'', pg_typeof(N'')
select N'a\\b' = 'a\\b'
select length(N'a\\b  ')
select N'ab\'
select 'a\'b', E'a\'b'
set backslash_quote = off
show backslash_quote
select 'a\'b'
select E'a\'b'
select 'a''b', 'a\\''b'
select N'a\'b'
select 'x' || 'a\'b'
select 'a\\b', 'c\'d'
select 'a\\b\'c'
select $$a\'b$$
select "a\'b" from (select 1 as "a\'b") s
select B'01', X'1f'
select 1 /* 'a\'b' */
select 1 -- 'a\'b'
set backslash_quote = safe_encoding
show backslash_quote
select 'a\'b'
set backslash_quote = on
select 'a\'b'
set backslash_quote = true
show backslash_quote
set backslash_quote = yes
show backslash_quote
set backslash_quote = 1
show backslash_quote
set backslash_quote = of
show backslash_quote
set backslash_quote = t
show backslash_quote
set backslash_quote = bogus
set backslash_quote = ''
set backslash_quote = 'SAFE_ENCODING'
show backslash_quote
set backslash_quote = 'Off'
show backslash_quote
show backslash_quote
set backslash_quote = off
reset backslash_quote
show backslash_quote
set standard_conforming_strings = on
set backslash_quote = off
select E'a\'b'
select E'a\\b', E'a''b'
select 'a\'
select U&'d\0061t'
select set_config('backslash_quote', 'off', false)
select E'x\'y'
select set_config('backslash_quote', 'nope', false)
reset backslash_quote
select E'x\'y'
set standard_conforming_strings = off
select 'ab\'
select N'ab\'
select E'ab\'
set backslash_quote = safe_encoding
select 'a\'b'
select E'a''b'
set backslash_quote = on
select 'a\'b'
reset backslash_quote
reset standard_conforming_strings
