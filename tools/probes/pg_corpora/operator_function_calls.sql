# reference-version: 15
# An operator's function called by name (int4pl, texteq, tideq) over columns and literals: resolved, or 42883 naming the function (RP-038).
select int4pl(a, b) from ac_t
select int4pl(b, b) from ac_t
select int4pl(b, '2') from ac_t
select tideq(t, '(1,1)') from ac_t
select tideq(t, b) from ac_t
select texteq(a, 'x') from ac_t
select int4pl(a, b) from (values ('x'::text, 1)) v(a, b)
select int4pl(b, 2) from ac_t
select texteq(a, b) from ac_t
select int4pl(b, b) + 1 from ac_t where int4lt(b, 3)
select id from (values (1)) v(id) where int4pl(id, 'x'::text) = 1
