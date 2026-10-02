DROP TABLE IF EXISTS b40_pk
CREATE TABLE b40_pk (a int NOT NULL, b int, c text)
CREATE UNIQUE INDEX b40_pk_ix ON b40_pk (a) INCLUDE (b, c)
