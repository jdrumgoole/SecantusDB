DROP TABLE IF EXISTS p4num
create table p4num(id int primary key, f float8, n numeric, r real)
insert into p4num values (1, 0.1, 0.1, 0.1), (2, 1e-300, 1e-300, 1), (3, 4611686018427387904, 4611686018427387904, 2), (4, 3e250, 5, 3)
