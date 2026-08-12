//! Access delivered-ordering inference.

use crate::{catalog, ir, properties};

pub(super) fn range_ordering_from_node_access(
    plan: &ir::NodeAccessPlan,
) -> properties::DeliveredOrdering {
    match plan {
        ir::NodeAccessPlan::RangeIndex { key, .. } => range_ordering(key),
        ir::NodeAccessPlan::Intersect(children)
            if children
                .iter()
                .all(|child| node_is_secondary(child.as_ref())) =>
        {
            children
                .iter()
                .map(|child| range_ordering_from_node_access(child.as_ref()))
                .find(|ordering| matches!(ordering, properties::DeliveredOrdering::ByKeys(_)))
                .unwrap_or(properties::DeliveredOrdering::Unordered)
        }
        _ => properties::DeliveredOrdering::Unordered,
    }
}

pub(super) fn range_ordering_from_edge_access(
    plan: &ir::EdgeAccessPlan,
) -> properties::DeliveredOrdering {
    match plan {
        ir::EdgeAccessPlan::RangeIndex { key, .. } => range_ordering(key),
        ir::EdgeAccessPlan::Intersect(children)
            if children
                .iter()
                .all(|child| edge_is_secondary(child.as_ref())) =>
        {
            children
                .iter()
                .map(|child| range_ordering_from_edge_access(child.as_ref()))
                .find(|ordering| matches!(ordering, properties::DeliveredOrdering::ByKeys(_)))
                .unwrap_or(properties::DeliveredOrdering::Unordered)
        }
        _ => properties::DeliveredOrdering::Unordered,
    }
}

fn node_is_secondary(plan: &ir::NodeAccessPlan) -> bool {
    match plan {
        ir::NodeAccessPlan::Empty
        | ir::NodeAccessPlan::EqualityIndex { .. }
        | ir::NodeAccessPlan::RangeIndex { .. } => true,
        ir::NodeAccessPlan::Intersect(children) | ir::NodeAccessPlan::Union(children) => children
            .iter()
            .all(|child| node_is_secondary(child.as_ref())),
        ir::NodeAccessPlan::PointIds { .. }
        | ir::NodeAccessPlan::FromParam { .. }
        | ir::NodeAccessPlan::FromVar { .. }
        | ir::NodeAccessPlan::AllScan
        | ir::NodeAccessPlan::LabelScan { .. }
        | ir::NodeAccessPlan::VectorSearch { .. }
        | ir::NodeAccessPlan::TextSearch { .. }
        | ir::NodeAccessPlan::ScanThenFilter { .. } => false,
    }
}

fn edge_is_secondary(plan: &ir::EdgeAccessPlan) -> bool {
    match plan {
        ir::EdgeAccessPlan::Empty
        | ir::EdgeAccessPlan::EqualityIndex { .. }
        | ir::EdgeAccessPlan::RangeIndex { .. } => true,
        ir::EdgeAccessPlan::Intersect(children) | ir::EdgeAccessPlan::Union(children) => children
            .iter()
            .all(|child| edge_is_secondary(child.as_ref())),
        ir::EdgeAccessPlan::PointIds { .. }
        | ir::EdgeAccessPlan::FromParam { .. }
        | ir::EdgeAccessPlan::FromVar { .. }
        | ir::EdgeAccessPlan::AllScan
        | ir::EdgeAccessPlan::LabelScan { .. }
        | ir::EdgeAccessPlan::VectorSearch { .. }
        | ir::EdgeAccessPlan::TextSearch { .. }
        | ir::EdgeAccessPlan::ScanThenFilter { .. } => false,
    }
}

fn range_ordering(key: &catalog::ScopedPropertyDirectionKey) -> properties::DeliveredOrdering {
    let order = match key.direction {
        helix_ast::index::RangeIndexDirection::Asc => helix_ast::traversal::Order::Asc,
        helix_ast::index::RangeIndexDirection::Desc => helix_ast::traversal::Order::Desc,
    };
    properties::DeliveredOrdering::ByKeys(
        ir::OrderKey {
            property: key.property.clone(),
            order,
        }
        .into(),
    )
}
