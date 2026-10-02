# reference-version: 15
# The catalog reads pgjdbc's DatabaseMetaData makes: a foreign key's
# conindid (the referenced key's index), a nine-relation comma join hashed on
# its WHERE equalities, expression and partial indexes in pg_index /
# pg_get_indexdef, ADD PRIMARY KEY USING INDEX, a domain's COMMENT through
# obj_description, a schema's composite type's namespace, a pseudo-type
# return, and a PRIMARY KEY named by ADD CONSTRAINT.
SELECT c.conname, c.contype, ic.relname FROM pg_constraint c LEFT JOIN pg_class ic ON ic.oid = c.conindid WHERE c.conrelid IN ('b39_pk'::regclass, 'b39_fk'::regclass) ORDER BY 1
SELECT pkc.relname, pka.attname, fkc.relname, fka.attname, pos.n, con.conname, pkic.relname FROM pg_catalog.pg_namespace pkn, pg_catalog.pg_class pkc, pg_catalog.pg_attribute pka, pg_catalog.pg_namespace fkn, pg_catalog.pg_class fkc, pg_catalog.pg_attribute fka, pg_catalog.pg_constraint con, pg_catalog.generate_series(1, 32) pos(n), pg_catalog.pg_class pkic WHERE pkn.oid = pkc.relnamespace AND pkc.oid = pka.attrelid AND pka.attnum = con.confkey[pos.n] AND con.confrelid = pkc.oid AND fkn.oid = fkc.relnamespace AND fkc.oid = fka.attrelid AND fka.attnum = con.conkey[pos.n] AND con.conrelid = fkc.oid AND con.contype = 'f' AND (pkic.relkind = 'i' OR pkic.relkind = 'I') AND pkic.oid = con.conindid AND fkc.relname = 'b39_fk' ORDER BY pkn.nspname, pkc.relname, con.conname, pos.n
SELECT c.relname, i.indkey, i.indnatts, i.indnkeyatts, i.indexprs IS NOT NULL, pg_get_indexdef(i.indexrelid, 1, false), pg_get_indexdef(i.indexrelid, 2, false), pg_get_expr(i.indpred, i.indrelid) FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid WHERE i.indrelid = 'b39_ix'::regclass ORDER BY 1
SELECT tmp.INDEX_NAME, tmp.ORDINAL_POSITION, trim(both '"' from pg_catalog.pg_get_indexdef(tmp.CI_OID, tmp.ORDINAL_POSITION, false)) AS "COLUMN_NAME", tmp.FILTER_CONDITION FROM (SELECT ci.relname AS INDEX_NAME, (information_schema._pg_expandarray(i.indkey)).n AS ORDINAL_POSITION, pg_catalog.pg_get_expr(i.indpred, i.indrelid) AS FILTER_CONDITION, ci.oid AS CI_OID, NOT i.indisunique AS NON_UNIQUE FROM pg_catalog.pg_class ct JOIN pg_catalog.pg_namespace n ON (ct.relnamespace = n.oid) JOIN pg_catalog.pg_index i ON (ct.oid = i.indrelid) JOIN pg_catalog.pg_class ci ON (ci.oid = i.indexrelid) WHERE ct.relname = 'b39_ix') AS tmp ORDER BY tmp.NON_UNIQUE, tmp.INDEX_NAME, tmp.ORDINAL_POSITION
ALTER TABLE b39_pki ADD PRIMARY KEY USING INDEX b39_pki_pkey
SELECT conname, contype, conkey FROM pg_constraint WHERE conrelid = 'b39_pki'::regclass
SELECT c.relname, i.indisprimary FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid WHERE i.indrelid = 'b39_pki'::regclass
INSERT INTO b39_pki VALUES (1, 1, 1, 1)
INSERT INTO b39_pki VALUES (2, 1, 1, 1)
COMMENT ON DOMAIN b39_dom IS 'b39 remark'
SELECT n.nspname, t.typname, t.typtype, obj_description(t.oid, 'pg_type') FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE t.typname IN ('b39_dom', 'b39_ct') ORDER BY 2
COMMENT ON DOMAIN b39_dom IS NULL
SELECT obj_description(t.oid, 'pg_type') FROM pg_type t WHERE t.typname = 'b39_dom'
SELECT t.typname, t.typtype FROM pg_proc p JOIN pg_type t ON t.oid = p.prorettype WHERE p.proname = 'b39_rec'
ALTER TABLE b39_err ADD CONSTRAINT b39_err_named PRIMARY KEY (id)
INSERT INTO b39_err VALUES (1, 1)
INSERT INTO b39_err VALUES (1, 2)
SELECT conname FROM pg_constraint WHERE conrelid = 'b39_err'::regclass
DROP TABLE b39_fk
DROP TABLE b39_pk
DROP TABLE b39_ix
DROP TABLE b39_pki
DROP TABLE b39_err
DROP DOMAIN b39_dom
DROP TYPE b39_s.b39_ct
DROP SCHEMA b39_s
DROP FUNCTION b39_rec(int)
