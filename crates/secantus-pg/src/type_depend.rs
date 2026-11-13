//! What depends on a user type's VALUES, and the DROP that removes it
//! (RP-607): the walk PostgreSQL 15.19's `findDependentObjects`
//! (dependency.c) makes from a type through `pg_depend`.
//!
//! Each object's dependents are visited depth first in
//! `object_address_comparator` order -- oid descending, then attnum
//! ascending within one relation -- and `reportDependentObjects` lists them
//! in reverse of the order the walk finished them. Since this server's oids
//! come from one counter in creation order (RP-137), that is the same
//! order. A type's array type is an internal dependent of it, never named,
//! whose own dependents are reported as depending on `T[]`; a domain over a
//! type is followed on to its own dependents, so a chain of domains drops
//! whole or not at all.
//!
//! The objects followed: typed tables (`OF type`), columns, domains and the
//! fields of composite types. Routines whose signature names a type are
//! `drop_type_signature_dependents`'s.

use super::*;

/// An object a type's values reach.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TypeNode {
    /// A user type, or (`array`) its internal array type.
    Type {
        name: String,
        array: bool,
    },
    Column {
        table: String,
        column: String,
    },
    TypedTable(String),
    Field {
        composite: String,
        field: String,
    },
}

impl TypeNode {
    fn of_type(name: &str) -> Self {
        TypeNode::Type {
            name: name.to_string(),
            array: false,
        }
    }

    /// `getObjectDescription`'s text for it.
    pub(crate) fn description(&self) -> String {
        match self {
            TypeNode::Type { name, array } => {
                format!("type {name}{}", if *array { "[]" } else { "" })
            }
            TypeNode::Column { table, column } => format!("column {column} of table {table}"),
            TypeNode::TypedTable(t) => format!("table {t}"),
            TypeNode::Field { composite, field } => {
                format!("column {field} of composite type {composite}")
            }
        }
    }
}

/// One reported dependent: the object, and the one it was found through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TypeDependent {
    pub node: TypeNode,
    pub depends_on: TypeNode,
}

/// A table: `(name, oid, columns)`; a column is `(name, attnum, type it
/// depends on)`.
pub(crate) type GraphTable = (String, i64, Vec<(String, i16, String)>);

/// A composite type: `(name, oid, fields)`; a field is `(name, type)`.
pub(crate) type GraphComposite = (String, i64, Vec<(String, String)>);

/// The type graph of one database, as the catalog has it now.
pub(crate) struct TypeGraph {
    /// Every user type's oid, by name.
    pub type_oids: HashMap<String, i64>,
    /// `(name, oid, base type)` of each domain.
    pub domains: Vec<(String, i64, String)>,
    pub tables: Vec<GraphTable>,
    /// `(table, oid, type)` of each table made `OF type`.
    pub typed_tables: Vec<(String, i64, String)>,
    /// Each composite type that is not a table's row type.
    pub composites: Vec<GraphComposite>,
}

/// `object_address_comparator`'s order, as a key sorted ascending: oid
/// descending, then subobject ascending. A type's array type is minted
/// right after it, so it sorts above the type and below anything newer.
type SortKey = (std::cmp::Reverse<i128>, i32);

fn key(oid: i64, array: bool, sub: i32) -> SortKey {
    (
        std::cmp::Reverse(i128::from(oid) * 2 + i128::from(array)),
        sub,
    )
}

impl TypeGraph {
    /// The objects that depend directly on `node`, in visiting order.
    fn children(&self, node: &TypeNode) -> Vec<TypeNode> {
        let TypeNode::Type { name, array } = node else {
            return Vec::new();
        };
        let shown = if *array {
            format!("{name}[]")
        } else {
            name.clone()
        };
        let mut out: Vec<(SortKey, TypeNode)> = Vec::new();
        if !*array {
            if let Some(oid) = self.type_oids.get(name) {
                out.push((
                    key(*oid, true, 0),
                    TypeNode::Type {
                        name: name.clone(),
                        array: true,
                    },
                ));
            }
            for (t, oid, of) in &self.typed_tables {
                if of == name {
                    out.push((key(*oid, false, 0), TypeNode::TypedTable(t.clone())));
                }
            }
        }
        for (t, oid, cols) in &self.tables {
            for (c, attnum, ty) in cols {
                if *ty == shown {
                    out.push((
                        key(*oid, false, i32::from(*attnum)),
                        TypeNode::Column {
                            table: t.clone(),
                            column: c.clone(),
                        },
                    ));
                }
            }
        }
        for (d, oid, base) in &self.domains {
            if *base == shown {
                out.push((key(*oid, false, 0), TypeNode::of_type(d)));
            }
        }
        for (c, oid, fields) in &self.composites {
            if c == name {
                continue;
            }
            for (i, (f, ty)) in fields.iter().enumerate() {
                if *ty == shown {
                    out.push((
                        key(*oid, false, i as i32 + 1),
                        TypeNode::Field {
                            composite: c.clone(),
                            field: f.clone(),
                        },
                    ));
                }
            }
        }
        out.sort_by_key(|a| a.0);
        out.into_iter().map(|(_, n)| n).collect()
    }

    /// What depends on `roots`, as `reportDependentObjects` lists it: the
    /// roots themselves and the internal array types left out.
    pub(crate) fn dependents(&self, roots: &[String]) -> Vec<TypeDependent> {
        let mut visited: HashSet<TypeNode> = HashSet::new();
        let mut finished: Vec<(TypeNode, Option<TypeNode>)> = Vec::new();
        for root in roots {
            self.visit(TypeNode::of_type(root), None, &mut visited, &mut finished);
        }
        let targets: Vec<TypeNode> = roots.iter().map(|r| TypeNode::of_type(r)).collect();
        finished
            .into_iter()
            .rev()
            .filter_map(|(node, from)| {
                let internal = matches!(node, TypeNode::Type { array: true, .. });
                if internal || targets.contains(&node) {
                    return None;
                }
                Some(TypeDependent {
                    node,
                    depends_on: from?,
                })
            })
            .collect()
    }

    fn visit(
        &self,
        node: TypeNode,
        from: Option<TypeNode>,
        visited: &mut HashSet<TypeNode>,
        finished: &mut Vec<(TypeNode, Option<TypeNode>)>,
    ) {
        if !visited.insert(node.clone()) {
            return;
        }
        for child in self.children(&node) {
            self.visit(child, Some(node.clone()), visited, finished);
        }
        finished.push((node, from));
    }
}

impl PgHandler {
    /// This database's type graph.
    fn type_graph(&self) -> PgWireResult<TypeGraph> {
        let mut type_oids = HashMap::new();
        let domains: Vec<(String, i64, String)> = self
            .domains()?
            .into_iter()
            .map(|d| (d.name, d.oid, d.base))
            .collect();
        for (n, oid, _) in &domains {
            type_oids.insert(n.clone(), *oid);
        }
        for (n, oid, _) in self.enums()? {
            type_oids.insert(n, oid);
        }
        for (n, oid, _) in self.ranges()? {
            type_oids.insert(n, oid);
        }
        let mut composites = Vec::new();
        for (n, oid, fields) in Arc::unwrap_or_clone(self.composites()?) {
            type_oids.insert(n.clone(), oid);
            if !self.row_type_of_table(&n)? {
                composites.push((n, oid, fields));
            }
        }
        let mut tables = Vec::new();
        let mut typed_tables = Vec::new();
        for def in self.shared_table_defs()?.iter() {
            let oid = self.relation_oid(&def.name).unwrap_or(0);
            if let Ok(of) = def
                .extra
                .get_str(secantus_pgplan::typed_tables::OF_TYPE_KEY)
            {
                typed_tables.push((def.name.clone(), oid, of.to_string()));
            }
            let cols = def
                .columns
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    // A domain column depends on its domain, not on the
                    // domain's base type.
                    let ty = c.extra.get_str("domain_type").unwrap_or(&c.pg_type);
                    (c.name.clone(), def.attnum(i), ty.to_string())
                })
                .collect();
            tables.push((def.name.clone(), oid, cols));
        }
        Ok(TypeGraph {
            type_oids,
            domains,
            tables,
            typed_tables,
            composites,
        })
    }

    /// The dependents of the types `roots` a DROP of `targets` (every type
    /// the statement names that exists; `roots` among them) must refuse or
    /// cascade to, the targets themselves left out: RESTRICT is 2BP01 naming
    /// them all, CASCADE drops them with one notice.
    pub(crate) fn drop_type_value_dependents(
        &self,
        roots: &[String],
        targets: &[String],
        cascade: bool,
    ) -> PgWireResult<()> {
        let excluded: Vec<TypeNode> = targets.iter().map(|t| TypeNode::of_type(t)).collect();
        let deps: Vec<TypeDependent> = self
            .type_graph()?
            .dependents(roots)
            .into_iter()
            .filter(|d| !excluded.contains(&d.node))
            .collect();
        if deps.is_empty() {
            return Ok(());
        }
        if !cascade {
            // `reportDependentObjects`: one object named, or several.
            let message = match targets {
                [one] => format!("cannot drop type {one} because other objects depend on it"),
                _ => "cannot drop desired object(s) because other objects depend on them".into(),
            };
            let mut info = ErrorInfo::new(
                "ERROR".into(),
                "2BP01".into(), // dependent_objects_still_exist
                message,
            );
            info.detail = Some(
                deps.iter()
                    .map(|d| {
                        format!(
                            "{} depends on {}",
                            d.node.description(),
                            d.depends_on.description()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            info.hint = Some("Use DROP ... CASCADE to drop the dependent objects too.".into());
            return Err(PgWireError::UserError(Box::new(info)));
        }
        self.cascade_notice(
            &deps
                .iter()
                .map(|d| d.node.description())
                .collect::<Vec<_>>(),
        );
        let typed: Vec<&String> = deps
            .iter()
            .filter_map(|d| match &d.node {
                TypeNode::TypedTable(t) => Some(t),
                _ => None,
            })
            .collect();
        for t in &typed {
            self.internal_sql(&format!(
                "DROP TABLE {} CASCADE",
                secantus_pgplan::scalar::quote_identifier(t)
            ))?;
        }
        // One ALTER per table, dropping all of its columns at once: two
        // single-column ALTERs of one table left the second column behind.
        let mut by_table: Vec<(String, Vec<String>)> = Vec::new();
        for d in &deps {
            let TypeNode::Column { table, column } = &d.node else {
                continue;
            };
            if typed.contains(&table) {
                continue;
            }
            match by_table.iter_mut().find(|(t, _)| t == table) {
                Some((_, cols)) => cols.push(column.clone()),
                None => by_table.push((table.clone(), vec![column.clone()])),
            }
        }
        for (table, cols) in by_table {
            self.execute_inner(
                Statement::AlterTable {
                    table,
                    missing_ok: false,
                    actions: cols
                        .into_iter()
                        .map(|name| secantus_pgplan::AlterTableAction::DropColumn {
                            name,
                            if_exists: true,
                        })
                        .collect(),
                },
                0,
            )?;
        }
        for d in &deps {
            if let TypeNode::Type { name, array: false } = &d.node {
                self.delete_type_doc(Self::DOMAIN_COLLECTION, name)?;
            }
        }
        for d in &deps {
            let TypeNode::Field { composite, field } = &d.node else {
                continue;
            };
            let Some(doc) = self
                .type_catalog_docs(Self::COMPOSITE_COLLECTION)?
                .iter()
                .find(|c| c.get_str("composite") == Ok(composite.as_str()))
                .map(|d| Document::clone(d))
            else {
                continue;
            };
            let id = doc.get_str("_id").unwrap_or(composite).to_string();
            // As ALTER TYPE ... DROP ATTRIBUTE: its stored values lose the
            // field, and its attnum is left as a dropped slot (RP-158).
            self.alter_type_attributes(
                &id,
                vec![secantus_pgplan::typed_tables::AttributeAction::Drop {
                    name: field.clone(),
                    if_exists: true,
                    cascade: true,
                }],
            )?;
        }
        Ok(())
    }

    /// `DROP DOMAIN`: every name is looked up before anything is dropped,
    /// as `RemoveObjects` does, and the domains go together through one
    /// dependency walk (`performMultipleDeletions`).
    pub(crate) fn drop_domains(
        &self,
        names: &[String],
        if_exists: bool,
        cascade: bool,
    ) -> PgWireResult<()> {
        let existing = self.domains()?;
        let mut found: Vec<String> = Vec::new();
        for name in names {
            if existing.iter().any(|d| d.name == *name) {
                if !found.contains(name) {
                    found.push(name.clone());
                }
                continue;
            }
            // `get_object_address_type`: a type of another kind is not a
            // domain, IF EXISTS or not.
            if self.enums()?.iter().any(|(n, _, _)| n == name)
                || self.composites()?.iter().any(|(n, _, _)| n == name)
                || self.ranges()?.iter().any(|(n, _, _)| n == name)
            {
                return Err(Self::user_error(
                    "42809",
                    format!(
                        "\"{}\" is not a domain",
                        secantus_pgplan::pgtypes::user_type_catalog_name(name)
                    ),
                ));
            }
            if if_exists {
                self.notice(
                    "00000",
                    format!(
                        "type \"{}\" does not exist, skipping",
                        secantus_pgplan::pgtypes::user_type_catalog_name(name)
                    ),
                    None,
                );
                continue;
            }
            return Err(Self::undefined_domain(name));
        }
        for name in &found {
            self.drop_type_signature_dependents(name, cascade)?;
        }
        self.drop_type_value_dependents(&found, &found, cascade)?;
        for name in &found {
            self.delete_type_doc(Self::DOMAIN_COLLECTION, name)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ty(n: &str) -> TypeNode {
        TypeNode::of_type(n)
    }

    fn col(t: &str, c: &str) -> TypeNode {
        TypeNode::Column {
            table: t.into(),
            column: c.into(),
        }
    }

    /// The order measured on PostgreSQL 15.19 for
    /// `create domain d607a as int; create table t1 (p d607a, q int, r
    /// d607a); create domain d607b as d607a; create table t0 (s d607a[]);
    /// create domain d607c as d607a[]; create type c607 as (f d607b, g
    /// int); drop domain d607a`.
    #[test]
    fn dependents_in_report_order() {
        let mut type_oids = HashMap::new();
        for (n, o) in [
            ("d607a", 100),
            ("d607b", 110),
            ("d607c", 130),
            ("c607", 140),
        ] {
            type_oids.insert(n.to_string(), o);
        }
        let g = TypeGraph {
            type_oids,
            domains: vec![
                ("d607a".into(), 100, "int4".into()),
                ("d607b".into(), 110, "d607a".into()),
                ("d607c".into(), 130, "d607a[]".into()),
            ],
            tables: vec![
                (
                    "t1".into(),
                    102,
                    vec![
                        ("p".into(), 1, "d607a".into()),
                        ("q".into(), 2, "int4".into()),
                        ("r".into(), 3, "d607a".into()),
                    ],
                ),
                ("t0".into(), 120, vec![("s".into(), 1, "d607a[]".into())]),
            ],
            typed_tables: vec![],
            composites: vec![(
                "c607".into(),
                140,
                vec![("f".into(), "d607b".into()), ("g".into(), "int4".into())],
            )],
        };
        let arr = TypeNode::Type {
            name: "d607a".into(),
            array: true,
        };
        let got: Vec<(TypeNode, TypeNode)> = g
            .dependents(&["d607a".into()])
            .into_iter()
            .map(|d| (d.node, d.depends_on))
            .collect();
        assert_eq!(
            got,
            vec![
                (col("t0", "s"), arr.clone()),
                (ty("d607c"), arr),
                (col("t1", "r"), ty("d607a")),
                (col("t1", "p"), ty("d607a")),
                (ty("d607b"), ty("d607a")),
                (
                    TypeNode::Field {
                        composite: "c607".into(),
                        field: "f".into()
                    },
                    ty("d607b")
                ),
            ]
        );
        // Both named: neither is reported, and what is over the second
        // still is.
        let both: Vec<TypeNode> = g
            .dependents(&["d607b".into(), "d607a".into()])
            .into_iter()
            .map(|d| d.node)
            .collect();
        assert!(!both.contains(&ty("d607b")));
        assert!(both.contains(&ty("d607c")));
    }
}
