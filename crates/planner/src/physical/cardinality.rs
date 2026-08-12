//! Payload-carrying physical cardinality programs.

use serde::{Deserialize, Serialize};

use crate::exec;

/// Costing and diagnostics family for an exact cardinality program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhysicalCardinality {
    /// One non-unique equality bitmap read.
    BitmapPoint,
    /// Same-index bitmap multi-get and union.
    BitmapBatchUnion,
    /// Explicit bitmap union.
    BitmapUnion,
    /// Explicit bitmap intersection.
    BitmapIntersection,
    /// Unique-owner lookup plus authoritative verification.
    UniqueVerified,
    /// Streaming range scan plus authoritative verification.
    VerifiedRange,
    /// Authoritative graph scan.
    AuthoritativeScan,
    /// Compile-time constant.
    Constant,
    /// Verified point reads.
    VerifiedPointReads,
    /// Runtime parameter or variable source.
    RuntimeInput,
    /// Full authoritative element scan.
    FullScan,
    /// Label bitmap cardinality.
    LabelBitmap,
    /// Unrestricted vector search.
    VectorSearch,
    /// Unrestricted text search.
    TextSearch,
    /// Explicit materialized set union.
    SetUnion,
    /// Explicit materialized set intersection.
    SetIntersection,
    /// Authoritative predicate filter cursor.
    FilterStream,
    /// Expansion cursor.
    ExpandStream,
    /// Explicit distinct cursor.
    DistinctStream,
    /// Restricted vector search cursor.
    RestrictedVectorStream,
    /// Restricted text search cursor.
    RestrictedTextStream,
    /// Variable cursor.
    VariableStream,
    /// Required ordered cursor.
    OrderedStream,
    /// Rows supplied by an executable dependency.
    InputRows,
    /// Scalar items supplied by an executable dependency.
    InputScalars,
    /// Explicit late-bound equality dispatch exception.
    DynamicEquality,
}

/// Complete physical count payload selected by the optimizer.
///
/// The family is derived from the executable ADT, so a diagnostic algorithm
/// tag cannot disagree with the program selected for lowering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PhysicalCountPlan(exec::ExecCountPlan);

impl PhysicalCountPlan {
    /// Build a physical plan from its complete executable payload.
    pub const fn new(plan: exec::ExecCountPlan) -> Self {
        Self(plan)
    }

    /// Exact executable payload.
    pub const fn executable(&self) -> &exec::ExecCountPlan {
        &self.0
    }

    /// Consume this physical wrapper.
    pub fn into_executable(self) -> exec::ExecCountPlan {
        self.0
    }

    /// Root physical algorithm family.
    pub fn family(&self) -> PhysicalCardinality {
        family(&self.0)
    }
}

fn family(plan: &exec::ExecCountPlan) -> PhysicalCardinality {
    match plan {
        exec::ExecCountPlan::Constant(_) => PhysicalCardinality::Constant,
        exec::ExecCountPlan::NodeBitmap(plan) => node_bitmap_family(&plan.bitmap),
        exec::ExecCountPlan::EdgeBitmap(plan) => edge_bitmap_family(&plan.bitmap),
        exec::ExecCountPlan::NodeUnique(_) => PhysicalCardinality::UniqueVerified,
        exec::ExecCountPlan::NodeRange(_) | exec::ExecCountPlan::EdgeRange(_) => {
            PhysicalCardinality::VerifiedRange
        }
        exec::ExecCountPlan::NodeAuthoritativeScan(_)
        | exec::ExecCountPlan::EdgeAuthoritativeScan(_) => PhysicalCardinality::AuthoritativeScan,
        exec::ExecCountPlan::NodePointReads { .. } | exec::ExecCountPlan::EdgePointReads { .. } => {
            PhysicalCardinality::VerifiedPointReads
        }
        exec::ExecCountPlan::NodeRuntimeInput { .. }
        | exec::ExecCountPlan::EdgeRuntimeInput { .. }
        | exec::ExecCountPlan::RuntimeInput { .. } => PhysicalCardinality::RuntimeInput,
        exec::ExecCountPlan::NodeFullScan { .. } | exec::ExecCountPlan::EdgeFullScan { .. } => {
            PhysicalCardinality::FullScan
        }
        exec::ExecCountPlan::NodeLabelBitmap { .. }
        | exec::ExecCountPlan::EdgeLabelBitmap { .. } => PhysicalCardinality::LabelBitmap,
        exec::ExecCountPlan::NodeVectorSearch(_) | exec::ExecCountPlan::EdgeVectorSearch(_) => {
            PhysicalCardinality::VectorSearch
        }
        exec::ExecCountPlan::NodeTextSearch(_) | exec::ExecCountPlan::EdgeTextSearch(_) => {
            PhysicalCardinality::TextSearch
        }
        exec::ExecCountPlan::NodeDynamicEquality(_)
        | exec::ExecCountPlan::EdgeDynamicEquality(_) => PhysicalCardinality::DynamicEquality,
        exec::ExecCountPlan::Stream(plan) => cursor_family(&plan.cursor),
        exec::ExecCountPlan::InputRows { .. } => PhysicalCardinality::InputRows,
        exec::ExecCountPlan::InputScalars { .. } => PhysicalCardinality::InputScalars,
    }
}

fn node_bitmap_family(bitmap: &exec::ExecNodeBitmapExpr) -> PhysicalCardinality {
    match bitmap {
        exec::ExecNodeBitmapExpr::PointRead { .. } => PhysicalCardinality::BitmapPoint,
        exec::ExecNodeBitmapExpr::BatchedUnionRead { .. } => PhysicalCardinality::BitmapBatchUnion,
        exec::ExecNodeBitmapExpr::Union { .. } => PhysicalCardinality::BitmapUnion,
        exec::ExecNodeBitmapExpr::Intersect { .. } => PhysicalCardinality::BitmapIntersection,
    }
}

fn edge_bitmap_family(bitmap: &exec::ExecEdgeBitmapExpr) -> PhysicalCardinality {
    match bitmap {
        exec::ExecEdgeBitmapExpr::PointRead { .. } => PhysicalCardinality::BitmapPoint,
        exec::ExecEdgeBitmapExpr::BatchedUnionRead { .. } => PhysicalCardinality::BitmapBatchUnion,
        exec::ExecEdgeBitmapExpr::Union { .. } => PhysicalCardinality::BitmapUnion,
        exec::ExecEdgeBitmapExpr::Intersect { .. } => PhysicalCardinality::BitmapIntersection,
    }
}

fn cursor_family(cursor: &exec::ExecCountCursorPlan) -> PhysicalCardinality {
    match cursor {
        exec::ExecCountCursorPlan::EmptyRows => PhysicalCardinality::Constant,
        exec::ExecCountCursorPlan::InputRows => PhysicalCardinality::InputRows,
        exec::ExecCountCursorPlan::NodeBitmap(bitmap) => node_bitmap_family(bitmap),
        exec::ExecCountCursorPlan::EdgeBitmap(bitmap) => edge_bitmap_family(bitmap),
        exec::ExecCountCursorPlan::NodeUnique { .. } => PhysicalCardinality::UniqueVerified,
        exec::ExecCountCursorPlan::NodeRange(_) | exec::ExecCountCursorPlan::EdgeRange(_) => {
            PhysicalCardinality::VerifiedRange
        }
        exec::ExecCountCursorPlan::NodeAuthoritativeScan(_)
        | exec::ExecCountCursorPlan::EdgeAuthoritativeScan(_) => {
            PhysicalCardinality::AuthoritativeScan
        }
        exec::ExecCountCursorPlan::NodePointReads(_)
        | exec::ExecCountCursorPlan::EdgePointReads(_) => PhysicalCardinality::VerifiedPointReads,
        exec::ExecCountCursorPlan::NodeRuntimeInput(_)
        | exec::ExecCountCursorPlan::EdgeRuntimeInput(_)
        | exec::ExecCountCursorPlan::RuntimeInput(_) => PhysicalCardinality::RuntimeInput,
        exec::ExecCountCursorPlan::NodeFullScan | exec::ExecCountCursorPlan::EdgeFullScan => {
            PhysicalCardinality::FullScan
        }
        exec::ExecCountCursorPlan::NodeLabelBitmap(_)
        | exec::ExecCountCursorPlan::EdgeLabelBitmap(_) => PhysicalCardinality::LabelBitmap,
        exec::ExecCountCursorPlan::NodeVectorSearch { .. }
        | exec::ExecCountCursorPlan::EdgeVectorSearch { .. } => PhysicalCardinality::VectorSearch,
        exec::ExecCountCursorPlan::NodeTextSearch { .. }
        | exec::ExecCountCursorPlan::EdgeTextSearch { .. } => PhysicalCardinality::TextSearch,
        exec::ExecCountCursorPlan::NodeDynamicEquality { .. }
        | exec::ExecCountCursorPlan::EdgeDynamicEquality { .. } => {
            PhysicalCardinality::DynamicEquality
        }
        exec::ExecCountCursorPlan::Union { .. } => PhysicalCardinality::SetUnion,
        exec::ExecCountCursorPlan::Intersect { .. } => PhysicalCardinality::SetIntersection,
        exec::ExecCountCursorPlan::Filter { .. } => PhysicalCardinality::FilterStream,
        exec::ExecCountCursorPlan::Window { input, .. } => cursor_family(input),
        exec::ExecCountCursorPlan::Order { .. } => PhysicalCardinality::OrderedStream,
        exec::ExecCountCursorPlan::Expand { .. } => PhysicalCardinality::ExpandStream,
        exec::ExecCountCursorPlan::VectorSearch { .. } => {
            PhysicalCardinality::RestrictedVectorStream
        }
        exec::ExecCountCursorPlan::TextSearch { .. } => PhysicalCardinality::RestrictedTextStream,
        exec::ExecCountCursorPlan::Variable { .. } => PhysicalCardinality::VariableStream,
        exec::ExecCountCursorPlan::Distinct { .. } => PhysicalCardinality::DistinctStream,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_is_derived_from_payload() {
        let input = PhysicalCountPlan::new(exec::ExecCountPlan::InputRows {
            window: exec::ExecCountWindowPlan::identity(),
        });
        let constant = PhysicalCountPlan::new(exec::ExecCountPlan::Constant(0));

        assert_eq!(input.family(), PhysicalCardinality::InputRows);
        assert_eq!(constant.family(), PhysicalCardinality::Constant);
    }
}
