//! Multi-database slot manager (`db->aDb` in `sqlite/src/sqliteInt.h`, `attach.c`, `build.c`).
//!
//! Slot layout matches SQLite 3.54.1:
//! - Slot `0` ([`DatabaseId::MAIN`]): `"main"` — always present and open.
//! - Slot `1` ([`DatabaseId::TEMP`]): `"temp"` — name `"temp"` is always reserved in slot `1`, while
//!   its [`Btree`] is lazily opened on first access (`sqlite3OpenTempDatabase`).
//! - Slots `2..nDb`: attached databases (up to `max_attached`, default [`SQLITE_MAX_ATTACHED`] = `10`).
//!   Detaching slot `k >= 2` shifts slots `(k + 1)..nDb` left by 1 (`sqlite3CollapseDatabaseArray`).
#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sqliters_btree::Btree;
use sqliters_pager::{AutoVacuumMode, LockingMode, PagerOpenOptions, TransactionState};
use sqliters_syntax::QualifiedName;
use sqliters_types::{
    DatabaseId, DatabaseKind, ErrorCode, LockGeometry, Result, SQLITE_MAX_ATTACHED, SqliteError,
    TextEncoding,
};
use sqliters_vdbe::BtreeAccess;
use sqliters_vfs::Vfs;

use crate::schema::{IndexSchema, Schema, TableSchema, ViewSchema};

/// Resolved relation (either a table or a view) located in [`DatabaseSet`].
#[derive(Debug, Clone)]
pub enum ResolvedRelation {
    Table(Box<TableSchema>),
    View(Box<ViewSchema>),
}

/// Connection-unique monotonic identity for a database schema instance.
///
/// Unlike numeric [`DatabaseId`] slot indices — which shift left when an earlier attached
/// database is detached (`sqlite3CollapseDatabaseArray`) — a [`SchemaInstanceId`] is never reused
/// on the same connection. Lifecycle hooks such as [`DatabaseSet::detach_temp_triggers`] match
/// against [`SchemaInstanceId`] (`Trigger::pTabSchema` in `attach.c:63-74`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SchemaInstanceId(pub u64);

/// Connection `PRAGMA temp_store` backing policy (`0 = DEFAULT`, `1 = FILE`, `2 = MEMORY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TempStoreMode {
    /// Default compile-time temporary storage (`SQLITE_TEMP_STORE = 1` -> file-backed).
    #[default]
    Default = 0,
    /// Explicit file-backed temporary storage (`PRAGMA temp_store = FILE` / `1`).
    File = 1,
    /// Explicit heap-memory temporary storage (`PRAGMA temp_store = MEMORY` / `2`).
    Memory = 2,
}

impl TempStoreMode {
    /// Returns `true` when temporary databases should use in-memory backing (`MEMORY`).
    #[inline]
    pub const fn is_memory(self) -> bool {
        matches!(self, Self::Memory)
    }

    /// Numeric value reported by `PRAGMA temp_store`.
    #[inline]
    pub const fn as_int(self) -> i64 {
        self as i64
    }

    /// Parses a `PRAGMA temp_store = ...` value matching `getTempStore` (`pragma.c:141-151`).
    pub fn from_pragma_str(s: &str) -> Self {
        let trimmed = s.trim();
        match trimmed.as_bytes().first().copied() {
            Some(b'0') if trimmed.len() == 1 => Self::Default,
            Some(b'1') if trimmed.len() == 1 => Self::File,
            Some(b'2') if trimmed.len() == 1 => Self::Memory,
            _ if trimmed.eq_ignore_ascii_case("file") => Self::File,
            _ if trimmed.eq_ignore_ascii_case("memory") => Self::Memory,
            _ => Self::Default,
        }
    }
}

/// Metadata binding a `TEMP` trigger to the [`SchemaInstanceId`] of its target table (`pTabSchema`).
///
/// When the referenced attached database is detached, [`DatabaseSet::detach_temp_triggers`]
/// redirects `target_schema_instance_id` to `TEMP`'s [`SchemaInstanceId`] (`attach.c:63-74`) so
/// a subsequently attached database reusing the same numeric [`DatabaseId`] slot never inherits
/// stale trigger bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempTriggerBinding {
    pub trigger_name: String,
    pub target_schema_instance_id: SchemaInstanceId,
    pub target_table_name: String,
}

/// Default maximum page count per database (`SQLITE_MAX_PAGE_COUNT` in `sqliteLimit.h`).
pub const SQLITE_MAX_PAGE_COUNT: u32 = 0xffff_fffe;

/// Single entry in [`DatabaseSet`] (`struct Db` in `sqlite/src/sqliteInt.h`).
pub struct DbSlot {
    /// Schema name (`"main"`, `"temp"`, or attached alias).
    pub name: String,
    /// Underlying [`Btree`] (`None` only for unopened `"temp"` in slot `1`).
    pub btree: Option<Btree>,
    /// Parsed in-memory catalog for this database slot.
    pub schema: Schema,
    /// Monotonic unique identity for this slot's schema lifetime.
    pub schema_instance_id: SchemaInstanceId,
    /// Deferred page size requested via `PRAGMA [schema.]page_size = N` for next `VACUUM`.
    pub next_page_size: Option<u32>,
    /// Deferred auto-vacuum mode requested via `PRAGMA [schema.]auto_vacuum = M` for next `VACUUM`.
    pub next_auto_vacuum: Option<AutoVacuumMode>,
    /// Per-database `PRAGMA [schema.]max_page_count` limit.
    pub max_page_count: u32,
    /// Whether URI `8_3_names=1` short super-journal/sidecar naming is enabled on this slot.
    pub enable_8_3_names: bool,
}

impl DbSlot {
    /// Reloads `self.schema` from `self.btree` using `self.name` for `SchemaFixer` permanent-view
    /// schema validation.
    pub fn reload_schema(&mut self) -> Result<()> {
        let bt = self
            .btree
            .as_mut()
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        self.schema = Schema::load_for_schema(bt, Some(&self.name))?;
        Ok(())
    }
}

impl std::fmt::Debug for DbSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbSlot")
            .field("name", &self.name)
            .field("is_open", &self.btree.is_some())
            .field("schema_instance_id", &self.schema_instance_id)
            .field("enable_8_3_names", &self.enable_8_3_names)
            .finish()
    }
}

/// Connection database array (`db->aDb[0..db->nDb]`) managing `"main"`, lazy `"temp"`, and
/// attached databases (`2..nDb`).
pub struct DatabaseSet {
    slots: Vec<DbSlot>,
    max_attached: usize,
    next_schema_instance_id: u64,
    temp_store: TempStoreMode,
    disk_vfs: Arc<dyn Vfs>,
    default_page_size: u32,
    lock_geometry: LockGeometry,
    interrupt_flag: Arc<AtomicBool>,
    temp_trigger_bindings: Vec<TempTriggerBinding>,
}

impl std::fmt::Debug for DatabaseSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseSet")
            .field("slots", &self.slots)
            .field("max_attached", &self.max_attached)
            .field("temp_store", &self.temp_store)
            .finish()
    }
}

impl DatabaseSet {
    /// Creates a new [`DatabaseSet`] with open `"main"` in slot `0` and lazy `"temp"` reserved in slot `1`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        main_btree: Btree,
        main_schema: Schema,
        disk_vfs: Arc<dyn Vfs>,
        default_page_size: u32,
        lock_geometry: LockGeometry,
        temp_store: TempStoreMode,
        main_enable_8_3_names: bool,
        interrupt_flag: Arc<AtomicBool>,
    ) -> Self {
        let slots = vec![
            DbSlot {
                name: "main".to_owned(),
                btree: Some(main_btree),
                schema: main_schema,
                schema_instance_id: SchemaInstanceId(1),
                next_page_size: None,
                next_auto_vacuum: None,
                max_page_count: SQLITE_MAX_PAGE_COUNT,
                enable_8_3_names: main_enable_8_3_names,
            },
            DbSlot {
                name: "temp".to_owned(),
                btree: None,
                schema: Schema::default(),
                schema_instance_id: SchemaInstanceId(2),
                next_page_size: None,
                next_auto_vacuum: None,
                max_page_count: SQLITE_MAX_PAGE_COUNT,
                enable_8_3_names: false,
            },
        ];
        Self {
            slots,
            max_attached: SQLITE_MAX_ATTACHED,
            next_schema_instance_id: 3,
            temp_store,
            disk_vfs,
            default_page_size,
            lock_geometry,
            interrupt_flag,
            temp_trigger_bindings: Vec::new(),
        }
    }

    /// Total number of slots (`db->nDb`, always `>= 2`).
    #[inline]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Number of currently attached databases (`slots[2..]`).
    #[inline]
    pub fn attached_count(&self) -> usize {
        self.slots.len().saturating_sub(2)
    }

    /// Maximum allowed attached databases on this connection (`SQLITE_LIMIT_ATTACHED`).
    #[inline]
    pub const fn max_attached(&self) -> usize {
        self.max_attached
    }

    /// Sets `SQLITE_LIMIT_ATTACHED` (clamped to `0..=SQLITE_MAX_ATTACHED`), returning the new limit.
    #[inline]
    pub fn set_max_attached(&mut self, limit: usize) -> usize {
        self.max_attached = limit.min(SQLITE_MAX_ATTACHED);
        self.max_attached
    }

    /// Current `PRAGMA temp_store` mode.
    #[inline]
    pub const fn temp_store(&self) -> TempStoreMode {
        self.temp_store
    }

    /// Disk [`Vfs`] used for temporary and attached disk databases on this connection.
    #[inline]
    pub fn disk_vfs(&self) -> &Arc<dyn Vfs> {
        &self.disk_vfs
    }

    /// Connection default page size candidate for newly opened temporary/attached databases.
    #[inline]
    pub const fn default_page_size(&self) -> u32 {
        self.default_page_size
    }

    /// Updates the connection default page size candidate.
    #[inline]
    pub fn set_default_page_size(&mut self, page_size: u32) {
        self.default_page_size = page_size;
    }

    /// Returns `true` if any open database slot currently holds a `Read` or `Write` B-tree transaction.
    pub fn any_in_transaction(&self) -> bool {
        self.slots.iter().any(|slot| {
            slot.btree
                .as_ref()
                .is_some_and(|bt| bt.transaction_state() != TransactionState::Open)
        })
    }

    /// Returns `true` if any open database slot currently holds a `Write` B-tree transaction.
    pub fn any_in_write_transaction(&self) -> bool {
        self.slots.iter().any(|slot| {
            slot.btree
                .as_ref()
                .is_some_and(|bt| bt.transaction_state() == TransactionState::Write)
        })
    }

    /// Connection lock-byte geometry.
    #[inline]
    pub const fn lock_geometry(&self) -> LockGeometry {
        self.lock_geometry
    }

    /// Shared connection interrupt flag.
    #[inline]
    pub fn interrupt_flag(&self) -> &Arc<AtomicBool> {
        &self.interrupt_flag
    }

    /// Updates the shared interrupt flag across all open [`Btree`] slots and future temporary/attached opens.
    pub fn set_interrupt_flag(&mut self, flag: Arc<AtomicBool>) {
        for slot in &mut self.slots {
            if let Some(ref mut bt) = slot.btree {
                bt.set_interrupt_flag(Some(Arc::clone(&flag)));
            }
        }
        self.interrupt_flag = flag;
    }

    /// Immutable slice of all slots (`0..nDb`).
    #[inline]
    pub fn slots(&self) -> &[DbSlot] {
        &self.slots
    }

    /// Mutable slice of all slots (`0..nDb`).
    #[inline]
    pub fn slots_mut(&mut self) -> &mut [DbSlot] {
        &mut self.slots
    }

    /// Returns immutable reference to slot `id`, if within `0..nDb`.
    #[inline]
    pub fn slot(&self, id: DatabaseId) -> Option<&DbSlot> {
        self.slots.get(id.index())
    }

    /// Returns mutable reference to slot `id`, if within `0..nDb`.
    #[inline]
    pub fn slot_mut(&mut self, id: DatabaseId) -> Option<&mut DbSlot> {
        self.slots.get_mut(id.index())
    }

    /// Returns `"main"` slot (`0`).
    #[inline]
    pub fn main_slot(&self) -> &DbSlot {
        &self.slots[0]
    }

    /// Returns mutable `"main"` slot (`0`).
    #[inline]
    pub fn main_slot_mut(&mut self) -> &mut DbSlot {
        &mut self.slots[0]
    }

    /// Returns `"main"` [`Btree`].
    #[inline]
    pub fn main_btree(&self) -> &Btree {
        self.slots[0]
            .btree
            .as_ref()
            .expect("main slot Btree is always open")
    }

    /// Returns mutable `"main"` [`Btree`].
    #[inline]
    pub fn main_btree_mut(&mut self) -> &mut Btree {
        self.slots[0]
            .btree
            .as_mut()
            .expect("main slot Btree is always open")
    }

    /// Returns `"main"` [`Schema`].
    #[inline]
    pub fn main_schema(&self) -> &Schema {
        &self.slots[0].schema
    }

    /// Returns mutable `"main"` [`Schema`].
    #[inline]
    pub fn main_schema_mut(&mut self) -> &mut Schema {
        &mut self.slots[0].schema
    }

    /// Returns `"temp"` [`SchemaInstanceId`] (slot `1`).
    #[inline]
    pub fn temp_schema_instance_id(&self) -> SchemaInstanceId {
        self.slots[1].schema_instance_id
    }

    /// Returns `true` if slot `1` (`"temp"`) has already opened its [`Btree`].
    #[inline]
    pub fn is_temp_open(&self) -> bool {
        self.slots[1].btree.is_some()
    }

    /// Lazily opens the `"temp"` [`Btree`] in slot `1` if not already open (`sqlite3OpenTempDatabase`
    /// in `build.c`), respecting `self.temp_store`, `encoding`, and `writable_schema`.
    pub fn ensure_temp_open(
        &mut self,
        encoding: TextEncoding,
        writable_schema: bool,
    ) -> Result<&mut DbSlot> {
        if self.slots[1].btree.is_none() {
            let opts = PagerOpenOptions {
                default_page_size: self.default_page_size,
                lock_geometry: Some(self.lock_geometry),
                db_kind: Some(DatabaseKind::PrivateTemporary),
                temp_in_memory: self.temp_store.is_memory(),
                ..PagerOpenOptions::default()
            };
            let mut bt = Btree::open_private_temporary(Arc::clone(&self.disk_vfs), opts)?;
            bt.set_interrupt_flag(Some(Arc::clone(&self.interrupt_flag)));
            bt.set_writable_schema(writable_schema);
            let _ = bt.pager_mut().set_locking_mode(LockingMode::Exclusive);
            bt.pager_mut().set_encoding(encoding)?;
            self.slots[1].btree = Some(bt);
        }
        Ok(&mut self.slots[1])
    }

    /// Updates `PRAGMA temp_store` (`changeTempStorage` / `invalidateTempStorage` in `pragma.c:159-192`).
    /// Returns `Ok(true)` if an open `"temp"` database was closed and invalidated.
    pub fn set_temp_store(&mut self, mode: TempStoreMode, in_autocommit: bool) -> Result<bool> {
        if self.temp_store == mode {
            return Ok(false);
        }
        if let Some(ref mut bt) = self.slots[1].btree {
            if !in_autocommit || bt.transaction_state() != TransactionState::Open {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "temporary storage cannot be changed from within a transaction",
                ));
            }
            bt.close()?;
            self.slots[1].btree = None;
            self.slots[1].schema = Schema::default();
            self.temp_store = mode;
            return Ok(true);
        }
        self.temp_store = mode;
        Ok(false)
    }

    /// Finds a database slot by case-insensitive schema name across `0..nDb`, **including** slot `1`
    /// (`"temp"`) even when `"temp"`'s [`Btree`] has not been opened yet (`sqlite3FindDbName`).
    pub fn find_db_id(&self, schema_name: &str) -> Option<DatabaseId> {
        self.slots
            .iter()
            .position(|slot| slot.name.eq_ignore_ascii_case(schema_name))
            .map(DatabaseId)
    }

    /// Finds an **open** database slot for `DETACH`, skipping any slot whose `btree` is `None`
    /// (`if (pDb->pBt == 0) continue;` in `attach.c:333`).
    pub fn find_open_db_id_for_detach(&self, schema_name: &str) -> Option<DatabaseId> {
        self.slots
            .iter()
            .position(|slot| slot.btree.is_some() && slot.name.eq_ignore_ascii_case(schema_name))
            .map(DatabaseId)
    }

    /// Resolves an optional schema qualifier for DDL / `PRAGMA` / `VACUUM` (`sqlite3TwoPartName`),
    /// returning `"unknown database <schema>"` if not found.
    pub fn resolve_schema_or_err(&self, schema_name: Option<&str>) -> Result<DatabaseId> {
        match schema_name {
            None => Ok(DatabaseId::MAIN),
            Some(s) => self
                .find_db_id(s)
                .ok_or_else(|| SqliteError::new(ErrorCode::Error, format!("unknown database {s}"))),
        }
    }

    /// Unqualified object search order: `1: temp` (when open) -> `0: main` -> `2..nDb: attached`.
    pub fn unqualified_search_order(&self) -> Vec<DatabaseId> {
        let mut order = Vec::with_capacity(self.slots.len());
        if self.slots[1].btree.is_some() {
            order.push(DatabaseId::TEMP);
        }
        order.push(DatabaseId::MAIN);
        for idx in 2..self.slots.len() {
            order.push(DatabaseId(idx));
        }
        order
    }

    /// Returns `true` if `table_name` is a built-in catalog table name in `db_id`.
    #[inline]
    pub fn is_catalog_table_in_db(db_id: DatabaseId, table_name: &str) -> bool {
        if table_name.eq_ignore_ascii_case("sqlite_master")
            || table_name.eq_ignore_ascii_case("sqlite_schema")
        {
            true
        } else {
            db_id == DatabaseId::TEMP
                && (table_name.eq_ignore_ascii_case("sqlite_temp_master")
                    || table_name.eq_ignore_ascii_case("sqlite_temp_schema"))
        }
    }

    /// Resolves a catalog table reference (`sqlite_master`, `sqlite_schema`, `sqlite_temp_master`,
    /// `sqlite_temp_schema`), returning `Ok(Some(db_id))` when `(schema, name)` names a valid
    /// catalog table, `Ok(None)` when `name` is not a catalog table, or `Err("no such table: ...")`
    /// when `schema` is unknown or `sqlite_temp_master` is qualified with a non-`temp` schema.
    pub fn resolve_catalog_table(&self, qname: &QualifiedName) -> Result<Option<DatabaseId>> {
        let name = qname.name.as_str();
        let is_std_cat = name.eq_ignore_ascii_case("sqlite_master")
            || name.eq_ignore_ascii_case("sqlite_schema");
        let is_temp_cat = name.eq_ignore_ascii_case("sqlite_temp_master")
            || name.eq_ignore_ascii_case("sqlite_temp_schema");
        if !is_std_cat && !is_temp_cat {
            return Ok(None);
        }
        match qname.schema.as_deref() {
            None => {
                if is_temp_cat {
                    Ok(Some(DatabaseId::TEMP))
                } else {
                    Ok(Some(DatabaseId::MAIN))
                }
            }
            Some(s) => {
                let Some(db_id) = self.find_db_id(s) else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such table: {s}.{name}"),
                    ));
                };
                if is_temp_cat && db_id != DatabaseId::TEMP {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such table: {s}.{name}"),
                    ));
                }
                Ok(Some(db_id))
            }
        }
    }

    /// Locates a table (catalog table, user table, or `sqlite_sequence`) across [`DatabaseSet`]
    /// following `sqlite3LocateTable` (`temp -> main -> attached` when unqualified; exact slot when qualified).
    pub fn find_table_in_set(
        &self,
        qname: &QualifiedName,
    ) -> Result<Option<(DatabaseId, TableSchema)>> {
        match self.find_relation_in_set(qname)? {
            Some((db_id, ResolvedRelation::Table(tbl))) => Ok(Some((db_id, *tbl))),
            Some((_, ResolvedRelation::View(_))) | None => Ok(None),
        }
    }

    /// Locates a relation (catalog table, user table, or view) across [`DatabaseSet`] following
    /// `sqlite3LocateTable` (`temp -> main -> attached` when unqualified; exact slot when qualified),
    /// treating tables and views as a single per-database namespace so `temp` views shadow `main`
    /// tables and `temp` tables shadow `main` views.
    pub fn find_relation_in_set(
        &self,
        qname: &QualifiedName,
    ) -> Result<Option<(DatabaseId, ResolvedRelation)>> {
        if let Some(cat_db_id) = self.resolve_catalog_table(qname)? {
            let mut tbl = TableSchema::sqlite_schema_table();
            tbl.name = qname.name.clone();
            if cat_db_id == DatabaseId::TEMP && !self.is_temp_open() {
                tbl.root_pgno = 0;
            }
            return Ok(Some((cat_db_id, ResolvedRelation::Table(Box::new(tbl)))));
        }
        if let Some(ref s) = qname.schema {
            let Some(db_id) = self.find_db_id(s) else {
                return Ok(None);
            };
            let Some(slot) = self.slot(db_id) else {
                return Ok(None);
            };
            if slot.btree.is_none() {
                return Ok(None);
            }
            if let Some(tbl) = slot.schema.find_user_table(&qname.name) {
                return Ok(Some((
                    db_id,
                    ResolvedRelation::Table(Box::new(tbl.clone())),
                )));
            }
            if let Some(view) = slot.schema.find_view(&qname.name) {
                return Ok(Some((
                    db_id,
                    ResolvedRelation::View(Box::new(view.clone())),
                )));
            }
            Ok(None)
        } else {
            for db_id in self.unqualified_search_order() {
                if let Some(slot) = self.slot(db_id) {
                    if let Some(tbl) = slot.schema.find_user_table(&qname.name) {
                        return Ok(Some((
                            db_id,
                            ResolvedRelation::Table(Box::new(tbl.clone())),
                        )));
                    }
                    if let Some(view) = slot.schema.find_view(&qname.name) {
                        return Ok(Some((
                            db_id,
                            ResolvedRelation::View(Box::new(view.clone())),
                        )));
                    }
                }
            }
            Ok(None)
        }
    }

    /// Locates a view across [`DatabaseSet`] (`temp -> main -> attached` when unqualified).
    pub fn find_view_in_set(&self, qname: &QualifiedName) -> Option<(DatabaseId, &ViewSchema)> {
        if let Some(ref s) = qname.schema {
            let db_id = self.find_db_id(s)?;
            let slot = self.slot(db_id)?;
            slot.btree.as_ref()?;
            slot.schema.find_view(&qname.name).map(|v| (db_id, v))
        } else {
            for db_id in self.unqualified_search_order() {
                if let Some(slot) = self.slot(db_id) {
                    if slot.schema.find_user_table(&qname.name).is_some() {
                        return None;
                    }
                    if let Some(view) = slot.schema.find_view(&qname.name) {
                        return Some((db_id, view));
                    }
                }
            }
            None
        }
    }

    /// Locates a user table (excluding catalog tables) across [`DatabaseSet`].
    pub fn find_user_table_in_set(
        &self,
        qname: &QualifiedName,
    ) -> Option<(DatabaseId, &TableSchema)> {
        if let Some(ref s) = qname.schema {
            let db_id = self.find_db_id(s)?;
            let slot = self.slot(db_id)?;
            slot.btree.as_ref()?;
            slot.schema.find_user_table(&qname.name).map(|t| (db_id, t))
        } else {
            for db_id in self.unqualified_search_order() {
                if let Some(slot) = self.slot(db_id) {
                    if let Some(tbl) = slot.schema.find_user_table(&qname.name) {
                        return Some((db_id, tbl));
                    }
                    if slot.schema.find_view(&qname.name).is_some() {
                        return None;
                    }
                }
            }
            None
        }
    }

    /// Finds a database slot by its stable [`SchemaInstanceId`].
    pub fn find_db_id_by_instance(&self, instance_id: SchemaInstanceId) -> Option<DatabaseId> {
        self.slots
            .iter()
            .position(|slot| slot.schema_instance_id == instance_id)
            .map(DatabaseId)
    }

    /// Returns immutable reference to the slot with `instance_id`.
    pub fn slot_by_instance(&self, instance_id: SchemaInstanceId) -> Option<&DbSlot> {
        self.slots
            .iter()
            .find(|slot| slot.schema_instance_id == instance_id)
    }

    /// Returns mutable reference to the slot with `instance_id`.
    pub fn slot_by_instance_mut(&mut self, instance_id: SchemaInstanceId) -> Option<&mut DbSlot> {
        self.slots
            .iter_mut()
            .find(|slot| slot.schema_instance_id == instance_id)
    }

    /// Locates a secondary index across [`DatabaseSet`] (`temp -> main -> attached` when unqualified).
    pub fn find_index_in_set(&self, qname: &QualifiedName) -> Option<(DatabaseId, &IndexSchema)> {
        if let Some(ref s) = qname.schema {
            let db_id = self.find_db_id(s)?;
            let slot = self.slot(db_id)?;
            slot.btree.as_ref()?;
            slot.schema.find_index(&qname.name).map(|idx| (db_id, idx))
        } else {
            for db_id in self.unqualified_search_order() {
                if let Some(slot) = self.slot(db_id)
                    && let Some(idx) = slot.schema.find_index(&qname.name)
                {
                    return Some((db_id, idx));
                }
            }
            None
        }
    }

    /// Locates a secondary index or synthesized `WITHOUT ROWID` PK autoindex across [`DatabaseSet`].
    pub fn find_index_including_without_rowid_pk_in_set(
        &self,
        qname: &QualifiedName,
    ) -> Option<(DatabaseId, IndexSchema)> {
        if let Some(ref s) = qname.schema {
            let db_id = self.find_db_id(s)?;
            let slot = self.slot(db_id)?;
            slot.btree.as_ref()?;
            slot.schema
                .find_index_including_without_rowid_pk(&qname.name)
                .map(|idx| (db_id, idx))
        } else {
            for db_id in self.unqualified_search_order() {
                if let Some(slot) = self.slot(db_id)
                    && let Some(idx) = slot
                        .schema
                        .find_index_including_without_rowid_pk(&qname.name)
                {
                    return Some((db_id, idx));
                }
            }
            None
        }
    }

    /// Appends a newly attached database slot (`slots[2..]`), assigning a fresh monotonic
    /// [`SchemaInstanceId`].
    pub fn attach_slot(
        &mut self,
        name: String,
        mut btree: Btree,
        schema: Schema,
        enable_8_3_names: bool,
    ) -> Result<DatabaseId> {
        if self.attached_count() >= self.max_attached {
            let _ = btree.close();
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("too many attached databases - max {}", self.max_attached),
            ));
        }
        if self.find_db_id(&name).is_some() {
            let _ = btree.close();
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("database {name} is already in use"),
            ));
        }
        btree.set_interrupt_flag(Some(Arc::clone(&self.interrupt_flag)));
        let instance_id = SchemaInstanceId(self.next_schema_instance_id);
        self.next_schema_instance_id += 1;
        let db_id = DatabaseId(self.slots.len());
        self.slots.push(DbSlot {
            name,
            btree: Some(btree),
            schema,
            schema_instance_id: instance_id,
            next_page_size: None,
            next_auto_vacuum: None,
            max_page_count: SQLITE_MAX_PAGE_COUNT,
            enable_8_3_names,
        });
        Ok(db_id)
    }

    /// Detaches attached slot `id` (`id.index() >= 2`), invoking [`Self::detach_temp_triggers`],
    /// closing its [`Btree`], and collapsing subsequent slots left by 1 (`sqlite3CollapseDatabaseArray`).
    ///
    /// Returns `(detached_schema_instance_id, redirected_temp_trigger_count)`.
    pub fn detach_slot(&mut self, id: DatabaseId) -> Result<(SchemaInstanceId, usize)> {
        let idx = id.index();
        if idx < 2 || idx >= self.slots.len() {
            return Err(SqliteError::misuse(format!(
                "detach_slot called on non-attached slot {idx}"
            )));
        }
        let mut removed = self.slots.remove(idx);
        let redirected = self.detach_temp_triggers(removed.schema_instance_id);
        if let Some(ref mut bt) = removed.btree {
            bt.close()?;
        }
        Ok((removed.schema_instance_id, redirected))
    }

    /// Registers a `TEMP` trigger binding referencing `target_schema_instance_id` (`Trigger::pTabSchema`).
    pub fn register_temp_trigger_binding(&mut self, binding: TempTriggerBinding) {
        self.temp_trigger_bindings.push(binding);
    }

    /// Returns active `TEMP` trigger bindings registered on this connection.
    #[inline]
    pub fn temp_trigger_bindings(&self) -> &[TempTriggerBinding] {
        &self.temp_trigger_bindings
    }

    /// Redirects any `TEMP` trigger bindings referencing `detached_schema_instance_id` to `"temp"`'s
    /// [`SchemaInstanceId`] (`attach.c:63-74`), returning the number of redirected bindings.
    pub fn detach_temp_triggers(&mut self, detached_schema_instance_id: SchemaInstanceId) -> usize {
        let temp_id = self.slots[1].schema_instance_id;
        let mut redirected = 0;
        for binding in &mut self.temp_trigger_bindings {
            if binding.target_schema_instance_id == detached_schema_instance_id {
                binding.target_schema_instance_id = temp_id;
                redirected += 1;
            }
        }
        redirected
    }

    /// Implements `sqlite3_db_filename(db, zDbName)`: returns the canonical OS path for a named disk
    /// database, or `None` for `"temp"`, `:memory:`, `PrivateTemporary` (`""`), or unknown schemas.
    pub fn db_filename(&self, schema_name: &str) -> Option<&Path> {
        let id = self.find_db_id(schema_name)?;
        if id == DatabaseId::TEMP {
            return None;
        }
        self.slots[id.index()].btree.as_ref()?.db_path()
    }

    /// Closes all open [`Btree`] handles in reverse order (`attached`, `temp`, `main`).
    pub fn close_all(&mut self) -> Result<()> {
        let mut first_err: Option<SqliteError> = None;
        for slot in self.slots.iter_mut().rev() {
            if let Some(ref mut bt) = slot.btree
                && let Err(e) = bt.close()
                && first_err.is_none()
            {
                first_err = Some(e);
            }
        }
        if let Some(e) = first_err {
            Err(e)
        } else {
            Ok(())
        }
    }
}

impl BtreeAccess for DatabaseSet {
    fn btree_ref(&self, db: DatabaseId) -> Result<&Btree> {
        let slot = self.slots.get(db.index()).ok_or_else(|| {
            SqliteError::misuse(format!("DatabaseId({}) out of range", db.index()))
        })?;
        slot.btree.as_ref().ok_or_else(|| {
            SqliteError::misuse(format!(
                "DatabaseId({}) ({}) Btree is not open",
                db.index(),
                slot.name
            ))
        })
    }

    fn btree_mut(&mut self, db: DatabaseId) -> Result<&mut Btree> {
        let slot = self.slots.get_mut(db.index()).ok_or_else(|| {
            SqliteError::misuse(format!("DatabaseId({}) out of range", db.index()))
        })?;
        slot.btree.as_mut().ok_or_else(|| {
            SqliteError::misuse(format!(
                "DatabaseId({}) ({}) Btree is not open",
                db.index(),
                slot.name
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqliters_types::SQLITE_DEFAULT_PAGE_SIZE;
    use sqliters_vfs::OsVfs;

    #[test]
    fn slot_collapse_and_detach_temp_triggers_preserve_schema_identity() {
        let main_bt = Btree::open_in_memory().unwrap();
        let vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());
        let flag = Arc::new(AtomicBool::new(false));
        let mut set = DatabaseSet::new(
            main_bt,
            Schema::default(),
            vfs,
            SQLITE_DEFAULT_PAGE_SIZE,
            LockGeometry::default(),
            TempStoreMode::Memory,
            false,
            flag,
        );

        assert_eq!(set.find_db_id("main"), Some(DatabaseId::MAIN));
        assert_eq!(set.find_db_id("TEMP"), Some(DatabaseId::TEMP));
        assert_eq!(set.find_open_db_id_for_detach("temp"), None);

        let id_a = set
            .attach_slot(
                "aux_a".to_owned(),
                Btree::open_in_memory().unwrap(),
                Schema::default(),
                false,
            )
            .unwrap();
        let id_b = set
            .attach_slot(
                "aux_b".to_owned(),
                Btree::open_in_memory().unwrap(),
                Schema::default(),
                false,
            )
            .unwrap();
        assert_eq!(id_a, DatabaseId(2));
        assert_eq!(id_b, DatabaseId(3));

        let inst_a = set.slot(id_a).unwrap().schema_instance_id;
        let inst_b = set.slot(id_b).unwrap().schema_instance_id;
        assert_ne!(inst_a, inst_b);

        set.register_temp_trigger_binding(TempTriggerBinding {
            trigger_name: "tr_on_a".to_owned(),
            target_schema_instance_id: inst_a,
            target_table_name: "t1".to_owned(),
        });

        // Detach slot 2 (`aux_a`): `aux_b` collapses from slot 3 -> slot 2, keeping `inst_b`!
        let (removed_inst, redirected) = set.detach_slot(id_a).unwrap();
        assert_eq!(removed_inst, inst_a);
        assert_eq!(redirected, 1);
        assert_eq!(set.find_db_id("aux_b"), Some(DatabaseId(2)));
        assert_eq!(set.slot(DatabaseId(2)).unwrap().schema_instance_id, inst_b);
        assert_eq!(
            set.temp_trigger_bindings()[0].target_schema_instance_id,
            set.temp_schema_instance_id()
        );

        // Attaching a new database into slot 3 gets a brand-new `SchemaInstanceId`!
        let id_c = set
            .attach_slot(
                "aux_c".to_owned(),
                Btree::open_in_memory().unwrap(),
                Schema::default(),
                false,
            )
            .unwrap();
        assert_eq!(id_c, DatabaseId(3));
        assert_ne!(set.slot(id_c).unwrap().schema_instance_id, inst_a);
        assert_ne!(set.slot(id_c).unwrap().schema_instance_id, inst_b);
    }
}
