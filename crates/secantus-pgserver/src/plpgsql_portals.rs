//! The portal a PL/pgSQL `OPEN` makes (see `plpgsql_fn/open_cursor.rs`): the
//! query's rows, kept as a cursor of this session -- the same state a SQL
//! `DECLARE` keeps, so `FETCH` / `MOVE` / `CLOSE` and `pg_cursors` see it.

use super::*;

impl PlHost<'_> {
    pub(crate) fn open_portal(
        &self,
        name: Option<&str>,
        statement: &str,
        sql: &str,
        params: &[Bson],
        types: &[String],
        scroll: bool,
    ) -> Result<String, plpgsql_fn::PlError> {
        let stmt = self.plan(sql, params, types)?;
        let (schema, rows) = self
            .joined(|| self.atomic(|| self.h.rows_with_schema(&stmt).map_err(|e| pl_error(&e))))?;
        self.h
            .register_portal(name, statement, schema, rows, scroll)
            .map_err(|e| pl_error(&e))
    }
}

impl PgHandler {
    /// Keep `rows` as the cursor `name` (or the next `<unnamed portal N>`).
    pub(crate) fn register_portal(
        &self,
        name: Option<&str>,
        statement: &str,
        schema: Vec<FieldInfo>,
        rows: Vec<Vec<Option<Bson>>>,
        scroll: bool,
    ) -> PgWireResult<String> {
        let mut cursors = self.cursors.lock().unwrap_or_else(|e| e.into_inner());
        let name = match name {
            Some(n) => n.to_string(),
            None => loop {
                let n = self
                    .portal_seq
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    + 1;
                let candidate = format!("<unnamed portal {n}>");
                if !cursors.contains_key(&candidate) {
                    break candidate;
                }
            },
        };
        if cursors.contains_key(&name) {
            return Err(Self::user_error(
                "42P03",
                format!("cursor \"{name}\" already in use"),
            ));
        }
        let schema = Arc::new(schema);
        let tz = self.session_timezone();
        let ds = self.session_datestyle();
        let cenc = self.client_encoding();
        let text_rows = rows
            .iter()
            .map(|r| encode_typed_row(&schema, r, &tz, &ds, cenc))
            .collect::<PgWireResult<Vec<_>>>()?;
        cursors.insert(
            name.clone(),
            CursorState {
                schema,
                rows: text_rows,
                pos: 0,
                statement: statement.to_string(),
                is_holdable: false,
                is_binary: false,
                is_scrollable: scroll,
                creation_time: bson::DateTime::now(),
                typed_rows: Some(rows),
                tz,
                tail: None,
                base: 0,
            },
        );
        Ok(name)
    }
}
