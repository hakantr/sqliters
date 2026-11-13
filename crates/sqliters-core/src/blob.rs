//! Incremental BLOB I/O (`sqlite3_blob_open`, `sqlite3_blob_read`, `sqlite3_blob_write`,
//! `sqlite3_blob_reopen`, `sqlite3_blob_bytes`, `sqlite3_blob_close` in `sqlite/src/vdbeblob.c`).
#![forbid(unsafe_code)]

use std::sync::Arc;

use sqliters_pager::TransactionState;
use sqliters_syntax::QualifiedName;
use sqliters_types::{DatabaseId, ErrorCode, Result, SqliteError};

use crate::db_set::{OpenBlobState, ResolvedRelation};
use crate::{Connection, ConnectionShared, HandleId, HandleKind, PendingDrop, SchemaInstanceId};

/// Returns `true` if `name` is a built-in eponymous virtual table name in SQLite 3.54.1
/// (`pragma_*`, `json_each`, `json_tree`, `generate_series`, `sqlite_dbpage`, `dbstat`).
fn is_eponymous_vtab_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("pragma_")
        || matches!(
            lower.as_str(),
            "json_each" | "json_tree" | "generate_series" | "sqlite_dbpage" | "dbstat"
        )
}

/// Incremental BLOB I/O handle (`sqlite3_blob*` in `sqlite/src/vdbeblob.c`).
///
/// Holds an owned `Arc` reference to the connection state without borrowing `&mut Connection`
/// or holding a `MutexGuard` across calls, allowing [`crate::Statement`]s and other [`Blob`]
/// handles to coexist on the same [`Connection`].
pub struct Blob {
    shared: Arc<ConnectionShared>,
    handle_id: Option<HandleId>,
    schema_instance_id: SchemaInstanceId,
    read_write: bool,
    rowid: i64,
    byte_len: usize,
}

impl std::fmt::Debug for Blob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Blob")
            .field("conn_id", &self.shared.conn_id)
            .field("handle_id", &self.handle_id)
            .field("schema_instance_id", &self.schema_instance_id)
            .field("read_write", &self.read_write)
            .field("rowid", &self.rowid)
            .field("byte_len", &self.byte_len)
            .finish()
    }
}

impl Blob {
    /// Opens an incremental BLOB handle on `(db_name, table_name, column_name, rowid)`
    /// (`sqlite3_blob_open` in `sqlite/src/vdbeblob.c:121-354`).
    pub fn open(
        conn: &Connection,
        db_name: &str,
        table_name: &str,
        column_name: &str,
        rowid: i64,
        read_write: bool,
    ) -> Result<Self> {
        let shared = Arc::clone(conn.shared());
        let mut session = shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;

        let _ = inner.ensure_schemas_loaded_for_blob(true)?;

        let qname = if db_name.is_empty() {
            QualifiedName::unqualified(table_name.to_owned())
        } else {
            QualifiedName::qualified(db_name.to_owned(), table_name.to_owned())
        };

        if let Some(ref s) = qname.schema
            && inner.db_set.find_db_id(s).is_none()
        {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such table: {s}.{table_name}"),
            ));
        }

        // Resolve `(db_name, table_name)` across `db_set` (`sqlite3LocateTable` in `vdbeblob.c:162-194`).
        let (db_id, tbl) = match inner.db_set.find_relation_in_set(&qname)? {
            Some((_, ResolvedRelation::View(_))) => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("cannot open view: {table_name}"),
                ));
            }
            Some((id, ResolvedRelation::Table(t))) => (id, *t),
            None => {
                if is_eponymous_vtab_name(table_name) {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("cannot open virtual table: {table_name}"),
                    ));
                }
                let full_name = if db_name.is_empty() {
                    table_name.to_owned()
                } else {
                    format!("{db_name}.{table_name}")
                };
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {full_name}"),
                ));
            }
        };

        // Virtual table check (`IsVirtual(pTab)` in `vdbeblob.c:163-166`).
        let raw_upper = tbl.raw_sql.trim_start().to_ascii_uppercase();
        if raw_upper.starts_with("CREATE VIRTUAL TABLE") {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("cannot open virtual table: {table_name}"),
            ));
        }

        // `WITHOUT ROWID` check (`!HasRowid(pTab)` in `vdbeblob.c:167-170`).
        if tbl.without_rowid {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("cannot open table without rowid: {table_name}"),
            ));
        }

        // Generated column check (`(pTab->tabFlags & TF_HasGenerated) != 0` in `vdbeblob.c:171-175`).
        if tbl.columns.iter().any(|c| c.generated.is_some()) {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("cannot open table with generated columns: {table_name}"),
            ));
        }

        // Ensure `"temp"` is open if targeting `"temp"` (`vdbeblob.c:183-184`).
        if db_id == DatabaseId::TEMP && !inner.db_set.is_temp_open() {
            let enc = inner.effective_encoding();
            let ws = inner.writable_schema;
            inner.db_set.ensure_temp_open(enc, ws)?;
        }

        // Locate target column (`sqlite3ColumnIndex(pTab, zColumn)` in `vdbeblob.c:199-206`).
        let Some((col_idx, col_schema)) = tbl.find_column(column_name) else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such column: \"{column_name}\""),
            ));
        };
        let canonical_col_name = col_schema.name.clone();

        // Additional guards when opening for read-write (`wrFlag != 0` in `vdbeblob.c:211-248`).
        if read_write {
            let is_slot_readonly = inner
                .db_set
                .slot(db_id)
                .and_then(|s| s.btree.as_ref())
                .is_some_and(|bt| bt.pager().is_readonly());
            if shared.is_readonly || is_slot_readonly {
                return Err(SqliteError::new(
                    ErrorCode::ReadOnly,
                    "attempt to write a readonly database",
                ));
            }
            if database_set_is_catalog(db_id, &tbl.name) && !inner.writable_schema {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("table {} may not be modified", tbl.name),
                ));
            }

            let mut z_fault: Option<&'static str> = None;

            if inner.foreign_keys {
                let is_child_fk = tbl.foreign_keys.iter().any(|fk| {
                    fk.from_cols.contains(&col_idx)
                        || fk
                            .from_col_names
                            .iter()
                            .any(|fc| fc.eq_ignore_ascii_case(&canonical_col_name))
                });
                let is_parent_fk = inner.db_set.slot(db_id).is_some_and(|slot| {
                    slot.schema.tables.iter().any(|child_tbl| {
                        child_tbl.foreign_keys.iter().any(|fk| {
                            if !fk.ref_table.eq_ignore_ascii_case(&tbl.name) {
                                return false;
                            }
                            match &fk.ref_cols {
                                None => tbl.columns.get(col_idx).is_some_and(|c| c.is_primary_key),
                                Some(cols) => cols
                                    .iter()
                                    .any(|tc| tc.eq_ignore_ascii_case(&canonical_col_name)),
                            }
                        })
                    })
                });
                if is_child_fk || is_parent_fk {
                    z_fault = Some("foreign key");
                }
            }

            let is_ipk = tbl.rowid_alias_col == Some(col_idx);
            let is_indexed = inner.db_set.slot(db_id).is_some_and(|slot| {
                slot.schema.indexes_for_table(&tbl.name).iter().any(|idx| {
                    idx.columns
                        .iter()
                        .any(|ic| ic.col_idx == Some(col_idx) || ic.expr.is_some())
                })
            });
            if is_ipk || is_indexed {
                z_fault = Some("indexed");
            }

            if let Some(fault) = z_fault {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("cannot open {fault} column for writing"),
                ));
            }
        }

        let (inst_id, slot_name, root_pgno) = {
            let slot = inner
                .db_set
                .slot(db_id)
                .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
            let live_root = if database_set_is_catalog(db_id, &tbl.name) {
                1
            } else {
                slot.schema
                    .find_user_table(&tbl.name)
                    .map_or(tbl.root_pgno, |t| t.root_pgno)
            };
            (slot.schema_instance_id, slot.name.clone(), live_root)
        };

        // Acquire the B-tree read or write transaction on `db_id`.
        let sp_depth = inner.savepoints.len();
        let mut started_slot_tx = false;
        let prev_tx_state = {
            let slot = inner
                .db_set
                .slot_mut(db_id)
                .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
            let bt = slot
                .btree
                .as_mut()
                .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
            let prev = bt.transaction_state();
            if read_write {
                if prev != TransactionState::Write {
                    inner.busy_policy.retry_btree(bt, |b| {
                        b.pager_mut().begin_write_with_savepoint_depth(sp_depth)
                    })?;
                    started_slot_tx = true;
                } else {
                    bt.pager_mut().begin_write_with_savepoint_depth(sp_depth)?;
                }
            } else if prev == TransactionState::Open {
                inner.busy_policy.retry_btree(bt, |b| b.begin_read())?;
                started_slot_tx = true;
            }
            prev
        };

        // Seek to `rowid` and verify the cell value is `TEXT` or `BLOB` (`blobSeekToRow` in `vdbeblob.c:54-116`).
        let seek_res = if root_pgno == 0 {
            Ok(None)
        } else if tbl.rowid_alias_col == Some(col_idx) {
            // An `INTEGER PRIMARY KEY` column in a rowid table has integer value `rowid` (or null in record header).
            let slot = inner.db_set.slot_mut(db_id).unwrap();
            let bt = slot.btree.as_mut().unwrap();
            match bt.seek_table_rowid(root_pgno, rowid)? {
                Some(_) => Err(SqliteError::new(
                    ErrorCode::Error,
                    "cannot open value of type integer",
                )),
                None => Ok(None),
            }
        } else {
            let slot = inner.db_set.slot_mut(db_id).unwrap();
            let bt = slot.btree.as_mut().unwrap();
            bt.inspect_table_row_column(root_pgno, rowid, col_idx)
        };

        let (_col_offset, byte_len) = match seek_res {
            Ok(Some(found)) => found,
            Ok(None) => {
                cleanup_failed_open_tx(
                    inner,
                    db_id,
                    inst_id,
                    read_write,
                    started_slot_tx,
                    prev_tx_state,
                );
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such rowid: {rowid}"),
                ));
            }
            Err(err) => {
                cleanup_failed_open_tx(
                    inner,
                    db_id,
                    inst_id,
                    read_write,
                    started_slot_tx,
                    prev_tx_state,
                );
                return Err(err);
            }
        };

        if !inner.is_autocommit() {
            inner.in_user_btree_tx = true;
        }

        let hid = inner.register_handle(HandleKind::Blob {
            writable: read_write,
        });
        if read_write {
            inner
                .activity
                .start_with_instances(hid, true, true, vec![inst_id], vec![inst_id]);
        } else {
            inner
                .activity
                .start_with_instances(hid, true, false, vec![inst_id], Vec::new());
        }

        inner.db_set.open_blobs_mut().insert(
            hid.0,
            OpenBlobState {
                handle_id: hid.0,
                schema_instance_id: inst_id,
                db_name: slot_name,
                table_name: tbl.name,
                column_name: canonical_col_name,
                col_idx,
                root_pgno,
                rowid,
                byte_len,
                read_write,
                started_slot_tx,
                expired: false,
                table_invalidated: false,
            },
        );

        drop(session);

        Ok(Self {
            shared,
            handle_id: Some(hid),
            schema_instance_id: inst_id,
            read_write,
            rowid,
            byte_len,
        })
    }

    /// Returns the byte length of the currently opened BLOB/TEXT value (`sqlite3_blob_bytes`).
    #[inline]
    pub fn bytes(&self) -> usize {
        self.len()
    }

    /// Returns the byte length of the currently opened BLOB/TEXT value (`sqlite3_blob_bytes`).
    #[inline]
    pub fn len(&self) -> usize {
        let Some(hid) = self.handle_id else {
            return 0;
        };
        if let Ok(session) = self.shared.enter()
            && let Some(state) = session.inner().db_set.open_blobs().get(&hid.0)
        {
            if state.expired || state.table_invalidated {
                return 0;
            }
            return state.byte_len;
        }
        self.byte_len
    }

    /// Returns `true` if the opened BLOB/TEXT value has a byte length of `0`.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the rowid currently associated with this BLOB handle.
    #[inline]
    pub fn rowid(&self) -> i64 {
        self.rowid
    }

    /// Reads `buf.len()` bytes starting at 0-based `offset` from the opened BLOB/TEXT value
    /// (`sqlite3_blob_read` in `sqlite/src/vdbeblob.c:471-473`).
    pub fn read_at(&self, buf: &mut [u8], offset: usize) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;

        let Some(hid) = self.handle_id else {
            return Err(SqliteError::misuse("blob handle is closed"));
        };

        let Some(state) = inner.db_set.open_blobs().get(&hid.0).cloned() else {
            return Err(SqliteError::new(
                ErrorCode::Abort,
                "blob handle has expired",
            ));
        };
        if state.table_invalidated {
            let _ = inner.finish_blob_activity(
                hid,
                self.schema_instance_id,
                self.read_write,
                &self.shared,
            );
            return Err(SqliteError::new(
                ErrorCode::Abort,
                "blob handle has expired",
            ));
        }
        if state.expired {
            return Err(SqliteError::new(
                ErrorCode::Abort,
                "blob handle has expired",
            ));
        }

        let Some(end) = offset.checked_add(buf.len()) else {
            return Err(SqliteError::new(ErrorCode::Error, "blob read out of range"));
        };
        if end > state.byte_len {
            return Err(SqliteError::new(ErrorCode::Error, "blob read out of range"));
        }
        if buf.is_empty() {
            return Ok(());
        }

        let Some(db_id) = inner
            .db_set
            .find_db_id_by_instance(state.schema_instance_id)
        else {
            let _ = inner.finish_blob_activity(
                hid,
                self.schema_instance_id,
                self.read_write,
                &self.shared,
            );
            return Err(SqliteError::new(
                ErrorCode::Abort,
                "blob handle has expired",
            ));
        };

        let inspect_res = {
            let slot = inner
                .db_set
                .slot_mut(db_id)
                .ok_or_else(|| SqliteError::new(ErrorCode::Abort, "blob handle has expired"))?;
            let bt = slot
                .btree
                .as_mut()
                .ok_or_else(|| SqliteError::new(ErrorCode::Abort, "blob handle has expired"))?;
            bt.inspect_table_row_column(state.root_pgno, state.rowid, state.col_idx)
        };

        let (col_payload_offset, live_len) = match inspect_res {
            Ok(Some(v)) => v,
            Ok(None) | Err(_) => {
                let _ = inner.finish_blob_activity(
                    hid,
                    self.schema_instance_id,
                    self.read_write,
                    &self.shared,
                );
                return Err(SqliteError::new(
                    ErrorCode::Abort,
                    "blob handle has expired",
                ));
            }
        };

        if live_len != state.byte_len {
            let _ = inner.finish_blob_activity(
                hid,
                self.schema_instance_id,
                self.read_write,
                &self.shared,
            );
            return Err(SqliteError::new(
                ErrorCode::Abort,
                "blob handle has expired",
            ));
        }

        let slot = inner.db_set.slot_mut(db_id).unwrap();
        let bt = slot.btree.as_mut().unwrap();
        bt.read_table_payload_slice(
            state.root_pgno,
            state.rowid,
            col_payload_offset + offset,
            buf,
        )
    }

    /// Writes `data` at 0-based `offset` into the opened BLOB/TEXT value in-place through the
    /// B-tree and Pager (`sqlite3_blob_write` in `sqlite/src/vdbeblob.c:478-480`).
    ///
    /// Never changes the length of the BLOB/TEXT value; returns `ErrorCode::Error` if
    /// `offset + data.len() > self.bytes()`, or `ErrorCode::ReadOnly` if opened read-only.
    pub fn write_at(&mut self, data: &[u8], offset: usize) -> Result<()> {
        if !self.read_write {
            return Err(SqliteError::new(
                ErrorCode::ReadOnly,
                "cannot write to read-only blob",
            ));
        }

        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;

        let Some(hid) = self.handle_id else {
            return Err(SqliteError::misuse("blob handle is closed"));
        };

        let Some(state) = inner.db_set.open_blobs().get(&hid.0).cloned() else {
            self.byte_len = 0;
            return Err(SqliteError::new(
                ErrorCode::Abort,
                "blob handle has expired",
            ));
        };
        if state.table_invalidated {
            self.byte_len = 0;
            let _ = inner.finish_blob_activity(
                hid,
                self.schema_instance_id,
                self.read_write,
                &self.shared,
            );
            return Err(SqliteError::new(
                ErrorCode::Abort,
                "blob handle has expired",
            ));
        }
        if state.expired {
            self.byte_len = 0;
            return Err(SqliteError::new(
                ErrorCode::Abort,
                "blob handle has expired",
            ));
        }

        let Some(end) = offset.checked_add(data.len()) else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "blob write out of range",
            ));
        };
        if end > state.byte_len {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "blob write out of range",
            ));
        }
        if data.is_empty() {
            return Ok(());
        }

        let Some(db_id) = inner
            .db_set
            .find_db_id_by_instance(state.schema_instance_id)
        else {
            self.byte_len = 0;
            let _ = inner.finish_blob_activity(
                hid,
                self.schema_instance_id,
                self.read_write,
                &self.shared,
            );
            return Err(SqliteError::new(
                ErrorCode::Abort,
                "blob handle has expired",
            ));
        };

        let sp_depth = inner.savepoints.len();
        let inspect_res = {
            let slot = inner
                .db_set
                .slot_mut(db_id)
                .ok_or_else(|| SqliteError::new(ErrorCode::Abort, "blob handle has expired"))?;
            let bt = slot
                .btree
                .as_mut()
                .ok_or_else(|| SqliteError::new(ErrorCode::Abort, "blob handle has expired"))?;
            bt.pager_mut().begin_write_with_savepoint_depth(sp_depth)?;
            bt.inspect_table_row_column(state.root_pgno, state.rowid, state.col_idx)
        };

        let (col_payload_offset, live_len) = match inspect_res {
            Ok(Some(v)) => v,
            Ok(None) | Err(_) => {
                self.byte_len = 0;
                let _ = inner.finish_blob_activity(
                    hid,
                    self.schema_instance_id,
                    self.read_write,
                    &self.shared,
                );
                return Err(SqliteError::new(
                    ErrorCode::Abort,
                    "blob handle has expired",
                ));
            }
        };

        if live_len != state.byte_len {
            self.byte_len = 0;
            let _ = inner.finish_blob_activity(
                hid,
                self.schema_instance_id,
                self.read_write,
                &self.shared,
            );
            return Err(SqliteError::new(
                ErrorCode::Abort,
                "blob handle has expired",
            ));
        }

        let slot = inner.db_set.slot_mut(db_id).unwrap();
        let bt = slot.btree.as_mut().unwrap();
        bt.write_table_payload_slice(
            state.root_pgno,
            state.rowid,
            col_payload_offset + offset,
            data,
        )
    }

    /// Moves this BLOB handle to point at `new_rowid` in the same table and column
    /// (`sqlite3_blob_reopen` in `sqlite/src/vdbeblob.c:503-532`).
    pub fn reopen(&mut self, new_rowid: i64) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;

        let Some(hid) = self.handle_id else {
            return Err(SqliteError::misuse("blob handle is closed"));
        };

        let Some(state) = inner.db_set.open_blobs().get(&hid.0).cloned() else {
            self.byte_len = 0;
            return Err(SqliteError::new(
                ErrorCode::Abort,
                "blob handle has expired",
            ));
        };
        if state.table_invalidated {
            self.byte_len = 0;
            let _ = inner.finish_blob_activity(
                hid,
                self.schema_instance_id,
                self.read_write,
                &self.shared,
            );
            return Err(SqliteError::new(
                ErrorCode::Abort,
                "blob handle has expired",
            ));
        }

        let Some(db_id) = inner
            .db_set
            .find_db_id_by_instance(state.schema_instance_id)
        else {
            self.byte_len = 0;
            let _ = inner.finish_blob_activity(
                hid,
                self.schema_instance_id,
                self.read_write,
                &self.shared,
            );
            return Err(SqliteError::new(
                ErrorCode::Abort,
                "blob handle has expired",
            ));
        };

        // Verify the table still exists in the slot schema.
        let live_root = {
            let Some(slot) = inner.db_set.slot(db_id) else {
                self.byte_len = 0;
                let _ = inner.finish_blob_activity(
                    hid,
                    self.schema_instance_id,
                    self.read_write,
                    &self.shared,
                );
                return Err(SqliteError::new(
                    ErrorCode::Abort,
                    "blob handle has expired",
                ));
            };
            if database_set_is_catalog(db_id, &state.table_name) {
                1
            } else if let Some(t) = slot.schema.find_user_table(&state.table_name) {
                t.root_pgno
            } else {
                self.byte_len = 0;
                let _ = inner.finish_blob_activity(
                    hid,
                    self.schema_instance_id,
                    self.read_write,
                    &self.shared,
                );
                return Err(SqliteError::new(
                    ErrorCode::Abort,
                    "blob handle has expired",
                ));
            }
        };

        let seek_res = {
            let slot = inner.db_set.slot_mut(db_id).unwrap();
            let Some(ref mut bt) = slot.btree else {
                self.byte_len = 0;
                let _ = inner.finish_blob_activity(
                    hid,
                    self.schema_instance_id,
                    self.read_write,
                    &self.shared,
                );
                return Err(SqliteError::new(
                    ErrorCode::Abort,
                    "blob handle has expired",
                ));
            };
            bt.inspect_table_row_column(live_root, new_rowid, state.col_idx)
        };

        match seek_res {
            Ok(Some((_col_offset, new_len))) => {
                if let Some(st) = inner.db_set.open_blobs_mut().get_mut(&hid.0) {
                    st.root_pgno = live_root;
                    st.rowid = new_rowid;
                    st.byte_len = new_len;
                    st.expired = false;
                }
                self.rowid = new_rowid;
                self.byte_len = new_len;
                Ok(())
            }
            Ok(None) => {
                self.byte_len = 0;
                let _ = inner.finish_blob_activity(
                    hid,
                    self.schema_instance_id,
                    self.read_write,
                    &self.shared,
                );
                Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such rowid: {new_rowid}"),
                ))
            }
            Err(err) => {
                self.byte_len = 0;
                let _ = inner.finish_blob_activity(
                    hid,
                    self.schema_instance_id,
                    self.read_write,
                    &self.shared,
                );
                Err(err)
            }
        }
    }

    /// Closes this BLOB handle (`sqlite3_blob_close` in `sqlite/src/vdbeblob.c:360-376`),
    /// releasing its B-tree lease and committing any autocommit write transaction if this handle
    /// was the last active writer.
    pub fn close(mut self) -> Result<()> {
        if self.handle_id.is_none() {
            return Ok(());
        }
        // Enter BEFORE giving up the handle id: when the entry is refused (`SQLITE_MISUSE` from
        // inside a callback of this connection, `SQLITE_BUSY` while another thread is using it),
        // `Drop` still owns the handle and queues the same cleanup on the connection. Taking the
        // id first leaked the handle registration and its B-tree lease for good.
        let shared = Arc::clone(&self.shared);
        let mut session = shared.enter()?;
        let Some(hid) = self.handle_id.take() else {
            return Ok(());
        };
        let inner = session.inner_mut();
        inner.close_blob_handle(hid, self.schema_instance_id, self.read_write, &self.shared)
    }
}

impl Drop for Blob {
    fn drop(&mut self) {
        if let Some(hid) = self.handle_id.take() {
            self.shared.enqueue_or_run_drop(PendingDrop::Blob {
                handle_id: hid,
                schema_instance_id: self.schema_instance_id,
                writable: self.read_write,
            });
        }
    }
}

fn database_set_is_catalog(db_id: DatabaseId, table_name: &str) -> bool {
    crate::DatabaseSet::is_catalog_table_in_db(db_id, table_name)
}

fn cleanup_failed_open_tx(
    inner: &mut crate::ConnectionInner,
    db_id: DatabaseId,
    inst_id: SchemaInstanceId,
    read_write: bool,
    started_slot_tx: bool,
    prev_tx_state: TransactionState,
) {
    if !started_slot_tx {
        return;
    }
    if !inner.is_autocommit() || inner.activity.has_active_tx_on_instance(inst_id) {
        return;
    }
    if let Some(slot) = inner.db_set.slot_mut(db_id)
        && let Some(ref mut bt) = slot.btree
    {
        if read_write && bt.transaction_state() == TransactionState::Write {
            let retain_read = prev_tx_state == TransactionState::Read;
            let _ = bt.rollback_with_trip(
                sqliters_types::extended::SQLITE_ABORT_ROLLBACK,
                true,
                retain_read,
            );
        } else if !read_write && bt.transaction_state() == TransactionState::Read {
            let _ = bt.pager_mut().end_read_if_unused();
        }
    }
}
