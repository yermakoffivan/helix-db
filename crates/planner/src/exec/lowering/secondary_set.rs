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
        ir::NodeAccessPlan::EqualityIndex { .. } => {
            let leaf = exec::node_exec_access(exec::SimpleNodeAccessLeaf::try_from(plan).ok()?);
            match leaf {
                exec::ExecNodeAccessPlan::Empty => Some(exec::ExecNodeSecondarySetPlan::Empty),
                exec::ExecNodeAccessPlan::Bitmap { bitmap } => {
                    Some(exec::ExecNodeSecondarySetPlan::Bitmap(bitmap))
                }
                exec::ExecNodeAccessPlan::Unique {
                    lookup,
                    verification,
                } => Some(exec::ExecNodeSecondarySetPlan::Unique {
                    lookup,
                    verification,
                }),
                exec::ExecNodeAccessPlan::AuthoritativeScan { predicate } => {
                    Some(exec::ExecNodeSecondarySetPlan::AuthoritativeScan(predicate))
                }
                exec::ExecNodeAccessPlan::DynamicEquality { index, key, param } => {
                    Some(exec::ExecNodeSecondarySetPlan::DynamicEquality { index, key, param })
                }
                exec::ExecNodeAccessPlan::FromParam { .. }
                | exec::ExecNodeAccessPlan::FromVar { .. }
                | exec::ExecNodeAccessPlan::AllScan
                | exec::ExecNodeAccessPlan::LabelScan { .. }
                | exec::ExecNodeAccessPlan::RangeIndex { .. }
                | exec::ExecNodeAccessPlan::SecondarySet { .. }
                | exec::ExecNodeAccessPlan::VectorSearch { .. }
                | exec::ExecNodeAccessPlan::TextSearch { .. } => {
                    unreachable!("equality leaf lowering returns an equality executable variant")
                }
            }
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
        ir::EdgeAccessPlan::EqualityIndex { .. } => {
            let leaf = exec::edge_exec_access(exec::SimpleEdgeAccessLeaf::try_from(plan).ok()?);
            match leaf {
                exec::ExecEdgeAccessPlan::Empty => Some(exec::ExecEdgeSecondarySetPlan::Empty),
                exec::ExecEdgeAccessPlan::Bitmap { bitmap } => {
                    Some(exec::ExecEdgeSecondarySetPlan::Bitmap(bitmap))
                }
                exec::ExecEdgeAccessPlan::AuthoritativeScan { predicate } => {
                    Some(exec::ExecEdgeSecondarySetPlan::AuthoritativeScan(predicate))
                }
                exec::ExecEdgeAccessPlan::DynamicEquality { index, key, param } => {
                    Some(exec::ExecEdgeSecondarySetPlan::DynamicEquality { index, key, param })
                }
                exec::ExecEdgeAccessPlan::FromParam { .. }
                | exec::ExecEdgeAccessPlan::FromVar { .. }
                | exec::ExecEdgeAccessPlan::AllScan
                | exec::ExecEdgeAccessPlan::LabelScan { .. }
                | exec::ExecEdgeAccessPlan::RangeIndex { .. }
                | exec::ExecEdgeAccessPlan::SecondarySet { .. }
                | exec::ExecEdgeAccessPlan::VectorSearch { .. }
                | exec::ExecEdgeAccessPlan::TextSearch { .. } => {
                    unreachable!("equality leaf lowering returns an equality executable variant")
                }
            }
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
            exec::ExecNodeSecondarySetPlan::Union { driver, rest } => {
                core::iter::once(*driver).chain(rest).collect()
            }
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
                    exec::ExecNodeSecondarySetPlan::Bitmap(exec::ExecNodeBitmapExpr::PointRead { index, key, .. })
                        if matches!(&flattened[0], exec::ExecNodeSecondarySetPlan::Bitmap(exec::ExecNodeBitmapExpr::PointRead {
                            index: first_index,
                            key: first_key,
                            ..
                        }) if index == first_index && key == first_key)
                )
            });
            if batch {
                let exec::ExecNodeSecondarySetPlan::Bitmap(exec::ExecNodeBitmapExpr::PointRead {
                    index,
                    key,
                    value,
                }) = flattened.remove(0)
                else {
                    unreachable!("batched node union contains equality leaves")
                };
                let values = core::iter::once(value)
                    .chain(flattened.into_iter().map(|child| {
                        let exec::ExecNodeSecondarySetPlan::Bitmap(
                            exec::ExecNodeBitmapExpr::PointRead { value, .. },
                        ) = child
                        else {
                            unreachable!("batched node union contains equality leaves")
                        };
                        value
                    }))
                    .collect::<Vec<_>>();
                return Some(exec::ExecNodeSecondarySetPlan::Bitmap(
                    exec::ExecNodeBitmapExpr::BatchedUnionRead {
                        index,
                        key,
                        values: ir::AtLeast::try_from_vec(values)
                            .expect("batched equality union has at least two values"),
                    },
                ));
            }
            let driver = flattened.remove(0);
            Some(exec::ExecNodeSecondarySetPlan::Union {
                driver: Box::new(driver),
                rest: ir::AtLeast::try_from_vec(flattened)
                    .expect("multi-child node union leaves at least one child"),
            })
        }
    }
}

fn edge_union(
    children: Vec<exec::ExecEdgeSecondarySetPlan>,
) -> Option<exec::ExecEdgeSecondarySetPlan> {
    let mut flattened = children
        .into_iter()
        .flat_map(|child| match child {
            exec::ExecEdgeSecondarySetPlan::Union { driver, rest } => {
                core::iter::once(*driver).chain(rest).collect()
            }
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
                    exec::ExecEdgeSecondarySetPlan::Bitmap(exec::ExecEdgeBitmapExpr::PointRead { index, key, .. })
                        if matches!(&flattened[0], exec::ExecEdgeSecondarySetPlan::Bitmap(exec::ExecEdgeBitmapExpr::PointRead {
                            index: first_index,
                            key: first_key,
                            ..
                        }) if index == first_index && key == first_key)
                )
            });
            if batch {
                let exec::ExecEdgeSecondarySetPlan::Bitmap(exec::ExecEdgeBitmapExpr::PointRead {
                    index,
                    key,
                    value,
                }) = flattened.remove(0)
                else {
                    unreachable!("batched edge union contains equality leaves")
                };
                let values = core::iter::once(value)
                    .chain(flattened.into_iter().map(|child| {
                        let exec::ExecEdgeSecondarySetPlan::Bitmap(
                            exec::ExecEdgeBitmapExpr::PointRead { value, .. },
                        ) = child
                        else {
                            unreachable!("batched edge union contains equality leaves")
                        };
                        value
                    }))
                    .collect::<Vec<_>>();
                return Some(exec::ExecEdgeSecondarySetPlan::Bitmap(
                    exec::ExecEdgeBitmapExpr::BatchedUnionRead {
                        index,
                        key,
                        values: ir::AtLeast::try_from_vec(values)
                            .expect("batched equality union has at least two values"),
                    },
                ));
            }
            let driver = flattened.remove(0);
            Some(exec::ExecEdgeSecondarySetPlan::Union {
                driver: Box::new(driver),
                rest: ir::AtLeast::try_from_vec(flattened)
                    .expect("multi-child edge union leaves at least one child"),
            })
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
    let driver = children.remove(0);
    Some(exec::ExecNodeSecondarySetPlan::Intersect {
        driver: Box::new(driver),
        rest: ir::AtLeast::try_from_vec(children)
            .expect("logical node intersection leaves at least one child"),
    })
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
    let driver = children.remove(0);
    Some(exec::ExecEdgeSecondarySetPlan::Intersect {
        driver: Box::new(driver),
        rest: ir::AtLeast::try_from_vec(children)
            .expect("logical edge intersection leaves at least one child"),
    })
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
            Some(exec::ExecNodeSecondarySetPlan::Bitmap(
                exec::ExecNodeBitmapExpr::BatchedUnionRead { key, values, .. }
            ))
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
