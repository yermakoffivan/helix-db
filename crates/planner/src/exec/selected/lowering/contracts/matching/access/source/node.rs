//! Selected node access-source matching.

use crate::{ir, physical};

use super::{SelectedAccessShapeMatch, SelectedAccessShapeMismatch};

pub(in crate::exec::selected::lowering) fn selected_node_access_matches(
    plan: &ir::NodeAccessPlan,
    access: &physical::PhysicalAccess,
) -> bool {
    selected_node_access_match(plan, access).is_matched()
}

pub(super) fn selected_node_access_match(
    plan: &ir::NodeAccessPlan,
    access: &physical::PhysicalAccess,
) -> SelectedAccessShapeMatch {
    match (plan, access) {
        (ir::NodeAccessPlan::Empty, physical::PhysicalAccess::Empty)
        | (
            ir::NodeAccessPlan::FromParam { .. } | ir::NodeAccessPlan::FromVar { .. },
            physical::PhysicalAccess::RuntimeInput,
        )
        | (ir::NodeAccessPlan::LabelScan { .. }, physical::PhysicalAccess::LabelScan)
        | (ir::NodeAccessPlan::RangeIndex { .. }, physical::PhysicalAccess::RangeIndex)
        | (ir::NodeAccessPlan::VectorSearch { .. }, physical::PhysicalAccess::VectorSearch)
        | (ir::NodeAccessPlan::TextSearch { .. }, physical::PhysicalAccess::TextSearch)
        | (ir::NodeAccessPlan::Intersect(_), physical::PhysicalAccess::SetIntersection) => {
            SelectedAccessShapeMatch::Matched
        }
        (ir::NodeAccessPlan::Union(children), physical::PhysicalAccess::BitmapBatchUnion)
            if node_union_is_literal_batch(children) =>
        {
            SelectedAccessShapeMatch::Matched
        }
        (ir::NodeAccessPlan::Union(children), physical::PhysicalAccess::SetUnion)
            if !node_union_is_literal_batch(children) =>
        {
            SelectedAccessShapeMatch::Matched
        }
        (ir::NodeAccessPlan::EqualityIndex { index, value, .. }, access)
            if node_equality_access_matches(index, value, access) =>
        {
            SelectedAccessShapeMatch::Matched
        }
        (
            ir::NodeAccessPlan::PointIds { .. },
            physical::PhysicalAccess::PointReads { .. } | physical::PhysicalAccess::Kv(_),
        )
        | (ir::NodeAccessPlan::AllScan, physical::PhysicalAccess::Kv(_)) => {
            SelectedAccessShapeMatch::Matched
        }
        (ir::NodeAccessPlan::ScanThenFilter { .. }, _) => SelectedAccessShapeMatch::NotMatched(
            SelectedAccessShapeMismatch::ResidualFilterRequiresPipeline,
        ),
        (
            ir::NodeAccessPlan::Empty
            | ir::NodeAccessPlan::FromParam { .. }
            | ir::NodeAccessPlan::FromVar { .. }
            | ir::NodeAccessPlan::AllScan
            | ir::NodeAccessPlan::PointIds { .. }
            | ir::NodeAccessPlan::LabelScan { .. }
            | ir::NodeAccessPlan::EqualityIndex { .. }
            | ir::NodeAccessPlan::RangeIndex { .. }
            | ir::NodeAccessPlan::VectorSearch { .. }
            | ir::NodeAccessPlan::TextSearch { .. }
            | ir::NodeAccessPlan::Intersect(_)
            | ir::NodeAccessPlan::Union(_),
            _,
        ) => SelectedAccessShapeMatch::NotMatched(
            SelectedAccessShapeMismatch::PhysicalAccessFamilyMismatch,
        ),
    }
}

fn node_union_is_literal_batch(children: &ir::AtLeast<ir::NodeAccessSourcePlan, 2>) -> bool {
    let Some(ir::NodeAccessPlan::EqualityIndex {
        index: first_index,
        key: first_key,
        value: first_value,
    }) = children.first().map(AsRef::as_ref)
    else {
        return false;
    };
    first_index.uniqueness == crate::catalog::IndexUniqueness::NonUnique
        && first_value.semantics() == ir::EqualityIndexValueSemantics::Indexed
        && children.iter().all(|child| {
            matches!(
                child.as_ref(),
                ir::NodeAccessPlan::EqualityIndex { index, key, value }
                    if index == first_index
                        && key == first_key
                        && value.semantics() == ir::EqualityIndexValueSemantics::Indexed
            )
        })
}

fn node_equality_access_matches(
    index: &crate::catalog::NodeEqualityIndexMeta,
    value: &ir::IndexValue,
    access: &physical::PhysicalAccess,
) -> bool {
    matches!(
        (value.semantics(), index.uniqueness, access),
        (
            ir::EqualityIndexValueSemantics::NonReflexive,
            _,
            physical::PhysicalAccess::Empty,
        ) | (
            ir::EqualityIndexValueSemantics::Indexed,
            crate::catalog::IndexUniqueness::NonUnique,
            physical::PhysicalAccess::EqualityBitmapPoint,
        ) | (
            ir::EqualityIndexValueSemantics::Indexed,
            crate::catalog::IndexUniqueness::Unique,
            physical::PhysicalAccess::EqualityUniqueVerified,
        ) | (
            ir::EqualityIndexValueSemantics::AuthoritativeNull,
            _,
            physical::PhysicalAccess::EqualityAuthoritativeScan,
        ) | (
            ir::EqualityIndexValueSemantics::RuntimeDependent,
            _,
            physical::PhysicalAccess::EqualityDynamic,
        )
    )
}
