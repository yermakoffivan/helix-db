//! Selected edge access-source matching.

use crate::{ir, physical};

use super::{SelectedAccessShapeMatch, SelectedAccessShapeMismatch};

pub(in crate::exec::selected::lowering) fn selected_edge_access_matches(
    plan: &ir::EdgeAccessPlan,
    access: &physical::PhysicalAccess,
) -> bool {
    selected_edge_access_match(plan, access).is_matched()
}

pub(super) fn selected_edge_access_match(
    plan: &ir::EdgeAccessPlan,
    access: &physical::PhysicalAccess,
) -> SelectedAccessShapeMatch {
    match (plan, access) {
        (ir::EdgeAccessPlan::Empty, physical::PhysicalAccess::Empty)
        | (
            ir::EdgeAccessPlan::FromParam { .. } | ir::EdgeAccessPlan::FromVar { .. },
            physical::PhysicalAccess::RuntimeInput,
        )
        | (ir::EdgeAccessPlan::LabelScan { .. }, physical::PhysicalAccess::LabelScan)
        | (ir::EdgeAccessPlan::RangeIndex { .. }, physical::PhysicalAccess::RangeIndex)
        | (ir::EdgeAccessPlan::VectorSearch { .. }, physical::PhysicalAccess::VectorSearch)
        | (ir::EdgeAccessPlan::TextSearch { .. }, physical::PhysicalAccess::TextSearch)
        | (ir::EdgeAccessPlan::Intersect(_), physical::PhysicalAccess::SetIntersection) => {
            SelectedAccessShapeMatch::Matched
        }
        (ir::EdgeAccessPlan::Union(children), physical::PhysicalAccess::BitmapBatchUnion)
            if edge_union_is_literal_batch(children) =>
        {
            SelectedAccessShapeMatch::Matched
        }
        (ir::EdgeAccessPlan::Union(children), physical::PhysicalAccess::SetUnion)
            if !edge_union_is_literal_batch(children) =>
        {
            SelectedAccessShapeMatch::Matched
        }
        (ir::EdgeAccessPlan::EqualityIndex { value, .. }, access)
            if edge_equality_access_matches(value, access) =>
        {
            SelectedAccessShapeMatch::Matched
        }
        (
            ir::EdgeAccessPlan::PointIds { .. },
            physical::PhysicalAccess::PointReads { .. } | physical::PhysicalAccess::Kv(_),
        )
        | (ir::EdgeAccessPlan::AllScan, physical::PhysicalAccess::Kv(_)) => {
            SelectedAccessShapeMatch::Matched
        }
        (ir::EdgeAccessPlan::ScanThenFilter { .. }, _) => SelectedAccessShapeMatch::NotMatched(
            SelectedAccessShapeMismatch::ResidualFilterRequiresPipeline,
        ),
        (
            ir::EdgeAccessPlan::Empty
            | ir::EdgeAccessPlan::FromParam { .. }
            | ir::EdgeAccessPlan::FromVar { .. }
            | ir::EdgeAccessPlan::AllScan
            | ir::EdgeAccessPlan::PointIds { .. }
            | ir::EdgeAccessPlan::LabelScan { .. }
            | ir::EdgeAccessPlan::EqualityIndex { .. }
            | ir::EdgeAccessPlan::RangeIndex { .. }
            | ir::EdgeAccessPlan::VectorSearch { .. }
            | ir::EdgeAccessPlan::TextSearch { .. }
            | ir::EdgeAccessPlan::Intersect(_)
            | ir::EdgeAccessPlan::Union(_),
            _,
        ) => SelectedAccessShapeMatch::NotMatched(
            SelectedAccessShapeMismatch::PhysicalAccessFamilyMismatch,
        ),
    }
}

fn edge_union_is_literal_batch(children: &ir::AtLeast<ir::EdgeAccessSourcePlan, 2>) -> bool {
    let Some(ir::EdgeAccessPlan::EqualityIndex {
        index: first_index,
        key: first_key,
        value: first_value,
    }) = children.first().map(AsRef::as_ref)
    else {
        return false;
    };
    first_value.semantics() == ir::EqualityIndexValueSemantics::Indexed
        && children.iter().all(|child| {
            matches!(
                child.as_ref(),
                ir::EdgeAccessPlan::EqualityIndex { index, key, value }
                    if index == first_index
                        && key == first_key
                        && value.semantics() == ir::EqualityIndexValueSemantics::Indexed
            )
        })
}

fn edge_equality_access_matches(value: &ir::IndexValue, access: &physical::PhysicalAccess) -> bool {
    matches!(
        (value.semantics(), access),
        (
            ir::EqualityIndexValueSemantics::NonReflexive,
            physical::PhysicalAccess::Empty,
        ) | (
            ir::EqualityIndexValueSemantics::Indexed,
            physical::PhysicalAccess::EqualityBitmapPoint,
        ) | (
            ir::EqualityIndexValueSemantics::AuthoritativeNull,
            physical::PhysicalAccess::EqualityAuthoritativeScan,
        ) | (
            ir::EqualityIndexValueSemantics::RuntimeDependent,
            physical::PhysicalAccess::EqualityDynamic,
        )
    )
}
