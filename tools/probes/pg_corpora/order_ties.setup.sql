DROP TABLE IF EXISTS ot_t, ot_u, ot_w CASCADE
create table ot_t (id int, v int, s text)
insert into ot_t values (1,2,'b'),(2,null,'a'),(3,1,'c'),(4,1,'a'),(5,1,'b'),(6,3,'c'),(7,1,'a'),(8,2,'c'),(9,1,'b'),(10,1,'a')
create table ot_u (id int, g int, h int)
insert into ot_u select i, i % 3, i % 5 from generate_series(1, 60) i
create table ot_w (id int, k int)
insert into ot_w select i, (i * 7) % 4 from generate_series(1, 200) i
