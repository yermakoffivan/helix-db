//! Storage index lookup contracts for executable access.
//!
//! Configured secondary indexes retain their deployed physical lookup path.
//! Canonical V2 identities and generation-qualified physical rows are resolved
//! through the same stable request snapshot.

use helix_planner::{catalog, ir, properties};
use slatedb::DbReadOps;

use super::super::stream::ast_to_db_value;
use super::super::ExecutionContext;
use crate::encoding::property::property_value::PropertyValue as DbPropertyValue;
use crate::error::{HelixDbError, Result};
#[cfg(test)]
use crate::HelixStorage;

impl<'db> ExecutionContext<'db> {
    pub(super) fn index_value(&self, value: &ir::IndexValue) -> Result<DbPropertyValue> {
        match value {
            ir::IndexValue::Literal(value) => {
                Ok(ast_to_db_value(value.as_property_value().clone()))
            }
            ir::IndexValue::Param(param) => self.param_value(param),
        }
    }

    pub(in crate::execution::interpreter) async fn lookup_equality_index_set(
        &self,
        property: &str,
        value: &DbPropertyValue,
    ) -> Result<roaring::RoaringTreemap> {
        let identity = crate::config::split_scoped_secondary_index_property(property)
            .map(|(label, property)| {
                secondary_identity(
                    crate::index_lifecycle::IndexIdentityFamily::SecondaryEquality,
                    crate::index_lifecycle::IndexElementKind::Node,
                    label,
                    property,
                )
            })
            .transpose()?;
        if let Some(active) = self.active_write_tx() {
            return lookup_equality_in_view(self, &active.txn, identity.as_ref(), property, value)
                .await;
        }
        if let Some(view) = self.request_read_view() {
            return lookup_equality_in_view(self, view, identity.as_ref(), property, value).await;
        }
        #[cfg(test)]
        {
            match self.db.storage() {
                HelixStorage::Reader(reader) => {
                    lookup_equality_in_view(
                        self,
                        reader.as_ref(),
                        identity.as_ref(),
                        property,
                        value,
                    )
                    .await
                }
                HelixStorage::Writer(writer) => {
                    lookup_equality_in_view(self, writer.db(), identity.as_ref(), property, value)
                        .await
                }
            }
        }
        #[cfg(not(test))]
        {
            Err(HelixDbError::InvariantViolation(
                "secondary equality lookup escaped its request read view".to_string(),
            ))
        }
    }

    pub(in crate::execution::interpreter) async fn lookup_managed_equality_union(
        &self,
        element_kind: crate::index_lifecycle::IndexElementKind,
        key: &catalog::ScopedPropertyKey,
        values: &[DbPropertyValue],
    ) -> Result<roaring::RoaringTreemap> {
        let identity = secondary_identity(
            crate::index_lifecycle::IndexIdentityFamily::SecondaryEquality,
            element_kind,
            key.label.as_ref(),
            key.property.as_ref(),
        )?;
        if let Some(active) = self.active_write_tx() {
            return lookup_managed_equalities_in_view(self, &active.txn, &identity, values).await;
        }
        if let Some(view) = self.request_read_view() {
            return lookup_managed_equalities_in_view(self, view, &identity, values).await;
        }
        #[cfg(test)]
        {
            match self.db.storage() {
                HelixStorage::Reader(reader) => {
                    lookup_managed_equalities_in_view(self, reader.as_ref(), &identity, values)
                        .await
                }
                HelixStorage::Writer(writer) => {
                    lookup_managed_equalities_in_view(self, writer.db(), &identity, values).await
                }
            }
        }
        #[cfg(not(test))]
        {
            Err(HelixDbError::InvariantViolation(
                "secondary equality batch escaped its request read view".to_string(),
            ))
        }
    }

    /// Execute a planner-selected literal bitmap batch without key folding.
    pub(in crate::execution::interpreter) async fn lookup_managed_equality_literal_batch(
        &self,
        element_kind: crate::index_lifecycle::IndexElementKind,
        key: &catalog::ScopedPropertyKey,
        values: &[DbPropertyValue],
    ) -> Result<roaring::RoaringTreemap> {
        let identity = secondary_identity(
            crate::index_lifecycle::IndexIdentityFamily::SecondaryEquality,
            element_kind,
            key.label.as_ref(),
            key.property.as_ref(),
        )?;
        if let Some(active) = self.active_write_tx() {
            let Some(handle) = active.index_context.active_handle(&identity) else {
                return Err(secondary_catalog_unavailable());
            };
            return lookup_managed_active_literal_batch(&active.txn, handle, values).await;
        }
        if let Some(view) = self.request_read_view() {
            if let Some(catalog) = self.request_read_index_catalog() {
                let Some(handle) = catalog.handle(&identity) else {
                    return Err(secondary_catalog_unavailable());
                };
                return lookup_managed_active_literal_batch(view, handle, values).await;
            }
            crate::index_lifecycle::secondary::record_equality_point_read();
            let Some(record) = crate::index_lifecycle::repository::load_index_record(
                view,
                self.tenant_scope,
                &identity,
            )
            .await?
            else {
                return Err(secondary_catalog_unavailable());
            };
            let Some(handle) = crate::index_lifecycle::ActiveIndexHandle::try_from_record(
                self.tenant_scope,
                &record,
            ) else {
                return Err(secondary_catalog_unavailable());
            };
            return lookup_managed_active_literal_batch(view, &handle, values).await;
        }
        Err(HelixDbError::InvariantViolation(
            "literal secondary equality batch escaped its request read view".to_string(),
        ))
    }

    /// Execute one exact equality point primitive with a required uniqueness lane.
    pub(in crate::execution::interpreter) async fn lookup_managed_equality_point_exact(
        &self,
        element_kind: crate::index_lifecycle::IndexElementKind,
        key: &catalog::ScopedPropertyKey,
        value: &DbPropertyValue,
        unique: bool,
    ) -> Result<roaring::RoaringTreemap> {
        let identity = secondary_identity(
            crate::index_lifecycle::IndexIdentityFamily::SecondaryEquality,
            element_kind,
            key.label.as_ref(),
            key.property.as_ref(),
        )?;
        if let Some(active) = self.active_write_tx() {
            let Some(handle) = active.index_context.active_handle(&identity) else {
                return Err(secondary_catalog_unavailable());
            };
            return lookup_managed_active_point_exact(&active.txn, handle, value, unique).await;
        }
        if let Some(view) = self.request_read_view() {
            if let Some(catalog) = self.request_read_index_catalog() {
                let Some(handle) = catalog.handle(&identity) else {
                    return Err(secondary_catalog_unavailable());
                };
                return lookup_managed_active_point_exact(view, handle, value, unique).await;
            }
            crate::index_lifecycle::secondary::record_equality_point_read();
            let Some(record) = crate::index_lifecycle::repository::load_index_record(
                view,
                self.tenant_scope,
                &identity,
            )
            .await?
            else {
                return Err(secondary_catalog_unavailable());
            };
            let Some(handle) = crate::index_lifecycle::ActiveIndexHandle::try_from_record(
                self.tenant_scope,
                &record,
            ) else {
                return Err(secondary_catalog_unavailable());
            };
            return lookup_managed_active_point_exact(view, &handle, value, unique).await;
        }
        Err(HelixDbError::InvariantViolation(
            "exact secondary equality point read escaped its request read view".to_string(),
        ))
    }

    pub(in crate::execution::interpreter) async fn lookup_global_edge_label_index(
        &self,
        label: &str,
    ) -> Result<roaring::RoaringTreemap> {
        if let Some(active) = self.active_write_tx() {
            return crate::search::lookup_global_edge_label_index_scoped(
                &active.txn,
                label,
                self.tenant_scope,
            )
            .await;
        }
        if let Some(view) = self.request_read_view() {
            return crate::search::lookup_global_edge_label_index_scoped(
                view,
                label,
                self.tenant_scope,
            )
            .await;
        }
        #[cfg(test)]
        {
            match self.db.storage() {
                HelixStorage::Reader(reader) => {
                    crate::search::lookup_global_edge_label_index_scoped(
                        reader.as_ref(),
                        label,
                        self.tenant_scope,
                    )
                    .await
                }
                HelixStorage::Writer(writer) => {
                    crate::search::lookup_global_edge_label_index_scoped(
                        writer.db(),
                        label,
                        self.tenant_scope,
                    )
                    .await
                }
            }
        }
        #[cfg(not(test))]
        Err(HelixDbError::InvariantViolation(
            "global edge label lookup escaped its request read view".to_string(),
        ))
    }

    #[cfg(any(test, feature = "production-coverage"))]
    pub(super) async fn lookup_global_edge_equality_index(
        &self,
        property: &str,
        value: &DbPropertyValue,
    ) -> Result<roaring::RoaringTreemap> {
        let identity = crate::config::split_scoped_secondary_index_property(property)
            .map(|(label, property)| {
                secondary_identity(
                    crate::index_lifecycle::IndexIdentityFamily::SecondaryEquality,
                    crate::index_lifecycle::IndexElementKind::Edge,
                    label,
                    property,
                )
            })
            .transpose()?;
        if let Some(active) = self.active_write_tx() {
            return lookup_equality_in_view(self, &active.txn, identity.as_ref(), property, value)
                .await;
        }
        if let Some(view) = self.request_read_view() {
            return lookup_equality_in_view(self, view, identity.as_ref(), property, value).await;
        }
        #[cfg(test)]
        {
            match self.db.storage() {
                HelixStorage::Reader(reader) => {
                    lookup_equality_in_view(
                        self,
                        reader.as_ref(),
                        identity.as_ref(),
                        property,
                        value,
                    )
                    .await
                }
                HelixStorage::Writer(writer) => {
                    lookup_equality_in_view(self, writer.db(), identity.as_ref(), property, value)
                        .await
                }
            }
        }
        #[cfg(not(test))]
        {
            Err(HelixDbError::InvariantViolation(
                "edge secondary equality lookup escaped its request read view".to_string(),
            ))
        }
    }

    pub(in crate::execution::interpreter) async fn lookup_out_neighbors_by_label(
        &self,
        node_id: u64,
        label: &str,
    ) -> Result<roaring::RoaringTreemap> {
        if let Some(active) = self.active_write_tx() {
            return crate::search::lookup_out_neighbors_by_label_scoped(
                &active.txn,
                node_id,
                label,
                self.tenant_scope,
            )
            .await;
        }
        if let Some(view) = self.request_read_view() {
            return crate::search::lookup_out_neighbors_by_label_scoped(
                view,
                node_id,
                label,
                self.tenant_scope,
            )
            .await;
        }
        #[cfg(test)]
        {
            match self.db.storage() {
                HelixStorage::Reader(reader) => {
                    crate::search::lookup_out_neighbors_by_label_scoped(
                        reader.as_ref(),
                        node_id,
                        label,
                        self.tenant_scope,
                    )
                    .await
                }
                HelixStorage::Writer(writer) => {
                    crate::search::lookup_out_neighbors_by_label_scoped(
                        writer.db(),
                        node_id,
                        label,
                        self.tenant_scope,
                    )
                    .await
                }
            }
        }
        #[cfg(not(test))]
        Err(HelixDbError::InvariantViolation(
            "out-neighbor label lookup escaped its request read view".to_string(),
        ))
    }

    pub(in crate::execution::interpreter) async fn lookup_in_neighbors_by_label(
        &self,
        node_id: u64,
        label: &str,
    ) -> Result<roaring::RoaringTreemap> {
        if let Some(active) = self.active_write_tx() {
            return crate::search::lookup_in_neighbors_by_label_scoped(
                &active.txn,
                node_id,
                label,
                self.tenant_scope,
            )
            .await;
        }
        if let Some(view) = self.request_read_view() {
            return crate::search::lookup_in_neighbors_by_label_scoped(
                view,
                node_id,
                label,
                self.tenant_scope,
            )
            .await;
        }
        #[cfg(test)]
        {
            match self.db.storage() {
                HelixStorage::Reader(reader) => {
                    crate::search::lookup_in_neighbors_by_label_scoped(
                        reader.as_ref(),
                        node_id,
                        label,
                        self.tenant_scope,
                    )
                    .await
                }
                HelixStorage::Writer(writer) => {
                    crate::search::lookup_in_neighbors_by_label_scoped(
                        writer.db(),
                        node_id,
                        label,
                        self.tenant_scope,
                    )
                    .await
                }
            }
        }
        #[cfg(not(test))]
        Err(HelixDbError::InvariantViolation(
            "in-neighbor label lookup escaped its request read view".to_string(),
        ))
    }

    pub(super) async fn lookup_edge_pair_index(
        &self,
        from: u64,
        to: u64,
    ) -> Result<roaring::RoaringTreemap> {
        if let Some(active) = self.active_write_tx() {
            return crate::search::lookup_edge_pair_index_scoped(
                &active.txn,
                from,
                to,
                self.tenant_scope,
            )
            .await;
        }
        if let Some(view) = self.request_read_view() {
            return crate::search::lookup_edge_pair_index_scoped(view, from, to, self.tenant_scope)
                .await;
        }
        #[cfg(test)]
        {
            match self.db.storage() {
                HelixStorage::Reader(reader) => {
                    crate::search::lookup_edge_pair_index_scoped(
                        reader.as_ref(),
                        from,
                        to,
                        self.tenant_scope,
                    )
                    .await
                }
                HelixStorage::Writer(writer) => {
                    crate::search::lookup_edge_pair_index_scoped(
                        writer.db(),
                        from,
                        to,
                        self.tenant_scope,
                    )
                    .await
                }
            }
        }
        #[cfg(not(test))]
        Err(HelixDbError::InvariantViolation(
            "edge pair lookup escaped its request read view".to_string(),
        ))
    }

    pub(in crate::execution::interpreter) async fn get_edge_endpoints(
        &self,
        edge_id: u64,
    ) -> Result<Option<(u64, u64)>> {
        #[cfg(test)]
        self.record_endpoint_get();
        if let Some(active) = self.active_write_tx() {
            return crate::search::get_edge_endpoints_scoped(
                &active.txn,
                edge_id,
                self.tenant_scope,
            )
            .await;
        }
        if let Some(view) = self.request_read_view() {
            return crate::search::get_edge_endpoints_scoped(view, edge_id, self.tenant_scope)
                .await;
        }
        #[cfg(test)]
        {
            match self.db.storage() {
                HelixStorage::Reader(reader) => {
                    crate::search::get_edge_endpoints_scoped(
                        reader.as_ref(),
                        edge_id,
                        self.tenant_scope,
                    )
                    .await
                }
                HelixStorage::Writer(writer) => {
                    crate::search::get_edge_endpoints_scoped(
                        writer.db(),
                        edge_id,
                        self.tenant_scope,
                    )
                    .await
                }
            }
        }
        #[cfg(not(test))]
        Err(HelixDbError::InvariantViolation(
            "edge endpoint lookup escaped its request read view".to_string(),
        ))
    }
}

/// Constructs the canonical V2 identity corresponding to one planner key.
fn secondary_identity(
    family: crate::index_lifecycle::IndexIdentityFamily,
    element_kind: crate::index_lifecycle::IndexElementKind,
    label: &str,
    property: &str,
) -> Result<crate::index_lifecycle::IndexIdentity> {
    Ok(crate::index_lifecycle::IndexIdentity::new(
        family,
        element_kind,
        crate::index_lifecycle::IndexComponent::try_new("label", label)?,
        crate::index_lifecycle::IndexComponent::try_new("property", property)?,
    ))
}

/// Resolves built-in label rows or one exact managed dynamic equality identity.
async fn lookup_equality_in_view(
    context: &ExecutionContext<'_>,
    reader: &(impl DbReadOps + Send + Sync),
    identity: Option<&crate::index_lifecycle::IndexIdentity>,
    property: &str,
    value: &DbPropertyValue,
) -> Result<roaring::RoaringTreemap> {
    if let Some(identity) = identity {
        return lookup_managed_equality_in_view(context, reader, identity, value).await;
    }
    if property != "$label" {
        return Err(HelixDbError::IndexLifecycleUnavailable {
            family: crate::error::IndexFamily::Secondary,
            reason: crate::error::IndexLifecycleUnavailableReason::CanonicalStateUnavailable,
        });
    }
    let Some(value) = value.as_str() else {
        return Ok(roaring::RoaringTreemap::new());
    };
    crate::search::lookup_equality_index_set_scoped(reader, property, value, context.tenant_scope)
        .await
}

/// Resolves one request-authorized equality generation and reads its physical row.
async fn lookup_managed_equality_in_view(
    context: &ExecutionContext<'_>,
    reader: &(impl DbReadOps + Send + Sync),
    identity: &crate::index_lifecycle::IndexIdentity,
    value: &DbPropertyValue,
) -> Result<roaring::RoaringTreemap> {
    lookup_managed_equalities_in_view(context, reader, identity, core::slice::from_ref(value)).await
}

async fn lookup_managed_equalities_in_view(
    context: &ExecutionContext<'_>,
    reader: &(impl DbReadOps + Send + Sync),
    identity: &crate::index_lifecycle::IndexIdentity,
    values: &[DbPropertyValue],
) -> Result<roaring::RoaringTreemap> {
    if let Some(active_write) = context.active_write_tx() {
        let Some(active) = active_write.index_context.active_handle(identity) else {
            return Err(HelixDbError::IndexLifecycleUnavailable {
                family: crate::error::IndexFamily::Secondary,
                reason: crate::error::IndexLifecycleUnavailableReason::CanonicalStateUnavailable,
            });
        };
        return lookup_managed_active_equalities_in_view(reader, active, values).await;
    }

    if let Some(catalog) = context.request_read_index_catalog() {
        let Some(active) = catalog.handle(identity) else {
            return Err(HelixDbError::IndexLifecycleUnavailable {
                family: crate::error::IndexFamily::Secondary,
                reason: crate::error::IndexLifecycleUnavailableReason::CanonicalStateUnavailable,
            });
        };
        return lookup_managed_active_equalities_in_view(reader, active, values).await;
    }

    crate::index_lifecycle::secondary::record_equality_point_read();
    let Some(record) = crate::index_lifecycle::repository::load_index_record(
        reader,
        context.tenant_scope,
        identity,
    )
    .await?
    else {
        return Err(HelixDbError::IndexLifecycleUnavailable {
            family: crate::error::IndexFamily::Secondary,
            reason: crate::error::IndexLifecycleUnavailableReason::CanonicalStateUnavailable,
        });
    };
    let Some(active) =
        crate::index_lifecycle::ActiveIndexHandle::try_from_record(context.tenant_scope, &record)
    else {
        return Err(HelixDbError::IndexLifecycleUnavailable {
            family: crate::error::IndexFamily::Secondary,
            reason: crate::error::IndexLifecycleUnavailableReason::CanonicalStateUnavailable,
        });
    };
    lookup_managed_active_equalities_in_view(reader, &active, values).await
}

async fn lookup_managed_active_equalities_in_view(
    reader: &(impl DbReadOps + Send + Sync),
    active: &crate::index_lifecycle::ActiveIndexHandle,
    values: &[DbPropertyValue],
) -> Result<roaring::RoaringTreemap> {
    if !matches!(
        active,
        crate::index_lifecycle::ActiveIndexHandle::Secondary { .. }
    ) {
        return Err(HelixDbError::IndexCatalogCorruption(
            "secondary equality identity resolved another Active family".to_string(),
        ));
    }
    crate::index_lifecycle::secondary::lookup_active_equality_generations(reader, active, values)
        .await
}

async fn lookup_managed_active_literal_batch(
    reader: &(impl DbReadOps + Send + Sync),
    active: &crate::index_lifecycle::ActiveIndexHandle,
    values: &[DbPropertyValue],
) -> Result<roaring::RoaringTreemap> {
    crate::index_lifecycle::secondary::lookup_active_equality_literal_batch(reader, active, values)
        .await
}

async fn lookup_managed_active_point_exact(
    reader: &(impl DbReadOps + Send + Sync),
    active: &crate::index_lifecycle::ActiveIndexHandle,
    value: &DbPropertyValue,
    unique: bool,
) -> Result<roaring::RoaringTreemap> {
    let Some(definition) = active.secondary_definition() else {
        return Err(HelixDbError::IndexCatalogCorruption(
            "exact equality point resolved another Active family".to_string(),
        ));
    };
    let lane_matches = match definition {
        crate::index_lifecycle::ValidatedSecondaryIndexDefinition::NodeEquality {
            unique: actual,
            ..
        } => *actual == unique,
        crate::index_lifecycle::ValidatedSecondaryIndexDefinition::EdgeEquality { .. } => !unique,
        crate::index_lifecycle::ValidatedSecondaryIndexDefinition::NodeRange { .. }
        | crate::index_lifecycle::ValidatedSecondaryIndexDefinition::EdgeRange { .. } => false,
    };
    if !lane_matches {
        return Err(HelixDbError::IndexCatalogCorruption(
            "planner equality uniqueness disagrees with its Active secondary definition"
                .to_string(),
        ));
    }
    crate::index_lifecycle::secondary::lookup_active_equality_point_literal(reader, active, value)
        .await
}

fn secondary_catalog_unavailable() -> HelixDbError {
    HelixDbError::IndexLifecycleUnavailable {
        family: crate::error::IndexFamily::Secondary,
        reason: crate::error::IndexLifecycleUnavailableReason::CanonicalStateUnavailable,
    }
}

pub(super) fn limited_index_ids(
    ids: roaring::RoaringTreemap,
    limit: Option<properties::PositiveUsize>,
) -> Vec<u64> {
    match limit {
        Some(limit) => ids.into_iter().take(limit.get()).collect(),
        None => ids.into_iter().collect(),
    }
}

#[cfg(any(test, feature = "production-coverage"))]
pub(super) fn scoped_property_key(key: &catalog::ScopedPropertyKey) -> String {
    crate::config::scoped_secondary_index_property(key.label.as_ref(), key.property.as_ref())
}

#[cfg(any(test, feature = "production-coverage"))]
#[cfg_attr(all(feature = "production-coverage", not(test)), allow(dead_code))]
pub(super) mod tests {
    use super::super::super::test_support;
    use super::*;
    use helix_ast::query::QueryValue;
    use helix_ast::value::PropertyValue;
    use helix_planner::context;

    fn name(value: &str) -> ir::NonEmptyString {
        test_support::name(value)
    }

    fn positive(value: usize) -> properties::PositiveUsize {
        properties::PositiveUsize::new(value).expect("positive test limit")
    }

    #[cfg_attr(test, tokio::test)]
    async fn index_value_converts_literals_and_runtime_parameters() {
        let db = test_support::open_db("access-index-value-conversion").await;
        let static_param = name("static_age");
        let dynamic_param = name("dynamic_name");
        let ctx = ExecutionContext::new(
            &db,
            context::ParamBindings::default()
                .with_value(static_param.clone(), PropertyValue::I64(42))
                .with_query_value(
                    dynamic_param.clone(),
                    QueryValue::String("alice".to_string()),
                ),
        );
        let literal = ir::IndexValue::Literal(
            ir::SecondaryIndexLiteral::new(PropertyValue::from("active"))
                .expect("literal is secondary-index compatible"),
        );

        assert_eq!(
            ctx.index_value(&literal).expect("literal converts"),
            DbPropertyValue::String("active".to_string())
        );
        assert_eq!(
            ctx.index_value(&ir::IndexValue::Param(static_param))
                .expect("static parameter converts"),
            DbPropertyValue::I64(42)
        );
        assert_eq!(
            ctx.index_value(&ir::IndexValue::Param(dynamic_param))
                .expect("dynamic parameter converts"),
            DbPropertyValue::String("alice".to_string())
        );
    }

    #[cfg_attr(test, tokio::test)]
    async fn index_value_rejects_missing_parameters() {
        let db = test_support::open_db("access-index-value-missing-param").await;
        let ctx = ExecutionContext::new(&db, context::ParamBindings::default());
        let err = ctx
            .index_value(&ir::IndexValue::Param(name("missing")))
            .expect_err("missing parameter should fail");

        assert!(err.to_string().contains("parameter `missing` is not bound"));
    }

    #[cfg_attr(test, test)]
    fn limited_index_ids_preserve_storage_order_and_apply_positive_limits() {
        let ids = roaring::RoaringTreemap::from_iter([9, 1, 5, 3]);

        assert_eq!(limited_index_ids(ids.clone(), None), vec![1, 3, 5, 9]);
        assert_eq!(limited_index_ids(ids, Some(positive(2))), vec![1, 3]);
    }

    #[cfg_attr(test, test)]
    fn scoped_property_key_uses_internal_secondary_index_scope() {
        let key = catalog::ScopedPropertyKey::try_new("User", "email")
            .expect("valid scoped property key");

        assert_eq!(
            scoped_property_key(&key),
            crate::config::scoped_secondary_index_property("User", "email")
        );
    }

    #[cfg(test)]
    #[tokio::test]
    async fn direct_storage_dispatches_all_index_lookup_contracts() {
        let config = test_support::in_memory_config("access-reader-index-lookups")
            .with_equality_index("User", "status")
            .with_edge_equality_index("FOLLOWS", "status");
        let writer = test_support::open_db_with_config(config.clone()).await;
        let alice = test_support::add_node_with_properties(
            &writer,
            "User",
            vec![("status", PropertyValue::from("active"))],
        )
        .await;
        let bob = test_support::add_user(&writer, "bob").await;
        let edge = test_support::add_edge_with_properties(
            &writer,
            alice,
            bob,
            "FOLLOWS",
            vec![("status", PropertyValue::from("active"))],
        )
        .await;
        let writer_context = ExecutionContext::new(&writer, context::ParamBindings::default());
        assert_eq!(
            writer_context
                .lookup_global_edge_equality_index(
                    &crate::config::scoped_secondary_index_property("FOLLOWS", "status"),
                    &DbPropertyValue::String("active".to_string()),
                )
                .await
                .expect("writer edge equality lookup succeeds")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![edge]
        );
        drop(writer);
        let reader = test_support::open_reader_with_config(config).await;
        let context = ExecutionContext::new(&reader, context::ParamBindings::default());

        assert_eq!(
            context
                .lookup_equality_index_set(
                    &crate::config::scoped_secondary_index_property("User", "status"),
                    &DbPropertyValue::String("active".to_string()),
                )
                .await
                .expect("reader node equality lookup succeeds")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![alice]
        );
        assert_eq!(
            context
                .lookup_global_edge_label_index("FOLLOWS")
                .await
                .expect("reader global edge label lookup succeeds")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![edge]
        );
        assert_eq!(
            context
                .lookup_global_edge_equality_index(
                    &crate::config::scoped_secondary_index_property("FOLLOWS", "status"),
                    &DbPropertyValue::String("active".to_string()),
                )
                .await
                .expect("reader global edge equality lookup succeeds")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![edge]
        );
        assert_eq!(
            context
                .lookup_out_neighbors_by_label(alice, "FOLLOWS")
                .await
                .expect("reader out-neighbor lookup succeeds")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![bob]
        );
        assert_eq!(
            context
                .lookup_in_neighbors_by_label(bob, "FOLLOWS")
                .await
                .expect("reader in-neighbor lookup succeeds")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![alice]
        );
        assert_eq!(
            context
                .lookup_edge_pair_index(alice, bob)
                .await
                .expect("reader edge-pair lookup succeeds")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![edge]
        );
        assert_eq!(
            context
                .get_edge_endpoints(edge)
                .await
                .expect("reader endpoint lookup succeeds"),
            Some((alice, bob))
        );
    }

    #[cfg_attr(test, tokio::test)]
    async fn active_transaction_dispatches_index_lookup_contracts() {
        let config = test_support::in_memory_config("access-active-index-lookups")
            .with_equality_index("User", "status")
            .with_edge_equality_index("FOLLOWS", "status");
        let db = test_support::open_db_with_config(config).await;
        let alice = test_support::add_node_with_properties(
            &db,
            "User",
            vec![("status", PropertyValue::from("active"))],
        )
        .await;
        let bob = test_support::add_user(&db, "bob").await;
        let edge = test_support::add_edge_with_properties(
            &db,
            alice,
            bob,
            "FOLLOWS",
            vec![("status", PropertyValue::from("active"))],
        )
        .await;
        let mut context = ExecutionContext::new(&db, context::ParamBindings::default());
        context
            .enable_request_write_scope()
            .await
            .expect("transaction and its exact mutation catalog open together");

        assert_eq!(
            context
                .lookup_equality_index_set(
                    &crate::config::scoped_secondary_index_property("User", "status"),
                    &DbPropertyValue::String("active".to_string()),
                )
                .await
                .expect("transaction node equality lookup succeeds")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![alice]
        );
        assert_eq!(
            context
                .lookup_global_edge_equality_index(
                    &crate::config::scoped_secondary_index_property("FOLLOWS", "status"),
                    &DbPropertyValue::String("active".to_string()),
                )
                .await
                .expect("transaction edge equality lookup succeeds")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![edge]
        );
        assert_eq!(
            context
                .lookup_in_neighbors_by_label(bob, "FOLLOWS")
                .await
                .expect("transaction in-neighbor lookup succeeds")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![alice]
        );
        assert_eq!(
            context
                .get_edge_endpoints(edge)
                .await
                .expect("transaction endpoint lookup succeeds"),
            Some((alice, bob))
        );
        context.abort_request_write_scope();
    }

    #[cfg_attr(test, tokio::test)]
    async fn request_snapshot_excludes_concurrent_edge_index_and_endpoint_phantoms() {
        let db = test_support::open_db("access-edge-index-request-snapshot").await;
        let alice = test_support::add_user(&db, "alice").await;
        let bob = test_support::add_user(&db, "bob").await;
        let carol = test_support::add_user(&db, "carol").await;
        let original = test_support::add_edge(&db, alice, bob, "FOLLOWS").await;
        let mut context = ExecutionContext::new(&db, context::ParamBindings::default());
        context
            .enable_request_read_view()
            .await
            .expect("request snapshot opens");

        let concurrent = test_support::add_edge(&db, alice, carol, "FOLLOWS").await;

        assert_eq!(
            context
                .lookup_global_edge_label_index("FOLLOWS")
                .await
                .expect("global label lookup uses the request snapshot")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![original]
        );
        assert_eq!(
            context
                .lookup_out_neighbors_by_label(alice, "FOLLOWS")
                .await
                .expect("out-neighbor lookup uses the request snapshot")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![bob]
        );
        assert!(context
            .lookup_in_neighbors_by_label(carol, "FOLLOWS")
            .await
            .expect("in-neighbor lookup uses the request snapshot")
            .is_empty());
        assert!(context
            .lookup_edge_pair_index(alice, carol)
            .await
            .expect("edge-pair lookup uses the request snapshot")
            .is_empty());
        assert_eq!(
            context
                .get_edge_endpoints(concurrent)
                .await
                .expect("endpoint lookup uses the request snapshot"),
            None
        );
        context
            .close_request_read_view()
            .expect("request snapshot closes");
    }

    #[cfg(all(feature = "production-coverage", not(test)))]
    pub(in crate::execution::interpreter::access) async fn run_production_contracts() {
        index_value_converts_literals_and_runtime_parameters().await;
        index_value_rejects_missing_parameters().await;
        limited_index_ids_preserve_storage_order_and_apply_positive_limits();
        scoped_property_key_uses_internal_secondary_index_scope();
        active_transaction_dispatches_index_lookup_contracts().await;
        request_snapshot_excludes_concurrent_edge_index_and_endpoint_phantoms().await;
    }
}
