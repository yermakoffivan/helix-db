//! Literal execution of planner-selected cardinality programs.
//!
//! Every match arm implements the primitive named by executable IR. This
//! module may evaluate encoded arithmetic and execute cursor nodes, but it does
//! not substitute access families, batch leaves, reorder children, or push
//! windows across cursor operators.

use std::collections::BTreeSet;

use futures::future::BoxFuture;
use futures::FutureExt;
use helix_ast::query::QueryValue;
use helix_ast::value::PropertyValue as AstPropertyValue;
use helix_planner::{exec, ir, properties};

use super::access::SearchReadLimit;
use super::*;
use crate::config::{TextElementType, VectorElementType};
use crate::search::vector::VectorEntityId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EvaluatedCountWindow {
    skip: usize,
    take: Option<usize>,
}

impl EvaluatedCountWindow {
    fn apply(self, cardinality: usize) -> usize {
        let cardinality = cardinality.saturating_sub(self.skip);
        self.take.map_or(cardinality, |take| cardinality.min(take))
    }

    fn apply_rows(self, rows: Vec<ExecutionRow>) -> Vec<ExecutionRow> {
        let rows = rows.into_iter().skip(self.skip);
        match self.take {
            Some(take) => rows.take(take).collect(),
            None => rows.collect(),
        }
    }

    fn threshold(self) -> Option<usize> {
        self.take.map(|take| self.skip.saturating_add(take))
    }
}

impl<'db> ExecutionContext<'db> {
    pub(in crate::execution::interpreter) async fn execute_count(
        &mut self,
        input: ExecutionValue,
        plan: &exec::ExecCountPlan,
    ) -> Result<ExecutionValue> {
        plan.validate().map_err(|error| {
            HelixDbError::InvariantViolation(format!("invalid count program: {error:?}"))
        })?;
        let dependency_contract = plan
            .dependency()
            .expect("validated count programs have valid dependencies");
        let count = match plan {
            exec::ExecCountPlan::Constant(count) => *count,
            exec::ExecCountPlan::NodeBitmap(plan) => {
                let window = self.count_window(&plan.window)?;
                window.apply(self.node_bitmap(&plan.bitmap).await?.len() as usize)
            }
            exec::ExecCountPlan::EdgeBitmap(plan) => {
                let window = self.count_window(&plan.window)?;
                window.apply(self.edge_bitmap(&plan.bitmap).await?.len() as usize)
            }
            exec::ExecCountPlan::NodeUnique(plan) => {
                let window = self.count_window(&plan.window)?;
                window.apply(usize::from(
                    self.verified_node_unique_owner(&plan.lookup, &plan.verification)
                        .await?
                        .is_some(),
                ))
            }
            exec::ExecCountPlan::NodeRange(plan) => {
                validate_range_index("node_range:", &plan.driver.index.index_id, &plan.driver.key)?;
                let window = self.count_window(&plan.window)?;
                let filters = match &plan.membership {
                    exec::ExecNodeRangeMembershipPlan::All => Vec::new(),
                    exec::ExecNodeRangeMembershipPlan::BitmapFilters(filters) => {
                        let mut bitmaps = Vec::with_capacity(filters.as_ref().len());
                        for filter in filters {
                            bitmaps.push(self.node_bitmap(filter).await?);
                        }
                        bitmaps
                    }
                };
                let accepted = self
                    .node_range_index_ids_with_membership(
                        &plan.driver.key,
                        &plan.driver.range,
                        &filters,
                        window.threshold(),
                    )
                    .await?
                    .len();
                window.apply(accepted)
            }
            exec::ExecCountPlan::EdgeRange(plan) => {
                validate_range_index("edge_range:", &plan.driver.index.index_id, &plan.driver.key)?;
                let window = self.count_window(&plan.window)?;
                let filters = match &plan.membership {
                    exec::ExecEdgeRangeMembershipPlan::All => Vec::new(),
                    exec::ExecEdgeRangeMembershipPlan::BitmapFilters(filters) => {
                        let mut bitmaps = Vec::with_capacity(filters.as_ref().len());
                        for filter in filters {
                            bitmaps.push(self.edge_bitmap(filter).await?);
                        }
                        bitmaps
                    }
                };
                let accepted = self
                    .edge_range_index_ids_with_membership(
                        &plan.driver.key,
                        &plan.driver.range,
                        &filters,
                        window.threshold(),
                    )
                    .await?
                    .len();
                window.apply(accepted)
            }
            exec::ExecCountPlan::NodeAuthoritativeScan(plan) => {
                let window = self.count_window(&plan.window)?;
                let rows = self
                    .authoritative_node_rows(&plan.predicate, window.threshold())
                    .await?;
                window.apply(rows)
            }
            exec::ExecCountPlan::EdgeAuthoritativeScan(plan) => {
                let window = self.count_window(&plan.window)?;
                let rows = self
                    .authoritative_edge_rows(&plan.predicate, window.threshold())
                    .await?;
                window.apply(rows)
            }
            exec::ExecCountPlan::NodePointReads { ids, window } => {
                let window = self.count_window(window)?;
                window.apply(
                    self.existing_node_count(ids.as_ref(), window.threshold())
                        .await?,
                )
            }
            exec::ExecCountPlan::EdgePointReads { ids, window } => {
                let window = self.count_window(window)?;
                window.apply(
                    self.existing_edge_count(ids.as_ref(), window.threshold())
                        .await?,
                )
            }
            exec::ExecCountPlan::NodeRuntimeInput { input, window } => {
                let window = self.count_window(window)?;
                let ids = self.runtime_ids(input)?;
                window.apply(self.existing_node_count(&ids, window.threshold()).await?)
            }
            exec::ExecCountPlan::EdgeRuntimeInput { input, window } => {
                let window = self.count_window(window)?;
                let ids = self.runtime_ids(input)?;
                window.apply(self.existing_edge_count(&ids, window.threshold()).await?)
            }
            exec::ExecCountPlan::RuntimeInput { input, window } => {
                let window = self.count_window(window)?;
                window.apply(self.runtime_row_count(input)?)
            }
            exec::ExecCountPlan::NodeFullScan { window } => {
                let window = self.count_window(window)?;
                let limit = positive_limit(window.threshold());
                let ids = self
                    .scan_element_ids(exec::ElementKeyspace::NodeProperty, limit)
                    .await?;
                window.apply(ids.len())
            }
            exec::ExecCountPlan::EdgeFullScan { window } => {
                let window = self.count_window(window)?;
                let limit = positive_limit(window.threshold());
                let ids = self
                    .scan_element_ids(exec::ElementKeyspace::EdgeEndpoints, limit)
                    .await?;
                window.apply(ids.len())
            }
            exec::ExecCountPlan::NodeLabelBitmap { label, window } => {
                let window = self.count_window(window)?;
                let bitmap = self
                    .lookup_equality_index_set(
                        "$label",
                        &DbPropertyValue::String(label.as_ref().to_string()),
                    )
                    .await?;
                window.apply(bitmap.len() as usize)
            }
            exec::ExecCountPlan::EdgeLabelBitmap { label, window } => {
                let window = self.count_window(window)?;
                let bitmap = self.lookup_global_edge_label_index(label.as_ref()).await?;
                window.apply(bitmap.len() as usize)
            }
            exec::ExecCountPlan::NodeVectorSearch(plan) => {
                let window = self.count_window(&plan.window)?;
                let results = self
                    .vector_search_results(
                        VectorElementType::Node,
                        &plan.key.label,
                        &plan.key.property,
                        &plan.index,
                        &plan.query_vector,
                        SearchReadLimit::new(&plan.k, None),
                    )
                    .await?;
                let ids = results
                    .into_iter()
                    .map(|result| match result.entity_id() {
                        VectorEntityId::Node(id) => Ok(id),
                        VectorEntityId::Edge(_) => Err(HelixDbError::InvariantViolation(
                            "edge-bound vector result reached exact node count".to_string(),
                        )),
                    })
                    .collect::<Result<Vec<_>>>()?;
                window.apply(self.existing_node_count(&ids, window.threshold()).await?)
            }
            exec::ExecCountPlan::EdgeVectorSearch(plan) => {
                let window = self.count_window(&plan.window)?;
                let results = self
                    .vector_search_results(
                        VectorElementType::Edge,
                        &plan.key.label,
                        &plan.key.property,
                        &plan.index,
                        &plan.query_vector,
                        SearchReadLimit::new(&plan.k, None),
                    )
                    .await?;
                let ids = results
                    .into_iter()
                    .map(|result| match result.entity_id() {
                        VectorEntityId::Edge(id) => Ok(id),
                        VectorEntityId::Node(_) => Err(HelixDbError::InvariantViolation(
                            "node-bound vector result reached exact edge count".to_string(),
                        )),
                    })
                    .collect::<Result<Vec<_>>>()?;
                window.apply(self.existing_edge_count(&ids, window.threshold()).await?)
            }
            exec::ExecCountPlan::NodeTextSearch(plan) => {
                let window = self.count_window(&plan.window)?;
                let results = self
                    .text_search_hits(
                        TextElementType::Node,
                        &plan.key.label,
                        &plan.key.property,
                        &plan.index,
                        &plan.query_text,
                        SearchReadLimit::new(&plan.k, None),
                    )
                    .await?;
                let ids = results
                    .into_iter()
                    .map(|result| result.entity_id)
                    .collect::<Vec<_>>();
                window.apply(self.existing_node_count(&ids, window.threshold()).await?)
            }
            exec::ExecCountPlan::EdgeTextSearch(plan) => {
                let window = self.count_window(&plan.window)?;
                let results = self
                    .text_search_hits(
                        TextElementType::Edge,
                        &plan.key.label,
                        &plan.key.property,
                        &plan.index,
                        &plan.query_text,
                        SearchReadLimit::new(&plan.k, None),
                    )
                    .await?;
                let ids = results
                    .into_iter()
                    .map(|result| result.entity_id)
                    .collect::<Vec<_>>();
                window.apply(self.existing_edge_count(&ids, window.threshold()).await?)
            }
            exec::ExecCountPlan::NodeDynamicEquality(plan) => {
                validate_node_equality_index(&plan.index.index_id, &plan.key)?;
                let window = self.count_window(&plan.window)?;
                let value = self.param_value(&plan.param)?;
                let ids = self
                    .lookup_managed_equality_union(
                        crate::index_lifecycle::IndexElementKind::Node,
                        &plan.key,
                        core::slice::from_ref(&value),
                    )
                    .await?;
                window.apply(ids.len() as usize)
            }
            exec::ExecCountPlan::EdgeDynamicEquality(plan) => {
                validate_edge_equality_index(&plan.index.index_id, &plan.key)?;
                let window = self.count_window(&plan.window)?;
                let value = self.param_value(&plan.param)?;
                let ids = self
                    .lookup_managed_equality_union(
                        crate::index_lifecycle::IndexElementKind::Edge,
                        &plan.key,
                        core::slice::from_ref(&value),
                    )
                    .await?;
                window.apply(ids.len() as usize)
            }
            exec::ExecCountPlan::Stream(plan) => {
                let mut dependency = Some(input);
                let rows = self.count_cursor(&plan.cursor, &mut dependency).await?;
                if dependency.is_some() && dependency_contract == exec::ExecCountDependency::Rows {
                    return Err(HelixDbError::InvariantViolation(
                        "count cursor did not consume its encoded row dependency".to_string(),
                    ));
                }
                self.count_window(&plan.window)?.apply(rows.len())
            }
            exec::ExecCountPlan::InputRows { window } => {
                let rows = match input {
                    ExecutionValue::Stream(rows) => rows.len(),
                    other @ (ExecutionValue::FoldedStream(_)
                    | ExecutionValue::Count(_)
                    | ExecutionValue::Bool(_)
                    | ExecutionValue::Scalars(_)
                    | ExecutionValue::IndexDdlReceipt(_)
                    | ExecutionValue::IndexOperationStatus(_)) => {
                        return Err(count_shape_error("rows", &other));
                    }
                };
                self.count_window(window)?.apply(rows)
            }
            exec::ExecCountPlan::InputScalars { window } => {
                let scalars = match input {
                    ExecutionValue::Count(_) | ExecutionValue::Bool(_) => 1,
                    ExecutionValue::Scalars(values) => values.len(),
                    other @ (ExecutionValue::Stream(_)
                    | ExecutionValue::FoldedStream(_)
                    | ExecutionValue::IndexDdlReceipt(_)
                    | ExecutionValue::IndexOperationStatus(_)) => {
                        return Err(count_shape_error("scalar items", &other));
                    }
                };
                self.count_window(window)?.apply(scalars)
            }
        };
        Ok(ExecutionValue::Count(count))
    }

    fn count_window(&self, plan: &exec::ExecCountWindowPlan) -> Result<EvaluatedCountWindow> {
        let mut resolve = |name: &ir::NonEmptyString| self.count_bound_param(name);
        let skip = plan.skip.evaluate(&mut resolve)?;
        let take = match &plan.take {
            exec::ExecCountTake::All => None,
            exec::ExecCountTake::AtMost(take) => Some(take.evaluate(&mut resolve)?),
        };
        Ok(EvaluatedCountWindow { skip, take })
    }

    fn count_bound_param(&self, name: &ir::NonEmptyString) -> Result<usize> {
        if let Some(value) = self.params.values.get(name) {
            let AstPropertyValue::I64(value) = value else {
                return Err(HelixDbError::Query(format!(
                    "count window parameter `{name}` is not a non-negative integer"
                )));
            };
            return usize::try_from(*value).map_err(|_| {
                HelixDbError::Query(format!(
                    "count window parameter `{name}` is not a non-negative integer"
                ))
            });
        }
        let Some(value) = self.params.query_values.get(name) else {
            return Err(HelixDbError::Query(format!(
                "count window parameter `{name}` is not bound"
            )));
        };
        let QueryValue::I64(value) = value else {
            return Err(HelixDbError::Query(format!(
                "count window parameter `{name}` is not a non-negative integer"
            )));
        };
        usize::try_from(*value).map_err(|_| {
            HelixDbError::Query(format!(
                "count window parameter `{name}` is not a non-negative integer"
            ))
        })
    }

    fn runtime_ids(&self, input: &exec::ExecRuntimeInputPlan) -> Result<Vec<u64>> {
        match input {
            exec::ExecRuntimeInputPlan::Param(param) => self.param_ids(param),
            exec::ExecRuntimeInputPlan::Variable(variable) => {
                let value = self.variable_value(variable)?;
                Ok(match value {
                    ExecutionValue::Stream(rows) => rows
                        .iter()
                        .filter_map(|row| row.current.as_ref().map(ElementRef::id))
                        .collect::<Vec<_>>(),
                    other @ (ExecutionValue::FoldedStream(_)
                    | ExecutionValue::Count(_)
                    | ExecutionValue::Bool(_)
                    | ExecutionValue::Scalars(_)
                    | ExecutionValue::IndexDdlReceipt(_)
                    | ExecutionValue::IndexOperationStatus(_)) => {
                        return Err(count_shape_error("element rows", other));
                    }
                })
            }
        }
    }

    fn runtime_row_count(&self, input: &exec::ExecRuntimeInputPlan) -> Result<usize> {
        match input {
            exec::ExecRuntimeInputPlan::Param(param) => self.param_ids(param).map(|ids| ids.len()),
            exec::ExecRuntimeInputPlan::Variable(variable) => {
                match self.variable_value(variable)? {
                    ExecutionValue::Stream(rows) => Ok(rows.len()),
                    other @ (ExecutionValue::FoldedStream(_)
                    | ExecutionValue::Count(_)
                    | ExecutionValue::Bool(_)
                    | ExecutionValue::Scalars(_)
                    | ExecutionValue::IndexDdlReceipt(_)
                    | ExecutionValue::IndexOperationStatus(_)) => {
                        Err(count_shape_error("rows", other))
                    }
                }
            }
        }
    }

    async fn existing_node_count(&self, ids: &[u64], threshold: Option<usize>) -> Result<usize> {
        let mut count = 0usize;
        for id in ids {
            if threshold.is_some_and(|threshold| count >= threshold) {
                break;
            }
            let key = keys::Key::Data {
                scope: self.tenant_scope,
                kind: keys::DataKeyKind::NodeProperty(keys::NodePropertyKey::new(*id)),
            }
            .to_bytes();
            if self.get_raw(&key).await?.is_some() {
                count = count.saturating_add(1);
            }
        }
        Ok(count)
    }

    async fn existing_edge_count(&self, ids: &[u64], threshold: Option<usize>) -> Result<usize> {
        let mut count = 0usize;
        for id in ids {
            if threshold.is_some_and(|threshold| count >= threshold) {
                break;
            }
            let key = keys::Key::Data {
                scope: self.tenant_scope,
                kind: keys::DataKeyKind::EdgeEndpoints(keys::EdgeEndpointsKey::new(*id)),
            }
            .to_bytes();
            if self.get_raw(&key).await?.is_some() {
                count = count.saturating_add(1);
            }
        }
        Ok(count)
    }

    async fn authoritative_node_rows(
        &self,
        predicate: &exec::ExecNodeAuthoritativeScanPredicate,
        threshold: Option<usize>,
    ) -> Result<usize> {
        let ids = self
            .scan_element_ids(exec::ElementKeyspace::NodeProperty, None)
            .await?;
        let mut accepted = 0usize;
        for id in ids {
            if threshold.is_some_and(|threshold| accepted >= threshold) {
                break;
            }
            let row = ExecutionRow::current(ElementRef::Node(id));
            let matches = match predicate {
                exec::ExecNodeAuthoritativeScanPredicate::NullEquality { key } => {
                    self.scoped_null_matches(&row, key).await?
                }
                exec::ExecNodeAuthoritativeScanPredicate::Predicate(predicate) => {
                    self.eval_predicate(&row, predicate.predicate()).await?
                }
            };
            if matches {
                accepted = accepted.saturating_add(1);
            }
        }
        Ok(accepted)
    }

    async fn authoritative_edge_rows(
        &self,
        predicate: &exec::ExecEdgeAuthoritativeScanPredicate,
        threshold: Option<usize>,
    ) -> Result<usize> {
        let ids = self
            .scan_element_ids(exec::ElementKeyspace::EdgeEndpoints, None)
            .await?;
        let mut accepted = 0usize;
        for id in ids {
            if threshold.is_some_and(|threshold| accepted >= threshold) {
                break;
            }
            let row = ExecutionRow::current(ElementRef::Edge(id));
            let matches = match predicate {
                exec::ExecEdgeAuthoritativeScanPredicate::NullEquality { key } => {
                    self.scoped_null_matches(&row, key).await?
                }
                exec::ExecEdgeAuthoritativeScanPredicate::Predicate(predicate) => {
                    self.eval_predicate(&row, predicate.predicate()).await?
                }
            };
            if matches {
                accepted = accepted.saturating_add(1);
            }
        }
        Ok(accepted)
    }

    pub(in crate::execution::interpreter) async fn scoped_null_matches(
        &self,
        row: &ExecutionRow,
        key: &helix_planner::catalog::ScopedPropertyKey,
    ) -> Result<bool> {
        let label = ir::NonEmptyString::from_static("$label");
        if self
            .row_property(row, &label)
            .await?
            .as_ref()
            .and_then(DbPropertyValue::as_str)
            != Some(key.label.as_ref())
        {
            return Ok(false);
        }
        Ok(self
            .row_property(row, &key.property)
            .await?
            .is_none_or(|value| matches!(value, DbPropertyValue::Null)))
    }

    /// Execute the exact unique-owner primitive and verify the authoritative row.
    ///
    /// An absent owner is a legitimate miss. A present owner whose label or
    /// indexed value disagrees with the plan is catalog corruption, never a
    /// reason to substitute an authoritative scan.
    pub(in crate::execution::interpreter) async fn verified_node_unique_owner(
        &self,
        lookup: &exec::ExecNodeUniqueOwnerReadPlan,
        verification: &exec::ExecNodeAuthoritativeVerificationPlan,
    ) -> Result<Option<u64>> {
        validate_node_equality_index(&lookup.index.metadata().index_id, &lookup.key)?;
        if lookup.key != verification.key || lookup.value != verification.value {
            return Err(HelixDbError::InvariantViolation(
                "unique verification does not match its exact owner lookup".to_string(),
            ));
        }
        let ids = self
            .lookup_managed_equality_point_exact(
                crate::index_lifecycle::IndexElementKind::Node,
                &lookup.key,
                &indexed_value(&lookup.value),
                true,
            )
            .await?;
        let mut ids = ids.into_iter();
        let Some(id) = ids.next() else {
            return Ok(None);
        };
        if ids.next().is_some() {
            return Err(HelixDbError::IndexCatalogCorruption(
                "unique equality owner read returned multiple node IDs".to_string(),
            ));
        }
        let row = ExecutionRow::current(ElementRef::Node(id));
        let expected = indexed_value(&verification.value);
        crate::index_lifecycle::secondary::record_equality_graph_read();
        let properties = self.row_properties(&row).await?;
        let label_matches = properties.iter().any(|property| {
            property.name == "$label"
                && property.value.as_str() == Some(verification.key.label.as_ref())
        });
        let value_matches = properties.iter().any(|property| {
            property.name == verification.key.property.as_ref()
                && property.value.eq_value(&expected)
        });
        if !label_matches || !value_matches {
            return Err(HelixDbError::IndexCatalogCorruption(
                "unique equality owner disagrees with its authoritative node".to_string(),
            ));
        }
        Ok(Some(id))
    }

    pub(in crate::execution::interpreter) fn node_bitmap<'a>(
        &'a self,
        expression: &'a exec::ExecNodeBitmapExpr,
    ) -> BoxFuture<'a, Result<roaring::RoaringTreemap>> {
        async move {
            self.check_execution_deadline()?;
            match expression {
                exec::ExecNodeBitmapExpr::PointRead { index, key, value } => {
                    validate_node_equality_index(&index.metadata().index_id, key)?;
                    self.lookup_managed_equality_point_exact(
                        crate::index_lifecycle::IndexElementKind::Node,
                        key,
                        &indexed_value(value),
                        false,
                    )
                    .await
                }
                exec::ExecNodeBitmapExpr::BatchedUnionRead { index, key, values } => {
                    validate_node_equality_index(&index.metadata().index_id, key)?;
                    let values = values.iter().map(indexed_value).collect::<Vec<_>>();
                    self.lookup_managed_equality_literal_batch(
                        crate::index_lifecycle::IndexElementKind::Node,
                        key,
                        &values,
                    )
                    .await
                }
                exec::ExecNodeBitmapExpr::Union { driver, rest } => {
                    let mut result = self.node_bitmap(driver).await?;
                    for child in rest {
                        result |= self.node_bitmap(child).await?;
                    }
                    Ok(result)
                }
                exec::ExecNodeBitmapExpr::Intersect { driver, rest } => {
                    let mut result = self.node_bitmap(driver).await?;
                    for child in rest {
                        result &= self.node_bitmap(child).await?;
                    }
                    Ok(result)
                }
            }
        }
        .boxed()
    }

    pub(in crate::execution::interpreter) fn edge_bitmap<'a>(
        &'a self,
        expression: &'a exec::ExecEdgeBitmapExpr,
    ) -> BoxFuture<'a, Result<roaring::RoaringTreemap>> {
        async move {
            self.check_execution_deadline()?;
            match expression {
                exec::ExecEdgeBitmapExpr::PointRead { index, key, value } => {
                    validate_edge_equality_index(&index.metadata().index_id, key)?;
                    self.lookup_managed_equality_point_exact(
                        crate::index_lifecycle::IndexElementKind::Edge,
                        key,
                        &indexed_value(value),
                        false,
                    )
                    .await
                }
                exec::ExecEdgeBitmapExpr::BatchedUnionRead { index, key, values } => {
                    validate_edge_equality_index(&index.metadata().index_id, key)?;
                    let values = values.iter().map(indexed_value).collect::<Vec<_>>();
                    self.lookup_managed_equality_literal_batch(
                        crate::index_lifecycle::IndexElementKind::Edge,
                        key,
                        &values,
                    )
                    .await
                }
                exec::ExecEdgeBitmapExpr::Union { driver, rest } => {
                    let mut result = self.edge_bitmap(driver).await?;
                    for child in rest {
                        result |= self.edge_bitmap(child).await?;
                    }
                    Ok(result)
                }
                exec::ExecEdgeBitmapExpr::Intersect { driver, rest } => {
                    let mut result = self.edge_bitmap(driver).await?;
                    for child in rest {
                        result &= self.edge_bitmap(child).await?;
                    }
                    Ok(result)
                }
            }
        }
        .boxed()
    }

    fn count_cursor<'a>(
        &'a mut self,
        cursor: &'a exec::ExecCountCursorPlan,
        dependency: &'a mut Option<ExecutionValue>,
    ) -> BoxFuture<'a, Result<Vec<ExecutionRow>>> {
        async move {
            self.check_execution_deadline()?;
            match cursor {
                exec::ExecCountCursorPlan::EmptyRows => Ok(Vec::new()),
                exec::ExecCountCursorPlan::InputRows => match dependency.take() {
                    Some(ExecutionValue::Stream(rows)) => Ok(rows),
                    Some(other) => Err(count_shape_error("rows", &other)),
                    None => Err(HelixDbError::InvariantViolation(
                        "count cursor consumed its row dependency more than once".to_string(),
                    )),
                },
                exec::ExecCountCursorPlan::NodeBitmap(bitmap) => Ok(self
                    .node_bitmap(bitmap)
                    .await?
                    .into_iter()
                    .map(|id| ExecutionRow::current(ElementRef::Node(id)))
                    .collect()),
                exec::ExecCountCursorPlan::EdgeBitmap(bitmap) => Ok(self
                    .edge_bitmap(bitmap)
                    .await?
                    .into_iter()
                    .map(|id| ExecutionRow::current(ElementRef::Edge(id)))
                    .collect()),
                exec::ExecCountCursorPlan::NodeUnique {
                    lookup,
                    verification,
                } => Ok(self
                    .verified_node_unique_owner(lookup, verification)
                    .await?
                    .into_iter()
                    .map(|id| ExecutionRow::current(ElementRef::Node(id)))
                    .collect()),
                exec::ExecCountCursorPlan::NodeRange(plan) => Ok(self
                    .validated_node_range_ids(plan)
                    .await?
                    .into_iter()
                    .map(|id| ExecutionRow::current(ElementRef::Node(id)))
                    .collect()),
                exec::ExecCountCursorPlan::EdgeRange(plan) => Ok(self
                    .validated_edge_range_ids(plan)
                    .await?
                    .into_iter()
                    .map(|id| ExecutionRow::current(ElementRef::Edge(id)))
                    .collect()),
                exec::ExecCountCursorPlan::NodeAuthoritativeScan(predicate) => {
                    let ids = self
                        .scan_element_ids(exec::ElementKeyspace::NodeProperty, None)
                        .await?;
                    let mut rows = Vec::new();
                    for id in ids {
                        let row = ExecutionRow::current(ElementRef::Node(id));
                        let matches = match predicate {
                            exec::ExecNodeAuthoritativeScanPredicate::NullEquality { key } => {
                                self.scoped_null_matches(&row, key).await?
                            }
                            exec::ExecNodeAuthoritativeScanPredicate::Predicate(predicate) => {
                                self.eval_predicate(&row, predicate.predicate()).await?
                            }
                        };
                        if matches {
                            rows.push(row);
                        }
                    }
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::EdgeAuthoritativeScan(predicate) => {
                    let ids = self
                        .scan_element_ids(exec::ElementKeyspace::EdgeEndpoints, None)
                        .await?;
                    let mut rows = Vec::new();
                    for id in ids {
                        let row = ExecutionRow::current(ElementRef::Edge(id));
                        let matches = match predicate {
                            exec::ExecEdgeAuthoritativeScanPredicate::NullEquality { key } => {
                                self.scoped_null_matches(&row, key).await?
                            }
                            exec::ExecEdgeAuthoritativeScanPredicate::Predicate(predicate) => {
                                self.eval_predicate(&row, predicate.predicate()).await?
                            }
                        };
                        if matches {
                            rows.push(row);
                        }
                    }
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::NodePointReads(ids) => {
                    let ExecutionValue::Stream(rows) =
                        self.node_rows(ids.as_ref().to_vec()).await?
                    else {
                        unreachable!("node row materialization returns a stream")
                    };
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::EdgePointReads(ids) => {
                    let ExecutionValue::Stream(rows) =
                        self.edge_rows(ids.as_ref().to_vec()).await?
                    else {
                        unreachable!("edge row materialization returns a stream")
                    };
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::NodeRuntimeInput(input) => {
                    let ids = self.runtime_ids(input)?;
                    let ExecutionValue::Stream(rows) = self.node_rows(ids).await? else {
                        unreachable!("node row materialization returns a stream")
                    };
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::EdgeRuntimeInput(input) => {
                    let ids = self.runtime_ids(input)?;
                    let ExecutionValue::Stream(rows) = self.edge_rows(ids).await? else {
                        unreachable!("edge row materialization returns a stream")
                    };
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::RuntimeInput(input) => match input {
                    exec::ExecRuntimeInputPlan::Variable(variable) => {
                        match self.variable_value(variable)?.clone() {
                            ExecutionValue::Stream(rows) => Ok(rows),
                            other @ (ExecutionValue::FoldedStream(_)
                            | ExecutionValue::Count(_)
                            | ExecutionValue::Bool(_)
                            | ExecutionValue::Scalars(_)
                            | ExecutionValue::IndexDdlReceipt(_)
                            | ExecutionValue::IndexOperationStatus(_)) => {
                                Err(count_shape_error("rows", &other))
                            }
                        }
                    }
                    exec::ExecRuntimeInputPlan::Param(param) => {
                        let ids = self.param_ids(param)?;
                        Ok(ids
                            .into_iter()
                            .map(|id| ExecutionRow::current(ElementRef::Node(id)))
                            .collect())
                    }
                },
                exec::ExecCountCursorPlan::NodeFullScan => {
                    let ids = self
                        .scan_element_ids(exec::ElementKeyspace::NodeProperty, None)
                        .await?;
                    Ok(ids
                        .into_iter()
                        .map(|id| ExecutionRow::current(ElementRef::Node(id)))
                        .collect())
                }
                exec::ExecCountCursorPlan::EdgeFullScan => {
                    let ids = self
                        .scan_element_ids(exec::ElementKeyspace::EdgeEndpoints, None)
                        .await?;
                    Ok(ids
                        .into_iter()
                        .map(|id| ExecutionRow::current(ElementRef::Edge(id)))
                        .collect())
                }
                exec::ExecCountCursorPlan::NodeLabelBitmap(label) => Ok(self
                    .lookup_equality_index_set(
                        "$label",
                        &DbPropertyValue::String(label.as_ref().to_string()),
                    )
                    .await?
                    .into_iter()
                    .map(|id| ExecutionRow::current(ElementRef::Node(id)))
                    .collect()),
                exec::ExecCountCursorPlan::EdgeLabelBitmap(label) => Ok(self
                    .lookup_global_edge_label_index(label.as_ref())
                    .await?
                    .into_iter()
                    .map(|id| ExecutionRow::current(ElementRef::Edge(id)))
                    .collect()),
                exec::ExecCountCursorPlan::NodeVectorSearch {
                    key,
                    index,
                    query_vector,
                    k,
                } => {
                    let results = self
                        .vector_search_results(
                            VectorElementType::Node,
                            &key.label,
                            &key.property,
                            index,
                            query_vector,
                            SearchReadLimit::new(k, None),
                        )
                        .await?;
                    let ExecutionValue::Stream(rows) = self.node_search_rows(results).await? else {
                        unreachable!("node search materialization returns a stream")
                    };
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::EdgeVectorSearch {
                    key,
                    index,
                    query_vector,
                    k,
                } => {
                    let results = self
                        .vector_search_results(
                            VectorElementType::Edge,
                            &key.label,
                            &key.property,
                            index,
                            query_vector,
                            SearchReadLimit::new(k, None),
                        )
                        .await?;
                    let ExecutionValue::Stream(rows) = self.edge_search_rows(results).await? else {
                        unreachable!("edge search materialization returns a stream")
                    };
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::NodeTextSearch {
                    key,
                    index,
                    query_text,
                    k,
                } => {
                    let hits = self
                        .text_search_hits(
                            TextElementType::Node,
                            &key.label,
                            &key.property,
                            index,
                            query_text,
                            SearchReadLimit::new(k, None),
                        )
                        .await?;
                    let ExecutionValue::Stream(rows) = self.node_text_search_rows(hits).await?
                    else {
                        unreachable!("node text materialization returns a stream")
                    };
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::EdgeTextSearch {
                    key,
                    index,
                    query_text,
                    k,
                } => {
                    let hits = self
                        .text_search_hits(
                            TextElementType::Edge,
                            &key.label,
                            &key.property,
                            index,
                            query_text,
                            SearchReadLimit::new(k, None),
                        )
                        .await?;
                    let ExecutionValue::Stream(rows) = self.edge_text_search_rows(hits).await?
                    else {
                        unreachable!("edge text materialization returns a stream")
                    };
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::NodeDynamicEquality { index, key, param } => {
                    validate_node_equality_index(&index.index_id, key)?;
                    let value = self.param_value(param)?;
                    Ok(self
                        .lookup_managed_equality_union(
                            crate::index_lifecycle::IndexElementKind::Node,
                            key,
                            core::slice::from_ref(&value),
                        )
                        .await?
                        .into_iter()
                        .map(|id| ExecutionRow::current(ElementRef::Node(id)))
                        .collect())
                }
                exec::ExecCountCursorPlan::EdgeDynamicEquality { index, key, param } => {
                    validate_edge_equality_index(&index.index_id, key)?;
                    let value = self.param_value(param)?;
                    Ok(self
                        .lookup_managed_equality_union(
                            crate::index_lifecycle::IndexElementKind::Edge,
                            key,
                            core::slice::from_ref(&value),
                        )
                        .await?
                        .into_iter()
                        .map(|id| ExecutionRow::current(ElementRef::Edge(id)))
                        .collect())
                }
                exec::ExecCountCursorPlan::Union { driver, rest } => {
                    let mut rows = self.count_cursor(driver, dependency).await?;
                    let mut seen = rows.iter().cloned().collect::<BTreeSet<_>>();
                    for child in rest {
                        for row in self.count_cursor(child, dependency).await? {
                            if seen.insert(row.clone()) {
                                rows.push(row);
                            }
                        }
                    }
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::Intersect { driver, rest } => {
                    let mut rows = self.count_cursor(driver, dependency).await?;
                    for child in rest {
                        let allowed = self
                            .count_cursor(child, dependency)
                            .await?
                            .into_iter()
                            .collect::<BTreeSet<_>>();
                        rows.retain(|row| allowed.contains(row));
                    }
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::Filter { input, predicate } => {
                    let input = ExecutionValue::Stream(self.count_cursor(input, dependency).await?);
                    let ExecutionValue::Stream(rows) = self.filter(input, predicate).await? else {
                        unreachable!("row filter returns a stream")
                    };
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::Window { input, window } => {
                    let rows = self.count_cursor(input, dependency).await?;
                    Ok(self.count_window(window)?.apply_rows(rows))
                }
                exec::ExecCountCursorPlan::Order { input, plan } => {
                    let input = ExecutionValue::Stream(self.count_cursor(input, dependency).await?);
                    let ExecutionValue::Stream(rows) = self.order(input, plan).await? else {
                        unreachable!("row order returns a stream")
                    };
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::Expand { input, plan } => {
                    let input = ExecutionValue::Stream(self.count_cursor(input, dependency).await?);
                    let ExecutionValue::Stream(rows) = self.expand(input, plan).await? else {
                        unreachable!("row expansion returns a stream")
                    };
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::VectorSearch { input, plan } => {
                    let input = ExecutionValue::Stream(self.count_cursor(input, dependency).await?);
                    let ExecutionValue::Stream(rows) =
                        self.restricted_vector_search(input, plan).await?
                    else {
                        unreachable!("restricted vector search returns a stream")
                    };
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::TextSearch { input, plan } => {
                    let input = ExecutionValue::Stream(self.count_cursor(input, dependency).await?);
                    let ExecutionValue::Stream(rows) =
                        self.restricted_text_search(input, plan).await?
                    else {
                        unreachable!("restricted text search returns a stream")
                    };
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::Variable { input, op } => {
                    let input = ExecutionValue::Stream(self.count_cursor(input, dependency).await?);
                    let executable = exec::ExecVariableOp::Stream(op.to_stream_op());
                    let ExecutionValue::Stream(rows) = self.variable(input, &executable)? else {
                        return Err(HelixDbError::InvariantViolation(
                            "pure count variable cursor produced a non-row shape".to_string(),
                        ));
                    };
                    Ok(rows)
                }
                exec::ExecCountCursorPlan::Distinct { input, plan } => {
                    let mut rows = self.count_cursor(input, dependency).await?;
                    match plan {
                        exec::ExecCountDistinctPlan::HashRows => {
                            let ExecutionValue::Stream(distinct) =
                                self.distinct(ExecutionValue::Stream(rows))?
                            else {
                                unreachable!("row distinct returns a stream")
                            };
                            Ok(distinct)
                        }
                        exec::ExecCountDistinctPlan::OrderedRows => {
                            rows.dedup();
                            Ok(rows)
                        }
                    }
                }
            }
        }
        .boxed()
    }

    async fn validated_node_range_ids(
        &self,
        plan: &exec::ExecNodeVerifiedRangeScanPlan,
    ) -> Result<Vec<u64>> {
        validate_range_index("node_range:", &plan.index.index_id, &plan.key)?;
        self.node_range_index_ids(&plan.key, &plan.range, None)
            .await
    }

    async fn validated_edge_range_ids(
        &self,
        plan: &exec::ExecEdgeVerifiedRangeScanPlan,
    ) -> Result<Vec<u64>> {
        validate_range_index("edge_range:", &plan.index.index_id, &plan.key)?;
        self.edge_range_index_ids(&plan.key, &plan.range, None)
            .await
    }
}

pub(in crate::execution::interpreter) fn indexed_value(
    value: &exec::ExecIndexedEqualityValue,
) -> DbPropertyValue {
    stream::ast_to_db_value(value.literal().as_property_value().clone())
}

fn positive_limit(limit: Option<usize>) -> Option<properties::PositiveUsize> {
    limit.and_then(properties::PositiveUsize::new)
}

fn count_shape_error(expected: &str, actual: &ExecutionValue) -> HelixDbError {
    if matches!(
        actual,
        ExecutionValue::IndexDdlReceipt(_) | ExecutionValue::IndexOperationStatus(_)
    ) {
        return HelixDbError::Query("count cannot consume an index lifecycle value".to_string());
    }
    HelixDbError::InvariantViolation(format!("count plan expected {expected}, got {actual:?}"))
}

pub(in crate::execution::interpreter) fn validate_node_equality_index(
    index_id: &ir::NonEmptyString,
    key: &helix_planner::catalog::ScopedPropertyKey,
) -> Result<()> {
    validate_index_id("node_eq:", index_id, key)
}

pub(in crate::execution::interpreter) fn validate_edge_equality_index(
    index_id: &ir::NonEmptyString,
    key: &helix_planner::catalog::ScopedPropertyKey,
) -> Result<()> {
    validate_index_id("edge_eq:", index_id, key)
}

fn validate_range_index(
    prefix: &'static str,
    index_id: &ir::NonEmptyString,
    key: &helix_planner::catalog::ScopedPropertyDirectionKey,
) -> Result<()> {
    validate_index_id(prefix, index_id, key)
}

fn validate_index_id(
    prefix: &'static str,
    actual: &ir::NonEmptyString,
    key: &impl std::fmt::Display,
) -> Result<()> {
    let expected = ir::NonEmptyString::from_prefixed_display(prefix, key);
    if actual != &expected {
        return Err(HelixDbError::IndexCatalogCorruption(format!(
            "planner logical index identity `{actual}` disagrees with `{expected}`"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use helix_ast::value::PropertyValue;
    use helix_planner::{catalog, context};

    use super::super::test_support;
    use super::*;

    fn indexed(value: &str) -> exec::ExecIndexedEqualityValue {
        exec::ExecIndexedEqualityValue::try_from(
            ir::SecondaryIndexLiteral::new(PropertyValue::from(value)).unwrap(),
        )
        .unwrap()
    }

    fn node_equality_index(label: &str, property: &str) -> exec::ExecNodeNonUniqueEqualityIndex {
        exec::ExecNodeNonUniqueEqualityIndex::try_from(catalog::NodeEqualityIndexMeta::new(
            test_support::name(&format!("node_eq:{label}:{property}")),
        ))
        .unwrap()
    }

    fn node_point(label: &str, property: &str, value: &str) -> exec::ExecNodeBitmapExpr {
        exec::ExecNodeBitmapExpr::PointRead {
            index: node_equality_index(label, property),
            key: catalog::ScopedPropertyKey::try_new(label, property).unwrap(),
            value: indexed(value),
        }
    }

    async fn execute_direct_count(
        db: &HelixDB,
        plan: exec::ExecCountPlan,
    ) -> Result<ExecutionValue> {
        let executable = test_support::executable(
            ir::PlanKind::Read,
            vec![test_support::step(
                1,
                Vec::new(),
                exec::ExecOp::Count {
                    plan: Box::new(plan),
                },
            )],
            1,
        );
        db.execute(&executable, context::ParamBindings::default())
            .await?
            .last
            .ok_or_else(|| {
                HelixDbError::InvariantViolation("direct count test has no result".to_string())
            })
    }

    #[tokio::test]
    async fn input_shapes_and_parameterized_windows_are_literal_contracts() {
        let db = test_support::open_db("count-input-shapes-and-windows").await;
        let skip = test_support::name("skip");
        let take = test_support::name("take");
        let params = context::ParamBindings::default()
            .with_value(skip.clone(), PropertyValue::I64(1))
            .with_value(take.clone(), PropertyValue::I64(2));
        let mut context = ExecutionContext::new(&db, params);
        let rows = vec![
            ExecutionRow::current(ElementRef::Node(1)),
            ExecutionRow::current(ElementRef::Node(2)),
            ExecutionRow::current(ElementRef::Node(3)),
            ExecutionRow::current(ElementRef::Node(4)),
        ];
        let plan = exec::ExecCountPlan::InputRows {
            window: exec::ExecCountWindowPlan {
                skip: exec::ExecUsizeExpr::Param(skip),
                take: exec::ExecCountTake::AtMost(exec::ExecUsizeExpr::Param(take)),
            },
        };

        assert_eq!(
            context
                .execute_count(ExecutionValue::Stream(rows), &plan)
                .await
                .unwrap(),
            ExecutionValue::Count(2)
        );
        let error = context
            .execute_count(ExecutionValue::Scalars(Vec::new()), &plan)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("count plan expected rows"));
    }

    #[tokio::test]
    async fn point_and_batch_bitmap_counts_issue_only_the_encoded_primitive() {
        let db = test_support::open_db_with_config(
            test_support::in_memory_config("count-exact-bitmap-primitives")
                .with_equality_index("User", "status"),
        )
        .await;
        test_support::add_node_with_properties(
            &db,
            "User",
            vec![("status", PropertyValue::from("active"))],
        )
        .await;
        test_support::add_node_with_properties(
            &db,
            "User",
            vec![("status", PropertyValue::from("inactive"))],
        )
        .await;
        crate::index_lifecycle::secondary::reset_equality_read_metrics();
        let point = exec::ExecCountPlan::NodeBitmap(exec::ExecNodeBitmapCountPlan {
            bitmap: node_point("User", "status", "active"),
            window: exec::ExecCountWindowPlan::identity(),
        });
        assert_eq!(
            execute_direct_count(&db, point).await.unwrap(),
            ExecutionValue::Count(1)
        );
        assert_eq!(
            crate::index_lifecycle::secondary::equality_read_metrics(),
            crate::index_lifecycle::secondary::SecondaryEqualityReadMetrics {
                point_reads: 2,
                multi_get_calls: 0,
                scans: 0,
                graph_reads: 0,
            }
        );

        crate::index_lifecycle::secondary::reset_equality_read_metrics();
        let batch = exec::ExecCountPlan::NodeBitmap(exec::ExecNodeBitmapCountPlan {
            bitmap: exec::ExecNodeBitmapExpr::BatchedUnionRead {
                index: node_equality_index("User", "status"),
                key: catalog::ScopedPropertyKey::try_new("User", "status").unwrap(),
                values: ir::AtLeast::from_pair(indexed("active"), indexed("inactive")),
            },
            window: exec::ExecCountWindowPlan::identity(),
        });
        assert_eq!(
            execute_direct_count(&db, batch).await.unwrap(),
            ExecutionValue::Count(2)
        );
        assert_eq!(
            crate::index_lifecycle::secondary::equality_read_metrics(),
            crate::index_lifecycle::secondary::SecondaryEqualityReadMetrics {
                point_reads: 3,
                multi_get_calls: 1,
                scans: 0,
                graph_reads: 0,
            }
        );
    }

    #[tokio::test]
    async fn bitmap_child_order_is_observed_and_never_reordered() {
        let db = test_support::open_db("count-bitmap-child-order").await;
        let mut context = ExecutionContext::new(&db, context::ParamBindings::default());
        let invalid_driver = exec::ExecNodeBitmapExpr::PointRead {
            index: node_equality_index("Other", "status"),
            key: catalog::ScopedPropertyKey::try_new("User", "status").unwrap(),
            value: indexed("active"),
        };
        let plan = exec::ExecCountPlan::NodeBitmap(exec::ExecNodeBitmapCountPlan {
            bitmap: exec::ExecNodeBitmapExpr::Union {
                driver: Box::new(invalid_driver),
                rest: ir::AtLeast::from_one(node_point("User", "status", "active")),
            },
            window: exec::ExecCountWindowPlan::identity(),
        });

        let error = context
            .execute_count(ExecutionValue::Stream(Vec::new()), &plan)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("node_eq:Other:status` disagrees with `node_eq:User:status"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn unique_count_performs_one_owner_read_and_one_authoritative_verification() {
        let db = test_support::open_db_with_config(
            test_support::in_memory_config("count-exact-unique-owner")
                .with_unique_equality_index("User", "email"),
        )
        .await;
        test_support::add_node_with_properties(
            &db,
            "User",
            vec![("email", PropertyValue::from("alice@example.com"))],
        )
        .await;
        let exact = exec::ExecNodeAccessPlan::exact_equality(
            catalog::NodeEqualityIndexMeta::new(test_support::name("node_eq:User:email"))
                .with_uniqueness(catalog::IndexUniqueness::Unique),
            catalog::ScopedPropertyKey::try_new("User", "email").unwrap(),
            ir::IndexValue::Literal(
                ir::SecondaryIndexLiteral::new(PropertyValue::from("alice@example.com")).unwrap(),
            ),
        );
        let exec::ExecNodeAccessPlan::Unique {
            lookup,
            verification,
        } = exact
        else {
            panic!("unique metadata must select the exact unique family")
        };
        crate::index_lifecycle::secondary::reset_equality_read_metrics();
        assert_eq!(
            execute_direct_count(
                &db,
                exec::ExecCountPlan::NodeUnique(exec::ExecNodeUniqueCountPlan {
                    lookup,
                    verification,
                    window: exec::ExecCountWindowPlan::identity(),
                }),
            )
            .await
            .unwrap(),
            ExecutionValue::Count(1)
        );
        assert_eq!(
            crate::index_lifecycle::secondary::equality_read_metrics(),
            crate::index_lifecycle::secondary::SecondaryEqualityReadMetrics {
                point_reads: 2,
                multi_get_calls: 0,
                scans: 0,
                graph_reads: 1,
            }
        );
    }

    #[tokio::test]
    async fn authoritative_null_count_applies_its_normalized_window_after_matches() {
        let db = test_support::open_db("count-authoritative-null-window").await;
        for properties in [
            vec![("status", PropertyValue::Null)],
            Vec::new(),
            vec![("status", PropertyValue::Null)],
            vec![("status", PropertyValue::from("active"))],
        ] {
            test_support::add_node_with_properties(&db, "User", properties).await;
        }
        test_support::add_node_with_properties(&db, "Other", vec![("status", PropertyValue::Null)])
            .await;
        let window = exec::ExecCountWindowPlan::identity()
            .then_skip(exec::ExecUsizeExpr::literal(1))
            .then_limit(exec::ExecUsizeExpr::literal(1));

        assert_eq!(
            execute_direct_count(
                &db,
                exec::ExecCountPlan::NodeAuthoritativeScan(exec::ExecNodeScanCountPlan {
                    predicate: exec::ExecNodeAuthoritativeScanPredicate::NullEquality {
                        key: catalog::ScopedPropertyKey::try_new("User", "status").unwrap(),
                    },
                    window,
                }),
            )
            .await
            .unwrap(),
            ExecutionValue::Count(1)
        );
    }
}
