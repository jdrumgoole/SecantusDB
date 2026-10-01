DROP TABLE IF EXISTS wn14;
create table wn14 (x float8, n numeric);
insert into wn14 values ('NaN', 'NaN'), ('NaN', 'NaN'), (1, 1), ('-0', 0), (0, '-0');
