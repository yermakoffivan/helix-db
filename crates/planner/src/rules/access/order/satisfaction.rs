use super::super::sources::{
    edge_source_hard_cardinality_upper_bound, node_source_hard_cardinality_upper_bound,
};
use super::direction::order_for_range_direction;
use crate::{catalog, ir, logical};

pub(in crate::rules::access) fn access_satisfies_order(order: &logical::AccessOrder) -> bool {
    match order.access() {
        logical::AccessPath::Node(path) => {
            node_access_satisfies_order(path.source(), order.ordering())
        }
        logical::AccessPath::Edge(path) => {
            edge_access_satisfies_order(path.source(), order.ordering())
        }
    }
}

fn node_access_satisfies_order(
    source: &ir::NodeAccessSourcePlan,
    ordering: &ir::OrderKeys,
) -> bool {
    node_source_hard_cardinality_upper_bound(source).is_some_and(|upper| upper <= 1)
        || node_secondary_source_satisfies_order(source.as_ref(), ordering)
}

fn edge_access_satisfies_order(
    source: &ir::EdgeAccessSourcePlan,
    ordering: &ir::OrderKeys,
) -> bool {
    edge_source_hard_cardinality_upper_bound(source).is_some_and(|upper| upper <= 1)
        || edge_secondary_source_satisfies_order(source.as_ref(), ordering)
}

fn node_secondary_source_satisfies_order(
    source: &ir::NodeAccessPlan,
    ordering: &ir::OrderKeys,
) -> bool {
    match source {
        ir::NodeAccessPlan::RangeIndex { key, .. } => range_index_satisfies_order(key, ordering),
        ir::NodeAccessPlan::Intersect(children)
            if children
                .iter()
                .all(|child| node_is_secondary(child.as_ref())) =>
        {
            children
                .iter()
                .any(|child| node_secondary_source_satisfies_order(child.as_ref(), ordering))
        }
        _ => false,
    }
}

fn edge_secondary_source_satisfies_order(
    source: &ir::EdgeAccessPlan,
    ordering: &ir::OrderKeys,
) -> bool {
    match source {
        ir::EdgeAccessPlan::RangeIndex { key, .. } => range_index_satisfies_order(key, ordering),
        ir::EdgeAccessPlan::Intersect(children)
            if children
                .iter()
                .all(|child| edge_is_secondary(child.as_ref())) =>
        {
            children
                .iter()
                .any(|child| edge_secondary_source_satisfies_order(child.as_ref(), ordering))
        }
        _ => false,
    }
}

fn node_is_secondary(source: &ir::NodeAccessPlan) -> bool {
    match source {
        ir::NodeAccessPlan::Empty
        | ir::NodeAccessPlan::EqualityIndex { .. }
        | ir::NodeAccessPlan::RangeIndex { .. } => true,
        ir::NodeAccessPlan::Intersect(children) | ir::NodeAccessPlan::Union(children) => children
            .iter()
            .all(|child| node_is_secondary(child.as_ref())),
        _ => false,
    }
}

fn edge_is_secondary(source: &ir::EdgeAccessPlan) -> bool {
    match source {
        ir::EdgeAccessPlan::Empty
        | ir::EdgeAccessPlan::EqualityIndex { .. }
        | ir::EdgeAccessPlan::RangeIndex { .. } => true,
        ir::EdgeAccessPlan::Intersect(children) | ir::EdgeAccessPlan::Union(children) => children
            .iter()
            .all(|child| edge_is_secondary(child.as_ref())),
        _ => false,
    }
}

fn range_index_satisfies_order(
    key: &catalog::ScopedPropertyDirectionKey,
    ordering: &ir::OrderKeys,
) -> bool {
    matches!(
        ordering.as_ref(),
        [required]
            if required.property == key.property
                && required.order == order_for_range_direction(key.direction)
    )
}
