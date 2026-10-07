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

impl Interp<'_> {
    pub(super) fn open_cursor(&mut self, body: &Value) -> Result<Flow, PlError> {
        let var = body
            .get("curvar")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .ok_or_else(|| PlError::new("XX000", "OPEN without a cursor variable"))?;
        // Scrollable unless NO SCROLL (CURSOR_OPT_NO_SCROLL): the rows are
        // kept, so backward fetches work, and pg_cursors says so.
        let scroll = !body
            .get("cursor_options")
            .and_then(Value::as_u64)
            .is_some_and(|o| o & 0x0004 != 0);
        let (statement, (sql, params, types)) =
            if let Some(q) = body.get("query").and_then(|q| expr_query(q)) {
                (q.0.to_string(), self.bind(q.0)?)
            } else if let Some(d) = body.get("dynquery") {
                let text = self.dynamic_sql(d)?;
                (text.clone(), (text, Vec::new(), Vec::new()))
            } else if let Some(q) = self.cursor_exprs.get(&var).cloned() {
                let bound = self.bind(&q)?;
                (q, bound)
            } else {
                return Err(PlError::unsupported("this OPEN form"));
            };
        let (current, var_name) = match self.datums.get(var) {
            Some(Datum::Var { value, name, .. }) => (value.clone(), name.clone()),
            _ => (Bson::Null, String::new()),
        };
        // A bound cursor variable starts out holding its own name.
        let current = match current {
            Bson::Null if self.cursor_exprs.contains_key(&var) => Bson::String(var_name),
            other => other,
        };
        let wanted = match &current {
            Bson::Null => None,
            Bson::String(s) => Some(s.clone()),
            other => Some(secantus_pgplan::value_text(other)),
        };
        let name =
            self.host
                .open_cursor(wanted.as_deref(), &statement, &sql, &params, &types, scroll)?;
        self.set_var(var, Bson::String(name))?;
        Ok(Flow::Next)
    }
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
