use serde::{Deserialize, Serialize};

use crate::{catalog, ir};

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
    /// Node equality-index lookup.
    EqualityIndex {
        /// Index metadata.
        index: catalog::NodeEqualityIndexMeta,
        /// Indexed property key.
        key: catalog::ScopedPropertyKey,
        /// Lookup value.
        value: ir::IndexValue,
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
    /// One or more values for the same equality index. Multiple values are a
    /// union and may be served as one close-key batch.
    Equality {
        /// Logical index metadata.
        index: catalog::NodeEqualityIndexMeta,
        /// Indexed property.
        key: catalog::ScopedPropertyKey,
        /// Non-empty lookup values.
        values: ir::AtLeast<ir::IndexValue, 1>,
    },
    /// Generation-qualified range scan resolved by the database.
    Range(ExecNodeSecondaryRangePlan),
    /// Unordered set intersection.
    Intersect(ir::AtLeast<ExecNodeSecondarySetPlan, 2>),
    /// Set union.
    Union(ir::AtLeast<ExecNodeSecondarySetPlan, 2>),
    /// Range-ordered intersection. Filters are fully applied before a limit may
    /// consume the ordered result.
    OrderedIntersect {
        /// Range scan that defines result order.
        driver: ExecNodeSecondaryRangePlan,
        /// Non-empty secondary filters applied to driver IDs.
        filters: ir::AtLeast<ExecNodeSecondarySetPlan, 1>,
    },
}
