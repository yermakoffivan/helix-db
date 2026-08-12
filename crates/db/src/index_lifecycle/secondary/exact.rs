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
