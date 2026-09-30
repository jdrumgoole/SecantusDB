//! Triggers: `CREATE` / `DROP TRIGGER`, and firing them around a write.
//!
//! A trigger is stored in `__sql_triggers__` in the Python server's shape
//! (`{_id: "table::name", name, table, timing, event, level, function}`), with
//! the facts that server does not record -- every event, `UPDATE OF` columns,
//! the arguments and the `WHEN` condition -- as extra keys it ignores.
//!
//! Firing follows PostgreSQL: BEFORE ROW triggers run in NAME order over each
//! row after its defaults are applied and before its constraints are checked,
//! and may rewrite it (`RETURN NEW` with changes) or skip it (`RETURN NULL`);
//! AFTER ROW triggers run once the statement's rows are written, where their
//! return value is ignored; STATEMENT triggers run once, even for no rows.

use bson::{Bson, Document};
use pgwire::api::results::{Response, Tag};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use secantus_pgcatalog::TableDef;
use secantus_pgplan::TriggerDef;

use crate::plpgsql_fn::{self, Record, TriggerData};
use crate::{PgHandler, PlHost};

pub(crate) const TRIGGER_COLLECTION: &str = "__sql_triggers__";

/// A constraint trigger's row event, held for COMMIT.
pub(crate) struct DeferredTrigger {
    pub(crate) trg: Document,
    op: String,
    new: Option<Record>,
    old: Option<Record>,
}

fn user_error(code: &str, message: String) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".into(),
        code.into(),
        message,
    )))
}

/// A trigger document's events: every one it names, or the Python server's
/// single `event`.
fn events_of(doc: &Document) -> Vec<String> {
    match doc.get_array("events") {
        Ok(a) => a
            .iter()
            .filter_map(|e| e.as_str().map(String::from))
            .collect(),
        Err(_) => doc
            .get_str("event")
            .map(|e| vec![e.to_string()])
            .unwrap_or_default(),
    }
}

fn strings(doc: &Document, key: &str) -> Vec<String> {
    doc.get_array(key)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

impl PgHandler {
    /// Every stored trigger.
    pub(crate) fn trigger_docs(&self) -> PgWireResult<Vec<Document>> {
        Ok(self.type_catalog_docs(TRIGGER_COLLECTION)?.to_vec())
    }

    /// The triggers on `table` for one timing / event / level, in name order.
    fn triggers_for(
        &self,
        table: &str,
        timing: &str,
        event: &str,
        level: &str,
    ) -> PgWireResult<Vec<Document>> {
        let mut out: Vec<Document> = self
            .trigger_docs()?
            .into_iter()
            .filter(|d| {
                d.get_str("table") == Ok(table)
                    && d.get_bool("enabled").unwrap_or(true)
                    && d.get_str("timing") == Ok(timing)
                    && d.get_str("level").unwrap_or("ROW") == level
                    && events_of(d).iter().any(|e| e == event)
            })
            .collect();
        out.sort_by(|a, b| a.get_str("name").ok().cmp(&b.get_str("name").ok()));
        Ok(out)
    }

    /// Does `table` have any trigger at all? The write paths check this
    /// first, so a table without one pays nothing.
    pub(crate) fn has_triggers(&self, table: &str) -> PgWireResult<bool> {
        Ok(self
            .trigger_docs()?
            .iter()
            .any(|d| d.get_str("table") == Ok(table)))
    }

    /// `ALTER TABLE ... ENABLE / DISABLE TRIGGER name | ALL | USER`.
    pub(crate) fn set_trigger_enabled(
        &self,
        table: &str,
        name: Option<&str>,
        enabled: bool,
    ) -> PgWireResult<()> {
        let mut found = false;
        for mut d in self.trigger_docs()? {
            if d.get_str("table") != Ok(table) || name.is_some_and(|n| d.get_str("name") != Ok(n)) {
                continue;
            }
            found = true;
            let id = d.get_str("_id").unwrap_or_default().to_string();
            d.insert("enabled", enabled);
            self.delete_type_doc(TRIGGER_COLLECTION, &id)?;
            self.insert_type_doc(TRIGGER_COLLECTION, &id, d)?;
        }
        match name {
            Some(n) if !found => Err(user_error(
                "42704",
                format!("trigger \"{n}\" for table \"{table}\" does not exist"),
            )),
            _ => Ok(()),
        }
    }

    pub(crate) fn create_trigger(&self, def: TriggerDef) -> PgWireResult<()> {
        let is_view = self.views()?.iter().any(|(n, _)| *n == def.table);
        if self.lookup(&def.table).is_none() && !is_view {
            return Err(user_error(
                "42P01",
                format!("relation \"{}\" does not exist", def.table),
            ));
        }
        // INSTEAD OF is for views only, and a view takes no row-level
        // BEFORE / AFTER trigger (measured on PostgreSQL 14).
        let wrong = |kind: &str, detail: &str| {
            let mut info = ErrorInfo::new(
                "ERROR".into(),
                "42809".into(),
                format!("\"{}\" is a {kind}", def.table),
            );
            info.detail = Some(detail.into());
            PgWireError::UserError(Box::new(info))
        };
        if def.timing == "INSTEAD OF" && !is_view {
            return Err(wrong("table", "Tables cannot have INSTEAD OF triggers."));
        }
        if is_view && def.timing != "INSTEAD OF" && def.level == "ROW" {
            return Err(wrong(
                "view",
                "Views cannot have row-level BEFORE or AFTER triggers.",
            ));
        }
        let function = self.user_function_docs()?.into_iter().find(|d| {
            d.get_str("name") == Ok(def.function.as_str()) && d.get_i32("nargs") == Ok(0)
        });
        let Some(function) = function else {
            return Err(user_error(
                "42883",
                format!("function {}() does not exist", def.function),
            ));
        };
        if !function.get_bool("returns_trigger").unwrap_or(false) {
            return Err(user_error(
                "42P17",
                format!("function {} must return type trigger", def.function),
            ));
        }
        let key = format!("{}::{}", def.table, def.name);
        self.ensure_collection(TRIGGER_COLLECTION)?;
        let exists = self
            .trigger_docs()?
            .iter()
            .any(|d| d.get_str("_id") == Ok(key.as_str()));
        if exists {
            if !def.replace {
                return Err(user_error(
                    "42710",
                    format!(
                        "trigger \"{}\" for relation \"{}\" already exists",
                        def.name, def.table
                    ),
                ));
            }
            self.delete_type_doc(TRIGGER_COLLECTION, &key)?;
        }
        let mut doc = bson::doc! {
            "_id": &key,
            "name": &def.name,
            "table": &def.table,
            "timing": &def.timing,
            "event": def.events.first().cloned().unwrap_or_default(),
            "events": def.events.clone(),
            "level": &def.level,
            "function": &def.function,
            "args": def.args.clone(),
            "update_columns": def.update_columns.clone(),
        };
        if let Some(n) = &def.transition_new {
            doc.insert("transition_new", n.as_str());
        }
        if let Some(o) = &def.transition_old {
            doc.insert("transition_old", o.as_str());
        }
        if def.constraint {
            doc.insert("constraint", true);
            doc.insert("deferrable", def.deferrable);
            doc.insert("initially_deferred", def.initially_deferred);
        }
        if let Some(when) = &def.when {
            doc.insert("when", when.clone());
        }
        self.insert_type_doc(TRIGGER_COLLECTION, &key, doc)
    }

    /// A trigger's oid, derived from its key so `pg_trigger` and
    /// `pg_get_triggerdef` agree on it.
    pub(crate) fn trigger_oid(doc: &Document) -> i64 {
        Self::index_oid(&format!(
            "trigger:{}",
            doc.get_str("_id").unwrap_or_default()
        )) | 0x0800_0000
    }

    /// Every trigger's `(oid, pg_get_triggerdef text)`, with the table
    /// schema-qualified (the non-pretty form).
    pub(crate) fn trigger_defs(&self) -> Vec<(i64, String)> {
        self.trigger_docs()
            .unwrap_or_default()
            .iter()
            .map(|t| {
                let strs = |k: &str| -> Vec<String> {
                    t.get_array(k)
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let mut events = strs("events");
                if events.is_empty() {
                    events.push(t.get_str("event").unwrap_or_default().to_string());
                }
                // ruleutils' order: the TRIGGER_TYPE_* bits, low to high.
                let update_columns = strs("update_columns");
                let mut parts = Vec::new();
                for e in ["INSERT", "DELETE", "UPDATE", "TRUNCATE"] {
                    if events.iter().any(|x| x == e) {
                        if e == "UPDATE" && !update_columns.is_empty() {
                            parts.push(format!("UPDATE OF {}", update_columns.join(", ")));
                        } else {
                            parts.push(e.to_string());
                        }
                    }
                }
                let constraint = t.get_bool("constraint").unwrap_or(false);
                let mut text = format!(
                    "CREATE {}TRIGGER {} {} {} ON public.{}",
                    if constraint { "CONSTRAINT " } else { "" },
                    t.get_str("name").unwrap_or_default(),
                    t.get_str("timing").unwrap_or("BEFORE"),
                    parts.join(" OR "),
                    t.get_str("table").unwrap_or_default(),
                );
                if constraint {
                    text.push_str(if t.get_bool("deferrable").unwrap_or(false) {
                        if t.get_bool("initially_deferred").unwrap_or(false) {
                            " DEFERRABLE INITIALLY DEFERRED"
                        } else {
                            " DEFERRABLE INITIALLY IMMEDIATE"
                        }
                    } else {
                        " NOT DEFERRABLE INITIALLY IMMEDIATE"
                    });
                }
                let old_t = t.get_str("transition_old").ok();
                let new_t = t.get_str("transition_new").ok();
                if old_t.is_some() || new_t.is_some() {
                    text.push_str(" REFERENCING");
                    if let Some(o) = old_t {
                        text.push_str(&format!(" OLD TABLE AS {o}"));
                    }
                    if let Some(n) = new_t {
                        text.push_str(&format!(" NEW TABLE AS {n}"));
                    }
                }
                text.push_str(&format!(
                    " FOR EACH {}",
                    t.get_str("level").unwrap_or("ROW")
                ));
                if let Ok(when) = t.get_str("when") {
                    text.push_str(&format!(" WHEN ({when})"));
                }
                let args: Vec<String> = strs("args")
                    .iter()
                    .map(|a| secantus_pgplan::scalar::quote_literal(a))
                    .collect();
                text.push_str(&format!(
                    " EXECUTE FUNCTION {}({})",
                    t.get_str("function").unwrap_or_default(),
                    args.join(", ")
                ));
                (Self::trigger_oid(t), text)
            })
            .collect()
    }

    pub(crate) fn drop_trigger(
        &self,
        name: &str,
        table: &str,
        if_exists: bool,
    ) -> PgWireResult<()> {
        if self.lookup(table).is_none() && !self.views()?.iter().any(|(n, _)| n == table) {
            if if_exists {
                self.notice(
                    "00000",
                    format!("relation \"{table}\" does not exist, skipping"),
                    None,
                );
                return Ok(());
            }
            return Err(user_error(
                "42P01",
                format!("relation \"{table}\" does not exist"),
            ));
        }
        let key = format!("{table}::{name}");
        let exists = self
            .trigger_docs()?
            .iter()
            .any(|d| d.get_str("_id") == Ok(key.as_str()));
        if !exists {
            if if_exists {
                self.notice(
                    "00000",
                    format!("trigger \"{name}\" for relation \"{table}\" does not exist, skipping"),
                    None,
                );
                return Ok(());
            }
            return Err(user_error(
                "42704",
                format!("trigger \"{name}\" for table \"{table}\" does not exist"),
            ));
        }
        self.delete_type_doc(TRIGGER_COLLECTION, &key)
    }

    /// Remove every trigger on `table` -- a dropped table takes its
    /// triggers with it.
    pub(crate) fn drop_table_triggers(&self, table: &str) -> PgWireResult<()> {
        for d in self.trigger_docs()? {
            if d.get_str("table") == Ok(table) {
                if let Ok(id) = d.get_str("_id") {
                    self.delete_type_doc(TRIGGER_COLLECTION, id)?;
                }
            }
        }
        self.drop_table_rules(table)
    }

    /// The triggers that call `function`, for DROP FUNCTION's dependency
    /// check.
    pub(crate) fn triggers_calling(&self, function: &str) -> PgWireResult<Vec<Document>> {
        Ok(self
            .trigger_docs()?
            .into_iter()
            .filter(|d| d.get_str("function") == Ok(function))
            .collect())
    }

    /// A stored row as the record a trigger sees.
    pub(crate) fn row_record(&self, def: &TableDef, row: &Document) -> PgWireResult<Record> {
        let tz = self.session_timezone();
        let mut columns = Vec::with_capacity(def.columns.len());
        let mut values = Vec::with_capacity(def.columns.len());
        for c in &def.columns {
            let wire = self
                .user_wire_type(&c.pg_type)
                .unwrap_or_else(|| crate::wire_type(&c.pg_type));
            let v = crate::resolve_cell(row, &c.field(), None, &wire, &tz)?;
            columns.push((c.name.clone(), c.pg_type.clone()));
            values.push(v.unwrap_or(Bson::Null));
        }
        Ok(Record { columns, values })
    }

    /// A trigger's returned record as a row to store, cast to each column's
    /// type as an assignment would be.
    fn record_row(def: &TableDef, rec: Record) -> PgWireResult<Document> {
        let names: Vec<String> = def.columns.iter().map(|c| c.name.clone()).collect();
        let mut values = Vec::with_capacity(names.len());
        for n in &names {
            let i = rec.columns.iter().position(|(c, _)| c == n);
            values.push(i.map(|i| rec.values[i].clone()).unwrap_or(Bson::Null));
        }
        secantus_pgplan::insert_row(def, &names, true, values).map_err(|e| Self::err(&e))
    }

    /// Run one trigger function. `Ok(None)` is `RETURN NULL`.
    fn run_trigger(
        &self,
        trg: &Document,
        op: &str,
        new: Option<Record>,
        old: Option<Record>,
    ) -> PgWireResult<Option<Record>> {
        let function = trg.get_str("function").unwrap_or_default();
        let doc = self
            .user_function_docs()?
            .into_iter()
            .find(|d| d.get_str("name") == Ok(function) && d.get_i32("nargs") == Ok(0))
            .ok_or_else(|| user_error("42883", format!("function {function}() does not exist")))?;
        let source = crate::plpgsql_create_sql(&doc);
        let data = TriggerData {
            new,
            old,
            op: op.to_string(),
            name: trg.get_str("name").unwrap_or_default().to_string(),
            table: trg.get_str("table").unwrap_or_default().to_string(),
            when: trg.get_str("timing").unwrap_or_default().to_string(),
            level: trg.get_str("level").unwrap_or("ROW").to_string(),
            args: strings(trg, "args"),
        };
        let outcome = self.with_transition_tables(trg, || {
            self.with_call_depth(|| {
                plpgsql_fn::run(
                    &source,
                    plpgsql_fn::Invocation {
                        args: &[],
                        arg_types: &[],
                        trigger: Some(data),
                        returns_set: false,
                    },
                    &PlHost { h: self },
                )
                .map_err(crate::wire_pl_error)
            })
        })?;
        let row = match outcome {
            plpgsql_fn::Outcome::Record(r) => r,
            _ => None,
        };
        Ok(row)
    }

    /// Does a row trigger's `WHEN` condition hold? Evaluated by the same
    /// interpreter, as a function returning the row when it does.
    fn when_holds(
        &self,
        trg: &Document,
        op: &str,
        new: &Option<Record>,
        old: &Option<Record>,
    ) -> PgWireResult<bool> {
        let Ok(cond) = trg.get_str("when") else {
            return Ok(true);
        };
        let row = if op == "DELETE" { "OLD" } else { "NEW" };
        let sql = format!(
            "CREATE FUNCTION secantus_trigger_when() RETURNS trigger AS $secantus_when$ \
             BEGIN IF ({cond}) THEN RETURN {row}; END IF; RETURN NULL; END \
             $secantus_when$ LANGUAGE plpgsql"
        );
        let data = TriggerData {
            new: new.clone(),
            old: old.clone(),
            op: op.to_string(),
            name: trg.get_str("name").unwrap_or_default().to_string(),
            table: trg.get_str("table").unwrap_or_default().to_string(),
            when: trg.get_str("timing").unwrap_or_default().to_string(),
            level: "ROW".to_string(),
            args: Vec::new(),
        };
        let outcome = plpgsql_fn::run(
            &sql,
            plpgsql_fn::Invocation {
                args: &[],
                arg_types: &[],
                trigger: Some(data),
                returns_set: false,
            },
            &PlHost { h: self },
        )
        .map_err(crate::wire_pl_error)?;
        Ok(matches!(outcome, plpgsql_fn::Outcome::Record(Some(_))))
    }

    /// Fire the STATEMENT triggers for one timing and event.
    pub(crate) fn fire_statement_triggers(
        &self,
        table: &str,
        timing: &str,
        event: &str,
    ) -> PgWireResult<()> {
        // A MERGE fires its target's statement triggers itself, once per
        // action type, rather than once per row action it runs.
        if crate::merge::statement_triggers_suppressed(table) {
            return Ok(());
        }
        for trg in self.triggers_for(table, timing, event, "STATEMENT")? {
            // A statement trigger's WHEN reads no row, so it is a plain
            // boolean expression.
            if let Ok(cond) = trg.get_str("when") {
                let out = plpgsql_fn::Host::query(
                    &PlHost { h: self },
                    &format!("SELECT ({cond})::boolean"),
                    &[],
                    &[],
                )
                .map_err(crate::wire_pl_error)?;
                if out.rows.first().and_then(|r| r.first()) != Some(&Bson::Boolean(true)) {
                    continue;
                }
            }
            self.run_trigger(&trg, event, None, None)?;
        }
        Ok(())
    }

    /// BEFORE INSERT ROW: each row through the table's triggers in turn.
    /// A row a trigger answers NULL for is dropped.
    pub(crate) fn before_insert_rows(
        &self,
        def: &TableDef,
        rows: Vec<Document>,
    ) -> PgWireResult<Vec<Document>> {
        let triggers = self.triggers_for(&def.name, "BEFORE", "INSERT", "ROW")?;
        if triggers.is_empty() {
            return Ok(rows);
        }
        let mut out = Vec::with_capacity(rows.len());
        'rows: for row in rows {
            let mut rec = self.row_record(def, &row)?;
            let mut changed = false;
            for trg in &triggers {
                let new = Some(rec.clone());
                if !self.when_holds(trg, "INSERT", &new, &None)? {
                    continue;
                }
                match self.run_trigger(trg, "INSERT", new, None)? {
                    Some(r) => {
                        changed |= r.values != rec.values;
                        rec = r;
                    }
                    None => continue 'rows,
                }
            }
            out.push(if changed {
                Self::record_row(def, rec)?
            } else {
                row
            });
        }
        Ok(out)
    }

    /// AFTER INSERT ROW, over the rows as written.
    pub(crate) fn after_insert_rows(&self, def: &TableDef, rows: &[Document]) -> PgWireResult<()> {
        self.note_transition(&def.name, rows.to_vec(), Vec::new());
        let triggers = self.triggers_for(&def.name, "AFTER", "INSERT", "ROW")?;
        for row in rows {
            if triggers.is_empty() {
                break;
            }
            let new = Some(self.row_record(def, row)?);
            for trg in &triggers {
                if self.when_holds(trg, "INSERT", &new, &None)? {
                    self.fire_after_row(trg, "INSERT", new.clone(), None)?;
                }
            }
        }
        Ok(())
    }

    /// Does an `UPDATE OF cols` trigger apply to an UPDATE assigning
    /// `targets` (column names)?
    fn update_of_applies(trg: &Document, targets: &[String]) -> bool {
        let cols = strings(trg, "update_columns");
        cols.is_empty() || cols.iter().any(|c| targets.contains(c))
    }

    /// BEFORE UPDATE ROW for one row: `Ok(None)` skips it, otherwise the
    /// row to write (unchanged when no trigger touched it).
    pub(crate) fn before_update_row(
        &self,
        def: &TableDef,
        targets: &[String],
        old_row: &Document,
        new_row: Document,
    ) -> PgWireResult<Option<Document>> {
        let triggers = self.triggers_for(&def.name, "BEFORE", "UPDATE", "ROW")?;
        if triggers.is_empty() {
            return Ok(Some(new_row));
        }
        let old = Some(self.row_record(def, old_row)?);
        let mut rec = self.row_record(def, &new_row)?;
        let mut changed = false;
        for trg in &triggers {
            if !Self::update_of_applies(trg, targets) {
                continue;
            }
            let new = Some(rec.clone());
            if !self.when_holds(trg, "UPDATE", &new, &old)? {
                continue;
            }
            match self.run_trigger(trg, "UPDATE", new, old.clone())? {
                Some(r) => {
                    changed |= r.values != rec.values;
                    rec = r;
                }
                None => return Ok(None),
            }
        }
        if !changed {
            return Ok(Some(new_row));
        }
        // The row keeps its key: the rebuilt row carries every column.
        let mut rebuilt = Self::record_row(def, rec)?;
        for (k, v) in old_row {
            if k == "_id" || k.starts_with("_id.") {
                rebuilt.insert(k.clone(), v.clone());
            }
        }
        Ok(Some(rebuilt))
    }

    /// AFTER UPDATE ROW over `(old, new)` pairs.
    pub(crate) fn after_update_rows(
        &self,
        def: &TableDef,
        targets: &[String],
        pairs: &[(Document, Document)],
    ) -> PgWireResult<()> {
        self.note_transition(
            &def.name,
            pairs.iter().map(|(_, n)| n.clone()).collect(),
            pairs.iter().map(|(o, _)| o.clone()).collect(),
        );
        let triggers = self.triggers_for(&def.name, "AFTER", "UPDATE", "ROW")?;
        if triggers.is_empty() {
            return Ok(());
        }
        for (old_row, new_row) in pairs {
            let old = Some(self.row_record(def, old_row)?);
            let new = Some(self.row_record(def, new_row)?);
            for trg in &triggers {
                if Self::update_of_applies(trg, targets)
                    && self.when_holds(trg, "UPDATE", &new, &old)?
                {
                    self.fire_after_row(trg, "UPDATE", new.clone(), old.clone())?;
                }
            }
        }
        Ok(())
    }

    /// BEFORE DELETE ROW: does the row survive to be deleted?
    pub(crate) fn before_delete_row(&self, def: &TableDef, row: &Document) -> PgWireResult<bool> {
        let triggers = self.triggers_for(&def.name, "BEFORE", "DELETE", "ROW")?;
        if triggers.is_empty() {
            return Ok(true);
        }
        let old = Some(self.row_record(def, row)?);
        for trg in &triggers {
            if !self.when_holds(trg, "DELETE", &None, &old)? {
                continue;
            }
            if self
                .run_trigger(trg, "DELETE", None, old.clone())?
                .is_none()
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// AFTER DELETE ROW over the deleted rows.
    pub(crate) fn after_delete_rows(&self, def: &TableDef, rows: &[Document]) -> PgWireResult<()> {
        self.note_transition(&def.name, Vec::new(), rows.to_vec());
        let triggers = self.triggers_for(&def.name, "AFTER", "DELETE", "ROW")?;
        if triggers.is_empty() {
            return Ok(());
        }
        for row in rows {
            let old = Some(self.row_record(def, row)?);
            for trg in &triggers {
                if self.when_holds(trg, "DELETE", &None, &old)? {
                    self.fire_after_row(trg, "DELETE", None, old.clone())?;
                }
            }
        }
        Ok(())
    }

    /// A write to a view with `INSTEAD OF` row triggers: each row the
    /// statement names goes to the triggers (in name order) instead of to
    /// any table; a row counts when every trigger returned non-NULL.
    pub(crate) fn run_instead_of(
        &self,
        io: secantus_pgplan::instead_of::InsteadOf,
    ) -> PgWireResult<Vec<Response>> {
        let q = secantus_pgplan::scalar::quote_identifier;
        let (view_cols, _) =
            self.internal_query(&format!("SELECT * FROM {} LIMIT 0", q(&io.view)))?;
        let width = view_cols.len();
        let (_, rows) = self.internal_query(&io.query_sql)?;
        let triggers = self.triggers_for(&io.view, "INSTEAD OF", &io.event, "ROW")?;
        self.fire_statement_triggers(&io.view, "BEFORE", &io.event)?;
        let record = |values: Vec<Bson>| Record {
            columns: view_cols.clone(),
            values,
        };
        let mut count = 0usize;
        for row in rows {
            let row: Vec<Bson> = row.into_iter().map(|v| v.unwrap_or(Bson::Null)).collect();
            let (new, old) = match io.event.as_str() {
                "INSERT" => {
                    let mut values = vec![Bson::Null; width];
                    for (i, v) in row.into_iter().enumerate() {
                        let slot = match io.columns.get(i) {
                            Some(name) => view_cols
                                .iter()
                                .position(|(c, _)| c == name)
                                .ok_or_else(|| {
                                    user_error(
                                        "42703",
                                        format!(
                                            "column \"{name}\" of relation \"{}\" does not exist",
                                            io.view
                                        ),
                                    )
                                })?,
                            None if io.columns.is_empty() && i < width => i,
                            None => {
                                return Err(user_error(
                                    "42601",
                                    "INSERT has more expressions than target columns".into(),
                                ))
                            }
                        };
                        values[slot] = v;
                    }
                    (Some(record(values)), None)
                }
                "UPDATE" => {
                    let old: Vec<Bson> = row[..width.min(row.len())].to_vec();
                    let mut new = old.clone();
                    for (name, v) in io.columns.iter().zip(row.iter().skip(width)) {
                        let slot =
                            view_cols
                                .iter()
                                .position(|(c, _)| c == name)
                                .ok_or_else(|| {
                                    user_error(
                                        "42703",
                                        format!(
                                            "column \"{name}\" of relation \"{}\" does not exist",
                                            io.view
                                        ),
                                    )
                                })?;
                        new[slot] = v.clone();
                    }
                    (Some(record(new)), Some(record(old)))
                }
                _ => (None, Some(record(row))),
            };
            let mut kept = true;
            for trg in &triggers {
                if self
                    .run_trigger(trg, &io.event, new.clone(), old.clone())?
                    .is_none()
                {
                    kept = false;
                    break;
                }
            }
            if kept {
                count += 1;
            }
        }
        self.fire_statement_triggers(&io.view, "AFTER", &io.event)?;
        let tag = match io.event.as_str() {
            "INSERT" => Tag::new("INSERT").with_oid(0).with_rows(count),
            other => Tag::new(other).with_rows(count),
        };
        Ok(vec![Response::Execution(tag)])
    }

    /// An AFTER ROW trigger's event: run now, or -- a constraint trigger
    /// whose constraint is deferred -- queued for COMMIT.
    fn fire_after_row(
        &self,
        trg: &Document,
        op: &str,
        new: Option<Record>,
        old: Option<Record>,
    ) -> PgWireResult<()> {
        if trg.get_bool("constraint").unwrap_or(false)
            && self.deferred_now(
                trg.get_str("name").unwrap_or_default(),
                trg.get_bool("deferrable").unwrap_or(false),
                trg.get_bool("initially_deferred").unwrap_or(false),
            )
        {
            self.deferred_triggers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(DeferredTrigger {
                    trg: trg.clone(),
                    op: op.to_string(),
                    new,
                    old,
                });
            return Ok(());
        }
        self.run_trigger(trg, op, new, old).map(|_| ())
    }

    pub(crate) fn run_deferred_trigger(&self, event: DeferredTrigger) -> PgWireResult<()> {
        self.run_trigger(&event.trg, &event.op, event.new, event.old)
            .map(|_| ())
    }

    /// `SET CONSTRAINTS`: the named (or ALL) deferrable constraints' mode for
    /// the rest of the block. IMMEDIATE also runs what is already queued for
    /// them, as PostgreSQL does. Outside a block it only warns.
    pub(crate) fn set_constraints(&self, names: &[String], deferred: bool) -> PgWireResult<()> {
        if !self
            .in_transaction
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            self.warning(
                "25P01",
                "SET CONSTRAINTS can only be used in transaction blocks".into(),
            );
            return Ok(());
        }
        // Every named constraint must exist and be deferrable.
        let mut known: Vec<(String, bool)> = Vec::new();
        for def in self.all_table_defs()? {
            for fk in &def.foreign_keys {
                known.push((fk.name.clone(), fk.deferrable));
            }
            for u in &def.unique_constraints {
                known.push((u.name.clone(), u.deferrable));
            }
        }
        for t in self.trigger_docs()? {
            if t.get_bool("constraint").unwrap_or(false) {
                known.push((
                    t.get_str("name").unwrap_or_default().to_string(),
                    t.get_bool("deferrable").unwrap_or(false),
                ));
            }
        }
        for name in names {
            match known.iter().find(|(n, _)| n == name) {
                None => {
                    return Err(user_error(
                        "42704",
                        format!("constraint \"{name}\" does not exist"),
                    ))
                }
                Some((_, false)) => {
                    return Err(user_error(
                        "42809",
                        format!("constraint \"{name}\" is not deferrable"),
                    ))
                }
                _ => {}
            }
        }
        {
            let mut modes = self
                .constraint_modes
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if names.is_empty() {
                modes.0 = Some(deferred);
                modes.1.clear();
            } else {
                for n in names {
                    modes.1.insert(n.clone(), deferred);
                }
            }
        }
        // This statement already runs inside the block, so the queued checks
        // run here directly (wrapping them in the block again would wait on
        // the block's own lock).
        if !deferred {
            self.run_deferred((!names.is_empty()).then_some(names))?;
        }
        Ok(())
    }

    fn note_transition(&self, table: &str, new: Vec<Document>, old: Vec<Document>) {
        self.transition_rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(table.to_string(), (new, old));
    }

    /// Run `f` with a trigger's `REFERENCING` transition tables in place:
    /// each is a temporary table shaped like the trigger's table, holding
    /// the statement's new (or old) rows, and gone when `f` returns.
    fn with_transition_tables<T>(
        &self,
        trg: &Document,
        f: impl FnOnce() -> PgWireResult<T>,
    ) -> PgWireResult<T> {
        let names: Vec<(String, bool)> = [("transition_new", true), ("transition_old", false)]
            .iter()
            .filter_map(|(k, is_new)| trg.get_str(k).ok().map(|n| (n.to_string(), *is_new)))
            .collect();
        if names.is_empty() {
            return f();
        }
        let q = secantus_pgplan::scalar::quote_identifier;
        let table = trg.get_str("table").unwrap_or_default().to_string();
        let def = self
            .lookup(&table)
            .ok_or_else(|| user_error("42P01", format!("relation \"{table}\" does not exist")))?;
        let (new, old) = self
            .transition_rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&table)
            .cloned()
            .unwrap_or_default();
        let mut made = Vec::new();
        let mut setup = || -> PgWireResult<()> {
            for (name, is_new) in &names {
                self.internal_sql(&format!(
                    "CREATE TEMP TABLE {} (LIKE {})",
                    q(name),
                    q(&table)
                ))?;
                made.push(name.clone());
                let tdef = self.lookup(name).ok_or_else(|| {
                    user_error("42P01", format!("relation \"{name}\" does not exist"))
                })?;
                let docs = if *is_new { &new } else { &old };
                self.insert_shaped(name, Self::reshape_rows(&def, &tdef, docs)?)?;
            }
            Ok(())
        };
        let out = setup().and_then(|()| f());
        // The tables go whatever happened; a failure to drop one is an
        // error of its own, reported when the trigger itself succeeded.
        let mut cleanup = Ok(());
        for name in made {
            if let Err(e) = self.internal_sql(&format!("DROP TABLE {}", q(&name))) {
                cleanup = Err(e);
            }
        }
        let value = out?;
        cleanup?;
        Ok(value)
    }
}
