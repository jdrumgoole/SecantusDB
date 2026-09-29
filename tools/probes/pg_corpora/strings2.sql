# --- padding: truncates from the LEFT, and an empty fill cannot pad
SELECT lpad('abc',3), lpad('abc',5), lpad('abc',2), lpad('abcdef',3)
SELECT lpad('',3,'x'), lpad('ab',7,'xy'), lpad('abc',0), lpad('abc',-1), lpad('abc',5,'')
SELECT rpad('abc',5), rpad('abc',5,'xy'), rpad('abc',2), rpad('abc',5,'')
SELECT lpad(NULL,3), lpad('a',NULL), lpad('a',3,NULL)
SELECT lpad(t,8,'.'), rpad(t,8,'.') FROM s10 WHERE id=1
# --- to_hex: the WIDTH follows the argument's type, not its value
SELECT to_hex(4294967295), to_hex(0), to_hex(2147483647)
SELECT to_hex(-2), to_hex((-1)::int), to_hex((-1)::bigint), to_hex(255::bigint)
SELECT to_hex(NULL::int)
# --- translate: a short `to` DELETES the extra `from` characters
SELECT translate('abc','abc','xy'), translate('abc','','x'), translate('aabb','ab','xy')
SELECT translate('abcabc','ab',''), translate('','a','b')
SELECT translate(NULL,'a','b'), translate('a',NULL,'b'), translate('a','a',NULL)
# --- overlay
SELECT overlay('abc' placing 'XY' from 1 for 0), overlay('abcdef' placing 'XY' from 2)
SELECT overlay('abc' placing 'XY' from 2 for 2), overlay('abcdef' placing '' from 2 for 3)
SELECT overlay('abc' placing 'XYZ' from 5)
SELECT overlay('abc' placing 'X' from 0)
# --- quoting
SELECT quote_literal(1), quote_literal(NULL), quote_nullable('a'), quote_nullable(NULL)
SELECT quote_literal('a''b'), quote_literal('a\b'), quote_ident('a b')
# --- split_part counts from the END when the field is negative
SELECT split_part('a,b,c',',',-1), split_part('a,b,c',',',-3), split_part('a,b,c',',',-4)
SELECT split_part('abc','',1), split_part('a,b,c',',',9)
SELECT split_part('a,b,c',',',0)
# --- substring from a regex
SELECT substring('abcde' from 'b(c)d'), substring('abc' from '(b)'), substring('abc' from 'b')
SELECT substring('abc' from 'x'), substring('abcde' from '%#"c#"%' for '#')
SELECT substring(t from '(c.)') FROM s10 WHERE id=1
# --- regexp_split_to_array
SELECT regexp_split_to_array('a1b22c','[0-9]+'), regexp_split_to_array('abc','')
# --- unistr
SELECT unistr('d\0061t\+000061'), unistr('\\'), unistr('a\0062')
SELECT unistr('\x')
# --- encoding
SELECT convert_from('\x616263'::bytea,'UTF8'), convert_from('abc'::bytea,'LATIN1')
