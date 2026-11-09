//! Narrow B-Tree storage routing contract (`BtreeAccess`) for `VdbeVm`.
//!
//! `BtreeAccess` lives in `sqliters-vdbe` (which already depends on `sqliters-btree` and
//! `sqliters-types`) so `VdbeVm::step` can route opcodes and open cursors by [`DatabaseId`]
//! without holding long-lived `&mut Btree` references or introducing a reverse dependency edge
//! to `sqliters-core`.

use sqliters_btree::Btree;
use sqliters_types::{DatabaseId, ErrorCode, Result, SqliteError};

/// Database-indexed B-tree routing interface consumed by [`crate::VdbeVm`].
pub trait BtreeAccess {
    /// Borrows the [`Btree`] attached at logical slot `db`.
    fn btree_ref(&self, db: DatabaseId) -> Result<&Btree>;

    /// Mutably borrows the [`Btree`] attached at logical slot `db`.
    fn btree_mut(&mut self, db: DatabaseId) -> Result<&mut Btree>;
}

/// Standalone single-database implementation mapping [`DatabaseId::MAIN`] (`0`) directly to `self`
/// and rejecting any other database slot index.
impl BtreeAccess for Btree {
    #[inline]
    fn btree_ref(&self, db: DatabaseId) -> Result<&Btree> {
        if db == DatabaseId::MAIN {
            Ok(self)
        } else {
            Err(SqliteError::new(
                ErrorCode::Error,
                format!("unknown database index {}", db.index()),
            ))
        }
    }

    #[inline]
    fn btree_mut(&mut self, db: DatabaseId) -> Result<&mut Btree> {
        if db == DatabaseId::MAIN {
            Ok(self)
        } else {
            Err(SqliteError::new(
                ErrorCode::Error,
                format!("unknown database index {}", db.index()),
            ))
        }
    }
}
