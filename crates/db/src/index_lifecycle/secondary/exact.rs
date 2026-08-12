//! Literal secondary-index primitives selected by executable plans.

use super::*;

#[async_trait]
trait ExactRangeRows {
    async fn next_exact(
        &mut self,
    ) -> std::result::Result<Option<slatedb::KeyValue>, slatedb::Error>;
}

#[async_trait]
impl ExactRangeRows for slatedb::DbIterator {
    async fn next_exact(
        &mut self,
    ) -> std::result::Result<Option<slatedb::KeyValue>, slatedb::Error> {
        self.next().await
    }
}

/// Records one authoritative graph read requested by an exact equality plan.
#[inline]
pub(crate) fn record_equality_graph_read() {
    #[cfg(any(test, feature = "production-coverage"))]
    BENCHMARK_GRAPH_READS.fetch_add(1, AtomicOrdering::Relaxed);
}

/// Executes one planner-selected indexed equality point read without choosing
/// or performing authoritative verification.
pub(crate) async fn lookup_active_equality_point_literal(
    reader: &(impl DbReadOps + Sync),
    handle: &ActiveIndexHandle,
    value: &PropertyValue,
) -> Result<roaring::RoaringTreemap> {
    let Some(definition) = handle.secondary_definition() else {
        return Err(corruption(
            "literal equality point read received a non-secondary Active handle",
        ));
    };
    if !matches!(
        definition,
        ValidatedSecondaryIndexDefinition::NodeEquality { .. }
            | ValidatedSecondaryIndexDefinition::EdgeEquality { .. }
    ) {
        return Err(corruption(
            "literal equality point read received a range definition",
        ));
    }
    let EqualityValueProjection::Indexed(value) = project_equality_value(value) else {
        return Err(corruption(
            "literal equality point read received a non-indexed value",
        ));
    };
    let lane = definition_lane(definition);
    let key = secondary_entry_key(
        handle.scope(),
        handle.index_id(),
        handle.generation(),
        definition,
        CanonicalSecondaryValue::equality(value),
        IndexEntityId::initial(),
    )
    .expect("validated indexed equality values always fit their physical key");
    record_equality_point_read();
    let Some(bytes) = reader.get(key).await? else {
        return Ok(roaring::RoaringTreemap::new());
    };
    if lane.is_unique() {
        let owner =
            decode_secondary_entry_value(handle.index_id(), handle.generation(), lane, &bytes)?;
        return Ok(roaring::RoaringTreemap::from_iter([owner.get()]));
    }
    SecondaryEqualityBitmapValue::decode(&bytes)
        .map(SecondaryEqualityBitmapValue::into_ids)
        .map_err(HelixDbError::from)
}

/// Executes one planner-selected literal bitmap multi-get.
///
/// Duplicate physical keys are preserved and the primitive always issues one
/// `multi_get`; executable validation owns the at-least-two invariant.
pub(crate) async fn lookup_active_equality_literal_batch(
    reader: &(impl DbReadOps + Sync),
    handle: &ActiveIndexHandle,
    values: &[PropertyValue],
) -> Result<roaring::RoaringTreemap> {
    if values.len() < 2 {
        return Err(corruption(
            "literal equality bitmap batch contained fewer than two values",
        ));
    }
    let Some(definition) = handle.secondary_definition() else {
        return Err(corruption(
            "literal equality bitmap batch received a non-secondary Active handle",
        ));
    };
    if !definition_uses_equality_bitmap(definition) {
        return Err(corruption(
            "literal equality bitmap batch received a non-bitmap definition",
        ));
    }
    let keys = values
        .iter()
        .map(|value| {
            let EqualityValueProjection::Indexed(value) = project_equality_value(value) else {
                return Err(corruption(
                    "literal equality bitmap batch received a non-indexed value",
                ));
            };
            secondary_entry_key(
                handle.scope(),
                handle.index_id(),
                handle.generation(),
                definition,
                CanonicalSecondaryValue::equality(value),
                IndexEntityId::initial(),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    keys.iter().for_each(|_| record_equality_point_read());
    #[cfg(any(test, feature = "production-coverage"))]
    BENCHMARK_MULTI_GETS.fetch_add(1, AtomicOrdering::Relaxed);
    let mut owners = roaring::RoaringTreemap::new();
    for bytes in reader.multi_get(&keys).await?.into_iter().flatten() {
        owners |= SecondaryEqualityBitmapValue::decode(&bytes)?.into_ids();
    }
    Ok(owners)
}

/// Scans an exact range generation and applies planner-ordered bitmap
/// membership before the accepted-match limit.
pub(crate) async fn scan_active_range_generation_with_membership(
    reader: &(impl DbReadOps + Sync),
    handle: &ActiveIndexHandle,
    query: Option<&SecondaryRangeQuery>,
    limit: Option<usize>,
    membership: &[roaring::RoaringTreemap],
) -> Result<Vec<u64>> {
    let Some(definition) = handle.secondary_definition() else {
        return Err(corruption(
            "secondary range serving received a non-secondary Active handle",
        ));
    };
    if !matches!(
        definition,
        ValidatedSecondaryIndexDefinition::NodeRange { .. }
            | ValidatedSecondaryIndexDefinition::EdgeRange { .. }
    ) {
        return Err(corruption(
            "secondary range serving received an equality definition",
        ));
    }

    let direction = match definition.direction() {
        RangeIndexDirection::Asc => StorageRangeIndexDirection::Asc,
        RangeIndexDirection::Desc => StorageRangeIndexDirection::Desc,
    };
    let lane = definition_lane(definition);
    let bounds = match query {
        Some(query) => match secondary_range_scan_bounds(direction, query)? {
            Some(bounds) => bounds,
            None => return Ok(Vec::new()),
        },
        None => (Bound::Unbounded, Bound::Unbounded),
    };
    let prefix = IndexKey::data_prefix(
        handle.scope(),
        ScopedKey::secondary_lane_prefix(handle.index_id(), handle.generation(), lane),
    );
    let rows = reader.scan_prefix(&prefix, bounds).await?;
    consume_active_range_rows(
        reader, handle, definition, direction, lane, rows, query, limit, membership,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn consume_active_range_rows(
    reader: &(impl DbReadOps + Sync),
    handle: &ActiveIndexHandle,
    definition: &ValidatedSecondaryIndexDefinition,
    direction: StorageRangeIndexDirection,
    lane: SecondaryEntryLane,
    mut rows: impl ExactRangeRows,
    query: Option<&SecondaryRangeQuery>,
    limit: Option<usize>,
    membership: &[roaring::RoaringTreemap],
) -> Result<Vec<u64>> {
    let mut owners = Vec::new();
    while let Some(row) = rows.next_exact().await? {
        let IndexKey::Data {
            kind: ScopedKey::SecondaryEntry(key),
            ..
        } = IndexKey::parse_from_slice(handle.scope(), &row.key)?
        else {
            return Err(corruption(
                "secondary range prefix yielded another key kind",
            ));
        };
        if key.index_id() != handle.index_id()
            || key.generation() != handle.generation()
            || key.lane() != lane
        {
            return Err(corruption(
                "secondary range entry escaped its exact serving prefix",
            ));
        }
        let key_owner = key
            .entity_id()
            .expect("the validated range lane always carries its key owner");
        let value_owner =
            decode_secondary_entry_value(handle.index_id(), handle.generation(), lane, &row.value)?;
        if key_owner != value_owner {
            return Err(corruption(
                "secondary range entry key/value owners disagree",
            ));
        }
        let key_value = key
            .range_value()
            .expect("the validated range lane always carries a range value");
        if !authoritative_range_matches(
            reader,
            handle.scope(),
            definition,
            value_owner,
            direction,
            key_value,
            query,
        )
        .await?
        {
            continue;
        }
        if !membership
            .iter()
            .all(|bitmap| bitmap.contains(value_owner.get()))
        {
            continue;
        }
        owners.push(value_owner.get());
        if limit.is_some_and(|limit| owners.len() >= limit) {
            break;
        }
    }
    Ok(owners)
}

#[cfg(all(feature = "production-coverage", not(test)))]
pub(crate) async fn run_production_contracts() {
    use slatedb::object_store::memory::InMemory;

    fn active_handle(
        definition: crate::index_lifecycle::ValidatedDynamicIndexDefinition,
        physical: crate::index_lifecycle::PhysicalGeneration,
    ) -> ActiveIndexHandle {
        let building = crate::index_lifecycle::IndexRecordV2::building(
            IndexId::initial(),
            definition,
            crate::index_lifecycle::IndexRevision::initial(),
            physical,
            crate::index_lifecycle::IndexOperationId::new_v4(),
        )
        .expect("exact serving fixture starts building");
        let active = building
            .transition(crate::index_lifecycle::IndexStateTransition::Activate)
            .expect("exact serving fixture activates");
        ActiveIndexHandle::try_from_record(DataScope::LegacyUnscoped, &active)
            .expect("exact serving fixture projects an Active handle")
    }

    fn secondary_handle(definition: crate::config::SecondaryIndexDefinition) -> ActiveIndexHandle {
        active_handle(
            crate::index_lifecycle::ValidatedDynamicIndexDefinition::try_from(definition)
                .expect("exact secondary fixture validates"),
            crate::index_lifecycle::PhysicalGeneration::Secondary {
                generation: IndexGenerationId::initial(),
            },
        )
    }

    async fn put_entry(db: &slatedb::Db, handle: &ActiveIndexHandle, value: &str, entity_id: u64) {
        let definition = handle
            .secondary_definition()
            .expect("exact entry fixture uses a secondary handle");
        let direction = match definition.direction() {
            RangeIndexDirection::Asc => StorageRangeIndexDirection::Asc,
            RangeIndexDirection::Desc => StorageRangeIndexDirection::Desc,
        };
        let canonical = if definition_uses_equality_bitmap(definition)
            || matches!(
                definition,
                ValidatedSecondaryIndexDefinition::NodeEquality { unique: true, .. }
            ) {
            CanonicalSecondaryValue::equality_string(value)
        } else {
            CanonicalSecondaryValue::range_string(direction, value)
        };
        let entity_id = IndexEntityId::new(entity_id);
        let lane = definition_lane(definition);
        let key = secondary_entry_key(
            handle.scope(),
            handle.index_id(),
            handle.generation(),
            definition,
            canonical,
            entity_id,
        )
        .expect("exact entry key validates");
        let value_bytes = if definition_uses_equality_bitmap(definition) {
            SecondaryEqualityBitmapValue::new(roaring::RoaringTreemap::from_iter([entity_id.get()]))
                .encode()
        } else {
            encode_secondary_entry(&SecondaryEntryValue {
                index_id: handle.index_id(),
                generation: handle.generation(),
                lane,
                entity_id,
            })
        };
        db.put(key, value_bytes)
            .await
            .expect("exact entry persists");
        db.put(
            authoritative_property_key(
                handle.scope(),
                IndexEntity {
                    kind: definition.element_kind(),
                    id: entity_id,
                },
            ),
            crate::encoding::v1::property::encode_properties(&[
                Property::string("$label", definition.label().as_str()),
                Property::string(definition.property().as_str(), value),
            ]),
        )
        .await
        .expect("exact authoritative row persists");
    }

    let db = slatedb::Db::builder(
        "secondary-exact-production-contracts",
        Arc::new(InMemory::new()),
    )
    .with_merge_operator(Arc::new(crate::merge_operator::HelixMergeOperator::new()))
    .build()
    .await
    .expect("exact serving database opens");
    let equality = secondary_handle(
        crate::config::SecondaryIndexDefinition::node_equality("User", "value").unwrap(),
    );
    let unique = secondary_handle(
        crate::config::SecondaryIndexDefinition::node_unique_equality("User", "value").unwrap(),
    );
    let range = secondary_handle(
        crate::config::SecondaryIndexDefinition::node_range_desc("User", "value").unwrap(),
    );
    let text_definition = crate::index_lifecycle::ValidatedDynamicIndexDefinition::try_from(
        crate::config::TextIndexDefinition::new_node("User", "value").unwrap(),
    )
    .unwrap();
    let text = active_handle(
        text_definition,
        crate::index_lifecycle::PhysicalGeneration::Text {
            generation: IndexGenerationId::initial(),
        },
    );

    assert!(lookup_active_equality_point_literal(
        &db,
        &text,
        &PropertyValue::String("value".to_string()),
    )
    .await
    .is_err());
    assert!(lookup_active_equality_point_literal(
        &db,
        &range,
        &PropertyValue::String("value".to_string()),
    )
    .await
    .is_err());
    assert!(
        lookup_active_equality_point_literal(&db, &equality, &PropertyValue::Null)
            .await
            .is_err()
    );
    assert!(lookup_active_equality_point_literal(
        &db,
        &equality,
        &PropertyValue::String("missing".to_string()),
    )
    .await
    .unwrap()
    .is_empty());

    put_entry(&db, &equality, "same", 3).await;
    put_entry(&db, &unique, "owner", 7).await;
    assert_eq!(
        lookup_active_equality_point_literal(
            &db,
            &unique,
            &PropertyValue::String("owner".to_string()),
        )
        .await
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>(),
        vec![7]
    );
    assert!(lookup_active_equality_literal_batch(
        &db,
        &equality,
        &[PropertyValue::String("same".to_string())],
    )
    .await
    .is_err());
    assert!(lookup_active_equality_literal_batch(
        &db,
        &text,
        &[
            PropertyValue::String("same".to_string()),
            PropertyValue::String("other".to_string()),
        ],
    )
    .await
    .is_err());
    assert!(lookup_active_equality_literal_batch(
        &db,
        &unique,
        &[
            PropertyValue::String("same".to_string()),
            PropertyValue::String("other".to_string()),
        ],
    )
    .await
    .is_err());
    assert!(lookup_active_equality_literal_batch(
        &db,
        &equality,
        &[
            PropertyValue::String("same".to_string()),
            PropertyValue::Null,
        ],
    )
    .await
    .is_err());
    assert_eq!(
        lookup_active_equality_literal_batch(
            &db,
            &equality,
            &[
                PropertyValue::String("same".to_string()),
                PropertyValue::String("missing".to_string()),
            ],
        )
        .await
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>(),
        vec![3]
    );

    assert!(
        scan_active_range_generation_with_membership(&db, &text, None, None, &[])
            .await
            .is_err()
    );
    assert!(
        scan_active_range_generation_with_membership(&db, &equality, None, None, &[])
            .await
            .is_err()
    );
    assert!(scan_active_range_generation_with_membership(
        &db,
        &range,
        Some(&SecondaryRangeQuery::Between {
            lower: PropertyValue::String("z".to_string()),
            lower_inclusive: true,
            upper: PropertyValue::String("a".to_string()),
            upper_inclusive: true,
        }),
        None,
        &[],
    )
    .await
    .unwrap()
    .is_empty());

    put_entry(&db, &range, "a", 1).await;
    put_entry(&db, &range, "b", 2).await;
    let rejects_all = roaring::RoaringTreemap::new();
    assert!(
        scan_active_range_generation_with_membership(&db, &range, None, None, &[rejects_all],)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        scan_active_range_generation_with_membership(&db, &range, None, Some(1), &[])
            .await
            .unwrap()
            .len(),
        1
    );
    db.close().await.expect("exact serving database closes");
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingRows;

    #[async_trait]
    impl ExactRangeRows for FailingRows {
        async fn next_exact(
            &mut self,
        ) -> std::result::Result<Option<slatedb::KeyValue>, slatedb::Error> {
            Err(slatedb::Error::unavailable(
                "injected exact iterator failure".to_string(),
            ))
        }
    }

    #[tokio::test]
    async fn exact_range_row_contract_propagates_iterator_failure() {
        let db = super::super::tests::test_db("secondary-exact-iterator-error").await;
        let handle = super::super::tests::active_read_handle(
            &db,
            crate::config::SecondaryIndexDefinition::node_range("User", "rank").unwrap(),
        )
        .await;
        let definition = handle.secondary_definition().unwrap();
        assert!(consume_active_range_rows(
            &db,
            &handle,
            definition,
            StorageRangeIndexDirection::Asc,
            definition_lane(definition),
            FailingRows,
            None,
            None,
            &[],
        )
        .await
        .is_err());
        db.close().await.unwrap();
    }
}
