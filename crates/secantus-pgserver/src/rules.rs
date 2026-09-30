//! `CREATE RULE` on INSERT / UPDATE / DELETE, stored in `__sql_rules__` and
//! listed in `pg_rewrite` / `pg_rules`. The rules themselves are applied by
//! the planner (`secantus_pgplan::rule_rewrite`), which rewrites a statement
//! into the ones PostgreSQL's rewriter produces; this module is their
//! catalog.

use bson::{Bson, Document};
use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;
use secantus_pgcatalog::{Column, TableDef};

use crate::PgHandler;

thread_local! {
    /// How deep rule rewrites are nested (a rule action whose target has
    /// rules of its own is rewritten in turn).
    static DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(crate) const RULE_COLLECTION: &str = "__sql_rules__";

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

    /// Does any table have a rule?
    pub(crate) fn any_rules(&self) -> bool {
        !self.rule_docs().is_empty()
    }

    /// Does `table` have any rule?
    pub(crate) fn has_rules(&self, table: &str) -> bool {
        self.rule_docs()
            .iter()
            .any(|d| d.get_str("table") == Ok(table))
    }

    pub(crate) fn create_rule(&self, rule: Rule, replace: bool) -> PgWireResult<Vec<Response>> {
        let is_view = self.views()?.iter().any(|(n, _)| *n == rule.table);
        if self.lookup(&rule.table).is_none() && !is_view {
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

    /// Run a rewritten statement: each step planned (with the original's
    /// parameters) and executed in order, the original's own step with its
    /// relation's rules not applied again.
    pub(crate) fn run_rule_plan(
        &self,
        p: secantus_pgplan::rule_rewrite::RulePlan,
        max_rows: usize,
    ) -> PgWireResult<Vec<Response>> {
        if DEPTH.with(|d| d.get()) >= 16 {
            return Err(Self::user_error(
                "42P17",
                format!(
                    "infinite recursion detected in rules for relation \"{}\"",
                    p.table
                ),
            ));
        }
        DEPTH.with(|d| d.set(d.get() + 1));
        let out = self.run_rule_steps(&p, max_rows);
        DEPTH.with(|d| d.set(d.get() - 1));
        out
    }

    fn run_rule_steps(
        &self,
        p: &secantus_pgplan::rule_rewrite::RulePlan,
        max_rows: usize,
    ) -> PgWireResult<Vec<Response>> {
        let tz = self.session_timezone();
        let mut tagged: Option<Vec<Response>> = None;
        for (i, (sql, is_original)) in p.steps.iter().enumerate() {
            let plan = || {
                let run = |stmt: &secantus_pgplan::Statement| self.subquery_rows(stmt);
                secantus_pgplan::planning_to_execute(|| {
                    secantus_pgplan::plan_with_session_types_and_subqueries(
                        sql,
                        &|n| self.lookup(n),
                        &p.params,
                        &p.param_types,
                        &tz,
                        Some(&run),
                    )
                })
            };
            let stmt = if *is_original {
                secantus_pgplan::rule_rewrite::with_rules_suppressed(&p.table, plan)
            } else {
                plan()
            }
            .map_err(|e| Self::err(&e))?;
            let responses = if *is_original {
                secantus_pgplan::rule_rewrite::with_rules_suppressed(&p.table, || {
                    self.execute(stmt, max_rows)
                })?
            } else {
                self.execute(stmt, max_rows)?
            };
            if p.tag_step == Some(i) {
                tagged = Some(responses);
            }
        }
        Ok(tagged.unwrap_or_else(|| {
            let tag = match p.kind.as_str() {
                "INSERT" => Tag::new("INSERT").with_oid(0).with_rows(0),
                other => Tag::new(other).with_rows(0),
            };
            vec![Response::Execution(tag)]
        }))
    }

    /// The enabled rules, for the planner.
    pub(crate) fn enabled_rules(&self) -> Vec<secantus_pgplan::rule_rewrite::RuleDef> {
        self.rule_docs()
            .iter()
            .map(rule_of)
            .filter(|(_, enabled, _)| *enabled)
            .map(|(r, _, _)| secantus_pgplan::rule_rewrite::RuleDef {
                table: r.table,
                name: r.name,
                event: r.event,
                instead: r.instead,
                condition: r.condition,
                actions: r.actions,
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
        let views = self.views().unwrap_or_default();
        let lookup = |n: &str| self.lookup(n);
        let view_sql = |n: &str| views.iter().find(|(v, _)| v == n).map(|(_, d)| d.clone());
        let cat = secantus_pgplan::ruleutils::Catalog {
            lookup: &lookup,
            view_sql: &view_sql,
        };
        self.rule_docs()
            .iter()
            .map(rule_of)
            .map(|(r, _, _)| {
                if let Some(text) = secantus_pgplan::ruleutils::rule_def(
                    &r.name,
                    &r.table,
                    &r.event,
                    r.instead,
                    r.condition.as_deref(),
                    &r.actions,
                    &cat,
                ) {
                    let mut d = Document::new();
                    d.insert(f("schemaname"), "public");
                    d.insert(f("tablename"), r.table.as_str());
                    d.insert(f("rulename"), r.name.as_str());
                    d.insert(f("definition"), text);
                    return d;
                }
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
