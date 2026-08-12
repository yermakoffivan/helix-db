use serde::{Deserialize, Serialize};

use crate::{catalog, exec, ir};

/// Native executable edge access.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecEdgeAccessPlan {
    /// Known empty edge stream.
    Empty,
    /// Runtime parameter edge IDs.
    FromParam { param: ir::NonEmptyString },
    /// Variable edge set.
    FromVar { variable: ir::NonEmptyString },
    /// Full edge scan.
    AllScan,
    /// Label scan.
    LabelScan { label: ir::NonEmptyString },
    /// Exact non-unique equality bitmap program.
    Bitmap {
        /// Planner-selected bitmap primitive tree.
        bitmap: exec::ExecEdgeBitmapExpr,
    },
    /// Exact authoritative equality scan.
    AuthoritativeScan {
        /// Predicate applied to authoritative graph rows.
        predicate: exec::ExecEdgeAuthoritativeScanPredicate,
    },
    /// Explicit runtime equality classifier exception.
    DynamicEquality {
        /// Logical index metadata used to validate the runtime branch.
        index: catalog::EdgeEqualityIndexMeta,
        /// Indexed property key.
        key: catalog::ScopedPropertyKey,
        /// Genuinely late-bound parameter.
        param: ir::NonEmptyString,
    },
    /// Edge range-index scan.
    RangeIndex {
        /// Index metadata.
        index: catalog::EdgeRangeIndexMeta,
        /// Indexed property key and direction.
        key: catalog::ScopedPropertyDirectionKey,
        /// Range bounds.
        range: ir::IndexRange,
    },
    /// V2-aware secondary-ID set evaluated before row materialization.
    SecondarySet {
        /// Logical secondary-index set contract.
        set: ExecEdgeSecondarySetPlan,
    },
    /// Edge vector search.
    VectorSearch {
        /// Search key.
        key: catalog::EdgeSearchIndexKey,
        /// Search index execution plan.
        index: ir::SearchIndexPlan,
        /// Query vector.
        query_vector: ir::VectorQueryInputPlan,
        /// Result count.
        k: ir::SearchLimitPlan,
    },
    /// Edge text search.
    TextSearch {
        /// Search key.
        key: catalog::EdgeSearchIndexKey,
        /// Search index execution plan.
        index: ir::SearchIndexPlan,
        /// Query text.
        query_text: ir::TextQueryInputPlan,
        /// Result count.
        k: ir::SearchLimitPlan,
    },
}

impl ExecEdgeAccessPlan {
    /// Classify one logical equality into an exact executable row-access primitive.
    pub fn exact_equality(
        index: catalog::EdgeEqualityIndexMeta,
        key: catalog::ScopedPropertyKey,
        value: ir::IndexValue,
    ) -> Self {
        match value {
            ir::IndexValue::Literal(value) => match value.semantics() {
                ir::EqualityIndexValueSemantics::Indexed => Self::Bitmap {
                    bitmap: exec::ExecEdgeBitmapExpr::PointRead {
                        index: exec::ExecEdgeNonUniqueEqualityIndex::new(index),
                        key,
                        value: exec::ExecIndexedEqualityValue::try_from(value)
                            .expect("indexed equality semantics produce an executable value"),
                    },
                },
                ir::EqualityIndexValueSemantics::AuthoritativeNull => Self::AuthoritativeScan {
                    predicate: exec::ExecEdgeAuthoritativeScanPredicate::NullEquality { key },
                },
                ir::EqualityIndexValueSemantics::NonReflexive => Self::Empty,
                ir::EqualityIndexValueSemantics::RuntimeDependent => {
                    unreachable!("literal equality semantics are never runtime-dependent")
                }
            },
            ir::IndexValue::Param(param) => Self::DynamicEquality { index, key, param },
        }
    }
}

/// Executable edge range leaf used directly or as an ordered intersection driver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecEdgeSecondaryRangePlan {
    /// Logical index metadata.
    pub index: catalog::EdgeRangeIndexMeta,
    /// Indexed property and physical direction capability.
    pub key: catalog::ScopedPropertyDirectionKey,
    /// Logical range bounds.
    pub range: ir::IndexRange,
}

/// V2-aware edge secondary-ID set.
///
/// The database resolves physical index ownership and generation from the
/// request-authorized Active catalog at execution time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecEdgeSecondarySetPlan {
    /// Statically empty set, including non-reflexive NaN equality.
    Empty,
    /// Exact non-unique equality bitmap program.
    Bitmap(exec::ExecEdgeBitmapExpr),
    /// Exact authoritative scan.
    AuthoritativeScan(exec::ExecEdgeAuthoritativeScanPredicate),
    /// Explicit runtime equality classifier exception.
    DynamicEquality {
        /// Logical index metadata.
        index: catalog::EdgeEqualityIndexMeta,
        /// Indexed property.
        key: catalog::ScopedPropertyKey,
        /// Genuinely late-bound parameter.
        param: ir::NonEmptyString,
    },
    /// Generation-qualified range scan resolved by the database.
    Range(ExecEdgeSecondaryRangePlan),
    /// Set intersection in planner-selected evaluation order.
    Intersect {
        /// First child executed.
        driver: Box<ExecEdgeSecondarySetPlan>,
        /// Remaining children in exact order.
        rest: ir::AtLeast<ExecEdgeSecondarySetPlan, 1>,
    },
    /// Set union in planner-selected evaluation order.
    Union {
        /// First child executed.
        driver: Box<ExecEdgeSecondarySetPlan>,
        /// Remaining children in exact order.
        rest: ir::AtLeast<ExecEdgeSecondarySetPlan, 1>,
    },
    /// Range-ordered intersection with post-filter limit semantics.
    OrderedIntersect {
        /// Range scan that defines result order.
        driver: ExecEdgeSecondaryRangePlan,
        /// Non-empty secondary filters applied to driver IDs.
        filters: ir::AtLeast<ExecEdgeSecondarySetPlan, 1>,
    },
}

impl ExecEdgeSecondarySetPlan {
    /// Classify one or more same-index logical equalities into exact set primitives.
    pub fn exact_equalities(
        index: catalog::EdgeEqualityIndexMeta,
        key: catalog::ScopedPropertyKey,
        values: ir::AtLeast<ir::IndexValue, 1>,
    ) -> Self {
        let mut children = values
            .into_iter()
            .map(|value| {
                match ExecEdgeAccessPlan::exact_equality(index.clone(), key.clone(), value) {
                    ExecEdgeAccessPlan::Empty => Self::Empty,
                    ExecEdgeAccessPlan::Bitmap { bitmap } => Self::Bitmap(bitmap),
                    ExecEdgeAccessPlan::AuthoritativeScan { predicate } => {
                        Self::AuthoritativeScan(predicate)
                    }
                    ExecEdgeAccessPlan::DynamicEquality { index, key, param } => {
                        Self::DynamicEquality { index, key, param }
                    }
                    ExecEdgeAccessPlan::FromParam { .. }
                    | ExecEdgeAccessPlan::FromVar { .. }
                    | ExecEdgeAccessPlan::AllScan
                    | ExecEdgeAccessPlan::LabelScan { .. }
                    | ExecEdgeAccessPlan::RangeIndex { .. }
                    | ExecEdgeAccessPlan::SecondarySet { .. }
                    | ExecEdgeAccessPlan::VectorSearch { .. }
                    | ExecEdgeAccessPlan::TextSearch { .. } => {
                        unreachable!("equality classification returns an equality variant")
                    }
                }
            })
            .collect::<Vec<_>>();
        if children.len() >= 2
            && children.iter().all(|child| {
                matches!(
                    child,
                    Self::Bitmap(exec::ExecEdgeBitmapExpr::PointRead {
                        index: child_index,
                        key: child_key,
                        ..
                    }) if matches!(
                        &children[0],
                        Self::Bitmap(exec::ExecEdgeBitmapExpr::PointRead {
                            index: first_index,
                            key: first_key,
                            ..
                        }) if child_index == first_index && child_key == first_key
                    )
                )
            })
        {
            let Self::Bitmap(exec::ExecEdgeBitmapExpr::PointRead { index, key, value }) =
                children.remove(0)
            else {
                unreachable!("same-index batch starts with a point read")
            };
            let values = core::iter::once(value)
                .chain(children.into_iter().map(|child| {
                    let Self::Bitmap(exec::ExecEdgeBitmapExpr::PointRead { value, .. }) = child
                    else {
                        unreachable!("same-index batch contains only point reads")
                    };
                    value
                }))
                .collect::<Vec<_>>();
            return Self::Bitmap(exec::ExecEdgeBitmapExpr::BatchedUnionRead {
                index,
                key,
                values: ir::AtLeast::try_from_vec(values)
                    .expect("same-index batch has at least two values"),
            });
        }
        let driver = children.remove(0);
        let Some(rest) = ir::AtLeast::try_from_vec(children) else {
            return driver;
        };
        Self::Union {
            driver: Box::new(driver),
            rest,
        }
    }
}
