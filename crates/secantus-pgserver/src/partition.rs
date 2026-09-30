//! Declarative partitioning, the executor half (the planner half and the
//! storage model are described in `secantus_pgplan::partitions`).
//!
//! A partition tree's rows live in its ROOT table. Each partition is a
//! catalog table that the planner sees as a view over its parent restricted
//! to its bound, checked `CASCADED`, so reads and writes through it need no
//! code here. What does: validating a new bound (its type, its arity, overlap
//! with its siblings), routing a row written to a partitioned table
//! (`23514 no partition of relation ... found for row`), moving rows on
//! `ATTACH` / `DETACH`, and taking a partition's rows with it on `DROP` /
//! `TRUNCATE`.
//!
//! Messages and SQLSTATEs measured on PostgreSQL 14.

use std::cmp::Ordering;

use bson::{Bson, Document};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use secantus_pgcatalog::TableDef;
use secantus_pgplan::partitions::{self, Key};

use crate::{PgHandler, PlHost};

fn user_err(code: &str, message: impl Into<String>) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".into(),
        code.into(),
        message.into(),
    )))
}

/// The table this one is a partition of.
pub(crate) fn parent_of(def: &TableDef) -> Option<&str> {
    def.extra.get_str("partition_of").ok()
}

/// `pg_get_partkeydef`: the strategy and the key, `LIST (k)`.
pub(crate) fn partkey_text(def: &TableDef) -> Option<String> {
    let strategy = match strategy(def) {
        "range" => "RANGE",
        "list" => "LIST",
        "hash" => "HASH",
        _ => return None,
    };
    Some(format!("{strategy} ({})", key_names(def).join(", ")))
}

/// Is `def` a partitioned table (`PARTITION BY`)?
pub(crate) fn is_partitioned(def: &TableDef) -> bool {
    def.extra.get_document("partition_by").is_ok()
}

fn strategy(def: &TableDef) -> &str {
    def.extra
        .get_document("partition_by")
        .ok()
        .and_then(|p| p.get_str("strategy").ok())
        .unwrap_or_default()
}

fn key_names(def: &TableDef) -> Vec<String> {
    def.extra
        .get_document("partition_by")
        .ok()
        .and_then(|p| p.get_array("columns").ok())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The key in DECLARED order -- a range bound compares its values
/// position by position, so table-column order would pair the wrong ones --
/// each a column or a parenthesised expression, with its type.
fn key_of(parent: &TableDef) -> Key<'_> {
    let by = parent.extra.get_document("partition_by").ok();
    let strs = |k: &str| -> Vec<&str> {
        by.and_then(|b| b.get_array(k).ok())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default()
    };
    let (names, types) = (strs("columns"), strs("key_types"));
    Key {
        columns: names
            .iter()
            .enumerate()
            .filter_map(|(i, n)| {
                let ty = types
                    .get(i)
                    .copied()
                    .or_else(|| parent.column(n).map(|c| c.pg_type.as_str()))?;
                Some((*n, ty))
            })
            .collect(),
    }
}

/// The key in DECLARED order, with its types.
fn ordered_key(parent: &TableDef) -> Vec<(String, String)> {
    key_of(parent)
        .columns
        .into_iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect()
}

fn bound_of(def: &TableDef) -> Document {
    def.extra
        .get_document("partition_bound")
        .cloned()
        .unwrap_or_default()
}

fn texts(bound: &Document, field: &str) -> Vec<Option<String>> {
    bound
        .get_array(field)
        .map(|a| a.iter().map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// One end of a range bound, for ordering: MINVALUE below everything,
/// MAXVALUE above.
#[derive(Clone)]
enum End {
    Min,
    Value(Bson),
    Max,
}

fn cmp_end(a: &End, b: &End) -> Ordering {
    match (a, b) {
        (End::Min, End::Min) | (End::Max, End::Max) => Ordering::Equal,
        (End::Min, _) | (_, End::Max) => Ordering::Less,
        (_, End::Min) | (End::Max, _) => Ordering::Greater,
        (End::Value(x), End::Value(y)) => {
            secantus_pgplan::compare_values(x, y).unwrap_or(Ordering::Equal)
        }
    }
}

/// Tuple order, as PostgreSQL compares range bounds: the first column that
/// differs decides, and a MINVALUE / MAXVALUE ends the comparison.
fn cmp_tuple(a: &[End], b: &[End]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let o = cmp_end(x, y);
        if o != Ordering::Equal {
            return o;
        }
        if !matches!(x, End::Value(_)) {
            break;
        }
    }
    Ordering::Equal
}

impl PgHandler {
    /// The partitions of `parent`, in name order.
    pub(crate) fn partitions_of(&self, parent: &str) -> PgWireResult<Vec<TableDef>> {
        let mut out: Vec<TableDef> = self
            .all_table_defs()?
            .into_iter()
            .filter(|d| parent_of(d) == Some(parent))
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// The root of the partition tree `def` belongs to.
    pub(crate) fn partition_root(&self, def: &TableDef) -> TableDef {
        let mut cur = def.clone();
        for _ in 0..64 {
            match parent_of(&cur).and_then(|p| self.lookup(p)) {
                Some(p) => cur = p,
                None => break,
            }
        }
        cur
    }

    /// The condition a row of `part` satisfies, over its parent's columns.
    pub(crate) fn partition_condition(&self, part: &TableDef) -> PgWireResult<Option<String>> {
        let Some(parent) = parent_of(part).and_then(|p| self.lookup(p)) else {
            return Ok(None);
        };
        let siblings: Vec<Document> = self
            .partitions_of(&parent.name)?
            .iter()
            .filter(|s| s.name != part.name)
            .map(bound_of)
            .collect();
        Ok(Some(partitions::bound_condition(
            &key_of(&parent),
            &bound_of(part),
            &siblings,
        )))
    }

    /// The condition over the ROOT's rows selecting exactly `part`'s: its
    /// own bound AND every ancestor's.
    fn partition_path_condition(&self, part: &TableDef) -> PgWireResult<String> {
        let mut conds = Vec::new();
        let mut cur = part.clone();
        for _ in 0..64 {
            let Some(c) = self.partition_condition(&cur)? else {
                break;
            };
            conds.push(c);
            match parent_of(&cur).and_then(|p| self.lookup(p)) {
                Some(p) => cur = p,
                None => break,
            }
        }
        Ok(if conds.is_empty() {
            "true".into()
        } else {
            conds.join(" AND ")
        })
    }

    /// Every partition as the planner reads it: a view over its parent.
    pub(crate) fn partition_views(&self) -> Vec<(String, String)> {
        let Ok(defs) = self.all_table_defs() else {
            return Vec::new();
        };
        defs.iter()
            .filter_map(|d| {
                let parent = parent_of(d)?;
                let cond = self.partition_condition(d).ok()??;
                Some((
                    d.name.clone(),
                    format!(
                        "SELECT * FROM {} WHERE {cond}",
                        secantus_pgplan::scalar::quote_identifier(parent)
                    ),
                ))
            })
            .collect()
    }

    /// Each partition's own column DEFAULTs (those differing from its
    /// parent's), which an INSERT written directly into the partition --
    /// served as a view -- gives a column it omits.
    pub(crate) fn partition_defaults(&self) -> Vec<(String, Vec<(String, String)>)> {
        let Ok(defs) = self.all_table_defs() else {
            return Vec::new();
        };
        defs.iter()
            .filter_map(|d| {
                let parent = self.lookup(parent_of(d)?)?;
                let cols: Vec<(String, String)> = d
                    .columns
                    .iter()
                    .filter_map(|c| {
                        let own = Self::default_expression(c);
                        let inherited = parent
                            .column(&c.name)
                            .map(Self::default_expression)
                            .unwrap_or(Bson::Null);
                        match own {
                            Bson::String(sql) if Bson::String(sql.clone()) != inherited => {
                                Some((c.name.clone(), sql))
                            }
                            _ => None,
                        }
                    })
                    .collect();
                (!cols.is_empty()).then(|| (d.name.clone(), cols))
            })
            .collect()
    }

    /// Every table's `tableoid`: its own oid, or -- for a partitioned one --
    /// the oid of the leaf partition each row falls in.
    pub(crate) fn tableoid_expressions(&self) -> Vec<(String, String)> {
        let Ok(defs) = self.all_table_defs() else {
            return Vec::new();
        };
        let oid = |n: &str| self.relation_oid(n).unwrap_or(0);
        let leaves = |root: &TableDef| -> Vec<(String, String)> {
            // (leaf, its condition over `root`'s rows)
            let mut out = Vec::new();
            let mut stack = vec![root.name.clone()];
            while let Some(level) = stack.pop() {
                for p in defs.iter().filter(|d| parent_of(d) == Some(level.as_str())) {
                    if is_partitioned(p) {
                        stack.push(p.name.clone());
                    } else if let Ok(c) = self.partition_path_condition(p) {
                        out.push((p.name.clone(), c));
                    }
                }
            }
            out
        };
        defs.iter()
            .map(|d| {
                let sql = if is_partitioned(d) {
                    let arms: Vec<String> = leaves(d)
                        .into_iter()
                        .map(|(leaf, cond)| format!("WHEN {cond} THEN {}", oid(&leaf)))
                        .collect();
                    if arms.is_empty() {
                        "NULL::oid".to_string()
                    } else {
                        format!("(CASE {} END)::oid", arms.join(" "))
                    }
                } else {
                    format!("{}::oid", oid(&d.name))
                };
                (d.name.clone(), sql)
            })
            .collect()
    }

    /// Is `name` a partition?
    pub(crate) fn is_partition(&self, name: &str) -> bool {
        self.lookup(name).is_some_and(|d| parent_of(&d).is_some())
    }

    /// A datum's value as the key column's type.
    fn datum_value(&self, text: &str, ty: &str) -> PgWireResult<Bson> {
        let lit = format!("'{}'", text.replace('\'', "''"));
        self.eval_constant_sql(&format!("SELECT {lit}::{ty}"))
    }

    fn range_ends(
        &self,
        texts: &[Option<String>],
        key: &[(String, String)],
    ) -> PgWireResult<Vec<End>> {
        texts
            .iter()
            .zip(key)
            .map(|(t, (_, ty))| {
                Ok(match t.as_deref() {
                    Some("MINVALUE") => End::Min,
                    Some("MAXVALUE") => End::Max,
                    Some(v) => End::Value(self.datum_value(v, ty)?),
                    None => End::Value(Bson::Null),
                })
            })
            .collect()
    }

    /// Check a new partition's bound against its parent and siblings, and
    /// rewrite its datums to their canonical text (`'2023-1-1'` is stored as
    /// `2023-01-01`, as `pg_get_expr` prints it).
    pub(crate) fn prepare_partition(
        &self,
        name: &str,
        parent_name: &str,
        bound: &mut Document,
    ) -> PgWireResult<()> {
        let parent = self.lookup(parent_name).ok_or_else(|| {
            user_err(
                "42P01",
                format!("relation \"{parent_name}\" does not exist"),
            )
        })?;
        if !is_partitioned(&parent) {
            return Err(user_err(
                "42P17",
                format!("\"{parent_name}\" is not partitioned"),
            ));
        }
        let key = ordered_key(&parent);
        let kind = bound.get_str("kind").unwrap_or_default().to_string();
        let strat = strategy(&parent);
        if kind == "default" && strat == "hash" {
            return Err(user_err(
                "42P16",
                "a hash-partitioned table may not have a default partition",
            ));
        }
        if kind != "default" && kind != strat {
            return Err(user_err(
                "42P16",
                format!("invalid bound specification for a {strat} partition"),
            ));
        }
        let canonical = |field: &str, bound: &mut Document| -> PgWireResult<()> {
            let mut out = Vec::new();
            for (i, t) in texts(bound, field).into_iter().enumerate() {
                let ty = &key[i.min(key.len() - 1)].1;
                out.push(match t.as_deref() {
                    None | Some("MINVALUE") | Some("MAXVALUE") => {
                        t.map(Bson::String).unwrap_or(Bson::Null)
                    }
                    Some(v) if v.eq_ignore_ascii_case("NULL") => {
                        if kind == "range" {
                            return Err(user_err("42P16", "cannot specify NULL in range bound"));
                        }
                        Bson::Null
                    }
                    Some(v) => {
                        let text =
                            self.eval_constant_sql(&format!("SELECT (({v})::{ty})::text"))?;
                        match text {
                            Bson::String(s) => Bson::String(s),
                            _ => Bson::Null,
                        }
                    }
                });
            }
            bound.insert(field, out);
            Ok(())
        };
        match kind.as_str() {
            "range" => {
                for (field, word) in [("from", "FROM"), ("to", "TO")] {
                    if texts(bound, field).len() != key.len() {
                        return Err(user_err(
                            "42P16",
                            format!(
                                "{word} must specify exactly one value per partitioning column"
                            ),
                        ));
                    }
                    canonical(field, bound)?;
                }
            }
            "list" => canonical("values", bound)?,
            "hash" => {
                let modulus = bound.get_i32("modulus").unwrap_or(0);
                let remainder = bound.get_i32("remainder").unwrap_or(0);
                if modulus < 1 {
                    return Err(user_err(
                        "42P16",
                        "modulus for hash partition must be an integer value greater than zero",
                    ));
                }
                if remainder < 0 {
                    return Err(user_err(
                        "42P16",
                        "remainder for hash partition must be an integer value greater than or equal to zero",
                    ));
                }
                if remainder >= modulus {
                    return Err(user_err(
                        "42P16",
                        "remainder for hash partition must be less than modulus",
                    ));
                }
            }
            _ => {}
        }
        let siblings: Vec<TableDef> = self
            .partitions_of(parent_name)?
            .into_iter()
            .filter(|s| s.name != name)
            .collect();
        match kind.as_str() {
            "default" => {
                if let Some(d) = siblings
                    .iter()
                    .find(|s| bound_of(s).get_str("kind") == Ok("default"))
                {
                    return Err(user_err(
                        "42P17",
                        format!(
                            "partition \"{name}\" conflicts with existing default partition \"{}\"",
                            d.name
                        ),
                    ));
                }
            }
            "range" => {
                let lo = self.range_ends(&texts(bound, "from"), &key)?;
                let hi = self.range_ends(&texts(bound, "to"), &key)?;
                if cmp_tuple(&lo, &hi) != Ordering::Less {
                    let render = |field: &str| {
                        texts(bound, field)
                            .iter()
                            .zip(&key)
                            .map(|(t, (_, ty))| {
                                partitions::render_datum(t.as_deref().unwrap_or("NULL"), ty)
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    let mut info = ErrorInfo::new(
                        "ERROR".into(),
                        "42P17".into(),
                        format!("empty range bound specified for partition \"{name}\""),
                    );
                    info.detail = Some(format!(
                        "Specified lower bound ({}) is greater than or equal to upper bound ({}).",
                        render("from"),
                        render("to")
                    ));
                    return Err(PgWireError::UserError(Box::new(info)));
                }
                for s in &siblings {
                    let b = bound_of(s);
                    if b.get_str("kind") != Ok("range") {
                        continue;
                    }
                    let lo2 = self.range_ends(&texts(&b, "from"), &key)?;
                    let hi2 = self.range_ends(&texts(&b, "to"), &key)?;
                    if cmp_tuple(&lo, &hi2) == Ordering::Less
                        && cmp_tuple(&lo2, &hi) == Ordering::Less
                    {
                        return Err(user_err(
                            "42P17",
                            format!(
                                "partition \"{name}\" would overlap partition \"{}\"",
                                s.name
                            ),
                        ));
                    }
                }
            }
            // Every modulus a factor of the next larger, and no two
            // partitions claiming one remainder class (PostgreSQL's rules).
            "hash" => {
                let m = bound.get_i32("modulus").unwrap_or(1);
                let r = bound.get_i32("remainder").unwrap_or(0);
                for s in &siblings {
                    let b = bound_of(s);
                    if b.get_str("kind") != Ok("hash") {
                        continue;
                    }
                    let (m2, r2) = (
                        b.get_i32("modulus").unwrap_or(1),
                        b.get_i32("remainder").unwrap_or(0),
                    );
                    let (small, big) = if m <= m2 { (m, m2) } else { (m2, m) };
                    if big % small != 0 {
                        let mut info = ErrorInfo::new(
                            "ERROR".into(),
                            "42P17".into(),
                            "every hash partition modulus must be a factor of the next larger modulus"
                                .into(),
                        );
                        info.detail = Some(format!(
                            "The new modulus {m} is not {} of modulus {m2}, the modulus of existing partition \"{}\".",
                            if m < m2 { "a factor" } else { "divisible by" },
                            s.name
                        ));
                        return Err(PgWireError::UserError(Box::new(info)));
                    }
                    let (rs, rb) = if m <= m2 { (r, r2) } else { (r2, r) };
                    if rb % small == rs {
                        return Err(user_err(
                            "42P17",
                            format!(
                                "partition \"{name}\" would overlap partition \"{}\"",
                                s.name
                            ),
                        ));
                    }
                }
            }
            "list" => {
                let ty = &key[0].1;
                let mine: Vec<Bson> = texts(bound, "values")
                    .iter()
                    .map(|t| match t {
                        Some(v) => self.datum_value(v, ty),
                        None => Ok(Bson::Null),
                    })
                    .collect::<PgWireResult<_>>()?;
                for s in &siblings {
                    let b = bound_of(s);
                    if b.get_str("kind") != Ok("list") {
                        continue;
                    }
                    for t in texts(&b, "values") {
                        let v = match t {
                            Some(v) => self.datum_value(&v, ty)?,
                            None => Bson::Null,
                        };
                        let clash = mine.iter().any(|m| match (m, &v) {
                            (Bson::Null, Bson::Null) => true,
                            (Bson::Null, _) | (_, Bson::Null) => false,
                            _ => secantus_pgplan::compare_values(m, &v) == Some(Ordering::Equal),
                        });
                        if clash {
                            return Err(user_err(
                                "42P17",
                                format!(
                                    "partition \"{name}\" would overlap partition \"{}\"",
                                    s.name
                                ),
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
        // A DEFAULT partition may already hold rows the new bound claims.
        if kind != "default" {
            if let Some(d) = siblings
                .iter()
                .find(|s| bound_of(s).get_str("kind") == Ok("default"))
            {
                let root = self.partition_root(&parent);
                let default_cond = self.partition_path_condition(d)?;
                let new_cond = partitions::bound_condition(&key_of(&parent), bound, &[]);
                for row in self.table_docs(&root.name)? {
                    if self.row_satisfies(&root, &default_cond, &row)?
                        && self.row_satisfies(&root, &new_cond, &row)?
                    {
                        return Err(user_err(
                            "23514",
                            format!(
                                "updated partition constraint for default partition \"{}\" would be violated by some row",
                                d.name
                            ),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn row_satisfies(&self, def: &TableDef, cond: &str, row: &Document) -> PgWireResult<bool> {
        let expr = secantus_pgplan::plan_check_expression(cond, def).map_err(|e| Self::err(&e))?;
        Ok(
            secantus_pgplan::apply_row_expr(&expr, row).map_err(|e| Self::err(&e))?
                == Bson::Boolean(true),
        )
    }

    /// A row written to the partitioned table `def` must land in a partition:
    /// walk down from `def` to a leaf, or fail naming the level that had none.
    pub(crate) fn check_partition_route(&self, def: &TableDef, row: &Document) -> PgWireResult<()> {
        if !is_partitioned(def) {
            return Ok(());
        }
        let mut level = def.clone();
        for _ in 0..64 {
            if !is_partitioned(&level) {
                return Ok(());
            }
            let mut found = None;
            for p in self.partitions_of(&level.name)? {
                if let Some(cond) = self.partition_condition(&p)? {
                    if self.row_satisfies(def, &cond, row)? {
                        found = Some(p);
                        break;
                    }
                }
            }
            match found {
                // The partition's OWN NOT NULLs and CHECKs hold for the rows
                // it takes, whichever table they were written through.
                Some(p) => {
                    let shaped = Self::reshape_rows(def, &p, std::slice::from_ref(row))?;
                    if let Some(r) = shaped.first() {
                        self.check_row_constraints(&p, r)?;
                    }
                    level = p;
                }
                None => {
                    let key = ordered_key(&level);
                    let names: Vec<&str> = key.iter().map(|(n, _)| n.as_str()).collect();
                    let values: Vec<String> = key
                        .iter()
                        .map(|(n, _)| {
                            let field = def.column(n).map(|c| c.field()).unwrap_or_default();
                            match row.get(&field) {
                                None | Some(Bson::Null) => "null".to_string(),
                                Some(v) => secantus_pgplan::value_text(v),
                            }
                        })
                        .collect();
                    let mut info = ErrorInfo::new(
                        "ERROR".into(),
                        "23514".into(),
                        format!("no partition of relation \"{}\" found for row", level.name),
                    );
                    info.detail = Some(format!(
                        "Partition key of the failing row contains ({}) = ({}).",
                        names.join(", "),
                        values.join(", ")
                    ));
                    return Err(PgWireError::UserError(Box::new(info)));
                }
            }
        }
        Ok(())
    }

    /// The partitions a row of `def` passes through, top level first.
    fn route_path(&self, def: &TableDef, row: &Document) -> PgWireResult<Vec<TableDef>> {
        let mut path = Vec::new();
        let mut level = def.clone();
        for _ in 0..64 {
            if !is_partitioned(&level) {
                break;
            }
            let mut next = None;
            for p in self.partitions_of(&level.name)? {
                if let Some(cond) = self.partition_condition(&p)? {
                    if self.row_satisfies(def, &cond, row)? {
                        next = Some(p);
                        break;
                    }
                }
            }
            match next {
                Some(p) => {
                    path.push(p.clone());
                    level = p;
                }
                None => break,
            }
        }
        Ok(path)
    }

    /// A partition's own PRIMARY KEY and UNIQUE constraints, over the rows
    /// the partition holds: `rows` (under the root `def`'s fields) against
    /// its stored rows other than `replacing`, and against one another.
    pub(crate) fn check_partition_unique(
        &self,
        def: &TableDef,
        rows: &[Document],
        replacing: &[Bson],
    ) -> PgWireResult<()> {
        if !is_partitioned(def) {
            return Ok(());
        }
        // (partition, constraint name, columns) -> keys already taken.
        let mut taken: Vec<(String, String, Vec<Vec<Bson>>)> = Vec::new();
        for row in rows {
            for p in self.route_path(def, row)? {
                let mut keys: Vec<(String, Vec<String>)> = Vec::new();
                let pk: Vec<String> = p
                    .columns
                    .iter()
                    .filter(|c| c.pk)
                    .map(|c| c.name.clone())
                    .collect();
                if !pk.is_empty() {
                    keys.push((crate::pk_constraint_name(&p), pk));
                }
                for u in p.unique_constraints.iter().filter(|u| !u.exclusion) {
                    keys.push((u.name.clone(), u.columns.clone()));
                }
                for (name, columns) in keys {
                    let key_of = |d: &Document| -> Option<Vec<Bson>> {
                        columns
                            .iter()
                            .map(|c| {
                                let field = def.column(c).map(|col| col.field())?;
                                match d.get(&field) {
                                    None | Some(Bson::Null) => None,
                                    Some(v) => Some(v.clone()),
                                }
                            })
                            .collect()
                    };
                    let Some(key) = key_of(row) else {
                        continue;
                    };
                    let slot = match taken
                        .iter()
                        .position(|(t, n, _)| *t == p.name && *n == name)
                    {
                        Some(i) => i,
                        None => {
                            let stored = match self.partition_rows(&p)? {
                                Some((_, docs)) => docs,
                                None => Vec::new(),
                            };
                            let existing: Vec<Vec<Bson>> = stored
                                .iter()
                                .filter(|d| !d.get("_id").is_some_and(|id| replacing.contains(id)))
                                .filter_map(key_of)
                                .collect();
                            taken.push((p.name.clone(), name.clone(), existing));
                            taken.len() - 1
                        }
                    };
                    if taken[slot].2.contains(&key) {
                        let text: Vec<String> =
                            key.iter().map(secantus_pgplan::value_text).collect();
                        return Err(Self::constraint_error(
                            "23505",
                            format!("duplicate key value violates unique constraint \"{name}\""),
                            format!(
                                "Key ({})=({}) already exists.",
                                columns.join(", "),
                                text.join(", ")
                            ),
                            &p,
                            Some(&name),
                            None,
                        ));
                    }
                    taken[slot].2.push(key);
                }
            }
        }
        Ok(())
    }

    /// The leaf partition a row of the partitioned table `def` lands in.
    fn leaf_for(&self, def: &TableDef, row: &Document) -> PgWireResult<Option<String>> {
        let mut level = def.clone();
        for _ in 0..64 {
            if !is_partitioned(&level) {
                return Ok(Some(level.name));
            }
            let mut next = None;
            for p in self.partitions_of(&level.name)? {
                if let Some(cond) = self.partition_condition(&p)? {
                    if self.row_satisfies(def, &cond, row)? {
                        next = Some(p);
                        break;
                    }
                }
            }
            match next {
                Some(p) => level = p,
                None => return Ok(None),
            }
        }
        Ok(None)
    }

    /// A unique violation in a partitioned table is reported against the
    /// LEAF partition's constraint (`m_lo_pkey`, not `m_pkey`), which is
    /// where PostgreSQL's index lives.
    pub(crate) fn relabel_partition_error(
        &self,
        def: &TableDef,
        rows: &[Document],
        err: PgWireError,
    ) -> PgWireError {
        if !is_partitioned(def) {
            return err;
        }
        let PgWireError::UserError(mut info) = err else {
            return err;
        };
        if info.code != "23505" {
            return PgWireError::UserError(info);
        }
        let leaves: Vec<Option<String>> = rows
            .iter()
            .map(|r| self.leaf_for(def, r).ok().flatten())
            .collect();
        if let Some(Some(leaf)) = leaves.first() {
            if leaves.iter().all(|l| l.as_deref() == Some(leaf.as_str())) {
                info.message = info
                    .message
                    .replace(&format!("\"{}_", def.name), &format!("\"{leaf}_"));
                // The relation is the leaf too.
                info.table = Some(leaf.clone());
                if let Some(c) = info.constraint.as_mut() {
                    if let Some(rest) = c.strip_prefix(&format!("{}_", def.name)) {
                        *c = format!("{leaf}_{rest}");
                    }
                }
            }
        }
        PgWireError::UserError(info)
    }

    /// Where `COPY ... FROM` into `def` writes: a partition's rows go to its
    /// tree's root (held to the partition's bound), a partitioned table's are
    /// routed. `(table, its def, rows shaped for it)`.
    pub(crate) fn copy_target(
        &self,
        def: &TableDef,
        rows: Vec<Document>,
    ) -> PgWireResult<(String, TableDef, Vec<Document>)> {
        if parent_of(def).is_none() {
            for row in &rows {
                self.check_partition_route(def, row)?;
            }
            return Ok((def.name.clone(), def.clone(), rows));
        }
        let root = self.partition_root(def);
        let cond = self.partition_path_condition(def)?;
        let shaped = Self::reshape_rows(def, &root, &rows)?;
        for row in &shaped {
            if !self.row_satisfies(&root, &cond, row)? {
                let mut info = ErrorInfo::new(
                    "ERROR".into(),
                    "23514".into(),
                    format!(
                        "new row for relation \"{}\" violates partition constraint",
                        def.name
                    ),
                );
                info.detail = Some(Self::failing_row_detail(&root, row));
                return Err(PgWireError::UserError(Box::new(info)));
            }
            self.check_partition_route(&root, row)?;
        }
        Ok((root.name.clone(), root, shaped))
    }

    /// A partition's rows, read from its tree's root, with the root's def
    /// (whose fields they are stored under). `None` for any other table.
    pub(crate) fn partition_rows(
        &self,
        def: &TableDef,
    ) -> PgWireResult<Option<(TableDef, Vec<Document>)>> {
        if parent_of(def).is_none() {
            return Ok(None);
        }
        let root = self.partition_root(def);
        let cond = self.partition_path_condition(def)?;
        let mut rows = Vec::new();
        for row in self.table_docs(&root.name)? {
            if self.row_satisfies(&root, &cond, &row)? {
                rows.push(row);
            }
        }
        Ok(Some((root, rows)))
    }

    pub(crate) fn internal_sql(&self, sql: &str) -> PgWireResult<()> {
        let host = PlHost { h: self };
        crate::plpgsql_fn::Host::execute(&host, sql, &[], &[])
            .map(|_| ())
            .map_err(|e| {
                let mut info = ErrorInfo::new("ERROR".into(), e.sqlstate, e.message);
                info.detail = e.detail;
                PgWireError::UserError(Box::new(info))
            })
    }

    /// Remove `part`'s rows from its tree's root: its `DROP` and `TRUNCATE`.
    pub(crate) fn delete_partition_rows(&self, part: &TableDef) -> PgWireResult<()> {
        let root = self.partition_root(part);
        let cond = self.partition_path_condition(part)?;
        self.internal_sql(&format!(
            "DELETE FROM {} WHERE {cond}",
            secantus_pgplan::scalar::quote_identifier(&root.name)
        ))
    }

    /// `docs` (stored under `from`'s fields) as rows of `to`: the values
    /// column by column, shaped as an INSERT into `to` stores them.
    pub(crate) fn reshape_rows(
        from: &TableDef,
        to: &TableDef,
        docs: &[Document],
    ) -> PgWireResult<Vec<Document>> {
        let names: Vec<String> = to.columns.iter().map(|c| c.name.clone()).collect();
        docs.iter()
            .map(|d| {
                let values: Vec<Bson> = to
                    .columns
                    .iter()
                    .map(|c| {
                        from.column(&c.name)
                            .and_then(|fc| d.get(fc.field()).cloned())
                            .unwrap_or(Bson::Null)
                    })
                    .collect();
                secantus_pgplan::insert_row(to, &names, true, values).map_err(|e| Self::err(&e))
            })
            .collect()
    }

    /// Insert already-shaped rows into `table` through the ordinary INSERT
    /// path, so constraints, uniqueness and routing all apply.
    pub(crate) fn insert_shaped(&self, table: &str, rows: Vec<Document>) -> PgWireResult<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let tz = self.session_timezone();
        let sql = format!(
            "INSERT INTO {} DEFAULT VALUES",
            secantus_pgplan::scalar::quote_identifier(table)
        );
        let stmt = secantus_pgplan::plan_with_session_types_and_subqueries(
            &sql,
            &|n| self.lookup(n),
            &[],
            &[],
            &tz,
            None,
        )
        .map_err(|e| Self::err(&e))?;
        let secantus_pgplan::Statement::Insert(mut ins) = stmt else {
            return Err(user_err("XX000", "could not plan the partition move"));
        };
        ins.rows = rows;
        ins.view_checks.clear();
        self.execute(secantus_pgplan::Statement::Insert(ins), 0)
            .map(|_| ())
    }

    /// `ALTER TABLE parent ATTACH PARTITION name FOR VALUES ...`: the
    /// table's rows move into the tree, each checked against the bound.
    pub(crate) fn attach_partition(
        &self,
        parent: &str,
        name: &str,
        bound: &Document,
    ) -> PgWireResult<()> {
        let parent_def = self
            .lookup(parent)
            .ok_or_else(|| user_err("42P01", format!("relation \"{parent}\" does not exist")))?;
        let mut def = self
            .lookup(name)
            .ok_or_else(|| user_err("42P01", format!("relation \"{name}\" does not exist")))?;
        if parent_of(&def).is_some() {
            return Err(user_err(
                "42P17",
                format!("\"{name}\" is already a partition"),
            ));
        }
        for c in &def.columns {
            if parent_def.column(&c.name).is_none() {
                return Err(user_err(
                    "42804",
                    format!(
                        "table \"{name}\" contains column \"{}\" not found in parent \"{parent}\"",
                        c.name
                    ),
                ));
            }
        }
        for c in &parent_def.columns {
            match def.column(&c.name) {
                None => {
                    return Err(user_err(
                        "42804",
                        format!("child table is missing column \"{}\"", c.name),
                    ))
                }
                Some(own) if own.pg_type != c.pg_type => {
                    return Err(user_err(
                        "42804",
                        format!(
                            "child table \"{name}\" has different type for column \"{}\"",
                            c.name
                        ),
                    ))
                }
                _ => {}
            }
        }
        let mut bound = bound.clone();
        self.prepare_partition(name, parent, &mut bound)?;
        let siblings: Vec<Document> = self.partitions_of(parent)?.iter().map(bound_of).collect();
        let cond = partitions::bound_condition(&key_of(&parent_def), &bound, &siblings);
        let docs = self.table_docs(name)?;
        for row in &docs {
            if !self.row_satisfies(&def, &cond, row)? {
                return Err(user_err(
                    "23514",
                    format!("partition constraint of relation \"{name}\" is violated by some row"),
                ));
            }
        }
        let root = self.partition_root(&parent_def);
        let rows = Self::reshape_rows(&def, &root, &docs)?;
        // The catalog first, so the rows route into the new partition; then
        // the rows leave the table's own storage for the tree's.
        def.extra.insert("partition_of", parent);
        def.extra.insert("partition_bound", bound);
        self.rewrite_catalog(name, &def)?;
        self.storage
            .delete_matching(self.db(), name, &Document::new(), 0, &Document::new(), None)
            .map_err(|e| Self::storage_err("could not attach the partition", e))?;
        self.insert_shaped(&root.name, rows)?;
        self.publish_user_types();
        Ok(())
    }

    /// `ALTER TABLE parent DETACH PARTITION name`: the partition becomes a
    /// table of its own and takes its rows with it.
    pub(crate) fn detach_partition(&self, parent: &str, name: &str) -> PgWireResult<()> {
        let mut def = self
            .lookup(name)
            .ok_or_else(|| user_err("42P01", format!("relation \"{name}\" does not exist")))?;
        if parent_of(&def) != Some(parent) {
            return Err(user_err(
                "42P01",
                format!("relation \"{name}\" is not a partition of relation \"{parent}\""),
            ));
        }
        let root = self.partition_root(&def);
        let cond = self.partition_path_condition(&def)?;
        let mut docs = Vec::new();
        for row in self.table_docs(&root.name)? {
            if self.row_satisfies(&root, &cond, &row)? {
                docs.push(row);
            }
        }
        def.extra.remove("partition_of");
        def.extra.remove("partition_bound");
        let rows = Self::reshape_rows(&root, &def, &docs)?;
        self.rewrite_catalog(name, &def)?;
        self.internal_sql(&format!(
            "DELETE FROM {} WHERE {cond}",
            secantus_pgplan::scalar::quote_identifier(&root.name)
        ))?;
        // The planner must see the table as a table again before its rows
        // are written back to it.
        self.publish_user_types();
        self.insert_shaped(name, rows)
    }

    /// A partition's columns are its parent's: the structural ALTERs are
    /// refused on it, as PostgreSQL does.
    pub(crate) fn refuse_partition_shape_change(
        action: &secantus_pgplan::AlterTableAction,
    ) -> PgWireResult<()> {
        use secantus_pgplan::AlterTableAction as A;
        let message = match action {
            A::AddColumn { .. } => "cannot add column to a partition".to_string(),
            A::DropColumn { name, .. } => format!("cannot drop inherited column \"{name}\""),
            A::AlterType { column, .. } => {
                format!("cannot alter inherited column \"{column}\"")
            }
            _ => return Ok(()),
        };
        Err(user_err("42P16", message))
    }

    /// After a partitioned table's columns change, its partitions' columns
    /// follow: they are the same columns.
    pub(crate) fn sync_partition_columns(&self, parent: &TableDef) -> PgWireResult<()> {
        for mut child in self.partitions_of(&parent.name)? {
            child.columns = parent.columns.clone();
            self.rewrite_catalog(&child.name.clone(), &child)?;
            if is_partitioned(&child) {
                self.sync_partition_columns(&child)?;
            }
        }
        Ok(())
    }

    /// `relpartbound` as `pg_get_expr` prints it.
    pub(crate) fn partition_bound_text(&self, part: &TableDef) -> Option<String> {
        let parent = self.lookup(parent_of(part)?)?;
        let key = ordered_key(&parent);
        let bound = bound_of(part);
        let render = |field: &str| -> String {
            texts(&bound, field)
                .iter()
                .enumerate()
                .map(|(i, t)| match t {
                    None => "NULL".to_string(),
                    Some(v) => partitions::render_datum(
                        v,
                        key.get(i.min(key.len().saturating_sub(1)))
                            .map(|(_, t)| t.as_str())
                            .unwrap_or("text"),
                    ),
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        Some(match bound.get_str("kind").unwrap_or_default() {
            "default" => "DEFAULT".into(),
            "list" => format!("FOR VALUES IN ({})", render("values")),
            "hash" => format!(
                "FOR VALUES WITH (modulus {}, remainder {})",
                bound.get_i32("modulus").unwrap_or(0),
                bound.get_i32("remainder").unwrap_or(0)
            ),
            _ => format!("FOR VALUES FROM ({}) TO ({})", render("from"), render("to")),
        })
    }

    /// `pg_partitioned_table.partstrat`, `partattrs`.
    pub(crate) fn partition_strategy(def: &TableDef) -> Option<(&'static str, Vec<i16>)> {
        let strat = match strategy(def) {
            "range" => "r",
            "list" => "l",
            "hash" => "h",
            _ => return None,
        };
        // An expression key is attribute 0, as in `pg_partitioned_table`.
        let attrs = key_names(def)
            .iter()
            .map(|n| {
                def.columns
                    .iter()
                    .position(|c| c.name == *n)
                    .map_or(0, |i| (i + 1) as i16)
            })
            .collect();
        Some((strat, attrs))
    }
}
