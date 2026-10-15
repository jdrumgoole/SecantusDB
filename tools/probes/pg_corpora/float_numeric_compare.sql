# reference-version: 15
# float8 against numeric, either way round, compares as float8 = float8 (and float4 against numeric as float4 = float8): =, <>, <, IN, ANY / ALL, BETWEEN, column against column (RP-040).
select id from p4num where f = 0.1 order by id
select id from p4num where f = n order by id
select id from p4num where f = 0.1::numeric order by id
select id from p4num where n = 0.1::float8 order by id
select id from p4num where f < 1 order by id
select id from p4num order by f
select id from p4num where f > 1e200 order by id
select id from p4num where n < 0.1000000000000001 order by id
select 0.1::float8 = 0.1::numeric
select id from p4num where r = 0.1 order by id
select id from p4num where n = 0.1 order by id
select id from p4num where n <> 0.1::float8 order by id
select id from p4num where f in (0.1, 5) order by id
select id from p4num where f = any(array[0.1]) order by id
select id from p4num where n > f order by id
select id from p4num where f between 0.05 and 0.2 order by id
select id from p4num where r = 0.10000000149011612 order by id
select id from p4num where r < 0.5 order by id
select id from p4num where r = 1 order by id
select id from p4num where n = any(array[0.1::float8]) order by id
select id from p4num where f <> all(array[0.1, 5]) order by id
select id from p4num where r = any('{0.1}') order by id
