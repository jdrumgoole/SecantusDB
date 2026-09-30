SELECT '(1,2)'::point, '1 , 2'::point, point(3,4), '[(1,2),(3,4)]'::lseg, '1,2,3,4'::lseg, '{1,-1,0}'::line, '[(0,0),(1,1)]'::line
SELECT '[(1,2),(3,4)]'::path, '((1,2),(3,4))'::path, '(1,2),(3,4)'::path, '1,2,3,4'::path
SELECT '((0,0),(1,0),(1,1))'::polygon, '(0,0),(1,0),(1,1)'::polygon, '<(1,2),3>'::circle, '((1,2),3)'::circle, '1,2,3'::circle, '(1,2),3'::circle
SELECT 'nope'::point
SELECT '{0,0,1}'::line
SELECT '<(1,2),-3>'::circle
SELECT point(1.5, 2.25) <-> point(4,6), area(circle '<(0,0),1>'), length('[(0,0),(3,4)]'::lseg), length('[(0,0),(3,4),(3,0)]'::path), length('((0,0),(3,4),(3,0))'::path)
SELECT area('((0,0),(4,0),(4,3))'::polygon)
SELECT center(box '(2,2),(0,0)'), center(circle '<(1,2),3>'), radius(circle '<(1,2),3>'), diameter(circle '<(1,2),3>'), npoints('((0,0),(1,0),(1,1))'::polygon), npoints('[(0,0),(1,0)]'::path), isclosed('[(0,0),(1,0)]'::path), isopen('[(0,0),(1,0)]'::path), pclose('[(0,0),(1,0)]'::path), popen('((0,0),(1,0))'::path)
SELECT point '(1,2)' + point '(3,4)', point '(1,2)' - point '(3,4)', point '(1,2)' * point '(3,4)', point '(1,2)' / point '(3,4)', box '(1,1),(0,0)' + point '(2,2)', circle '<(0,0),1>' + point '(1,1)'
SELECT polygon '((0,0),(2,0),(2,2),(0,2))' @> point '(1,1)', point '(3,3)' <@ polygon '((0,0),(2,0),(2,2),(0,2))', circle '<(0,0),5>' @> point '(3,4)', box '(2,2),(0,0)' @> point '(1,1)', circle '<(0,0),1>' && circle '<(1,0),1>', point '(1,2)' ~= point '(1,2)', '[(0,0),(1,1)]'::lseg = '[(0,0),(1,1)]'::lseg
SELECT point(box '(2,2),(0,0)'), point(circle '<(1,2),3>'), box(circle '<(0,0),1>'), box(point '(0,0)', point '(1,1)'), circle(point '(0,0)', 2), circle(box '(2,2),(0,0)'), polygon(box '(1,1),(0,0)'), lseg(point '(0,0)', point '(1,1)'), line(point '(0,0)', point '(1,1)'), path(polygon '((0,0),(1,1),(1,0))'), polygon(path '((0,0),(1,1),(1,0))')
SELECT (point '(1,2)')[0], (point '(1,2)')[1], point '(0,0)' <-> circle '<(5,0),1>', point '(0,0)' <-> box '(3,4),(2,1)', '[(0,0),(1,1)]'::lseg ?# '[(0,1),(1,0)]'::lseg, '[(0,0),(1,1)]'::lseg # '[(0,1),(1,0)]'::lseg, @-@ '[(0,0),(3,4)]'::lseg, @@ circle '<(1,2),3>', # '((0,0),(1,0),(1,1))'::polygon
SELECT circle '<(0,0),1>' = circle '<(5,5),1>', circle '<(0,0),1>' < circle '<(0,0),2>', point '(1,2)' << point '(3,2)', point '(1,2)' >^ point '(1,1)', point '(1,1)' ?- point '(5,1)', point '(1,1)' ?| point '(1,5)', '[(0,0),(1,0)]'::lseg ?-| '[(0,0),(0,1)]'::lseg, '[(0,0),(1,0)]'::lseg ?|| '[(0,1),(1,1)]'::lseg
SELECT polygon(4, circle '<(0,0),1>'), '(1e20,2)'::point, '(0.1,0.30000000000000004)'::point
SELECT point(1,2) = point(1,2)
SELECT p, s, l, pa, pg, c, b FROM gt
SELECT p <-> point '(0,0)', c @> p, pg @> point '(1,1)', area(c), length(s), npoints(pg) FROM gt
SELECT id FROM gt WHERE c @> point '(3,3)'
SELECT p[0] + p[1] FROM gt
UPDATE gt SET p = p + point '(10,10)' RETURNING p
SELECT pg_typeof(p), pg_typeof(s), pg_typeof(l), pg_typeof(pa), pg_typeof(pg), pg_typeof(c) FROM gt
SELECT ARRAY['(1,2)'::point, '(3,4)'::point], ARRAY['<(0,0),1>'::circle]
SELECT '((0,0),(1,0))'::path::polygon, '[(0,0),(1,0)]'::path::polygon
SELECT box '(2,2),(0,0)'::point, box '(2,2),(0,0)'::polygon, circle '<(0,0),2>'::box
SELECT polygon '((0,0),(4,0),(4,4),(0,4))' ~= polygon '((4,4),(0,4),(0,0),(4,0))'
SELECT point '(0,0)' <-> '[(1,1),(2,2)]'::path, circle '<(0,0),1>' <-> circle '<(5,0),1>', box '(1,1),(0,0)' <-> box '(5,5),(4,4)'
