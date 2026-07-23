use super::{QueryModifiers, StorageLocator, WardrobeEngine};
use crate::wrdb_lib::command::dispatch as command_dispatch;
use crate::wrdb_lib::database::Database;
use crate::wrdb_lib::drawer::{
    Drawer, VacuumReport, delete_rules, hydration, nested_decomposition, relationship,
};
use crate::wrdb_lib::pointer;
use crate::wrdb_lib::query;
use crate::wrdb_lib::routing::{self, DatabaseRoute, ExecutionContext};
use crate::wrdb_lib::wal;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{Error, ErrorKind, Result};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use uuid::Uuid;

#[derive(Default)]
struct RequestHydrationCache {
    records: hydration::HydrationCache,
    virtual_children: HashMap<VirtualRelationshipCacheKey, Vec<Value>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct VirtualRelationshipCacheKey {
    target_drawer: String,
    mapped_by: String,
    parent_pointer: String,
    include_ids: bool,
}

impl WardrobeEngine {
    pub(super) fn database_for_route(&self, route: DatabaseRoute) -> Result<Arc<RwLock<Database>>> {
        let storage_path = route.storage_path(&self.root_directory)?;

        if let Some(database) = Self::read_lock(&self.routed_databases)?
            .get(&route)
            .cloned()
        {
            return Ok(database);
        }

        let mut routed_databases = Self::write_lock(&self.routed_databases)?;
        if !routed_databases.contains_key(&route) {
            let database = Database::initialize_with_cache_limit_wal_thresholds_and_durability(
                storage_path,
                self.max_cached_drawers,
                self.wal_size_threshold_bytes,
                self.wal_ops_threshold_count,
                self.durability_policy.clone(),
            )?;
            let database = Arc::new(RwLock::new(database));
            wal::recover_database::<Self>(&database)?;
            routed_databases.insert(route.clone(), database);
        }

        routed_databases.get(&route).cloned().ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                "Failed to acquire routed database handle",
            )
        })
    }

    pub(super) fn read_lock<T>(lock: &RwLock<T>) -> Result<RwLockReadGuard<'_, T>> {
        lock.read()
            .map_err(|_| Error::other("Wardrobe lock was poisoned during read"))
    }

    pub(super) fn write_lock<T>(lock: &RwLock<T>) -> Result<RwLockWriteGuard<'_, T>> {
        lock.write()
            .map_err(|_| Error::other("Wardrobe lock was poisoned during write"))
    }

    fn load_drawer_handle(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        primary_key: &str,
        unique_constraints: Vec<String>,
    ) -> Result<Arc<RwLock<Drawer>>> {
        let mut database = Self::write_lock(database_core)?;
        database.load_drawer(drawer_name, primary_key, unique_constraints)?;
        database.use_drawer(drawer_name).ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("Drawer '{}' could not be loaded", drawer_name),
            )
        })
    }

    fn active_drawer_handle_or_load_from_disk(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        primary_key: &str,
        unique_constraints: Vec<String>,
    ) -> Result<Option<Arc<RwLock<Drawer>>>> {
        if let Some(drawer) = Self::read_lock(database_core)?.use_drawer(drawer_name) {
            return Ok(Some(drawer));
        }

        let mut database = Self::write_lock(database_core)?;
        database.active_drawer_or_load_from_disk(drawer_name, primary_key, unique_constraints)
    }

    pub(super) fn upsert_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<String> {
        let wal_payload = payload.clone();
        wal::run_upsert_transaction(database_core, drawer_name, &wal_payload, context, || {
            Self::upsert_in_database_unlogged(database_core, drawer_name, payload, context)
        })
    }

    pub(super) fn bulk_upsert_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        records: Vec<Value>,
        atomic: bool,
        context: ExecutionContext<'_>,
    ) -> Result<Vec<String>> {
        if !atomic {
            let mut pointers = Vec::with_capacity(records.len());
            for record in records {
                pointers.push(Self::upsert_in_database(
                    database_core,
                    drawer_name,
                    record,
                    context,
                )?);
            }
            return Ok(pointers);
        }

        let wal_records = records.clone();
        wal::run_bulk_upsert_transaction(database_core, drawer_name, &wal_records, context, || {
            Self::bulk_upsert_in_database_unlogged(database_core, drawer_name, records, context)
        })
    }

    fn bulk_upsert_in_database_unlogged(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        records: Vec<Value>,
        context: ExecutionContext<'_>,
    ) -> Result<Vec<String>> {
        let target_primary_key = "_id";
        let physical_drawer_name =
            routing::scoped_drawer_name(drawer_name, context.drawer_namespace);
        let mut pointers = Vec::with_capacity(records.len());
        let mut prepared_records = Vec::with_capacity(records.len());

        for payload in records {
            let Value::Object(map) = payload else {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Payload root must be a JSON object",
                ));
            };

            let record_key = match map.get(target_primary_key).and_then(|v| v.as_str()) {
                Some(existing_id) => {
                    pointer::normalize_primary_key(&physical_drawer_name, drawer_name, existing_id)
                }
                None => Uuid::new_v4().simple().to_string(),
            };
            let record_pointer = pointer::format_pointer(&physical_drawer_name, &record_key);
            pointers.push(record_pointer.clone());
            prepared_records.push((record_key, map, record_pointer));
        }

        let drawer_handle = Self::load_drawer_handle(
            database_core,
            &physical_drawer_name,
            target_primary_key,
            Vec::new(),
        )?;
        let declared_relationship_constraints =
            Self::read_lock(&drawer_handle)?.relationship_constraints();
        for (_, map, _) in &prepared_records {
            nested_decomposition::validate_declared_relationship_targets(
                map,
                &physical_drawer_name,
                &declared_relationship_constraints,
                context,
            )?;
        }
        let mut full_records = Vec::with_capacity(prepared_records.len());
        let mut reverse_records = Vec::with_capacity(prepared_records.len());

        for (record_key, map, record_pointer) in prepared_records {
            let mut relationship_constraints =
                Self::read_lock(&drawer_handle)?.relationship_constraints();
            nested_decomposition::register_inline_relationship_aliases(
                &map,
                &mut relationship_constraints,
                |field_name, rule| {
                    Self::write_lock(&drawer_handle)?
                        .register_relationship_constraint(field_name, rule)
                        .map_err(|error| Error::new(ErrorKind::InvalidData, error))
                },
            )?;
            let processed_map = nested_decomposition::decompose_nested_objects(
                map,
                &physical_drawer_name,
                &record_pointer,
                &relationship_constraints,
                context,
                |drawer_name, value, child_context, hidden_field| {
                    Self::upsert_child_in_database_unlogged(
                        database_core,
                        drawer_name,
                        value,
                        child_context,
                        hidden_field,
                    )
                },
            )?;

            let mut full_record = processed_map;
            full_record.insert(
                target_primary_key.to_string(),
                Value::String(record_key.clone()),
            );
            let full_record_value = Value::Object(full_record);
            reverse_records.push((record_key, full_record_value.clone()));
            full_records.push(full_record_value);
        }

        let (relationship_constraints, delete_rules, upsert_result) = {
            let mut drawer = Self::write_lock(&drawer_handle)?;
            let relationship_constraints = drawer.relationship_constraints();
            let delete_rules = drawer.delete_rules();
            let upsert_result = drawer.upsert_records_atomic(full_records)?;
            (relationship_constraints, delete_rules, upsert_result)
        };

        match upsert_result {
            Ok(_) => {
                Self::write_lock(database_core)?.replace_reverse_relationships_for_records(
                    &physical_drawer_name,
                    &reverse_records,
                    &relationship_constraints,
                    &delete_rules,
                )?;
                Self::read_lock(database_core)?.mark_drawer_mutated(&physical_drawer_name);
                Ok(pointers)
            }
            Err(validation_error) => Err(Error::new(ErrorKind::InvalidData, validation_error)),
        }
    }

    fn upsert_child_in_database_unlogged(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
        hidden_field: Option<&str>,
    ) -> Result<String> {
        let Some(hidden_field) = hidden_field else {
            return Self::upsert_in_database_unlogged(database_core, drawer_name, payload, context);
        };

        let physical_drawer_name =
            routing::scoped_drawer_name(drawer_name, context.drawer_namespace);
        let drawer_handle =
            Self::load_drawer_handle(database_core, &physical_drawer_name, "_id", Vec::new())?;
        Self::write_lock(&drawer_handle)?.mark_hidden_field(hidden_field)?;

        let mut payload = payload;
        if let Value::Object(map) = &payload {
            if map.len() == 2 && map.contains_key(hidden_field) {
                if let Some(record_id) = map.get("_id").and_then(Value::as_str) {
                    let record_key = pointer::normalize_primary_key(
                        &physical_drawer_name,
                        drawer_name,
                        record_id,
                    );
                    let existing_record = Self::write_lock(&drawer_handle)?
                        .find_by_primary_key_with_migration(&record_key)?;

                    if let Some(Value::Object(mut existing_map)) = existing_record {
                        if let Some(parent_pointer) = map.get(hidden_field) {
                            existing_map.insert(hidden_field.to_string(), parent_pointer.clone());
                            payload = Value::Object(existing_map);
                        }
                    }
                }
            }
        }

        Self::upsert_in_database_unlogged(database_core, drawer_name, payload, context)
    }

    fn upsert_in_database_unlogged(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<String> {
        if let Value::Object(map) = payload {
            let target_primary_key = "_id";
            let physical_drawer_name =
                routing::scoped_drawer_name(drawer_name, context.drawer_namespace);

            let record_key = match map.get(target_primary_key).and_then(|v| v.as_str()) {
                Some(existing_id) => {
                    pointer::normalize_primary_key(&physical_drawer_name, drawer_name, existing_id)
                }
                None => Uuid::new_v4().simple().to_string(),
            };
            let record_pointer = pointer::format_pointer(&physical_drawer_name, &record_key);

            let drawer_handle = Self::load_drawer_handle(
                database_core,
                &physical_drawer_name,
                target_primary_key,
                Vec::new(),
            )?;
            let mut relationship_constraints =
                Self::read_lock(&drawer_handle)?.relationship_constraints();
            nested_decomposition::validate_declared_relationship_targets(
                &map,
                &physical_drawer_name,
                &relationship_constraints,
                context,
            )?;
            nested_decomposition::register_inline_relationship_aliases(
                &map,
                &mut relationship_constraints,
                |field_name, rule| {
                    Self::write_lock(&drawer_handle)?
                        .register_relationship_constraint(field_name, rule)
                        .map_err(|error| Error::new(ErrorKind::InvalidData, error))
                },
            )?;
            let processed_map = nested_decomposition::decompose_nested_objects(
                map,
                &physical_drawer_name,
                &record_pointer,
                &relationship_constraints,
                context,
                |drawer_name, value, child_context, hidden_field| {
                    Self::upsert_child_in_database_unlogged(
                        database_core,
                        drawer_name,
                        value,
                        child_context,
                        hidden_field,
                    )
                },
            )?;

            let mut full_record = processed_map;
            full_record.insert(
                target_primary_key.to_string(),
                Value::String(record_key.clone()),
            );
            let full_record_value = Value::Object(full_record);

            let (relationship_constraints, delete_rules, upsert_result) = {
                let mut drawer = Self::write_lock(&drawer_handle)?;
                let relationship_constraints = drawer.relationship_constraints();
                let delete_rules = drawer.delete_rules();
                let upsert_result = drawer.upsert_record(full_record_value.clone())?;
                (relationship_constraints, delete_rules, upsert_result)
            };

            match upsert_result {
                Ok(_) => {
                    Self::write_lock(database_core)?.replace_reverse_relationships_for_record(
                        &physical_drawer_name,
                        &record_key,
                        &full_record_value,
                        &relationship_constraints,
                        &delete_rules,
                    )?;
                    Self::read_lock(database_core)?.mark_drawer_mutated(&physical_drawer_name);
                    Ok(record_pointer)
                }
                Err(validation_error) => Err(Error::new(ErrorKind::InvalidData, validation_error)),
            }
        } else {
            Err(Error::new(
                ErrorKind::InvalidInput,
                "Payload root must be a JSON object",
            ))
        }
    }

    pub(super) fn find_all_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        context: ExecutionContext<'_>,
        hydrate: bool,
    ) -> std::io::Result<Vec<Value>> {
        let physical_drawer_name =
            routing::scoped_drawer_name(drawer_name, context.drawer_namespace);
        let mut records = if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )? {
            Self::write_lock(&drawer)?.find_all_records_with_migration()?
        } else {
            Vec::new()
        };

        if hydrate {
            let mut hydration_cache = RequestHydrationCache::default();
            hydration::hydrate_records_with_cache(
                &mut records,
                true,
                &mut hydration_cache.records,
                |drawer_name, record_key| {
                    Self::fetch_record_for_hydration(database_core, drawer_name, record_key)
                },
            )?;
            Self::attach_virtual_relationships(
                database_core,
                &physical_drawer_name,
                &mut records,
                true,
                context,
                &mut hydration_cache,
            )?;
        }
        Self::remove_hidden_fields_from_records(
            database_core,
            &physical_drawer_name,
            &mut records,
        )?;

        Ok(records)
    }

    pub(super) fn find_by_filter_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        filter: Value,
        modifiers: Option<QueryModifiers>,
        context: ExecutionContext<'_>,
        hydrate: bool,
    ) -> Result<Vec<Value>> {
        let filter_map = query::filter_map(&filter)?;
        let physical_drawer_name =
            routing::scoped_drawer_name(drawer_name, context.drawer_namespace);

        let mut records = if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )? {
            let mut drawer = Self::write_lock(&drawer)?;
            if let Some(offsets) = drawer.indexed_candidate_offsets(filter_map)? {
                drawer.records_at_offsets_with_migration(offsets)?
            } else {
                drawer.find_all_records_with_migration()?
            }
        } else {
            Vec::new()
        };

        records.retain(|record| {
            query::record_matches_filter(record, filter_map, context.drawer_namespace)
        });
        query::apply_query_modifiers(&mut records, modifiers.as_ref());
        if hydrate {
            let mut hydration_cache = RequestHydrationCache::default();
            hydration::hydrate_records_with_cache(
                &mut records,
                true,
                &mut hydration_cache.records,
                |drawer_name, record_key| {
                    Self::fetch_record_for_hydration(database_core, drawer_name, record_key)
                },
            )?;
            Self::attach_virtual_relationships(
                database_core,
                &physical_drawer_name,
                &mut records,
                true,
                context,
                &mut hydration_cache,
            )?;
        }
        Self::remove_hidden_fields_from_records(
            database_core,
            &physical_drawer_name,
            &mut records,
        )?;

        Ok(records)
    }

    pub(super) fn count_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        filter: Option<Value>,
        _modifiers: Option<QueryModifiers>,
        context: ExecutionContext<'_>,
    ) -> Result<usize> {
        let physical_drawer_name =
            routing::scoped_drawer_name(drawer_name, context.drawer_namespace);
        let Some(filter) = filter else {
            let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
                database_core,
                &physical_drawer_name,
                "_id",
                Vec::new(),
            )?
            else {
                return Ok(0);
            };

            return Ok(Self::read_lock(&drawer)?.record_count());
        };

        let filter_map = query::filter_map(&filter)?;
        let count = if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )? {
            let mut drawer = Self::write_lock(&drawer)?;
            if let Some(offsets) = drawer.indexed_candidate_offsets(filter_map)? {
                offsets.len()
            } else {
                drawer
                    .find_all_records_with_migration()?
                    .into_iter()
                    .filter(|record| {
                        query::record_matches_filter(record, filter_map, context.drawer_namespace)
                    })
                    .count()
            }
        } else {
            0
        };

        Ok(count)
    }

    pub(super) fn vacuum_drawer_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        context: ExecutionContext<'_>,
    ) -> Result<VacuumReport> {
        let physical_drawer_name =
            routing::scoped_drawer_name(drawer_name, context.drawer_namespace);
        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )?
        else {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Drawer '{}' was not found", drawer_name),
            ));
        };

        Self::write_lock(&drawer)?.vacuum()
    }

    pub(super) fn migrate_drawer_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        context: ExecutionContext<'_>,
    ) -> Result<VacuumReport> {
        let physical_drawer_name =
            routing::scoped_drawer_name(drawer_name, context.drawer_namespace);
        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )?
        else {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Drawer '{}' was not found", drawer_name),
            ));
        };

        Self::write_lock(&drawer)?.migrate_all_records()
    }

    pub(super) fn find_by_id_in_database(
        database_core: &RwLock<Database>,
        pointer: &str,
        context: ExecutionContext<'_>,
        hydrate: bool,
    ) -> Result<Option<Value>> {
        let physical_pointer = routing::scoped_pointer(pointer, context.drawer_namespace);
        let (drawer_name, record_key) = pointer::parse_pointer(&physical_pointer)?;

        if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &drawer_name,
            "_id",
            Vec::new(),
        )? {
            let found_record =
                Self::write_lock(&drawer)?.find_by_primary_key_with_migration(&record_key)?;
            if let Some(mut record) = found_record {
                if hydrate {
                let mut active_pointer_path = HashSet::from([physical_pointer]);
                let mut hydration_cache = RequestHydrationCache::default();
                hydration::hydrate_value_with_cache(
                    &mut record,
                    false,
                    &mut active_pointer_path,
                    &mut hydration_cache.records,
                    &mut |drawer_name, record_key| {
                        Self::fetch_record_for_hydration(database_core, drawer_name, record_key)
                    },
                )?;
                Self::attach_virtual_relationships(
                    database_core,
                    &drawer_name,
                    std::slice::from_mut(&mut record),
                    false,
                    context,
                    &mut hydration_cache,
                )?;
                }
                Self::remove_hidden_fields_from_value(database_core, &drawer_name, &mut record)?;
                if let Value::Object(ref mut map) = record {
                    map.remove("_id");
                }
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    pub(super) fn delete_by_id_in_database(
        database_core: &RwLock<Database>,
        locator: StorageLocator,
        context: ExecutionContext<'_>,
    ) -> Result<bool> {
        let pointer = pointer::locator_to_pointer(locator);
        wal::run_delete_transaction(database_core, &pointer, context, || {
            Self::delete_by_id_in_database_unlogged(database_core, &pointer, context)
        })
    }

    fn delete_by_id_in_database_unlogged(
        database_core: &RwLock<Database>,
        pointer: &str,
        context: ExecutionContext<'_>,
    ) -> Result<bool> {
        let mut active_delete_path = HashSet::new();
        let physical_pointer = routing::scoped_pointer(pointer, context.drawer_namespace);
        Self::delete_by_id_inner(
            database_core,
            &physical_pointer,
            &mut active_delete_path,
            context,
        )
    }

    pub(super) fn delete_by_filter_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        filter: Value,
        context: ExecutionContext<'_>,
    ) -> Result<usize> {
        let wal_filter = filter.clone();
        wal::run_delete_by_filter_transaction(
            database_core,
            drawer_name,
            &wal_filter,
            context,
            || {
                Self::delete_by_filter_in_database_unlogged(
                    database_core,
                    drawer_name,
                    filter,
                    context,
                )
            },
        )
    }

    fn delete_by_filter_in_database_unlogged(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        filter: Value,
        context: ExecutionContext<'_>,
    ) -> Result<usize> {
        let filter_map = query::filter_map(&filter)?;
        let physical_drawer_name =
            routing::scoped_drawer_name(drawer_name, context.drawer_namespace);
        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )?
        else {
            return Ok(0);
        };

        let (records, cascade_fields, inverse_delete_rules) = {
            let mut drawer = Self::write_lock(&drawer)?;
            (
                drawer.records_matching_filter_candidates(filter_map, context.drawer_namespace)?,
                drawer.cascade_delete_fields(),
                delete_rules::inverse_delete_rules(
                    drawer.delete_rules(),
                    drawer.relationship_constraints(),
                ),
            )
        };

        if records.is_empty() {
            return Ok(0);
        }

        let mut target_keys = Vec::with_capacity(records.len());
        let mut target_pointers = Vec::with_capacity(records.len());
        for record in &records {
            let record_key = Self::primary_key_from_record_for_delete(record)?;
            target_pointers.push(pointer::format_pointer(&physical_drawer_name, &record_key));
            target_keys.push(record_key);
        }
        let target_pointer_set = target_pointers.iter().cloned().collect::<HashSet<_>>();

        let mut active_delete_path = HashSet::new();
        for (record, pointer) in records.iter().zip(target_pointers.iter()) {
            Self::apply_delete_rule_side_effects(
                database_core,
                pointer,
                record,
                &cascade_fields,
                &inverse_delete_rules,
                &mut active_delete_path,
                &target_pointer_set,
                context,
            )?;
        }

        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )?
        else {
            return Ok(0);
        };

        let mut unique_target_keys = target_keys.clone();
        unique_target_keys.sort();
        unique_target_keys.dedup();

        let deleted_count =
            Self::write_lock(&drawer)?.delete_by_primary_keys_set_based(target_keys)?;
        if deleted_count > 0 {
            Self::write_lock(database_core)?.remove_reverse_relationships_for_record_keys(
                &physical_drawer_name,
                &unique_target_keys,
            )?;
            Self::read_lock(database_core)?.mark_drawer_mutated(&physical_drawer_name);
        }

        Ok(deleted_count)
    }

    fn primary_key_from_record_for_delete(record: &Value) -> Result<String> {
        let id = record.get("_id").and_then(Value::as_str).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                "record is missing a string _id for delete",
            )
        })?;

        if id.starts_with('@') {
            let (_, record_key) = pointer::parse_pointer(id)?;
            Ok(record_key)
        } else {
            Ok(id.to_string())
        }
    }

    fn delete_by_id_inner(
        database_core: &RwLock<Database>,
        pointer: &str,
        active_delete_path: &mut HashSet<String>,
        context: ExecutionContext<'_>,
    ) -> Result<bool> {
        if active_delete_path.contains(pointer) {
            return Ok(false);
        }

        let (drawer_name, record_key) = pointer::parse_pointer(pointer)?;
        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &drawer_name,
            "_id",
            Vec::new(),
        )?
        else {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Drawer '{}' could not be loaded for delete", drawer_name),
            ));
        };

        let (record, cascade_fields, inverse_delete_rules) = {
            let mut drawer = Self::write_lock(&drawer)?;
            (
                drawer.find_by_primary_key_with_migration(&record_key)?,
                drawer.cascade_delete_fields(),
                delete_rules::inverse_delete_rules(
                    drawer.delete_rules(),
                    drawer.relationship_constraints(),
                ),
            )
        };
        let Some(record) = record else {
            return Ok(false);
        };

        Self::apply_delete_rule_side_effects(
            database_core,
            pointer,
            &record,
            &cascade_fields,
            &inverse_delete_rules,
            active_delete_path,
            &HashSet::new(),
            context,
        )?;

        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &drawer_name,
            "_id",
            Vec::new(),
        )?
        else {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Drawer '{}' could not be loaded for delete", drawer_name),
            ));
        };

        let deleted_record = Self::write_lock(&drawer)?.delete_by_primary_key(&record_key)?;
        if deleted_record.is_some() {
            Self::write_lock(database_core)?
                .remove_reverse_relationships_for_record_keys(&drawer_name, &[record_key])?;
            Self::read_lock(database_core)?.mark_drawer_mutated(&drawer_name);
        }

        Ok(deleted_record.is_some())
    }

    fn apply_delete_rule_side_effects(
        database_core: &RwLock<Database>,
        pointer: &str,
        record: &Value,
        cascade_fields: &[String],
        inverse_delete_rules: &[delete_rules::InverseDeleteRule],
        active_delete_path: &mut HashSet<String>,
        skip_delete_pointers: &HashSet<String>,
        context: ExecutionContext<'_>,
    ) -> Result<()> {
        if active_delete_path.contains(pointer) {
            return Ok(());
        }

        delete_rules::evaluate_restrict_delete_rules(
            pointer,
            inverse_delete_rules,
            context.drawer_namespace,
            |target_drawer, mapped_by, parent_pointer| {
                Self::records_matching_parent_pointer(
                    database_core,
                    target_drawer,
                    mapped_by,
                    parent_pointer,
                    context,
                )
            },
        )?;

        active_delete_path.insert(pointer.to_string());
        let result = (|| {
            let cascade_child_pointers = delete_rules::collect_inverse_delete_rule_pointers(
                pointer,
                inverse_delete_rules,
                delete_rules::DeleteAction::Cascade,
                context.drawer_namespace,
                |target_drawer, mapped_by, parent_pointer| {
                    Self::records_matching_parent_pointer(
                        database_core,
                        target_drawer,
                        mapped_by,
                        parent_pointer,
                        context,
                    )
                },
            )?;
            for cascade_pointer in cascade_child_pointers {
                if !skip_delete_pointers.contains(&cascade_pointer) {
                    Self::delete_by_id_inner(
                        database_core,
                        &cascade_pointer,
                        active_delete_path,
                        context,
                    )?;
                }
            }

            let cascade_pointers = delete_rules::collect_cascade_pointers(record, cascade_fields);
            for cascade_pointer in cascade_pointers {
                if !skip_delete_pointers.contains(&cascade_pointer) {
                    Self::delete_by_id_inner(
                        database_core,
                        &cascade_pointer,
                        active_delete_path,
                        context,
                    )?;
                }
            }

            delete_rules::apply_set_null_delete_rules(
                pointer,
                inverse_delete_rules,
                context.drawer_namespace,
                |target_drawer, mapped_by, parent_pointer| {
                    Self::records_matching_parent_pointer(
                        database_core,
                        target_drawer,
                        mapped_by,
                        parent_pointer,
                        context,
                    )
                },
                |physical_target_drawer, field_name, child_record| {
                    let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
                        database_core,
                        physical_target_drawer,
                        "_id",
                        Vec::new(),
                    )?
                    else {
                        return Err(Error::new(
                            ErrorKind::NotFound,
                            format!(
                                "Drawer '{}' could not be loaded for SetNull delete rule '{}'",
                                physical_target_drawer, field_name
                            ),
                        ));
                    };

                    let child_key = Self::primary_key_from_record_for_delete(&child_record)?;
                    let child_record_value = child_record.clone();
                    let (relationship_constraints, delete_rules, upsert_result) = {
                        let mut drawer = Self::write_lock(&drawer)?;
                        let relationship_constraints = drawer.relationship_constraints();
                        let delete_rules = drawer.delete_rules();
                        let upsert_result = drawer.upsert_record(child_record)?;
                        (relationship_constraints, delete_rules, upsert_result)
                    };

                    match upsert_result {
                        Ok(_) => {
                            Self::write_lock(database_core)?
                                .replace_reverse_relationships_for_record(
                                    physical_target_drawer,
                                    &child_key,
                                    &child_record_value,
                                    &relationship_constraints,
                                    &delete_rules,
                                )?;
                            Ok(())
                        }
                        Err(validation_error) => {
                            Err(Error::new(ErrorKind::InvalidData, validation_error))
                        }
                    }
                },
            )
        })();
        active_delete_path.remove(pointer);
        result
    }

    pub(super) fn manage_schema_in_database(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        action: &str,
        kind: &str,
        field_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<Value> {
        let physical_drawer_name =
            routing::scoped_drawer_name(drawer_name, context.drawer_namespace);
        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_drawer_name,
            "_id",
            Vec::new(),
        )?
        else {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Drawer '{drawer_name}' could not be loaded for schema management"),
            ));
        };

        Self::write_lock(&drawer)?.manage_schema_rule(action, kind, field_name, payload)
    }

    fn records_matching_parent_pointer(
        database_core: &RwLock<Database>,
        target_drawer: &str,
        mapped_by: &str,
        parent_pointer: &str,
        context: ExecutionContext<'_>,
    ) -> Result<Vec<Value>> {
        if let Some(records) = Self::records_matching_parent_pointer_from_reverse_index(
            database_core,
            target_drawer,
            mapped_by,
            parent_pointer,
            context,
        )? {
            return Ok(records);
        }

        let physical_target_drawer =
            routing::scoped_drawer_name(target_drawer, context.drawer_namespace);
        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_target_drawer,
            "_id",
            Vec::new(),
        )?
        else {
            return Ok(Vec::new());
        };

        let records = Self::write_lock(&drawer)?
            .find_all_records_with_migration()?
            .into_iter()
            .filter(|record| {
                record.get(mapped_by).is_some_and(|value| {
                    delete_rules::value_contains_pointer(value, parent_pointer)
                })
            })
            .collect();

        Ok(records)
    }

    fn records_matching_parent_pointer_from_reverse_index(
        database_core: &RwLock<Database>,
        target_drawer: &str,
        mapped_by: &str,
        parent_pointer: &str,
        context: ExecutionContext<'_>,
    ) -> Result<Option<Vec<Value>>> {
        let physical_target_drawer =
            routing::scoped_drawer_name(target_drawer, context.drawer_namespace);
        let reverse_entries = {
            let database = Self::read_lock(database_core)?;
            database.reverse_relationships_for_parent(parent_pointer)
        };

        let mut child_ids = reverse_entries
            .into_iter()
            .filter(|entry| {
                entry.child_drawer == physical_target_drawer && entry.field_name == mapped_by
            })
            .map(|entry| entry.child_id)
            .collect::<Vec<_>>();
        child_ids.sort();
        child_ids.dedup();

        if child_ids.is_empty() {
            return Ok(Some(Vec::new()));
        }

        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_target_drawer,
            "_id",
            Vec::new(),
        )?
        else {
            return Ok(Some(Vec::new()));
        };

        let mut drawer = Self::write_lock(&drawer)?;
        let mut records = Vec::with_capacity(child_ids.len());
        for child_id in child_ids {
            let Some(record) = drawer.find_by_primary_key_with_migration(&child_id)? else {
                continue;
            };

            if record
                .get(mapped_by)
                .is_some_and(|value| delete_rules::value_contains_pointer(value, parent_pointer))
            {
                records.push(record);
            }
        }

        Ok(Some(records))
    }

    fn fetch_record_for_hydration(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        record_key: &str,
    ) -> Result<Option<Value>> {
        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            drawer_name,
            "_id",
            Vec::new(),
        )?
        else {
            return Ok(None);
        };

        let mut record =
            Self::write_lock(&drawer)?.find_by_primary_key_with_migration(record_key)?;
        if let Some(record) = record.as_mut() {
            Self::remove_hidden_fields_from_value(database_core, drawer_name, record)?;
        }
        Ok(record)
    }

    fn attach_virtual_relationships(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        records: &mut [Value],
        include_ids: bool,
        context: ExecutionContext<'_>,
        hydration_cache: &mut RequestHydrationCache,
    ) -> Result<()> {
        let virtual_relationships = {
            let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
                database_core,
                drawer_name,
                "_id",
                Vec::new(),
            )?
            else {
                return Ok(());
            };

            relationship::virtual_relationships(
                Self::read_lock(&drawer)?.relationship_constraints(),
            )
        };

        hydration::hydrate_virtual_relationships(
            drawer_name,
            records,
            &virtual_relationships,
            include_ids,
            |relationship, parent_pointer, include_ids| {
                Self::virtual_relationship_children(
                    database_core,
                    &relationship.target_drawer,
                    &relationship.mapped_by,
                    parent_pointer,
                    include_ids,
                    context,
                    hydration_cache,
                )
            },
        )
    }

    fn virtual_relationship_children(
        database_core: &RwLock<Database>,
        target_drawer: &str,
        mapped_by: &str,
        parent_pointer: &str,
        include_ids: bool,
        context: ExecutionContext<'_>,
        hydration_cache: &mut RequestHydrationCache,
    ) -> Result<Vec<Value>> {
        let physical_target_drawer =
            routing::scoped_drawer_name(target_drawer, context.drawer_namespace);
        let cache_key = VirtualRelationshipCacheKey {
            target_drawer: physical_target_drawer.clone(),
            mapped_by: mapped_by.to_string(),
            parent_pointer: parent_pointer.to_string(),
            include_ids,
        };

        if let Some(child_records) = hydration_cache.virtual_children.get(&cache_key) {
            return Ok(child_records.clone());
        }

        let mut child_records = if let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            &physical_target_drawer,
            "_id",
            Vec::new(),
        )? {
            let mut drawer = Self::write_lock(&drawer)?;
            let mut filter_map = serde_json::Map::new();
            filter_map.insert(
                mapped_by.to_string(),
                Value::String(parent_pointer.to_string()),
            );
            if let Some(offsets) = drawer.indexed_candidate_offsets(&filter_map)? {
                drawer.records_at_offsets_with_migration(offsets)?
            } else {
                drawer.find_all_records_with_migration()?
            }
        } else {
            Vec::new()
        };

        child_records.retain(|record| {
            record.get(mapped_by).and_then(|value| value.as_str()) == Some(parent_pointer)
        });
        hydration::hydrate_records_with_cache(
            &mut child_records,
            include_ids,
            &mut hydration_cache.records,
            |drawer_name, record_key| {
                Self::fetch_record_for_hydration(database_core, drawer_name, record_key)
            },
        )?;
        Self::remove_hidden_fields_from_records(
            database_core,
            &physical_target_drawer,
            &mut child_records,
        )?;

        hydration_cache
            .virtual_children
            .insert(cache_key, child_records.clone());

        Ok(child_records)
    }

    fn remove_hidden_fields_from_records(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        records: &mut [Value],
    ) -> Result<()> {
        let hidden_fields = Self::hidden_output_fields_for_drawer(database_core, drawer_name)?;
        if hidden_fields.is_empty() {
            return Ok(());
        }

        for record in records {
            Drawer::remove_hidden_fields_from_value(record, &hidden_fields);
        }

        Ok(())
    }

    fn remove_hidden_fields_from_value(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        record: &mut Value,
    ) -> Result<()> {
        let hidden_fields = Self::hidden_output_fields_for_drawer(database_core, drawer_name)?;
        Drawer::remove_hidden_fields_from_value(record, &hidden_fields);
        Ok(())
    }

    fn hidden_output_fields_for_drawer(
        database_core: &RwLock<Database>,
        drawer_name: &str,
    ) -> Result<Vec<String>> {
        let Some(drawer) = Self::active_drawer_handle_or_load_from_disk(
            database_core,
            drawer_name,
            "_id",
            Vec::new(),
        )?
        else {
            return Ok(Vec::new());
        };

        Ok(Self::read_lock(&drawer)?.hidden_output_fields())
    }
}

impl command_dispatch::DatabaseCommandExecutor for WardrobeEngine {
    fn upsert_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<String> {
        WardrobeEngine::upsert_in_database(database, drawer_name, payload, context)
    }

    fn bulk_upsert_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        records: Vec<Value>,
        atomic: bool,
        context: ExecutionContext<'_>,
    ) -> Result<Vec<String>> {
        WardrobeEngine::bulk_upsert_in_database(database, drawer_name, records, atomic, context)
    }

    fn find_all_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        context: ExecutionContext<'_>,
        hydrate: bool,
    ) -> Result<Vec<Value>> {
        WardrobeEngine::find_all_in_database(database, drawer_name, context, hydrate)
    }

    fn find_by_id_in_database(
        database: &RwLock<Database>,
        pointer: &str,
        context: ExecutionContext<'_>,
        hydrate: bool,
    ) -> Result<Option<Value>> {
        WardrobeEngine::find_by_id_in_database(database, pointer, context, hydrate)
    }

    fn find_by_filter_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        filter: Value,
        modifiers: Option<QueryModifiers>,
        context: ExecutionContext<'_>,
        hydrate: bool,
    ) -> Result<Vec<Value>> {
        WardrobeEngine::find_by_filter_in_database(
            database,
            drawer_name,
            filter,
            modifiers,
            context,
            hydrate,
        )
    }

    fn count_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        filter: Option<Value>,
        modifiers: Option<QueryModifiers>,
        context: ExecutionContext<'_>,
    ) -> Result<usize> {
        WardrobeEngine::count_in_database(database, drawer_name, filter, modifiers, context)
    }

    fn delete_by_id_in_database(
        database: &RwLock<Database>,
        locator: StorageLocator,
        context: ExecutionContext<'_>,
    ) -> Result<bool> {
        WardrobeEngine::delete_by_id_in_database(database, locator, context)
    }

    fn delete_by_filter_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        filter: Value,
        context: ExecutionContext<'_>,
    ) -> Result<usize> {
        WardrobeEngine::delete_by_filter_in_database(database, drawer_name, filter, context)
    }

    fn manage_schema_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        action: &str,
        kind: &str,
        field_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<Value> {
        WardrobeEngine::manage_schema_in_database(
            database,
            drawer_name,
            action,
            kind,
            field_name,
            payload,
            context,
        )
    }

    fn vacuum_drawer_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        context: ExecutionContext<'_>,
    ) -> Result<VacuumReport> {
        WardrobeEngine::vacuum_drawer_in_database(database, drawer_name, context)
    }

    fn migrate_drawer_in_database(
        database: &RwLock<Database>,
        drawer_name: &str,
        context: ExecutionContext<'_>,
    ) -> Result<VacuumReport> {
        WardrobeEngine::migrate_drawer_in_database(database, drawer_name, context)
    }
}

impl wal::WalReplayExecutor for WardrobeEngine {
    fn replay_upsert(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        payload: Value,
        context: ExecutionContext<'_>,
    ) -> Result<()> {
        WardrobeEngine::upsert_in_database_unlogged(database_core, drawer_name, payload, context)
            .map(|_| ())
    }

    fn replay_bulk_upsert(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        records: Vec<Value>,
        context: ExecutionContext<'_>,
    ) -> Result<()> {
        WardrobeEngine::bulk_upsert_in_database_unlogged(
            database_core,
            drawer_name,
            records,
            context,
        )
        .map(|_| ())
    }

    fn replay_delete(
        database_core: &RwLock<Database>,
        pointer: &str,
        context: ExecutionContext<'_>,
    ) -> Result<()> {
        WardrobeEngine::delete_by_id_in_database_unlogged(database_core, pointer, context)
            .map(|_| ())
    }

    fn replay_delete_by_filter(
        database_core: &RwLock<Database>,
        drawer_name: &str,
        filter: Value,
        context: ExecutionContext<'_>,
    ) -> Result<()> {
        WardrobeEngine::delete_by_filter_in_database_unlogged(
            database_core,
            drawer_name,
            filter,
            context,
        )
        .map(|_| ())
    }
}
