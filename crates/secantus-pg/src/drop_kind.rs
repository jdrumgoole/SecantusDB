//! A DROP that names a relation of another kind (RP-528).
//!
//! PostgreSQL's `RemoveRelations` (tablecmds.c) looks every name of a DROP
//! TABLE, VIEW, MATERIALIZED VIEW, INDEX, SEQUENCE or FOREIGN TABLE up
//! before it drops any of them, and `RangeVarCallbackForDropRelation`
//! refuses a relation whose `relkind` is not the statement's with
//! `DropErrorMsgWrongType`: 42809 `"m" is not a table`, hinting at the DROP
//! that fits the relation's real kind. `IF EXISTS` does not excuse it: the
//! name exists, as the wrong kind.

use super::*;

/// A relation's `pg_class.relkind`, as far as a DROP tells them apart: a
/// partitioned table is a table and a partitioned index an index, as in
/// `RangeVarCallbackForDropRelation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelKind {
    Table,
    Sequence,
    View,
    MatView,
    Index,
    CompositeType,
    ForeignTable,
}

impl RelKind {
    /// `dropmsgstringarray`'s `nota_msg`, for the kind the DROP expected.
    fn nota_msg(self, name: &str) -> String {
        match self {
            RelKind::Table => format!("\"{name}\" is not a table"),
            RelKind::Sequence => format!("\"{name}\" is not a sequence"),
            RelKind::View => format!("\"{name}\" is not a view"),
            RelKind::MatView => format!("\"{name}\" is not a materialized view"),
            RelKind::Index => format!("\"{name}\" is not an index"),
            RelKind::CompositeType => format!("\"{name}\" is not a type"),
            RelKind::ForeignTable => format!("\"{name}\" is not a foreign table"),
        }
    }

    /// The kind's name in `dropmsgstringarray`'s `nonexistent_msg`.
    fn noun(self) -> &'static str {
        match self {
            RelKind::Table => "table",
            RelKind::Sequence => "sequence",
            RelKind::View => "view",
            RelKind::MatView => "materialized view",
            RelKind::Index => "index",
            RelKind::CompositeType => "type",
            RelKind::ForeignTable => "foreign table",
        }
    }

    /// `dropmsgstringarray`'s `drophint_msg`, for the kind the relation is.
    fn drophint_msg(self) -> &'static str {
        match self {
            RelKind::Table => "Use DROP TABLE to remove a table.",
            RelKind::Sequence => "Use DROP SEQUENCE to remove a sequence.",
            RelKind::View => "Use DROP VIEW to remove a view.",
            RelKind::MatView => "Use DROP MATERIALIZED VIEW to remove a materialized view.",
            RelKind::Index => "Use DROP INDEX to remove an index.",
            RelKind::CompositeType => "Use DROP TYPE to remove a type.",
            RelKind::ForeignTable => "Use DROP FOREIGN TABLE to remove a foreign table.",
        }
    }
}

/// `DropErrorMsgWrongType`: `name` is a `wrong`, the DROP wanted a `right`.
pub(crate) fn drop_wrong_type(name: &str, wrong: RelKind, right: RelKind) -> PgWireError {
    let bare = secantus_pgplan::schemas::split_key(name).1;
    let mut info = ErrorInfo::new(
        "ERROR".into(),
        "42809".into(), // wrong_object_type
        right.nota_msg(&bare),
    );
    info.hint = Some(wrong.drophint_msg().into());
    PgWireError::UserError(Box::new(info))
}

/// `DropErrorMsgNonExistent`: `rel->relname`, the bare name, in the
/// kind's `nonexistent_msg` and `nonexistent_code` -- an undefined object
/// for an index or a foreign table, an undefined table otherwise.
fn drop_nonexistent(name: &str, right: RelKind) -> PgWireError {
    let code = match right {
        RelKind::Index | RelKind::ForeignTable => "42704", // undefined_object
        _ => "42P01",                                      // undefined_table
    };
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".into(),
        code.into(),
        format!(
            "{} \"{}\" does not exist",
            right.noun(),
            secantus_pgplan::schemas::split_key(name).1
        ),
    )))
}

impl PgHandler {
    /// The kind of relation catalog key `key` names, or `None` when no
    /// relation has that name. A materialized view's rows live in a table,
    /// so it is asked for first.
    pub(crate) fn relation_kind(&self, key: &str) -> PgWireResult<Option<RelKind>> {
        if self.matviews()?.iter().any(|(n, _, _)| n == key) {
            return Ok(Some(RelKind::MatView));
        }
        if self.views()?.iter().any(|(n, _)| n == key) {
            return Ok(Some(RelKind::View));
        }
        if self.sequence_doc(key)?.is_some() {
            return Ok(Some(RelKind::Sequence));
        }
        if let Some(def) = self.lookup(key) {
            return Ok(Some(if secantus_pgplan::fdw::is_foreign(&def) {
                RelKind::ForeignTable
            } else {
                RelKind::Table
            }));
        }
        if self.find_index_by_name(key)?.is_some() {
            return Ok(Some(RelKind::Index));
        }
        if self.composites()?.iter().any(|(n, _, _)| n == key)
            || secantus_pgplan::is_standalone_composite(key)
        {
            return Ok(Some(RelKind::CompositeType));
        }
        Ok(None)
    }

    /// `RemoveRelations`' lookup of each name, in order, before anything is
    /// dropped: the first that is a relation of another kind than `right`,
    /// or (without IF EXISTS) is missing, refuses the whole statement --
    /// ahead of any dependency check, and with none of the names dropped.
    /// Under IF EXISTS a missing name's NOTICE is raised here, as the lookup
    /// goes. Returns the names that exist, for the DROP to drop.
    pub(crate) fn check_drop_kinds(
        &self,
        names: &[String],
        right: RelKind,
        if_exists: bool,
    ) -> PgWireResult<Vec<String>> {
        let mut present = Vec::with_capacity(names.len());
        for name in names {
            // Index names are kept per table: one the DROP INDEX finds is
            // its index, whatever else shares the name.
            if right == RelKind::Index && self.find_index_by_name(name)?.is_some() {
                present.push(name.clone());
                continue;
            }
            match self.relation_kind(name)? {
                None if if_exists => self.notice(
                    "00000",
                    format!(
                        "{} \"{}\" does not exist, skipping",
                        right.noun(),
                        secantus_pgplan::schemas::split_key(name).1
                    ),
                    None,
                ),
                None => return Err(drop_nonexistent(name, right)),
                Some(kind) if kind == right => present.push(name.clone()),
                Some(wrong) => return Err(drop_wrong_type(name, wrong, right)),
            }
        }
        Ok(present)
    }
}
