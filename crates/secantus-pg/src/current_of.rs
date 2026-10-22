//! `UPDATE / DELETE ... WHERE CURRENT OF c` (RP-072): the row the cursor
//! sits on, by its stored `_id`.
//!
//! The planner lowers the clause to a filter naming the cursor
//! (`secantus_pgplan::CURRENT_OF_KEY`); the statement replaces it with the
//! row's id when it runs, raising PostgreSQL's `execCurrentOf` errors in
//! its order (measured on 15.19): a cursor that does not exist (34000), a
//! WITH HOLD cursor whose transaction has ended, one that is not a simply
//! updatable scan of the target table, and one not positioned on a row
//! (each 24000). A row the block has since deleted matches nothing, as in
//! PostgreSQL (`DELETE 0`).

use super::*;

impl PgHandler {
    /// Whether a cursor over `sel` is a simply updatable scan of its table:
    /// PostgreSQL's `search_plan_tree` reaches the table's scan through
    /// Limit and Result nodes only, so a join, a grouping, a DISTINCT, a
    /// window or a set operation hides it, and so does the Sort an ORDER BY
    /// puts above it -- unless the cursor is FOR UPDATE / SHARE, whose row
    /// mark names the table whatever the plan.
    pub(crate) fn simply_updatable(&self, sel: &secantus_pgplan::Select) -> bool {
        let locked = sel.lock.is_some() || !sel.lock_clauses.is_empty();
        sel.join.is_none()
            && sel.sub.is_none()
            && sel.series.is_none()
            && sel.windows.is_empty()
            && sel.distinct == secantus_pgplan::Distinct::None
            && !sel.table.is_empty()
            && Self::virtual_table(&sel.table).is_none()
            && (sel.order.is_empty() || locked || self.index_ordered(sel))
    }

    /// Whether PostgreSQL reads `sel`'s ORDER BY from a B-tree index rather
    /// than sorting: a cursor is planned for its first tenth
    /// (`cursor_tuple_fraction`), which an index scan in the wanted order
    /// starts fastest, so it is the plan whenever the ORDER BY keys are
    /// plain columns leading an index -- forward, or every key reversed
    /// (a backward scan), NULLS as the index keeps them (measured on 15.19:
    /// a cursor `order by` its primary key is an Index Scan, updatable).
    /// A partial index, a non-B-tree one, or one with its own NULLS order,
    /// operator class or collation is not counted.
    fn index_ordered(&self, sel: &secantus_pgplan::Select) -> bool {
        use secantus_pgplan::Nulls;
        if sel.order.iter().any(|k| k.expr.is_some() || k.jsonb) {
            return false;
        }
        let Ok(indexes) = self.storage.list_indexes(self.db(), &sel.table) else {
            return false;
        };
        indexes.iter().any(|ix| {
            if ["partialFilterExpression", "sqlMethod", "sqlKeys"]
                .iter()
                .any(|k| ix.contains_key(*k))
            {
                return false;
            }
            let Ok(key) = ix.get_document("key") else {
                return false;
            };
            let cols: Vec<(&str, bool)> = key
                .iter()
                .map(|(f, d)| (f.as_str(), !matches!(d, Bson::Int32(-1) | Bson::Int64(-1))))
                .collect();
            if cols.len() < sel.order.len() {
                return false;
            }
            // An ascending index column keeps NULLs last, a descending one
            // first; a backward scan reverses both.
            let matches = |backward: bool| {
                sel.order.iter().zip(&cols).all(|(k, (f, asc))| {
                    let ascending = *asc != backward;
                    let nulls_last = ascending;
                    k.field == *f
                        && k.ascending == ascending
                        && matches!(
                            (&k.nulls, nulls_last),
                            (Nulls::Last, true) | (Nulls::First, false)
                        )
                })
            };
            matches(false) || matches(true)
        })
    }

    /// `filter` with a `WHERE CURRENT OF` marker replaced by the id of the
    /// row its cursor sits on; any other filter unchanged.
    pub(crate) fn resolve_current_of(
        &self,
        table: &str,
        filter: Document,
    ) -> PgWireResult<Document> {
        let Some(name) = filter
            .get_str(secantus_pgplan::CURRENT_OF_KEY)
            .ok()
            .map(str::to_string)
        else {
            return Ok(filter);
        };
        let cursors = self.cursors.lock().unwrap_or_else(|e| e.into_inner());
        let Some(cursor) = cursors.get(&name) else {
            return Err(Self::user_error(
                "34000",
                format!("cursor \"{name}\" does not exist"),
            ));
        };
        if cursor.held {
            return Err(Self::user_error(
                "24000",
                format!("cursor \"{name}\" is held from a previous transaction"),
            ));
        }
        let (_, relname) = secantus_pgplan::schemas::split_key(table);
        let Some(scan) = cursor.current_of.as_ref().filter(|c| c.table == table) else {
            return Err(Self::user_error(
                "24000",
                format!("cursor \"{name}\" is not a simply updatable scan of table \"{relname}\""),
            ));
        };
        let id = usize::try_from(cursor.pos - 1)
            .ok()
            .and_then(|i| i.checked_sub(cursor.base))
            .and_then(|i| scan.ids.get(i));
        let Some(id) = id else {
            return Err(Self::user_error(
                "24000",
                format!("cursor \"{name}\" is not positioned on a row"),
            ));
        };
        Ok(bson::doc! {"_id": id.clone()})
    }
}
