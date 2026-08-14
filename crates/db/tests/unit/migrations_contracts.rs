use super::*;

fn legacy_secondary(
    element_type: config::SecondaryIndexElementType,
    kind: config::SecondaryIndexKind,
    unique: bool,
    direction: config::RangeIndexDirection,
) -> LegacySecondaryIndexDefinition {
    LegacySecondaryIndexDefinition {
        element_type,
        kind,
        label: "Document".to_string(),
        property: "score".to_string(),
        unique,
        direction,
    }
}

#[test]
fn legacy_secondary_definition_matrix_accepts_only_representable_algorithms() {
    use config::{RangeIndexDirection, SecondaryIndexElementType, SecondaryIndexKind};

    for element_type in [
        SecondaryIndexElementType::Node,
        SecondaryIndexElementType::Edge,
    ] {
        for kind in [SecondaryIndexKind::Equality, SecondaryIndexKind::Range] {
            for unique in [false, true] {
                for direction in [RangeIndexDirection::Asc, RangeIndexDirection::Desc] {
                    let result =
                        legacy_secondary(element_type, kind, unique, direction).into_runtime();
                    let expected_valid = match (element_type, kind, unique, direction) {
                        (
                            SecondaryIndexElementType::Node,
                            SecondaryIndexKind::Equality,
                            false | true,
                            RangeIndexDirection::Asc,
                        ) => true,
                        (
                            SecondaryIndexElementType::Node | SecondaryIndexElementType::Edge,
                            SecondaryIndexKind::Range,
                            false,
                            RangeIndexDirection::Asc | RangeIndexDirection::Desc,
                        ) => true,
                        (
                            SecondaryIndexElementType::Edge,
                            SecondaryIndexKind::Equality,
                            false,
                            RangeIndexDirection::Asc,
                        ) => true,
                        _ => false,
                    };
                    assert_eq!(
                        result.is_ok(),
                        expected_valid,
                        "unexpected legacy matrix result for {element_type:?}/{kind:?}/{unique}/{direction:?}"
                    );
                }
            }
        }
    }
}

fn legacy_vector(element_type: config::VectorElementType) -> LegacyVectorIndexDefinition {
    LegacyVectorIndexDefinition {
        element_type,
        label: "Document".to_string(),
        property: "embedding".to_string(),
        tenant_property: Some("tenant".to_string()),
        dimension: 3,
        metric: crate::search::vector::VectorDistanceMetric::Cosine,
        m: 8,
        m0: 16,
        ef_construction: 64,
        ml: 0.5,
        simhash_threshold: 64,
        sampling_ratio: 0.25,
        adaptive_enabled: true,
        adaptive_failure_prob: 0.01,
    }
}

fn legacy_text(element_type: config::TextElementType) -> LegacyTextIndexDefinition {
    LegacyTextIndexDefinition {
        element_type,
        label: "Document".to_string(),
        property: "body".to_string(),
        tenant_property: Some("tenant".to_string()),
        analyzer: config::TextAnalyzerKind::StandardStemEn,
        positions_enabled: true,
    }
}

#[test]
fn legacy_dynamic_definition_families_preserve_identity_and_runtime_shape() {
    use crate::index_lifecycle::{IndexDefinitionFamily, IndexElementKind};

    let definitions = [
        LegacyDynamicIndexDefinition::Secondary(legacy_secondary(
            config::SecondaryIndexElementType::Node,
            config::SecondaryIndexKind::Equality,
            true,
            config::RangeIndexDirection::Asc,
        )),
        LegacyDynamicIndexDefinition::Vector(legacy_vector(config::VectorElementType::Node)),
        LegacyDynamicIndexDefinition::Vector(legacy_vector(config::VectorElementType::Edge)),
        LegacyDynamicIndexDefinition::Text(legacy_text(config::TextElementType::Node)),
        LegacyDynamicIndexDefinition::Text(legacy_text(config::TextElementType::Edge)),
    ];

    for definition in definitions {
        let key = definition.key();
        let identity = key.identity().expect("legacy identity validates");
        let validated = definition
            .clone()
            .into_validated()
            .expect("legacy definition validates");
        assert_eq!(identity, validated.identity());
        match validated.family() {
            IndexDefinitionFamily::Secondary => {
                assert!(matches!(key, LegacyDynamicIndexKey::Secondary(_)));
                assert_eq!(identity.element_kind(), IndexElementKind::Node);
            }
            IndexDefinitionFamily::Vector => {
                assert!(matches!(key, LegacyDynamicIndexKey::Vector { .. }));
            }
            IndexDefinitionFamily::Text => {
                assert!(matches!(key, LegacyDynamicIndexKey::Text { .. }));
            }
        }
    }
}

#[test]
fn legacy_catalog_rows_round_trip_definition_and_tombstone_variants() {
    use crate::index_lifecycle::ValidatedDynamicIndexDefinition;

    let definitions = [
        ValidatedDynamicIndexDefinition::try_from(
            config::SecondaryIndexDefinition::node_equality("Document", "score").unwrap(),
        )
        .unwrap(),
        ValidatedDynamicIndexDefinition::try_from(
            config::VectorIndexDefinition::new_edge(
                "Document",
                "embedding",
                3,
                crate::search::vector::VectorDistanceMetric::Euclidean,
            )
            .unwrap(),
        )
        .unwrap(),
        ValidatedDynamicIndexDefinition::try_from(
            config::TextIndexDefinition::new_node("Document", "body").unwrap(),
        )
        .unwrap(),
    ];

    for definition in definitions {
        for tombstone in [false, true] {
            let (key, value) = migration_parity_legacy_catalog_row(&definition, tombstone)
                .expect("legacy catalog row encodes");
            assert!(!key.is_empty());
            let decoded: LegacyDynamicIndexCatalogEntry =
                serde_json::from_slice(&value).expect("legacy catalog row decodes");
            assert_eq!(
                matches!(decoded, LegacyDynamicIndexCatalogEntry::Tombstone { .. }),
                tombstone
            );
        }
    }
}

#[test]
fn migration_identity_mode_stage_and_resume_contracts_are_exhaustive() {
    let ids = [
        MigrationId::GraphFormatV1Rewrite,
        MigrationId::LegacyVectorPropertyMaterialization,
        MigrationId::LegacyVectorPhysicalCleanup,
        MigrationId::GraphFormatV1Cleanup,
    ];
    let expected_initial = [
        MigrationStage::PropertyIndexes,
        MigrationStage::NodeProperties,
        MigrationStage::FenceLegacyVectorSources,
        MigrationStage::LegacyEdgePairs,
    ];
    for (id, initial) in ids.into_iter().zip(expected_initial) {
        assert!(!id.storage_name().is_empty());
        assert!(!id.log_name().is_empty());
        assert_eq!(id.initial_stage(), initial);
        let encoded = serde_json::to_vec(&id).unwrap();
        assert_eq!(serde_json::from_slice::<MigrationId>(&encoded).unwrap(), id);
    }

    for mode in [MigrationMode::BlockingStartup, MigrationMode::Background] {
        assert!(!mode.log_name().is_empty());
        let encoded = serde_json::to_vec(&mode).unwrap();
        assert_eq!(
            serde_json::from_slice::<MigrationMode>(&encoded).unwrap(),
            mode
        );
    }

    let stages = [
        MigrationStage::PropertyIndexes,
        MigrationStage::NodeProperties,
        MigrationStage::LegacyEdgePairs,
        MigrationStage::EdgeEndpoints,
        MigrationStage::FenceLegacyVectorSources,
        MigrationStage::LegacyVectorHotRows,
        MigrationStage::LegacyVectorLayer0Rows,
        MigrationStage::LegacyVectorCoreRows,
        MigrationStage::LegacyVectorDefinitions,
        MigrationStage::ReleaseLegacyVectorReservations,
    ];
    for stage in stages {
        assert!(!stage.prefix(DataScope::LegacyUnscoped).is_empty());
        assert!(!stage.log_name().is_empty());
    }

    assert!(MigrationResumeKey::new(Vec::new()).is_none());
    assert!(MigrationResumeKey::try_from(Vec::new()).is_err());
    let resume = MigrationResumeKey::new(vec![1, 2, 3]).unwrap();
    assert_eq!(resume.as_bytes(), &[1, 2, 3]);
    assert_eq!(Vec::<u8>::from(resume.clone()), vec![1, 2, 3]);
    let encoded = serde_json::to_vec(&resume).unwrap();
    assert_eq!(
        serde_json::from_slice::<MigrationResumeKey>(&encoded).unwrap(),
        resume
    );
}

#[test]
fn migration_job_state_machine_preserves_counters_and_rejects_invalid_transitions() {
    let resume = MigrationResumeKey::new(vec![7]).unwrap();
    let mut running = MigrationJob::new(
        MigrationId::GraphFormatV1Rewrite,
        MigrationMode::BlockingStartup,
    );
    assert!(running.is_runnable());
    assert!(!running.is_completed());
    assert!(!running.is_failed());
    assert_eq!(
        running.state.running_stage(),
        Some(MigrationStage::PropertyIndexes)
    );
    assert_eq!(running.state.processed_rows(), 0);
    assert_eq!(running.state.log_name(), "running");
    running.record_advanced(resume.clone(), u64::MAX);
    running.record_advanced(resume.clone(), 1);
    assert_eq!(running.state.processed_rows(), u64::MAX);
    running.advance_stage(MigrationStage::NodeProperties);
    assert_eq!(
        running.state.running_stage(),
        Some(MigrationStage::NodeProperties)
    );
    running.fail("failed");
    assert!(running.is_failed());
    assert_eq!(running.state.running_stage(), None);
    assert_eq!(running.state.log_name(), "failed");

    let failed_before = running.clone();
    running.record_advanced(resume.clone(), 1);
    running.fail("ignored");
    assert_eq!(running, failed_before);
    running.retry();
    assert!(running.is_runnable());
    assert_eq!(running.state.processed_rows(), u64::MAX);
    running.complete();
    assert!(running.is_completed());
    assert_eq!(running.state.log_name(), "completed");
    assert_eq!(running.state.running_stage(), None);

    let completed_before = running.clone();
    running.record_advanced(resume, 1);
    running.advance_stage(MigrationStage::EdgeEndpoints);
    running.complete();
    running.fail("ignored");
    running.retry();
    assert_eq!(running, completed_before);

    let key = MigrationJobKey::new(DataScope::LegacyUnscoped, MigrationId::GraphFormatV1Cleanup);
    assert!(!key.as_ref().is_empty());
    assert_eq!(key.clone().into_bytes().as_ref(), key.as_ref());
    assert!(!migration_job_scan_prefix_scoped(DataScope::LegacyUnscoped).is_empty());
    assert!(!MigrationStep::IDLE.advanced);
    assert_eq!(MigrationStep::IDLE.rows, 0);
    assert_eq!(MigrationStep::IDLE.admitted_bytes, 0);
}

#[test]
fn legacy_range_direction_preserves_both_physical_directions() {
    assert_eq!(
        legacy_range_direction(config::RangeIndexDirection::Asc),
        crate::encoding::v1::indexes::range::RangeIndexDirection::Asc
    );
    assert_eq!(
        legacy_range_direction(config::RangeIndexDirection::Desc),
        crate::encoding::v1::indexes::range::RangeIndexDirection::Desc
    );
}
