//! `EXPLAIN`: the SHAPE of a statement's plan, in PostgreSQL's layout.
//!
//! This server has no cost model, and inventing one would be the kind of
//! plausible-but-wrong answer the project refuses: a client comparing costs
//! would be comparing fiction. So the node TYPES and their nesting are real --
//! a scan is an `Index Scan` exactly when the storage planner would use the
//! index -- and the costs print as zeros (`COSTS OFF` omits them, which is
//! what a test comparing plans uses anyway).

use bson::Document;
use secantus_pgplan::joins::{JoinKind, JoinNode};
use secantus_pgplan::{ExplainOptions, Statement};

/// One plan node: its header, its detail lines, its children.
pub(crate) struct PlanNode {
    pub name: String,
    /// `(label, value)`: `Sort Key: id`, `Group Key: g`, `Index Name: ...`.
    pub details: Vec<(String, String)>,
    pub children: Vec<PlanNode>,
    /// JSON-only facts (`Relation Name`, `Index Name`).
    pub props: Vec<(String, String)>,
}

impl PlanNode {
    fn new(name: impl Into<String>) -> Self {
        PlanNode {
            name: name.into(),
            details: Vec::new(),
            children: Vec::new(),
            props: Vec::new(),
        }
    }

    fn over(name: impl Into<String>, child: PlanNode) -> Self {
        let mut n = PlanNode::new(name);
        n.children.push(child);
        n
    }
}

/// How a table scan will run: `Some((index, key))` for an index scan.
pub(crate) type ScanChooser<'a> = dyn Fn(&str, &Document) -> Option<String> + 'a;

/// The plan tree of a planned statement.
pub(crate) fn plan_tree(stmt: &Statement, scan: &ScanChooser<'_>) -> PlanNode {
    match stmt {
        Statement::Select(sel) => {
            let mut node = if let Some(sub) = &sel.sub {
                match sub.plan.as_ref() {
                    Statement::JoinRows(j) => join_tree(&j.tree, scan),
                    inner => {
                        let mut n = PlanNode::over(
                            format!("Subquery Scan on {}", display_alias(&sub.alias)),
                            plan_tree(inner, scan),
                        );
                        n.props.push(("Alias".into(), display_alias(&sub.alias)));
                        n
                    }
                }
            } else if sel.series.is_some() {
                PlanNode::new("Function Scan on generate_series")
            } else if sel.join.is_some() {
                let mut n = PlanNode::new("Nested Loop");
                n.children.push(PlanNode::new("Seq Scan"));
                n.children.push(PlanNode::new("Seq Scan"));
                n
            } else if sel.table.is_empty() {
                PlanNode::new("Result")
            } else {
                table_scan(&sel.table, &sel.filter, scan)
            };
            if sel.residual.is_some() && node.details.is_empty() && !sel.table.is_empty() {
                node.details.push(("Filter".into(), "(...)".into()));
            }
            if !sel.windows.is_empty() {
                node = PlanNode::over("WindowAgg", node);
            }
            if !sel.order.is_empty() {
                let mut sort = PlanNode::over("Sort", node);
                let keys: Vec<String> = sel
                    .order
                    .iter()
                    .map(|k| {
                        let name = sel
                            .columns
                            .iter()
                            .find(|(_, f)| *f == k.field)
                            .map_or_else(|| display_alias(&k.field), |(o, _)| o.clone());
                        if k.ascending {
                            name
                        } else {
                            format!("{name} DESC")
                        }
                    })
                    .collect();
                sort.details.push(("Sort Key".into(), keys.join(", ")));
                node = sort;
            }
            if !matches!(sel.distinct, secantus_pgplan::Distinct::None) {
                node = PlanNode::over("Unique", node);
            }
            if sel.limit.is_some() || sel.offset > 0 {
                node = PlanNode::over("Limit", node);
            }
            node
        }
        Statement::Aggregate(agg) => {
            let source = if let Some(sub) = &agg.sub {
                match sub.plan.as_ref() {
                    Statement::JoinRows(j) => join_tree(&j.tree, scan),
                    inner => PlanNode::over(
                        format!("Subquery Scan on {}", display_alias(&sub.alias)),
                        plan_tree(inner, scan),
                    ),
                }
            } else if agg.table.is_empty() {
                PlanNode::new("Result")
            } else {
                table_scan(&agg.table, &agg.filter, scan)
            };
            let mut node = if agg.group_by.is_empty() {
                PlanNode::over("Aggregate", source)
            } else {
                let mut n = PlanNode::over("HashAggregate", source);
                let keys: Vec<String> = agg.group_by.iter().map(|k| k.name.clone()).collect();
                n.details.push(("Group Key".into(), keys.join(", ")));
                n
            };
            if !agg.order.is_empty() {
                node = PlanNode::over("Sort", node);
            }
            if agg.limit.is_some() || agg.offset > 0 {
                node = PlanNode::over("Limit", node);
            }
            node
        }
        Statement::SetOp(set) => {
            let mut n = PlanNode::new("Append");
            n.children.push(plan_tree(&set.left, scan));
            n.children.push(plan_tree(&set.right, scan));
            n
        }
        Statement::SelectConstant(_) => PlanNode::new("Result"),
        Statement::ValuesConstant(_) => PlanNode::new("Values Scan on \"*VALUES*\""),
        Statement::Insert(ins) => {
            let child = match &ins.source {
                Some(src) => plan_tree(src, scan),
                None if ins.rows.len() > 1 => PlanNode::new("Values Scan on \"*VALUES*\""),
                None => PlanNode::new("Result"),
            };
            PlanNode::over(format!("Insert on {}", ins.table), child)
        }
        Statement::Update(u) => PlanNode::over(
            format!("Update on {}", u.table),
            table_scan(&u.table, &u.filter, scan),
        ),
        Statement::Delete(d) => PlanNode::over(
            format!("Delete on {}", d.table),
            table_scan(&d.table, &d.filter, scan),
        ),
        _ => PlanNode::new("Result"),
    }
}

/// A join's key names are `alias<US>column`; a plan names only the alias.
fn display_alias(name: &str) -> String {
    name.split(secantus_pgplan::joins::SEP).next().unwrap_or(name).to_string()
}

fn table_scan(table: &str, filter: &Document, scan: &ScanChooser<'_>) -> PlanNode {
    let mut n = match scan(table, filter) {
        Some(index) => {
            let mut n = PlanNode::new(format!("Index Scan using {index} on {table}"));
            n.props.push(("Index Name".into(), index));
            n
        }
        None => PlanNode::new(format!("Seq Scan on {table}")),
    };
    n.props.push(("Relation Name".into(), table.to_string()));
    n
}

fn join_tree(node: &JoinNode, scan: &ScanChooser<'_>) -> PlanNode {
    match node {
        JoinNode::Leaf { plan, def, .. } => {
            let mut n = plan_tree(plan, scan);
            if n.name == "Result" && !def.name.is_empty() {
                n.name = format!("Subquery Scan on {}", def.name);
            }
            n
        }
        JoinNode::Join {
            kind,
            left,
            right,
            equi,
            ..
        } => {
            let side = match kind {
                JoinKind::Inner => "",
                JoinKind::Left => " Left",
                JoinKind::Right => " Right",
                JoinKind::Full => " Full",
            };
            let name = if equi.is_empty() {
                format!("Nested Loop{side} Join").replace("Nested Loop Join", "Nested Loop")
            } else {
                format!("Hash{side} Join")
            };
            let mut n = PlanNode::new(name);
            n.children.push(join_tree(left, scan));
            let right = join_tree(right, scan);
            n.children.push(if equi.is_empty() {
                right
            } else {
                PlanNode::over("Hash", right)
            });
            n
        }
    }
}

const ZERO_COSTS: &str = "  (cost=0.00..0.00 rows=0 width=0)";

/// The `QUERY PLAN` text rows, PostgreSQL's layout: a child at depth d is
/// `2 + 6(d-1)` spaces and `->  `, and a node's details sit at `2 + 6d`.
pub(crate) fn render_text(root: &PlanNode, options: &ExplainOptions, actual_rows: Option<usize>) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(n: &PlanNode, depth: usize, options: &ExplainOptions, actual: Option<usize>, out: &mut Vec<String>) {
        let mut header = if depth == 0 {
            n.name.clone()
        } else {
            format!("{}->  {}", " ".repeat(2 + 6 * (depth - 1)), n.name)
        };
        if options.costs {
            header.push_str(ZERO_COSTS);
        }
        if let Some(rows) = actual {
            header.push_str(&format!(" (actual time=0.000..0.000 rows={rows} loops=1)"));
        }
        out.push(header);
        for (label, value) in &n.details {
            out.push(format!("{}{label}: {value}", " ".repeat(2 + 6 * depth)));
        }
        for c in &n.children {
            walk(c, depth + 1, options, None, out);
        }
    }
    walk(root, 0, options, actual_rows, &mut out);
    if actual_rows.is_some() {
        out.push("Planning Time: 0.000 ms".into());
        out.push("Execution Time: 0.000 ms".into());
    }
    out
}

/// The `QUERY PLAN` as PostgreSQL's `FORMAT JSON`: one array holding one
/// object whose `Plan` is the tree.
pub(crate) fn render_json(root: &PlanNode, options: &ExplainOptions, actual_rows: Option<usize>) -> String {
    fn node(n: &PlanNode, options: &ExplainOptions, actual: Option<usize>) -> serde_json_lite::Value {
        let mut fields: Vec<(String, serde_json_lite::Value)> = Vec::new();
        let node_type = n
            .name
            .split(" on ")
            .next()
            .unwrap_or(&n.name)
            .split(" using ")
            .next()
            .unwrap_or(&n.name)
            .to_string();
        fields.push(("Node Type".into(), serde_json_lite::Value::Str(node_type)));
        for (k, v) in &n.props {
            fields.push((k.clone(), serde_json_lite::Value::Str(v.clone())));
        }
        for (k, v) in &n.details {
            let value = if k.ends_with("Key") {
                serde_json_lite::Value::Arr(
                    v.split(", ").map(|s| serde_json_lite::Value::Str(s.to_string())).collect(),
                )
            } else {
                serde_json_lite::Value::Str(v.clone())
            };
            fields.push((k.clone(), value));
        }
        if options.costs {
            for k in ["Startup Cost", "Total Cost"] {
                fields.push((k.into(), serde_json_lite::Value::Num("0.00".into())));
            }
            for k in ["Plan Rows", "Plan Width"] {
                fields.push((k.into(), serde_json_lite::Value::Num("0".into())));
            }
        }
        if let Some(rows) = actual {
            fields.push(("Actual Rows".into(), serde_json_lite::Value::Num(rows.to_string())));
            fields.push(("Actual Loops".into(), serde_json_lite::Value::Num("1".into())));
        }
        if !n.children.is_empty() {
            fields.push((
                "Plans".into(),
                serde_json_lite::Value::Arr(n.children.iter().map(|c| node(c, options, None)).collect()),
            ));
        }
        serde_json_lite::Value::Obj(fields)
    }
    let mut top = vec![("Plan".to_string(), node(root, options, actual_rows))];
    if actual_rows.is_some() {
        top.push(("Planning Time".into(), serde_json_lite::Value::Num("0.000".into())));
        top.push(("Execution Time".into(), serde_json_lite::Value::Num("0.000".into())));
    }
    serde_json_lite::Value::Arr(vec![serde_json_lite::Value::Obj(top)]).pretty(0)
}

/// The little JSON writer `FORMAT JSON` needs, laid out as PostgreSQL lays it
/// out (two-space indent, `"key": value`).
mod serde_json_lite {
    pub enum Value {
        Str(String),
        Num(String),
        Arr(Vec<Value>),
        Obj(Vec<(String, Value)>),
    }

    fn quote(s: &str) -> String {
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    impl Value {
        pub fn pretty(&self, indent: usize) -> String {
            let pad = " ".repeat(indent);
            let inner = " ".repeat(indent + 2);
            match self {
                Value::Str(s) => quote(s),
                Value::Num(n) => n.clone(),
                Value::Arr(items) if items.is_empty() => "[]".into(),
                Value::Arr(items) => {
                    let parts: Vec<String> = items
                        .iter()
                        .map(|v| format!("{inner}{}", v.pretty(indent + 2)))
                        .collect();
                    format!("[\n{}\n{pad}]", parts.join(",\n"))
                }
                Value::Obj(fields) => {
                    let parts: Vec<String> = fields
                        .iter()
                        .map(|(k, v)| format!("{inner}{}: {}", quote(k), v.pretty(indent + 2)))
                        .collect();
                    format!("{{\n{}\n{pad}}}", parts.join(",\n"))
                }
            }
        }
    }
}
