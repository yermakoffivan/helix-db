use serde::{Deserialize, Serialize};

use crate::{catalog, exec, ir};

/// Native executable node access.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecNodeAccessPlan {
    /// Known empty node stream.
    Empty,
    /// Runtime parameter node IDs.
    FromParam { param: ir::NonEmptyString },
    /// Variable node set.
    FromVar { variable: ir::NonEmptyString },
    /// Full node scan.
    AllScan,
    /// Label scan.
    LabelScan { label: ir::NonEmptyString },
    /// Exact non-unique equality bitmap program.
    Bitmap {
        /// Planner-selected bitmap primitive tree.
        bitmap: exec::ExecNodeBitmapExpr,
    },
    /// Exact unique-owner lookup followed by authoritative verification.
    Unique {
        /// Planner-selected unique owner point read.
        lookup: exec::ExecNodeUniqueOwnerReadPlan,
        /// Required authoritative verification.
        verification: exec::ExecNodeAuthoritativeVerificationPlan,
    },
    /// Exact authoritative equality scan.
    AuthoritativeScan {
        /// Predicate applied to authoritative graph rows.
        predicate: exec::ExecNodeAuthoritativeScanPredicate,
    },
    /// Explicit runtime equality classifier exception.
    DynamicEquality {
        /// Logical index metadata used to validate the runtime branch.
        index: catalog::NodeEqualityIndexMeta,
        /// Indexed property key.
        key: catalog::ScopedPropertyKey,
        /// Genuinely late-bound parameter.
        param: ir::NonEmptyString,
    },
    /// Node range-index scan.
    RangeIndex {
        /// Index metadata.
        index: catalog::NodeRangeIndexMeta,
        /// Indexed property key and direction.
        key: catalog::ScopedPropertyDirectionKey,
        /// Range bounds.
        range: ir::IndexRange,
    },
    /// V2-aware secondary-ID set evaluated before row materialization.
    SecondarySet {
        /// Logical secondary-index set contract.
        set: ExecNodeSecondarySetPlan,
    },
    /// Node vector search.
    VectorSearch {
        /// Search key.
        key: catalog::NodeSearchIndexKey,
        /// Search index execution plan.
        index: ir::SearchIndexPlan,
        /// Query vector.
        query_vector: ir::VectorQueryInputPlan,
        /// Result count.
        k: ir::SearchLimitPlan,
    },
    /// Node text search.
    TextSearch {
        /// Search key.
        key: catalog::NodeSearchIndexKey,
        /// Search index execution plan.
        index: ir::SearchIndexPlan,
        /// Query text.
        query_text: ir::TextQueryInputPlan,
        /// Result count.
        k: ir::SearchLimitPlan,
    },
}

impl ExecNodeAccessPlan {
    /// Classify one logical equality into an exact executable row-access primitive.
    pub fn exact_equality(
        index: catalog::NodeEqualityIndexMeta,
        key: catalog::ScopedPropertyKey,
        value: ir::IndexValue,
    ) -> Self {
        match value {
            ir::IndexValue::Literal(value) => match value.semantics() {
                ir::EqualityIndexValueSemantics::Indexed => {
                    let value = exec::ExecIndexedEqualityValue::try_from(value)
                        .expect("indexed equality semantics produce an executable value");
                    match index.uniqueness {
                        catalog::IndexUniqueness::Unique => Self::Unique {
                            lookup: exec::ExecNodeUniqueOwnerReadPlan {
                                index: exec::ExecNodeUniqueEqualityIndex::try_from(index)
                                    .expect("unique metadata produces a unique executable index"),
                                key: key.clone(),
                                value: value.clone(),
                            },
                            verification: exec::ExecNodeAuthoritativeVerificationPlan {
                                key,
                                value,
                            },
                        },
                        catalog::IndexUniqueness::NonUnique => Self::Bitmap {
                            bitmap: exec::ExecNodeBitmapExpr::PointRead {
                                index: exec::ExecNodeNonUniqueEqualityIndex::try_from(index)
                                    .expect(
                                    "non-unique metadata produces a non-unique executable index",
                                ),
                                key,
                                value,
                            },
                        },
                    }
                }
                ir::EqualityIndexValueSemantics::AuthoritativeNull => Self::AuthoritativeScan {
                    predicate: exec::ExecNodeAuthoritativeScanPredicate::NullEquality { key },
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

/// Executable node range leaf used directly or as an ordered intersection driver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecNodeSecondaryRangePlan {
    /// Logical index metadata.
    pub index: catalog::NodeRangeIndexMeta,
    /// Indexed property and physical direction capability.
    pub key: catalog::ScopedPropertyDirectionKey,
    /// Logical range bounds.
    pub range: ir::IndexRange,
}

/// V2-aware node secondary-ID set.
///
/// Raw physical keys, index IDs, generations, and tenant scope are deliberately
/// absent. The database resolves those details from the request-authorized
/// Active index catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecNodeSecondarySetPlan {
    /// Statically empty set, including non-reflexive NaN equality.
    Empty,
    /// Exact non-unique equality bitmap program.
    Bitmap(exec::ExecNodeBitmapExpr),
    /// Exact unique-owner lookup and verification.
    Unique {
        /// Planner-selected owner read.
        lookup: exec::ExecNodeUniqueOwnerReadPlan,
        /// Required authoritative verification.
        verification: exec::ExecNodeAuthoritativeVerificationPlan,
    },
    /// Exact authoritative scan.
    AuthoritativeScan(exec::ExecNodeAuthoritativeScanPredicate),
    /// Explicit runtime equality classifier exception.
    DynamicEquality {
        /// Logical index metadata.
        index: catalog::NodeEqualityIndexMeta,
        /// Indexed property.
        key: catalog::ScopedPropertyKey,
        /// Genuinely late-bound parameter.
        param: ir::NonEmptyString,
    },
    /// Generation-qualified range scan resolved by the database.
    Range(ExecNodeSecondaryRangePlan),
    /// Set intersection in planner-selected evaluation order.
    Intersect {
        /// First child executed.
        driver: Box<ExecNodeSecondarySetPlan>,
        /// Remaining children in exact order.
        rest: ir::AtLeast<ExecNodeSecondarySetPlan, 1>,
    },
    /// Set union in planner-selected evaluation order.
    Union {
        /// First child executed.
        driver: Box<ExecNodeSecondarySetPlan>,
        /// Remaining children in exact order.
        rest: ir::AtLeast<ExecNodeSecondarySetPlan, 1>,
    },
    /// Range-ordered intersection. Filters are fully applied before a limit may
    /// consume the ordered result.
    OrderedIntersect {
        /// Range scan that defines result order.
        driver: ExecNodeSecondaryRangePlan,
        /// Non-empty secondary filters applied to driver IDs.
        filters: ir::AtLeast<ExecNodeSecondarySetPlan, 1>,
    },
}

impl ExecNodeSecondarySetPlan {
    /// Classify one or more same-index logical equalities into exact set primitives.
    pub fn exact_equalities(
        index: catalog::NodeEqualityIndexMeta,
        key: catalog::ScopedPropertyKey,
        values: ir::AtLeast<ir::IndexValue, 1>,
    ) -> Self {
        let mut children = values
            .into_iter()
            .map(|value| {
                match ExecNodeAccessPlan::exact_equality(index.clone(), key.clone(), value) {
                    ExecNodeAccessPlan::Empty => Self::Empty,
                    ExecNodeAccessPlan::Bitmap { bitmap } => Self::Bitmap(bitmap),
                    ExecNodeAccessPlan::Unique {
                        lookup,
                        verification,
                    } => Self::Unique {
                        lookup,
                        verification,
                    },
                    ExecNodeAccessPlan::AuthoritativeScan { predicate } => {
                        Self::AuthoritativeScan(predicate)
                    }
                    ExecNodeAccessPlan::DynamicEquality { index, key, param } => {
                        Self::DynamicEquality { index, key, param }
                    }
                    ExecNodeAccessPlan::FromParam { .. }
                    | ExecNodeAccessPlan::FromVar { .. }
                    | ExecNodeAccessPlan::AllScan
                    | ExecNodeAccessPlan::LabelScan { .. }
                    | ExecNodeAccessPlan::RangeIndex { .. }
                    | ExecNodeAccessPlan::SecondarySet { .. }
                    | ExecNodeAccessPlan::VectorSearch { .. }
                    | ExecNodeAccessPlan::TextSearch { .. } => {
                        unreachable!("equality classification returns an equality variant")
                    }
                }
            })
            .collect::<Vec<_>>();
        if children.len() >= 2
            && children.iter().all(|child| {
                matches!(
                    child,
                    Self::Bitmap(exec::ExecNodeBitmapExpr::PointRead {
                        index: child_index,
                        key: child_key,
                        ..
                    }) if matches!(
                        &children[0],
                        Self::Bitmap(exec::ExecNodeBitmapExpr::PointRead {
                            index: first_index,
                            key: first_key,
                            ..
                        }) if child_index == first_index && child_key == first_key
                    )
                )
            })
        {
            let Self::Bitmap(exec::ExecNodeBitmapExpr::PointRead { index, key, value }) =
                children.remove(0)
            else {
                unreachable!("same-index batch starts with a point read")
            };
            let values = core::iter::once(value)
                .chain(children.into_iter().map(|child| {
                    let Self::Bitmap(exec::ExecNodeBitmapExpr::PointRead { value, .. }) = child
                    else {
                        unreachable!("same-index batch contains only point reads")
                    };
                    value
                }))
                .collect::<Vec<_>>();
            return Self::Bitmap(exec::ExecNodeBitmapExpr::BatchedUnionRead {
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
