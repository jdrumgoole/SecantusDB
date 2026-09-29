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
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use secantus_pgcatalog::TableDef;
use secantus_pgplan::TriggerDef;

use crate::plpgsql_fn::{self, Record, TriggerData};
use crate::{PgHandler, PlHost};

pub(crate) const TRIGGER_COLLECTION: &str = "__sql_triggers__";

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

    pub(crate) fn create_trigger(&self, def: TriggerDef) -> PgWireResult<()> {
        if self.lookup(&def.table).is_none() {
            return Err(user_error(
                "42P01",
                format!("relation \"{}\" does not exist", def.table),
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
        if let Some(when) = &def.when {
            doc.insert("when", when.clone());
        }
        self.insert_type_doc(TRIGGER_COLLECTION, &key, doc)
    }

    pub(crate) fn drop_trigger(
        &self,
        name: &str,
        table: &str,
        if_exists: bool,
    ) -> PgWireResult<()> {
        if self.lookup(table).is_none() {
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
        Ok(())
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
        let outcome = self.with_call_depth(|| {
            plpgsql_fn::run(
                &crate::plpgsql_create_sql(&doc),
                plpgsql_fn::Invocation {
                    args: &[],
                    trigger: Some(data),
                    returns_set: false,
                },
                &PlHost { h: self },
            )
            .map_err(crate::wire_pl_error)
        })?;
        Ok(match outcome {
            plpgsql_fn::Outcome::Record(r) => r,
            _ => None,
        })
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
        let triggers = self.triggers_for(&def.name, "AFTER", "INSERT", "ROW")?;
        for row in rows {
            if triggers.is_empty() {
                break;
            }
            let new = Some(self.row_record(def, row)?);
            for trg in &triggers {
                if self.when_holds(trg, "INSERT", &new, &None)? {
                    self.run_trigger(trg, "INSERT", new.clone(), None)?;
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
                    self.run_trigger(trg, "UPDATE", new.clone(), old.clone())?;
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
        let triggers = self.triggers_for(&def.name, "AFTER", "DELETE", "ROW")?;
        if triggers.is_empty() {
            return Ok(());
        }
        for row in rows {
            let old = Some(self.row_record(def, row)?);
            for trg in &triggers {
                if self.when_holds(trg, "DELETE", &None, &old)? {
                    self.run_trigger(trg, "DELETE", None, old.clone())?;
                }
            }
        }
        Ok(())
    }
}
