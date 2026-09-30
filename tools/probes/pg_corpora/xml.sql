SELECT xmlelement(name foo, 'bar')
SELECT xmlelement(name foo, xmlattributes('a&b<"c' as x, 1 as y, null as z), 'x<y&z', xmlelement(name e))
SELECT xmlelement(name foo)
SELECT xmlelement(name "Foo Bar", 1, true, 2.5, '2020-01-02'::date, '2020-01-02 03:04:05'::timestamp, 'ab'::bytea)
SELECT xmlelement(name a, '2020-01-02 03:04:05+02'::timestamptz)
SELECT xmlforest('x' as a, null as b, 3 as c)
SELECT xmlconcat('<a/>', null, '<b>x</b>')
SELECT xmlconcat(null, null) IS NULL
SELECT xmlcomment('hello')
SELECT xmlcomment('a--b')
SELECT xmlpi(name php, 'echo 1;')
SELECT xmlpi(name xml, 'x')
SELECT xmlpi(name foo)
SELECT xmlpi(name foo, '  bar')
SELECT xmlroot('<a/>', version '1.0', standalone yes)
SELECT xmlroot('<?xml version="1.1"?><a/>', version no value)
SELECT xmlroot('<a/>', version '1.1'), xmlroot('<a/>', version '1.0')
SELECT xmlparse(document '<a><b/></a>')
SELECT xmlparse(content 'abc<b/>')
SELECT xmlparse(document 'abc<b/>')
SELECT '<a>'::xml
SELECT '<a b="1" b="2"/>'::xml
SELECT '<a><b></a></b>'::xml
SELECT '<a>&amp;</a>'::xml, '<!DOCTYPE a><a/>'::xml
SELECT '<a>&foo;</a>'::xml
SELECT xmlserialize(content '<a>x</a>'::xml as text)
SELECT xmlserialize(document 'x<a/>'::xml as text)
SELECT '<a/>'::xml IS DOCUMENT, 'a<b/>'::xml IS DOCUMENT
SELECT xml_is_well_formed('<a>'), xml_is_well_formed('<a/>'), xml_is_well_formed_document('x<a/>'), xml_is_well_formed_content('x<a/>')
SELECT pg_typeof(xmlelement(name a)), '<a/>'::xml::text
SELECT xmlelement(name a, '<b/>'::xml)
SELECT xmlelement(name a, xmlattributes('x' as "b c"))
SELECT xmlelement(name a, xmlattributes(1 as b, 2 as b))
SELECT xmlelement(name a, xmlattributes(1))
SELECT xmlforest(1)
SELECT xmlelement(name a, array[1,2]), xmlelement(name a, 'x'::text, null, 'y')
SELECT xmlelement(name "xml-x"), xmlelement(name "_xa"), xmlelement(name ":a"), xmlelement(name "a:b"), xmlelement(name "1a")
SELECT xmlelement(name a, xmlattributes(E'x>y\tz\nw\rq''s' as b), E'x>y\tz\nw\rq''s"')
SELECT xmlconcat('<?xml version="1.1"?><a/>', '<?xml version="1.1" standalone="no"?><b/>')
SELECT xmlconcat('<?xml version="1.0" standalone="yes"?><a/>', '<?xml version="1.0" standalone="yes"?><b/>')
SELECT xmlelement(name row, xmlforest(id, name)) FROM xt ORDER BY id
SELECT xmlelement(name r, xmlattributes(id, name AS nm), doc) FROM xt ORDER BY id
SELECT xmlagg(xmlelement(name i, id) ORDER BY id DESC) FROM xt
SELECT xmlagg(doc) FROM xt
SELECT n, xmlagg(xmlelement(name x, id)) FROM xt GROUP BY n ORDER BY n
SELECT doc IS DOCUMENT FROM xt ORDER BY id
SELECT id FROM xt WHERE xml_is_well_formed_document(doc::text) ORDER BY id
INSERT INTO xt VALUES (4, 'd', '<bad', 1)
SELECT xmlelement(name a, '')
SELECT xmlforest(null as a) IS NULL
SELECT xmlpi(name foo, null) IS NULL, xmlroot(null, version '1.0') IS NULL
SELECT xmlelement(name a, 1.5::float4, 1e20::float8, 'infinity'::float8)
SELECT '<a/>'::xml = '<a/>'::xml
SELECT xmlelement(name a, '2020-01-02 03:04:05.25'::timestamp)
SELECT xmlparse(content '<?xml version="1.0"?><a/>')
SELECT '<?xml version="1.0"?>x<a/>'::xml
SELECT xmlelement(name "e", xmlelement(name "f", xmlattributes(true as t)))
SELECT xpath('/a/b/text()', '<a><b>x</b><b>y</b></a>')
SELECT xpath('/a/b', '<a><b>x</b><b/></a>')
SELECT xpath_exists('/a/c', '<a><b/></a>'), xmlexists('//b' passing '<a><b/></a>')
SELECT xpath('count(/a/b)', '<a><b/><b/></a>'), xpath('/a/@x', '<a x="1"/>')
SELECT xpath('//n:b/text()', '<a xmlns:n="u"><n:b>q</n:b></a>', array[array['n','u']])
SELECT xpath('/a/text()', '<a>&amp;&lt;</a>')
SELECT xpath('//b', '<a xmlns="urn:x"><b k="v">1</b></a>')
SELECT xpath('/a/b[2]', '<a><b>1</b><b><c>2</c></b></a>')
SELECT xpath('string(/a)', '<a>hi</a>'), xpath('1 = 1', '<a/>'), xpath('1 div 2', '<a/>')
SELECT xpath('', '<a/>')
SELECT xpath('/a', 'not xml')
SELECT xpath('/a[', '<a/>')
SELECT xpath('/a/comment()', '<a><!--c--></a>')
SELECT id, xpath('/r/i/text()', doc) FROM xt ORDER BY id
SELECT id FROM xt WHERE xmlexists('/r/i' PASSING BY REF doc) ORDER BY id
SELECT (xpath('/a/b/text()', '<a><b>x</b></a>'))[1]::text
SELECT pg_typeof(xpath('/a', '<a/>'))
