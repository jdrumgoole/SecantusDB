//! `CREATE RULE` on INSERT / UPDATE / DELETE: a query rewrite PostgreSQL
//! applies before any trigger. Each rule runs here as a ROW-level action
//! with `NEW` / `OLD`, which is what a rule's action sees for the rows its
//! statement touches:
//!
//! * `DO ALSO <actions>` runs its actions after each row is written (when
//!   the rule's WHERE holds);
//! * `DO INSTEAD <actions>` runs them in place of the row, which is not
//!   written;
//! * `DO INSTEAD NOTHING` drops the row.
//!
//! They are executed as synthetic triggers (`rule_triggers`), ordered ahead
//! of the table's real triggers because a rewrite happens first, and they are
//! never listed in `pg_trigger`. `pg_rewrite` and `pg_rules` report them.

use bson::{Bson, Document};
use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;
use secantus_pgcatalog::{Column, TableDef};

use crate::PgHandler;

pub(crate) const RULE_COLLECTION: &str = "__sql_rules__";

thread_local! {
    /// Rows an INSTEAD rule of the statement's own kind replaced.
    static INSTEAD_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Read and reset the replaced-row count.
pub(crate) fn take_instead_rows() -> usize {
    INSTEAD_ROWS.with(|c| c.replace(0))
}

pub(crate) fn add_instead_rows(n: usize) {
    INSTEAD_ROWS.with(|c| c.set(c.get() + n));
}

/// One rule, as `CREATE RULE` planned it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Rule {
    pub table: String,
    pub name: String,
    /// `INSERT` / `UPDATE` / `DELETE`.
    pub event: String,
    pub instead: bool,
    /// The WHERE condition, as SQL.
    pub condition: Option<String>,
    /// The action statements, as SQL; empty for `NOTHING`.
    pub actions: Vec<String>,
}

fn rule_key(table: &str, name: &str) -> String {
    format!("{table}/{name}")
}

fn rule_of(d: &Document) -> (Rule, bool, i64) {
    let s = |k: &str| d.get_str(k).unwrap_or_default().to_string();
    (
        Rule {
            table: s("table"),
            name: s("name"),
            event: s("event"),
            instead: d.get_bool("instead").unwrap_or(false),
            condition: d.get_str("where").ok().map(str::to_string),
            actions: d
                .get_array("actions")
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        },
        d.get_bool("enabled").unwrap_or(true),
        d.get_i64("oid").unwrap_or(0),
    )
}

impl PgHandler {
    fn rule_docs(&self) -> Vec<Document> {
        self.type_catalog_docs(RULE_COLLECTION)
            .map(|d| d.to_vec())
            .unwrap_or_default()
    }

    /// Does `table` have any rule?
    pub(crate) fn has_rules(&self, table: &str) -> bool {
        self.rule_docs()
            .iter()
            .any(|d| d.get_str("table") == Ok(table))
    }

    pub(crate) fn create_rule(&self, rule: Rule, replace: bool) -> PgWireResult<Vec<Response>> {
        if self.lookup(&rule.table).is_none() {
            return Err(Self::relation_missing(&rule.table));
        }
        let key = rule_key(&rule.table, &rule.name);
        let exists = self
            .rule_docs()
            .iter()
            .any(|d| d.get_str("_id") == Ok(&key));
        if exists && !replace {
            return Err(Self::user_error(
                "42710",
                format!(
                    "rule \"{}\" for relation \"{}\" already exists",
                    rule.name, rule.table
                ),
            ));
        }
        let mut doc = bson::doc! {
            "_id": &key,
            "table": &rule.table,
            "name": &rule.name,
            "event": &rule.event,
            "instead": rule.instead,
            "actions": rule.actions.iter().map(|a| Bson::String(a.clone())).collect::<Vec<_>>(),
            "enabled": true,
            "oid": Self::index_oid(&format!("rule:{key}")),
        };
        if let Some(w) = &rule.condition {
            doc.insert("where", w);
        }
        self.put(RULE_COLLECTION, &key, doc)?;
        Ok(vec![Response::Execution(Tag::new("CREATE RULE"))])
    }

    pub(crate) fn drop_rule(
        &self,
        table: &str,
        name: &str,
        if_exists: bool,
    ) -> PgWireResult<Vec<Response>> {
        let key = rule_key(table, name);
        if !self
            .rule_docs()
            .iter()
            .any(|d| d.get_str("_id") == Ok(&key))
        {
            let message = format!("rule \"{name}\" for relation \"{table}\" does not exist");
            if if_exists {
                self.notice("00000", format!("{message}, skipping"), None);
                return Ok(vec![Response::Execution(Tag::new("DROP RULE"))]);
            }
            return Err(Self::user_error("42704", message));
        }
        self.delete_type_doc(RULE_COLLECTION, &key)?;
        Ok(vec![Response::Execution(Tag::new("DROP RULE"))])
    }

    /// `ALTER TABLE ... ENABLE / DISABLE RULE name`.
    pub(crate) fn set_rule_enabled(
        &self,
        table: &str,
        name: &str,
        enabled: bool,
    ) -> PgWireResult<()> {
        let key = rule_key(table, name);
        let Some(mut doc) = self
            .rule_docs()
            .into_iter()
            .find(|d| d.get_str("_id") == Ok(&key))
        else {
            return Err(Self::user_error(
                "42704",
                format!("rule \"{name}\" for table \"{table}\" does not exist"),
            ));
        };
        doc.insert("enabled", enabled);
        self.put(RULE_COLLECTION, &key, doc)
    }

    /// A dropped table's rules go with it.
    pub(crate) fn drop_table_rules(&self, table: &str) -> PgWireResult<()> {
        for d in self.rule_docs() {
            if d.get_str("table") == Ok(table) {
                self.delete_type_doc(RULE_COLLECTION, d.get_str("_id").unwrap_or_default())?;
            }
        }
        Ok(())
    }

    /// The rules on `table` for `event` that run at `timing` (`BEFORE` for
    /// INSTEAD, `AFTER` for ALSO), as synthetic ROW trigger documents
    /// carrying their own PL/pgSQL body (`inline_function`), in name order.
    pub(crate) fn rule_triggers(&self, table: &str, timing: &str, event: &str) -> Vec<Document> {
        let mut rules: Vec<Rule> = self
            .rule_docs()
            .iter()
            .map(rule_of)
            .filter(|(r, enabled, _)| {
                *enabled
                    && r.table == table
                    && r.event == event
                    && (timing == "BEFORE") == r.instead
            })
            .map(|(r, _, _)| r)
            .collect();
        rules.sort_by(|a, b| a.name.cmp(&b.name));
        rules
            .into_iter()
            .map(|r| {
                let row = if r.event == "DELETE" { "OLD" } else { "NEW" };
                let actions: String = r.actions.iter().map(|a| format!("{a}; ")).collect();
                let body = if r.instead {
                    // The row is replaced: the actions run, and the row is
                    // not written (a trigger's NULL).
                    match &r.condition {
                        Some(c) => format!(
                            "BEGIN IF ({c}) THEN {actions}RETURN NULL; END IF; RETURN {row}; END"
                        ),
                        None => format!("BEGIN {actions}RETURN NULL; END"),
                    }
                } else {
                    match &r.condition {
                        Some(c) => format!("BEGIN IF ({c}) THEN {actions}END IF; RETURN NULL; END"),
                        None => format!("BEGIN {actions}RETURN NULL; END"),
                    }
                };
                bson::doc! {
                    "name": format!("\u{1}rule:{}", r.name),
                    "table": &r.table,
                    "timing": timing,
                    "level": "ROW",
                    "events": [event],
                    "rule": true,
                    "rule_instead_same_kind": r.instead
                        && r.actions.iter().any(|a| a.trim_start().to_ascii_uppercase().starts_with(&r.event)),
                    "inline_function": format!(
                        "CREATE FUNCTION secantus_rule() RETURNS trigger AS $secantus_rule$ {body} $secantus_rule$ LANGUAGE plpgsql"
                    ),
                }
            })
            .collect()
    }

    /// `pg_rewrite`'s rows for the user rules.
    pub(crate) fn pg_rewrite_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        self.rule_docs()
            .iter()
            .map(rule_of)
            .map(|(r, enabled, oid)| {
                let mut d = Document::new();
                d.insert(f("oid"), Bson::Int64(oid));
                d.insert(f("rulename"), r.name.as_str());
                d.insert(
                    f("ev_class"),
                    Bson::Int64(self.relation_oid(&r.table).unwrap_or(0)),
                );
                d.insert(
                    f("ev_type"),
                    match r.event.as_str() {
                        "UPDATE" => "2",
                        "INSERT" => "3",
                        "DELETE" => "4",
                        _ => "1",
                    },
                );
                d.insert(f("ev_enabled"), if enabled { "O" } else { "D" });
                d.insert(f("is_instead"), r.instead);
                d
            })
            .collect()
    }

    /// `pg_rules`: each rule's definition as ruleutils prints it.
    pub(crate) fn pg_rules_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        self.rule_docs()
            .iter()
            .map(rule_of)
            .map(|(r, _, _)| {
                let condition = r
                    .condition
                    .as_ref()
                    .map(|c| format!("\n   WHERE ({c})"))
                    .unwrap_or_default();
                let kind = if r.instead { "INSTEAD" } else { "" };
                let body = match r.actions.as_slice() {
                    [] => " NOTHING".to_string(),
                    [one] => format!(" {one}"),
                    many => format!(" ({})", many.join("; ")),
                };
                let definition = format!(
                    "CREATE RULE {} AS\n    ON {} TO public.{}{condition} DO {kind}{body};",
                    r.name, r.event, r.table
                );
                let mut d = Document::new();
                d.insert(f("schemaname"), "public");
                d.insert(f("tablename"), r.table.as_str());
                d.insert(f("rulename"), r.name.as_str());
                d.insert(f("definition"), definition);
                d
            })
            .collect()
    }
}

/// `pg_rewrite`.
pub(crate) fn pg_rewrite_def() -> TableDef {
    TableDef::new(
        "pg_rewrite",
        vec![
            Column::new("oid", "oid", false),
            Column::new("rulename", "name", false),
            Column::new("ev_class", "oid", false),
            Column::new("ev_type", secantus_pgplan::QUOTED_CHAR, false),
            Column::new("ev_enabled", secantus_pgplan::QUOTED_CHAR, false),
            Column::new("is_instead", "bool", false),
        ],
    )
}

/// `pg_rules`.
pub(crate) fn pg_rules_def() -> TableDef {
    TableDef::new(
        "pg_rules",
        vec![
            Column::new("schemaname", "name", false),
            Column::new("tablename", "name", false),
            Column::new("rulename", "name", false),
            Column::new("definition", "text", false),
        ],
    )
}
