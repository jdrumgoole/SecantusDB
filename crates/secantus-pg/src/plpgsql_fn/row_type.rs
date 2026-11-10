//! A variable of a composite type: `r t`, `r t%ROWTYPE` (RP-689).
//!
//! PostgreSQL compiles a variable whose declared type is composite -- a
//! table's row type, a `CREATE TYPE ... AS (...)`, or a relation's
//! `%ROWTYPE` -- as a RECORD of that type (`plpgsql_build_variable`,
//! `PLPGSQL_TTYPE_REC` with `rectypeid` set), so `r.a` is a field the body
//! can read and assign. libpg_query's `parse_datatype` has no catalog: it
//! makes every type but `record` a scalar, and the grammar then refuses
//! `r.a := 5` (`"r.a" is not a known variable`).
//!
//! So the declarations whose type the catalog says is composite are
//! rewritten to `record` before the body is parsed, on the same line, and
//! the interpreter is told each one's type: such a record starts NULL, a
//! field of it reads NULL and an assignment to one instantiates it with
//! every field NULL first (`instantiate_empty_record_variable`), and a row
//! moved into it is converted to its fields' types by position
//! (`exec_move_row` into a record whose `rectypeid` is not RECORD).

use super::*;

/// A declaration the rewrite made a record: its line in the body and its
/// name, as the parsed `PLpgSQL_rec` carries them, and its row type.
#[derive(Debug, Clone)]
pub(super) struct RowDecl {
    pub lineno: usize,
    pub refname: String,
    pub row_type: RowType,
}

/// A record variable's declared row type: its name and each field's
/// `(name, declared type text)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RowType {
    pub name: String,
    pub columns: Vec<(String, String)>,
}

/// A declaration whose type is a bare (possibly qualified) name or a
/// `%ROWTYPE`: where its type text lies in the CREATE text, its line and
/// name, and the text.
#[derive(Debug, Clone)]
struct Candidate {
    start: usize,
    end: usize,
    lineno: usize,
    refname: String,
    text: String,
}

/// The body of a CREATE text the server builds (`plpgsql_create_sql`, a DO
/// block's wrapper): between its dollar-quote tags. Answers its byte offset
/// and text.
fn body_of(create_sql: &str) -> Option<(usize, &str)> {
    for tag in ["$secantus_body$", "$secantus_do$"] {
        let Some(open) = create_sql.find(tag) else {
            continue;
        };
        let from = open + tag.len();
        let close = create_sql.rfind(tag).filter(|c| *c >= from)?;
        return Some((from, &create_sql[from..close]));
    }
    None
}

/// Every DECLARE-section declaration of `create_sql` whose type could be a
/// row type, found with PostgreSQL's scanner.
fn candidates(create_sql: &str) -> Vec<Candidate> {
    static CACHE: OnceLock<Mutex<HashMap<String, Vec<Candidate>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(c) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(create_sql)
    {
        return c.clone();
    }
    let found = scan_candidates(create_sql);
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(create_sql.to_string(), found.clone());
    found
}

fn scan_candidates(create_sql: &str) -> Vec<Candidate> {
    let Some((offset, body)) = body_of(create_sql) else {
        return Vec::new();
    };
    // Cheap: a body with no DECLARE has nothing to find.
    if !body.to_ascii_lowercase().contains("declare") {
        return Vec::new();
    }
    let Ok(scan) = pg_query::scan(body) else {
        return Vec::new();
    };
    let tokens: Vec<(usize, usize, String)> = scan
        .tokens
        .iter()
        .filter_map(|t| {
            let (s, e) = (t.start as usize, t.end as usize);
            Some((s, e, body.get(s..e)?.to_string()))
        })
        .collect();
    let lower = |i: usize| tokens.get(i).map(|t| t.2.to_ascii_lowercase());
    let is_word = |s: &str| {
        s.starts_with('"')
            || s.chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_')
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if lower(i).as_deref() != Some("declare") {
            i += 1;
            continue;
        }
        i += 1;
        // The declarations, up to the block's BEGIN.
        while i < tokens.len() && lower(i).as_deref() != Some("begin") {
            // One declaration: its tokens up to its `;` outside parentheses.
            let first = i;
            let mut depth = 0i32;
            while i < tokens.len() {
                match tokens[i].2.as_str() {
                    "(" | "[" => depth += 1,
                    ")" | "]" => depth -= 1,
                    ";" if depth <= 0 => break,
                    _ => {}
                }
                i += 1;
            }
            let decl = &tokens[first..i.min(tokens.len())];
            i += 1;
            let Some((name_start, _, name)) = decl.first() else {
                continue;
            };
            if !is_word(name) {
                continue;
            }
            let mut j = 1;
            if decl.get(j).map(|t| t.2.to_ascii_lowercase()).as_deref() == Some("constant") {
                j += 1;
            }
            let ty_from = j;
            while let Some((_, _, t)) = decl.get(j) {
                let t = t.to_ascii_lowercase();
                if matches!(
                    t.as_str(),
                    "collate" | "not" | ":=" | "=" | "default" | "null"
                ) {
                    break;
                }
                j += 1;
            }
            let ty = &decl[ty_from..j];
            let shape_ok = !ty.is_empty()
                && ty
                    .iter()
                    .all(|(_, _, t)| is_word(t) || t == "." || t == "%")
                && !matches!(
                    ty[0].2.to_ascii_lowercase().as_str(),
                    "alias" | "cursor" | "scroll" | "no" | "record"
                );
            if !shape_ok {
                continue;
            }
            let (start, end) = (ty[0].0, ty[ty.len() - 1].1);
            let refname = if let Some(q) = name.strip_prefix('"') {
                q.strip_suffix('"').unwrap_or(q).replace("\"\"", "\"")
            } else {
                name.to_lowercase()
            };
            out.push(Candidate {
                start: offset + start,
                end: offset + end,
                lineno: body[..*name_start].matches('\n').count() + 1,
                refname,
                text: body[start..end].to_string(),
            });
        }
    }
    out
}

/// `create_sql` with each declaration whose type `resolve` answers as a
/// row type ([`Host::row_type`]) declared `record` instead, and those
/// declarations. The text is unchanged when there are none.
pub(crate) fn row_typed(
    create_sql: &str,
    resolve: &dyn Fn(&str) -> Option<secantus_pgplan::PlpgsqlRowType>,
) -> (String, Vec<RowDecl>) {
    let mut decls = Vec::new();
    let mut spans = Vec::new();
    for c in candidates(create_sql) {
        // A built-in type is never a row type: no catalog lookup for it.
        let canonical = canonical_type(&c.text);
        if secantus_pgplan::pgtypes::oid_of_name(&canonical).is_some() {
            continue;
        }
        if let Some((name, columns)) = resolve(&c.text) {
            spans.push((c.start, c.end));
            decls.push(RowDecl {
                lineno: c.lineno,
                refname: c.refname,
                row_type: RowType { name, columns },
            });
        }
    }
    if spans.is_empty() {
        return (create_sql.to_string(), decls);
    }
    let mut out = create_sql.to_string();
    for (start, end) in spans.into_iter().rev() {
        out.replace_range(start..end, "record");
    }
    (out, decls)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(body: &str) -> String {
        format!(
            "CREATE FUNCTION inline_code_block() RETURNS void AS \
             $secantus_do${body}\n$secantus_do$ LANGUAGE plpgsql"
        )
    }

    #[test]
    fn finds_the_declarations_a_row_type_could_be() {
        let sql = wrap(
            " declare r t687; k constant \"T\".x%rowtype := null; n int; \
             c cursor for select 1; a alias for $1; z record;\n\
             q s.t not null default 1; begin end ",
        );
        let found: Vec<(String, String, usize)> = candidates(&sql)
            .into_iter()
            .map(|c| (c.refname, c.text, c.lineno))
            .collect();
        assert_eq!(
            found,
            vec![
                ("r".into(), "t687".into(), 1),
                ("k".into(), "\"T\".x%rowtype".into(), 1),
                ("n".into(), "int".into(), 1),
                ("q".into(), "s.t".into(), 2),
            ]
        );
    }

    #[test]
    fn rewrites_only_what_resolves() {
        let sql = wrap(" declare r t687; n int; begin r.a := 5; end ");
        let (out, decls) = row_typed(&sql, &|t| {
            (t == "t687").then(|| ("t687".into(), vec![("a".into(), "int4".into())]))
        });
        assert!(out.contains("declare r record; n int;"), "{out}");
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].refname, "r");
        assert_eq!(decls[0].row_type.name, "t687");
        assert!(parsed(&out).is_ok());
        let (same, none) = row_typed(&sql, &|_| None);
        assert_eq!(same, sql);
        assert!(none.is_empty());
    }
}
