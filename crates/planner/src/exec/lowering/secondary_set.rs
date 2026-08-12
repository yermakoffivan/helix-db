//! Pure secondary-index access lowering.
//!
//! Eligible logical access trees become one executable ID-set operation. Mixed
//! trees return `None` and retain the legacy row-stream lowering path.

use crate::{exec, ir};

pub(in crate::exec) fn node_secondary_set(
    plan: &ir::NodeAccessPlan,
) -> Option<exec::ExecNodeSecondarySetPlan> {
    match plan {
        ir::NodeAccessPlan::Empty => Some(exec::ExecNodeSecondarySetPlan::Empty),
        ir::NodeAccessPlan::EqualityIndex { index, key, value } => {
            if value.semantics() == ir::EqualityIndexValueSemantics::NonReflexive {
                return Some(exec::ExecNodeSecondarySetPlan::Empty);
            }
            Some(exec::ExecNodeSecondarySetPlan::Equality {
                index: index.clone(),
                key: key.clone(),
                values: ir::AtLeast::from_one(value.clone()),
            })
        }
        ir::NodeAccessPlan::RangeIndex { index, key, range } => Some(
            exec::ExecNodeSecondarySetPlan::Range(exec::ExecNodeSecondaryRangePlan {
                index: index.clone(),
                key: key.clone(),
                range: range.clone(),
            }),
        ),
        ir::NodeAccessPlan::Union(children) => {
            let children = children
                .iter()
                .map(|child| node_secondary_set(child.as_ref()))
                .collect::<Option<Vec<_>>>()?;
            node_union(children)
        }
        ir::NodeAccessPlan::Intersect(children) => {
            let children = children
                .iter()
                .map(|child| node_secondary_set(child.as_ref()))
                .collect::<Option<Vec<_>>>()?;
            node_intersection(children)
        }
        ir::NodeAccessPlan::PointIds { .. }
        | ir::NodeAccessPlan::FromParam { .. }
        | ir::NodeAccessPlan::FromVar { .. }
        | ir::NodeAccessPlan::AllScan
        | ir::NodeAccessPlan::LabelScan { .. }
        | ir::NodeAccessPlan::VectorSearch { .. }
        | ir::NodeAccessPlan::TextSearch { .. }
        | ir::NodeAccessPlan::ScanThenFilter { .. } => None,
    }
}

pub(in crate::exec) fn edge_secondary_set(
    plan: &ir::EdgeAccessPlan,
) -> Option<exec::ExecEdgeSecondarySetPlan> {
    match plan {
        ir::EdgeAccessPlan::Empty => Some(exec::ExecEdgeSecondarySetPlan::Empty),
        ir::EdgeAccessPlan::EqualityIndex { index, key, value } => {
            if value.semantics() == ir::EqualityIndexValueSemantics::NonReflexive {
                return Some(exec::ExecEdgeSecondarySetPlan::Empty);
            }
            Some(exec::ExecEdgeSecondarySetPlan::Equality {
                index: index.clone(),
                key: key.clone(),
                values: ir::AtLeast::from_one(value.clone()),
            })
        }
        ir::EdgeAccessPlan::RangeIndex { index, key, range } => Some(
            exec::ExecEdgeSecondarySetPlan::Range(exec::ExecEdgeSecondaryRangePlan {
                index: index.clone(),
                key: key.clone(),
                range: range.clone(),
            }),
        ),
        ir::EdgeAccessPlan::Union(children) => {
            let children = children
                .iter()
                .map(|child| edge_secondary_set(child.as_ref()))
                .collect::<Option<Vec<_>>>()?;
            edge_union(children)
        }
        ir::EdgeAccessPlan::Intersect(children) => {
            let children = children
                .iter()
                .map(|child| edge_secondary_set(child.as_ref()))
                .collect::<Option<Vec<_>>>()?;
            edge_intersection(children)
        }
        ir::EdgeAccessPlan::PointIds { .. }
        | ir::EdgeAccessPlan::FromParam { .. }
        | ir::EdgeAccessPlan::FromVar { .. }
        | ir::EdgeAccessPlan::AllScan
        | ir::EdgeAccessPlan::LabelScan { .. }
        | ir::EdgeAccessPlan::VectorSearch { .. }
        | ir::EdgeAccessPlan::TextSearch { .. }
        | ir::EdgeAccessPlan::ScanThenFilter { .. } => None,
    }
}

fn node_union(
    children: Vec<exec::ExecNodeSecondarySetPlan>,
) -> Option<exec::ExecNodeSecondarySetPlan> {
    let mut flattened = children
        .into_iter()
        .flat_map(|child| match child {
            exec::ExecNodeSecondarySetPlan::Union(children) => children.into_iter().collect(),
            child => vec![child],
        })
        .filter(|child| !matches!(child, exec::ExecNodeSecondarySetPlan::Empty))
        .collect::<Vec<_>>();
    match flattened.len() {
        0 => Some(exec::ExecNodeSecondarySetPlan::Empty),
        1 => flattened.pop(),
        _ => {
            let batch = flattened.iter().all(|child| {
                matches!(
                    child,
                    exec::ExecNodeSecondarySetPlan::Equality { index, key, .. }
                        if matches!(&flattened[0], exec::ExecNodeSecondarySetPlan::Equality {
                            index: first_index,
                            key: first_key,
                            ..
                        } if index == first_index && key == first_key)
                )
            });
            if batch {
                let exec::ExecNodeSecondarySetPlan::Equality { index, key, values } =
                    flattened.remove(0)
                else {
                    unreachable!("batched node union contains equality leaves")
                };
                let values = values
                    .into_iter()
                    .chain(flattened.into_iter().flat_map(|child| {
                        let exec::ExecNodeSecondarySetPlan::Equality { values, .. } = child else {
                            unreachable!("batched node union contains equality leaves")
                        };
                        values
                    }))
                    .collect::<Vec<_>>();
                return Some(exec::ExecNodeSecondarySetPlan::Equality {
                    index,
                    key,
                    values: ir::AtLeast::try_from_vec(values)
                        .expect("batched equality union remains non-empty"),
                });
            }
            Some(exec::ExecNodeSecondarySetPlan::Union(
                ir::AtLeast::try_from_vec(flattened)
                    .expect("multi-child node union has at least two children"),
            ))
        }
    }
}

fn edge_union(
    children: Vec<exec::ExecEdgeSecondarySetPlan>,
) -> Option<exec::ExecEdgeSecondarySetPlan> {
    let mut flattened = children
        .into_iter()
        .flat_map(|child| match child {
            exec::ExecEdgeSecondarySetPlan::Union(children) => children.into_iter().collect(),
            child => vec![child],
        })
        .filter(|child| !matches!(child, exec::ExecEdgeSecondarySetPlan::Empty))
        .collect::<Vec<_>>();
    match flattened.len() {
        0 => Some(exec::ExecEdgeSecondarySetPlan::Empty),
        1 => flattened.pop(),
        _ => {
            let batch = flattened.iter().all(|child| {
                matches!(
                    child,
                    exec::ExecEdgeSecondarySetPlan::Equality { index, key, .. }
                        if matches!(&flattened[0], exec::ExecEdgeSecondarySetPlan::Equality {
                            index: first_index,
                            key: first_key,
                            ..
                        } if index == first_index && key == first_key)
                )
            });
            if batch {
                let exec::ExecEdgeSecondarySetPlan::Equality { index, key, values } =
                    flattened.remove(0)
                else {
                    unreachable!("batched edge union contains equality leaves")
                };
                let values = values
                    .into_iter()
                    .chain(flattened.into_iter().flat_map(|child| {
                        let exec::ExecEdgeSecondarySetPlan::Equality { values, .. } = child else {
                            unreachable!("batched edge union contains equality leaves")
                        };
                        values
                    }))
                    .collect::<Vec<_>>();
                return Some(exec::ExecEdgeSecondarySetPlan::Equality {
                    index,
                    key,
                    values: ir::AtLeast::try_from_vec(values)
                        .expect("batched equality union remains non-empty"),
                });
            }
            Some(exec::ExecEdgeSecondarySetPlan::Union(
                ir::AtLeast::try_from_vec(flattened)
                    .expect("multi-child edge union has at least two children"),
            ))
        }
    }
}

fn node_intersection(
    mut children: Vec<exec::ExecNodeSecondarySetPlan>,
) -> Option<exec::ExecNodeSecondarySetPlan> {
    if children
        .iter()
        .any(|child| matches!(child, exec::ExecNodeSecondarySetPlan::Empty))
    {
        return Some(exec::ExecNodeSecondarySetPlan::Empty);
    }
    let driver = children
        .iter()
        .position(|child| matches!(child, exec::ExecNodeSecondarySetPlan::Range(_)));
    if let Some(driver) = driver {
        let exec::ExecNodeSecondarySetPlan::Range(driver) = children.remove(driver) else {
            unreachable!("node ordered driver position contains a range")
        };
        return Some(exec::ExecNodeSecondarySetPlan::OrderedIntersect {
            driver,
            filters: ir::AtLeast::try_from_vec(children)
                .expect("logical intersection leaves at least one filter"),
        });
    }
    Some(exec::ExecNodeSecondarySetPlan::Intersect(
        ir::AtLeast::try_from_vec(children)
            .expect("logical node intersection has at least two children"),
    ))
}

fn edge_intersection(
    mut children: Vec<exec::ExecEdgeSecondarySetPlan>,
) -> Option<exec::ExecEdgeSecondarySetPlan> {
    if children
        .iter()
        .any(|child| matches!(child, exec::ExecEdgeSecondarySetPlan::Empty))
    {
        return Some(exec::ExecEdgeSecondarySetPlan::Empty);
    }
    let driver = children
        .iter()
        .position(|child| matches!(child, exec::ExecEdgeSecondarySetPlan::Range(_)));
    if let Some(driver) = driver {
        let exec::ExecEdgeSecondarySetPlan::Range(driver) = children.remove(driver) else {
            unreachable!("edge ordered driver position contains a range")
        };
        return Some(exec::ExecEdgeSecondarySetPlan::OrderedIntersect {
            driver,
            filters: ir::AtLeast::try_from_vec(children)
                .expect("logical intersection leaves at least one filter"),
        });
    }
    Some(exec::ExecEdgeSecondarySetPlan::Intersect(
        ir::AtLeast::try_from_vec(children)
            .expect("logical edge intersection has at least two children"),
    ))
}

#[cfg(test)]
mod tests {
    use helix_ast::index::RangeIndexDirection;
    use helix_ast::value::PropertyValue;

    use super::*;

    fn equality_value(value: PropertyValue) -> ir::IndexValue {
        ir::IndexValue::Literal(
            ir::SecondaryIndexLiteral::new(value).expect("test value is index compatible"),
        )
    }

    fn node_equality(property: &str, value: PropertyValue) -> ir::NodeAccessPlan {
        ir::NodeAccessPlan::EqualityIndex {
            index: crate::catalog::NodeEqualityIndexMeta::try_new(format!("user_{property}"))
                .expect("test index ID is non-empty"),
            key: crate::catalog::ScopedPropertyKey::try_new("User", property)
                .expect("test key is valid"),
            value: equality_value(value),
        }
    }

    fn node_source(plan: ir::NodeAccessPlan) -> ir::NodeAccessSourcePlan {
        ir::NodeAccessSourcePlan::new(plan).expect("test source is valid")
    }

    #[test]
    fn same_index_union_batches_values_into_one_equality_leaf() {
        let plan = ir::NodeAccessPlan::Union(ir::AtLeast::from_pair(
            node_source(node_equality("name", PropertyValue::from("alice"))),
            node_source(node_equality("name", PropertyValue::from("bob"))),
        ));

        assert!(matches!(
            node_secondary_set(&plan),
            Some(exec::ExecNodeSecondarySetPlan::Equality { key, values, .. })
                if key.property == "name" && values.len() == 2
        ));
    }

    #[test]
    fn mixed_union_retains_legacy_row_stream_lowering() {
        let ids = ir::ElementIds::new(ir::AtLeast::from_one(7)).expect("test ID is valid");
        let plan = ir::NodeAccessPlan::Union(ir::AtLeast::from_pair(
            node_source(node_equality("name", PropertyValue::from("alice"))),
            node_source(ir::NodeAccessPlan::PointIds { ids }),
        ));

        assert_eq!(node_secondary_set(&plan), None);
    }

    #[test]
    fn range_intersection_becomes_an_ordered_driver_with_filters() {
        let range = ir::NodeAccessPlan::RangeIndex {
            index: crate::catalog::NodeRangeIndexMeta::try_new("user_age")
                .expect("test index ID is non-empty"),
            key: crate::catalog::ScopedPropertyDirectionKey::try_new(
                "User",
                "age",
                RangeIndexDirection::Desc,
            )
            .expect("test range key is valid"),
            range: ir::IndexRange::All,
        };
        let plan = ir::NodeAccessPlan::Intersect(ir::AtLeast::from_pair(
            node_source(node_equality("status", PropertyValue::from("active"))),
            node_source(range),
        ));

        assert!(matches!(
            node_secondary_set(&plan),
            Some(exec::ExecNodeSecondarySetPlan::OrderedIntersect { driver, filters })
                if driver.key.property == "age" && filters.len() == 1
        ));
    }

    #[test]
    fn nan_equality_lowers_to_a_static_empty_set() {
        assert_eq!(
            node_secondary_set(&node_equality("score", PropertyValue::F64(f64::NAN))),
            Some(exec::ExecNodeSecondarySetPlan::Empty)
        );
    }
}
