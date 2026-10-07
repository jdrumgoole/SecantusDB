//! Named protocol-level portals (a `Bind` with a portal name -- pgjdbc's
//! `C_n` for a fetch-size query) as `pg_cursors` lists them. PostgreSQL shows
//! every open portal there, not only the SQL `DECLARE`d ones, and a portal
//! lasts until it is closed or its transaction ends.

use super::*;

/// One open named portal: its query, binary-ness and creation time.
pub(crate) struct WirePortal {
    statement: String,
    binary: bool,
    created: bson::DateTime,
}

impl PgHandler {
    pub(crate) fn note_wire_portal(&self, name: &str, statement: &str, format: &Format) {
        if name.is_empty() {
            return;
        }
        let binary = match format {
            Format::UnifiedBinary => true,
            Format::Individual(f) => !f.is_empty() && f.iter().all(|c| *c == 1),
            Format::UnifiedText => false,
        };
        self.wire_portals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                name.to_string(),
                WirePortal {
                    statement: statement.to_string(),
                    binary,
                    created: bson::DateTime::now(),
                },
            );
    }

    pub(crate) fn forget_wire_portal(&self, name: &str) {
        self.wire_portals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name);
    }

    /// The transaction ended: so did its portals.
    pub(crate) fn forget_wire_portals(&self) {
        self.wire_portals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// `pg_cursors` rows for the open named portals that are not also SQL
    /// cursors (those are listed from `cursors`).
    pub(crate) fn wire_portal_rows(&self, def: &TableDef) -> Vec<Document> {
        let field = |c: &str| def.field_of(c).expect("column");
        let declared = self.cursors.lock().unwrap_or_else(|e| e.into_inner());
        self.wire_portals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(name, _)| !declared.contains_key(*name))
            .map(|(name, p)| {
                let mut d = Document::new();
                d.insert(field("name"), name.as_str());
                d.insert(field("statement"), p.statement.as_str());
                d.insert(field("is_holdable"), Bson::Boolean(false));
                d.insert(field("is_binary"), Bson::Boolean(p.binary));
                d.insert(field("is_scrollable"), Bson::Boolean(false));
                d.insert(field("creation_time"), Bson::DateTime(p.created));
                d
            })
            .collect()
    }
}
