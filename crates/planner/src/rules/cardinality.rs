//! Logical cardinality implementation and physical program construction.

use helix_ast::{expr::Expr, query, value};

use crate::{catalog, context, cost, exec, ir, logical, optimizer, physical, properties};

use super::{KnownRuleId, RuleId, RuleKind, RuleMetadata, RuleRejection};

/// Implement logical cardinality as payload-complete physical alternatives.
pub struct StreamCardinalityImplementationRule {
    metadata: RuleMetadata,
}

impl Default for StreamCardinalityImplementationRule {
    fn default() -> Self {
        Self {
            metadata: RuleMetadata::new(
                RuleId::known(KnownRuleId::SeedStreamCardinality),
                RuleKind::Implementation,
            ),
        }
    }
}

impl optimizer::OptimizerRule for StreamCardinalityImplementationRule {
    fn metadata(&self) -> &RuleMetadata {
        &self.metadata
    }

    fn apply(&self, input: optimizer::RuleInput<'_>) -> optimizer::RuleResult {
        let logical::LogicalExpr::StreamCardinality(cardinality) = input.expr else {
            return optimizer::RuleResult::NotApplicable;
        };
        let plans = match count_plans(cardinality.input(), &input) {
            Ok(plans) => plans,
            Err(rejection) => return optimizer::RuleResult::Rejected(rejection),
        };
        let input_delivered = super::root_stream_delivered_properties(
            cardinality.input(),
            input.storage,
            input.stats,
        );
        let delivered = super::cardinality_output_delivered(input_delivered);
        let alternatives = plans
            .into_iter()
            .map(|plan| {
                let cost = count_cost(&plan, input.stats, input.storage);
                physical::PhysicalAlternative::new(
                    physical::PhysicalExpr::Cardinality(Box::new(
                        physical::PhysicalCountPlan::new(plan),
                    )),
                    delivered.clone(),
                    cost,
                )
            })
            .collect::<Vec<_>>();
        let alternatives = ir::AtLeast::<_, 1>::try_from_vec(alternatives)
            .expect("cardinality implementation always produces an alternative");
        optimizer::RuleResult::Applied(optimizer::RuleEffect::Physical(alternatives))
    }
}

fn count_plans(
    input: &logical::RootStream,
    rule: &optimizer::RuleInput<'_>,
) -> Result<Vec<exec::ExecCountPlan>, RuleRejection> {
    match input {
        logical::RootStream::Access(access) => access_count_plans(access, rule),
        logical::RootStream::Pipeline(pipeline) if has_variable_write(pipeline.ops()) => {
            Ok(vec![exec::ExecCountPlan::InputRows {
                window: exec::ExecCountWindowPlan::identity(),
            }])
        }
        logical::RootStream::Pipeline(pipeline) => Ok(vec![root_pipeline_count(pipeline, rule)?]),
        logical::RootStream::Project(_)
        | logical::RootStream::Cardinality(_)
        | logical::RootStream::Aggregate(_) => Ok(vec![exec::ExecCountPlan::InputScalars {
            window: exec::ExecCountWindowPlan::identity(),
        }]),
        logical::RootStream::VariableSource(source) => {
            Ok(vec![exec::ExecCountPlan::RuntimeInput {
                input: exec::ExecRuntimeInputPlan::Variable(source.variable().clone()),
                window: exec::ExecCountWindowPlan::identity(),
            }])
        }
        logical::RootStream::Mutation(_)
        | logical::RootStream::Branch(_)
        | logical::RootStream::Repeat(_)
        | logical::RootStream::Reserved(_)
        | logical::RootStream::VariableWrite(_) => Ok(vec![exec::ExecCountPlan::InputRows {
            window: exec::ExecCountWindowPlan::identity(),
        }]),
    }
}

fn access_count_plans(
    access: &logical::AccessStream,
    rule: &optimizer::RuleInput<'_>,
) -> Result<Vec<exec::ExecCountPlan>, RuleRejection> {
    match access {
        logical::AccessStream::Path(path) => {
            direct_access_plans(path, exec::ExecCountWindowPlan::identity(), rule)
        }
        logical::AccessStream::Order(order) => {
            direct_access_plans(order.access(), exec::ExecCountWindowPlan::identity(), rule)
        }
        logical::AccessStream::Distinct(distinct) => Ok(vec![exec::ExecCountPlan::Stream(
            exec::ExecCountStreamPlan {
                cursor: exec::ExecCountCursorPlan::Distinct {
                    input: Box::new(access_cursor(distinct.access(), rule)?),
                    plan: exec::ExecCountDistinctPlan::HashRows,
                },
                window: exec::ExecCountWindowPlan::identity(),
            },
        )]),
        logical::AccessStream::Window(window) => {
            direct_access_plans(window.access(), static_window(window.window()), rule)
        }
        logical::AccessStream::Filter(filter) => Ok(vec![exec::ExecCountPlan::Stream(
            exec::ExecCountStreamPlan {
                cursor: exec::ExecCountCursorPlan::Filter {
                    input: Box::new(access_cursor(filter.access(), rule)?),
                    predicate: filter.predicate().clone(),
                },
                window: exec::ExecCountWindowPlan::identity(),
            },
        )]),
        logical::AccessStream::Pipeline(pipeline) if has_variable_write(pipeline.ops()) => {
            Ok(vec![exec::ExecCountPlan::InputRows {
                window: exec::ExecCountWindowPlan::identity(),
            }])
        }
        logical::AccessStream::Pipeline(pipeline) => access_pipeline_count(pipeline, rule),
    }
}

fn access_pipeline_count(
    pipeline: &logical::AccessPipeline,
    rule: &optimizer::RuleInput<'_>,
) -> Result<Vec<exec::ExecCountPlan>, RuleRejection> {
    let (prefix, window) = trailing_count_window(pipeline.ops())?;
    if prefix.is_empty() {
        return direct_access_plans(pipeline.access(), window, rule);
    }
    let cursor = fold_cursor(access_cursor(pipeline.access(), rule)?, prefix)?;
    Ok(vec![exec::ExecCountPlan::Stream(
        exec::ExecCountStreamPlan { cursor, window },
    )])
}

fn root_pipeline_count(
    pipeline: &logical::RootPipeline,
    rule: &optimizer::RuleInput<'_>,
) -> Result<exec::ExecCountPlan, RuleRejection> {
    let (prefix, window) = trailing_count_window(pipeline.ops())?;
    let cursor = match pipeline.input() {
        logical::RootStream::Access(access) => access_stream_cursor(access, rule)?,
        logical::RootStream::VariableSource(source) => exec::ExecCountCursorPlan::RuntimeInput(
            exec::ExecRuntimeInputPlan::Variable(source.variable().clone()),
        ),
        logical::RootStream::Mutation(_)
        | logical::RootStream::Branch(_)
        | logical::RootStream::Repeat(_)
        | logical::RootStream::Pipeline(_)
        | logical::RootStream::Reserved(_)
        | logical::RootStream::Project(_)
        | logical::RootStream::Cardinality(_)
        | logical::RootStream::Aggregate(_)
        | logical::RootStream::VariableWrite(_) => exec::ExecCountCursorPlan::InputRows,
    };
    let cursor = fold_cursor(cursor, prefix)?;
    Ok(exec::ExecCountPlan::Stream(exec::ExecCountStreamPlan {
        cursor,
        window,
    }))
}

fn has_variable_write(ops: &[logical::StreamPipelineOp]) -> bool {
    ops.iter()
        .any(|op| matches!(op, logical::StreamPipelineOp::VariableWrite { .. }))
}

fn static_window(window: logical::AccessWindowRange) -> exec::ExecCountWindowPlan {
    let window_plan = exec::ExecCountWindowPlan::identity()
        .then_skip(exec::ExecUsizeExpr::literal(window.start()));
    match window.end() {
        Some(end) => window_plan.then_limit(exec::ExecUsizeExpr::literal(
            end.saturating_sub(window.start()),
        )),
        None => window_plan,
    }
}

fn trailing_count_window(
    ops: &[logical::StreamPipelineOp],
) -> Result<(&[logical::StreamPipelineOp], exec::ExecCountWindowPlan), RuleRejection> {
    let suffix_start = ops
        .iter()
        .rposition(|op| {
            !matches!(
                op,
                logical::StreamPipelineOp::Window { .. }
                    | logical::StreamPipelineOp::Limit { .. }
                    | logical::StreamPipelineOp::Skip { .. }
                    | logical::StreamPipelineOp::Range { .. }
                    | logical::StreamPipelineOp::Order { .. }
            )
        })
        .map_or(0, |position| position.saturating_add(1));
    let window = ops[suffix_start..]
        .iter()
        .try_fold(exec::ExecCountWindowPlan::identity(), |window, op| {
            append_window(window, op)
        })?;
    Ok((&ops[..suffix_start], window))
}

fn append_window(
    window: exec::ExecCountWindowPlan,
    op: &logical::StreamPipelineOp,
) -> Result<exec::ExecCountWindowPlan, RuleRejection> {
    match op {
        logical::StreamPipelineOp::Window {
            window: static_range,
        } => {
            let window = window.then_skip(exec::ExecUsizeExpr::literal(static_range.start()));
            Ok(match static_range.end() {
                Some(end) => window.then_limit(exec::ExecUsizeExpr::literal(
                    end.saturating_sub(static_range.start()),
                )),
                None => window,
            })
        }
        logical::StreamPipelineOp::Limit { count } => Ok(window.then_limit(bound_expr(count)?)),
        logical::StreamPipelineOp::Skip { count } => Ok(window.then_skip(bound_expr(count)?)),
        logical::StreamPipelineOp::Range { range } => {
            let (start, end) = range_exprs(range)?;
            Ok(window.then_range(start, end))
        }
        logical::StreamPipelineOp::Order { .. } => Ok(window),
        logical::StreamPipelineOp::Filter { .. }
        | logical::StreamPipelineOp::Expand { .. }
        | logical::StreamPipelineOp::VectorSearch { .. }
        | logical::StreamPipelineOp::TextSearch { .. }
        | logical::StreamPipelineOp::Variable { .. }
        | logical::StreamPipelineOp::VariableWrite { .. }
        | logical::StreamPipelineOp::Distinct => Err(rejection("non_window_suffix_operator")),
    }
}

fn fold_cursor(
    cursor: exec::ExecCountCursorPlan,
    ops: &[logical::StreamPipelineOp],
) -> Result<exec::ExecCountCursorPlan, RuleRejection> {
    let mut cursor = cursor;
    let mut positioned_window = exec::ExecCountWindowPlan::identity();
    let mut has_positioned_window = false;
    for op in ops {
        if matches!(
            op,
            logical::StreamPipelineOp::Window { .. }
                | logical::StreamPipelineOp::Limit { .. }
                | logical::StreamPipelineOp::Skip { .. }
                | logical::StreamPipelineOp::Range { .. }
        ) {
            positioned_window = append_window(positioned_window, op)?;
            has_positioned_window = true;
            continue;
        }
        if has_positioned_window {
            cursor = exec::ExecCountCursorPlan::Window {
                input: Box::new(cursor),
                window: positioned_window,
            };
            positioned_window = exec::ExecCountWindowPlan::identity();
            has_positioned_window = false;
        }
        cursor = match op {
            logical::StreamPipelineOp::Filter { predicate } => exec::ExecCountCursorPlan::Filter {
                input: Box::new(cursor),
                predicate: predicate.clone(),
            },
            logical::StreamPipelineOp::Order { ordering } => exec::ExecCountCursorPlan::Order {
                input: Box::new(cursor),
                plan: ir::OrderPlan::ExplicitSort(ordering.clone()),
            },
            logical::StreamPipelineOp::Expand { plan } => exec::ExecCountCursorPlan::Expand {
                input: Box::new(cursor),
                plan: plan.clone(),
            },
            logical::StreamPipelineOp::VectorSearch { plan } => {
                exec::ExecCountCursorPlan::VectorSearch {
                    input: Box::new(cursor),
                    plan: plan.clone(),
                }
            }
            logical::StreamPipelineOp::TextSearch { plan } => {
                exec::ExecCountCursorPlan::TextSearch {
                    input: Box::new(cursor),
                    plan: plan.clone(),
                }
            }
            logical::StreamPipelineOp::Variable { op } => exec::ExecCountCursorPlan::Variable {
                input: Box::new(cursor),
                op: op.clone(),
            },
            logical::StreamPipelineOp::Distinct => exec::ExecCountCursorPlan::Distinct {
                input: Box::new(cursor),
                plan: exec::ExecCountDistinctPlan::HashRows,
            },
            logical::StreamPipelineOp::VariableWrite { .. } => {
                return Err(rejection("count_cursor_crossed_variable_write_barrier"));
            }
            logical::StreamPipelineOp::Window { .. }
            | logical::StreamPipelineOp::Limit { .. }
            | logical::StreamPipelineOp::Skip { .. }
            | logical::StreamPipelineOp::Range { .. } => {
                unreachable!("positioned windows are handled before cursor operators")
            }
        };
    }
    if has_positioned_window {
        cursor = exec::ExecCountCursorPlan::Window {
            input: Box::new(cursor),
            window: positioned_window,
        };
    }
    Ok(cursor)
}

fn bound_expr(bound: &ir::StreamBoundPlan) -> Result<exec::ExecUsizeExpr, RuleRejection> {
    match bound {
        ir::StreamBoundPlan::Literal(value) => Ok(exec::ExecUsizeExpr::literal(*value)),
        ir::StreamBoundPlan::Expr(expr) => match expr.expr() {
            Expr::Param(param) => ir::NonEmptyString::new(param.clone())
                .map(exec::ExecUsizeExpr::Param)
                .ok_or_else(|| rejection("empty_count_window_parameter")),
            Expr::Property(_)
            | Expr::Id
            | Expr::Timestamp
            | Expr::DateTimeNow
            | Expr::Constant(_)
            | Expr::Add { .. }
            | Expr::Sub { .. }
            | Expr::Mul { .. }
            | Expr::Div { .. }
            | Expr::Mod { .. }
            | Expr::Neg { .. }
            | Expr::Case { .. } => Err(rejection("unsupported_count_window_expression")),
        },
    }
}

fn range_exprs(
    range: &ir::StreamRangePlan,
) -> Result<(exec::ExecUsizeExpr, exec::ExecUsizeExpr), RuleRejection> {
    match range {
        ir::StreamRangePlan::Literal(range) => Ok((
            exec::ExecUsizeExpr::literal(range.start()),
            exec::ExecUsizeExpr::literal(range.end()),
        )),
        ir::StreamRangePlan::Dynamic(range) => {
            Ok((bound_expr(range.start())?, bound_expr(range.end())?))
        }
    }
}

fn direct_access_plans(
    access: &logical::AccessPath,
    window: exec::ExecCountWindowPlan,
    rule: &optimizer::RuleInput<'_>,
) -> Result<Vec<exec::ExecCountPlan>, RuleRejection> {
    match access {
        logical::AccessPath::Node(path) => node_count_plans(path.source().as_ref(), window, rule),
        logical::AccessPath::Edge(path) => edge_count_plans(path.source().as_ref(), window, rule),
    }
}

fn node_count_plans(
    source: &ir::NodeAccessPlan,
    window: exec::ExecCountWindowPlan,
    rule: &optimizer::RuleInput<'_>,
) -> Result<Vec<exec::ExecCountPlan>, RuleRejection> {
    let plan = match source {
        ir::NodeAccessPlan::Empty => exec::ExecCountPlan::Constant(0),
        ir::NodeAccessPlan::PointIds { ids } => exec::ExecCountPlan::NodePointReads {
            ids: ids.clone(),
            window,
        },
        ir::NodeAccessPlan::FromParam { param } => exec::ExecCountPlan::NodeRuntimeInput {
            input: exec::ExecRuntimeInputPlan::Param(param.clone()),
            window,
        },
        ir::NodeAccessPlan::FromVar { variable } => exec::ExecCountPlan::NodeRuntimeInput {
            input: exec::ExecRuntimeInputPlan::Variable(variable.clone()),
            window,
        },
        ir::NodeAccessPlan::AllScan => exec::ExecCountPlan::NodeFullScan { window },
        ir::NodeAccessPlan::LabelScan { label } => exec::ExecCountPlan::NodeLabelBitmap {
            label: label.clone(),
            window,
        },
        ir::NodeAccessPlan::EqualityIndex { index, key, value } => {
            return Ok(vec![node_equality_count(index, key, value, window, rule)?]);
        }
        ir::NodeAccessPlan::RangeIndex { index, key, range } => {
            exec::ExecCountPlan::NodeRange(exec::ExecNodeRangeCountPlan {
                driver: exec::ExecNodeVerifiedRangeScanPlan {
                    index: index.clone(),
                    key: key.clone(),
                    range: range.clone(),
                },
                membership: exec::ExecNodeRangeMembershipPlan::All,
                window,
            })
        }
        ir::NodeAccessPlan::VectorSearch {
            key,
            index,
            query_vector,
            k,
        } => exec::ExecCountPlan::NodeVectorSearch(exec::ExecNodeVectorSearchCountPlan {
            key: key.clone(),
            index: index.clone(),
            query_vector: query_vector.clone(),
            k: k.clone(),
            window,
        }),
        ir::NodeAccessPlan::TextSearch {
            key,
            index,
            query_text,
            k,
        } => exec::ExecCountPlan::NodeTextSearch(exec::ExecNodeTextSearchCountPlan {
            key: key.clone(),
            index: index.clone(),
            query_text: query_text.clone(),
            k: k.clone(),
            window,
        }),
        ir::NodeAccessPlan::Intersect(children) => {
            return node_intersection_count_plans(children, window, rule);
        }
        ir::NodeAccessPlan::Union(children) => {
            if let Some(bitmap) = node_bitmap_set(children, true, rule)? {
                return Ok(vec![exec::ExecCountPlan::NodeBitmap(
                    exec::ExecNodeBitmapCountPlan { bitmap, window },
                )]);
            }
            exec::ExecCountPlan::Stream(exec::ExecCountStreamPlan {
                cursor: node_set_cursor(children, true, rule)?,
                window,
            })
        }
        ir::NodeAccessPlan::ScanThenFilter { source, residual } => {
            exec::ExecCountPlan::Stream(exec::ExecCountStreamPlan {
                cursor: exec::ExecCountCursorPlan::Filter {
                    input: Box::new(node_cursor(source.as_ref(), rule)?),
                    predicate: residual.clone(),
                },
                window,
            })
        }
    };
    Ok(vec![plan])
}

fn edge_count_plans(
    source: &ir::EdgeAccessPlan,
    window: exec::ExecCountWindowPlan,
    rule: &optimizer::RuleInput<'_>,
) -> Result<Vec<exec::ExecCountPlan>, RuleRejection> {
    let plan = match source {
        ir::EdgeAccessPlan::Empty => exec::ExecCountPlan::Constant(0),
        ir::EdgeAccessPlan::PointIds { ids } => exec::ExecCountPlan::EdgePointReads {
            ids: ids.clone(),
            window,
        },
        ir::EdgeAccessPlan::FromParam { param } => exec::ExecCountPlan::EdgeRuntimeInput {
            input: exec::ExecRuntimeInputPlan::Param(param.clone()),
            window,
        },
        ir::EdgeAccessPlan::FromVar { variable } => exec::ExecCountPlan::EdgeRuntimeInput {
            input: exec::ExecRuntimeInputPlan::Variable(variable.clone()),
            window,
        },
        ir::EdgeAccessPlan::AllScan => exec::ExecCountPlan::EdgeFullScan { window },
        ir::EdgeAccessPlan::LabelScan { label } => exec::ExecCountPlan::EdgeLabelBitmap {
            label: label.clone(),
            window,
        },
        ir::EdgeAccessPlan::EqualityIndex { index, key, value } => {
            return Ok(vec![edge_equality_count(index, key, value, window, rule)?]);
        }
        ir::EdgeAccessPlan::RangeIndex { index, key, range } => {
            exec::ExecCountPlan::EdgeRange(exec::ExecEdgeRangeCountPlan {
                driver: exec::ExecEdgeVerifiedRangeScanPlan {
                    index: index.clone(),
                    key: key.clone(),
                    range: range.clone(),
                },
                membership: exec::ExecEdgeRangeMembershipPlan::All,
                window,
            })
        }
        ir::EdgeAccessPlan::VectorSearch {
            key,
            index,
            query_vector,
            k,
        } => exec::ExecCountPlan::EdgeVectorSearch(exec::ExecEdgeVectorSearchCountPlan {
            key: key.clone(),
            index: index.clone(),
            query_vector: query_vector.clone(),
            k: k.clone(),
            window,
        }),
        ir::EdgeAccessPlan::TextSearch {
            key,
            index,
            query_text,
            k,
        } => exec::ExecCountPlan::EdgeTextSearch(exec::ExecEdgeTextSearchCountPlan {
            key: key.clone(),
            index: index.clone(),
            query_text: query_text.clone(),
            k: k.clone(),
            window,
        }),
        ir::EdgeAccessPlan::Intersect(children) => {
            return edge_intersection_count_plans(children, window, rule);
        }
        ir::EdgeAccessPlan::Union(children) => {
            if let Some(bitmap) = edge_bitmap_set(children, true, rule)? {
                return Ok(vec![exec::ExecCountPlan::EdgeBitmap(
                    exec::ExecEdgeBitmapCountPlan { bitmap, window },
                )]);
            }
            exec::ExecCountPlan::Stream(exec::ExecCountStreamPlan {
                cursor: edge_set_cursor(children, true, rule)?,
                window,
            })
        }
        ir::EdgeAccessPlan::ScanThenFilter { source, residual } => {
            exec::ExecCountPlan::Stream(exec::ExecCountStreamPlan {
                cursor: exec::ExecCountCursorPlan::Filter {
                    input: Box::new(edge_cursor(source.as_ref(), rule)?),
                    predicate: residual.clone(),
                },
                window,
            })
        }
    };
    Ok(vec![plan])
}

enum EqualityValue {
    Indexed(exec::ExecIndexedEqualityValue),
    AuthoritativeNull,
    NonReflexive,
    Dynamic(ir::NonEmptyString),
}

fn classify_equality(
    value: &ir::IndexValue,
    rule: &optimizer::RuleInput<'_>,
) -> Result<EqualityValue, RuleRejection> {
    let (params, late_bound_params) = rule
        .cardinality_bindings()
        .expect("cardinality helpers are only called by the cardinality rule");
    let literal = match value {
        ir::IndexValue::Literal(literal) => literal.clone(),
        ir::IndexValue::Param(param) if late_bound_params.contains(param) => {
            return Ok(EqualityValue::Dynamic(param.clone()));
        }
        ir::IndexValue::Param(param) => {
            let value = if let Some(value) = params.values.get(param) {
                value.clone()
            } else if let Some(value) = params.query_values.get(param) {
                query_property_value(value)?
            } else {
                return Err(rejection("missing_planning_equality_parameter"));
            };
            ir::SecondaryIndexLiteral::new(value)
                .map_err(|_| rejection("unsupported_planning_equality_parameter"))?
        }
    };
    match literal.semantics() {
        ir::EqualityIndexValueSemantics::Indexed => {
            exec::ExecIndexedEqualityValue::try_from(literal)
                .map(EqualityValue::Indexed)
                .map_err(|_| rejection("invalid_indexed_equality_value"))
        }
        ir::EqualityIndexValueSemantics::AuthoritativeNull => Ok(EqualityValue::AuthoritativeNull),
        ir::EqualityIndexValueSemantics::NonReflexive => Ok(EqualityValue::NonReflexive),
        ir::EqualityIndexValueSemantics::RuntimeDependent => {
            Err(rejection("literal_equality_was_runtime_dependent"))
        }
    }
}

fn query_property_value(value: &query::QueryValue) -> Result<value::PropertyValue, RuleRejection> {
    match value {
        query::QueryValue::Null => Ok(value::PropertyValue::Null),
        query::QueryValue::Bool(value) => Ok(value::PropertyValue::Bool(*value)),
        query::QueryValue::I64(value) => Ok(value::PropertyValue::I64(*value)),
        query::QueryValue::F64(value) => Ok(value::PropertyValue::F64(*value)),
        query::QueryValue::F32(value) => Ok(value::PropertyValue::F32(*value)),
        query::QueryValue::String(value) => Ok(value::PropertyValue::String(value.clone())),
        query::QueryValue::Array(_) | query::QueryValue::Object(_) => {
            Err(rejection("unsupported_planning_equality_parameter"))
        }
    }
}

fn node_equality_count(
    index: &catalog::NodeEqualityIndexMeta,
    key: &catalog::ScopedPropertyKey,
    value: &ir::IndexValue,
    window: exec::ExecCountWindowPlan,
    rule: &optimizer::RuleInput<'_>,
) -> Result<exec::ExecCountPlan, RuleRejection> {
    Ok(match classify_equality(value, rule)? {
        EqualityValue::Indexed(value) => match index.uniqueness {
            catalog::IndexUniqueness::NonUnique => {
                exec::ExecCountPlan::NodeBitmap(exec::ExecNodeBitmapCountPlan {
                    bitmap: exec::ExecNodeBitmapExpr::PointRead {
                        index: index
                            .clone()
                            .try_into()
                            .map_err(|_| rejection("non_unique_index_validation_failed"))?,
                        key: key.clone(),
                        value,
                    },
                    window,
                })
            }
            catalog::IndexUniqueness::Unique => {
                let index = exec::ExecNodeUniqueEqualityIndex::try_from(index.clone())
                    .map_err(|_| rejection("unique_index_validation_failed"))?;
                exec::ExecCountPlan::NodeUnique(exec::ExecNodeUniqueCountPlan {
                    lookup: exec::ExecNodeUniqueOwnerReadPlan {
                        index,
                        key: key.clone(),
                        value: value.clone(),
                    },
                    verification: exec::ExecNodeAuthoritativeVerificationPlan {
                        key: key.clone(),
                        value,
                    },
                    window,
                })
            }
        },
        EqualityValue::AuthoritativeNull => {
            exec::ExecCountPlan::NodeAuthoritativeScan(exec::ExecNodeScanCountPlan {
                predicate: exec::ExecNodeAuthoritativeScanPredicate::NullEquality {
                    key: key.clone(),
                },
                window,
            })
        }
        EqualityValue::NonReflexive => exec::ExecCountPlan::Constant(0),
        EqualityValue::Dynamic(param) => {
            exec::ExecCountPlan::NodeDynamicEquality(exec::ExecNodeDynamicEqualityCountPlan {
                index: index.clone(),
                key: key.clone(),
                param,
                window,
            })
        }
    })
}

fn edge_equality_count(
    index: &catalog::EdgeEqualityIndexMeta,
    key: &catalog::ScopedPropertyKey,
    value: &ir::IndexValue,
    window: exec::ExecCountWindowPlan,
    rule: &optimizer::RuleInput<'_>,
) -> Result<exec::ExecCountPlan, RuleRejection> {
    Ok(match classify_equality(value, rule)? {
        EqualityValue::Indexed(value) => {
            exec::ExecCountPlan::EdgeBitmap(exec::ExecEdgeBitmapCountPlan {
                bitmap: exec::ExecEdgeBitmapExpr::PointRead {
                    index: exec::ExecEdgeNonUniqueEqualityIndex::new(index.clone()),
                    key: key.clone(),
                    value,
                },
                window,
            })
        }
        EqualityValue::AuthoritativeNull => {
            exec::ExecCountPlan::EdgeAuthoritativeScan(exec::ExecEdgeScanCountPlan {
                predicate: exec::ExecEdgeAuthoritativeScanPredicate::NullEquality {
                    key: key.clone(),
                },
                window,
            })
        }
        EqualityValue::NonReflexive => exec::ExecCountPlan::Constant(0),
        EqualityValue::Dynamic(param) => {
            exec::ExecCountPlan::EdgeDynamicEquality(exec::ExecEdgeDynamicEqualityCountPlan {
                index: index.clone(),
                key: key.clone(),
                param,
                window,
            })
        }
    })
}

fn node_cursor(
    source: &ir::NodeAccessPlan,
    rule: &optimizer::RuleInput<'_>,
) -> Result<exec::ExecCountCursorPlan, RuleRejection> {
    let identity = exec::ExecCountWindowPlan::identity();
    let plan = node_count_plans(source, identity, rule)?
        .into_iter()
        .next()
        .expect("node source produces a count plan");
    count_plan_cursor(plan)
}

fn edge_cursor(
    source: &ir::EdgeAccessPlan,
    rule: &optimizer::RuleInput<'_>,
) -> Result<exec::ExecCountCursorPlan, RuleRejection> {
    let identity = exec::ExecCountWindowPlan::identity();
    let plan = edge_count_plans(source, identity, rule)?
        .into_iter()
        .next()
        .expect("edge source produces a count plan");
    count_plan_cursor(plan)
}

fn access_cursor(
    access: &logical::AccessPath,
    rule: &optimizer::RuleInput<'_>,
) -> Result<exec::ExecCountCursorPlan, RuleRejection> {
    match access {
        logical::AccessPath::Node(path) => node_cursor(path.source().as_ref(), rule),
        logical::AccessPath::Edge(path) => edge_cursor(path.source().as_ref(), rule),
    }
}

fn access_stream_cursor(
    access: &logical::AccessStream,
    rule: &optimizer::RuleInput<'_>,
) -> Result<exec::ExecCountCursorPlan, RuleRejection> {
    match access {
        logical::AccessStream::Path(path) => access_cursor(path, rule),
        logical::AccessStream::Order(order) => Ok(exec::ExecCountCursorPlan::Order {
            input: Box::new(access_cursor(order.access(), rule)?),
            plan: ir::OrderPlan::ExplicitSort(order.ordering().clone()),
        }),
        logical::AccessStream::Distinct(distinct) => Ok(exec::ExecCountCursorPlan::Distinct {
            input: Box::new(access_cursor(distinct.access(), rule)?),
            plan: exec::ExecCountDistinctPlan::HashRows,
        }),
        logical::AccessStream::Window(window) => Ok(exec::ExecCountCursorPlan::Window {
            input: Box::new(access_cursor(window.access(), rule)?),
            window: static_window(window.window()),
        }),
        logical::AccessStream::Filter(filter) => Ok(exec::ExecCountCursorPlan::Filter {
            input: Box::new(access_cursor(filter.access(), rule)?),
            predicate: filter.predicate().clone(),
        }),
        logical::AccessStream::Pipeline(pipeline) => {
            if has_variable_write(pipeline.ops()) {
                return Err(rejection("count_cursor_crossed_variable_write_barrier"));
            }
            fold_cursor(access_cursor(pipeline.access(), rule)?, pipeline.ops())
        }
    }
}

fn count_plan_cursor(
    plan: exec::ExecCountPlan,
) -> Result<exec::ExecCountCursorPlan, RuleRejection> {
    match plan {
        exec::ExecCountPlan::Constant(0) => Ok(exec::ExecCountCursorPlan::EmptyRows),
        exec::ExecCountPlan::Constant(_) => Err(rejection("nonzero_constant_is_not_a_row_cursor")),
        exec::ExecCountPlan::NodeBitmap(plan) => {
            Ok(exec::ExecCountCursorPlan::NodeBitmap(plan.bitmap))
        }
        exec::ExecCountPlan::EdgeBitmap(plan) => {
            Ok(exec::ExecCountCursorPlan::EdgeBitmap(plan.bitmap))
        }
        exec::ExecCountPlan::NodeUnique(plan) => Ok(exec::ExecCountCursorPlan::NodeUnique {
            lookup: plan.lookup,
            verification: plan.verification,
        }),
        exec::ExecCountPlan::NodeRange(plan) => {
            Ok(exec::ExecCountCursorPlan::NodeRange(plan.driver))
        }
        exec::ExecCountPlan::EdgeRange(plan) => {
            Ok(exec::ExecCountCursorPlan::EdgeRange(plan.driver))
        }
        exec::ExecCountPlan::NodeAuthoritativeScan(plan) => Ok(
            exec::ExecCountCursorPlan::NodeAuthoritativeScan(plan.predicate),
        ),
        exec::ExecCountPlan::EdgeAuthoritativeScan(plan) => Ok(
            exec::ExecCountCursorPlan::EdgeAuthoritativeScan(plan.predicate),
        ),
        exec::ExecCountPlan::NodePointReads { ids, .. } => {
            Ok(exec::ExecCountCursorPlan::NodePointReads(ids))
        }
        exec::ExecCountPlan::EdgePointReads { ids, .. } => {
            Ok(exec::ExecCountCursorPlan::EdgePointReads(ids))
        }
        exec::ExecCountPlan::NodeRuntimeInput { input, .. } => {
            Ok(exec::ExecCountCursorPlan::NodeRuntimeInput(input))
        }
        exec::ExecCountPlan::EdgeRuntimeInput { input, .. } => {
            Ok(exec::ExecCountCursorPlan::EdgeRuntimeInput(input))
        }
        exec::ExecCountPlan::RuntimeInput { input, .. } => {
            Ok(exec::ExecCountCursorPlan::RuntimeInput(input))
        }
        exec::ExecCountPlan::NodeFullScan { .. } => Ok(exec::ExecCountCursorPlan::NodeFullScan),
        exec::ExecCountPlan::EdgeFullScan { .. } => Ok(exec::ExecCountCursorPlan::EdgeFullScan),
        exec::ExecCountPlan::NodeLabelBitmap { label, .. } => {
            Ok(exec::ExecCountCursorPlan::NodeLabelBitmap(label))
        }
        exec::ExecCountPlan::EdgeLabelBitmap { label, .. } => {
            Ok(exec::ExecCountCursorPlan::EdgeLabelBitmap(label))
        }
        exec::ExecCountPlan::NodeVectorSearch(plan) => {
            Ok(exec::ExecCountCursorPlan::NodeVectorSearch {
                key: plan.key,
                index: plan.index,
                query_vector: plan.query_vector,
                k: plan.k,
            })
        }
        exec::ExecCountPlan::EdgeVectorSearch(plan) => {
            Ok(exec::ExecCountCursorPlan::EdgeVectorSearch {
                key: plan.key,
                index: plan.index,
                query_vector: plan.query_vector,
                k: plan.k,
            })
        }
        exec::ExecCountPlan::NodeTextSearch(plan) => {
            Ok(exec::ExecCountCursorPlan::NodeTextSearch {
                key: plan.key,
                index: plan.index,
                query_text: plan.query_text,
                k: plan.k,
            })
        }
        exec::ExecCountPlan::EdgeTextSearch(plan) => {
            Ok(exec::ExecCountCursorPlan::EdgeTextSearch {
                key: plan.key,
                index: plan.index,
                query_text: plan.query_text,
                k: plan.k,
            })
        }
        exec::ExecCountPlan::NodeDynamicEquality(plan) => {
            Ok(exec::ExecCountCursorPlan::NodeDynamicEquality {
                index: plan.index,
                key: plan.key,
                param: plan.param,
            })
        }
        exec::ExecCountPlan::EdgeDynamicEquality(plan) => {
            Ok(exec::ExecCountCursorPlan::EdgeDynamicEquality {
                index: plan.index,
                key: plan.key,
                param: plan.param,
            })
        }
        exec::ExecCountPlan::Stream(plan) => Ok(plan.cursor),
        exec::ExecCountPlan::InputRows { .. } => Ok(exec::ExecCountCursorPlan::InputRows),
        exec::ExecCountPlan::InputScalars { .. } => {
            Err(rejection("scalar_count_input_is_not_a_row_cursor"))
        }
    }
}

fn node_bitmap_set(
    children: &ir::AtLeast<ir::NodeAccessSourcePlan, 2>,
    union: bool,
    rule: &optimizer::RuleInput<'_>,
) -> Result<Option<exec::ExecNodeBitmapExpr>, RuleRejection> {
    let mut bitmaps = Vec::with_capacity(children.len());
    for child in children {
        let Some(bitmap) = node_bitmap_expr(child.as_ref(), rule)? else {
            return Ok(None);
        };
        bitmaps.push(bitmap);
    }
    if union && let Some(batch) = node_bitmap_batch(&bitmaps) {
        return Ok(Some(batch));
    }
    let driver = bitmaps.remove(0);
    let rest = ir::AtLeast::<_, 1>::try_from_vec(bitmaps)
        .expect("set source has at least two bitmap children");
    Ok(Some(if union {
        exec::ExecNodeBitmapExpr::Union {
            driver: Box::new(driver),
            rest,
        }
    } else {
        exec::ExecNodeBitmapExpr::Intersect {
            driver: Box::new(driver),
            rest,
        }
    }))
}

fn edge_bitmap_set(
    children: &ir::AtLeast<ir::EdgeAccessSourcePlan, 2>,
    union: bool,
    rule: &optimizer::RuleInput<'_>,
) -> Result<Option<exec::ExecEdgeBitmapExpr>, RuleRejection> {
    let mut bitmaps = Vec::with_capacity(children.len());
    for child in children {
        let Some(bitmap) = edge_bitmap_expr(child.as_ref(), rule)? else {
            return Ok(None);
        };
        bitmaps.push(bitmap);
    }
    if union && let Some(batch) = edge_bitmap_batch(&bitmaps) {
        return Ok(Some(batch));
    }
    let driver = bitmaps.remove(0);
    let rest = ir::AtLeast::<_, 1>::try_from_vec(bitmaps)
        .expect("set source has at least two bitmap children");
    Ok(Some(if union {
        exec::ExecEdgeBitmapExpr::Union {
            driver: Box::new(driver),
            rest,
        }
    } else {
        exec::ExecEdgeBitmapExpr::Intersect {
            driver: Box::new(driver),
            rest,
        }
    }))
}

fn node_bitmap_expr(
    source: &ir::NodeAccessPlan,
    rule: &optimizer::RuleInput<'_>,
) -> Result<Option<exec::ExecNodeBitmapExpr>, RuleRejection> {
    match source {
        ir::NodeAccessPlan::EqualityIndex { index, key, value }
            if index.uniqueness == catalog::IndexUniqueness::NonUnique =>
        {
            Ok(match classify_equality(value, rule)? {
                EqualityValue::Indexed(value) => Some(exec::ExecNodeBitmapExpr::PointRead {
                    index: index
                        .clone()
                        .try_into()
                        .map_err(|_| rejection("non_unique_index_validation_failed"))?,
                    key: key.clone(),
                    value,
                }),
                EqualityValue::AuthoritativeNull
                | EqualityValue::NonReflexive
                | EqualityValue::Dynamic(_) => None,
            })
        }
        ir::NodeAccessPlan::Union(children) => node_bitmap_set(children, true, rule),
        ir::NodeAccessPlan::Intersect(children) => node_bitmap_set(children, false, rule),
        ir::NodeAccessPlan::Empty
        | ir::NodeAccessPlan::PointIds { .. }
        | ir::NodeAccessPlan::FromParam { .. }
        | ir::NodeAccessPlan::FromVar { .. }
        | ir::NodeAccessPlan::AllScan
        | ir::NodeAccessPlan::LabelScan { .. }
        | ir::NodeAccessPlan::EqualityIndex { .. }
        | ir::NodeAccessPlan::RangeIndex { .. }
        | ir::NodeAccessPlan::VectorSearch { .. }
        | ir::NodeAccessPlan::TextSearch { .. }
        | ir::NodeAccessPlan::ScanThenFilter { .. } => Ok(None),
    }
}

fn edge_bitmap_expr(
    source: &ir::EdgeAccessPlan,
    rule: &optimizer::RuleInput<'_>,
) -> Result<Option<exec::ExecEdgeBitmapExpr>, RuleRejection> {
    match source {
        ir::EdgeAccessPlan::EqualityIndex { index, key, value } => {
            Ok(match classify_equality(value, rule)? {
                EqualityValue::Indexed(value) => Some(exec::ExecEdgeBitmapExpr::PointRead {
                    index: exec::ExecEdgeNonUniqueEqualityIndex::new(index.clone()),
                    key: key.clone(),
                    value,
                }),
                EqualityValue::AuthoritativeNull
                | EqualityValue::NonReflexive
                | EqualityValue::Dynamic(_) => None,
            })
        }
        ir::EdgeAccessPlan::Union(children) => edge_bitmap_set(children, true, rule),
        ir::EdgeAccessPlan::Intersect(children) => edge_bitmap_set(children, false, rule),
        ir::EdgeAccessPlan::Empty
        | ir::EdgeAccessPlan::PointIds { .. }
        | ir::EdgeAccessPlan::FromParam { .. }
        | ir::EdgeAccessPlan::FromVar { .. }
        | ir::EdgeAccessPlan::AllScan
        | ir::EdgeAccessPlan::LabelScan { .. }
        | ir::EdgeAccessPlan::RangeIndex { .. }
        | ir::EdgeAccessPlan::VectorSearch { .. }
        | ir::EdgeAccessPlan::TextSearch { .. }
        | ir::EdgeAccessPlan::ScanThenFilter { .. } => Ok(None),
    }
}

fn node_bitmap_batch(bitmaps: &[exec::ExecNodeBitmapExpr]) -> Option<exec::ExecNodeBitmapExpr> {
    let exec::ExecNodeBitmapExpr::PointRead { index, key, value } = bitmaps.first()? else {
        return None;
    };
    let mut values = vec![value.clone()];
    for bitmap in &bitmaps[1..] {
        let exec::ExecNodeBitmapExpr::PointRead {
            index: next_index,
            key: next_key,
            value,
        } = bitmap
        else {
            return None;
        };
        if next_index != index || next_key != key {
            return None;
        }
        values.push(value.clone());
    }
    Some(exec::ExecNodeBitmapExpr::BatchedUnionRead {
        index: index.clone(),
        key: key.clone(),
        values: ir::AtLeast::<_, 2>::try_from_vec(values)
            .expect("bitmap batch comes from an at-least-two set"),
    })
}

fn edge_bitmap_batch(bitmaps: &[exec::ExecEdgeBitmapExpr]) -> Option<exec::ExecEdgeBitmapExpr> {
    let exec::ExecEdgeBitmapExpr::PointRead { index, key, value } = bitmaps.first()? else {
        return None;
    };
    let mut values = vec![value.clone()];
    for bitmap in &bitmaps[1..] {
        let exec::ExecEdgeBitmapExpr::PointRead {
            index: next_index,
            key: next_key,
            value,
        } = bitmap
        else {
            return None;
        };
        if next_index != index || next_key != key {
            return None;
        }
        values.push(value.clone());
    }
    Some(exec::ExecEdgeBitmapExpr::BatchedUnionRead {
        index: index.clone(),
        key: key.clone(),
        values: ir::AtLeast::<_, 2>::try_from_vec(values)
            .expect("bitmap batch comes from an at-least-two set"),
    })
}

fn node_set_cursor(
    children: &ir::AtLeast<ir::NodeAccessSourcePlan, 2>,
    union: bool,
    rule: &optimizer::RuleInput<'_>,
) -> Result<exec::ExecCountCursorPlan, RuleRejection> {
    let mut cursors = children
        .iter()
        .map(|child| node_cursor(child.as_ref(), rule))
        .collect::<Result<Vec<_>, _>>()?;
    let driver = cursors.remove(0);
    let rest = ir::AtLeast::<_, 1>::try_from_vec(cursors).expect("set has remaining children");
    Ok(if union {
        exec::ExecCountCursorPlan::Union {
            driver: Box::new(driver),
            rest,
        }
    } else {
        exec::ExecCountCursorPlan::Intersect {
            driver: Box::new(driver),
            rest,
        }
    })
}

fn edge_set_cursor(
    children: &ir::AtLeast<ir::EdgeAccessSourcePlan, 2>,
    union: bool,
    rule: &optimizer::RuleInput<'_>,
) -> Result<exec::ExecCountCursorPlan, RuleRejection> {
    let mut cursors = children
        .iter()
        .map(|child| edge_cursor(child.as_ref(), rule))
        .collect::<Result<Vec<_>, _>>()?;
    let driver = cursors.remove(0);
    let rest = ir::AtLeast::<_, 1>::try_from_vec(cursors).expect("set has remaining children");
    Ok(if union {
        exec::ExecCountCursorPlan::Union {
            driver: Box::new(driver),
            rest,
        }
    } else {
        exec::ExecCountCursorPlan::Intersect {
            driver: Box::new(driver),
            rest,
        }
    })
}

fn node_intersection_count_plans(
    children: &ir::AtLeast<ir::NodeAccessSourcePlan, 2>,
    window: exec::ExecCountWindowPlan,
    rule: &optimizer::RuleInput<'_>,
) -> Result<Vec<exec::ExecCountPlan>, RuleRejection> {
    if let Some(bitmap) = node_bitmap_set(children, false, rule)? {
        return Ok(vec![exec::ExecCountPlan::NodeBitmap(
            exec::ExecNodeBitmapCountPlan { bitmap, window },
        )]);
    }
    let mut alternatives = Vec::new();
    for (driver_index, child) in children.iter().enumerate() {
        let ir::NodeAccessPlan::RangeIndex { index, key, range } = child.as_ref() else {
            continue;
        };
        let filters = children
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != driver_index)
            .map(|(_, child)| node_bitmap_expr(child.as_ref(), rule))
            .collect::<Result<Option<Vec<_>>, _>>()?;
        let Some(filters) = filters else { continue };
        let Some(filters) = ir::AtLeast::<_, 1>::try_from_vec(filters) else {
            continue;
        };
        alternatives.push(exec::ExecCountPlan::NodeRange(
            exec::ExecNodeRangeCountPlan {
                driver: exec::ExecNodeVerifiedRangeScanPlan {
                    index: index.clone(),
                    key: key.clone(),
                    range: range.clone(),
                },
                membership: exec::ExecNodeRangeMembershipPlan::BitmapFilters(filters),
                window: window.clone(),
            },
        ));
    }
    if alternatives.is_empty() {
        alternatives.push(exec::ExecCountPlan::Stream(exec::ExecCountStreamPlan {
            cursor: node_set_cursor(children, false, rule)?,
            window,
        }));
    }
    Ok(alternatives)
}

fn edge_intersection_count_plans(
    children: &ir::AtLeast<ir::EdgeAccessSourcePlan, 2>,
    window: exec::ExecCountWindowPlan,
    rule: &optimizer::RuleInput<'_>,
) -> Result<Vec<exec::ExecCountPlan>, RuleRejection> {
    if let Some(bitmap) = edge_bitmap_set(children, false, rule)? {
        return Ok(vec![exec::ExecCountPlan::EdgeBitmap(
            exec::ExecEdgeBitmapCountPlan { bitmap, window },
        )]);
    }
    let mut alternatives = Vec::new();
    for (driver_index, child) in children.iter().enumerate() {
        let ir::EdgeAccessPlan::RangeIndex { index, key, range } = child.as_ref() else {
            continue;
        };
        let filters = children
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != driver_index)
            .map(|(_, child)| edge_bitmap_expr(child.as_ref(), rule))
            .collect::<Result<Option<Vec<_>>, _>>()?;
        let Some(filters) = filters else { continue };
        let Some(filters) = ir::AtLeast::<_, 1>::try_from_vec(filters) else {
            continue;
        };
        alternatives.push(exec::ExecCountPlan::EdgeRange(
            exec::ExecEdgeRangeCountPlan {
                driver: exec::ExecEdgeVerifiedRangeScanPlan {
                    index: index.clone(),
                    key: key.clone(),
                    range: range.clone(),
                },
                membership: exec::ExecEdgeRangeMembershipPlan::BitmapFilters(filters),
                window: window.clone(),
            },
        ));
    }
    if alternatives.is_empty() {
        alternatives.push(exec::ExecCountPlan::Stream(exec::ExecCountStreamPlan {
            cursor: edge_set_cursor(children, false, rule)?,
            window,
        }));
    }
    Ok(alternatives)
}

fn count_cost(
    plan: &exec::ExecCountPlan,
    stats: &context::StatsSnapshot,
    storage: &cost::StorageCostProfile,
) -> cost::CostVector {
    match plan {
        exec::ExecCountPlan::Constant(_) => cost::CostVector::ZERO,
        exec::ExecCountPlan::NodeBitmap(plan) => node_bitmap_cost(&plan.bitmap, stats, storage),
        exec::ExecCountPlan::EdgeBitmap(plan) => edge_bitmap_cost(&plan.bitmap, stats, storage),
        exec::ExecCountPlan::NodeUnique(plan) => storage.unique_equality_lookup(
            storage.unique_equality_rows(stats.node_eq_cardinality.get(&plan.lookup.key).copied()),
        ),
        exec::ExecCountPlan::NodeRange(plan) => storage.secondary_range_lookup(
            stats
                .node_range_cardinality
                .get(&plan.driver.key)
                .copied()
                .map_or(storage.default_range_index_rows, cost::EstimatedRows::rows),
        ),
        exec::ExecCountPlan::EdgeRange(plan) => storage.secondary_range_lookup(
            stats
                .edge_range_cardinality
                .get(&plan.driver.key)
                .copied()
                .map_or(storage.default_range_index_rows, cost::EstimatedRows::rows),
        ),
        exec::ExecCountPlan::NodeAuthoritativeScan(plan) => {
            storage.null_equality_scan(node_scan_rows(&plan.predicate, stats, storage))
        }
        exec::ExecCountPlan::EdgeAuthoritativeScan(plan) => {
            storage.null_equality_scan(edge_scan_rows(&plan.predicate, stats, storage))
        }
        exec::ExecCountPlan::NodePointReads { ids, .. }
        | exec::ExecCountPlan::EdgePointReads { ids, .. } => {
            storage.point_gets(properties::PositiveUsize::at_least_one(ids.as_ref().len()))
        }
        exec::ExecCountPlan::NodeRuntimeInput { .. }
        | exec::ExecCountPlan::EdgeRuntimeInput { .. }
        | exec::ExecCountPlan::RuntimeInput { .. }
        | exec::ExecCountPlan::InputRows { .. }
        | exec::ExecCountPlan::InputScalars { .. } => storage.source_inject(),
        exec::ExecCountPlan::NodeFullScan { .. } | exec::ExecCountPlan::EdgeFullScan { .. } => {
            storage.range_scan(storage.default_unknown_scan_rows)
        }
        exec::ExecCountPlan::NodeLabelBitmap { label, .. } => storage.range_scan(
            stats
                .node_label_cardinality
                .get(label)
                .copied()
                .map_or(storage.default_unknown_scan_rows, cost::EstimatedRows::rows),
        ),
        exec::ExecCountPlan::EdgeLabelBitmap { label, .. } => storage.range_scan(
            stats
                .edge_label_cardinality
                .get(label)
                .copied()
                .map_or(storage.default_unknown_scan_rows, cost::EstimatedRows::rows),
        ),
        exec::ExecCountPlan::NodeVectorSearch(_)
        | exec::ExecCountPlan::EdgeVectorSearch(_)
        | exec::ExecCountPlan::NodeTextSearch(_)
        | exec::ExecCountPlan::EdgeTextSearch(_) => {
            storage.range_scan(storage.default_unknown_scan_rows)
        }
        exec::ExecCountPlan::NodeDynamicEquality(_)
        | exec::ExecCountPlan::EdgeDynamicEquality(_) => storage
            .bitmap_equality_lookup(storage.default_equality_index_rows)
            .serial(storage.null_equality_scan(storage.default_unknown_scan_rows)),
        exec::ExecCountPlan::Stream(plan) => cursor_cost(&plan.cursor, stats, storage),
    }
}

fn node_bitmap_cost(
    bitmap: &exec::ExecNodeBitmapExpr,
    stats: &context::StatsSnapshot,
    storage: &cost::StorageCostProfile,
) -> cost::CostVector {
    match bitmap {
        exec::ExecNodeBitmapExpr::PointRead { key, .. } => {
            storage.bitmap_equality_lookup(stats.node_eq_cardinality.get(key).copied().map_or(
                storage.default_equality_index_rows,
                cost::EstimatedRows::rows,
            ))
        }
        exec::ExecNodeBitmapExpr::BatchedUnionRead { key, values, .. } => {
            let rows = stats.node_eq_cardinality.get(key).copied().map_or(
                storage.default_equality_index_rows,
                cost::EstimatedRows::rows,
            );
            storage.bitmap_equality_batch(
                properties::PositiveUsize::at_least_one(values.len()),
                cost::EstimatedRows::rows(rows.as_rows().saturating_mul(values.len() as u64)),
            )
        }
        exec::ExecNodeBitmapExpr::Union { driver, rest }
        | exec::ExecNodeBitmapExpr::Intersect { driver, rest } => {
            rest.iter()
                .fold(node_bitmap_cost(driver, stats, storage), |cost, child| {
                    cost.serial(node_bitmap_cost(child, stats, storage)).serial(
                        storage.secondary_set_operation(storage.default_equality_index_rows),
                    )
                })
        }
    }
}

fn edge_bitmap_cost(
    bitmap: &exec::ExecEdgeBitmapExpr,
    stats: &context::StatsSnapshot,
    storage: &cost::StorageCostProfile,
) -> cost::CostVector {
    match bitmap {
        exec::ExecEdgeBitmapExpr::PointRead { key, .. } => {
            storage.bitmap_equality_lookup(stats.edge_eq_cardinality.get(key).copied().map_or(
                storage.default_equality_index_rows,
                cost::EstimatedRows::rows,
            ))
        }
        exec::ExecEdgeBitmapExpr::BatchedUnionRead { key, values, .. } => {
            let rows = stats.edge_eq_cardinality.get(key).copied().map_or(
                storage.default_equality_index_rows,
                cost::EstimatedRows::rows,
            );
            storage.bitmap_equality_batch(
                properties::PositiveUsize::at_least_one(values.len()),
                cost::EstimatedRows::rows(rows.as_rows().saturating_mul(values.len() as u64)),
            )
        }
        exec::ExecEdgeBitmapExpr::Union { driver, rest }
        | exec::ExecEdgeBitmapExpr::Intersect { driver, rest } => {
            rest.iter()
                .fold(edge_bitmap_cost(driver, stats, storage), |cost, child| {
                    cost.serial(edge_bitmap_cost(child, stats, storage)).serial(
                        storage.secondary_set_operation(storage.default_equality_index_rows),
                    )
                })
        }
    }
}

fn cursor_cost(
    cursor: &exec::ExecCountCursorPlan,
    stats: &context::StatsSnapshot,
    storage: &cost::StorageCostProfile,
) -> cost::CostVector {
    match cursor {
        exec::ExecCountCursorPlan::EmptyRows => cost::CostVector::ZERO,
        exec::ExecCountCursorPlan::InputRows
        | exec::ExecCountCursorPlan::NodeRuntimeInput(_)
        | exec::ExecCountCursorPlan::EdgeRuntimeInput(_)
        | exec::ExecCountCursorPlan::RuntimeInput(_) => storage.source_inject(),
        exec::ExecCountCursorPlan::NodeBitmap(bitmap) => node_bitmap_cost(bitmap, stats, storage),
        exec::ExecCountCursorPlan::EdgeBitmap(bitmap) => edge_bitmap_cost(bitmap, stats, storage),
        exec::ExecCountCursorPlan::NodeUnique { lookup, .. } => storage.unique_equality_lookup(
            storage.unique_equality_rows(stats.node_eq_cardinality.get(&lookup.key).copied()),
        ),
        exec::ExecCountCursorPlan::NodeRange(plan) => storage.secondary_range_lookup(
            stats
                .node_range_cardinality
                .get(&plan.key)
                .copied()
                .map_or(storage.default_range_index_rows, cost::EstimatedRows::rows),
        ),
        exec::ExecCountCursorPlan::EdgeRange(plan) => storage.secondary_range_lookup(
            stats
                .edge_range_cardinality
                .get(&plan.key)
                .copied()
                .map_or(storage.default_range_index_rows, cost::EstimatedRows::rows),
        ),
        exec::ExecCountCursorPlan::NodeAuthoritativeScan(predicate) => {
            storage.null_equality_scan(node_scan_rows(predicate, stats, storage))
        }
        exec::ExecCountCursorPlan::EdgeAuthoritativeScan(predicate) => {
            storage.null_equality_scan(edge_scan_rows(predicate, stats, storage))
        }
        exec::ExecCountCursorPlan::NodePointReads(ids)
        | exec::ExecCountCursorPlan::EdgePointReads(ids) => {
            storage.point_gets(properties::PositiveUsize::at_least_one(ids.as_ref().len()))
        }
        exec::ExecCountCursorPlan::NodeFullScan | exec::ExecCountCursorPlan::EdgeFullScan => {
            storage.range_scan(storage.default_unknown_scan_rows)
        }
        exec::ExecCountCursorPlan::NodeLabelBitmap(label) => storage.range_scan(
            stats
                .node_label_cardinality
                .get(label)
                .copied()
                .map_or(storage.default_unknown_scan_rows, cost::EstimatedRows::rows),
        ),
        exec::ExecCountCursorPlan::EdgeLabelBitmap(label) => storage.range_scan(
            stats
                .edge_label_cardinality
                .get(label)
                .copied()
                .map_or(storage.default_unknown_scan_rows, cost::EstimatedRows::rows),
        ),
        exec::ExecCountCursorPlan::NodeVectorSearch { .. }
        | exec::ExecCountCursorPlan::EdgeVectorSearch { .. }
        | exec::ExecCountCursorPlan::NodeTextSearch { .. }
        | exec::ExecCountCursorPlan::EdgeTextSearch { .. } => {
            storage.range_scan(storage.default_unknown_scan_rows)
        }
        exec::ExecCountCursorPlan::NodeDynamicEquality { .. }
        | exec::ExecCountCursorPlan::EdgeDynamicEquality { .. } => storage
            .bitmap_equality_lookup(storage.default_equality_index_rows)
            .serial(storage.null_equality_scan(storage.default_unknown_scan_rows)),
        exec::ExecCountCursorPlan::Union { driver, rest }
        | exec::ExecCountCursorPlan::Intersect { driver, rest } => {
            rest.iter()
                .fold(cursor_cost(driver, stats, storage), |cost, child| {
                    cost.serial(cursor_cost(child, stats, storage))
                        .serial(storage.secondary_set_operation(storage.default_unknown_scan_rows))
                })
        }
        exec::ExecCountCursorPlan::Filter { input, .. } => cursor_cost(input, stats, storage)
            .serial(storage.predicate_eval(storage.default_unknown_scan_rows)),
        exec::ExecCountCursorPlan::Window { input, .. } => cursor_cost(input, stats, storage),
        exec::ExecCountCursorPlan::Order { input, .. } => cursor_cost(input, stats, storage)
            .serial(storage.explicit_sort(storage.default_unknown_scan_rows)),
        exec::ExecCountCursorPlan::Expand { input, .. }
        | exec::ExecCountCursorPlan::VectorSearch { input, .. }
        | exec::ExecCountCursorPlan::TextSearch { input, .. }
        | exec::ExecCountCursorPlan::Variable { input, .. }
        | exec::ExecCountCursorPlan::Distinct { input, .. } => cursor_cost(input, stats, storage)
            .serial(storage.stream_operator(storage.default_unknown_scan_rows)),
    }
}

fn node_scan_rows(
    predicate: &exec::ExecNodeAuthoritativeScanPredicate,
    stats: &context::StatsSnapshot,
    storage: &cost::StorageCostProfile,
) -> cost::EstimatedRows {
    match predicate {
        exec::ExecNodeAuthoritativeScanPredicate::NullEquality { key } => stats
            .node_label_cardinality
            .get(&key.label)
            .copied()
            .map_or(storage.default_unknown_scan_rows, cost::EstimatedRows::rows),
        exec::ExecNodeAuthoritativeScanPredicate::Predicate(_) => storage.default_unknown_scan_rows,
    }
}

fn edge_scan_rows(
    predicate: &exec::ExecEdgeAuthoritativeScanPredicate,
    stats: &context::StatsSnapshot,
    storage: &cost::StorageCostProfile,
) -> cost::EstimatedRows {
    match predicate {
        exec::ExecEdgeAuthoritativeScanPredicate::NullEquality { key } => stats
            .edge_label_cardinality
            .get(&key.label)
            .copied()
            .map_or(storage.default_unknown_scan_rows, cost::EstimatedRows::rows),
        exec::ExecEdgeAuthoritativeScanPredicate::Predicate(_) => storage.default_unknown_scan_rows,
    }
}

fn rejection(reason: &'static str) -> RuleRejection {
    RuleRejection::new(reason).expect("cardinality rejection reasons are non-empty")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use helix_ast::{index::RangeIndexDirection, query::QueryValue, value::PropertyValue};

    use super::*;
    use crate::optimizer::OptimizerRule;

    fn name(value: &str) -> ir::NonEmptyString {
        ir::NonEmptyString::new(value).unwrap()
    }

    fn node_path(plan: ir::NodeAccessPlan) -> logical::AccessPath {
        logical::AccessPath::Node(logical::NodeAccessPath::new(
            ir::NodeAccessSourcePlan::new(plan).unwrap(),
        ))
    }

    fn edge_path(plan: ir::EdgeAccessPlan) -> logical::AccessPath {
        logical::AccessPath::Edge(logical::EdgeAccessPath::new(
            ir::EdgeAccessSourcePlan::new(plan).unwrap(),
        ))
    }

    fn literal(value: PropertyValue) -> ir::IndexValue {
        ir::IndexValue::Literal(ir::SecondaryIndexLiteral::new(value).unwrap())
    }

    fn node_equality(
        index_id: &str,
        property: &str,
        value: ir::IndexValue,
        uniqueness: catalog::IndexUniqueness,
    ) -> ir::NodeAccessPlan {
        ir::NodeAccessPlan::EqualityIndex {
            index: catalog::NodeEqualityIndexMeta::new(name(index_id)).with_uniqueness(uniqueness),
            key: catalog::ScopedPropertyKey::try_new("User", property).unwrap(),
            value,
        }
    }

    fn edge_equality(index_id: &str, property: &str, value: ir::IndexValue) -> ir::EdgeAccessPlan {
        ir::EdgeAccessPlan::EqualityIndex {
            index: catalog::EdgeEqualityIndexMeta::new(name(index_id)),
            key: catalog::ScopedPropertyKey::try_new("LIKES", property).unwrap(),
            value,
        }
    }

    fn apply(
        input: logical::RootStream,
        params: context::ParamBindings,
        late_bound_params: BTreeSet<ir::NonEmptyString>,
    ) -> optimizer::RuleResult {
        let expr = logical::LogicalExpr::StreamCardinality(
            logical::StreamCardinality::new(input)
                .with_planning_bindings(params, late_bound_params),
        );
        let storage = cost::StorageCostProfile::default();
        let indexes = catalog::IndexCatalogSnapshot::default();
        let limits = context::PlannerLimits::default();
        let stats = context::StatsSnapshot::default();
        StreamCardinalityImplementationRule::default().apply(optimizer::RuleInput {
            expr: &expr,
            planner_limits: &limits,
            stats: &stats,
            storage: &storage,
            indexes: &indexes,
        })
    }

    fn plans(result: optimizer::RuleResult) -> Vec<exec::ExecCountPlan> {
        let optimizer::RuleResult::Applied(optimizer::RuleEffect::Physical(alternatives)) = result
        else {
            panic!("expected physical cardinality alternatives");
        };
        alternatives
            .into_iter()
            .map(|alternative| {
                let physical::PhysicalExpr::Cardinality(plan) = alternative.expr else {
                    panic!("expected physical cardinality plan");
                };
                plan.into_executable()
            })
            .collect()
    }

    fn access(input: logical::AccessPath) -> logical::RootStream {
        logical::RootStream::Access(logical::AccessStream::Path(input))
    }

    #[test]
    fn rule_outcome_matrix_covers_not_applicable_applied_and_rejected() {
        let non_cardinality =
            logical::LogicalExpr::AccessPath(node_path(ir::NodeAccessPlan::AllScan));
        let storage = cost::StorageCostProfile::default();
        let indexes = catalog::IndexCatalogSnapshot::default();
        let limits = context::PlannerLimits::default();
        let stats = context::StatsSnapshot::default();
        assert_eq!(
            StreamCardinalityImplementationRule::default().apply(optimizer::RuleInput {
                expr: &non_cardinality,
                planner_limits: &limits,
                stats: &stats,
                storage: &storage,
                indexes: &indexes,
            }),
            optimizer::RuleResult::NotApplicable
        );

        assert!(matches!(
            apply(
                access(node_path(ir::NodeAccessPlan::AllScan)),
                context::ParamBindings::default(),
                BTreeSet::new(),
            ),
            optimizer::RuleResult::Applied(_)
        ));

        let rejected = apply(
            access(node_path(node_equality(
                "user_email",
                "email",
                ir::IndexValue::Param(name("missing")),
                catalog::IndexUniqueness::NonUnique,
            ))),
            context::ParamBindings::default(),
            BTreeSet::new(),
        );
        assert!(matches!(
            rejected,
            optimizer::RuleResult::Rejected(RuleRejection { reason })
                if reason.as_ref() == "missing_planning_equality_parameter"
        ));
    }

    #[test]
    fn node_equality_values_choose_exact_physical_algorithms() {
        let cases = [
            (
                literal(PropertyValue::from("alice@example.test")),
                catalog::IndexUniqueness::NonUnique,
                physical::PhysicalCardinality::BitmapPoint,
            ),
            (
                literal(PropertyValue::from("alice@example.test")),
                catalog::IndexUniqueness::Unique,
                physical::PhysicalCardinality::UniqueVerified,
            ),
            (
                literal(PropertyValue::Null),
                catalog::IndexUniqueness::NonUnique,
                physical::PhysicalCardinality::AuthoritativeScan,
            ),
            (
                literal(PropertyValue::F64(f64::NAN)),
                catalog::IndexUniqueness::NonUnique,
                physical::PhysicalCardinality::Constant,
            ),
            (
                literal(PropertyValue::F32(f32::NAN)),
                catalog::IndexUniqueness::NonUnique,
                physical::PhysicalCardinality::Constant,
            ),
            (
                literal(PropertyValue::F64Array(vec![1.0, f64::NAN])),
                catalog::IndexUniqueness::NonUnique,
                physical::PhysicalCardinality::Constant,
            ),
            (
                literal(PropertyValue::F32Array(vec![f32::NAN])),
                catalog::IndexUniqueness::NonUnique,
                physical::PhysicalCardinality::Constant,
            ),
        ];

        for (value, uniqueness, expected) in cases {
            let [plan] = plans(apply(
                access(node_path(node_equality(
                    "user_email",
                    "email",
                    value,
                    uniqueness,
                ))),
                context::ParamBindings::default(),
                BTreeSet::new(),
            ))
            .try_into()
            .unwrap();
            assert_eq!(physical::PhysicalCountPlan::new(plan).family(), expected);
        }
    }

    #[test]
    fn edge_equality_values_choose_bitmap_scan_and_constant() {
        for (value, expected) in [
            (
                literal(PropertyValue::I64(7)),
                physical::PhysicalCardinality::BitmapPoint,
            ),
            (
                literal(PropertyValue::Null),
                physical::PhysicalCardinality::AuthoritativeScan,
            ),
            (
                literal(PropertyValue::F64(f64::NAN)),
                physical::PhysicalCardinality::Constant,
            ),
        ] {
            let [plan] = plans(apply(
                access(edge_path(edge_equality("likes_weight", "weight", value))),
                context::ParamBindings::default(),
                BTreeSet::new(),
            ))
            .try_into()
            .unwrap();
            assert_eq!(physical::PhysicalCountPlan::new(plan).family(), expected);
        }
    }

    #[test]
    fn ordinary_and_scoped_parameters_are_classified_at_the_correct_boundary() {
        let parameter = name("email");
        let source = || {
            access(node_path(node_equality(
                "user_email",
                "email",
                ir::IndexValue::Param(parameter.clone()),
                catalog::IndexUniqueness::NonUnique,
            )))
        };
        let property_params =
            context::ParamBindings::default().with_value(parameter.clone(), "alice@example.test");
        let query_params =
            context::ParamBindings::default().with_query_value(parameter.clone(), QueryValue::Null);

        assert!(matches!(
            plans(apply(source(), property_params, BTreeSet::new())).as_slice(),
            [exec::ExecCountPlan::NodeBitmap(_)]
        ));
        assert!(matches!(
            plans(apply(source(), query_params, BTreeSet::new())).as_slice(),
            [exec::ExecCountPlan::NodeAuthoritativeScan(_)]
        ));
        assert!(matches!(
            plans(apply(
                source(),
                context::ParamBindings::default(),
                BTreeSet::from([parameter]),
            ))
            .as_slice(),
            [exec::ExecCountPlan::NodeDynamicEquality(_)]
        ));
    }

    #[test]
    fn nested_query_parameters_are_rejected_before_physical_lowering() {
        let parameter = name("value");
        for nested in [
            QueryValue::Array(vec![QueryValue::I64(1)]),
            QueryValue::Object(std::collections::BTreeMap::from([(
                "key".to_string(),
                QueryValue::Bool(true),
            )])),
        ] {
            let result = apply(
                access(edge_path(edge_equality(
                    "likes_weight",
                    "weight",
                    ir::IndexValue::Param(parameter.clone()),
                ))),
                context::ParamBindings::default().with_query_value(parameter.clone(), nested),
                BTreeSet::new(),
            );
            assert!(matches!(
                result,
                optimizer::RuleResult::Rejected(RuleRejection { reason })
                    if reason.as_ref() == "unsupported_planning_equality_parameter"
            ));
        }
    }

    #[test]
    fn same_index_union_is_batched_and_different_indexes_preserve_driver_order() {
        let left = ir::NodeAccessSourcePlan::new(node_equality(
            "user_age",
            "age",
            literal(PropertyValue::I64(21)),
            catalog::IndexUniqueness::NonUnique,
        ))
        .unwrap();
        let same = ir::NodeAccessSourcePlan::new(node_equality(
            "user_age",
            "age",
            literal(PropertyValue::I64(42)),
            catalog::IndexUniqueness::NonUnique,
        ))
        .unwrap();
        let different = ir::NodeAccessSourcePlan::new(node_equality(
            "user_score",
            "score",
            literal(PropertyValue::I64(90)),
            catalog::IndexUniqueness::NonUnique,
        ))
        .unwrap();

        let [batched] = plans(apply(
            access(node_path(ir::NodeAccessPlan::Union(
                ir::AtLeast::from_pair(left.clone(), same),
            ))),
            context::ParamBindings::default(),
            BTreeSet::new(),
        ))
        .try_into()
        .unwrap();
        assert!(matches!(
            batched,
            exec::ExecCountPlan::NodeBitmap(exec::ExecNodeBitmapCountPlan {
                bitmap: exec::ExecNodeBitmapExpr::BatchedUnionRead { values, .. },
                ..
            }) if values.len() == 2
        ));

        let [union] = plans(apply(
            access(node_path(ir::NodeAccessPlan::Union(
                ir::AtLeast::from_pair(left, different),
            ))),
            context::ParamBindings::default(),
            BTreeSet::new(),
        ))
        .try_into()
        .unwrap();
        let exec::ExecCountPlan::NodeBitmap(exec::ExecNodeBitmapCountPlan {
            bitmap: exec::ExecNodeBitmapExpr::Union { driver, rest },
            ..
        }) = union
        else {
            panic!("expected explicit bitmap union");
        };
        assert!(matches!(
            driver.as_ref(),
            exec::ExecNodeBitmapExpr::PointRead { key, .. } if key.property.as_ref() == "age"
        ));
        assert!(matches!(
            rest.first(),
            Some(exec::ExecNodeBitmapExpr::PointRead { key, .. })
                if key.property.as_ref() == "score"
        ));
    }

    #[test]
    fn verified_range_intersection_encodes_driver_membership_and_window() {
        let range = ir::NodeAccessSourcePlan::new(ir::NodeAccessPlan::RangeIndex {
            index: catalog::NodeRangeIndexMeta::try_new("user_age").unwrap(),
            key: catalog::ScopedPropertyDirectionKey::try_new(
                "User",
                "age",
                RangeIndexDirection::Desc,
            )
            .unwrap(),
            range: ir::IndexRange::All,
        })
        .unwrap();
        let bitmap = ir::NodeAccessSourcePlan::new(node_equality(
            "user_active",
            "active",
            literal(PropertyValue::Bool(true)),
            catalog::IndexUniqueness::NonUnique,
        ))
        .unwrap();
        let pipeline = logical::AccessPipeline::new(
            node_path(ir::NodeAccessPlan::Intersect(ir::AtLeast::from_pair(
                range, bitmap,
            ))),
            ir::AtLeast::from_one_and_rest(
                logical::StreamPipelineOp::Skip {
                    count: ir::StreamBoundPlan::Literal(100),
                },
                vec![logical::StreamPipelineOp::Limit {
                    count: ir::StreamBoundPlan::Literal(10),
                }],
            ),
        )
        .unwrap();
        let [plan] = plans(apply(
            logical::RootStream::Access(logical::AccessStream::Pipeline(pipeline)),
            context::ParamBindings::default(),
            BTreeSet::new(),
        ))
        .try_into()
        .unwrap();
        let exec::ExecCountPlan::NodeRange(plan) = plan else {
            panic!("expected range-driven count");
        };
        assert_eq!(plan.driver.key.direction, RangeIndexDirection::Desc);
        assert!(matches!(
            plan.membership,
            exec::ExecNodeRangeMembershipPlan::BitmapFilters(ref filters)
                if filters.len() == 1
        ));
        assert_eq!(plan.window.skip, exec::ExecUsizeExpr::Literal(100));
        assert_eq!(
            plan.window.take,
            exec::ExecCountTake::AtMost(exec::ExecUsizeExpr::Literal(10))
        );
    }

    #[test]
    fn positioned_windows_distinct_and_variable_write_keep_their_semantic_boundaries() {
        let predicate =
            ir::PredicatePlan::new(helix_ast::expr::Predicate::eq("active", true)).unwrap();
        let positioned = logical::AccessPipeline::new(
            node_path(ir::NodeAccessPlan::AllScan),
            ir::AtLeast::from_one_and_rest(
                logical::StreamPipelineOp::Limit {
                    count: ir::StreamBoundPlan::Literal(5),
                },
                vec![logical::StreamPipelineOp::Filter { predicate }],
            ),
        )
        .unwrap();
        let [plan] = plans(apply(
            logical::RootStream::Access(logical::AccessStream::Pipeline(positioned)),
            context::ParamBindings::default(),
            BTreeSet::new(),
        ))
        .try_into()
        .unwrap();
        assert!(matches!(
            plan,
            exec::ExecCountPlan::Stream(exec::ExecCountStreamPlan {
                cursor: exec::ExecCountCursorPlan::Filter { input, .. },
                ..
            }) if matches!(input.as_ref(), exec::ExecCountCursorPlan::Window { .. })
        ));

        let distinct = logical::AccessDistinct::new(node_path(ir::NodeAccessPlan::AllScan));
        assert!(matches!(
            plans(apply(
                logical::RootStream::Access(logical::AccessStream::Distinct(distinct)),
                context::ParamBindings::default(),
                BTreeSet::new(),
            ))
            .as_slice(),
            [exec::ExecCountPlan::Stream(exec::ExecCountStreamPlan {
                cursor: exec::ExecCountCursorPlan::Distinct { .. },
                ..
            })]
        ));

        let write = logical::AccessPipeline::new(
            node_path(ir::NodeAccessPlan::AllScan),
            ir::AtLeast::from_one(logical::StreamPipelineOp::VariableWrite {
                op: logical::StreamVariableWriteOp::Store(name("saved")),
            }),
        )
        .unwrap();
        assert!(matches!(
            plans(apply(
                logical::RootStream::Access(logical::AccessStream::Pipeline(write)),
                context::ParamBindings::default(),
                BTreeSet::new(),
            ))
            .as_slice(),
            [exec::ExecCountPlan::InputRows { .. }]
        ));
    }
}
