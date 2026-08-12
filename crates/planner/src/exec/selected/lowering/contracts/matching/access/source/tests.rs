use super::{edge, node, SelectedAccessShapeMatch, SelectedAccessShapeMismatch};
use crate::{catalog, ir, physical};

#[test]
fn node_runtime_access_matches_only_runtime_physical_access() {
    let plan = ir::NodeAccessPlan::FromParam {
        param: ir::NonEmptyString::new("node_ids").expect("test name is non-empty"),
    };

    assert_eq!(
        node::selected_node_access_match(&plan, &physical::PhysicalAccess::RuntimeInput),
        SelectedAccessShapeMatch::Matched
    );
    assert_eq!(
        node::selected_node_access_match(&plan, &physical::PhysicalAccess::Empty),
        SelectedAccessShapeMatch::NotMatched(
            SelectedAccessShapeMismatch::PhysicalAccessFamilyMismatch
        )
    );
    assert!(node::selected_node_access_matches(
        &plan,
        &physical::PhysicalAccess::RuntimeInput,
    ));
    assert!(!node::selected_node_access_matches(
        &plan,
        &physical::PhysicalAccess::Empty,
    ));
}

#[test]
fn scan_then_filter_reports_pipeline_requirement_for_node_and_edge() {
    let predicate = ir::PredicatePlan::new(helix_ast::expr::Predicate::eq("active", true))
        .expect("predicate is valid");

    assert_eq!(
        node::selected_node_access_match(
            &ir::NodeAccessPlan::ScanThenFilter {
                source: ir::NodeAccessSourcePlan::from_unfiltered(ir::NodeAccessPlan::AllScan),
                residual: predicate.clone(),
            },
            &physical::PhysicalAccess::Empty,
        ),
        SelectedAccessShapeMatch::NotMatched(
            SelectedAccessShapeMismatch::ResidualFilterRequiresPipeline
        )
    );
    assert_eq!(
        edge::selected_edge_access_match(
            &ir::EdgeAccessPlan::ScanThenFilter {
                source: ir::EdgeAccessSourcePlan::from_unfiltered(ir::EdgeAccessPlan::AllScan),
                residual: predicate,
            },
            &physical::PhysicalAccess::Empty,
        ),
        SelectedAccessShapeMatch::NotMatched(
            SelectedAccessShapeMismatch::ResidualFilterRequiresPipeline
        )
    );
}

#[test]
fn equality_sources_match_only_their_selected_physical_algorithm() {
    let key = catalog::ScopedPropertyKey::try_new("User", "status").unwrap();
    let literal = |value| {
        ir::IndexValue::Literal(ir::SecondaryIndexLiteral::new(value).expect("test value is valid"))
    };
    let node_plan = |value, uniqueness| ir::NodeAccessPlan::EqualityIndex {
        index: catalog::NodeEqualityIndexMeta::try_new("node_eq:User:status")
            .unwrap()
            .with_uniqueness(uniqueness),
        key: key.clone(),
        value,
    };
    let cases = [
        (
            node_plan(
                literal(helix_ast::value::PropertyValue::from("active")),
                catalog::IndexUniqueness::NonUnique,
            ),
            physical::PhysicalAccess::EqualityBitmapPoint,
        ),
        (
            node_plan(
                literal(helix_ast::value::PropertyValue::from("active")),
                catalog::IndexUniqueness::Unique,
            ),
            physical::PhysicalAccess::EqualityUniqueVerified,
        ),
        (
            node_plan(
                literal(helix_ast::value::PropertyValue::Null),
                catalog::IndexUniqueness::NonUnique,
            ),
            physical::PhysicalAccess::EqualityAuthoritativeScan,
        ),
        (
            node_plan(
                literal(helix_ast::value::PropertyValue::F64(f64::NAN)),
                catalog::IndexUniqueness::NonUnique,
            ),
            physical::PhysicalAccess::Empty,
        ),
        (
            node_plan(
                ir::IndexValue::Param(ir::NonEmptyString::new("late_status").unwrap()),
                catalog::IndexUniqueness::Unique,
            ),
            physical::PhysicalAccess::EqualityDynamic,
        ),
    ];
    let alternatives = [
        physical::PhysicalAccess::EqualityBitmapPoint,
        physical::PhysicalAccess::EqualityUniqueVerified,
        physical::PhysicalAccess::EqualityAuthoritativeScan,
        physical::PhysicalAccess::Empty,
        physical::PhysicalAccess::EqualityDynamic,
    ];
    for (plan, selected) in cases {
        for candidate in &alternatives {
            assert_eq!(
                node::selected_node_access_matches(&plan, candidate),
                candidate == &selected,
                "{plan:?} matched {candidate:?} instead of {selected:?}",
            );
        }
    }

    let edge = ir::EdgeAccessPlan::EqualityIndex {
        index: catalog::EdgeEqualityIndexMeta::try_new("edge_eq:User:status").unwrap(),
        key,
        value: literal(helix_ast::value::PropertyValue::from("active")),
    };
    assert!(edge::selected_edge_access_matches(
        &edge,
        &physical::PhysicalAccess::EqualityBitmapPoint,
    ));
    assert!(!edge::selected_edge_access_matches(
        &edge,
        &physical::PhysicalAccess::EqualityUniqueVerified,
    ));
}

#[test]
fn equality_unions_match_batch_only_when_every_literal_has_one_non_unique_identity() {
    let source = |property: &str, value: ir::IndexValue| {
        ir::NodeAccessSourcePlan::new(ir::NodeAccessPlan::EqualityIndex {
            index: catalog::NodeEqualityIndexMeta::try_new(format!("node_eq:User:{property}"))
                .unwrap(),
            key: catalog::ScopedPropertyKey::try_new("User", property).unwrap(),
            value,
        })
        .unwrap()
    };
    let literal = |value: &str| {
        ir::IndexValue::Literal(
            ir::SecondaryIndexLiteral::new(helix_ast::value::PropertyValue::from(value)).unwrap(),
        )
    };
    let batch = ir::NodeAccessPlan::Union(ir::AtLeast::from_pair(
        source("status", literal("active")),
        source("status", literal("paused")),
    ));
    assert!(node::selected_node_access_matches(
        &batch,
        &physical::PhysicalAccess::BitmapBatchUnion,
    ));
    assert!(!node::selected_node_access_matches(
        &batch,
        &physical::PhysicalAccess::SetUnion,
    ));

    let explicit = ir::NodeAccessPlan::Union(ir::AtLeast::from_pair(
        source("status", literal("active")),
        source("role", literal("admin")),
    ));
    assert!(node::selected_node_access_matches(
        &explicit,
        &physical::PhysicalAccess::SetUnion,
    ));
    assert!(!node::selected_node_access_matches(
        &explicit,
        &physical::PhysicalAccess::BitmapBatchUnion,
    ));

    let null = ir::NodeAccessPlan::Union(ir::AtLeast::from_pair(
        source("status", literal("active")),
        source(
            "status",
            ir::IndexValue::Literal(
                ir::SecondaryIndexLiteral::new(helix_ast::value::PropertyValue::Null).unwrap(),
            ),
        ),
    ));
    assert!(node::selected_node_access_matches(
        &null,
        &physical::PhysicalAccess::SetUnion,
    ));
    assert!(!node::selected_node_access_matches(
        &null,
        &physical::PhysicalAccess::BitmapBatchUnion,
    ));

    let edge_source = |property: &str, value: ir::IndexValue| {
        ir::EdgeAccessSourcePlan::new(ir::EdgeAccessPlan::EqualityIndex {
            index: catalog::EdgeEqualityIndexMeta::try_new(format!("edge_eq:FOLLOWS:{property}"))
                .unwrap(),
            key: catalog::ScopedPropertyKey::try_new("FOLLOWS", property).unwrap(),
            value,
        })
        .unwrap()
    };
    let edge_batch = ir::EdgeAccessPlan::Union(ir::AtLeast::from_pair(
        edge_source("status", literal("active")),
        edge_source("status", literal("paused")),
    ));
    assert!(edge::selected_edge_access_matches(
        &edge_batch,
        &physical::PhysicalAccess::BitmapBatchUnion,
    ));
    assert!(!edge::selected_edge_access_matches(
        &edge_batch,
        &physical::PhysicalAccess::SetUnion,
    ));
    let edge_explicit = ir::EdgeAccessPlan::Union(ir::AtLeast::from_pair(
        edge_source("status", literal("active")),
        edge_source("kind", literal("friend")),
    ));
    assert!(edge::selected_edge_access_matches(
        &edge_explicit,
        &physical::PhysicalAccess::SetUnion,
    ));
    assert!(!edge::selected_edge_access_matches(
        &edge_explicit,
        &physical::PhysicalAccess::BitmapBatchUnion,
    ));

    let node_non_equality_first = ir::NodeAccessPlan::Union(ir::AtLeast::from_pair(
        ir::NodeAccessSourcePlan::new(ir::NodeAccessPlan::Empty).unwrap(),
        source("status", literal("active")),
    ));
    assert!(!node::selected_node_access_matches(
        &node_non_equality_first,
        &physical::PhysicalAccess::BitmapBatchUnion,
    ));
    assert!(node::selected_node_access_matches(
        &node_non_equality_first,
        &physical::PhysicalAccess::SetUnion,
    ));

    let edge_non_equality_first = ir::EdgeAccessPlan::Union(ir::AtLeast::from_pair(
        ir::EdgeAccessSourcePlan::new(ir::EdgeAccessPlan::Empty).unwrap(),
        edge_source("status", literal("active")),
    ));
    assert!(!edge::selected_edge_access_matches(
        &edge_non_equality_first,
        &physical::PhysicalAccess::BitmapBatchUnion,
    ));
    assert!(edge::selected_edge_access_matches(
        &edge_non_equality_first,
        &physical::PhysicalAccess::SetUnion,
    ));

    let node_same_index_other_key = ir::NodeAccessPlan::Union(ir::AtLeast::from_pair(
        source("status", literal("active")),
        ir::NodeAccessSourcePlan::new(ir::NodeAccessPlan::EqualityIndex {
            index: catalog::NodeEqualityIndexMeta::try_new("node_eq:User:status").unwrap(),
            key: catalog::ScopedPropertyKey::try_new("User", "role").unwrap(),
            value: literal("admin"),
        })
        .unwrap(),
    ));
    assert!(node::selected_node_access_matches(
        &node_same_index_other_key,
        &physical::PhysicalAccess::SetUnion,
    ));

    let node_unique_first = ir::NodeAccessPlan::Union(ir::AtLeast::from_pair(
        ir::NodeAccessSourcePlan::new(ir::NodeAccessPlan::EqualityIndex {
            index: catalog::NodeEqualityIndexMeta::try_new("node_eq:User:status")
                .unwrap()
                .with_uniqueness(catalog::IndexUniqueness::Unique),
            key: catalog::ScopedPropertyKey::try_new("User", "status").unwrap(),
            value: literal("active"),
        })
        .unwrap(),
        source("status", literal("paused")),
    ));
    assert!(node::selected_node_access_matches(
        &node_unique_first,
        &physical::PhysicalAccess::SetUnion,
    ));

    let node_null_first = ir::NodeAccessPlan::Union(ir::AtLeast::from_pair(
        source(
            "status",
            ir::IndexValue::Literal(
                ir::SecondaryIndexLiteral::new(helix_ast::value::PropertyValue::Null).unwrap(),
            ),
        ),
        source("status", literal("active")),
    ));
    assert!(node::selected_node_access_matches(
        &node_null_first,
        &physical::PhysicalAccess::SetUnion,
    ));

    let edge_same_index_other_key = ir::EdgeAccessPlan::Union(ir::AtLeast::from_pair(
        edge_source("status", literal("active")),
        ir::EdgeAccessSourcePlan::new(ir::EdgeAccessPlan::EqualityIndex {
            index: catalog::EdgeEqualityIndexMeta::try_new("edge_eq:FOLLOWS:status").unwrap(),
            key: catalog::ScopedPropertyKey::try_new("FOLLOWS", "kind").unwrap(),
            value: literal("friend"),
        })
        .unwrap(),
    ));
    assert!(edge::selected_edge_access_matches(
        &edge_same_index_other_key,
        &physical::PhysicalAccess::SetUnion,
    ));

    let edge_null_first = ir::EdgeAccessPlan::Union(ir::AtLeast::from_pair(
        edge_source(
            "status",
            ir::IndexValue::Literal(
                ir::SecondaryIndexLiteral::new(helix_ast::value::PropertyValue::Null).unwrap(),
            ),
        ),
        edge_source("status", literal("active")),
    ));
    assert!(edge::selected_edge_access_matches(
        &edge_null_first,
        &physical::PhysicalAccess::SetUnion,
    ));

    let edge_null_child = ir::EdgeAccessPlan::Union(ir::AtLeast::from_pair(
        edge_source("status", literal("active")),
        edge_source(
            "status",
            ir::IndexValue::Literal(
                ir::SecondaryIndexLiteral::new(helix_ast::value::PropertyValue::Null).unwrap(),
            ),
        ),
    ));
    assert!(edge::selected_edge_access_matches(
        &edge_null_child,
        &physical::PhysicalAccess::SetUnion,
    ));
}
