set statement_timeout = '120s';;
select count(*) from (
    SELECT CAST(current_database() AS information_schema.sql_identifier) AS object_catalog,
           CAST(n.nspname AS information_schema.sql_identifier) AS object_schema,
           CAST(x.objname AS information_schema.sql_identifier) AS object_name,
           CAST(x.objtype AS information_schema.character_data) AS object_type,
           CAST(x.objdtdid AS information_schema.sql_identifier) AS collection_type_identifier,
           CAST(
             CASE WHEN nbt.nspname = 'pg_catalog' THEN format_type(bt.oid, null)
                  ELSE 'USER-DEFINED' END AS information_schema.character_data) AS data_type,

           CAST(null AS information_schema.cardinal_number) AS character_maximum_length,
           CAST(null AS information_schema.cardinal_number) AS character_octet_length,
           CAST(null AS information_schema.sql_identifier) AS character_set_catalog,
           CAST(null AS information_schema.sql_identifier) AS character_set_schema,
           CAST(null AS information_schema.sql_identifier) AS character_set_name,
           CAST(CASE WHEN nco.nspname IS NOT NULL THEN current_database() END AS information_schema.sql_identifier) AS collation_catalog,
           CAST(nco.nspname AS information_schema.sql_identifier) AS collation_schema,
           CAST(co.collname AS information_schema.sql_identifier) AS collation_name,
           CAST(null AS information_schema.cardinal_number) AS numeric_precision,
           CAST(null AS information_schema.cardinal_number) AS numeric_precision_radix,
           CAST(null AS information_schema.cardinal_number) AS numeric_scale,
           CAST(null AS information_schema.cardinal_number) AS datetime_precision,
           CAST(null AS information_schema.character_data) AS interval_type,
           CAST(null AS information_schema.cardinal_number) AS interval_precision,

           CAST(null AS information_schema.character_data) AS domain_default, -- XXX maybe a bug in the standard

           CAST(current_database() AS information_schema.sql_identifier) AS udt_catalog,
           CAST(nbt.nspname AS information_schema.sql_identifier) AS udt_schema,
           CAST(bt.typname AS information_schema.sql_identifier) AS udt_name,

           CAST(null AS information_schema.sql_identifier) AS scope_catalog,
           CAST(null AS information_schema.sql_identifier) AS scope_schema,
           CAST(null AS information_schema.sql_identifier) AS scope_name,

           CAST(null AS information_schema.cardinal_number) AS maximum_cardinality,
           CAST('a' || CAST(x.objdtdid AS text) AS information_schema.sql_identifier) AS dtd_identifier

    FROM pg_namespace n, pg_type at, pg_namespace nbt, pg_type bt,
         (
           /* columns, attributes */
           SELECT c.relnamespace, CAST(c.relname AS information_schema.sql_identifier),
                  CASE WHEN c.relkind = 'c' THEN 'USER-DEFINED TYPE'::text ELSE 'TABLE'::text END,
                  a.attnum, a.atttypid, a.attcollation
           FROM pg_class c, pg_attribute a
           WHERE c.oid = a.attrelid
                 AND c.relkind IN ('r', 'v', 'f', 'c', 'p')
                 AND attnum > 0 AND NOT attisdropped

           UNION ALL

           /* domains */
           SELECT t.typnamespace, CAST(t.typname AS information_schema.sql_identifier),
                  'DOMAIN'::text, 1, t.typbasetype, t.typcollation
           FROM pg_type t
           WHERE t.typtype = 'd'

           UNION ALL

           /* parameters */
           SELECT pronamespace,
                  CAST(nameconcatoid(proname, oid) AS information_schema.sql_identifier),
                  'ROUTINE'::text, (ss.x).n, (ss.x).x, 0
           FROM (SELECT p.pronamespace, p.proname, p.oid,
                        information_schema._pg_expandarray(coalesce(p.proallargtypes, p.proargtypes::oid[])) AS x
                 FROM pg_proc p) AS ss

           UNION ALL

           /* result types */
           SELECT p.pronamespace,
                  CAST(nameconcatoid(p.proname, p.oid) AS information_schema.sql_identifier),
                  'ROUTINE'::text, 0, p.prorettype, 0
           FROM pg_proc p

         ) AS x (objschema, objname, objtype, objdtdid, objtypeid, objcollation)
         LEFT JOIN (pg_collation co JOIN pg_namespace nco ON (co.collnamespace = nco.oid))
           ON x.objcollation = co.oid AND (nco.nspname, co.collname) <> ('pg_catalog', 'default')

    WHERE n.oid = x.objschema
          AND at.oid = x.objtypeid
          AND (at.typelem <> 0 AND at.typlen = -1)
          AND at.typelem = bt.oid
          AND nbt.oid = bt.typnamespace

          AND (n.nspname, x.objname, x.objtype, CAST(x.objdtdid AS information_schema.sql_identifier)) IN
              ( SELECT object_schema, object_name, object_type, dtd_identifier
                    FROM information_schema.data_type_privileges )) q;;
select count(*) from (
    SELECT CAST(current_database() AS information_schema.sql_identifier) AS object_catalog,
           CAST(n.nspname AS information_schema.sql_identifier) AS object_schema,
           CAST(x.objname AS information_schema.sql_identifier) AS object_name,
           CAST(x.objtype AS information_schema.character_data) AS object_type,
           CAST(x.objdtdid AS information_schema.sql_identifier) AS collection_type_identifier,
           CAST(
             CASE WHEN nbt.nspname = 'pg_catalog' THEN format_type(bt.oid, null)
                  ELSE 'USER-DEFINED' END AS information_schema.character_data) AS data_type,

           CAST(null AS information_schema.cardinal_number) AS character_maximum_length,
           CAST(null AS information_schema.cardinal_number) AS character_octet_length,
           CAST(null AS information_schema.sql_identifier) AS character_set_catalog,
           CAST(null AS information_schema.sql_identifier) AS character_set_schema,
           CAST(null AS information_schema.sql_identifier) AS character_set_name,
           CAST(CASE WHEN nco.nspname IS NOT NULL THEN current_database() END AS information_schema.sql_identifier) AS collation_catalog,
           CAST(nco.nspname AS information_schema.sql_identifier) AS collation_schema,
           CAST(co.collname AS information_schema.sql_identifier) AS collation_name,
           CAST(null AS information_schema.cardinal_number) AS numeric_precision,
           CAST(null AS information_schema.cardinal_number) AS numeric_precision_radix,
           CAST(null AS information_schema.cardinal_number) AS numeric_scale,
           CAST(null AS information_schema.cardinal_number) AS datetime_precision,
           CAST(null AS information_schema.character_data) AS interval_type,
           CAST(null AS information_schema.cardinal_number) AS interval_precision,

           CAST(null AS information_schema.character_data) AS domain_default, -- XXX maybe a bug in the standard

           CAST(current_database() AS information_schema.sql_identifier) AS udt_catalog,
           CAST(nbt.nspname AS information_schema.sql_identifier) AS udt_schema,
           CAST(bt.typname AS information_schema.sql_identifier) AS udt_name,

           CAST(null AS information_schema.sql_identifier) AS scope_catalog,
           CAST(null AS information_schema.sql_identifier) AS scope_schema,
           CAST(null AS information_schema.sql_identifier) AS scope_name,

           CAST(null AS information_schema.cardinal_number) AS maximum_cardinality,
           CAST('a' || CAST(x.objdtdid AS text) AS information_schema.sql_identifier) AS dtd_identifier

    FROM (SELECT DISTINCT object_schema, object_name, object_type, dtd_identifier FROM information_schema.data_type_privileges) "ANY_subquery", pg_namespace n, pg_type at, pg_namespace nbt, pg_type bt,
         (
           /* columns, attributes */
           SELECT c.relnamespace, CAST(c.relname AS information_schema.sql_identifier),
                  CASE WHEN c.relkind = 'c' THEN 'USER-DEFINED TYPE'::text ELSE 'TABLE'::text END,
                  a.attnum, a.atttypid, a.attcollation
           FROM pg_class c, pg_attribute a
           WHERE c.oid = a.attrelid
                 AND c.relkind IN ('r', 'v', 'f', 'c', 'p')
                 AND attnum > 0 AND NOT attisdropped

           UNION ALL

           /* domains */
           SELECT t.typnamespace, CAST(t.typname AS information_schema.sql_identifier),
                  'DOMAIN'::text, 1, t.typbasetype, t.typcollation
           FROM pg_type t
           WHERE t.typtype = 'd'

           UNION ALL

           /* parameters */
           SELECT pronamespace,
                  CAST(nameconcatoid(proname, oid) AS information_schema.sql_identifier),
                  'ROUTINE'::text, (ss.x).n, (ss.x).x, 0
           FROM (SELECT p.pronamespace, p.proname, p.oid,
                        information_schema._pg_expandarray(coalesce(p.proallargtypes, p.proargtypes::oid[])) AS x
                 FROM pg_proc p) AS ss

           UNION ALL

           /* result types */
           SELECT p.pronamespace,
                  CAST(nameconcatoid(p.proname, p.oid) AS information_schema.sql_identifier),
                  'ROUTINE'::text, 0, p.prorettype, 0
           FROM pg_proc p

         ) AS x (objschema, objname, objtype, objdtdid, objtypeid, objcollation)
         LEFT JOIN (pg_collation co JOIN pg_namespace nco ON (co.collnamespace = nco.oid))
           ON x.objcollation = co.oid AND (nco.nspname, co.collname) <> ('pg_catalog', 'default')

    WHERE n.oid = x.objschema
          AND at.oid = x.objtypeid
          AND (at.typelem <> 0 AND at.typlen = -1)
          AND at.typelem = bt.oid
          AND nbt.oid = bt.typnamespace

          AND n.nspname = "ANY_subquery".object_schema AND x.objname = "ANY_subquery".object_name AND x.objtype = "ANY_subquery".object_type AND CAST(x.objdtdid AS information_schema.sql_identifier) = "ANY_subquery".dtd_identifier) q;;
select * from (
    SELECT CAST(current_database() AS information_schema.sql_identifier) AS object_catalog,
           CAST(n.nspname AS information_schema.sql_identifier) AS object_schema,
           CAST(x.objname AS information_schema.sql_identifier) AS object_name,
           CAST(x.objtype AS information_schema.character_data) AS object_type,
           CAST(x.objdtdid AS information_schema.sql_identifier) AS collection_type_identifier,
           CAST(
             CASE WHEN nbt.nspname = 'pg_catalog' THEN format_type(bt.oid, null)
                  ELSE 'USER-DEFINED' END AS information_schema.character_data) AS data_type,

           CAST(null AS information_schema.cardinal_number) AS character_maximum_length,
           CAST(null AS information_schema.cardinal_number) AS character_octet_length,
           CAST(null AS information_schema.sql_identifier) AS character_set_catalog,
           CAST(null AS information_schema.sql_identifier) AS character_set_schema,
           CAST(null AS information_schema.sql_identifier) AS character_set_name,
           CAST(CASE WHEN nco.nspname IS NOT NULL THEN current_database() END AS information_schema.sql_identifier) AS collation_catalog,
           CAST(nco.nspname AS information_schema.sql_identifier) AS collation_schema,
           CAST(co.collname AS information_schema.sql_identifier) AS collation_name,
           CAST(null AS information_schema.cardinal_number) AS numeric_precision,
           CAST(null AS information_schema.cardinal_number) AS numeric_precision_radix,
           CAST(null AS information_schema.cardinal_number) AS numeric_scale,
           CAST(null AS information_schema.cardinal_number) AS datetime_precision,
           CAST(null AS information_schema.character_data) AS interval_type,
           CAST(null AS information_schema.cardinal_number) AS interval_precision,

           CAST(null AS information_schema.character_data) AS domain_default, -- XXX maybe a bug in the standard

           CAST(current_database() AS information_schema.sql_identifier) AS udt_catalog,
           CAST(nbt.nspname AS information_schema.sql_identifier) AS udt_schema,
           CAST(bt.typname AS information_schema.sql_identifier) AS udt_name,

           CAST(null AS information_schema.sql_identifier) AS scope_catalog,
           CAST(null AS information_schema.sql_identifier) AS scope_schema,
           CAST(null AS information_schema.sql_identifier) AS scope_name,

           CAST(null AS information_schema.cardinal_number) AS maximum_cardinality,
           CAST('a' || CAST(x.objdtdid AS text) AS information_schema.sql_identifier) AS dtd_identifier

    FROM pg_namespace n, pg_type at, pg_namespace nbt, pg_type bt,
         (
           /* columns, attributes */
           SELECT c.relnamespace, CAST(c.relname AS information_schema.sql_identifier),
                  CASE WHEN c.relkind = 'c' THEN 'USER-DEFINED TYPE'::text ELSE 'TABLE'::text END,
                  a.attnum, a.atttypid, a.attcollation
           FROM pg_class c, pg_attribute a
           WHERE c.oid = a.attrelid
                 AND c.relkind IN ('r', 'v', 'f', 'c', 'p')
                 AND attnum > 0 AND NOT attisdropped

           UNION ALL

           /* domains */
           SELECT t.typnamespace, CAST(t.typname AS information_schema.sql_identifier),
                  'DOMAIN'::text, 1, t.typbasetype, t.typcollation
           FROM pg_type t
           WHERE t.typtype = 'd'

           UNION ALL

           /* parameters */
           SELECT pronamespace,
                  CAST(nameconcatoid(proname, oid) AS information_schema.sql_identifier),
                  'ROUTINE'::text, (ss.x).n, (ss.x).x, 0
           FROM (SELECT p.pronamespace, p.proname, p.oid,
                        information_schema._pg_expandarray(coalesce(p.proallargtypes, p.proargtypes::oid[])) AS x
                 FROM pg_proc p) AS ss

           UNION ALL

           /* result types */
           SELECT p.pronamespace,
                  CAST(nameconcatoid(p.proname, p.oid) AS information_schema.sql_identifier),
                  'ROUTINE'::text, 0, p.prorettype, 0
           FROM pg_proc p

         ) AS x (objschema, objname, objtype, objdtdid, objtypeid, objcollation)
         LEFT JOIN (pg_collation co JOIN pg_namespace nco ON (co.collnamespace = nco.oid))
           ON x.objcollation = co.oid AND (nco.nspname, co.collname) <> ('pg_catalog', 'default')

    WHERE n.oid = x.objschema
          AND at.oid = x.objtypeid
          AND (at.typelem <> 0 AND at.typlen = -1)
          AND at.typelem = bt.oid
          AND nbt.oid = bt.typnamespace

          AND (n.nspname, x.objname, x.objtype, CAST(x.objdtdid AS information_schema.sql_identifier)) IN
              ( SELECT object_schema, object_name, object_type, dtd_identifier
                    FROM information_schema.data_type_privileges )) q except all select * from (
    SELECT CAST(current_database() AS information_schema.sql_identifier) AS object_catalog,
           CAST(n.nspname AS information_schema.sql_identifier) AS object_schema,
           CAST(x.objname AS information_schema.sql_identifier) AS object_name,
           CAST(x.objtype AS information_schema.character_data) AS object_type,
           CAST(x.objdtdid AS information_schema.sql_identifier) AS collection_type_identifier,
           CAST(
             CASE WHEN nbt.nspname = 'pg_catalog' THEN format_type(bt.oid, null)
                  ELSE 'USER-DEFINED' END AS information_schema.character_data) AS data_type,

           CAST(null AS information_schema.cardinal_number) AS character_maximum_length,
           CAST(null AS information_schema.cardinal_number) AS character_octet_length,
           CAST(null AS information_schema.sql_identifier) AS character_set_catalog,
           CAST(null AS information_schema.sql_identifier) AS character_set_schema,
           CAST(null AS information_schema.sql_identifier) AS character_set_name,
           CAST(CASE WHEN nco.nspname IS NOT NULL THEN current_database() END AS information_schema.sql_identifier) AS collation_catalog,
           CAST(nco.nspname AS information_schema.sql_identifier) AS collation_schema,
           CAST(co.collname AS information_schema.sql_identifier) AS collation_name,
           CAST(null AS information_schema.cardinal_number) AS numeric_precision,
           CAST(null AS information_schema.cardinal_number) AS numeric_precision_radix,
           CAST(null AS information_schema.cardinal_number) AS numeric_scale,
           CAST(null AS information_schema.cardinal_number) AS datetime_precision,
           CAST(null AS information_schema.character_data) AS interval_type,
           CAST(null AS information_schema.cardinal_number) AS interval_precision,

           CAST(null AS information_schema.character_data) AS domain_default, -- XXX maybe a bug in the standard

           CAST(current_database() AS information_schema.sql_identifier) AS udt_catalog,
           CAST(nbt.nspname AS information_schema.sql_identifier) AS udt_schema,
           CAST(bt.typname AS information_schema.sql_identifier) AS udt_name,

           CAST(null AS information_schema.sql_identifier) AS scope_catalog,
           CAST(null AS information_schema.sql_identifier) AS scope_schema,
           CAST(null AS information_schema.sql_identifier) AS scope_name,

           CAST(null AS information_schema.cardinal_number) AS maximum_cardinality,
           CAST('a' || CAST(x.objdtdid AS text) AS information_schema.sql_identifier) AS dtd_identifier

    FROM (SELECT DISTINCT object_schema, object_name, object_type, dtd_identifier FROM information_schema.data_type_privileges) "ANY_subquery", pg_namespace n, pg_type at, pg_namespace nbt, pg_type bt,
         (
           /* columns, attributes */
           SELECT c.relnamespace, CAST(c.relname AS information_schema.sql_identifier),
                  CASE WHEN c.relkind = 'c' THEN 'USER-DEFINED TYPE'::text ELSE 'TABLE'::text END,
                  a.attnum, a.atttypid, a.attcollation
           FROM pg_class c, pg_attribute a
           WHERE c.oid = a.attrelid
                 AND c.relkind IN ('r', 'v', 'f', 'c', 'p')
                 AND attnum > 0 AND NOT attisdropped

           UNION ALL

           /* domains */
           SELECT t.typnamespace, CAST(t.typname AS information_schema.sql_identifier),
                  'DOMAIN'::text, 1, t.typbasetype, t.typcollation
           FROM pg_type t
           WHERE t.typtype = 'd'

           UNION ALL

           /* parameters */
           SELECT pronamespace,
                  CAST(nameconcatoid(proname, oid) AS information_schema.sql_identifier),
                  'ROUTINE'::text, (ss.x).n, (ss.x).x, 0
           FROM (SELECT p.pronamespace, p.proname, p.oid,
                        information_schema._pg_expandarray(coalesce(p.proallargtypes, p.proargtypes::oid[])) AS x
                 FROM pg_proc p) AS ss

           UNION ALL

           /* result types */
           SELECT p.pronamespace,
                  CAST(nameconcatoid(p.proname, p.oid) AS information_schema.sql_identifier),
                  'ROUTINE'::text, 0, p.prorettype, 0
           FROM pg_proc p

         ) AS x (objschema, objname, objtype, objdtdid, objtypeid, objcollation)
         LEFT JOIN (pg_collation co JOIN pg_namespace nco ON (co.collnamespace = nco.oid))
           ON x.objcollation = co.oid AND (nco.nspname, co.collname) <> ('pg_catalog', 'default')

    WHERE n.oid = x.objschema
          AND at.oid = x.objtypeid
          AND (at.typelem <> 0 AND at.typlen = -1)
          AND at.typelem = bt.oid
          AND nbt.oid = bt.typnamespace

          AND n.nspname = "ANY_subquery".object_schema AND x.objname = "ANY_subquery".object_name AND x.objtype = "ANY_subquery".object_type AND CAST(x.objdtdid AS information_schema.sql_identifier) = "ANY_subquery".dtd_identifier) q;;
select * from (
    SELECT CAST(current_database() AS information_schema.sql_identifier) AS object_catalog,
           CAST(n.nspname AS information_schema.sql_identifier) AS object_schema,
           CAST(x.objname AS information_schema.sql_identifier) AS object_name,
           CAST(x.objtype AS information_schema.character_data) AS object_type,
           CAST(x.objdtdid AS information_schema.sql_identifier) AS collection_type_identifier,
           CAST(
             CASE WHEN nbt.nspname = 'pg_catalog' THEN format_type(bt.oid, null)
                  ELSE 'USER-DEFINED' END AS information_schema.character_data) AS data_type,

           CAST(null AS information_schema.cardinal_number) AS character_maximum_length,
           CAST(null AS information_schema.cardinal_number) AS character_octet_length,
           CAST(null AS information_schema.sql_identifier) AS character_set_catalog,
           CAST(null AS information_schema.sql_identifier) AS character_set_schema,
           CAST(null AS information_schema.sql_identifier) AS character_set_name,
           CAST(CASE WHEN nco.nspname IS NOT NULL THEN current_database() END AS information_schema.sql_identifier) AS collation_catalog,
           CAST(nco.nspname AS information_schema.sql_identifier) AS collation_schema,
           CAST(co.collname AS information_schema.sql_identifier) AS collation_name,
           CAST(null AS information_schema.cardinal_number) AS numeric_precision,
           CAST(null AS information_schema.cardinal_number) AS numeric_precision_radix,
           CAST(null AS information_schema.cardinal_number) AS numeric_scale,
           CAST(null AS information_schema.cardinal_number) AS datetime_precision,
           CAST(null AS information_schema.character_data) AS interval_type,
           CAST(null AS information_schema.cardinal_number) AS interval_precision,

           CAST(null AS information_schema.character_data) AS domain_default, -- XXX maybe a bug in the standard

           CAST(current_database() AS information_schema.sql_identifier) AS udt_catalog,
           CAST(nbt.nspname AS information_schema.sql_identifier) AS udt_schema,
           CAST(bt.typname AS information_schema.sql_identifier) AS udt_name,

           CAST(null AS information_schema.sql_identifier) AS scope_catalog,
           CAST(null AS information_schema.sql_identifier) AS scope_schema,
           CAST(null AS information_schema.sql_identifier) AS scope_name,

           CAST(null AS information_schema.cardinal_number) AS maximum_cardinality,
           CAST('a' || CAST(x.objdtdid AS text) AS information_schema.sql_identifier) AS dtd_identifier

    FROM (SELECT DISTINCT object_schema, object_name, object_type, dtd_identifier FROM information_schema.data_type_privileges) "ANY_subquery", pg_namespace n, pg_type at, pg_namespace nbt, pg_type bt,
         (
           /* columns, attributes */
           SELECT c.relnamespace, CAST(c.relname AS information_schema.sql_identifier),
                  CASE WHEN c.relkind = 'c' THEN 'USER-DEFINED TYPE'::text ELSE 'TABLE'::text END,
                  a.attnum, a.atttypid, a.attcollation
           FROM pg_class c, pg_attribute a
           WHERE c.oid = a.attrelid
                 AND c.relkind IN ('r', 'v', 'f', 'c', 'p')
                 AND attnum > 0 AND NOT attisdropped

           UNION ALL

           /* domains */
           SELECT t.typnamespace, CAST(t.typname AS information_schema.sql_identifier),
                  'DOMAIN'::text, 1, t.typbasetype, t.typcollation
           FROM pg_type t
           WHERE t.typtype = 'd'

           UNION ALL

           /* parameters */
           SELECT pronamespace,
                  CAST(nameconcatoid(proname, oid) AS information_schema.sql_identifier),
                  'ROUTINE'::text, (ss.x).n, (ss.x).x, 0
           FROM (SELECT p.pronamespace, p.proname, p.oid,
                        information_schema._pg_expandarray(coalesce(p.proallargtypes, p.proargtypes::oid[])) AS x
                 FROM pg_proc p) AS ss

           UNION ALL

           /* result types */
           SELECT p.pronamespace,
                  CAST(nameconcatoid(p.proname, p.oid) AS information_schema.sql_identifier),
                  'ROUTINE'::text, 0, p.prorettype, 0
           FROM pg_proc p

         ) AS x (objschema, objname, objtype, objdtdid, objtypeid, objcollation)
         LEFT JOIN (pg_collation co JOIN pg_namespace nco ON (co.collnamespace = nco.oid))
           ON x.objcollation = co.oid AND (nco.nspname, co.collname) <> ('pg_catalog', 'default')

    WHERE n.oid = x.objschema
          AND at.oid = x.objtypeid
          AND (at.typelem <> 0 AND at.typlen = -1)
          AND at.typelem = bt.oid
          AND nbt.oid = bt.typnamespace

          AND n.nspname = "ANY_subquery".object_schema AND x.objname = "ANY_subquery".object_name AND x.objtype = "ANY_subquery".object_type AND CAST(x.objdtdid AS information_schema.sql_identifier) = "ANY_subquery".dtd_identifier) q except all select * from (
    SELECT CAST(current_database() AS information_schema.sql_identifier) AS object_catalog,
           CAST(n.nspname AS information_schema.sql_identifier) AS object_schema,
           CAST(x.objname AS information_schema.sql_identifier) AS object_name,
           CAST(x.objtype AS information_schema.character_data) AS object_type,
           CAST(x.objdtdid AS information_schema.sql_identifier) AS collection_type_identifier,
           CAST(
             CASE WHEN nbt.nspname = 'pg_catalog' THEN format_type(bt.oid, null)
                  ELSE 'USER-DEFINED' END AS information_schema.character_data) AS data_type,

           CAST(null AS information_schema.cardinal_number) AS character_maximum_length,
           CAST(null AS information_schema.cardinal_number) AS character_octet_length,
           CAST(null AS information_schema.sql_identifier) AS character_set_catalog,
           CAST(null AS information_schema.sql_identifier) AS character_set_schema,
           CAST(null AS information_schema.sql_identifier) AS character_set_name,
           CAST(CASE WHEN nco.nspname IS NOT NULL THEN current_database() END AS information_schema.sql_identifier) AS collation_catalog,
           CAST(nco.nspname AS information_schema.sql_identifier) AS collation_schema,
           CAST(co.collname AS information_schema.sql_identifier) AS collation_name,
           CAST(null AS information_schema.cardinal_number) AS numeric_precision,
           CAST(null AS information_schema.cardinal_number) AS numeric_precision_radix,
           CAST(null AS information_schema.cardinal_number) AS numeric_scale,
           CAST(null AS information_schema.cardinal_number) AS datetime_precision,
           CAST(null AS information_schema.character_data) AS interval_type,
           CAST(null AS information_schema.cardinal_number) AS interval_precision,

           CAST(null AS information_schema.character_data) AS domain_default, -- XXX maybe a bug in the standard

           CAST(current_database() AS information_schema.sql_identifier) AS udt_catalog,
           CAST(nbt.nspname AS information_schema.sql_identifier) AS udt_schema,
           CAST(bt.typname AS information_schema.sql_identifier) AS udt_name,

           CAST(null AS information_schema.sql_identifier) AS scope_catalog,
           CAST(null AS information_schema.sql_identifier) AS scope_schema,
           CAST(null AS information_schema.sql_identifier) AS scope_name,

           CAST(null AS information_schema.cardinal_number) AS maximum_cardinality,
           CAST('a' || CAST(x.objdtdid AS text) AS information_schema.sql_identifier) AS dtd_identifier

    FROM pg_namespace n, pg_type at, pg_namespace nbt, pg_type bt,
         (
           /* columns, attributes */
           SELECT c.relnamespace, CAST(c.relname AS information_schema.sql_identifier),
                  CASE WHEN c.relkind = 'c' THEN 'USER-DEFINED TYPE'::text ELSE 'TABLE'::text END,
                  a.attnum, a.atttypid, a.attcollation
           FROM pg_class c, pg_attribute a
           WHERE c.oid = a.attrelid
                 AND c.relkind IN ('r', 'v', 'f', 'c', 'p')
                 AND attnum > 0 AND NOT attisdropped

           UNION ALL

           /* domains */
           SELECT t.typnamespace, CAST(t.typname AS information_schema.sql_identifier),
                  'DOMAIN'::text, 1, t.typbasetype, t.typcollation
           FROM pg_type t
           WHERE t.typtype = 'd'

           UNION ALL

           /* parameters */
           SELECT pronamespace,
                  CAST(nameconcatoid(proname, oid) AS information_schema.sql_identifier),
                  'ROUTINE'::text, (ss.x).n, (ss.x).x, 0
           FROM (SELECT p.pronamespace, p.proname, p.oid,
                        information_schema._pg_expandarray(coalesce(p.proallargtypes, p.proargtypes::oid[])) AS x
                 FROM pg_proc p) AS ss

           UNION ALL

           /* result types */
           SELECT p.pronamespace,
                  CAST(nameconcatoid(p.proname, p.oid) AS information_schema.sql_identifier),
                  'ROUTINE'::text, 0, p.prorettype, 0
           FROM pg_proc p

         ) AS x (objschema, objname, objtype, objdtdid, objtypeid, objcollation)
         LEFT JOIN (pg_collation co JOIN pg_namespace nco ON (co.collnamespace = nco.oid))
           ON x.objcollation = co.oid AND (nco.nspname, co.collname) <> ('pg_catalog', 'default')

    WHERE n.oid = x.objschema
          AND at.oid = x.objtypeid
          AND (at.typelem <> 0 AND at.typlen = -1)
          AND at.typelem = bt.oid
          AND nbt.oid = bt.typnamespace

          AND (n.nspname, x.objname, x.objtype, CAST(x.objdtdid AS information_schema.sql_identifier)) IN
              ( SELECT object_schema, object_name, object_type, dtd_identifier
                    FROM information_schema.data_type_privileges )) q;;
