//! Exact executable cardinality contracts.
//!
//! Count plans are physical programs. Each variant names the primitive the
//! interpreter must execute; the interpreter must not replace it with a
//! different access, batching, ordering, or materialization strategy.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{catalog, ir};

/// Failure to construct a physical equality contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecEqualityContractError {
    /// A non-unique node index was required.
    ExpectedNodeNonUnique,
    /// A unique node index was required.
    ExpectedNodeUnique,
    /// A reflexive, physically indexed equality value was required.
    ExpectedIndexedValue,
}

impl std::fmt::Display for ExecEqualityContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExpectedNodeNonUnique => f.write_str("expected a non-unique node equality index"),
            Self::ExpectedNodeUnique => f.write_str("expected a unique node equality index"),
            Self::ExpectedIndexedValue => {
                f.write_str("expected a reflexive physically indexed equality value")
            }
        }
    }
}

impl std::error::Error for ExecEqualityContractError {}

/// Node equality index proven to be non-unique.
///
/// ```
/// use helix_planner::{catalog, exec, ir};
///
/// let metadata = catalog::NodeEqualityIndexMeta::new(
///     ir::NonEmptyString::new("user_email").unwrap(),
/// );
/// let index = exec::ExecNodeNonUniqueEqualityIndex::try_from(metadata).unwrap();
/// assert_eq!(index.metadata().index_id.as_ref(), "user_email");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ExecNodeNonUniqueEqualityIndex(catalog::NodeEqualityIndexMeta);

impl ExecNodeNonUniqueEqualityIndex {
    /// Borrow the validated catalog metadata.
    pub const fn metadata(&self) -> &catalog::NodeEqualityIndexMeta {
        &self.0
    }

    /// Consume the wrapper.
    pub fn into_metadata(self) -> catalog::NodeEqualityIndexMeta {
        self.0
    }
}

impl TryFrom<catalog::NodeEqualityIndexMeta> for ExecNodeNonUniqueEqualityIndex {
    type Error = ExecEqualityContractError;

    fn try_from(value: catalog::NodeEqualityIndexMeta) -> Result<Self, Self::Error> {
        match value.uniqueness {
            catalog::IndexUniqueness::NonUnique => Ok(Self(value)),
            catalog::IndexUniqueness::Unique => {
                Err(ExecEqualityContractError::ExpectedNodeNonUnique)
            }
        }
    }
}

impl<'de> Deserialize<'de> for ExecNodeNonUniqueEqualityIndex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        catalog::NodeEqualityIndexMeta::deserialize(deserializer)?
            .try_into()
            .map_err(D::Error::custom)
    }
}

/// Node equality index proven to be unique.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ExecNodeUniqueEqualityIndex(catalog::NodeEqualityIndexMeta);

impl ExecNodeUniqueEqualityIndex {
    /// Borrow the validated catalog metadata.
    pub const fn metadata(&self) -> &catalog::NodeEqualityIndexMeta {
        &self.0
    }

    /// Consume the wrapper.
    pub fn into_metadata(self) -> catalog::NodeEqualityIndexMeta {
        self.0
    }
}

impl TryFrom<catalog::NodeEqualityIndexMeta> for ExecNodeUniqueEqualityIndex {
    type Error = ExecEqualityContractError;

    fn try_from(value: catalog::NodeEqualityIndexMeta) -> Result<Self, Self::Error> {
        match value.uniqueness {
            catalog::IndexUniqueness::Unique => Ok(Self(value)),
            catalog::IndexUniqueness::NonUnique => {
                Err(ExecEqualityContractError::ExpectedNodeUnique)
            }
        }
    }
}

impl<'de> Deserialize<'de> for ExecNodeUniqueEqualityIndex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        catalog::NodeEqualityIndexMeta::deserialize(deserializer)?
            .try_into()
            .map_err(D::Error::custom)
    }
}

/// Edge equality index validated by the edge catalog contract.
///
/// Edge equality indexes are currently always non-unique; the distinct wrapper
/// prevents a future node/edge metadata mix-up at bitmap call sites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecEdgeNonUniqueEqualityIndex(catalog::EdgeEqualityIndexMeta);

impl ExecEdgeNonUniqueEqualityIndex {
    /// Wrap validated edge equality metadata.
    pub const fn new(metadata: catalog::EdgeEqualityIndexMeta) -> Self {
        Self(metadata)
    }

    /// Borrow the validated catalog metadata.
    pub const fn metadata(&self) -> &catalog::EdgeEqualityIndexMeta {
        &self.0
    }

    /// Consume the wrapper.
    pub fn into_metadata(self) -> catalog::EdgeEqualityIndexMeta {
        self.0
    }
}

/// Equality value proven to have a reflexive physical index encoding.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ExecIndexedEqualityValue(ir::SecondaryIndexLiteral);

impl ExecIndexedEqualityValue {
    /// Borrow the validated literal.
    pub const fn literal(&self) -> &ir::SecondaryIndexLiteral {
        &self.0
    }

    /// Consume the wrapper.
    pub fn into_literal(self) -> ir::SecondaryIndexLiteral {
        self.0
    }
}

impl TryFrom<ir::SecondaryIndexLiteral> for ExecIndexedEqualityValue {
    type Error = ExecEqualityContractError;

    fn try_from(value: ir::SecondaryIndexLiteral) -> Result<Self, Self::Error> {
        match value.semantics() {
            ir::EqualityIndexValueSemantics::Indexed => Ok(Self(value)),
            ir::EqualityIndexValueSemantics::AuthoritativeNull
            | ir::EqualityIndexValueSemantics::NonReflexive
            | ir::EqualityIndexValueSemantics::RuntimeDependent => {
                Err(ExecEqualityContractError::ExpectedIndexedValue)
            }
        }
    }
}

impl<'de> Deserialize<'de> for ExecIndexedEqualityValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        ir::SecondaryIndexLiteral::deserialize(deserializer)?
            .try_into()
            .map_err(D::Error::custom)
    }
}

/// Small executable arithmetic expression for count windows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecUsizeExpr {
    /// Literal value.
    Literal(usize),
    /// Non-negative integer request parameter.
    Param(ir::NonEmptyString),
    /// Minimum of two expressions.
    Min(Box<Self>, Box<Self>),
    /// Saturating addition.
    SaturatingAdd(Box<Self>, Box<Self>),
    /// Saturating subtraction.
    SaturatingSub(Box<Self>, Box<Self>),
}

impl ExecUsizeExpr {
    /// Construct a literal.
    pub const fn literal(value: usize) -> Self {
        Self::Literal(value)
    }

    /// Construct and simplify a minimum expression.
    pub fn min(left: Self, right: Self) -> Self {
        if left == right {
            return left;
        }
        match (&left, &right) {
            (Self::Literal(left), Self::Literal(right)) => Self::Literal((*left).min(*right)),
            (Self::Literal(0), _) | (_, Self::Literal(0)) => Self::Literal(0),
            _ => Self::Min(Box::new(left), Box::new(right)),
        }
    }

    /// Construct and simplify saturating addition.
    pub fn saturating_add(left: Self, right: Self) -> Self {
        match (&left, &right) {
            (Self::Literal(left), Self::Literal(right)) => {
                Self::Literal(left.saturating_add(*right))
            }
            (Self::Literal(0), _) => right,
            (_, Self::Literal(0)) => left,
            _ => Self::SaturatingAdd(Box::new(left), Box::new(right)),
        }
    }

    /// Construct and simplify saturating subtraction.
    pub fn saturating_sub(left: Self, right: Self) -> Self {
        if left == right {
            return Self::Literal(0);
        }
        match (&left, &right) {
            (Self::Literal(left), Self::Literal(right)) => {
                Self::Literal(left.saturating_sub(*right))
            }
            (Self::Literal(0), _) => Self::Literal(0),
            (_, Self::Literal(0)) => left,
            _ => Self::SaturatingSub(Box::new(left), Box::new(right)),
        }
    }

    /// Number of nodes in this expression.
    pub fn node_count(&self) -> usize {
        match self {
            Self::Literal(_) | Self::Param(_) => 1,
            Self::Min(left, right)
            | Self::SaturatingAdd(left, right)
            | Self::SaturatingSub(left, right) => 1usize
                .saturating_add(left.node_count())
                .saturating_add(right.node_count()),
        }
    }

    /// Evaluate using a caller-provided parameter resolver.
    pub fn evaluate<E>(
        &self,
        resolve: &mut impl FnMut(&ir::NonEmptyString) -> Result<usize, E>,
    ) -> Result<usize, E> {
        match self {
            Self::Literal(value) => Ok(*value),
            Self::Param(param) => resolve(param),
            Self::Min(left, right) => Ok(left.evaluate(resolve)?.min(right.evaluate(resolve)?)),
            Self::SaturatingAdd(left, right) => Ok(left
                .evaluate(resolve)?
                .saturating_add(right.evaluate(resolve)?)),
            Self::SaturatingSub(left, right) => Ok(left
                .evaluate(resolve)?
                .saturating_sub(right.evaluate(resolve)?)),
        }
    }
}

/// Canonical upper bound for a count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecCountTake {
    /// Consume all matching rows.
    All,
    /// Consume at most the evaluated amount.
    AtMost(ExecUsizeExpr),
}

/// Canonical count window.
///
/// ```
/// use helix_planner::exec::{ExecCountTake, ExecCountWindowPlan, ExecUsizeExpr};
///
/// let window = ExecCountWindowPlan::identity()
///     .then_skip(ExecUsizeExpr::literal(100))
///     .then_limit(ExecUsizeExpr::literal(10));
/// assert_eq!(window.skip, ExecUsizeExpr::literal(100));
/// assert_eq!(window.take, ExecCountTake::AtMost(ExecUsizeExpr::literal(10)));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecCountWindowPlan {
    /// Matches discarded before counting.
    pub skip: ExecUsizeExpr,
    /// Maximum counted matches.
    pub take: ExecCountTake,
}

impl ExecCountWindowPlan {
    /// Identity window.
    pub const fn identity() -> Self {
        Self {
            skip: ExecUsizeExpr::Literal(0),
            take: ExecCountTake::All,
        }
    }

    /// Append a limit using sequential stream semantics.
    pub fn then_limit(self, count: ExecUsizeExpr) -> Self {
        let take = match self.take {
            ExecCountTake::All => ExecCountTake::AtMost(count),
            ExecCountTake::AtMost(existing) => {
                ExecCountTake::AtMost(ExecUsizeExpr::min(existing, count))
            }
        };
        Self {
            skip: self.skip,
            take,
        }
    }

    /// Append a skip using sequential stream semantics.
    pub fn then_skip(self, count: ExecUsizeExpr) -> Self {
        match self.take {
            ExecCountTake::All => Self {
                skip: ExecUsizeExpr::saturating_add(self.skip, count),
                take: ExecCountTake::All,
            },
            ExecCountTake::AtMost(existing) => {
                let consumed = ExecUsizeExpr::min(existing.clone(), count);
                Self {
                    skip: ExecUsizeExpr::saturating_add(self.skip, consumed.clone()),
                    take: ExecCountTake::AtMost(ExecUsizeExpr::saturating_sub(existing, consumed)),
                }
            }
        }
    }

    /// Append a half-open range using sequential stream semantics.
    pub fn then_range(self, start: ExecUsizeExpr, end: ExecUsizeExpr) -> Self {
        self.then_skip(start.clone())
            .then_limit(ExecUsizeExpr::saturating_sub(end, start))
    }

    /// Evaluate the final cardinality for an already-computed source count.
    pub fn apply<E>(
        &self,
        cardinality: usize,
        resolve: &mut impl FnMut(&ir::NonEmptyString) -> Result<usize, E>,
    ) -> Result<usize, E> {
        let after_skip = cardinality.saturating_sub(self.skip.evaluate(resolve)?);
        match &self.take {
            ExecCountTake::All => Ok(after_skip),
            ExecCountTake::AtMost(take) => Ok(after_skip.min(take.evaluate(resolve)?)),
        }
    }
}

impl Default for ExecCountWindowPlan {
    fn default() -> Self {
        Self::identity()
    }
}

/// Exact node non-unique equality bitmap program.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecNodeBitmapExpr {
    /// Read one bitmap row.
    PointRead {
        /// Proven non-unique index.
        index: ExecNodeNonUniqueEqualityIndex,
        /// Indexed property.
        key: catalog::ScopedPropertyKey,
        /// Indexed literal.
        value: ExecIndexedEqualityValue,
    },
    /// Read and union at least two bitmap rows with one multi-get.
    BatchedUnionRead {
        /// Proven non-unique index.
        index: ExecNodeNonUniqueEqualityIndex,
        /// Indexed property.
        key: catalog::ScopedPropertyKey,
        /// Planner-selected values in request order.
        values: ir::AtLeast<ExecIndexedEqualityValue, 2>,
    },
    /// Union children in encoded order.
    Union {
        /// First child.
        driver: Box<Self>,
        /// Remaining children.
        rest: ir::AtLeast<Self, 1>,
    },
    /// Intersect children in encoded order.
    Intersect {
        /// First child.
        driver: Box<Self>,
        /// Remaining children.
        rest: ir::AtLeast<Self, 1>,
    },
}

/// Exact edge non-unique equality bitmap program.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecEdgeBitmapExpr {
    /// Read one bitmap row.
    PointRead {
        /// Edge equality index.
        index: ExecEdgeNonUniqueEqualityIndex,
        /// Indexed property.
        key: catalog::ScopedPropertyKey,
        /// Indexed literal.
        value: ExecIndexedEqualityValue,
    },
    /// Read and union at least two bitmap rows with one multi-get.
    BatchedUnionRead {
        /// Edge equality index.
        index: ExecEdgeNonUniqueEqualityIndex,
        /// Indexed property.
        key: catalog::ScopedPropertyKey,
        /// Planner-selected values in request order.
        values: ir::AtLeast<ExecIndexedEqualityValue, 2>,
    },
    /// Union children in encoded order.
    Union {
        /// First child.
        driver: Box<Self>,
        /// Remaining children.
        rest: ir::AtLeast<Self, 1>,
    },
    /// Intersect children in encoded order.
    Intersect {
        /// First child.
        driver: Box<Self>,
        /// Remaining children.
        rest: ir::AtLeast<Self, 1>,
    },
}

/// Node bitmap count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecNodeBitmapCountPlan {
    /// Exact bitmap expression.
    pub bitmap: ExecNodeBitmapExpr,
    /// Normalized count window.
    pub window: ExecCountWindowPlan,
}

/// Edge bitmap count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecEdgeBitmapCountPlan {
    /// Exact bitmap expression.
    pub bitmap: ExecEdgeBitmapExpr,
    /// Normalized count window.
    pub window: ExecCountWindowPlan,
}

/// Exact unique-owner point read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecNodeUniqueOwnerReadPlan {
    /// Proven unique index.
    pub index: ExecNodeUniqueEqualityIndex,
    /// Indexed property.
    pub key: catalog::ScopedPropertyKey,
    /// Indexed literal.
    pub value: ExecIndexedEqualityValue,
}

/// Exact authoritative verification of a unique node owner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecNodeAuthoritativeVerificationPlan {
    /// Property expected on the authoritative node.
    pub key: catalog::ScopedPropertyKey,
    /// Value expected on the authoritative node.
    pub value: ExecIndexedEqualityValue,
}

/// Verified unique node count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecNodeUniqueCountPlan {
    /// Owner lookup.
    pub lookup: ExecNodeUniqueOwnerReadPlan,
    /// Authoritative graph verification.
    pub verification: ExecNodeAuthoritativeVerificationPlan,
    /// Normalized count window.
    pub window: ExecCountWindowPlan,
}

/// Exact verified node range driver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecNodeVerifiedRangeScanPlan {
    /// Logical index identity.
    pub index: catalog::NodeRangeIndexMeta,
    /// Property and selected physical direction.
    pub key: catalog::ScopedPropertyDirectionKey,
    /// Exact bounds.
    pub range: ir::IndexRange,
}

/// Exact verified edge range driver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecEdgeVerifiedRangeScanPlan {
    /// Logical index identity.
    pub index: catalog::EdgeRangeIndexMeta,
    /// Property and selected physical direction.
    pub key: catalog::ScopedPropertyDirectionKey,
    /// Exact bounds.
    pub range: ir::IndexRange,
}

/// Bitmap membership program for a node range count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecNodeRangeMembershipPlan {
    /// Accept every verified range match.
    All,
    /// Test bitmap filters in encoded order.
    BitmapFilters(ir::AtLeast<ExecNodeBitmapExpr, 1>),
}

/// Bitmap membership program for an edge range count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecEdgeRangeMembershipPlan {
    /// Accept every verified range match.
    All,
    /// Test bitmap filters in encoded order.
    BitmapFilters(ir::AtLeast<ExecEdgeBitmapExpr, 1>),
}

/// Streaming verified node range count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecNodeRangeCountPlan {
    /// Selected range driver.
    pub driver: ExecNodeVerifiedRangeScanPlan,
    /// Exact membership program.
    pub membership: ExecNodeRangeMembershipPlan,
    /// Normalized count window.
    pub window: ExecCountWindowPlan,
}

/// Streaming verified edge range count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecEdgeRangeCountPlan {
    /// Selected range driver.
    pub driver: ExecEdgeVerifiedRangeScanPlan,
    /// Exact membership program.
    pub membership: ExecEdgeRangeMembershipPlan,
    /// Normalized count window.
    pub window: ExecCountWindowPlan,
}

/// Exact authoritative node scan predicate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecNodeAuthoritativeScanPredicate {
    /// Property is absent or null on a node with the scoped label.
    NullEquality { key: catalog::ScopedPropertyKey },
    /// Evaluate a residual predicate authoritatively.
    Predicate(ir::PredicatePlan),
}

/// Exact authoritative edge scan predicate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecEdgeAuthoritativeScanPredicate {
    /// Property is absent or null on an edge with the scoped label.
    NullEquality { key: catalog::ScopedPropertyKey },
    /// Evaluate a residual predicate authoritatively.
    Predicate(ir::PredicatePlan),
}

/// Authoritative node scan count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecNodeScanCountPlan {
    /// Exact predicate.
    pub predicate: ExecNodeAuthoritativeScanPredicate,
    /// Normalized count window.
    pub window: ExecCountWindowPlan,
}

/// Authoritative edge scan count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecEdgeScanCountPlan {
    /// Exact predicate.
    pub predicate: ExecEdgeAuthoritativeScanPredicate,
    /// Normalized count window.
    pub window: ExecCountWindowPlan,
}

/// Exact runtime input source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecRuntimeInputPlan {
    /// Request parameter.
    Param(ir::NonEmptyString),
    /// Previously bound variable.
    Variable(ir::NonEmptyString),
}

/// Exact node vector-search source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecNodeVectorSearchCountPlan {
    /// Search index key.
    pub key: catalog::NodeSearchIndexKey,
    /// Search execution identity.
    pub index: ir::SearchIndexPlan,
    /// Query vector.
    pub query_vector: ir::VectorQueryInputPlan,
    /// Search result limit.
    pub k: ir::SearchLimitPlan,
    /// Normalized count window.
    pub window: ExecCountWindowPlan,
}

/// Exact edge vector-search source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecEdgeVectorSearchCountPlan {
    /// Search index key.
    pub key: catalog::EdgeSearchIndexKey,
    /// Search execution identity.
    pub index: ir::SearchIndexPlan,
    /// Query vector.
    pub query_vector: ir::VectorQueryInputPlan,
    /// Search result limit.
    pub k: ir::SearchLimitPlan,
    /// Normalized count window.
    pub window: ExecCountWindowPlan,
}

/// Exact node text-search source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecNodeTextSearchCountPlan {
    /// Search index key.
    pub key: catalog::NodeSearchIndexKey,
    /// Search execution identity.
    pub index: ir::SearchIndexPlan,
    /// Query text.
    pub query_text: ir::TextQueryInputPlan,
    /// Search result limit.
    pub k: ir::SearchLimitPlan,
    /// Normalized count window.
    pub window: ExecCountWindowPlan,
}

/// Exact edge text-search source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecEdgeTextSearchCountPlan {
    /// Search index key.
    pub key: catalog::EdgeSearchIndexKey,
    /// Search execution identity.
    pub index: ir::SearchIndexPlan,
    /// Query text.
    pub query_text: ir::TextQueryInputPlan,
    /// Search result limit.
    pub k: ir::SearchLimitPlan,
    /// Normalized count window.
    pub window: ExecCountWindowPlan,
}

/// Explicit exception permitting runtime equality classification for a node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecNodeDynamicEqualityCountPlan {
    /// Catalog index metadata, including node uniqueness.
    pub index: catalog::NodeEqualityIndexMeta,
    /// Indexed property.
    pub key: catalog::ScopedPropertyKey,
    /// Genuinely late-bound parameter.
    pub param: ir::NonEmptyString,
    /// Normalized count window.
    pub window: ExecCountWindowPlan,
}

/// Explicit exception permitting runtime equality classification for an edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecEdgeDynamicEqualityCountPlan {
    /// Catalog index metadata.
    pub index: catalog::EdgeEqualityIndexMeta,
    /// Indexed property.
    pub key: catalog::ScopedPropertyKey,
    /// Genuinely late-bound parameter.
    pub param: ir::NonEmptyString,
    /// Normalized count window.
    pub window: ExecCountWindowPlan,
}

/// Explicit row-distinct algorithm used by a count cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecCountDistinctPlan {
    /// Hash-based row identity set.
    HashRows,
    /// Adjacent deduplication over planner-proven identity ordering.
    OrderedRows,
}

/// Recursive, exactly ordered row cursor used when count still needs identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecCountCursorPlan {
    /// Rows supplied by the count step dependency.
    InputRows,
    /// Node equality bitmap IDs.
    NodeBitmap(ExecNodeBitmapExpr),
    /// Edge equality bitmap IDs.
    EdgeBitmap(ExecEdgeBitmapExpr),
    /// Verified unique node owner.
    NodeUnique {
        /// Owner lookup.
        lookup: ExecNodeUniqueOwnerReadPlan,
        /// Authoritative verification.
        verification: ExecNodeAuthoritativeVerificationPlan,
    },
    /// Verified node range IDs.
    NodeRange(ExecNodeVerifiedRangeScanPlan),
    /// Verified edge range IDs.
    EdgeRange(ExecEdgeVerifiedRangeScanPlan),
    /// Authoritative node predicate scan.
    NodeAuthoritativeScan(ExecNodeAuthoritativeScanPredicate),
    /// Authoritative edge predicate scan.
    EdgeAuthoritativeScan(ExecEdgeAuthoritativeScanPredicate),
    /// Verified node point reads.
    NodePointReads(ir::ElementIds),
    /// Verified edge point reads.
    EdgePointReads(ir::ElementIds),
    /// Node runtime input.
    NodeRuntimeInput(ExecRuntimeInputPlan),
    /// Edge runtime input.
    EdgeRuntimeInput(ExecRuntimeInputPlan),
    /// Full node scan.
    NodeFullScan,
    /// Full edge scan.
    EdgeFullScan,
    /// Node label bitmap.
    NodeLabelBitmap(ir::NonEmptyString),
    /// Edge label bitmap.
    EdgeLabelBitmap(ir::NonEmptyString),
    /// Node vector-search rows.
    NodeVectorSearch {
        /// Search key.
        key: catalog::NodeSearchIndexKey,
        /// Search execution identity.
        index: ir::SearchIndexPlan,
        /// Query vector.
        query_vector: ir::VectorQueryInputPlan,
        /// Result limit.
        k: ir::SearchLimitPlan,
    },
    /// Edge vector-search rows.
    EdgeVectorSearch {
        /// Search key.
        key: catalog::EdgeSearchIndexKey,
        /// Search execution identity.
        index: ir::SearchIndexPlan,
        /// Query vector.
        query_vector: ir::VectorQueryInputPlan,
        /// Result limit.
        k: ir::SearchLimitPlan,
    },
    /// Node text-search rows.
    NodeTextSearch {
        /// Search key.
        key: catalog::NodeSearchIndexKey,
        /// Search execution identity.
        index: ir::SearchIndexPlan,
        /// Query text.
        query_text: ir::TextQueryInputPlan,
        /// Result limit.
        k: ir::SearchLimitPlan,
    },
    /// Edge text-search rows.
    EdgeTextSearch {
        /// Search key.
        key: catalog::EdgeSearchIndexKey,
        /// Search execution identity.
        index: ir::SearchIndexPlan,
        /// Query text.
        query_text: ir::TextQueryInputPlan,
        /// Result limit.
        k: ir::SearchLimitPlan,
    },
    /// Explicit node runtime equality dispatch exception.
    NodeDynamicEquality {
        /// Catalog index metadata.
        index: catalog::NodeEqualityIndexMeta,
        /// Indexed property.
        key: catalog::ScopedPropertyKey,
        /// Genuinely late-bound parameter.
        param: ir::NonEmptyString,
    },
    /// Explicit edge runtime equality dispatch exception.
    EdgeDynamicEquality {
        /// Catalog index metadata.
        index: catalog::EdgeEqualityIndexMeta,
        /// Indexed property.
        key: catalog::ScopedPropertyKey,
        /// Genuinely late-bound parameter.
        param: ir::NonEmptyString,
    },
    /// Materialized row union in encoded child order.
    Union {
        /// First child.
        driver: Box<Self>,
        /// Remaining children.
        rest: ir::AtLeast<Self, 1>,
    },
    /// Materialized row intersection in encoded child order.
    Intersect {
        /// First child.
        driver: Box<Self>,
        /// Remaining children.
        rest: ir::AtLeast<Self, 1>,
    },
    /// Authoritative predicate filter.
    Filter {
        /// Input cursor.
        input: Box<Self>,
        /// Predicate.
        predicate: ir::PredicatePlan,
    },
    /// Positioned canonical window that could not cross a semantic barrier.
    Window {
        /// Input cursor.
        input: Box<Self>,
        /// Window.
        window: ExecCountWindowPlan,
    },
    /// Required ordering that could not be proven irrelevant.
    Order {
        /// Input cursor.
        input: Box<Self>,
        /// Exact order plan.
        plan: ir::OrderPlan,
    },
    /// Graph expansion.
    Expand {
        /// Input cursor.
        input: Box<Self>,
        /// Exact expansion.
        plan: ir::ExpandPlan,
    },
    /// Restricted vector ranking.
    VectorSearch {
        /// Input cursor.
        input: Box<Self>,
        /// Exact restricted search.
        plan: Box<ir::RestrictedVectorSearchPlan>,
    },
    /// Restricted text ranking.
    TextSearch {
        /// Input cursor.
        input: Box<Self>,
        /// Exact restricted search.
        plan: Box<ir::RestrictedTextSearchPlan>,
    },
    /// Side-effect-free stream variable operation.
    Variable {
        /// Input cursor.
        input: Box<Self>,
        /// Exact variable operation.
        op: crate::logical::PureStreamVariableOp,
    },
    /// Explicit distinct algorithm.
    Distinct {
        /// Input cursor.
        input: Box<Self>,
        /// Selected algorithm.
        plan: ExecCountDistinctPlan,
    },
}

/// Streaming count over an exact recursive cursor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecCountStreamPlan {
    /// Exact cursor program.
    pub cursor: ExecCountCursorPlan,
    /// Final normalized count window.
    pub window: ExecCountWindowPlan,
}

/// Fully physical count operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecCountPlan {
    /// Final constant result. Windows have already been proven irrelevant or folded.
    Constant(usize),
    /// Node equality bitmap cardinality.
    NodeBitmap(ExecNodeBitmapCountPlan),
    /// Edge equality bitmap cardinality.
    EdgeBitmap(ExecEdgeBitmapCountPlan),
    /// Verified unique node owner cardinality.
    NodeUnique(ExecNodeUniqueCountPlan),
    /// Streaming verified node range cardinality.
    NodeRange(ExecNodeRangeCountPlan),
    /// Streaming verified edge range cardinality.
    EdgeRange(ExecEdgeRangeCountPlan),
    /// Authoritative node scan cardinality.
    NodeAuthoritativeScan(ExecNodeScanCountPlan),
    /// Authoritative edge scan cardinality.
    EdgeAuthoritativeScan(ExecEdgeScanCountPlan),
    /// Verified static node point reads.
    NodePointReads {
        /// Unique IDs in planner order.
        ids: ir::ElementIds,
        /// Normalized count window.
        window: ExecCountWindowPlan,
    },
    /// Verified static edge point reads.
    EdgePointReads {
        /// Unique IDs in planner order.
        ids: ir::ElementIds,
        /// Normalized count window.
        window: ExecCountWindowPlan,
    },
    /// Node IDs supplied at runtime.
    NodeRuntimeInput {
        /// Exact runtime source.
        input: ExecRuntimeInputPlan,
        /// Normalized count window.
        window: ExecCountWindowPlan,
    },
    /// Edge IDs supplied at runtime.
    EdgeRuntimeInput {
        /// Exact runtime source.
        input: ExecRuntimeInputPlan,
        /// Normalized count window.
        window: ExecCountWindowPlan,
    },
    /// Full authoritative node scan.
    NodeFullScan { window: ExecCountWindowPlan },
    /// Full authoritative edge scan.
    EdgeFullScan { window: ExecCountWindowPlan },
    /// Node label bitmap cardinality.
    NodeLabelBitmap {
        /// Label.
        label: ir::NonEmptyString,
        /// Normalized count window.
        window: ExecCountWindowPlan,
    },
    /// Edge label bitmap cardinality.
    EdgeLabelBitmap {
        /// Label.
        label: ir::NonEmptyString,
        /// Normalized count window.
        window: ExecCountWindowPlan,
    },
    /// Node vector search cardinality.
    NodeVectorSearch(ExecNodeVectorSearchCountPlan),
    /// Edge vector search cardinality.
    EdgeVectorSearch(ExecEdgeVectorSearchCountPlan),
    /// Node text search cardinality.
    NodeTextSearch(ExecNodeTextSearchCountPlan),
    /// Edge text search cardinality.
    EdgeTextSearch(ExecEdgeTextSearchCountPlan),
    /// Explicit node runtime equality dispatch exception.
    NodeDynamicEquality(ExecNodeDynamicEqualityCountPlan),
    /// Explicit edge runtime equality dispatch exception.
    EdgeDynamicEquality(ExecEdgeDynamicEqualityCountPlan),
    /// Recursive exact cursor fallback for identity-sensitive pipelines and sets.
    Stream(ExecCountStreamPlan),
    /// Count rows emitted by a preserved effect barrier.
    InputRows { window: ExecCountWindowPlan },
    /// Count scalar items emitted by a nested scalar terminal.
    InputScalars { window: ExecCountWindowPlan },
}

#[cfg(test)]
mod tests {
    use helix_ast::value::PropertyValue;

    use super::*;

    fn name(value: &str) -> ir::NonEmptyString {
        ir::NonEmptyString::new(value).unwrap()
    }

    fn literal(value: usize) -> ExecUsizeExpr {
        ExecUsizeExpr::Literal(value)
    }

    #[test]
    fn node_index_wrappers_reject_the_wrong_uniqueness() {
        let non_unique = catalog::NodeEqualityIndexMeta::new(name("non_unique"));
        let unique = catalog::NodeEqualityIndexMeta::new(name("unique"))
            .with_uniqueness(catalog::IndexUniqueness::Unique);

        assert!(ExecNodeNonUniqueEqualityIndex::try_from(non_unique.clone()).is_ok());
        assert_eq!(
            ExecNodeUniqueEqualityIndex::try_from(non_unique),
            Err(ExecEqualityContractError::ExpectedNodeUnique)
        );
        assert!(ExecNodeUniqueEqualityIndex::try_from(unique.clone()).is_ok());
        assert_eq!(
            ExecNodeNonUniqueEqualityIndex::try_from(unique),
            Err(ExecEqualityContractError::ExpectedNodeNonUnique)
        );
    }

    #[test]
    fn node_index_wrapper_deserialization_revalidates_uniqueness() {
        let unique = catalog::NodeEqualityIndexMeta::new(name("unique"))
            .with_uniqueness(catalog::IndexUniqueness::Unique);
        let json = serde_json::to_string(&unique).unwrap();

        assert!(serde_json::from_str::<ExecNodeUniqueEqualityIndex>(&json).is_ok());
        assert!(serde_json::from_str::<ExecNodeNonUniqueEqualityIndex>(&json).is_err());
    }

    #[test]
    fn indexed_value_rejects_null_and_non_reflexive_literals() {
        let indexed = ir::SecondaryIndexLiteral::new(PropertyValue::from("alice")).unwrap();
        let null = ir::SecondaryIndexLiteral::new(PropertyValue::Null).unwrap();
        let nan = ir::SecondaryIndexLiteral::new(PropertyValue::F64(f64::NAN)).unwrap();

        assert!(ExecIndexedEqualityValue::try_from(indexed).is_ok());
        assert_eq!(
            ExecIndexedEqualityValue::try_from(null),
            Err(ExecEqualityContractError::ExpectedIndexedValue)
        );
        assert_eq!(
            ExecIndexedEqualityValue::try_from(nan),
            Err(ExecEqualityContractError::ExpectedIndexedValue)
        );
    }

    #[test]
    fn indexed_value_deserialization_revalidates_semantics() {
        let null = ir::SecondaryIndexLiteral::new(PropertyValue::Null).unwrap();
        let json = serde_json::to_string(&null).unwrap();

        assert!(serde_json::from_str::<ExecIndexedEqualityValue>(&json).is_err());
    }

    #[test]
    fn expression_constructors_fold_every_arithmetic_family() {
        assert_eq!(ExecUsizeExpr::min(literal(4), literal(2)), literal(2));
        assert_eq!(
            ExecUsizeExpr::min(literal(0), ExecUsizeExpr::Param(name("n"))),
            literal(0)
        );
        assert_eq!(
            ExecUsizeExpr::saturating_add(literal(usize::MAX), literal(1)),
            literal(usize::MAX)
        );
        assert_eq!(
            ExecUsizeExpr::saturating_add(literal(0), literal(7)),
            literal(7)
        );
        assert_eq!(
            ExecUsizeExpr::saturating_sub(literal(2), literal(7)),
            literal(0)
        );
        assert_eq!(
            ExecUsizeExpr::saturating_sub(literal(7), literal(0)),
            literal(7)
        );
        assert_eq!(
            ExecUsizeExpr::saturating_sub(literal(7), literal(7)),
            literal(0)
        );
    }

    #[test]
    fn expressions_evaluate_parameters_and_saturating_operations() {
        let expression = ExecUsizeExpr::saturating_sub(
            ExecUsizeExpr::saturating_add(ExecUsizeExpr::Param(name("n")), literal(usize::MAX)),
            ExecUsizeExpr::min(ExecUsizeExpr::Param(name("m")), literal(8)),
        );
        let mut resolve = |name: &ir::NonEmptyString| -> Result<usize, ()> {
            Ok(match name.as_ref() {
                "n" => 10,
                "m" => 3,
                _ => return Err(()),
            })
        };

        assert_eq!(expression.evaluate(&mut resolve), Ok(usize::MAX - 3));
        assert_eq!(expression.node_count(), 7);
    }

    #[test]
    fn windows_compose_with_sequential_slice_semantics() {
        let window = ExecCountWindowPlan::identity()
            .then_skip(literal(100))
            .then_limit(literal(10))
            .then_skip(literal(3))
            .then_range(literal(2), literal(5));
        let mut resolve = |_name: &ir::NonEmptyString| -> Result<usize, ()> { Err(()) };

        assert_eq!(window.skip, literal(105));
        assert_eq!(window.take, ExecCountTake::AtMost(literal(3)));
        assert_eq!(window.apply(200, &mut resolve), Ok(3));
        assert_eq!(window.apply(104, &mut resolve), Ok(0));
    }

    #[test]
    fn unbounded_skip_and_limit_zero_are_canonical() {
        let skipped = ExecCountWindowPlan::identity().then_skip(literal(4));
        let limited = skipped.then_limit(literal(0));
        let mut resolve = |_name: &ir::NonEmptyString| -> Result<usize, ()> { Err(()) };

        assert_eq!(limited.skip, literal(4));
        assert_eq!(limited.take, ExecCountTake::AtMost(literal(0)));
        assert_eq!(limited.apply(usize::MAX, &mut resolve), Ok(0));
    }

    #[test]
    fn count_contract_round_trips() {
        let plan = ExecCountPlan::InputRows {
            window: ExecCountWindowPlan::identity().then_limit(ExecUsizeExpr::Param(name("limit"))),
        };
        let json = serde_json::to_string(&plan).unwrap();

        assert_eq!(serde_json::from_str::<ExecCountPlan>(&json).unwrap(), plan);
    }
}
