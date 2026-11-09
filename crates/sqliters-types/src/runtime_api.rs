//! Cross-phase runtime value and execution-service contracts (`DatabaseId`, `RuntimeValue`,
//! `FunctionFlags`, `AggregateStepId`, `RowMutationOp`, and `ExecutionServices`).
//!
//! This module lives in `sqliters-types` with **zero** dependency on `Btree`, `Pager`, `VdbeVm`,
//! or `Connection`, enabling both `sqliters-vdbe` and `sqliters-core` to share a single execution
//! service boundary without introducing reverse crate edges.

use std::cmp::Ordering;
use std::ops::{BitAnd, BitOr, BitOrAssign};

use crate::error::{ErrorCode, Result, SqliteError};
use crate::value::Value;

/// Maximum number of attached databases per connection (`SQLITE_MAX_ATTACHED` in `sqliteLimit.h`).
pub const SQLITE_MAX_ATTACHED: usize = 10;

/// Total database slot capacity (`0: main`, `1: temp`, `2..=11: attached`).
pub const MAX_DB_SLOTS: usize = 2 + SQLITE_MAX_ATTACHED;

/// Logical database slot index (`iDb` in SQLite `db->aDb[iDb]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct DatabaseId(pub usize);

impl DatabaseId {
    /// Slot `0`: primary database (`main`).
    pub const MAIN: Self = Self(0);

    /// Slot `1`: temporary database (`temp`).
    pub const TEMP: Self = Self(1);

    /// Returns the raw zero-based slot index.
    #[inline]
    pub const fn index(self) -> usize {
        self.0
    }

    /// Returns `true` if this identifier refers to `main` (`0`).
    #[inline]
    pub const fn is_main(self) -> bool {
        self.0 == 0
    }

    /// Returns `true` if this identifier refers to `temp` (`1`).
    #[inline]
    pub const fn is_temp(self) -> bool {
        self.0 == 1
    }

    /// Returns `true` if this identifier refers to an attached database slot (`2..=11`).
    #[inline]
    pub const fn is_attached(self) -> bool {
        self.0 >= 2 && self.0 < MAX_DB_SLOTS
    }
}

/// Ephemeral runtime SQL value pairing one of the 5 on-disk [`Value`] storage classes with an
/// in-flight 8-bit `subtype` tag (`Mem.eSubtype` / `MEM_Subtype` in `sqlite/src/vdbeInt.h`).
///
/// `subtype` is **never** persisted into SQLite Format 3 records, B-trees, or sorter keys;
/// reading from disk or sorter records always yields `subtype == 0`.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeValue {
    pub value: Value,
    pub subtype: u8,
}

impl Default for RuntimeValue {
    #[inline]
    fn default() -> Self {
        Self::null()
    }
}

impl RuntimeValue {
    /// Constructs a plain runtime value with `subtype == 0`.
    #[inline]
    pub const fn plain(value: Value) -> Self {
        Self { value, subtype: 0 }
    }

    /// Constructs a runtime value truncating `subtype` to its low 8 bits (`(subtype & 0xff) as u8`),
    /// matching `sqlite3_result_subtype` (`sqlite/src/vdbeapi.c:579`).
    #[inline]
    pub const fn with_subtype(value: Value, subtype: u32) -> Self {
        Self {
            value,
            subtype: (subtype & 0xff) as u8,
        }
    }

    /// Constructs a plain `NULL` runtime value (`subtype == 0`).
    #[inline]
    pub const fn null() -> Self {
        Self {
            value: Value::Null,
            subtype: 0,
        }
    }

    /// Clears `subtype` to `0` in place (`OP_ClrSubtype`).
    #[inline]
    pub fn clear_subtype(&mut self) {
        self.subtype = 0;
    }

    /// Consumes `self` and returns a copy with `subtype == 0`.
    #[inline]
    pub fn without_subtype(mut self) -> Self {
        self.subtype = 0;
        self
    }

    /// Returns `true` if `self.subtype != 0`.
    #[inline]
    pub const fn has_subtype(&self) -> bool {
        self.subtype != 0
    }
}

impl From<Value> for RuntimeValue {
    #[inline]
    fn from(value: Value) -> Self {
        Self::plain(value)
    }
}

impl From<RuntimeValue> for Value {
    #[inline]
    fn from(rv: RuntimeValue) -> Self {
        rv.value
    }
}

/// Scalar/aggregate/window function property bitflags (`sqlite.h.in` `SQLITE_DETERMINISTIC`..`SQLITE_RESULT_SUBTYPE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FunctionFlags(pub u32);

impl FunctionFlags {
    pub const NONE: Self = Self(0);
    /// Function always produces the same output for identical inputs (`SQLITE_DETERMINISTIC`).
    pub const DETERMINISTIC: Self = Self(0x0000_0800);
    /// Function may only be invoked from top-level SQL (`SQLITE_DIRECTONLY`).
    pub const DIRECTONLY: Self = Self(0x0008_0000);
    /// Function inspects incoming argument subtypes via `sqlite3_value_subtype` (`SQLITE_SUBTYPE`).
    pub const SUBTYPE: Self = Self(0x0010_0000);
    /// Function has no side effects and is safe inside untrusted schemas (`SQLITE_INNOCUOUS`).
    pub const INNOCUOUS: Self = Self(0x0020_0000);
    /// Function is permitted to set result subtypes via `sqlite3_result_subtype` (`SQLITE_RESULT_SUBTYPE`).
    pub const RESULT_SUBTYPE: Self = Self(0x0100_0000);

    #[inline]
    pub const fn contains(self, flag: Self) -> bool {
        (self.0 & flag.0) == flag.0
    }

    #[inline]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[inline]
    pub const fn without(self, flag: Self) -> Self {
        Self(self.0 & !flag.0)
    }
}

impl BitOr for FunctionFlags {
    type Output = Self;

    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl BitOrAssign for FunctionFlags {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for FunctionFlags {
    type Output = Self;

    #[inline]
    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

/// Owned handle identifying an in-flight aggregate or window accumulator instance managed by an
/// [`ExecutionServices`] implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AggregateStepId(pub usize);

/// Row-level DML operation kind for mutation/trigger/hook pipelines (`SQLITE_INSERT`, `SQLITE_UPDATE`, `SQLITE_DELETE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowMutationOp {
    Insert,
    Update,
    Delete,
}

/// Storage-independent row mutation notification metadata passed to [`ExecutionServices::on_row_mutation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowMutationEvent<'a> {
    pub db: DatabaseId,
    pub db_name: &'a str,
    pub table_name: &'a str,
    pub op: RowMutationOp,
    pub rowid: i64,
}

/// Strips incoming argument subtypes when invoking a function whose [`FunctionFlags`] lacks
/// [`FunctionFlags::SUBTYPE`] (`sqlite/src/vdbe.c` `OP_Function` `SQLITE_SUBTYPE` check).
#[inline]
pub fn strip_unsubscribed_subtypes<'a>(
    flags: FunctionFlags,
    args: &'a [RuntimeValue],
    scratch: &'a mut Vec<RuntimeValue>,
) -> &'a [RuntimeValue] {
    if flags.contains(FunctionFlags::SUBTYPE) || args.iter().all(|a| a.subtype == 0) {
        args
    } else {
        scratch.clear();
        scratch.extend(args.iter().cloned().map(RuntimeValue::without_subtype));
        scratch.as_slice()
    }
}

/// Enforces `SQLITE_STRICT_SUBTYPE` (`sqlite/src/vdbeapi.c:568-575`): if a scalar function returns
/// `subtype != 0` without [`FunctionFlags::RESULT_SUBTYPE`] while `strict_subtype` is enabled,
/// fails with `ErrorCode::Error` `"misuse of sqlite3_result_subtype() by <func>()"`.
#[inline]
pub fn enforce_result_subtype_contract(
    func_name: &str,
    flags: FunctionFlags,
    strict_subtype: bool,
    result: RuntimeValue,
) -> Result<RuntimeValue> {
    if result.subtype != 0 && !flags.contains(FunctionFlags::RESULT_SUBTYPE) && strict_subtype {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("misuse of sqlite3_result_subtype() by {func_name}()"),
        ));
    }
    Ok(result)
}

/// Storage-independent runtime execution services shared by compiled VDBE programs and the
/// relational interpreter (`InterruptHandle`, progress handler ticks, scalar/aggregate/window
/// invocation, fallible collations, and row-mutation hook notifications).
pub trait ExecutionServices {
    /// Checks whether execution has been interrupted (`sqlite3_interrupt` / `AtomicBool`),
    /// returning `Err(ErrorCode::Interrupt)` if interrupted.
    fn check_interrupted(&mut self) -> Result<()>;

    /// Advances the logical progress counter by one unit and invokes any configured progress
    /// callback (`sqlite3_progress_handler`).
    fn tick_progress(&mut self) -> Result<()>;

    /// Invokes a scalar SQL function with ephemeral [`RuntimeValue`] arguments and returns its
    /// [`RuntimeValue`] output after enforcing `FunctionFlags::SUBTYPE` input filtering and
    /// `FunctionFlags::RESULT_SUBTYPE` strict validation.
    fn call_scalar(&mut self, name: &str, args: &[RuntimeValue]) -> Result<RuntimeValue>;

    /// Compares two UTF-8 strings under the named collation sequence (`BINARY`, `NOCASE`, `RTRIM`,
    /// or user-registered collation).
    fn compare_collated(
        &mut self,
        collation_name: &str,
        left: &str,
        right: &str,
    ) -> Result<Ordering>;

    /// Whether `SQLITE_STRICT_SUBTYPE` enforcement is active (default `true` in SQLite 3.54.1).
    #[inline]
    fn strict_subtype(&self) -> bool {
        true
    }

    /// Opens a stateful aggregate or window accumulator instance.
    fn agg_open(&mut self, name: &str, _arity: usize) -> Result<AggregateStepId> {
        Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such function: {name}"),
        ))
    }

    /// Feeds one row of arguments into an open aggregate/window accumulator (`xStep`).
    fn agg_step(&mut self, _id: AggregateStepId, _args: &[RuntimeValue]) -> Result<()> {
        Err(SqliteError::misuse(
            "aggregate step on unopened accumulator",
        ))
    }

    /// Reads the current window value without consuming accumulator state (`xValue`).
    fn agg_value(&mut self, _id: AggregateStepId) -> Result<RuntimeValue> {
        Err(SqliteError::misuse(
            "aggregate value on unopened accumulator",
        ))
    }

    /// Removes a departing window frame row from an open accumulator (`xInverse`).
    fn agg_inverse(&mut self, _id: AggregateStepId, _args: &[RuntimeValue]) -> Result<()> {
        Err(SqliteError::misuse(
            "aggregate inverse on unopened accumulator",
        ))
    }

    /// Finalizes and releases an open aggregate/window accumulator (`xFinal`).
    fn agg_finalize(&mut self, _id: AggregateStepId) -> Result<RuntimeValue> {
        Err(SqliteError::misuse(
            "aggregate finalize on unopened accumulator",
        ))
    }

    /// Notifies registered row-mutation hooks (`sqlite3_update_hook` / invalidators).
    fn on_row_mutation(&mut self, _event: RowMutationEvent<'_>) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_id_slots_and_helpers() {
        assert_eq!(DatabaseId::default(), DatabaseId::MAIN);
        assert_eq!(DatabaseId::MAIN.index(), 0);
        assert_eq!(DatabaseId::TEMP.index(), 1);
        assert!(DatabaseId::MAIN.is_main());
        assert!(!DatabaseId::MAIN.is_temp());
        assert!(!DatabaseId::MAIN.is_attached());
        assert!(DatabaseId::TEMP.is_temp());
        assert!(DatabaseId(2).is_attached());
        assert!(DatabaseId(11).is_attached());
        assert!(!DatabaseId(12).is_attached());
        assert_eq!(MAX_DB_SLOTS, 12);
    }

    #[test]
    fn runtime_value_subtype_mask_and_helpers() {
        let plain = RuntimeValue::from(Value::Text("abc".to_owned()));
        assert_eq!(plain.subtype, 0);
        assert!(!plain.has_subtype());

        let masked = RuntimeValue::with_subtype(Value::Text("{}".to_owned()), 0x14A);
        assert_eq!(masked.subtype, 0x4A);
        assert!(masked.has_subtype());

        let mut cleared = masked.clone();
        cleared.clear_subtype();
        assert_eq!(cleared.subtype, 0);
        assert_eq!(masked.without_subtype().subtype, 0);
    }

    #[test]
    fn subtype_contract_stripping_and_strict_enforcement() {
        let args = vec![
            RuntimeValue::with_subtype(Value::Text("{}".to_owned()), 74),
            RuntimeValue::plain(Value::Integer(1)),
        ];
        let mut scratch = Vec::new();
        let stripped =
            strip_unsubscribed_subtypes(FunctionFlags::DETERMINISTIC, &args, &mut scratch);
        assert_eq!(stripped[0].subtype, 0);
        assert_eq!(stripped[0].value, Value::Text("{}".to_owned()));

        let preserved = strip_unsubscribed_subtypes(
            FunctionFlags::DETERMINISTIC | FunctionFlags::SUBTYPE,
            &args,
            &mut scratch,
        );
        assert_eq!(preserved[0].subtype, 74);

        let bad = enforce_result_subtype_contract(
            "bad_fn",
            FunctionFlags::DETERMINISTIC,
            true,
            RuntimeValue::with_subtype(Value::Text("x".to_owned()), 74),
        )
        .unwrap_err();
        assert_eq!(bad.code, ErrorCode::Error);
        assert_eq!(
            bad.message,
            "misuse of sqlite3_result_subtype() by bad_fn()"
        );

        let ok = enforce_result_subtype_contract(
            "good_fn",
            FunctionFlags::RESULT_SUBTYPE,
            true,
            RuntimeValue::with_subtype(Value::Text("x".to_owned()), 74),
        )
        .unwrap();
        assert_eq!(ok.subtype, 74);
    }
}
