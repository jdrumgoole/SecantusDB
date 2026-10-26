//! PostgreSQL 15's analysed parse nodes (`primnodes.h`, `parsenodes.h`), as
//! far as a stored rule's `Query` can hold them. Field names and order follow
//! the C structs, because `out.rs` writes them in `outfuncs.c`'s order.

pub type Oid = u32;

/// A datum as `outDatum` prints it.
#[derive(Clone, Debug, PartialEq)]
pub enum Datum {
    /// A pass-by-value datum: its 8 bytes in memory order, and the length
    /// `datumGetSize` reports.
    ByVal { bytes: [u8; 8], len: u32 },
    /// A pass-by-reference datum's bytes.
    ByRef(Vec<u8>),
}

impl Datum {
    pub fn from_i64(v: i64, len: u32) -> Self {
        Datum::ByVal {
            bytes: v.to_le_bytes(),
            len,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoercionForm {
    ExplicitCall = 0,
    ExplicitCast = 1,
    ImplicitCast = 2,
    SqlSyntax = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CoercionContext {
    Implicit = 0,
    Assignment = 1,
    Plpgsql = 2,
    Explicit = 3,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Var {
    pub varno: i32,
    pub varattno: i32,
    pub vartype: Oid,
    pub vartypmod: i32,
    pub varcollid: Oid,
    pub varlevelsup: u32,
    pub varnosyn: u32,
    pub varattnosyn: i32,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Const {
    pub consttype: Oid,
    pub consttypmod: i32,
    pub constcollid: Oid,
    pub constlen: i32,
    pub constbyval: bool,
    /// `None` is a NULL.
    pub constvalue: Option<Datum>,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub paramkind: i32,
    pub paramid: i32,
    pub paramtype: Oid,
    pub paramtypmod: i32,
    pub paramcollid: Oid,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Aggref {
    pub aggfnoid: Oid,
    pub aggtype: Oid,
    pub aggcollid: Oid,
    pub inputcollid: Oid,
    pub aggtranstype: Oid,
    pub aggargtypes: Vec<Oid>,
    pub aggdirectargs: Vec<Node>,
    pub args: Vec<TargetEntry>,
    pub aggorder: Vec<SortGroupClause>,
    pub aggdistinct: Vec<SortGroupClause>,
    pub aggfilter: Option<Node>,
    pub aggstar: bool,
    pub aggvariadic: bool,
    pub aggkind: char,
    pub agglevelsup: u32,
    pub aggsplit: i32,
    pub aggno: i32,
    pub aggtransno: i32,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GroupingFunc {
    pub args: Vec<Node>,
    pub refs: Vec<i32>,
    pub cols: Vec<i32>,
    pub agglevelsup: u32,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowFunc {
    pub winfnoid: Oid,
    pub wintype: Oid,
    pub wincollid: Oid,
    pub inputcollid: Oid,
    pub args: Vec<Node>,
    pub aggfilter: Option<Node>,
    pub winref: u32,
    pub winstar: bool,
    pub winagg: bool,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubscriptingRef {
    pub refcontainertype: Oid,
    pub refelemtype: Oid,
    pub refrestype: Oid,
    pub reftypmod: i32,
    pub refcollid: Oid,
    /// `None` entries are omitted bounds (`a[:2]`), written as `<>`.
    pub refupperindexpr: Vec<Option<Node>>,
    pub reflowerindexpr: Option<Vec<Option<Node>>>,
    pub refexpr: Node,
    pub refassgnexpr: Option<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FuncExpr {
    pub funcid: Oid,
    pub funcresulttype: Oid,
    pub funcretset: bool,
    pub funcvariadic: bool,
    pub funcformat: CoercionForm,
    pub funccollid: Oid,
    pub inputcollid: Oid,
    pub args: Vec<Node>,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NamedArgExpr {
    pub arg: Node,
    pub name: String,
    pub argnumber: i32,
    pub location: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    Op,
    Distinct,
    NullIf,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OpExpr {
    pub kind: OpKind,
    pub opno: Oid,
    pub opfuncid: Oid,
    pub opresulttype: Oid,
    pub opretset: bool,
    pub opcollid: Oid,
    pub inputcollid: Oid,
    pub args: Vec<Node>,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScalarArrayOpExpr {
    pub opno: Oid,
    pub opfuncid: Oid,
    pub hashfuncid: Oid,
    pub negfuncid: Oid,
    pub use_or: bool,
    pub inputcollid: Oid,
    pub args: Vec<Node>,
    pub location: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoolOp {
    And,
    Or,
    Not,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoolExpr {
    pub boolop: BoolOp,
    pub args: Vec<Node>,
    pub location: i32,
}

/// `SubLinkType`, in `primnodes.h` order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubLinkType {
    Exists = 0,
    All = 1,
    Any = 2,
    RowCompare = 3,
    Expr = 4,
    MultiExpr = 5,
    Array = 6,
    Cte = 7,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubLink {
    pub sub_link_type: SubLinkType,
    pub sub_link_id: i32,
    pub testexpr: Option<Node>,
    pub oper_name: Vec<String>,
    pub subselect: Box<Query>,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldSelect {
    pub arg: Node,
    pub fieldnum: i32,
    pub resulttype: Oid,
    pub resulttypmod: i32,
    pub resultcollid: Oid,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RelabelType {
    pub arg: Node,
    pub resulttype: Oid,
    pub resulttypmod: i32,
    pub resultcollid: Oid,
    pub relabelformat: CoercionForm,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CoerceViaIO {
    pub arg: Node,
    pub resulttype: Oid,
    pub resultcollid: Oid,
    pub coerceformat: CoercionForm,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrayCoerceExpr {
    pub arg: Node,
    pub elemexpr: Node,
    pub resulttype: Oid,
    pub resulttypmod: i32,
    pub resultcollid: Oid,
    pub coerceformat: CoercionForm,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConvertRowtypeExpr {
    pub arg: Node,
    pub resulttype: Oid,
    pub convertformat: CoercionForm,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CollateExpr {
    pub arg: Node,
    pub coll_oid: Oid,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CaseWhen {
    pub expr: Node,
    pub result: Node,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CaseExpr {
    pub casetype: Oid,
    pub casecollid: Oid,
    pub arg: Option<Node>,
    pub args: Vec<CaseWhen>,
    pub defresult: Node,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CaseTestExpr {
    pub type_id: Oid,
    pub type_mod: i32,
    pub collation: Oid,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrayExpr {
    pub array_typeid: Oid,
    pub array_collid: Oid,
    pub element_typeid: Oid,
    pub elements: Vec<Node>,
    pub multidims: bool,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RowExpr {
    pub args: Vec<Node>,
    pub row_typeid: Oid,
    pub row_format: CoercionForm,
    pub colnames: Vec<String>,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RowCompareExpr {
    pub rctype: i32,
    pub opnos: Vec<Oid>,
    pub opfamilies: Vec<Oid>,
    pub inputcollids: Vec<Oid>,
    pub largs: Vec<Node>,
    pub rargs: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CoalesceExpr {
    pub coalescetype: Oid,
    pub coalescecollid: Oid,
    pub args: Vec<Node>,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MinMaxExpr {
    pub minmaxtype: Oid,
    pub minmaxcollid: Oid,
    pub inputcollid: Oid,
    /// 0 greatest, 1 least.
    pub op: i32,
    pub args: Vec<Node>,
    pub location: i32,
}

/// `SQLValueFunctionOp`, PostgreSQL 15's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvfOp {
    CurrentDate = 0,
    CurrentTime = 1,
    CurrentTimeN = 2,
    CurrentTimestamp = 3,
    CurrentTimestampN = 4,
    Localtime = 5,
    LocaltimeN = 6,
    Localtimestamp = 7,
    LocaltimestampN = 8,
    CurrentRole = 9,
    CurrentUser = 10,
    User = 11,
    SessionUser = 12,
    CurrentCatalog = 13,
    CurrentSchema = 14,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SqlValueFunction {
    pub op: SvfOp,
    pub type_: Oid,
    pub typmod: i32,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NullTest {
    pub arg: Node,
    /// 0 IS NULL, 1 IS NOT NULL.
    pub nulltesttype: i32,
    pub argisrow: bool,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BooleanTest {
    pub arg: Node,
    pub booltesttype: i32,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CoerceToDomain {
    pub arg: Node,
    pub resulttype: Oid,
    pub resulttypmod: i32,
    pub resultcollid: Oid,
    pub coercionformat: CoercionForm,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SetToDefault {
    pub type_id: Oid,
    pub type_mod: i32,
    pub collation: Oid,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TargetEntry {
    pub expr: Node,
    pub resno: i32,
    pub resname: Option<String>,
    pub ressortgroupref: u32,
    pub resorigtbl: Oid,
    pub resorigcol: i32,
    pub resjunk: bool,
}

/// An analysed expression (and the few other nodes that can stand where
/// one does).
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Var(Var),
    Const(Const),
    Param(Param),
    Aggref(Box<Aggref>),
    GroupingFunc(Box<GroupingFunc>),
    WindowFunc(Box<WindowFunc>),
    SubscriptingRef(Box<SubscriptingRef>),
    FuncExpr(Box<FuncExpr>),
    NamedArgExpr(Box<NamedArgExpr>),
    OpExpr(Box<OpExpr>),
    ScalarArrayOpExpr(Box<ScalarArrayOpExpr>),
    BoolExpr(Box<BoolExpr>),
    SubLink(Box<SubLink>),
    FieldSelect(Box<FieldSelect>),
    RelabelType(Box<RelabelType>),
    CoerceViaIO(Box<CoerceViaIO>),
    ArrayCoerceExpr(Box<ArrayCoerceExpr>),
    ConvertRowtypeExpr(Box<ConvertRowtypeExpr>),
    CollateExpr(Box<CollateExpr>),
    CaseExpr(Box<CaseExpr>),
    CaseTestExpr(CaseTestExpr),
    ArrayExpr(Box<ArrayExpr>),
    RowExpr(Box<RowExpr>),
    RowCompareExpr(Box<RowCompareExpr>),
    CoalesceExpr(Box<CoalesceExpr>),
    MinMaxExpr(Box<MinMaxExpr>),
    SqlValueFunction(SqlValueFunction),
    NullTest(Box<NullTest>),
    BooleanTest(Box<BooleanTest>),
    CoerceToDomain(Box<CoerceToDomain>),
    SetToDefault(SetToDefault),
    TargetEntry(Box<TargetEntry>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Alias {
    pub aliasname: String,
    pub colnames: Vec<String>,
}

impl Alias {
    pub fn new(name: &str) -> Self {
        Alias {
            aliasname: name.to_string(),
            colnames: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RangeTblFunction {
    pub funcexpr: Node,
    pub funccolcount: i32,
    pub funccolnames: Vec<String>,
    pub funccoltypes: Vec<Oid>,
    pub funccoltypmods: Vec<i32>,
    pub funccolcollations: Vec<Oid>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RteKind {
    Relation {
        relid: Oid,
        relkind: char,
        rellockmode: i32,
    },
    Subquery {
        subquery: Box<Query>,
        security_barrier: bool,
    },
    Join {
        jointype: i32,
        joinmergedcols: i32,
        joinaliasvars: Vec<Node>,
        joinleftcols: Vec<i32>,
        joinrightcols: Vec<i32>,
        join_using_alias: Option<Alias>,
    },
    Function {
        functions: Vec<RangeTblFunction>,
        funcordinality: bool,
    },
    Values {
        values_lists: Vec<Vec<Node>>,
        coltypes: Vec<Oid>,
        coltypmods: Vec<i32>,
        colcollations: Vec<Oid>,
    },
    Cte {
        ctename: String,
        ctelevelsup: u32,
        self_reference: bool,
        coltypes: Vec<Oid>,
        coltypmods: Vec<i32>,
        colcollations: Vec<Oid>,
    },
    Result,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RangeTblEntry {
    pub alias: Option<Alias>,
    pub eref: Alias,
    pub kind: RteKind,
    pub lateral: bool,
    pub inh: bool,
    pub in_from_cl: bool,
    pub required_perms: u32,
    pub check_as_user: Oid,
    pub selected_cols: std::collections::BTreeSet<i32>,
    pub inserted_cols: std::collections::BTreeSet<i32>,
    pub updated_cols: std::collections::BTreeSet<i32>,
}

impl RangeTblEntry {
    pub fn relid(&self) -> Option<Oid> {
        match &self.kind {
            RteKind::Relation { relid, .. } => Some(*relid),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum JoinTreeNode {
    RangeTblRef(i32),
    JoinExpr(Box<JoinExpr>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct JoinExpr {
    pub jointype: i32,
    pub is_natural: bool,
    pub larg: JoinTreeNode,
    pub rarg: JoinTreeNode,
    pub using_clause: Vec<String>,
    pub join_using_alias: Option<Alias>,
    pub quals: Option<Node>,
    pub alias: Option<Alias>,
    pub rtindex: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FromExpr {
    pub fromlist: Vec<JoinTreeNode>,
    pub quals: Option<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SortGroupClause {
    pub tle_sort_group_ref: u32,
    pub eqop: Oid,
    pub sortop: Oid,
    pub nulls_first: bool,
    pub hashable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GroupingSet {
    pub kind: i32,
    pub content: Vec<GroupingSetContent>,
    pub location: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GroupingSetContent {
    Set(GroupingSet),
    Ref(i32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowClause {
    pub name: Option<String>,
    pub refname: Option<String>,
    pub partition_clause: Vec<SortGroupClause>,
    pub order_clause: Vec<SortGroupClause>,
    pub frame_options: i32,
    pub start_offset: Option<Node>,
    pub end_offset: Option<Node>,
    pub start_in_range_func: Oid,
    pub end_in_range_func: Oid,
    pub in_range_coll: Oid,
    pub in_range_asc: bool,
    pub in_range_nulls_first: bool,
    pub winref: u32,
    pub copied_order: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SetOpNode {
    RangeTblRef(i32),
    SetOp(Box<SetOperationStmt>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SetOperationStmt {
    pub op: i32,
    pub all: bool,
    pub larg: SetOpNode,
    pub rarg: SetOpNode,
    pub col_types: Vec<Oid>,
    pub col_typmods: Vec<i32>,
    pub col_collations: Vec<Oid>,
    pub group_clauses: Vec<SortGroupClause>,
}

/// `OnConflictExpr`.
#[derive(Clone, Debug, PartialEq)]
pub struct OnConflictExpr {
    pub action: i32,
    pub arbiter_elems: Vec<InferenceElem>,
    pub arbiter_where: Option<Node>,
    pub constraint: Oid,
    pub on_conflict_set: Vec<TargetEntry>,
    pub on_conflict_where: Option<Node>,
    pub excl_rel_index: i32,
    pub excl_rel_tlist: Vec<TargetEntry>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InferenceElem {
    pub expr: Node,
    pub infercollid: Oid,
    pub inferopclass: Oid,
}

/// `CTESearchClause`.
#[derive(Clone, Debug, PartialEq)]
pub struct CteSearchClause {
    pub search_col_list: Vec<String>,
    pub search_breadth_first: bool,
    pub search_seq_column: String,
    pub location: i32,
}

/// `CTECycleClause`.
#[derive(Clone, Debug, PartialEq)]
pub struct CteCycleClause {
    pub cycle_col_list: Vec<String>,
    pub cycle_mark_column: String,
    pub cycle_mark_value: Node,
    pub cycle_mark_default: Node,
    pub cycle_path_column: String,
    pub location: i32,
    pub cycle_mark_type: Oid,
    pub cycle_mark_typmod: i32,
    pub cycle_mark_collation: Oid,
    pub cycle_mark_neop: Oid,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommonTableExpr {
    pub ctename: String,
    pub aliascolnames: Vec<String>,
    pub ctematerialized: i32,
    pub ctequery: Box<Query>,
    pub location: i32,
    pub cterecursive: bool,
    pub cterefcount: i32,
    pub ctecolnames: Vec<String>,
    pub ctecoltypes: Vec<Oid>,
    pub ctecoltypmods: Vec<i32>,
    pub ctecolcollations: Vec<Oid>,
    pub search_clause: Option<CteSearchClause>,
    pub cycle_clause: Option<CteCycleClause>,
    /// Whether `ctequery` holds the analysed query yet (PostgreSQL tells by
    /// the node's tag); a reference before then is a recursive
    /// self-reference. Not part of the printed node.
    pub analyzed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RowMarkClause {
    pub rti: u32,
    pub strength: i32,
    pub wait_policy: i32,
    pub pushed_down: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UtilityStmt {
    Notify {
        conditionname: String,
        payload: Option<String>,
    },
}

/// `CmdType`.
pub const CMD_SELECT: i32 = 1;
pub const CMD_UPDATE: i32 = 2;
pub const CMD_INSERT: i32 = 3;
pub const CMD_DELETE: i32 = 4;
pub const CMD_UTILITY: i32 = 6;
pub const CMD_NOTHING: i32 = 7;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Query {
    pub command_type: i32,
    pub query_source: i32,
    pub can_set_tag: bool,
    pub utility_stmt: Option<UtilityStmt>,
    pub result_relation: i32,
    pub has_aggs: bool,
    pub has_window_funcs: bool,
    pub has_target_srfs: bool,
    pub has_sub_links: bool,
    pub has_distinct_on: bool,
    pub has_recursive: bool,
    pub has_modifying_cte: bool,
    pub has_for_update: bool,
    pub has_row_security: bool,
    pub is_return: bool,
    pub cte_list: Vec<CommonTableExpr>,
    pub rtable: Vec<RangeTblEntry>,
    pub jointree: Option<FromExpr>,
    pub target_list: Vec<TargetEntry>,
    pub override_: i32,
    pub on_conflict: Option<OnConflictExpr>,
    pub returning_list: Vec<TargetEntry>,
    pub group_clause: Vec<SortGroupClause>,
    pub group_distinct: bool,
    pub grouping_sets: Vec<GroupingSet>,
    pub having_qual: Option<Node>,
    pub window_clause: Vec<WindowClause>,
    pub distinct_clause: Vec<SortGroupClause>,
    pub sort_clause: Vec<SortGroupClause>,
    pub limit_offset: Option<Node>,
    pub limit_count: Option<Node>,
    pub limit_option: i32,
    pub row_marks: Vec<RowMarkClause>,
    pub set_operations: Option<SetOperationStmt>,
    pub constraint_deps: Vec<Oid>,
    pub stmt_location: i32,
    pub stmt_len: i32,
}
