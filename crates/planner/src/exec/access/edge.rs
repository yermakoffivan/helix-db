use serde::{Deserialize, Serialize};

use crate::{catalog, ir};

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
    /// Edge equality-index lookup.
    EqualityIndex {
        /// Index metadata.
        index: catalog::EdgeEqualityIndexMeta,
        /// Indexed property key.
        key: catalog::ScopedPropertyKey,
        /// Lookup value.
        value: ir::IndexValue,
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
    /// One or more values for the same equality index.
    Equality {
        /// Logical index metadata.
        index: catalog::EdgeEqualityIndexMeta,
        /// Indexed property.
        key: catalog::ScopedPropertyKey,
        /// Non-empty lookup values.
        values: ir::AtLeast<ir::IndexValue, 1>,
    },
    /// Generation-qualified range scan resolved by the database.
    Range(ExecEdgeSecondaryRangePlan),
    /// Unordered set intersection.
    Intersect(ir::AtLeast<ExecEdgeSecondarySetPlan, 2>),
    /// Set union.
    Union(ir::AtLeast<ExecEdgeSecondarySetPlan, 2>),
    /// Range-ordered intersection with post-filter limit semantics.
    OrderedIntersect {
        /// Range scan that defines result order.
        driver: ExecEdgeSecondaryRangePlan,
        /// Non-empty secondary filters applied to driver IDs.
        filters: ir::AtLeast<ExecEdgeSecondarySetPlan, 1>,
    },
}
