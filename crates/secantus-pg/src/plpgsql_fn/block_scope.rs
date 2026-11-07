//! A block's label qualifies the variables its DECLARE section declares, and
//! a FOR loop's label its loop variable: `<<blk>> declare x int; begin
//! return blk.x; end`. PostgreSQL resolves such a name at compile time
//! against `plpgsql_ns`, the namespace the grammar builds as it reads the
//! body (`plpgsql_ns_lookup`: from the innermost level outwards, a level
//! whose label is `blk` is searched for `x`).
//!
//! libpg_query's JSON keeps neither the namespace nor a block's
//! `initvarnos`, so the levels are rebuilt here, once per function, from two
//! things it does keep: the datums in the order the compiler made them, and
//! the body's text. The k-th `BEGIN` of the body is the k-th block of the
//! statement tree in source order, and the names its DECLARE section
//! declares are read off the text. Each name is then the next datum of that
//! name in creation order, walking the tree as `pl_gram.y` reads it: a
//! block's declarations before its body, a FOR loop's variable before its
//! body, an EXCEPTION section's `sqlstate` / `sqlerrm` after the body.
//!
//! The result is stored in the parsed function: `__scopes`, one entry per
//! block (`label`, `vars`: `[name, datum]` pairs, `parent`, and `handler`:
//! the `sqlstate` / `sqlerrm` its EXCEPTION section declares), and a
//! `__scope` index on each block statement, so a copy of the tree keeps it.
//!
//! The levels decide a bare name too: `x` is the variable of the innermost
//! level being run that declares it, then the function's own (`Interp::named`),
//! so a block's variable goes out of scope when the block ends.

use super::*;

/// One namespace level while the function runs: a block's (or a FOR loop's)
/// label and the variables declared in it, by datum number.
#[derive(Debug, Clone, Default)]
pub(super) struct Scope {
    pub(super) label: Option<String>,
    pub(super) vars: Vec<(String, usize)>,
    pub(super) parent: Option<usize>,
    /// `sqlstate` and `sqlerrm`, which `pl_gram.y` declares at the block's
    /// EXCEPTION in a level of their own: the one a handler runs in.
    pub(super) handler: Vec<(String, usize)>,
}

/// What one DECLARE section declares, in order: each name, and for an
/// `ALIAS FOR` the name or `$n` it stands for.
#[derive(Debug, Default)]
struct Decls(Vec<(String, Option<String>)>);

/// A word of the body as PL/pgSQL folds it: a quoted identifier as written,
/// anything else in lower case.
fn fold(raw: &str) -> String {
    if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
        raw[1..raw.len() - 1].replace("\"\"", "\"")
    } else {
        raw.to_ascii_lowercase()
    }
}

/// The DECLARE section of each block, by the block's `BEGIN` in source
/// order. `None` when the text does not scan.
fn declarations(body: &str) -> Option<Vec<Decls>> {
    let scan = pg_query::scan(body).ok()?;
    let mut blocks = Vec::new();
    let mut current: Option<Decls> = None;
    // At the start of a declaration: the next word is the name declared.
    let mut at_start = false;
    // The words of the declaration being read, after its name.
    let mut words: Vec<String> = Vec::new();
    for t in &scan.tokens {
        let raw = body.get(t.start as usize..t.end as usize)?;
        let word = fold(raw);
        let quoted = raw.starts_with('"');
        match current.as_mut() {
            None => {
                if !quoted && word == "declare" {
                    current = Some(Decls::default());
                    at_start = true;
                } else if !quoted && word == "begin" {
                    blocks.push(Decls::default());
                }
            }
            Some(d) => {
                if !quoted && word == "begin" {
                    blocks.push(current.take().unwrap_or_default());
                } else if !quoted && word == "declare" || raw == ";" {
                    at_start = true;
                } else if at_start {
                    d.0.push((word, None));
                    words.clear();
                    at_start = false;
                } else {
                    words.push(word);
                    // `name ALIAS FOR target`
                    if let ([a, f, target], Some(last)) = (words.as_slice(), d.0.last_mut()) {
                        if a == "alias" && f == "for" {
                            last.1 = Some(target.clone());
                        }
                    }
                }
            }
        }
    }
    Some(blocks)
}

/// The source of a CREATE FUNCTION: its `AS` string.
fn function_source(create_sql: &str) -> Option<String> {
    use pg_query::protobuf::node::Node as N;
    let p = pg_query::parse(create_sql).ok()?;
    let N::CreateFunctionStmt(c) = p.protobuf.stmts.first()?.stmt.as_ref()?.node.as_ref()? else {
        return None;
    };
    c.options.iter().find_map(|o| {
        let N::DefElem(d) = o.node.as_ref()? else {
            return None;
        };
        if d.defname != "as" {
            return None;
        }
        let N::List(l) = d.arg.as_ref()?.node.as_ref()? else {
            return None;
        };
        match l.items.first()?.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        }
    })
}

/// The name of each datum that enters a namespace (a variable or a record),
/// by number; `None` for the rest (rows, record fields).
fn datum_names(f: &Value) -> Vec<Option<String>> {
    f.get("datums")
        .and_then(Value::as_array)
        .map(|ds| {
            ds.iter()
                .map(|d| {
                    d.get("PLpgSQL_var")
                        .or_else(|| d.get("PLpgSQL_rec"))
                        .and_then(|v| v.get("refname"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

struct Builder<'a> {
    names: Vec<Option<String>>,
    /// The next datum the compiler would have made.
    next: usize,
    decls: std::vec::IntoIter<Decls>,
    scopes: Vec<Scope>,
    /// The datum numbers of the arguments, for `ALIAS FOR $n`.
    args: &'a [usize],
}

impl Builder<'_> {
    /// The next datum named `name`, as the compiler made it.
    fn take(&mut self, name: &str) -> Option<usize> {
        let at = (self.next..self.names.len()).find(|&i| self.names[i].as_deref() == Some(name))?;
        self.next = at + 1;
        Some(at)
    }

    /// What `name` stands for where an ALIAS names it: a variable of this
    /// level declared before it, of an enclosing level, or of the function.
    fn resolve(&self, target: &str, level: &Scope) -> Option<usize> {
        if let Some(n) = target
            .strip_prefix('$')
            .and_then(|d| d.parse::<usize>().ok())
        {
            return n.checked_sub(1).and_then(|i| self.args.get(i)).copied();
        }
        let in_level = |s: &Scope| {
            s.vars
                .iter()
                .rev()
                .find(|(n, _)| n == target)
                .map(|(_, d)| *d)
        };
        if let Some(d) = in_level(level) {
            return Some(d);
        }
        let mut up = level.parent;
        while let Some(k) = up {
            if let Some(d) = in_level(&self.scopes[k]) {
                return Some(d);
            }
            up = self.scopes[k].parent;
        }
        // The function's own level: everything made before the body.
        self.names
            .iter()
            .enumerate()
            .take(self.first_body_datum())
            .rev()
            .find(|(_, n)| n.as_deref() == Some(target))
            .map(|(i, _)| i)
    }

    fn first_body_datum(&self) -> usize {
        self.names
            .iter()
            .position(|n| n.as_deref() == Some("found"))
            .map_or(0, |i| i + 1)
    }

    fn stmts(&mut self, list: Option<&mut Value>, parent: Option<usize>) {
        if let Some(Value::Array(items)) = list {
            for s in items {
                self.stmt(s, parent);
            }
        }
    }

    fn stmt(&mut self, s: &mut Value, parent: Option<usize>) {
        let Some((kind, body)) = s.as_object_mut().and_then(|o| o.iter_mut().next()) else {
            return;
        };
        let kind = kind.clone();
        match kind.as_str() {
            // The block `add_dummy_return` wraps around a body whose outer
            // block has a label or an EXCEPTION section: not in the text,
            // and without a line, it declares nothing.
            "PLpgSQL_stmt_block" if body.get("lineno").is_none() => {
                self.stmts(body.get_mut("body"), parent);
            }
            "PLpgSQL_stmt_block" => {
                let decls = self.decls.next().unwrap_or_default();
                let mut level = Scope {
                    label: body
                        .get("label")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    vars: Vec::new(),
                    parent,
                    handler: Vec::new(),
                };
                for (name, alias) in decls.0 {
                    let datum = match alias {
                        Some(target) => self.resolve(&target, &level),
                        None => self.take(&name),
                    };
                    if let Some(d) = datum {
                        level.vars.push((name, d));
                    }
                }
                let k = self.scopes.len();
                self.scopes.push(level);
                if let Some(o) = body.as_object_mut() {
                    o.insert("__scope".into(), Value::from(k));
                }
                self.stmts(body.get_mut("body"), Some(k));
                if let Some(handlers) = body
                    .get_mut("exceptions")
                    .and_then(|e| e.get_mut("PLpgSQL_exception_block"))
                {
                    // Made at EXCEPTION, after the body; a level of their
                    // own, which the block's label does not reach.
                    for name in ["sqlstate", "sqlerrm"] {
                        if let Some(d) = self.take(name) {
                            self.scopes[k].handler.push((name.to_string(), d));
                        }
                    }
                    if let Some(Value::Array(list)) = handlers.get_mut("exc_list") {
                        for h in list {
                            let action = h
                                .get_mut("PLpgSQL_exception")
                                .and_then(|h| h.get_mut("action"));
                            self.stmts(action, Some(k));
                        }
                    }
                }
            }
            "PLpgSQL_stmt_fori" | "PLpgSQL_stmt_forc" => {
                let var = body
                    .get("var")
                    .and_then(|v| v.get("PLpgSQL_var").or_else(|| v.get("PLpgSQL_rec")))
                    .and_then(|v| v.get("refname"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if let Some(v) = var {
                    self.take(&v);
                }
                self.stmts(body.get_mut("body"), parent);
            }
            "PLpgSQL_stmt_if" => {
                self.stmts(body.get_mut("then_body"), parent);
                if let Some(Value::Array(list)) = body.get_mut("elsif_list") {
                    for e in list {
                        let stmts = e
                            .get_mut("PLpgSQL_if_elsif")
                            .and_then(|e| e.get_mut("stmts"));
                        self.stmts(stmts, parent);
                    }
                }
                self.stmts(body.get_mut("else_body"), parent);
            }
            "PLpgSQL_stmt_case" => {
                if let Some(Value::Array(list)) = body.get_mut("case_when_list") {
                    for w in list {
                        let stmts = w
                            .get_mut("PLpgSQL_case_when")
                            .and_then(|w| w.get_mut("stmts"));
                        self.stmts(stmts, parent);
                    }
                }
                self.stmts(body.get_mut("else_stmts"), parent);
            }
            _ => self.stmts(body.get_mut("body"), parent),
        }
    }
}

/// Rebuild the namespace levels of a parsed function from the CREATE text
/// it was parsed from, and store them in it (see the module comment). A
/// body that does not scan is left without them: no label then qualifies a
/// variable, as before.
pub(super) fn annotate(f: &mut Value, create_sql: &str) {
    let Some(decls) = function_source(create_sql).and_then(|s| declarations(&s)) else {
        return;
    };
    let names = datum_names(f);
    let first = names
        .iter()
        .position(|n| n.as_deref() == Some("found"))
        .map_or(0, |i| i + 1);
    // `$n` is the n-th datum before FOUND that is a variable or a record:
    // the arguments come first.
    let args: Vec<usize> = (0..first.saturating_sub(1))
        .filter(|&i| names[i].is_some())
        .collect();
    let mut b = Builder {
        names,
        next: first,
        decls: decls.into_iter(),
        scopes: Vec::new(),
        args: &args,
    };
    let Some(action) = f.get_mut("action") else {
        return;
    };
    b.stmt(action, None);
    let scopes: Vec<Value> = b
        .scopes
        .iter()
        .map(|s| {
            serde_json::json!({
                "label": s.label,
                "vars": s.vars.iter().map(|(n, d)| serde_json::json!([n, d])).collect::<Vec<_>>(),
                "parent": s.parent,
                "handler": s.handler.iter().map(|(n, d)| serde_json::json!([n, d])).collect::<Vec<_>>(),
            })
        })
        .collect();
    if let Some(o) = f.as_object_mut() {
        o.insert("__scopes".into(), Value::Array(scopes));
    }
}

/// The levels `annotate` stored, by index.
pub(super) fn scopes(f: &Value) -> Vec<Scope> {
    f.get("__scopes")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|s| {
                    let pairs = |key: &str| -> Vec<(String, usize)> {
                        s.get(key)
                            .and_then(Value::as_array)
                            .map(|vs| {
                                vs.iter()
                                    .filter_map(|v| {
                                        Some((
                                            v.get(0)?.as_str()?.to_string(),
                                            v.get(1)?.as_u64()? as usize,
                                        ))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    };
                    Scope {
                        label: s.get("label").and_then(Value::as_str).map(str::to_string),
                        vars: pairs("vars"),
                        parent: s.get("parent").and_then(Value::as_u64).map(|p| p as usize),
                        handler: pairs("handler"),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The block a statement opens, if `annotate` numbered it.
pub(super) fn scope_of(body: &Value) -> Option<usize> {
    body.get("__scope")
        .and_then(Value::as_u64)
        .map(|k| k as usize)
}

impl Interp<'_> {
    /// `label.name`: the datum of `name` in the innermost level labelled
    /// `label` that declares it, as `plpgsql_ns_lookup` searches outwards.
    pub(super) fn labelled(&self, label: &str, name: &str) -> Option<usize> {
        self.active.iter().rev().find_map(|s| {
            if s.label.as_deref() != Some(label) {
                return None;
            }
            s.vars
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, d)| *d)
        })
    }

    /// A bare `name`: the datum of the innermost level being run that
    /// declares it, then of the function's own level (`function_level`), as `plpgsql_ns_lookup` searches outwards
    /// with no qualifier. A variable of a block that has ended is in no
    /// level, and so is not found. Without rebuilt levels (a body that does
    /// not scan) it is the last datum of that name.
    pub(super) fn named(&self, name: &str) -> Option<usize> {
        let is = |d: &Datum| match d {
            Datum::Var { name: n, .. } | Datum::Rec { name: n, .. } => n == name,
            _ => false,
        };
        let (false, Some(end)) = (self.scopes.is_empty(), self.function_level) else {
            return self.datums.iter().rposition(is);
        };
        if let Some(d) = self.active.iter().rev().find_map(|s| {
            s.vars
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, d)| *d)
        }) {
            return Some(d);
        }
        self.datums.get(..end)?.iter().rposition(is)
    }

    /// The level a handler of block `k` runs in, beyond the block's own.
    pub(super) fn handler_level(&self, k: usize) -> Option<Scope> {
        let s = self.scopes.get(k)?;
        (!s.handler.is_empty()).then(|| Scope {
            label: None,
            vars: s.handler.clone(),
            parent: None,
            handler: Vec::new(),
        })
    }

    /// The levels enclosing (and including) block `k`, outermost first: the
    /// namespace a declaration of `k` is read in.
    pub(super) fn chain(&self, k: usize) -> Vec<Scope> {
        let mut out = Vec::new();
        let mut at = Some(k);
        while let Some(i) = at {
            let Some(s) = self.scopes.get(i) else { break };
            out.push(s.clone());
            at = s.parent;
        }
        out.reverse();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_by_block() {
        let d = declarations(
            " <<o>> declare x int := 1; \"Y\" text; c cursor (a int) for select a; \
             begin declare z alias for $1; begin null; end; begin null; end; end ",
        )
        .unwrap();
        let names: Vec<Vec<(String, Option<String>)>> = d.into_iter().map(|d| d.0).collect();
        assert_eq!(
            names,
            vec![
                vec![("x".into(), None), ("Y".into(), None), ("c".into(), None)],
                vec![("z".into(), Some("$1".into()))],
                vec![],
            ]
        );
    }

    /// A level's label and its `(name, datum)` pairs.
    type Level = (Option<String>, Vec<(String, usize)>);

    /// The datums each level names, by the labels, per block in source
    /// order.
    fn levels(create_sql: &str) -> Vec<Level> {
        let f = super::super::parsed(create_sql).unwrap();
        scopes(&f).into_iter().map(|s| (s.label, s.vars)).collect()
    }

    #[test]
    fn wrapped_outer_block_keeps_its_declarations() {
        // A labelled outer block with handlers is wrapped in a block of
        // libpg_query's own (`add_dummy_return`), which declares nothing.
        let l = levels(
            "create function f() returns text language plpgsql as $$ <<o>> declare x int := 4; \
             begin perform 1/0; return '0'; exception when others then return o.x; end $$",
        );
        assert_eq!(l, vec![(Some("o".into()), vec![("x".into(), 1)])]);
    }

    #[test]
    fn a_handler_level_holds_its_blocks_sqlstate_and_sqlerrm() {
        // Three blocks: the outer one (x), a middle one with a handler, and
        // the innermost (y) with its own. Datums: found, x, y, then the
        // innermost EXCEPTION's sqlstate and sqlerrm (3, 4), made before the
        // middle one's (5, 6), as the grammar reads the text.
        let f = super::super::parsed(
            "create function f() returns text language plpgsql as $$ declare x int; \
             begin begin declare y int; begin perform 1/0; exception when others then null; end; \
             exception when others then return sqlstate; end; end $$",
        )
        .unwrap();
        let handlers: Vec<Vec<(String, usize)>> =
            scopes(&f).into_iter().map(|s| s.handler).collect();
        assert_eq!(
            handlers,
            vec![
                vec![],
                vec![("sqlstate".into(), 5), ("sqlerrm".into(), 6)],
                vec![("sqlstate".into(), 3), ("sqlerrm".into(), 4)],
            ]
        );
    }

    #[test]
    fn nested_blocks_and_loop_variables_in_creation_order() {
        // datums: p, found, x, i (the loop's), x (inner), y alias of p.
        let l = levels(
            "create function f(p int) returns int language plpgsql as $$ <<o>> declare x int; \
             begin for i in 1..2 loop null; end loop; <<n>> declare x int; y alias for $1; \
             begin return o.x + n.x + n.y; end; end $$",
        );
        assert_eq!(
            l,
            vec![
                (Some("o".into()), vec![("x".into(), 2)]),
                (Some("n".into()), vec![("x".into(), 4), ("y".into(), 0)]),
            ]
        );
    }
}
