DROP TABLE IF EXISTS r15_t
create table r15_t (a text[], acl aclitem[])
insert into r15_t values (array['x','y'], '{postgres=arwdDxt/postgres}')
