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
        // A simply updatable scan keeps each row's id, for `WHERE CURRENT
        // OF` (`current_of.rs`).
        let updatable = match &stmt {
            Statement::Select(sel) if self.h.simply_updatable(sel) => Some(sel.table.clone()),
            _ => None,
        };
        if updatable.is_some() {
            *self.h.cursor_ids.lock().unwrap_or_else(|e| e.into_inner()) = Some(Vec::new());
        }
        let read = self
            .joined(|| self.atomic(|| self.h.rows_with_schema(&stmt).map_err(|e| pl_error(&e))));
        let ids = self
            .h
            .cursor_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let (schema, rows) = read?;
        let current_of = updatable
            .zip(ids)
            .filter(|(_, ids)| ids.len() == rows.len())
            .map(|(table, ids)| CurrentOf { table, ids });
        self.h
            .register_portal(name, statement, schema, rows, scroll, current_of)
            .map_err(|e| pl_error(&e))
    }

    /// PL/pgSQL's `FETCH` / `MOVE`: the SQL statement's step, then the row
    /// the cursor sits on.
    pub(crate) fn fetch_portal(
        &self,
        name: &str,
        direction: secantus_pgplan::FetchDirection,
        count: i64,
    ) -> Result<(u64, plpgsql_fn::QueryOut), plpgsql_fn::PlError> {
        let (moved, _) = self
            .h
            .fetch_n(name, direction, count, true)
            .map_err(|e| pl_error(&e))?;
        let moved = moved as u64;
        let cursors = self.h.cursors.lock().unwrap_or_else(|e| e.into_inner());
        let Some(cursor) = cursors.get(name) else {
            return Err(pl_error(&PgHandler::user_error(
                "34000",
                format!("cursor \"{name}\" does not exist"),
            )));
        };
        let columns = cursor
            .schema
            .iter()
            .map(|f| {
                let ty = secantus_pgplan::pgtypes::type_name_of_oid(i64::from(f.datatype().oid()))
                    .unwrap_or_else(|| "text".to_string());
                (f.name().to_string(), ty)
            })
            .collect();
        let row = usize::try_from(cursor.pos - 1)
            .ok()
            .and_then(|i| i.checked_sub(cursor.base))
            .and_then(|i| cursor.typed_rows.as_ref()?.get(i))
            .map(|r| r.iter().map(|v| v.clone().unwrap_or(Bson::Null)).collect());
        Ok((
            moved,
            plpgsql_fn::QueryOut {
                columns,
                rows: row.into_iter().collect(),
            },
        ))
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
        current_of: Option<CurrentOf>,
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
                born: next_cursor_born(),
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
                failed: false,
                current_of,
                held: false,
            },
        );
        Ok(name)
    }
}
