//! Access-order scheduler predicates.

use super::super::{AccessOrder, AccessPath};
use crate::{catalog, ir};

pub(in crate::logical::access::unary) fn order_has_order_elision_candidate(
    order: &AccessOrder,
) -> bool {
    order
        .access()
        .hard_cardinality_upper_bound()
        .is_some_and(|upper| upper <= 1)
        || access_order_satisfaction_candidate(order.access(), order.ordering())
}

pub(in crate::logical::access::unary) fn order_has_range_direction_candidate(
    order: &AccessOrder,
) -> bool {
    let [required] = order.ordering().as_ref() else {
        return false;
    };
    access_range_direction_candidate(order.access(), required)
}

fn access_range_direction_candidate(access: &AccessPath, required: &ir::OrderKey) -> bool {
    match access {
        AccessPath::Node(path) => match path.source().as_ref() {
            ir::NodeAccessPlan::RangeIndex { key, .. } => range_direction_candidate(key, required),
            _ => false,
        },
        AccessPath::Edge(path) => match path.source().as_ref() {
            ir::EdgeAccessPlan::RangeIndex { key, .. } => range_direction_candidate(key, required),
            _ => false,
        },
    }
}

fn access_order_satisfaction_candidate(access: &AccessPath, ordering: &ir::OrderKeys) -> bool {
    let [required] = ordering.as_ref() else {
        return false;
    };
    match access {
        AccessPath::Node(path) => match path.source().as_ref() {
            ir::NodeAccessPlan::RangeIndex { key, .. } => range_satisfies_order(key, required),
            ir::NodeAccessPlan::Intersect(children)
                if children
                    .iter()
                    .all(|child| node_is_secondary(child.as_ref())) =>
            {
                children.iter().any(|child| {
                    matches!(
                        child.as_ref(),
                        ir::NodeAccessPlan::RangeIndex { key, .. }
                            if range_satisfies_order(key, required)
                    )
                })
            }
            _ => false,
        },
        AccessPath::Edge(path) => match path.source().as_ref() {
            ir::EdgeAccessPlan::RangeIndex { key, .. } => range_satisfies_order(key, required),
            ir::EdgeAccessPlan::Intersect(children)
                if children
                    .iter()
                    .all(|child| edge_is_secondary(child.as_ref())) =>
            {
                children.iter().any(|child| {
                    matches!(
                        child.as_ref(),
                        ir::EdgeAccessPlan::RangeIndex { key, .. }
                            if range_satisfies_order(key, required)
                    )
                })
            }
            _ => false,
        },
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

fn range_satisfies_order(
    key: &catalog::ScopedPropertyDirectionKey,
    required: &ir::OrderKey,
) -> bool {
    key.property == required.property && key.direction == range_direction_for_order(required.order)
}

fn range_direction_candidate(
    key: &catalog::ScopedPropertyDirectionKey,
    required: &ir::OrderKey,
) -> bool {
    key.property == required.property && key.direction != range_direction_for_order(required.order)
}

fn range_direction_for_order(
    order: helix_ast::traversal::Order,
) -> helix_ast::index::RangeIndexDirection {
    match order {
        helix_ast::traversal::Order::Asc => helix_ast::index::RangeIndexDirection::Asc,
        helix_ast::traversal::Order::Desc => helix_ast::index::RangeIndexDirection::Desc,
    }
}
