// Both builds expand precisely these domain operations. NodeRef<'nodes> and the few snapshot
// helpers resolve to native methods in the engine, and to opaque-handle calls in the plugin.
#[macro_export]
macro_rules! drizzle_algorithm {
    () => {
const DRIZZLE_TABLE_BUILDERS: &[&str] =
    &["sqliteTable", "pgTable", "mysqlTable", "singlestoreTable"];

const SELECT_ENTRY_METHODS: &[&str] = &["select", "selectDistinct", "selectDistinctOn"];

const SELECT_CHAIN_METHODS: &[&str] = &[
    "from",
    "where",
    "orderBy",
    "groupBy",
    "having",
    "limit",
    "offset",
    "leftJoin",
    "rightJoin",
    "innerJoin",
    "fullJoin",
    "crossJoin",
    "get",
    "all",
    "values",
    "execute",
    "prepare",
];


#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryRowColumnInfoPayload {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_name: Option<String>,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nullable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Vec<EvidenceItem>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryRowNestedRelationInfoPayload {
    pub name: String,
    pub cardinality: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_name: Option<String>,
    pub columns: Vec<String>,
    pub column_details: BTreeMap<String, QueryRowColumnInfoPayload>,
    pub relations: BTreeMap<String, Self>,
    pub has_dynamic_columns: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Vec<EvidenceItem>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryRowProjectionValuePayload {
    pub library: &'static str,
    pub query_kind: &'static str,
    pub cardinality: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_name: Option<String>,
    pub projected_columns: Vec<String>,
    pub columns: BTreeMap<String, QueryRowColumnInfoPayload>,
    pub relations: BTreeMap<String, QueryRowNestedRelationInfoPayload>,
    pub has_dynamic_columns: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DrizzleTableColumnInfoPayload {
    pub name: String,
    pub sql_name: String,
    pub nullable: bool,
    pub primary_key: bool,
    pub has_default: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Vec<EvidenceItem>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DrizzleTableColumnsValuePayload {
    pub library: &'static str,
    pub table_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql_table_name: Option<String>,
    pub column_names: Vec<String>,
    pub columns: BTreeMap<String, DrizzleTableColumnInfoPayload>,
    pub has_dynamic_columns: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineFactContributionPayload {
    pub subject: String,
    pub subject_location: SourceLocation,
    pub value: QueryRowProjectionValuePayload,
    pub evidence: Vec<EvidenceItem>,
    pub gaps: Vec<GapItem>,
}

struct ColumnChainMeta {
    sql_name: String,
    nullable: bool,
    primary_key: bool,
    has_default: bool,
}

#[derive(Clone)]
struct TableColumnDef {
    ts_name: String,
    sql_name: String,
    nullable: bool,
    primary_key: bool,
    has_default: bool,
    location: SourceLocation,
}

#[derive(Clone)]
struct ExtractedTable<'nodes> {
    decl_node: Option<NodeRef<'nodes>>,
    ts_var_name: Option<String>,
    sql_table_name: Option<String>,
    display_name: String,
    location: SourceLocation,
    columns: BTreeMap<String, TableColumnDef>,
    has_dynamic_columns: bool,
    gaps: Vec<GapItem>,
}

#[derive(Clone)]
struct RelationTargetInfo<'nodes> {
    cardinality: &'static str,
    target_table: Option<ExtractedTable<'nodes>>,
    location: SourceLocation,
}

#[derive(Default)]
struct RelationsIndex<'nodes> {
    by_table_decl: FxHashMap<NodeRef<'nodes>, BTreeMap<String, RelationTargetInfo<'nodes>>>,
    by_table_name: FxHashMap<String, BTreeMap<String, RelationTargetInfo<'nodes>>>,
    schema_tables: BTreeMap<String, ExtractedTable<'nodes>>,
    all_tables: Vec<ExtractedTable<'nodes>>,
}

struct SelectFieldsResult {
    columns: BTreeMap<String, QueryRowColumnInfoPayload>,
    has_dynamic_columns: bool,
    gaps: Vec<GapItem>,
    evidence: Vec<EvidenceItem>,
}

struct RelationalEvalResult {
    columns: BTreeMap<String, QueryRowColumnInfoPayload>,
    relations: BTreeMap<String, QueryRowNestedRelationInfoPayload>,
    has_dynamic_columns: bool,
    gaps: Vec<GapItem>,
    evidence: Vec<EvidenceItem>,
}

struct RelationalWithResult {
    relations: BTreeMap<String, QueryRowNestedRelationInfoPayload>,
    has_dynamic_columns: bool,
    gaps: Vec<GapItem>,
    evidence: Vec<EvidenceItem>,
}

struct ColumnRefMeta {
    table_name: Option<String>,
    sql_name: Option<String>,
    nullable: Option<bool>,
    is_table_column: bool,
    evidence: Vec<EvidenceItem>,
}

struct FluentStep<'nodes> {
    call: NodeRef<'nodes>,
    method_name: String,
}

fn compare_utf16(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn strip_string_quotes(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
            || (trimmed.starts_with('`') && trimmed.ends_with('`')))
    {
        return trimmed[1..trimmed.len() - 1].to_string();
    }
    trimmed.to_string()
}


fn location_key(loc: &SourceLocation) -> (String, u32, u32) {
    (loc.file.clone(), loc.start, loc.end)
}

fn dedupe_evidence(items: Vec<EvidenceItem>) -> Vec<EvidenceItem> {
    let mut seen = FxHashSet::default();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if let Ok(key) = serde_json::to_string(&item)
            && seen.insert(key)
        {
            out.push(item);
        }
    }
    out
}

fn dedupe_gaps(items: Vec<GapItem>) -> Vec<GapItem> {
    let mut seen = FxHashSet::default();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if let Ok(key) = serde_json::to_string(&item)
            && seen.insert(key)
        {
            out.push(item);
        }
    }
    out
}

fn build_projection_value(
    query_kind: &'static str,
    cardinality: &'static str,
    table_name: Option<String>,
    columns: BTreeMap<String, QueryRowColumnInfoPayload>,
    relations: BTreeMap<String, QueryRowNestedRelationInfoPayload>,
    has_dynamic_columns: bool,
) -> QueryRowProjectionValuePayload {
    let mut keys: Vec<String> = columns.keys().chain(relations.keys()).cloned().collect();
    keys.sort_by(|a, b| compare_utf16(a, b));
    keys.dedup();
    QueryRowProjectionValuePayload {
        library: "drizzle",
        query_kind,
        cardinality,
        table_name,
        projected_columns: keys,
        columns,
        relations,
        has_dynamic_columns,
    }
}

const fn is_string_like_literal(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::StringLiteral | Kind::NumericLiteral | Kind::NoSubstitutionTemplateLiteral
    )
}

fn find_single_block_return_expr<'nodes>(block: NodeRef<'nodes>) -> Option<NodeRef<'nodes>> {
    let mut returns = Vec::new();
    collect_nodes_in_subtree(block, Kind::ReturnStatement, &mut returns);
    if returns.len() == 1 {
        returns[0].expression()
    } else {
        None
    }
}

fn collect_nodes_in_subtree<'nodes>(root: NodeRef<'nodes>, target_kind: Kind, out: &mut Vec<NodeRef<'nodes>>) {
    if root.kind() == target_kind {
        out.push(root);
    }
    root.for_each_child(&mut |child| {
        collect_nodes_in_subtree(child, target_kind, out);
        false
    });
}


struct DrizzleEvaluator<'nodes, 'checker> {
    cwd: &'checker str,
    checker: &'checker mut CheckerHandle,
    table_cache: FxHashMap<NodeRef<'nodes>, Option<ExtractedTable<'nodes>>>,
}

impl<'nodes, 'checker> DrizzleEvaluator<'nodes, 'checker> {
    fn new(cwd: &'checker str, checker: &'checker mut CheckerHandle) -> Self {
        Self {
            cwd,
            checker,
            table_cache: FxHashMap::default(),
        }
    }

    fn loc(&self, node: NodeRef<'nodes>) -> Option<SourceLocation> {
        node_location_rel(self.cwd, node)
    }

    fn type_text(&mut self, node: NodeRef<'nodes>) -> String {
        read_type_text(self.checker, node)
    }

    fn resolve_property_key(
        &self,
        name_node: NodeRef<'nodes>,
        member_loc: &SourceLocation,
        gap_kind: &'static str,
    ) -> Result<String, Option<GapItem>> {
        match name_node.kind() {
            Kind::Identifier => node_text(name_node).ok_or(None),
            kind if is_string_like_literal(kind) => node_text(name_node)
                .map(|t| strip_string_quotes(&t))
                .ok_or(None),
            Kind::ComputedPropertyName => {
                if let Some(expr) = name_node.expression() {
                    let inner = unwrap_expr_wrappers(expr);
                    if is_string_like_literal(inner.kind())
                        && let Some(text) = node_text(inner)
                    {
                        return Ok(strip_string_quotes(&text));
                    }
                }
                let text = node_text(name_node).unwrap_or_default();
                let at = self.loc(name_node).unwrap_or_else(|| member_loc.clone());
                Err(Some(GapItem {
                    kind: gap_kind,
                    message: format!(
                        "Computed property name \"{text}\" cannot be statically resolved."
                    ),
                    at: Some(at),
                }))
            }
            _ => {
                let text = node_text(name_node).unwrap_or_default();
                let at = self.loc(name_node).unwrap_or_else(|| member_loc.clone());
                Err(Some(GapItem {
                    kind: gap_kind,
                    message: format!(
                        "Dynamic property key \"{text}\" ({}) cannot be statically resolved.",
                        kind_label(name_node)
                    ),
                    at: Some(at),
                }))
            }
        }
    }

    fn extract_column_chain_metadata(ts_name: &str, expr_node: NodeRef<'nodes>) -> ColumnChainMeta {
        let mut current = Some(unwrap_expr_wrappers(expr_node));
        let mut seen = FxHashSet::default();
        let mut nullable = true;
        let mut primary_key = false;
        let mut has_default = false;
        let mut sql_name = ts_name.to_string();

        while let Some(node) = current {
            if !seen.insert(node) {
                break;
            }
            if node.kind() != Kind::CallExpression {
                break;
            }
            let Some(expr) = node.expression() else {
                break;
            };
            let callee = unwrap_expr_wrappers(expr);
            match callee.kind() {
                Kind::PropertyAccessExpression => {
                    let Some(recv) = callee.expression() else {
                        break;
                    };
                    let method_name = callee.name().and_then(node_text).unwrap_or_default();
                    if method_name == "notNull" {
                        nullable = false;
                        current = Some(unwrap_expr_wrappers(recv));
                        continue;
                    }
                    if method_name == "primaryKey" {
                        nullable = false;
                        primary_key = true;
                        current = Some(unwrap_expr_wrappers(recv));
                        continue;
                    }
                    if matches!(
                        method_name.as_str(),
                        "default" | "defaultRandom" | "defaultNow" | "$defaultFn" | "$default"
                    ) {
                        has_default = true;
                        current = Some(unwrap_expr_wrappers(recv));
                        continue;
                    }
                    let recv_unwrapped = unwrap_expr_wrappers(recv);
                    if recv_unwrapped.kind() == Kind::CallExpression {
                        current = Some(recv_unwrapped);
                        continue;
                    }
                    if let Some(&first_arg) = node.arguments().first() {
                        let first = unwrap_expr_wrappers(first_arg);
                        if matches!(
                            first.kind(),
                            Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral
                        ) && let Some(text) = node_text(first)
                        {
                            sql_name = strip_string_quotes(&text);
                        }
                    }
                    break;
                }
                Kind::Identifier => {
                    if let Some(&first_arg) = node.arguments().first() {
                        let first = unwrap_expr_wrappers(first_arg);
                        if matches!(
                            first.kind(),
                            Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral
                        ) && let Some(text) = node_text(first)
                        {
                            sql_name = strip_string_quotes(&text);
                        }
                    }
                    break;
                }
                _ => break,
            }
        }

        ColumnChainMeta {
            sql_name,
            nullable,
            primary_key,
            has_default,
        }
    }

    fn extract_table_columns_object(
        &mut self,
        obj_node: NodeRef<'nodes>,
        visited_decls: &FxHashSet<NodeRef<'nodes>>,
    ) -> (BTreeMap<String, TableColumnDef>, bool, Vec<GapItem>) {
        let mut columns = BTreeMap::new();
        let mut has_dynamic_columns = false;
        let mut gaps = Vec::new();

        for &child in properties(obj_node).iter() {
            let Some(child_loc) = self.loc(child) else {
                continue;
            };
            match child.kind() {
                Kind::PropertyAssignment => {
                    let Some(name_node) = child.name() else {
                        continue;
                    };
                    let Some(init_node) = child.initializer() else {
                        continue;
                    };
                    match self.resolve_property_key(name_node, &child_loc, "dynamic-table-columns") {
                        Ok(key) => {
                            let meta = Self::extract_column_chain_metadata(&key, init_node);
                            columns.insert(
                                key.clone(),
                                TableColumnDef {
                                    ts_name: key,
                                    sql_name: meta.sql_name,
                                    nullable: meta.nullable,
                                    primary_key: meta.primary_key,
                                    has_default: meta.has_default,
                                    location: child_loc,
                                },
                            );
                        }
                        Err(gap) => {
                            has_dynamic_columns = true;
                            if let Some(g) = gap {
                                gaps.push(g);
                            }
                        }
                    }
                }
                Kind::ShorthandPropertyAssignment => {
                    let key_name = child
                        .name()
                        .and_then(node_text)
                        .or_else(|| node_text(child))
                        .unwrap_or_default();
                    let meta = resolve_decl_for_ref(self.cwd, self.checker, child)
                        .and_then(|(_, decl_node)| safe_initializer(decl_node))
                        .map(|init| Self::extract_column_chain_metadata(&key_name, init))
                        .unwrap_or(ColumnChainMeta {
                            sql_name: key_name.clone(),
                            nullable: true,
                            primary_key: false,
                            has_default: false,
                        });
                    columns.insert(
                        key_name.clone(),
                        TableColumnDef {
                            ts_name: key_name,
                            sql_name: meta.sql_name,
                            nullable: meta.nullable,
                            primary_key: meta.primary_key,
                            has_default: meta.has_default,
                            location: child_loc,
                        },
                    );
                }
                Kind::SpreadAssignment => {
                    if !self.merge_table_spread_member(
                        child,
                        &child_loc,
                        visited_decls,
                        &mut columns,
                        &mut has_dynamic_columns,
                        &mut gaps,
                    ) {
                        has_dynamic_columns = true;
                        let spread_text = node_text(child).unwrap_or_default();
                        gaps.push(GapItem {
                            kind: "dynamic-table-columns",
                            message: format!(
                                "Spread \"{spread_text}\" in Drizzle table definition cannot be statically resolved."
                            ),
                            at: Some(child_loc),
                        });
                    }
                }
                _ => {
                    let member_text = node_text(child).unwrap_or_default();
                    has_dynamic_columns = true;
                    gaps.push(GapItem {
                        kind: "dynamic-table-columns",
                        message: format!(
                            "Unsupported dynamic member \"{member_text}\" ({}) in Drizzle table definition.",
                            kind_label(child)
                        ),
                        at: Some(child_loc),
                    });
                }
            }
        }

        (columns, has_dynamic_columns, gaps)
    }

    fn merge_table_spread_member(
        &mut self,
        child: NodeRef<'nodes>,
        _child_loc: &SourceLocation,
        visited_decls: &FxHashSet<NodeRef<'nodes>>,
        columns: &mut BTreeMap<String, TableColumnDef>,
        has_dynamic_columns: &mut bool,
        gaps: &mut Vec<GapItem>,
    ) -> bool {
        let Some(expr) = child.expression() else {
            return false;
        };
        let unwrapped = unwrap_expr_wrappers(expr);
        if unwrapped.kind() == Kind::ObjectLiteralExpression {
            let (nested_cols, nested_dyn, nested_gaps) =
                self.extract_table_columns_object(unwrapped, visited_decls);
            columns.extend(nested_cols);
            if nested_dyn {
                *has_dynamic_columns = true;
            }
            gaps.extend(nested_gaps);
            return true;
        }
        if unwrapped.kind() == Kind::Identifier
            && let Some((_, decl_node)) = resolve_decl_for_ref(self.cwd, self.checker, unwrapped)
            && !visited_decls.contains(&decl_node)
            && let Some(init) = safe_initializer(decl_node)
        {
            let init_unwrapped = unwrap_expr_wrappers(init);
            if init_unwrapped.kind() == Kind::ObjectLiteralExpression {
                let mut next_visited = visited_decls.clone();
                next_visited.insert(decl_node);
                let (nested_cols, nested_dyn, nested_gaps) =
                    self.extract_table_columns_object(init_unwrapped, &next_visited);
                columns.extend(nested_cols);
                if nested_dyn {
                    *has_dynamic_columns = true;
                }
                gaps.extend(nested_gaps);
                return true;
            }
        }
        false
    }

    fn evaluate_table_expr(
        &mut self,
        node: NodeRef<'nodes>,
        visited_decls: &FxHashSet<NodeRef<'nodes>>,
    ) -> Option<ExtractedTable<'nodes>> {
        let unwrapped = unwrap_expr_wrappers(node);
        if visited_decls.is_empty()
            && let Some(cached) = self.table_cache.get(&unwrapped)
        {
            return cached.clone();
        }
        let result = self.evaluate_table_unwrapped(unwrapped, visited_decls);
        if visited_decls.is_empty() {
            self.table_cache.insert(unwrapped, result.clone());
        }
        result
    }

    fn evaluate_table_unwrapped(
        &mut self,
        node: NodeRef<'nodes>,
        visited_decls: &FxHashSet<NodeRef<'nodes>>,
    ) -> Option<ExtractedTable<'nodes>> {
        if node.kind() == Kind::VariableDeclaration {
            let init = node.initializer()?;
            let mut next_visited = visited_decls.clone();
            next_visited.insert(node);
            let mut table = self.evaluate_table_expr(init, &next_visited)?;
            let var_name = node
                .name()
                .and_then(node_text)
                .unwrap_or_else(|| table.display_name.clone());
            table.decl_node = Some(node);
            table.ts_var_name = Some(var_name.clone());
            table.display_name = var_name;
            return Some(table);
        }

        if node.kind() == Kind::Identifier {
            let (decl_name, decl_node) = resolve_decl_for_ref(self.cwd, self.checker, node)?;
            if !visited_decls.contains(&decl_node)
                && let Some(init) = safe_initializer(decl_node)
            {
                let mut next_visited = visited_decls.clone();
                next_visited.insert(decl_node);
                if let Some(mut table) = self.evaluate_table_expr(init, &next_visited) {
                    let var_name = decl_name.or_else(|| node_text(node))?;
                    table.decl_node = Some(decl_node);
                    table.ts_var_name = Some(var_name.clone());
                    table.display_name = var_name;
                    return Some(table);
                }
            }
            return None;
        }

        if node.kind() != Kind::CallExpression {
            return None;
        }
        let callee = unwrap_expr_wrappers(node.expression()?);
        let is_table_call = match callee.kind() {
            Kind::Identifier => {
                node_text(callee).is_some_and(|t| DRIZZLE_TABLE_BUILDERS.contains(&t.as_str()))
            }
            Kind::PropertyAccessExpression => callee
                .name()
                .and_then(node_text)
                .is_some_and(|p| DRIZZLE_TABLE_BUILDERS.contains(&p.as_str()) || p == "table"),
            Kind::CallExpression => callee
                .expression()
                .map(unwrap_expr_wrappers)
                .filter(|n| n.kind() == Kind::Identifier)
                .and_then(node_text)
                .is_some_and(|t| t.ends_with("TableCreator")),
            _ => false,
        };
        if !is_table_call {
            return None;
        }

        let args = node.arguments();
        if args.len() < 2 {
            return None;
        }

        let name_arg = unwrap_expr_wrappers(args[0]);
        let sql_table_name = matches!(
            name_arg.kind(),
            Kind::StringLiteral | Kind::NoSubstitutionTemplateLiteral
        )
        .then(|| node_text(name_arg).map(|t| strip_string_quotes(&t)))
        .flatten();

        let cols_arg = unwrap_expr_wrappers(args[1]);
        let cols_obj = match cols_arg.kind() {
            Kind::ObjectLiteralExpression => Some(cols_arg),
            Kind::ArrowFunction | Kind::FunctionExpression => cols_arg.body().and_then(|body| {
                let body_unwrapped = unwrap_expr_wrappers(body);
                match body_unwrapped.kind() {
                    Kind::ObjectLiteralExpression => Some(body_unwrapped),
                    Kind::Block => find_single_block_return_expr(body_unwrapped)
                        .map(unwrap_expr_wrappers)
                        .filter(|n| n.kind() == Kind::ObjectLiteralExpression),
                    _ => None,
                }
            }),
            _ => None,
        }?;

        let (columns, has_dynamic_columns, gaps) =
            self.extract_table_columns_object(cols_obj, visited_decls);
        let loc = self.loc(node)?;
        Some(ExtractedTable {
            decl_node: None,
            ts_var_name: None,
            display_name: sql_table_name
                .clone()
                .unwrap_or_else(|| "table".to_string()),
            sql_table_name,
            location: loc,
            columns,
            has_dynamic_columns,
            gaps,
        })
    }

    fn resolve_object_literal(
        &mut self,
        node: NodeRef<'nodes>,
        visited_decls: &FxHashSet<NodeRef<'nodes>>,
    ) -> Option<NodeRef<'nodes>> {
        let unwrapped = unwrap_expr_wrappers(node);
        if unwrapped.kind() == Kind::ObjectLiteralExpression {
            return Some(unwrapped);
        }
        if unwrapped.kind() == Kind::Identifier
            && let Some((_, decl_node)) = resolve_decl_for_ref(self.cwd, self.checker, unwrapped)
            && !visited_decls.contains(&decl_node)
            && let Some(init) = safe_initializer(decl_node)
        {
            let mut next_visited = visited_decls.clone();
            next_visited.insert(decl_node);
            return self.resolve_object_literal(init, &next_visited);
        }
        None
    }

    fn build_relations_index(
        &mut self,
        calls: &[NodeRef<'nodes>],
        var_decls: &[NodeRef<'nodes>],
    ) -> RelationsIndex<'nodes> {
        let mut index = RelationsIndex::default();
        let empty = FxHashSet::default();

        for &decl in var_decls {
            let Some(init) = decl.initializer() else {
                continue;
            };
            if unwrap_expr_wrappers(init).kind() == Kind::CallExpression
                && let Some(table) = self.evaluate_table_expr(decl, &empty)
            {
                index.all_tables.push(table);
            }
        }

        for &call in calls {
            let Some(expr) = call.expression() else {
                continue;
            };
            let callee = unwrap_expr_wrappers(expr);
            let callee_name = match callee.kind() {
                Kind::Identifier => node_text(callee).unwrap_or_default(),
                Kind::PropertyAccessExpression => {
                    callee.name().and_then(node_text).unwrap_or_default()
                }
                _ => continue,
            };

            if callee_name == "relations" {
                self.index_relations_call(call, &empty, &mut index);
            } else if callee_name == "drizzle" {
                self.index_drizzle_schema_call(call, &empty, &mut index);
            }
        }

        index
    }

    fn index_relations_call(
        &mut self,
        call: NodeRef<'nodes>,
        empty: &FxHashSet<NodeRef<'nodes>>,
        index: &mut RelationsIndex<'nodes>,
    ) {
        let args = call.arguments();
        if args.len() < 2 {
            return;
        }
        let Some(source_table) = self.evaluate_table_expr(args[0], empty) else {
            return;
        };
        let cb = unwrap_expr_wrappers(args[1]);
        if !matches!(cb.kind(), Kind::ArrowFunction | Kind::FunctionExpression) {
            return;
        }
        let Some(body) = cb.body() else {
            return;
        };
        let body_unwrapped = unwrap_expr_wrappers(body);
        let obj_node = match body_unwrapped.kind() {
            Kind::ObjectLiteralExpression => Some(body_unwrapped),
            Kind::Block => find_single_block_return_expr(body_unwrapped)
                .map(unwrap_expr_wrappers)
                .filter(|n| n.kind() == Kind::ObjectLiteralExpression),
            _ => None,
        };
        let Some(obj) = obj_node else {
            return;
        };

        for &child in properties(obj).iter() {
            if child.kind() != Kind::PropertyAssignment {
                continue;
            }
            let (Some(name_node), Some(init_node), Some(child_loc)) =
                (child.name(), child.initializer(), self.loc(child))
            else {
                continue;
            };
            let Ok(rel_key) = self.resolve_property_key(name_node, &child_loc, "dynamic-query-projection") else {
                continue;
            };
            let init_unwrapped = unwrap_expr_wrappers(init_node);
            if init_unwrapped.kind() != Kind::CallExpression {
                continue;
            }
            let Some(rel_expr) = init_unwrapped.expression() else {
                continue;
            };
            let rel_callee = unwrap_expr_wrappers(rel_expr);
            let rel_kind = match rel_callee.kind() {
                Kind::Identifier => node_text(rel_callee),
                Kind::PropertyAccessExpression => rel_callee.name().and_then(node_text),
                _ => None,
            };
            let cardinality = match rel_kind.as_deref() {
                Some("one") => "single",
                Some("many") => "many",
                _ => continue,
            };
            let target_table = init_unwrapped
                .arguments()
                .first()
                .and_then(|&arg| self.evaluate_table_expr(arg, empty));
            let info = RelationTargetInfo {
                cardinality,
                target_table,
                location: child_loc,
            };
            if let Some(decl_node) = source_table.decl_node {
                index
                    .by_table_decl
                    .entry(decl_node)
                    .or_default()
                    .insert(rel_key.clone(), info.clone());
            }
            for key in [&source_table.ts_var_name, &source_table.sql_table_name]
                .into_iter()
                .flatten()
            {
                index
                    .by_table_name
                    .entry(key.clone())
                    .or_default()
                    .insert(rel_key.clone(), info.clone());
            }
        }
    }

    fn index_drizzle_schema_call(
        &mut self,
        call: NodeRef<'nodes>,
        empty: &FxHashSet<NodeRef<'nodes>>,
        index: &mut RelationsIndex<'nodes>,
    ) {
        let Some(&opts_arg) = call.arguments().get(1) else {
            return;
        };
        let opts_unwrapped = unwrap_expr_wrappers(opts_arg);
        if opts_unwrapped.kind() != Kind::ObjectLiteralExpression {
            return;
        }
        for &prop in properties(opts_unwrapped).iter()
        {
            if prop.kind() != Kind::PropertyAssignment {
                continue;
            }
            let (Some(name_node), Some(init_node), Some(prop_loc)) =
                (prop.name(), prop.initializer(), self.loc(prop))
            else {
                continue;
            };
            if self
                .resolve_property_key(name_node, &prop_loc, "dynamic-query-projection")
                .ok()
                .as_deref()
                != Some("schema")
            {
                continue;
            }
            let Some(schema_obj) = self.resolve_object_literal(init_node, empty) else {
                continue;
            };
            for &entry in properties(schema_obj).iter() {
                match entry.kind() {
                    Kind::PropertyAssignment => {
                        let (Some(e_name), Some(e_init), Some(e_loc)) =
                            (entry.name(), entry.initializer(), self.loc(entry))
                        else {
                            continue;
                        };
                        if let Ok(entry_key) =
                            self.resolve_property_key(e_name, &e_loc, "dynamic-query-projection")
                            && let Some(table) = self.evaluate_table_expr(e_init, empty)
                        {
                            index.schema_tables.insert(entry_key, table);
                        }
                    }
                    Kind::ShorthandPropertyAssignment => {
                        let entry_key = entry
                            .name()
                            .and_then(node_text)
                            .or_else(|| node_text(entry))
                            .unwrap_or_default();
                        if !entry_key.is_empty()
                            && let Some(table) = self.evaluate_table_expr(entry, empty)
                        {
                            index.schema_tables.insert(entry_key, table);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn lookup_table_for_query_key(
        table_key: &str,
        index: &RelationsIndex<'nodes>,
    ) -> Option<ExtractedTable<'nodes>> {
        if let Some(table) = index.schema_tables.get(table_key) {
            return Some(table.clone());
        }
        if let Some(table) = index
            .all_tables
            .iter()
            .find(|t| t.ts_var_name.as_deref() == Some(table_key))
        {
            return Some(table.clone());
        }
        index
            .all_tables
            .iter()
            .find(|t| t.sql_table_name.as_deref() == Some(table_key))
            .cloned()
    }

    fn lookup_relation_for_table(
        table: Option<&ExtractedTable<'nodes>>,
        relation_name: &str,
        index: &RelationsIndex<'nodes>,
    ) -> Option<RelationTargetInfo<'nodes>> {
        let tbl = table?;
        if let Some(decl_node) = tbl.decl_node
            && let Some(hit) = index
                .by_table_decl
                .get(&decl_node)
                .and_then(|map| map.get(relation_name))
        {
            return Some(hit.clone());
        }
        for name_opt in [&tbl.ts_var_name, &tbl.sql_table_name] {
            if let Some(name) = name_opt
                && let Some(hit) = index
                    .by_table_name
                    .get(name)
                    .and_then(|map| map.get(relation_name))
            {
                return Some(hit.clone());
            }
        }
        None
    }

    fn evaluate_column_ref(
        &mut self,
        expr_node: NodeRef<'nodes>,
        fallback_table: Option<&ExtractedTable<'nodes>>,
    ) -> ColumnRefMeta {
        let unwrapped = unwrap_expr_wrappers(expr_node);
        let empty = FxHashSet::default();

        if unwrapped.kind() == Kind::PropertyAccessExpression
            && let Some(recv) = unwrapped.expression()
            && let Some(name_node) = unwrapped.name()
        {
            let col_prop_name = node_text(name_node).unwrap_or_default();
            let resolved_table = self.evaluate_table_expr(recv, &empty);
            let table_ref = resolved_table.as_ref().or(fallback_table);
            if let Some(tbl) = table_ref
                && let Some(col_def) = tbl.columns.get(&col_prop_name)
            {
                return ColumnRefMeta {
                    table_name: Some(tbl.display_name.clone()),
                    sql_name: Some(col_def.sql_name.clone()),
                    nullable: Some(col_def.nullable),
                    is_table_column: true,
                    evidence: vec![EvidenceItem::simple(
                        "source",
                        format!(
                            "Table column \"{}.{col_prop_name}\" (SQL \"{}\")",
                            tbl.display_name, col_def.sql_name
                        ),
                        Some(col_def.location.clone()),
                    )],
                };
            }
        }

        if unwrapped.kind() == Kind::Identifier {
            if let Some((_, decl_node)) = resolve_decl_for_ref(self.cwd, self.checker, unwrapped)
                && let Some(init) = safe_initializer(decl_node)
            {
                return self.evaluate_column_ref(init, fallback_table);
            }
            let ident_text = node_text(unwrapped).unwrap_or_default();
            if let Some(tbl) = fallback_table
                && let Some(col_def) = tbl.columns.get(&ident_text)
            {
                return ColumnRefMeta {
                    table_name: Some(tbl.display_name.clone()),
                    sql_name: Some(col_def.sql_name.clone()),
                    nullable: Some(col_def.nullable),
                    is_table_column: true,
                    evidence: vec![EvidenceItem::simple(
                        "source",
                        format!(
                            "Table column \"{}.{ident_text}\" (SQL \"{}\")",
                            tbl.display_name, col_def.sql_name
                        ),
                        Some(col_def.location.clone()),
                    )],
                };
            }
        }

        ColumnRefMeta {
            table_name: None,
            sql_name: None,
            nullable: None,
            is_table_column: false,
            evidence: Vec::new(),
        }
    }

    fn evaluate_select_fields_object(
        &mut self,
        fields_arg: NodeRef<'nodes>,
        fallback_table: Option<&ExtractedTable<'nodes>>,
        visited_decls: &FxHashSet<NodeRef<'nodes>>,
    ) -> SelectFieldsResult {
        let unwrapped = unwrap_expr_wrappers(fields_arg);

        if unwrapped.kind() == Kind::Identifier {
            if let Some((_, decl_node)) = resolve_decl_for_ref(self.cwd, self.checker, unwrapped)
                && !visited_decls.contains(&decl_node)
                && let Some(init) = safe_initializer(decl_node)
            {
                let mut next_visited = visited_decls.clone();
                next_visited.insert(decl_node);
                return self.evaluate_select_fields_object(init, fallback_table, &next_visited);
            }
            let ident_text = node_text(unwrapped).unwrap_or_default();
            let loc = self.loc(unwrapped);
            return SelectFieldsResult {
                columns: BTreeMap::new(),
                has_dynamic_columns: true,
                gaps: vec![GapItem {
                    kind: "dynamic-query-projection",
                    message: format!(
                        "Drizzle select projection \"{ident_text}\" could not be statically resolved."
                    ),
                    at: loc.clone(),
                }],
                evidence: vec![EvidenceItem::simple(
                    "source",
                    format!("Dynamic select projection \"{ident_text}\""),
                    loc,
                )],
            };
        }

        if unwrapped.kind() == Kind::CallExpression
            && let Some(res) = self.evaluate_get_table_columns_call(unwrapped)
        {
            return res;
        }

        if unwrapped.kind() != Kind::ObjectLiteralExpression {
            let snap_text = node_text(unwrapped).unwrap_or_default();
            let loc = self.loc(unwrapped);
            return SelectFieldsResult {
                columns: BTreeMap::new(),
                has_dynamic_columns: true,
                gaps: vec![GapItem {
                    kind: "dynamic-query-projection",
                    message: format!(
                        "Drizzle select projection \"{snap_text}\" is not a static object literal."
                    ),
                    at: loc.clone(),
                }],
                evidence: vec![EvidenceItem::simple(
                    "source",
                    "Non-literal select projection".to_string(),
                    loc,
                )],
            };
        }

        let Some(obj_loc) = self.loc(unwrapped) else {
            return SelectFieldsResult {
                columns: BTreeMap::new(),
                has_dynamic_columns: true,
                gaps: Vec::new(),
                evidence: Vec::new(),
            };
        };

        let mut out = SelectFieldsResult {
            columns: BTreeMap::new(),
            has_dynamic_columns: false,
            gaps: Vec::new(),
            evidence: vec![EvidenceItem::simple(
                "source",
                "Drizzle select column projection".to_string(),
                Some(obj_loc),
            )],
        };

        for &child in properties(unwrapped).iter()
        {
            self.evaluate_select_object_member(child, fallback_table, visited_decls, &mut out);
        }

        SelectFieldsResult {
            columns: out.columns,
            has_dynamic_columns: out.has_dynamic_columns,
            gaps: dedupe_gaps(out.gaps),
            evidence: dedupe_evidence(out.evidence),
        }
    }

    fn evaluate_get_table_columns_call(&mut self, call: NodeRef<'nodes>) -> Option<SelectFieldsResult> {
        let expr = call.expression()?;
        let callee = unwrap_expr_wrappers(expr);
        let callee_name = match callee.kind() {
            Kind::Identifier => node_text(callee).unwrap_or_default(),
            Kind::PropertyAccessExpression => callee.name().and_then(node_text).unwrap_or_default(),
            _ => return None,
        };
        if callee_name != "getTableColumns" {
            return None;
        }
        let &first_arg = call.arguments().first()?;
        let empty = FxHashSet::default();
        let table = self.evaluate_table_expr(first_arg, &empty)?;
        let columns = Self::full_table_columns(Some(&table));
        Some(SelectFieldsResult {
            columns,
            has_dynamic_columns: table.has_dynamic_columns,
            gaps: table.gaps,
            evidence: vec![EvidenceItem::simple(
                "source",
                format!("Expanded getTableColumns({})", table.display_name),
                self.loc(call),
            )],
        })
    }

    fn evaluate_select_object_member(
        &mut self,
        child: NodeRef<'nodes>,
        fallback_table: Option<&ExtractedTable<'nodes>>,
        visited_decls: &FxHashSet<NodeRef<'nodes>>,
        out: &mut SelectFieldsResult,
    ) {
        let Some(child_loc) = self.loc(child) else {
            return;
        };
        match child.kind() {
            Kind::PropertyAssignment => {
                let (Some(name_node), Some(init_node)) = (child.name(), child.initializer()) else {
                    return;
                };
                match self.resolve_property_key(name_node, &child_loc, "dynamic-query-projection") {
                    Ok(key) => {
                        let col_meta = self.evaluate_column_ref(init_node, fallback_table);
                        self.record_projected_column(key, child_loc, col_meta, out);
                    }
                    Err(gap) => {
                        out.has_dynamic_columns = true;
                        if let Some(g) = gap {
                            out.gaps.push(g);
                        }
                    }
                }
            }
            Kind::ShorthandPropertyAssignment => {
                let key_name = child
                    .name()
                    .and_then(node_text)
                    .or_else(|| node_text(child))
                    .unwrap_or_default();
                let col_meta = self.evaluate_column_ref(child, fallback_table);
                self.record_projected_column(key_name, child_loc, col_meta, out);
            }
            Kind::SpreadAssignment => {
                let spread_text = node_text(child).unwrap_or_default();
                if let Some(expr) = child.expression() {
                    let spread_res =
                        self.evaluate_select_fields_object(expr, fallback_table, visited_decls);
                    if spread_res.has_dynamic_columns && spread_res.columns.is_empty() {
                        out.has_dynamic_columns = true;
                        out.gaps.push(GapItem {
                            kind: "dynamic-query-projection",
                            message: format!(
                                "Spread \"{spread_text}\" in Drizzle select projection cannot be statically resolved."
                            ),
                            at: Some(child_loc),
                        });
                    } else {
                        if spread_res.has_dynamic_columns {
                            out.has_dynamic_columns = true;
                        }
                        out.gaps.extend(spread_res.gaps);
                        out.columns.extend(spread_res.columns);
                        out.evidence.extend(spread_res.evidence);
                    }
                } else {
                    out.has_dynamic_columns = true;
                    out.gaps.push(GapItem {
                        kind: "dynamic-query-projection",
                        message: format!(
                            "Spread \"{spread_text}\" in Drizzle select projection cannot be statically resolved."
                        ),
                        at: Some(child_loc),
                    });
                }
            }
            _ => {
                let member_text = node_text(child).unwrap_or_default();
                out.has_dynamic_columns = true;
                out.gaps.push(GapItem {
                    kind: "dynamic-query-projection",
                    message: format!(
                        "Unsupported dynamic member \"{member_text}\" ({}) in Drizzle select projection.",
                        kind_label(child)
                    ),
                    at: Some(child_loc),
                });
            }
        }
    }

    fn record_projected_column(
        &self,
        key: String,
        prop_loc: SourceLocation,
        col_meta: ColumnRefMeta,
        out: &mut SelectFieldsResult,
    ) {
        let _ = self;
        let mut col_ev = vec![EvidenceItem::simple(
            "source",
            format!("Projected column \"{key}\""),
            Some(prop_loc),
        )];
        col_ev.extend(col_meta.evidence.iter().cloned());
        out.columns.insert(
            key.clone(),
            QueryRowColumnInfoPayload {
                name: key,
                sql_name: col_meta.sql_name,
                table_name: col_meta.table_name,
                kind: if col_meta.is_table_column {
                    "tableColumn"
                } else {
                    "computed"
                },
                nullable: col_meta.nullable,
                evidence: Some(dedupe_evidence(col_ev)),
            },
        );
        out.evidence.extend(col_meta.evidence);
    }

    fn full_table_columns(
        table: Option<&ExtractedTable<'nodes>>,
    ) -> BTreeMap<String, QueryRowColumnInfoPayload> {
        let mut out = BTreeMap::new();
        let Some(tbl) = table else {
            return out;
        };
        for col in tbl.columns.values() {
            out.insert(
                col.ts_name.clone(),
                QueryRowColumnInfoPayload {
                    name: col.ts_name.clone(),
                    sql_name: Some(col.sql_name.clone()),
                    table_name: Some(tbl.display_name.clone()),
                    kind: "tableColumn",
                    nullable: Some(col.nullable),
                    evidence: Some(vec![EvidenceItem::simple(
                        "source",
                        format!("Table column \"{}.{}\"", tbl.display_name, col.ts_name),
                        Some(col.location.clone()),
                    )]),
                },
            );
        }
        out
    }

    fn collect_relational_option_fields(
        &self,
        opts_obj: NodeRef<'nodes>,
        has_dynamic_columns: &mut bool,
        gaps: &mut Vec<GapItem>,
    ) -> (Option<NodeRef<'nodes>>, Option<NodeRef<'nodes>>) {
        let mut columns_arg: Option<NodeRef<'nodes>> = None;
        let mut with_arg: Option<NodeRef<'nodes>> = None;
        for &child in properties(opts_obj).iter() {
            if child.kind() == Kind::PropertyAssignment {
                let (Some(name_node), Some(init_node), Some(child_loc)) =
                    (child.name(), child.initializer(), self.loc(child))
                else {
                    continue;
                };
                if let Ok(key) =
                    self.resolve_property_key(name_node, &child_loc, "dynamic-query-projection")
                {
                    if key == "columns" {
                        columns_arg = Some(init_node);
                    } else if key == "with" {
                        with_arg = Some(init_node);
                    }
                }
            } else if child.kind() == Kind::SpreadAssignment {
                let text = node_text(child).unwrap_or_default();
                *has_dynamic_columns = true;
                gaps.push(GapItem {
                    kind: "dynamic-query-projection",
                    message: format!(
                        "Spread \"{text}\" in Drizzle relational query options prevents static enumeration."
                    ),
                    at: self.loc(child),
                });
            }
        }
        (columns_arg, with_arg)
    }

    fn evaluate_relational_options(
        &mut self,
        table: Option<&ExtractedTable<'nodes>>,
        table_key: &str,
        opts_node: Option<NodeRef<'nodes>>,
        index: &RelationsIndex<'nodes>,
        visited_decls: &FxHashSet<NodeRef<'nodes>>,
    ) -> RelationalEvalResult {
        let mut gaps = table.map_or_default(|t| t.gaps.clone());
        let mut evidence = table.map_or_default(|t| {
            vec![EvidenceItem::simple(
                "source",
                format!("Drizzle table \"{}\" definition", t.display_name),
                Some(t.location.clone()),
            )]
        });
        let mut has_dynamic_columns = table.is_some_and(|t| t.has_dynamic_columns);

        if table.is_none() {
            has_dynamic_columns = true;
            gaps.push(GapItem {
                kind: "unresolved-drizzle-table",
                message: format!(
                    "Drizzle table \"{table_key}\" could not be resolved to a table definition."
                ),
                at: None,
            });
        }

        let Some(opts_ref) = opts_node else {
            return RelationalEvalResult {
                columns: Self::full_table_columns(table),
                relations: BTreeMap::new(),
                has_dynamic_columns,
                gaps,
                evidence,
            };
        };

        let Some(opts_obj) = self.resolve_object_literal(opts_ref, visited_decls) else {
            let raw = unwrap_expr_wrappers(opts_ref);
            if raw.kind() == Kind::UndefinedKeyword {
                return RelationalEvalResult {
                    columns: Self::full_table_columns(table),
                    relations: BTreeMap::new(),
                    has_dynamic_columns,
                    gaps,
                    evidence,
                };
            }
            gaps.push(GapItem {
                kind: "dynamic-query-projection",
                message: format!(
                    "Relational query options for \"{table_key}\" are not statically resolvable."
                ),
                at: self.loc(raw),
            });
            return RelationalEvalResult {
                columns: BTreeMap::new(),
                relations: BTreeMap::new(),
                has_dynamic_columns: true,
                gaps,
                evidence,
            };
        };

        let (columns_arg, with_arg) =
            self.collect_relational_option_fields(opts_obj, &mut has_dynamic_columns, &mut gaps);

        let columns = columns_arg.map_or_else(
            || Self::full_table_columns(table),
            |cols_node| {
                let cols_eval =
                    self.evaluate_relational_columns_mask(table, cols_node, visited_decls);
                if cols_eval.has_dynamic_columns {
                    has_dynamic_columns = true;
                }
                gaps.extend(cols_eval.gaps);
                evidence.extend(cols_eval.evidence);
                cols_eval.columns
            },
        );

        let relations = with_arg.map_or_else(BTreeMap::new, |with_node| {
            let with_eval = self.evaluate_relational_with(table, with_node, index, visited_decls);
            if with_eval.has_dynamic_columns {
                has_dynamic_columns = true;
            }
            gaps.extend(with_eval.gaps);
            evidence.extend(with_eval.evidence);
            with_eval.relations
        });

        RelationalEvalResult {
            columns,
            relations,
            has_dynamic_columns,
            gaps: dedupe_gaps(gaps),
            evidence: dedupe_evidence(evidence),
        }
    }

    fn build_columns_from_mask_selection(
        table: Option<&ExtractedTable<'nodes>>,
        included: Vec<(String, SourceLocation)>,
        excluded: &FxHashSet<String>,
    ) -> BTreeMap<String, QueryRowColumnInfoPayload> {
        let mut columns = BTreeMap::new();
        if !included.is_empty() {
            for (col_name, prop_loc) in included {
                let col_def = table.and_then(|t| t.columns.get(&col_name));
                let mut col_ev = vec![EvidenceItem::simple(
                    "source",
                    format!("Projected column \"{col_name}\""),
                    Some(prop_loc),
                )];
                if let Some(def) = col_def {
                    let tbl_name = table.map_or("table", |t| t.display_name.as_str());
                    col_ev.push(EvidenceItem::simple(
                        "source",
                        format!("Table column \"{tbl_name}.{col_name}\""),
                        Some(def.location.clone()),
                    ));
                }
                columns.insert(
                    col_name.clone(),
                    QueryRowColumnInfoPayload {
                        name: col_name,
                        sql_name: col_def.map(|d| d.sql_name.clone()),
                        table_name: table.map(|t| t.display_name.clone()),
                        kind: "tableColumn",
                        nullable: col_def.map(|d| d.nullable),
                        evidence: Some(dedupe_evidence(col_ev)),
                    },
                );
            }
        } else if !excluded.is_empty()
            && let Some(tbl) = table
        {
            for col_def in tbl.columns.values() {
                if excluded.contains(&col_def.ts_name) {
                    continue;
                }
                columns.insert(
                    col_def.ts_name.clone(),
                    QueryRowColumnInfoPayload {
                        name: col_def.ts_name.clone(),
                        sql_name: Some(col_def.sql_name.clone()),
                        table_name: Some(tbl.display_name.clone()),
                        kind: "tableColumn",
                        nullable: Some(col_def.nullable),
                        evidence: Some(vec![EvidenceItem::simple(
                            "source",
                            format!("Table column \"{}.{}\"", tbl.display_name, col_def.ts_name),
                            Some(col_def.location.clone()),
                        )]),
                    },
                );
            }
        }
        columns
    }

    fn evaluate_relational_columns_mask(
        &mut self,
        table: Option<&ExtractedTable<'nodes>>,
        columns_node: NodeRef<'nodes>,
        visited_decls: &FxHashSet<NodeRef<'nodes>>,
    ) -> SelectFieldsResult {
        let Some(mask_obj) = self.resolve_object_literal(columns_node, visited_decls) else {
            let raw = unwrap_expr_wrappers(columns_node);
            if raw.kind() == Kind::UndefinedKeyword {
                return SelectFieldsResult {
                    columns: Self::full_table_columns(table),
                    has_dynamic_columns: false,
                    gaps: Vec::new(),
                    evidence: Vec::new(),
                };
            }
            return SelectFieldsResult {
                columns: BTreeMap::new(),
                has_dynamic_columns: true,
                gaps: vec![GapItem {
                    kind: "dynamic-query-projection",
                    message: "Relational query `columns` option is not a static object literal."
                        .to_string(),
                    at: self.loc(raw),
                }],
                evidence: Vec::new(),
            };
        };

        let mut included: Vec<(String, SourceLocation)> = Vec::new();
        let mut excluded = FxHashSet::default();
        let mut gaps = Vec::new();
        let mut has_dynamic_columns = false;

        for &child in properties(mask_obj).iter() {
            let Some(child_loc) = self.loc(child) else {
                continue;
            };
            if child.kind() == Kind::PropertyAssignment
                && let Some(name_node) = child.name()
                && let Some(init_node) = child.initializer()
            {
                match self.resolve_property_key(name_node, &child_loc, "dynamic-query-projection") {
                    Ok(key) => {
                        let val = unwrap_expr_wrappers(init_node);
                        let val_text = node_text(val).map_or_default(|s| s.trim().to_string());
                        if val_text == "true" {
                            included.push((key, child_loc));
                        } else if val_text == "false" {
                            excluded.insert(key);
                        } else {
                            has_dynamic_columns = true;
                            gaps.push(GapItem {
                                kind: "dynamic-query-projection",
                                message: format!(
                                    "Relational query column \"{key}\" uses non-literal flag \"{val_text}\"."
                                ),
                                at: Some(child_loc),
                            });
                        }
                    }
                    Err(gap) => {
                        has_dynamic_columns = true;
                        if let Some(g) = gap {
                            gaps.push(g);
                        }
                    }
                }
            } else {
                let text = node_text(child).unwrap_or_default();
                has_dynamic_columns = true;
                gaps.push(GapItem {
                    kind: "dynamic-query-projection",
                    message: format!("Dynamic member \"{text}\" in relational query `columns`."),
                    at: Some(child_loc),
                });
            }
        }

        let evidence = vec![EvidenceItem::simple(
            "source",
            "Drizzle relational query `columns` projection".to_string(),
            self.loc(mask_obj),
        )];
        let columns = Self::build_columns_from_mask_selection(table, included, &excluded);

        SelectFieldsResult {
            columns,
            has_dynamic_columns,
            gaps,
            evidence,
        }
    }

    fn evaluate_single_with_entry(
        &mut self,
        parent_table: Option<&ExtractedTable<'nodes>>,
        child: NodeRef<'nodes>,
        index: &RelationsIndex<'nodes>,
        visited_decls: &FxHashSet<NodeRef<'nodes>>,
        out: &mut RelationalWithResult,
    ) {
        let Some(child_loc) = self.loc(child) else {
            return;
        };
        if child.kind() != Kind::PropertyAssignment {
            let text = node_text(child).unwrap_or_default();
            out.has_dynamic_columns = true;
            out.gaps.push(GapItem {
                kind: "dynamic-query-projection",
                message: format!("Dynamic entry \"{text}\" in relational query `with`."),
                at: Some(child_loc),
            });
            return;
        }
        let (Some(name_node), Some(init_node)) = (child.name(), child.initializer()) else {
            return;
        };
        let rel_name =
            match self.resolve_property_key(name_node, &child_loc, "dynamic-query-projection") {
                Ok(k) => k,
            Err(gap) => {
                out.has_dynamic_columns = true;
                if let Some(g) = gap {
                    out.gaps.push(g);
                }
                return;
            }
        };

        let spec = unwrap_expr_wrappers(init_node);
        let spec_text = node_text(spec).map_or_default(|s| s.trim().to_string());
        if spec_text == "false" {
            return;
        }

        let rel_target = Self::lookup_relation_for_table(parent_table, &rel_name, index);
        let Some(rel_info) = rel_target.filter(|r| r.target_table.is_some()) else {
            out.has_dynamic_columns = true;
            let parent_name = parent_table.map_or("unknown", |t| t.display_name.as_str());
            out.gaps.push(GapItem {
                kind: "unresolved-drizzle-relation",
                message: format!(
                    "Drizzle relation \"{rel_name}\" on table \"{parent_name}\" could not be resolved."
                ),
                at: Some(child_loc),
            });
            return;
        };

        let target_table = rel_info.target_table.expect("checked is_some");
        let nested_opts = (spec_text != "true").then_some(init_node);
        let nested_eval = self.evaluate_relational_options(
            Some(&target_table),
            &target_table.display_name,
            nested_opts,
            index,
            visited_decls,
        );

        if nested_eval.has_dynamic_columns {
            out.has_dynamic_columns = true;
        }
        out.gaps.extend(nested_eval.gaps);

        let mut rel_ev = vec![
            EvidenceItem::simple(
                "source",
                format!(
                    "Nested relation \"{rel_name}\" ({}) from \"{}\"",
                    rel_info.cardinality, target_table.display_name
                ),
                Some(child_loc),
            ),
            EvidenceItem::simple(
                "source",
                format!("Relation \"{rel_name}\" defined here"),
                Some(rel_info.location),
            ),
        ];
        rel_ev.extend(nested_eval.evidence);
        let rel_evidence = dedupe_evidence(rel_ev);
        out.evidence.extend(rel_evidence.iter().cloned());

        let mut nested_cols: Vec<String> = nested_eval
            .columns
            .keys()
            .chain(nested_eval.relations.keys())
            .cloned()
            .collect();
        nested_cols.sort_by(|a, b| compare_utf16(a, b));
        nested_cols.dedup();

        out.relations.insert(
            rel_name.clone(),
            QueryRowNestedRelationInfoPayload {
                name: rel_name,
                cardinality: rel_info.cardinality,
                table_name: Some(target_table.display_name),
                columns: nested_cols,
                column_details: nested_eval.columns,
                relations: nested_eval.relations,
                has_dynamic_columns: nested_eval.has_dynamic_columns,
                evidence: Some(rel_evidence),
            },
        );
    }

    fn evaluate_relational_with(
        &mut self,
        parent_table: Option<&ExtractedTable<'nodes>>,
        with_node: NodeRef<'nodes>,
        index: &RelationsIndex<'nodes>,
        visited_decls: &FxHashSet<NodeRef<'nodes>>,
    ) -> RelationalWithResult {
        let Some(with_obj) = self.resolve_object_literal(with_node, visited_decls) else {
            let raw = unwrap_expr_wrappers(with_node);
            if raw.kind() == Kind::UndefinedKeyword {
                return RelationalWithResult {
                    relations: BTreeMap::new(),
                    has_dynamic_columns: false,
                    gaps: Vec::new(),
                    evidence: Vec::new(),
                };
            }
            return RelationalWithResult {
                relations: BTreeMap::new(),
                has_dynamic_columns: true,
                gaps: vec![GapItem {
                    kind: "dynamic-query-projection",
                    message: "Relational query `with` option is not a static object literal."
                        .to_string(),
                    at: self.loc(raw),
                }],
                evidence: Vec::new(),
            };
        };

        let mut out = RelationalWithResult {
            relations: BTreeMap::new(),
            has_dynamic_columns: false,
            gaps: Vec::new(),
            evidence: vec![EvidenceItem::simple(
                "source",
                "Drizzle relational query `with` clause".to_string(),
                self.loc(with_obj),
            )],
        };

        for &child in properties(with_obj).iter() {
            self.evaluate_single_with_entry(parent_table, child, index, visited_decls, &mut out);
        }

        out
    }
}

fn collect_fluent_chain<'nodes>(outer_call: NodeRef<'nodes>) -> Option<Vec<FluentStep<'nodes>>> {
    let mut steps = Vec::new();
    let mut current = Some(outer_call);
    let mut seen = FxHashSet::default();

    while let Some(node) = current {
        if !seen.insert(node) {
            break;
        }
        if node.kind() != Kind::CallExpression {
            break;
        }
        let expr = node.expression()?;
        let callee = unwrap_expr_wrappers(expr);
        if callee.kind() != Kind::PropertyAccessExpression {
            break;
        }
        let recv = callee.expression()?;
        let name_node = callee.name()?;
        let method_name = node_text(name_node).unwrap_or_default();
        steps.push(FluentStep {
            call: node,
            method_name: method_name.clone(),
        });
        if SELECT_ENTRY_METHODS.contains(&method_name.as_str()) {
            steps.reverse();
            return Some(steps);
        }
        if !SELECT_CHAIN_METHODS.contains(&method_name.as_str()) {
            return None;
        }
        current = Some(unwrap_expr_wrappers(recv));
    }

    None
}


fn looks_like_relational_builder_type(type_text: &str) -> bool {
    type_text.contains("RelationalQueryBuilder")
        || type_text.contains("SQLiteRelationalQuery")
        || type_text.contains("PgRelationalQuery")
        || type_text.contains("MySqlRelationalQuery")
}

fn looks_like_db_client_type(type_text: &str) -> bool {
    type_text.contains("Drizzle")
        || type_text.contains("SQLiteDatabase")
        || type_text.contains("PgDatabase")
        || type_text.contains("MySqlDatabase")
        || type_text.contains("BaseSQLiteDatabase")
        || type_text.contains("DbClient")
}


struct CollectedDrizzleNodes<'nodes> {
    all_calls: Vec<NodeRef<'nodes>>,
    all_var_decls: Vec<NodeRef<'nodes>>,
    inner_receiver_locs: FxHashSet<(String, u32, u32)>,
}


struct ExtractedQueryItem<'nodes> {
    call: NodeRef<'nodes>,
    value: QueryRowProjectionValuePayload,
    evidence: Vec<EvidenceItem>,
    gaps: Vec<GapItem>,
}

fn try_extract_relational_call<'nodes>(
    evaluator: &mut DrizzleEvaluator<'nodes, '_>,
    call: NodeRef<'nodes>,
    call_loc: &SourceLocation,
    callee_recv: NodeRef<'nodes>,
    method_name: &str,
    relations_index: &RelationsIndex<'nodes>,
) -> Option<ExtractedQueryItem<'nodes>> {
    if method_name != "findFirst" && method_name != "findMany" {
        return None;
    }
    let query_target = unwrap_expr_wrappers(callee_recv);
    if query_target.kind() != Kind::PropertyAccessExpression {
        return None;
    }
    let (qt_expr, qt_name) = (query_target.expression()?, query_target.name()?);
    let table_key = node_text(qt_name).unwrap_or_default();
    let query_root = unwrap_expr_wrappers(qt_expr);
    let query_prop_name = match query_root.kind() {
        Kind::PropertyAccessExpression => query_root.name().and_then(node_text).unwrap_or_default(),
        Kind::Identifier => node_text(query_root).unwrap_or_default(),
        _ => String::new(),
    };
    if query_prop_name != "query" || table_key.is_empty() {
        return None;
    }

    let table = DrizzleEvaluator::lookup_table_for_query_key(&table_key, relations_index);
    let looks_like_drizzle =
        table.is_some() || looks_like_relational_builder_type(&evaluator.type_text(query_target));
    if !looks_like_drizzle {
        return None;
    }

    let empty = FxHashSet::default();
    let opts_arg = call.arguments().first().copied();
    let rel_eval = evaluator.evaluate_relational_options(
        table.as_ref(),
        &table_key,
        opts_arg,
        relations_index,
        &empty,
    );
    let mut evidence = vec![EvidenceItem::simple(
        "source",
        format!("Drizzle relational query db.query.{table_key}.{method_name}()"),
        Some(call_loc.clone()),
    )];
    evidence.extend(rel_eval.evidence);
    let value = build_projection_value(
        "relational",
        if method_name == "findFirst" {
            "single"
        } else {
            "many"
        },
        Some(
            table
                .as_ref()
                .map_or_else(|| table_key.clone(), |t| t.display_name.clone()),
        ),
        rel_eval.columns,
        rel_eval.relations,
        rel_eval.has_dynamic_columns,
    );
    Some(ExtractedQueryItem {
        call,
        value,
        evidence: dedupe_evidence(evidence),
        gaps: rel_eval.gaps,
    })
}

fn try_extract_fluent_select_call<'nodes>(
    evaluator: &mut DrizzleEvaluator<'nodes, '_>,
    call: NodeRef<'nodes>,
    call_loc: &SourceLocation,
) -> Option<ExtractedQueryItem<'nodes>> {
    let chain = collect_fluent_chain(call)?;
    let select_step = &chain[0];
    let from_step = chain.iter().find(|s| s.method_name == "from")?;
    let empty = FxHashSet::default();

    let table_arg = from_step.call.arguments().first().copied();
    let from_table = table_arg.and_then(|arg| evaluator.evaluate_table_expr(arg, &empty));
    if from_table.is_none() {
        let db_type_text = select_step
            .call
            .expression()
            .map(unwrap_expr_wrappers)
            .filter(|n| n.kind() == Kind::PropertyAccessExpression)
            .and_then(|n| n.expression())
            .map_or_default(|recv| evaluator.type_text(recv));
        if !looks_like_db_client_type(&db_type_text) {
            return None;
        }
    }

    let last_method = chain.last().map_or("", |s| s.method_name.as_str());
    let cardinality = if last_method == "get" {
        "single"
    } else {
        "many"
    };
    let select_arg = select_step.call.arguments().first().copied();

    let mut columns = BTreeMap::new();
    let mut has_dynamic_columns = from_table.as_ref().is_some_and(|t| t.has_dynamic_columns);
    let mut gaps = from_table.as_ref().map_or_default(|t| t.gaps.clone());
    let mut evidence = vec![EvidenceItem::simple(
        "source",
        format!("Drizzle .{}() query", select_step.method_name),
        Some(call_loc.clone()),
    )];
    if let Some(ref tbl) = from_table {
        evidence.push(EvidenceItem::simple(
            "source",
            format!("Drizzle table \"{}\" definition", tbl.display_name),
            Some(tbl.location.clone()),
        ));
    }

    match select_arg {
        None => {
            if let Some(ref tbl) = from_table {
                columns = DrizzleEvaluator::full_table_columns(Some(tbl));
            } else {
                has_dynamic_columns = true;
                gaps.push(GapItem {
                    kind: "unresolved-drizzle-table",
                    message: "Table passed to .from() could not be statically resolved."
                        .to_string(),
                    at: evaluator.loc(from_step.call),
                });
            }
        }
        Some(arg) => {
            let unwrapped_arg = unwrap_expr_wrappers(arg);
            if unwrapped_arg.kind() == Kind::UndefinedKeyword {
                if let Some(ref tbl) = from_table {
                    columns = DrizzleEvaluator::full_table_columns(Some(tbl));
                }
            } else {
                let eval_res =
                    evaluator.evaluate_select_fields_object(arg, from_table.as_ref(), &empty);
                columns = eval_res.columns;
                if eval_res.has_dynamic_columns {
                    has_dynamic_columns = true;
                }
                gaps.extend(eval_res.gaps);
                evidence.extend(eval_res.evidence);
            }
        }
    }

    let value = build_projection_value(
        "select",
        cardinality,
        from_table.as_ref().map(|t| t.display_name.clone()),
        columns,
        BTreeMap::new(),
        has_dynamic_columns,
    );
    Some(ExtractedQueryItem {
        call,
        value,
        evidence: dedupe_evidence(evidence),
        gaps: dedupe_gaps(gaps),
    })
}

fn extract_all_drizzle_items<'nodes>(
    cwd: &str,
    checker: &mut CheckerHandle,
    nodes: &CollectedDrizzleNodes<'nodes>,
    requested_nodes: Option<&FxHashSet<NodeRef<'nodes>>>,
) -> Vec<ExtractedQueryItem<'nodes>> {
    let mut evaluator = DrizzleEvaluator::new(cwd, checker);
    let relations_index = evaluator.build_relations_index(&nodes.all_calls, &nodes.all_var_decls);
    let mut out = Vec::new();

    for &call in &nodes.all_calls {
        if let Some(targets) = requested_nodes
            && !targets.contains(&call)
        {
            continue;
        }
        let Some(call_loc) = evaluator.loc(call) else {
            continue;
        };
        let Some(expr) = call.expression() else {
            continue;
        };
        let callee = unwrap_expr_wrappers(expr);
        if callee.kind() != Kind::PropertyAccessExpression {
            continue;
        }
        let (Some(callee_recv), Some(callee_name_node)) = (callee.expression(), callee.name())
        else {
            continue;
        };
        let method_name = node_text(callee_name_node).unwrap_or_default();

        if let Some(item) = try_extract_relational_call(
            &mut evaluator,
            call,
            &call_loc,
            callee_recv,
            &method_name,
            &relations_index,
        ) {
            out.push(item);
            continue;
        }

        if requested_nodes.is_none() && nodes.inner_receiver_locs.contains(&location_key(&call_loc))
        {
            continue;
        }

        if let Some(item) = try_extract_fluent_select_call(&mut evaluator, call, &call_loc) {
            out.push(item);
        }
    }
    drop(evaluator);
    out
}

struct ExtractedTableColumnsItem<'nodes> {
    subject: NodeRef<'nodes>,
    value: DrizzleTableColumnsValuePayload,
    evidence: Vec<EvidenceItem>,
    gaps: Vec<GapItem>,
}

fn table_matches_condition(table: &ExtractedTable<'_>, requested_table_name: Option<&str>) -> bool {
    let Some(name) = requested_table_name else {
        return true;
    };
    table.display_name == name || table.sql_table_name.as_deref() == Some(name)
}

fn build_table_columns_item<'nodes>(
    subject: NodeRef<'nodes>,
    table: ExtractedTable<'nodes>,
) -> ExtractedTableColumnsItem<'nodes> {
    let mut columns = BTreeMap::new();
    let mut column_names = Vec::with_capacity(table.columns.len());
    let mut evidence = vec![EvidenceItem::simple(
        "source",
        format!("Drizzle table \"{}\" definition", table.display_name),
        Some(table.location.clone()),
    )];

    for (name, col) in table.columns {
        let col_evidence = EvidenceItem::simple(
            "source",
            format!(
                "Column \"{}\" defined in Drizzle table \"{}\"",
                col.ts_name, table.display_name
            ),
            Some(col.location.clone()),
        );
        evidence.push(col_evidence.clone());
        column_names.push(name.clone());
        columns.insert(
            name.clone(),
            DrizzleTableColumnInfoPayload {
                name,
                sql_name: col.sql_name,
                nullable: col.nullable,
                primary_key: col.primary_key,
                has_default: col.has_default,
                evidence: Some(vec![col_evidence]),
            },
        );
    }

    column_names.sort_by(|a, b| compare_utf16(a, b));
    let value = DrizzleTableColumnsValuePayload {
        library: "drizzle",
        table_name: table.display_name,
        sql_table_name: table.sql_table_name,
        column_names,
        columns,
        has_dynamic_columns: table.has_dynamic_columns,
    };

    ExtractedTableColumnsItem {
        subject,
        value,
        evidence: dedupe_evidence(evidence),
        gaps: dedupe_gaps(table.gaps),
    }
}

fn extract_all_table_columns_items<'nodes>(
    cwd: &str,
    checker: &mut CheckerHandle,
    nodes: &CollectedDrizzleNodes<'nodes>,
    requested_targets: Option<&[NodeRef<'nodes>]>,
    requested_table_name: Option<&str>,
) -> Vec<ExtractedTableColumnsItem<'nodes>> {
    let mut evaluator = DrizzleEvaluator::new(cwd, checker);
    let empty = FxHashSet::default();
    let mut out = Vec::new();

    if let Some(targets) = requested_targets {
        let mut seen_targets = FxHashSet::default();
        for &target in targets {
            if !seen_targets.insert(target) {
                continue;
            }
            if let Some(table) = evaluator.evaluate_table_expr(target, &empty)
                && table_matches_condition(&table, requested_table_name)
            {
                out.push(build_table_columns_item(target, table));
            }
        }
        return out;
    }

    let mut emitted_call_locs = FxHashSet::default();
    for &var_decl in &nodes.all_var_decls {
        if let Some(table) = evaluator.evaluate_table_expr(var_decl, &empty) {
            emitted_call_locs.insert(location_key(&table.location));
            if table_matches_condition(&table, requested_table_name) {
                out.push(build_table_columns_item(var_decl, table));
            }
        }
    }

    for &call in &nodes.all_calls {
        let Some(call_loc) = evaluator.loc(call) else {
            continue;
        };
        if emitted_call_locs.contains(&location_key(&call_loc)) {
            continue;
        }
        if let Some(table) = evaluator.evaluate_table_expr(call, &empty) {
            emitted_call_locs.insert(location_key(&table.location));
            if table_matches_condition(&table, requested_table_name) {
                out.push(build_table_columns_item(call, table));
            }
        }
    }

    out
}


fn classify_receiver_locations<'nodes>(cwd: &str, all_calls: &[NodeRef<'nodes>]) -> FxHashSet<(String,u32,u32)> {
    let mut inner_receiver_locs = FxHashSet::default();
    for &call in all_calls {
        if let Some(expr) = call.expression() {
            let callee = unwrap_expr_wrappers(expr);
            if callee.kind() == Kind::PropertyAccessExpression
                && let Some(recv) = callee.expression()
            {
                let recv_unwrapped = unwrap_expr_wrappers(recv);
                if recv_unwrapped.kind() == Kind::CallExpression
                    && let Some(loc) = node_location_rel(cwd, recv_unwrapped)
                {
                    inner_receiver_locs.insert(location_key(&loc));
                }
            }
        }
    }

 inner_receiver_locs
}

};
}
