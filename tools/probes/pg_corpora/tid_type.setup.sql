DROP TABLE IF EXISTS tid_t CASCADE
create table tid_t (id int primary key, t tid, ts tid[])
insert into tid_t values (1, '(1,1)', '{"(1,1)","(0,2)"}'), (2, '(0,5)', null), (3, '(2,0)', '{}'), (4, null, '{"(9,9)",NULL}'), (5, '(1,65535)', null)
INSERT INTO tid_t VALUES (7, '(3,3)'::tid, array['(4,4)'::tid])
DROP TABLE IF EXISTS tid_c CASCADE
create table tid_c (t tid, a tid[])
