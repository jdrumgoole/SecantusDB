//! PL/pgSQL `OPEN cur FOR query` / `OPEN cur FOR EXECUTE text` / `OPEN
//! bound_cursor`: a portal the caller can `FETCH` from once the function has
//! returned its name -- how a function hands back a `refcursor` (pgjdbc's
//! `RefCursorTest`). The portal is named by the variable's value, or
//! `<unnamed portal N>` when it is NULL, and the variable takes that name.
//!
//! libpg_query's PL/pgSQL grammar types a function PARAMETER as `unknown`,
//! so `OPEN p FOR ...` over a `refcursor` parameter is refused at parse time
//! with "variable must be of type cursor or refcursor". `cursor_param_rewrite`
//! routes such an OPEN through a declared local instead.

use super::*;

/// The cursor variable a cursor statement names (`curvar`). libpg_query
/// leaves a zero field out, so a missing one is datum 0.
fn cursor_var(body: &Value) -> usize {
    body.get("curvar")
        .and_then(Value::as_u64)
        .map_or(0, |n| n as usize)
}

impl Interp<'_> {
    /// The portal name a cursor variable asks for: its value, or `None`
    /// when it is NULL (the portal is then `<unnamed portal N>`). A name a
    /// portal already has is 42P03, before anything else runs
    /// (`exec_stmt_open` and `exec_stmt_forc` look it up with
    /// `SPI_cursor_find` first).
    fn wanted_portal(&self, var: usize) -> Result<Option<String>, PlError> {
        let wanted = match self.datums.get(var) {
            Some(Datum::Var { value, .. }) => match value {
                Bson::Null => None,
                Bson::String(s) => Some(s.clone()),
                other => Some(secantus_pgplan::value_text(other)),
            },
            _ => None,
        };
        if let Some(name) = &wanted {
            if self.host.cursor_exists(name) {
                return Err(PlError::new(
                    "42P03",
                    format!("cursor \"{name}\" already in use"),
                ));
            }
        }
        Ok(wanted)
    }

    /// Whether a cursor's declared options leave it scrollable: unless NO
    /// SCROLL (CURSOR_OPT_NO_SCROLL) the rows are kept, so backward
    /// fetches work, and pg_cursors says so.
    fn scrollable(options: u64) -> bool {
        options & 0x0004 == 0
    }

    /// A bound cursor's query, bound over its arguments' values: the
    /// arguments are variables of a level of their own, labelled with the
    /// cursor's name, that its query was compiled in (`decl_cursor_args`).
    fn bind_bound_cursor(
        &mut self,
        var: usize,
        body: &Value,
        query: &str,
    ) -> Result<(String, Vec<Bson>, Vec<String>), PlError> {
        let depth = self.active.len();
        if let Some(level) = self.cursor_arguments(var, body)? {
            self.active.push(level);
        }
        let bound = self.bind(query);
        self.active.truncate(depth);
        bound
    }

    /// A dynamic statement's `USING` values (`exec_eval_using_params`),
    /// each evaluated once, in order, when the statement runs: `$1` is the
    /// first. Their types are the values' own.
    pub(super) fn using_values(&self, body: &Value) -> Result<Vec<Bson>, PlError> {
        body.get("params")
            .and_then(Value::as_array)
            .map(|ps| ps.iter().map(|p| self.eval(p)).collect())
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    pub(super) fn open_cursor(&mut self, body: &Value) -> Result<Flow, PlError> {
        let var = cursor_var(body);
        // A bound cursor declares its options (`c NO SCROLL CURSOR FOR ...`).
        let options = body
            .get("cursor_options")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            | self.cursor_options.get(&var).copied().unwrap_or(0);
        let wanted = self.wanted_portal(var)?;
        let (statement, (sql, params, types)) =
            if let Some(q) = body.get("query").and_then(|q| expr_query(q)) {
                (q.0.to_string(), self.bind(q.0)?)
            } else if let Some(d) = body.get("dynquery") {
                let text = self.dynamic_sql(d)?;
                let using = self.using_values(body)?;
                (text.clone(), (text, using, Vec::new()))
            } else if let Some(q) = self.cursor_exprs.get(&var).cloned() {
                let bound = self.bind_bound_cursor(var, body, &q)?;
                (q, bound)
            } else {
                return Err(PlError::unsupported("this OPEN form"));
            };
        let name = self.host.open_cursor(
            wanted.as_deref(),
            &statement,
            &sql,
            &params,
            &types,
            Self::scrollable(options),
        )?;
        self.set_var(var, Bson::String(name))?;
        Ok(Flow::Next)
    }

    /// `FOR rec IN cur [(args)] LOOP ... END LOOP` (`exec_stmt_forc`): the
    /// bound cursor is opened as an OPEN opens it -- its name checked, its
    /// arguments assigned, the portal named by the variable or, when that
    /// is NULL, `<unnamed portal N>`, which the variable holds while the
    /// loop runs -- and the loop walks the portal (`exec_for_query`). The
    /// portal is closed when the loop ends, by EXIT or RETURN too, and a
    /// variable that was NULL is NULL again.
    ///
    /// The loop reads the portal a row at a time, so a FETCH or MOVE on the
    /// cursor in the body moves the loop with it, as it does on PostgreSQL.
    pub(super) fn for_cursor(
        &mut self,
        body: &Value,
        label: Option<&str>,
    ) -> Result<Flow, PlError> {
        let var = cursor_var(body);
        let target = body
            .get("var")
            .and_then(|v| v.as_object())
            .and_then(|o| o.values().next())
            .and_then(|v| v.get("dno"))
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .ok_or_else(|| PlError::new("XX000", "a cursor FOR loop without a loop variable"))?;
        let wanted = self.wanted_portal(var)?;
        let query = self
            .cursor_exprs
            .get(&var)
            .cloned()
            .ok_or_else(|| PlError::new("XX000", "a cursor FOR loop over an unbound cursor"))?;
        let (sql, params, types) = self.bind_bound_cursor(var, body, &query)?;
        let options = self.cursor_options.get(&var).copied().unwrap_or(0);
        let portal = self.host.open_cursor(
            wanted.as_deref(),
            &query,
            &sql,
            &params,
            &types,
            Self::scrollable(options),
        )?;
        if wanted.is_none() {
            self.set_var(var, Bson::String(portal.clone()))?;
        }
        // The loop's own level: its label qualifies the loop variable.
        let depth = self.active.len();
        let name = match self.datums.get(target) {
            Some(Datum::Rec { name, .. } | Datum::Var { name, .. }) => name.clone(),
            _ => String::new(),
        };
        self.active.push(block_scope::Scope {
            label: label.map(str::to_string),
            vars: vec![(name, target)],
            ..Default::default()
        });
        let walked = self.walk_pinned(&portal, target, body, label);
        self.active.truncate(depth);
        let flow = walked?;
        if wanted.is_none() {
            self.set_var(var, Bson::Null)?;
        }
        Ok(flow)
    }

    /// `FOR target IN query LOOP` / `FOR target IN EXECUTE text LOOP`
    /// (`exec_stmt_fors` / `exec_stmt_dynfors`): the query is opened as an
    /// unnamed portal (`exec_run_select` / `SPI_cursor_parse_open`), so
    /// `pg_cursors` lists `<unnamed portal N>` while the loop runs, and
    /// `exec_for_query` walks it pinned, as it walks a cursor FOR loop's
    /// (RP-828). The portal is closed when the loop ends.
    pub(super) fn for_query(
        &mut self,
        body: &Value,
        dynamic: bool,
        label: Option<&str>,
    ) -> Result<Flow, PlError> {
        let query = body.get("query").unwrap_or(&Value::Null);
        let (statement, sql, params, types) = if dynamic {
            let sql = self.dynamic_sql(query)?;
            let using = self.using_values(body)?;
            (sql.clone(), sql, using, Vec::new())
        } else {
            let (text, _) =
                expr_query(query).ok_or_else(|| PlError::new("XX000", "a FOR without a query"))?;
            let (sql, params, types) = self.bind(text)?;
            (text.to_string(), sql, params, types)
        };
        let portal = self
            .host
            .open_cursor(None, &statement, &sql, &params, &types, false)?;
        // A list of scalar targets is an anonymous row, pushed for the loop.
        let base = self.datums.len();
        let walked = self
            .variable_datum(body, "var")
            .and_then(|target| self.walk_pinned(&portal, target, body, label));
        self.datums.truncate(base);
        walked
    }

    /// `exec_for_query` over an open portal: pinned while it is walked, so
    /// the body cannot close it and a procedure's COMMIT or ROLLBACK holds
    /// it, then closed.
    fn walk_pinned(
        &mut self,
        portal: &str,
        target: usize,
        body: &Value,
        label: Option<&str>,
    ) -> Result<Flow, PlError> {
        self.host.pin_cursor(portal, true);
        let walked = self.walk_portal(portal, target, body, label);
        self.host.pin_cursor(portal, false);
        match walked {
            Ok(flow) => {
                self.host.close_cursor(portal)?;
                Ok(flow)
            }
            // PostgreSQL leaves the portal to the abort of the (sub)
            // transaction the error ends, which drops it; it is dropped
            // here, and the body's error is the one reported.
            Err(e) => {
                if self.host.cursor_exists(portal) {
                    self.host.close_cursor(portal)?;
                }
                Err(e)
            }
        }
    }

    /// `exec_for_query`: each row of the portal into the loop variable, then
    /// the body. With no row at all the variable is set to NULL. FOUND is
    /// set when the loop ends, by whether a row was read; the body sees
    /// the FOUND it was entered with.
    fn walk_portal(
        &mut self,
        portal: &str,
        target: usize,
        body: &Value,
        label: Option<&str>,
    ) -> Result<Flow, PlError> {
        use secantus_pgplan::FetchDirection as Fd;
        let stmts = body.get("body").unwrap_or(&Value::Null);
        let mut found = false;
        let flow = loop {
            let (_, out) = self.host.fetch_cursor(portal, Fd::Forward, 1, false)?;
            let Some(row) = out.rows.first() else {
                if !found {
                    self.assign_row(target, &out, None, &out.domains)?;
                }
                break Flow::Next;
            };
            found = true;
            self.assign_row(target, &out, Some(row), &out.domains)?;
            match self.stmts(stmts)? {
                Flow::Exit(l) if Self::label_matches(&l, label) => break Flow::Next,
                Flow::Continue(l) if Self::label_matches(&l, label) => {}
                Flow::Next => {}
                other => break other,
            }
        };
        self.set_found(found);
        Ok(flow)
    }

    /// Assign a bound cursor's arguments, and give back the level its query
    /// reads them in; `None` for a cursor declared with none.
    ///
    /// `exec_stmt_open` runs the OPEN's `argquery` (the argument list
    /// `read_cursor_args` builds, in declared order) as a `SELECT ... INTO`
    /// the cursor's argument row, through `exec_stmt_execsql`: each value
    /// moves into its variable by the assignment rule (a domain's CHECK, a
    /// length), and FOUND and ROW_COUNT are set by it as by any SELECT
    /// INTO. An error raised running it carries its `SQL expression` frame.
    fn cursor_arguments(
        &mut self,
        var: usize,
        body: &Value,
    ) -> Result<Option<block_scope::Scope>, PlError> {
        let Some(&row) = self.cursor_argrows.get(&var) else {
            return Ok(None);
        };
        let Some(Datum::Row { fields }) = self.datums.get(row) else {
            return Err(PlError::new(
                "XX000",
                "a cursor's argument row is not a row",
            ));
        };
        let fields = fields.clone();
        let (text, mode) = body
            .get("argquery")
            .and_then(|q| expr_query(q))
            .ok_or_else(|| PlError::new("XX000", "an OPEN of a cursor with arguments has none"))?;
        let frame = || bind_frame(text, mode);
        let raw = format!("SELECT {text}");
        let (sql, params, types) = self.bind(&raw).map_err(|e| e.with_sql_frame(frame))?;
        let out = self.host.query(&sql, &params, &types).map_err(|e| {
            let framed = !literal_input_error(&raw, &e);
            e.with_sql_frame(|| if framed { frame() } else { None })
        })?;
        // Not STRICT: a second row is not read (`tcount` 1).
        let domains = self.host.output_domains(&sql, &types);
        let first = out.rows.first().map(|r| r.as_slice());
        self.assign_row(row, &out, first, &domains)?;
        self.set_found(first.is_some());
        self.row_count = u64::from(first.is_some());
        let vars = fields
            .iter()
            .filter_map(|&d| match self.datums.get(d) {
                Some(Datum::Var { name, .. }) => Some((name.clone(), d)),
                _ => None,
            })
            .collect();
        let label = match self.datums.get(var) {
            Some(Datum::Var { name, .. }) => Some(name.clone()),
            _ => None,
        };
        Ok(Some(block_scope::Scope {
            label,
            vars,
            ..Default::default()
        }))
    }

    /// The portal a cursor variable names: its value (22004 when NULL, as
    /// `exec_stmt_fetch` refuses it).
    fn portal_of(&self, body: &Value) -> Result<String, PlError> {
        let var = body
            .get("curvar")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .ok_or_else(|| PlError::new("XX000", "a cursor statement without a variable"))?;
        match self.datums.get(var) {
            Some(Datum::Var { value, name, .. }) => match value {
                Bson::Null => Err(PlError::new(
                    "22004",
                    format!("cursor variable \"{name}\" is null"),
                )),
                Bson::String(s) => Ok(s.clone()),
                other => Ok(secantus_pgplan::value_text(other)),
            },
            _ => Err(PlError::new(
                "XX000",
                "a cursor statement without a variable",
            )),
        }
    }

    /// `FETCH [direction] cur INTO target` / `MOVE [direction] cur`: one
    /// step of the portal, as the SQL statement takes it. A FETCH fills its
    /// target with the row the cursor then sits on (NULLs past either end)
    /// and sets FOUND by it; a MOVE sets FOUND when it moved over a row.
    pub(super) fn fetch_cursor(&mut self, body: &Value) -> Result<Flow, PlError> {
        use secantus_pgplan::FetchDirection as Fd;
        let is_move = body
            .get("is_move")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !is_move
            && body
                .get("returns_multiple_rows")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            return Err(PlError::new(
                "42601",
                "FETCH statement cannot return multiple rows",
            ));
        }
        let portal = self.portal_of(body)?;
        let direction = match body.get("direction").and_then(Value::as_u64) {
            Some(1) => Fd::Backward,
            Some(2) => Fd::Absolute,
            Some(3) => Fd::Relative,
            _ => Fd::Forward,
        };
        let count = match body.get("expr") {
            Some(e) => match self.eval(e)? {
                Bson::Int32(n) => i64::from(n),
                Bson::Int64(n) => n,
                Bson::Double(f) => f as i64,
                Bson::Null => 0,
                other => secantus_pgplan::value_text(&other).parse().unwrap_or(0),
            },
            None => body.get("how_many").and_then(Value::as_i64).unwrap_or(0),
        };
        let (moved, out) = self.host.fetch_cursor(&portal, direction, count, is_move)?;
        if is_move {
            self.set_found(moved > 0);
            self.row_count = moved;
            return Ok(Flow::Next);
        }
        let first = out.rows.first().map(|r| r.as_slice());
        // The portal's row type names each column's domain, as its tuple
        // descriptor does: a record field is of the domain, and a value of
        // it moves in with no coercion (RP-807).
        self.assign_into(body, "target", &out, first, &out.domains)?;
        self.set_found(first.is_some());
        self.row_count = u64::from(first.is_some());
        Ok(Flow::Next)
    }

    /// `CLOSE cur`.
    pub(super) fn close_cursor(&mut self, body: &Value) -> Result<Flow, PlError> {
        let portal = self.portal_of(body)?;
        self.host.close_cursor(&portal)?;
        Ok(Flow::Next)
    }
}

/// A bound cursor variable's default: its own name, as a `refcursor`
/// literal. PostgreSQL 15's grammar gives every `c CURSOR FOR ...` this
/// `default_val` (`curname_def` in `pl_gram.y`'s `decl_statement`), so the
/// variable reads `c` before any OPEN and its portal is named `c`; one set
/// to NULL opens `<unnamed portal N>`. libpg_query parses with a later
/// grammar, which dropped it, so it is made here. `None` for any other
/// variable.
pub(super) fn cursor_name_default(var: &Value) -> Option<Value> {
    var.get("cursor_explicit_expr")?;
    let name = var.get("refname")?.as_str()?;
    // As `curname_def` writes it: quotes doubled, and a backslash doubled
    // under E'' so standard_conforming_strings cannot change its reading.
    let mut text = String::with_capacity(name.len() + 24);
    if name.contains('\\') {
        text.push('E');
    }
    text.push('\'');
    for ch in name.chars() {
        if ch == '\'' || ch == '\\' {
            text.push(ch);
        }
        text.push(ch);
    }
    text.push_str("'::pg_catalog.refcursor");
    // RAW_PARSE_PLPGSQL_EXPR, as `curname_def->parseMode`.
    Some(serde_json::json!({ "PLpgSQL_expr": { "query": text, "parseMode": 2 } }))
}

/// The bound cursors' declared options, by datum number.
pub(super) fn cursor_options(f: &Value) -> HashMap<usize, u64> {
    f.get("datums")
        .and_then(Value::as_array)
        .map(|ds| {
            ds.iter()
                .enumerate()
                .filter_map(|(i, d)| {
                    let o = d.get("PLpgSQL_var")?.get("cursor_options")?.as_u64()?;
                    Some((i, o))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The bound cursors' argument rows (`cursor_explicit_argrow`), by datum
/// number; a cursor with no arguments has none (-1).
pub(super) fn cursor_argrows(f: &Value) -> HashMap<usize, usize> {
    f.get("datums")
        .and_then(Value::as_array)
        .map(|ds| {
            ds.iter()
                .enumerate()
                .filter_map(|(i, d)| {
                    let v = d.get("PLpgSQL_var")?;
                    v.get("cursor_explicit_expr")?;
                    let row = v.get("cursor_explicit_argrow")?.as_u64()?;
                    Some((i, usize::try_from(row).ok()?))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The bound cursors' queries (`c CURSOR FOR SELECT ...`), by datum number.
pub(super) fn cursor_exprs(f: &Value) -> HashMap<usize, String> {
    f.get("datums")
        .and_then(Value::as_array)
        .map(|ds| {
            ds.iter()
                .enumerate()
                .filter_map(|(i, d)| {
                    let q = d
                        .get("PLpgSQL_var")?
                        .get("cursor_explicit_expr")
                        .and_then(|e| expr_query(e))?;
                    Some((i, q.0.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `variable "x" must be of type cursor or refcursor` names a PARAMETER the
/// grammar could not type. Rewrite each `OPEN x` to open a declared local
/// `__refcur_x` and copy its name back: `__refcur_x := x; OPEN __refcur_x
/// ...; x := __refcur_x;`. `None` when the text holds nothing to rewrite.
pub(super) fn cursor_param_rewrite(sql: &str, var: &str) -> Option<String> {
    let lower = sql.to_ascii_lowercase();
    let local = format!("__refcur_{var}");
    let word_at = |i: usize, w: &str| {
        lower[i..].starts_with(w)
            && (i == 0 || !is_ident(lower.as_bytes()[i - 1]))
            && lower
                .as_bytes()
                .get(i + w.len())
                .is_none_or(|b| !is_ident(*b))
    };
    // The body begins at its first BEGIN or DECLARE.
    let start = (0..lower.len()).find(|&i| word_at(i, "declare") || word_at(i, "begin"))?;
    let mut out = String::with_capacity(sql.len() + 64);
    out.push_str(&sql[..start]);
    if word_at(start, "declare") {
        out.push_str("DECLARE ");
        out.push_str(&local);
        out.push_str(" refcursor;");
    } else {
        out.push_str("DECLARE ");
        out.push_str(&local);
        out.push_str(" refcursor; ");
    }
    let mut i = if word_at(start, "declare") {
        start + "declare".len()
    } else {
        start
    };
    let mut changed = false;
    let bytes = sql.as_bytes();
    while i < sql.len() {
        let open = word_at(i, "open") && {
            let mut j = i + 4;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            word_at(j, &var.to_ascii_lowercase()) && {
                i = j + var.len();
                true
            }
        };
        if !open {
            out.push(bytes[i] as char);
            if !bytes[i].is_ascii() {
                // Copy the whole multi-byte character.
                let ch = sql[i..].chars().next().expect("char");
                out.pop();
                out.push(ch);
                i += ch.len_utf8();
                continue;
            }
            i += 1;
            continue;
        }
        // The statement runs to the next `;` outside quotes.
        let mut j = i;
        let mut quote: Option<u8> = None;
        while j < bytes.len() {
            match (quote, bytes[j]) {
                (None, b'\'' | b'"') => quote = Some(bytes[j]),
                (Some(q), c) if c == q => quote = None,
                (None, b';') => break,
                _ => {}
            }
            j += 1;
        }
        out.push_str(&format!("{local} := {var}; OPEN {local}"));
        out.push_str(&sql[i..j.min(sql.len())]);
        out.push_str(&format!("; {var} := {local}"));
        i = j;
        changed = true;
    }
    changed.then_some(out)
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bound cursor variable's default is its own name as a `refcursor`
    /// literal, written as PostgreSQL 15's `curname_def` writes it; a plain
    /// `refcursor` has none (RP-817).
    #[test]
    fn a_bound_cursors_name_default() {
        let f = super::super::parsed(
            "create function f() returns int language plpgsql as $$ declare \
             c cursor for select 1; \"o'k\" cursor for select 2; \"b\\s\" cursor for select 3; \
             e refcursor; begin return 1; end $$",
        )
        .unwrap();
        let defaults: Vec<(String, Option<String>)> = f["datums"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|d| d.get("PLpgSQL_var"))
            .map(|v| {
                let q = cursor_name_default(v)
                    .and_then(|e| expr_query(&e).map(|(q, m)| (q.to_string(), m)));
                if let Some((_, mode)) = &q {
                    assert_eq!(*mode, 2);
                }
                (v["refname"].as_str().unwrap().to_string(), q.map(|q| q.0))
            })
            .filter(|(n, _)| n != "found")
            .collect();
        assert_eq!(
            defaults,
            [
                ("c".into(), Some("'c'::pg_catalog.refcursor".into())),
                ("o'k".into(), Some("'o''k'::pg_catalog.refcursor".into())),
                (
                    "b\\s".into(),
                    Some("E'b\\\\s'::pg_catalog.refcursor".into())
                ),
                ("e".into(), None),
            ]
        );
    }

    /// A bound cursor with arguments names its argument row, whose fields
    /// are the argument variables in declared order; a cursor with none,
    /// and a plain `refcursor`, have none (RP-809).
    #[test]
    fn a_bound_cursors_argument_row() {
        let f = super::super::parsed(
            "create function f() returns int language plpgsql as $$ declare \
             c cursor (k int, s text) for select s || k; d cursor for select 1; \
             e refcursor; begin return 1; end $$",
        )
        .unwrap();
        let rows = cursor_argrows(&f);
        let names: Vec<Option<String>> = f["datums"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| {
                d.get("PLpgSQL_var")
                    .and_then(|v| v["refname"].as_str())
                    .map(str::to_string)
            })
            .collect();
        let at = |n: &str| names.iter().position(|x| x.as_deref() == Some(n)).unwrap();
        assert_eq!(rows.len(), 1);
        let row = rows[&at("c")];
        let fields: Vec<&str> = f["datums"][row]["PLpgSQL_row"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| {
                names[x["varno"].as_u64().unwrap() as usize]
                    .as_deref()
                    .unwrap()
            })
            .collect();
        assert_eq!(fields, ["k", "s"]);
    }
}
