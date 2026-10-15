//! The catalog reads behind the SQL/XML mappings, which PostgreSQL 15's
//! xml.c does through SPI and the syscache (`schema_get_xml_visible_tables`,
//! `database_get_xml_visible_schemas`, `LookupExplicitNamespace`, ...), run
//! here through the executor as the same SQL; and the checks SPI makes on a
//! query it is handed as text.

use super::*;

/// A relation as the schema mappings name it: `(relname, nspname)`.
pub(crate) type Rel = (String, String);

/// `get_database_name(MyDatabaseId)`.
pub(crate) fn database_name() -> Result<String> {
    Ok(correlated::run_sql("SELECT current_database()::text")?
        .first()
        .and_then(|r| r.first())
        .map(render_value_text)
        .unwrap_or_default())
}

/// The oids a catalog query returns, in its order.
pub(crate) fn oid_list(sql: &str) -> Result<Vec<i64>> {
    Ok(correlated::run_sql(sql)?
        .iter()
        .filter_map(|r| r.first())
        .filter(|v| **v != Bson::Null)
        .map(|v| crate::xml_schema::int_of(Some(v)))
        .collect())
}

/// xml.c's `XML_VISIBLE_SCHEMAS`, with the privilege asked by the schema's
/// name: `has_schema_privilege(oid, ...)` reads its oid here as a name.
const VISIBLE_SCHEMAS: &str = "SELECT oid FROM pg_catalog.pg_namespace WHERE \
     pg_catalog.has_schema_privilege (nspname, 'USAGE') AND NOT \
     (nspname ~ '^pg_' OR nspname = 'information_schema')";

/// `schema_get_xml_visible_tables`: the schema's tables, views and
/// materialized views its role may read, by name.
pub(crate) fn schema_tables(nspid: i64) -> Result<Vec<i64>> {
    oid_list(&format!(
        "SELECT oid FROM pg_catalog.pg_class WHERE relnamespace = {nspid} AND \
         relkind IN ('r','m','v') AND pg_catalog.has_table_privilege (oid, 'SELECT') \
         ORDER BY relname"
    ))
}

/// `database_get_xml_visible_schemas`, by name.
pub(crate) fn database_schemas() -> Result<Vec<i64>> {
    oid_list(&format!("{VISIBLE_SCHEMAS} ORDER BY nspname"))
}

/// `database_get_xml_visible_tables`. PostgreSQL asks for no order and
/// reads pg_class's heap, which for relations made in turn is oid order.
pub(crate) fn database_tables() -> Result<Vec<i64>> {
    oid_list(&format!(
        "SELECT oid FROM pg_catalog.pg_class WHERE relkind IN ('r','m','v') AND \
         pg_catalog.has_table_privilege(pg_class.oid, 'SELECT') AND relnamespace IN \
         ({VISIBLE_SCHEMAS}) ORDER BY oid"
    ))
}

/// A relation's `(relname, nspname)` by oid.
pub(crate) fn relation(oid: i64) -> Result<Rel> {
    let rows = correlated::run_sql(&format!(
        "SELECT c.relname::text, n.nspname::text FROM pg_catalog.pg_class c JOIN \
         pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE c.oid = {oid}"
    ))?;
    let row = rows
        .first()
        .ok_or_else(|| Error::Internal(format!("cache lookup failed for relation {oid}")))?;
    let text = |i: usize| row.get(i).map(render_value_text).unwrap_or_default();
    Ok((text(0), text(1)))
}

/// A schema's name by oid.
pub(crate) fn schema_name(oid: i64) -> Result<String> {
    Ok(correlated::run_sql(&format!(
        "SELECT nspname::text FROM pg_catalog.pg_namespace WHERE oid = {oid}"
    ))?
    .first()
    .and_then(|r| r.first())
    .map(render_value_text)
    .unwrap_or_default())
}

/// `LookupExplicitNamespace(name, false)`: 3F000 for a schema that does not
/// exist.
pub(crate) fn schema_oid(name: &str) -> Result<i64> {
    let lit = name.replace('\'', "''");
    oid_list(&format!(
        "SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = '{lit}'"
    ))?
    .first()
    .copied()
    .ok_or_else(|| Error::Sqlstate("3F000", format!("schema \"{name}\" does not exist")))
}

/// What SPI says of a query it is handed as text before running it.
/// `as_cursor` is `SPI_cursor_open`'s check (the schema mappings open the
/// query as a portal to read its descriptor): 42P11 for one that returns
/// no rows or is more than one statement. Then, as every mapping runs its
/// query read-only, 0A000 for anything that is not a plain SELECT (15.19:
/// `INSERT is not allowed in a non-volatile function`). A statement this
/// does not know by name is left to the executor.
pub(crate) fn check_query(sql: &str, as_cursor: bool) -> Result<()> {
    let Ok(tree) = pg_query::parse(sql) else {
        return Ok(());
    };
    let stmts = &tree.protobuf.stmts;
    if stmts.len() > 1 {
        return match as_cursor {
            true => Err(Error::Sqlstate(
                "42P11",
                "cannot open multi-query plan as cursor".into(),
            )),
            false => Ok(()),
        };
    }
    let Some(node) = stmts.first().and_then(|s| s.stmt.as_ref()?.node.as_ref()) else {
        return Ok(());
    };
    let (command, returns_rows) = match node {
        N::SelectStmt(_) => return Ok(()),
        N::InsertStmt(i) => ("INSERT", !i.returning_list.is_empty()),
        N::UpdateStmt(u) => ("UPDATE", !u.returning_list.is_empty()),
        N::DeleteStmt(d) => ("DELETE", !d.returning_list.is_empty()),
        N::ExplainStmt(_) => ("EXPLAIN", true),
        N::VariableShowStmt(_) => ("SHOW", true),
        N::CreateStmt(_) => ("CREATE TABLE", false),
        _ => return Ok(()),
    };
    if as_cursor && !returns_rows {
        return Err(Error::Sqlstate(
            "42P11",
            format!("cannot open {command} query as cursor"),
        ));
    }
    Err(Error::Sqlstate(
        "0A000",
        format!("{command} is not allowed in a non-volatile function"),
    ))
}
