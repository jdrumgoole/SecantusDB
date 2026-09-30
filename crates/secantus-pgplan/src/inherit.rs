//! Table inheritance (`CREATE TABLE c (...) INHERITS (p)`).
//!
//! A child is a table of its own, holding its own rows, whose columns begin
//! with its parent's. What makes it a child is how statements over the
//! PARENT behave without `ONLY`:
//!
//! * a read reads the parent's rows and every descendant's, each projected
//!   to the parent's columns -- planned as `ONLY p UNION ALL ONLY c ...`;
//! * `UPDATE` / `DELETE` / `TRUNCATE` reach every descendant too -- planned
//!   as one statement per table, run in sequence;
//! * the ALTERs PostgreSQL recurses (a column added, dropped, retyped,
//!   defaulted, a CHECK) apply to every descendant.
//!
//! `INSERT` into a parent stays in the parent, as in PostgreSQL.

use super::*;

thread_local! {
    /// Set while a statement that names `tableoid` has its FROM expanded:
    /// each union arm then carries its table's oid under that name.
    pub(crate) static WANT_TABLEOID: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// `(child, parent)` pairs, and each parent's column names in order.
    static TREE: std::cell::RefCell<(Vec<(String, String)>, Vec<(String, Vec<String>)>)> =
        const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
}

/// Install the database's inheritance: `(child, parent)` pairs and each
/// parent's columns.
pub fn set_inheritance(pairs: Vec<(String, String)>, columns: Vec<(String, Vec<String>)>) {
    TREE.with(|t| *t.borrow_mut() = (pairs, columns));
}

/// Does any table inherit from another?
pub(crate) fn descendants_any() -> bool {
    TREE.with(|t| !t.borrow().0.is_empty())
}

/// Every descendant of `table`, breadth first.
pub fn descendants(table: &str) -> Vec<String> {
    TREE.with(|t| {
        let t = t.borrow();
        let mut out: Vec<String> = Vec::new();
        let mut frontier = vec![table.to_string()];
        while let Some(p) = frontier.pop() {
            for (c, parent) in &t.0 {
                if *parent == p && !out.contains(c) {
                    out.push(c.clone());
                    frontier.insert(0, c.clone());
                }
            }
        }
        out
    })
}

fn parent_columns(table: &str) -> Option<Vec<String>> {
    TREE.with(|t| {
        t.borrow()
            .1
            .iter()
            .find(|(p, _)| p == table)
            .map(|(_, c)| c.clone())
    })
}

/// A FROM item reading a parent WITHOUT `ONLY`, as the union of the tree.
pub(crate) fn expand_from(item: &mut pg_query::protobuf::Node) -> Result<()> {
    let Some(N::RangeVar(r)) = item.node.as_ref() else {
        return Ok(());
    };
    if !r.inh || !(r.schemaname.is_empty() || r.schemaname == "public") {
        return Ok(());
    }
    let children = descendants(&r.relname);
    if children.is_empty() {
        return Ok(());
    }
    let Some(columns) = parent_columns(&r.relname) else {
        return Ok(());
    };
    let q = scalar::quote_identifier;
    let list = columns.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
    let with_oid = WANT_TABLEOID.with(std::cell::Cell::get);
    let sql = std::iter::once(&r.relname)
        .chain(children.iter())
        .map(|t| {
            if with_oid {
                format!(
                    "SELECT {list}, {}::regclass::oid AS tableoid FROM ONLY {}",
                    scalar::quote_literal(t),
                    q(t)
                )
            } else {
                format!("SELECT {list} FROM ONLY {}", q(t))
            }
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    let N::SelectStmt(body) = parse_one(&sql)? else {
        return Err(Error::Internal("an inheritance union".into()));
    };
    let alias = r
        .alias
        .clone()
        .filter(|a| !a.aliasname.is_empty())
        .unwrap_or(pg_query::protobuf::Alias {
            aliasname: r.relname.clone(),
            colnames: Vec::new(),
        });
    item.node = Some(N::RangeSubselect(Box::new(
        pg_query::protobuf::RangeSubselect {
            lateral: false,
            subquery: Some(Box::new(pg_query::protobuf::Node {
                node: Some(N::SelectStmt(body)),
            })),
            alias: Some(alias),
        },
    )));
    Ok(())
}

/// The ALTER TABLE actions PostgreSQL applies to every descendant too.
fn recurses(cmd: &pg_query::protobuf::AlterTableCmd) -> bool {
    use pg_query::protobuf::AlterTableType as AT;
    use pg_query::protobuf::ConstrType as CT;
    match AT::try_from(cmd.subtype) {
        Ok(
            AT::AtAddColumn
            | AT::AtDropColumn
            | AT::AtAlterColumnType
            | AT::AtColumnDefault
            | AT::AtSetNotNull
            | AT::AtDropNotNull
            | AT::AtDropConstraint,
        ) => true,
        Ok(AT::AtAddConstraint) => matches!(
            cmd.def.as_ref().and_then(|d| d.node.as_ref()),
            Some(N::Constraint(k)) if CT::try_from(k.contype) == Ok(CT::ConstrCheck)
        ),
        _ => false,
    }
}

/// A statement over a parent (without `ONLY`) that has to reach its
/// descendants: `(tag, the same statement over each table)`, parent first,
/// each marked `ONLY` so it is not expanded again.
pub(crate) fn per_table(node: &N) -> Result<Option<(&'static str, Vec<N>)>> {
    let (relation, tag) = match node {
        N::UpdateStmt(u) => (u.relation.as_ref(), "UPDATE"),
        N::DeleteStmt(d) => (d.relation.as_ref(), "DELETE"),
        N::AlterTableStmt(a)
            if a.objtype == ObjectType::ObjectTable as i32
                && !a.cmds.is_empty()
                && a.cmds.iter().all(
                    |c| matches!(c.node.as_ref(), Some(N::AlterTableCmd(cmd)) if recurses(cmd)),
                ) =>
        {
            (a.relation.as_ref(), "ALTER TABLE")
        }
        N::RenameStmt(r)
            if ObjectType::try_from(r.rename_type) == Ok(ObjectType::ObjectColumn)
                && ObjectType::try_from(r.relation_type) == Ok(ObjectType::ObjectTable) =>
        {
            (r.relation.as_ref(), "ALTER TABLE")
        }
        _ => return Ok(None),
    };
    let Some(r) = relation else {
        return Ok(None);
    };
    if !r.inh {
        return Ok(None);
    }
    let children = descendants(&r.relname);
    if children.is_empty() {
        return Ok(None);
    }
    let returning = match node {
        N::UpdateStmt(u) => !u.returning_list.is_empty(),
        N::DeleteStmt(d) => !d.returning_list.is_empty(),
        _ => false,
    };
    if returning {
        return Err(Error::Unsupported(format!(
            "{tag} ... RETURNING over an inheritance tree"
        )));
    }
    let parent = r.relname.clone();
    let mut out = Vec::new();
    for table in std::iter::once(&parent).chain(children.iter()) {
        let mut n = node.clone();
        let dml = matches!(n, N::UpdateStmt(_) | N::DeleteStmt(_));
        let rel = match &mut n {
            N::UpdateStmt(u) => u.relation.as_mut(),
            N::DeleteStmt(d) => d.relation.as_mut(),
            N::AlterTableStmt(a) => a.relation.as_mut(),
            N::RenameStmt(r) => r.relation.as_mut(),
            _ => None,
        };
        if let Some(rel) = rel {
            rel.relname = table.clone();
            rel.inh = false;
            // A reference qualified by the parent's name still resolves.
            if rel.alias.as_ref().is_none_or(|a| a.aliasname.is_empty()) && *table != parent {
                if dml {
                    rel.alias = Some(pg_query::protobuf::Alias {
                        aliasname: parent.clone(),
                        colnames: Vec::new(),
                    });
                }
            }
        }
        out.push(n);
    }
    Ok(Some((tag, out)))
}

/// `CREATE TABLE c (...) INHERITS (p, ...)`: the parents' columns first,
/// in order, then the child's own; a column declared on both must agree in
/// type (42804) and is merged. Each parent's CHECKs and NOT NULLs come too.
pub(crate) fn merge_parents(
    def: &mut TableDef,
    parents: &[String],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<()> {
    let own = std::mem::take(&mut def.columns);
    let mut columns: Vec<Column> = Vec::new();
    let mut checks = Vec::new();
    for p in parents {
        let pdef = lookup(p).ok_or_else(|| Error::UndefinedTable(p.clone()))?;
        for c in &pdef.columns {
            if let Some(existing) = columns.iter().find(|x| x.name == c.name) {
                if existing.pg_type != c.pg_type {
                    return Err(type_conflict(&c.name, &existing.pg_type, &c.pg_type));
                }
                continue;
            }
            let mut col = Column::new(&c.name, &c.pg_type, false);
            col.typmod = c.typmod;
            col.nullable = c.nullable || c.pk;
            if c.pk {
                col.nullable = false;
            }
            col.default = c.default.clone();
            col.extra = c.extra.clone();
            columns.push(col);
        }
        for k in &pdef.check_constraints {
            if !checks.iter().any(|x: &CheckConstraint| x.name == k.name) {
                checks.push(k.clone());
            }
        }
    }
    for c in own {
        if let Some(existing) = columns.iter_mut().find(|x| x.name == c.name) {
            if existing.pg_type != c.pg_type {
                return Err(type_conflict(&c.name, &existing.pg_type, &c.pg_type));
            }
            if !c.nullable {
                existing.nullable = false;
            }
            if c.default.is_some() {
                existing.default = c.default.clone();
            }
            continue;
        }
        columns.push(c);
    }
    def.columns = columns;
    for k in checks {
        if !def.check_constraints.iter().any(|x| x.name == k.name) {
            def.check_constraints.push(k);
        }
    }
    def.extra.insert(
        "inherits",
        parents
            .iter()
            .map(|p| Bson::String(p.clone()))
            .collect::<Vec<_>>(),
    );
    Ok(())
}

/// 42804; PostgreSQL's DETAIL (`integer versus text`) has no channel here.
fn type_conflict(column: &str, _a: &str, _b: &str) -> Error {
    Error::DatatypeMismatch(format!("column \"{column}\" has a type conflict"))
}
