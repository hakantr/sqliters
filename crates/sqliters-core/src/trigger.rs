//! Trigger catalog descriptors, `SchemaFixer` schema-isolation pinning, `RAISE()` scope guards,
//! and static DDL validation (`sqlite/src/trigger.c` and `sqlite/src/attach.c`).
#![forbid(unsafe_code)]

use sqliters_syntax::{
    ColumnRef, CommonTableExpr, ConflictAction, CreateTriggerStmt, DeleteStmt, Expr, FromClause,
    FunctionArgs, IndexedColumn, InsertSource, InsertStmt, JoinClause, JoinConstraint, LimitClause,
    OrderByTerm, QualifiedName, ResultColumn, SelectBody, SelectCore, SelectStmt, SetAssignment,
    TableOrSubquery, TriggerEvent, TriggerStep, TriggerTiming, UpdateStmt, UpsertAction,
    UpsertClause, UpsertTarget,
};
use sqliters_types::{DatabaseId, ErrorCode, Result, SqliteError};

use crate::db_set::{DatabaseSet, ResolvedRelation, SchemaInstanceId};

/// Default `SQLITE_LIMIT_TRIGGER_STEPS` (`SQLITE_MAX_TRIGGER_STEPS` in `sqliteLimit.h`).
pub const SQLITE_MAX_TRIGGER_STEPS: usize = 65_000;

/// Default `SQLITE_LIMIT_TRIGGER_DEPTH` (`SQLITE_MAX_TRIGGER_DEPTH` in `sqliteLimit.h`).
pub const SQLITE_MAX_TRIGGER_DEPTH: usize = 1_000;

/// Parsed trigger entry from `sqlite_schema` / `sqlite_temp_schema` (`type = 'trigger'`, `rootpage = 0`).
#[derive(Debug, Clone)]
pub struct TriggerSchema {
    pub schema_rowid: i64,
    pub name: String,
    pub tbl_name: String,
    /// Explicit schema qualifier written on `ON <schema>.<table>`, if any (used when resolving `TEMP` triggers).
    pub on_table_schema: Option<String>,
    /// Resolved target schema instance for `TEMP` triggers (`Trigger::pTabSchema` in `sqliteInt.h`).
    pub target_schema_instance_id: Option<SchemaInstanceId>,
    /// `true` when `detach_temp_triggers` redirected this `TEMP` trigger away from a detached database.
    pub detached_to_temp: bool,
    /// `true` when loading `TEMP` schema found this trigger's target missing or incompatible (`db->init.orphanTrigger`).
    pub is_orphan: bool,
    pub timing: TriggerTiming,
    pub event: TriggerEvent,
    pub when_clause: Option<Expr>,
    pub body: Vec<TriggerStep>,
    pub raw_sql: String,
}

/// Coarse DML event category used when matching candidate triggers (`TK_INSERT`, `TK_UPDATE`, `TK_DELETE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerEventKind {
    Insert,
    Update,
    Delete,
}

impl TriggerSchema {
    /// Returns `true` when this trigger is installed in the active trigger namespace (`!self.is_orphan`).
    #[inline]
    pub const fn is_active(&self) -> bool {
        !self.is_orphan
    }

    /// Returns `true` if `self.event` matches `event_kind` and (for `UPDATE OF ...`) at least one
    /// column in `columns` overlaps syntactically with `modified_cols` (`checkColumnOverlap` in
    /// `sqlite/src/trigger.c:787-794`).
    pub fn matches_event_and_columns(
        &self,
        event_kind: TriggerEventKind,
        modified_cols: Option<&[&str]>,
    ) -> bool {
        match (&self.event, event_kind) {
            (TriggerEvent::Insert, TriggerEventKind::Insert) => true,
            (TriggerEvent::Delete, TriggerEventKind::Delete) => true,
            (TriggerEvent::Update { columns }, TriggerEventKind::Update) => {
                let Some(of_cols) = columns.as_deref() else {
                    return true;
                };
                if of_cols.is_empty() {
                    return true;
                }
                let Some(changed) = modified_cols else {
                    return true;
                };
                of_cols.iter().any(|of_col| {
                    changed
                        .iter()
                        .any(|ch_col| of_col.eq_ignore_ascii_case(ch_col))
                })
            }
            _ => false,
        }
    }
}

/// Computes the effective `ON CONFLICT` policy for a DML step inside a trigger program
/// (`pParse->eOrconf = (orconf == OE_Default) ? pStep->orconf : orconf` in `sqlite/src/trigger.c:1143`).
#[inline]
pub const fn effective_trigger_step_conflict(
    outer_or_conflict: Option<ConflictAction>,
    step_or_conflict: Option<ConflictAction>,
) -> Option<ConflictAction> {
    match outer_or_conflict {
        Some(outer) => Some(outer),
        None => step_or_conflict,
    }
}

/// Outcome of validating a `CREATE [TEMP] TRIGGER` statement against [`DatabaseSet`]
/// (`sqlite3BeginTrigger` + `sqlite3FinishTrigger` in `sqlite/src/trigger.c`).
#[derive(Debug, Clone)]
pub enum CreateTriggerPlan {
    /// `IF NOT EXISTS` matched an existing active trigger in `trigger_db_id`.
    ExistingNoop,
    /// Validated trigger ready to be persisted into `trigger_db_id`.
    Create {
        trigger_db_id: DatabaseId,
        target_db_id: DatabaseId,
        target_schema_instance_id: SchemaInstanceId,
        target_table_name: String,
    },
}

#[inline]
pub(crate) fn is_reserved_trigger_or_system_name(name: &str) -> bool {
    name.len() >= 7 && name.as_bytes()[..7].eq_ignore_ascii_case(b"sqlite_")
}

/// Validates and resolves `CREATE [TEMP] TRIGGER` in the exact check order of
/// `sqlite3BeginTrigger`, `triggerStepAllocate`, and `sqlite3FinishTrigger` (`sqlite/src/trigger.c`).
pub fn resolve_and_validate_create_trigger(
    db_set: &DatabaseSet,
    ct: &CreateTriggerStmt,
    writable_schema: bool,
) -> Result<CreateTriggerPlan> {
    // 1. `if (isTemp && pName2->n > 0)` (`trigger.c:128-135`)
    if ct.temp && ct.name.schema.is_some() {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "temporary trigger may not have qualified name",
        ));
    }

    // 2. Determine initial trigger schema `trigger_db_id` (`trigger.c:136-171`)
    let mut trigger_db_id = if ct.temp {
        DatabaseId::TEMP
    } else {
        db_set.resolve_schema_or_err(ct.name.schema.as_deref())?
    };

    // Auto-`TEMP` placement: unqualified `CREATE TRIGGER tr ... ON tbl` where `tbl` resolves to `TEMP`
    // (`if (db->init.busy == 0 && pName2->n == 0 && pTab && pTab->pSchema == db->aDb[1].pSchema)`, `trigger.c:167-171`).
    if !ct.temp
        && ct.name.schema.is_none()
        && let Ok(Some((DatabaseId::TEMP, _))) = db_set.find_relation_in_set(&ct.on_table)
    {
        trigger_db_id = DatabaseId::TEMP;
    }

    // 3. `sqlite3FixSrcList(&sFix, pTableName)` + `sqlite3SrcListLookup` (`trigger.c:176-184`)
    let (target_db_id, target_rel) = if trigger_db_id != DatabaseId::TEMP {
        let slot = db_set
            .slot(trigger_db_id)
            .ok_or_else(|| SqliteError::misuse("trigger target database slot closed"))?;
        let slot_name = slot.name.as_str();
        if let Some(ref explicit_s) = ct.on_table.schema
            && !explicit_s.eq_ignore_ascii_case(slot_name)
        {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "trigger {} cannot reference objects in database {explicit_s}",
                    ct.name_token_text
                ),
            ));
        }
        let pinned_qn = QualifiedName::qualified(slot_name.to_owned(), ct.on_table.name.clone());
        match db_set.find_relation_in_set(&pinned_qn)? {
            Some(found) => found,
            None => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {slot_name}.{}", ct.on_table.name),
                ));
            }
        }
    } else {
        match db_set.find_relation_in_set(&ct.on_table)? {
            Some(found) => found,
            None => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {}", ct.on_table.display_name()),
                ));
            }
        }
    };

    // 4. `sqlite3CheckObjectName` (`trigger.c:201`)
    if !writable_schema && is_reserved_trigger_or_system_name(&ct.name.name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("object name reserved for internal use: {}", ct.name.name),
        ));
    }

    // 5. Separate trigger namespace collision check (`trigHash` in `trigger.c:206-215`)
    if let Some(slot) = db_set.slot(trigger_db_id)
        && slot.btree.is_some()
        && slot.schema.find_trigger(&ct.name.name).is_some()
    {
        if ct.if_not_exists {
            return Ok(CreateTriggerPlan::ExistingNoop);
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("trigger {} already exists", ct.name_token_text),
        ));
    }

    // 6. Disallow triggers on system tables (`sqlite_*`, `trigger.c:224-227`)
    let resolved_rel_name = match &target_rel {
        ResolvedRelation::Table(t) => t.name.as_str(),
        ResolvedRelation::View(v) => v.name.as_str(),
    };
    if is_reserved_trigger_or_system_name(resolved_rel_name)
        || is_reserved_trigger_or_system_name(&ct.on_table.name)
    {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "cannot create trigger on system table",
        ));
    }

    // 7. `INSTEAD OF` vs `BEFORE`/`AFTER` target kind check (`trigger.c:233-242`)
    // Note: on permanent triggers `sqlite3FixSrcList` clears `zDatabase` on `pTableName->a[0]`,
    // whereas on `TEMP` triggers `%S` preserves any explicit `<schema>.` qualifier!
    let target_disp = if trigger_db_id == DatabaseId::TEMP {
        ct.on_table.display_name()
    } else {
        ct.on_table.name.clone()
    };
    match (&target_rel, ct.timing) {
        (ResolvedRelation::View(_), TriggerTiming::Before) => {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("cannot create BEFORE trigger on view: {target_disp}"),
            ));
        }
        (ResolvedRelation::View(_), TriggerTiming::After) => {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("cannot create AFTER trigger on view: {target_disp}"),
            ));
        }
        (ResolvedRelation::Table(_), TriggerTiming::InsteadOf) => {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("cannot create INSTEAD OF trigger on table: {target_disp}"),
            ));
        }
        _ => {}
    }

    // 8. Disallow qualified DML target tables inside trigger bodies (`trigger.c:485-491`, `implementation_plan.md:661`)
    validate_trigger_body_dml_targets(&ct.body)?;

    // 9. `SQLITE_LIMIT_TRIGGER_STEPS` check (`trigger.c:347-349`)
    if ct.body.len() > db_set.trigger_steps_limit() {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!(
                "trigger \"{}\" contains too many steps",
                ct.name.name.replace('"', "\"\"")
            ),
        ));
    }

    // 10. `sqlite3FixTriggerStep` + `sqlite3FixExpr` (`trigger.c:352-357`, `attach.c:480-642`)
    if trigger_db_id != DatabaseId::TEMP {
        let slot_name = db_set
            .slot(trigger_db_id)
            .map_or("main", |s| s.name.as_str());
        let _ = fix_permanent_trigger(&ct.name.name, slot_name, ct.when_clause.as_ref(), &ct.body)?;
    } else {
        validate_trigger_no_parameters(ct.when_clause.as_ref(), &ct.body)?;
    }

    let target_schema_instance_id = db_set
        .slot(target_db_id)
        .map(|s| s.schema_instance_id)
        .ok_or_else(|| SqliteError::misuse("target database slot missing"))?;

    Ok(CreateTriggerPlan::Create {
        trigger_db_id,
        target_db_id,
        target_schema_instance_id,
        target_table_name: ct.on_table.name.clone(),
    })
}

/// Rejects schema-qualified DML targets (`INSERT INTO db.t`, `UPDATE db.t`, `DELETE FROM db.t`)
/// inside trigger bodies (`triggerStepAllocate` in `sqlite/src/trigger.c` and `implementation_plan.md:661`).
pub fn validate_trigger_body_dml_targets(body: &[TriggerStep]) -> Result<()> {
    for step in body {
        let has_schema = match step {
            TriggerStep::Select(_) => false,
            TriggerStep::Insert(ins) => ins.table.schema.is_some(),
            TriggerStep::Update(upd) => upd.table.schema.is_some(),
            TriggerStep::Delete(del) => del.table.schema.is_some(),
        };
        if has_schema {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "qualified table names are not allowed on INSERT, UPDATE, and DELETE statements within triggers",
            ));
        }
    }
    Ok(())
}

/// Rejects any bind parameter (`?`, `?NNN`, `:x`, `@x`, `$x`) in a trigger's `body` or `WHEN`
/// clause (`"trigger cannot use variables"`, `fixExprCb` in `sqlite/src/attach.c:487`).
pub fn validate_trigger_no_parameters(
    when_clause: Option<&Expr>,
    body: &[TriggerStep],
) -> Result<()> {
    for step in body {
        validate_step_no_params(step)?;
    }
    if let Some(w) = when_clause {
        validate_trig_expr_no_params(w)?;
    }
    Ok(())
}

fn validate_step_no_params(step: &TriggerStep) -> Result<()> {
    match step {
        TriggerStep::Select(sel) => validate_trig_select_no_params(sel),
        TriggerStep::Insert(ins) => {
            for cte in &ins.ctes {
                validate_trig_select_no_params(&cte.query)?;
            }
            match &ins.source {
                InsertSource::Values(rows) => {
                    for row in rows {
                        for e in row {
                            validate_trig_expr_no_params(e)?;
                        }
                    }
                }
                InsertSource::Select(sel) => validate_trig_select_no_params(sel)?,
                InsertSource::DefaultValues => {}
            }
            for uc in &ins.upsert_clauses {
                if let Some(ref tgt) = uc.target {
                    for ic in &tgt.columns {
                        if let Some(ref ie) = ic.expr {
                            validate_trig_expr_no_params(ie)?;
                        }
                    }
                    if let Some(ref tw) = tgt.where_clause {
                        validate_trig_expr_no_params(tw)?;
                    }
                }
                if let UpsertAction::DoUpdate {
                    ref assignments,
                    ref where_clause,
                } = uc.action
                {
                    for asgn in assignments {
                        validate_trig_expr_no_params(&asgn.expr)?;
                    }
                    if let Some(uw) = where_clause {
                        validate_trig_expr_no_params(uw)?;
                    }
                }
            }
            Ok(())
        }
        TriggerStep::Update(upd) => {
            for cte in &upd.ctes {
                validate_trig_select_no_params(&cte.query)?;
            }
            for asgn in &upd.assignments {
                validate_trig_expr_no_params(&asgn.expr)?;
            }
            if let Some(ref fc) = upd.from {
                validate_trig_from_no_params(fc)?;
            }
            if let Some(ref w) = upd.selection {
                validate_trig_expr_no_params(w)?;
            }
            Ok(())
        }
        TriggerStep::Delete(del) => {
            for cte in &del.ctes {
                validate_trig_select_no_params(&cte.query)?;
            }
            if let Some(ref w) = del.selection {
                validate_trig_expr_no_params(w)?;
            }
            Ok(())
        }
    }
}

fn validate_trig_select_no_params(select: &SelectStmt) -> Result<()> {
    for cte in &select.ctes {
        validate_trig_select_no_params(&cte.query)?;
    }
    validate_trig_core_no_params(&select.body.head)?;
    for (_, core) in &select.body.compounds {
        validate_trig_core_no_params(core)?;
    }
    for ob in &select.order_by {
        validate_trig_expr_no_params(&ob.expr)?;
    }
    if let Some(ref lim) = select.limit {
        validate_trig_expr_no_params(&lim.limit)?;
        if let Some(ref off) = lim.offset {
            validate_trig_expr_no_params(off)?;
        }
    }
    Ok(())
}

fn validate_trig_core_no_params(core: &SelectCore) -> Result<()> {
    match core {
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            ..
        } => {
            for rc in projection {
                if let ResultColumn::Expr { expr, .. } = rc {
                    validate_trig_expr_no_params(expr)?;
                }
            }
            if let Some(fc) = from {
                validate_trig_from_no_params(fc)?;
            }
            if let Some(w) = selection {
                validate_trig_expr_no_params(w)?;
            }
            for g in group_by {
                validate_trig_expr_no_params(g)?;
            }
            if let Some(h) = having {
                validate_trig_expr_no_params(h)?;
            }
        }
        SelectCore::Values(rows) => {
            for row in rows {
                for e in row {
                    validate_trig_expr_no_params(e)?;
                }
            }
        }
    }
    Ok(())
}

fn validate_trig_from_no_params(fc: &FromClause) -> Result<()> {
    validate_trig_tos_no_params(&fc.head)?;
    for j in &fc.joins {
        validate_trig_tos_no_params(&j.right)?;
        if let Some(JoinConstraint::On(ref on_e)) = j.constraint {
            validate_trig_expr_no_params(on_e)?;
        }
    }
    Ok(())
}

fn validate_trig_tos_no_params(tos: &TableOrSubquery) -> Result<()> {
    match tos {
        TableOrSubquery::Table { table_args, .. } => {
            if let Some(args) = table_args {
                for a in args {
                    validate_trig_expr_no_params(a)?;
                }
            }
            Ok(())
        }
        TableOrSubquery::Subquery { query, .. } => validate_trig_select_no_params(query),
    }
}

fn validate_trig_expr_no_params(expr: &Expr) -> Result<()> {
    match expr {
        Expr::Bind(_) => Err(SqliteError::new(
            ErrorCode::Error,
            "trigger cannot use variables",
        )),
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::OversizedHex(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Column(_) => Ok(()),
        Expr::Unary { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => validate_trig_expr_no_params(inner),
        Expr::Binary { left, right, .. } => {
            validate_trig_expr_no_params(left)?;
            validate_trig_expr_no_params(right)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            validate_trig_expr_no_params(e)?;
            validate_trig_expr_no_params(low)?;
            validate_trig_expr_no_params(high)
        }
        Expr::InList { expr: e, list, .. } => {
            validate_trig_expr_no_params(e)?;
            for item in list {
                validate_trig_expr_no_params(item)?;
            }
            Ok(())
        }
        Expr::InSubquery {
            expr: e, subquery, ..
        } => {
            validate_trig_expr_no_params(e)?;
            validate_trig_select_no_params(subquery)
        }
        Expr::InTable { expr: e, .. } => validate_trig_expr_no_params(e),
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            validate_trig_expr_no_params(e)?;
            validate_trig_expr_no_params(pattern)?;
            if let Some(esc) = escape {
                validate_trig_expr_no_params(esc)?;
            }
            Ok(())
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                validate_trig_expr_no_params(op)?;
            }
            for (c, v) in branches {
                validate_trig_expr_no_params(c)?;
                validate_trig_expr_no_params(v)?;
            }
            if let Some(el) = else_expr {
                validate_trig_expr_no_params(el)?;
            }
            Ok(())
        }
        Expr::FunctionCall { args, filter, .. } => {
            if let FunctionArgs::Exprs(es) = args {
                for e in es {
                    validate_trig_expr_no_params(e)?;
                }
            }
            if let Some(f) = filter {
                validate_trig_expr_no_params(f)?;
            }
            Ok(())
        }
        Expr::Exists { subquery, .. } | Expr::Subquery(subquery) => {
            validate_trig_select_no_params(subquery)
        }
        Expr::RowValue(es) => {
            for e in es {
                validate_trig_expr_no_params(e)?;
            }
            Ok(())
        }
        Expr::Raise { message, .. } => {
            if let Some(m) = message {
                validate_trig_expr_no_params(m)?;
            }
            Ok(())
        }
    }
}

/// Converts any embedded `Expr::Bind` parameter inside an existing catalog trigger into `Expr::Null`
/// during `Schema::load` (`if (pFix->pParse->db->init.busy) pExpr->op = TK_NULL;`, `attach.c:484-485`).
pub fn replace_bind_params_with_null_in_trigger(
    when_clause: &mut Option<Expr>,
    body: &mut [TriggerStep],
) {
    if let Some(w) = when_clause {
        replace_bind_in_expr(w);
    }
    for step in body {
        match step {
            TriggerStep::Select(sel) => replace_bind_in_select(sel),
            TriggerStep::Insert(ins) => {
                for cte in &mut ins.ctes {
                    replace_bind_in_select(&mut cte.query);
                }
                match &mut ins.source {
                    InsertSource::Values(rows) => {
                        for row in rows {
                            for e in row {
                                replace_bind_in_expr(e);
                            }
                        }
                    }
                    InsertSource::Select(sel) => replace_bind_in_select(sel),
                    InsertSource::DefaultValues => {}
                }
                for uc in &mut ins.upsert_clauses {
                    if let Some(ref mut tgt) = uc.target {
                        for ic in &mut tgt.columns {
                            if let Some(ref mut ie) = ic.expr {
                                replace_bind_in_expr(ie);
                            }
                        }
                        if let Some(ref mut tw) = tgt.where_clause {
                            replace_bind_in_expr(tw);
                        }
                    }
                    if let UpsertAction::DoUpdate {
                        ref mut assignments,
                        ref mut where_clause,
                    } = uc.action
                    {
                        for asgn in assignments {
                            replace_bind_in_expr(&mut asgn.expr);
                        }
                        if let Some(uw) = where_clause {
                            replace_bind_in_expr(uw);
                        }
                    }
                }
            }
            TriggerStep::Update(upd) => {
                for cte in &mut upd.ctes {
                    replace_bind_in_select(&mut cte.query);
                }
                for asgn in &mut upd.assignments {
                    replace_bind_in_expr(&mut asgn.expr);
                }
                if let Some(ref mut fc) = upd.from {
                    replace_bind_in_from(fc);
                }
                if let Some(ref mut w) = upd.selection {
                    replace_bind_in_expr(w);
                }
            }
            TriggerStep::Delete(del) => {
                for cte in &mut del.ctes {
                    replace_bind_in_select(&mut cte.query);
                }
                if let Some(ref mut w) = del.selection {
                    replace_bind_in_expr(w);
                }
            }
        }
    }
}

fn replace_bind_in_select(select: &mut SelectStmt) {
    for cte in &mut select.ctes {
        replace_bind_in_select(&mut cte.query);
    }
    replace_bind_in_core(&mut select.body.head);
    for (_, core) in &mut select.body.compounds {
        replace_bind_in_core(core);
    }
    for ob in &mut select.order_by {
        replace_bind_in_expr(&mut ob.expr);
    }
    if let Some(ref mut lim) = select.limit {
        replace_bind_in_expr(&mut lim.limit);
        if let Some(ref mut off) = lim.offset {
            replace_bind_in_expr(off);
        }
    }
}

fn replace_bind_in_core(core: &mut SelectCore) {
    match core {
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            ..
        } => {
            for rc in projection {
                if let ResultColumn::Expr { expr, .. } = rc {
                    replace_bind_in_expr(expr);
                }
            }
            if let Some(fc) = from {
                replace_bind_in_from(fc);
            }
            if let Some(w) = selection {
                replace_bind_in_expr(w);
            }
            for g in group_by {
                replace_bind_in_expr(g);
            }
            if let Some(h) = having {
                replace_bind_in_expr(h);
            }
        }
        SelectCore::Values(rows) => {
            for row in rows {
                for e in row {
                    replace_bind_in_expr(e);
                }
            }
        }
    }
}

fn replace_bind_in_from(fc: &mut FromClause) {
    replace_bind_in_tos(&mut fc.head);
    for j in &mut fc.joins {
        replace_bind_in_tos(&mut j.right);
        if let Some(JoinConstraint::On(ref mut on_e)) = j.constraint {
            replace_bind_in_expr(on_e);
        }
    }
}

fn replace_bind_in_tos(tos: &mut TableOrSubquery) {
    match tos {
        TableOrSubquery::Table { table_args, .. } => {
            if let Some(args) = table_args {
                for a in args {
                    replace_bind_in_expr(a);
                }
            }
        }
        TableOrSubquery::Subquery { query, .. } => replace_bind_in_select(query),
    }
}

fn replace_bind_in_expr(expr: &mut Expr) {
    match expr {
        Expr::Bind(_) => *expr = Expr::Null,
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::OversizedHex(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Column(_) => {}
        Expr::Unary { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => replace_bind_in_expr(inner),
        Expr::Binary { left, right, .. } => {
            replace_bind_in_expr(left);
            replace_bind_in_expr(right);
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            replace_bind_in_expr(e);
            replace_bind_in_expr(low);
            replace_bind_in_expr(high);
        }
        Expr::InList { expr: e, list, .. } => {
            replace_bind_in_expr(e);
            for item in list {
                replace_bind_in_expr(item);
            }
        }
        Expr::InSubquery {
            expr: e, subquery, ..
        } => {
            replace_bind_in_expr(e);
            replace_bind_in_select(subquery);
        }
        Expr::InTable { expr: e, .. } => replace_bind_in_expr(e),
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            replace_bind_in_expr(e);
            replace_bind_in_expr(pattern);
            if let Some(esc) = escape {
                replace_bind_in_expr(esc);
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                replace_bind_in_expr(op);
            }
            for (c, v) in branches {
                replace_bind_in_expr(c);
                replace_bind_in_expr(v);
            }
            if let Some(el) = else_expr {
                replace_bind_in_expr(el);
            }
        }
        Expr::FunctionCall { args, filter, .. } => {
            if let FunctionArgs::Exprs(es) = args {
                for e in es {
                    replace_bind_in_expr(e);
                }
            }
            if let Some(f) = filter {
                replace_bind_in_expr(f);
            }
        }
        Expr::Exists { subquery, .. } | Expr::Subquery(subquery) => {
            replace_bind_in_select(subquery);
        }
        Expr::RowValue(es) => {
            for e in es {
                replace_bind_in_expr(e);
            }
        }
        Expr::Raise { message, .. } => {
            if let Some(m) = message {
                replace_bind_in_expr(m);
            }
        }
    }
}

// ============================================================================
// Permanent Trigger `SchemaFixer` (`sqlite3FixInit` / `sqlite3FixTriggerStep` in `attach.c`)
// ============================================================================

/// Applies `SchemaFixer` (`sqlite3FixTriggerStep` + `sqlite3FixExpr`) to a permanent trigger:
/// - Rejects any bind parameter (`"trigger cannot use variables"`).
/// - Rejects any explicit reference to another database (`"trigger <name> cannot reference objects in database <other>"`).
/// - Pins all unqualified relation references in `WHEN` and `body` (`SELECT`, `FROM`, `CTE`s,
///   `IN table`, `UPDATE ... FROM`, and DML target tables) strictly to `db_schema_name` so `TEMP`
///   tables/views can never shadow objects referenced by a permanent trigger.
pub fn fix_permanent_trigger(
    trigger_name: &str,
    db_schema_name: &str,
    when_clause: Option<&Expr>,
    body: &[TriggerStep],
) -> Result<(Option<Expr>, Vec<TriggerStep>)> {
    let mut fixed_steps = Vec::with_capacity(body.len());
    for step in body {
        fixed_steps.push(fix_trig_step(trigger_name, db_schema_name, step)?);
    }
    let fixed_when = match when_clause {
        Some(w) => Some(fix_trig_expr(trigger_name, db_schema_name, w, &[])?),
        None => None,
    };
    Ok((fixed_when, fixed_steps))
}

fn check_allowed_trig_schema(
    trigger_name: &str,
    db_schema_name: &str,
    explicit: &str,
) -> Result<()> {
    if !explicit.eq_ignore_ascii_case(db_schema_name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("trigger {trigger_name} cannot reference objects in database {explicit}"),
        ));
    }
    Ok(())
}

fn fix_trig_qualified_relation(
    trigger_name: &str,
    db_schema_name: &str,
    qn: &QualifiedName,
    in_scope_ctes: &[String],
) -> Result<QualifiedName> {
    if let Some(ref s) = qn.schema {
        check_allowed_trig_schema(trigger_name, db_schema_name, s)?;
        Ok(QualifiedName::qualified(
            db_schema_name.to_owned(),
            qn.name.clone(),
        ))
    } else if in_scope_ctes
        .iter()
        .any(|cte_name| cte_name.eq_ignore_ascii_case(&qn.name))
    {
        Ok(qn.clone())
    } else {
        Ok(QualifiedName::qualified(
            db_schema_name.to_owned(),
            qn.name.clone(),
        ))
    }
}

fn fix_trig_step(
    trigger_name: &str,
    db_schema_name: &str,
    step: &TriggerStep,
) -> Result<TriggerStep> {
    match step {
        TriggerStep::Select(sel) => Ok(TriggerStep::Select(fix_trig_select(
            trigger_name,
            db_schema_name,
            sel,
            &[],
        )?)),
        TriggerStep::Insert(ins) => {
            let source = match &ins.source {
                InsertSource::Values(rows) => {
                    let mut fixed_rows = Vec::with_capacity(rows.len());
                    for row in rows {
                        let mut fixed_row = Vec::with_capacity(row.len());
                        for e in row {
                            fixed_row.push(fix_trig_expr(trigger_name, db_schema_name, e, &[])?);
                        }
                        fixed_rows.push(fixed_row);
                    }
                    InsertSource::Values(fixed_rows)
                }
                InsertSource::Select(sel) => InsertSource::Select(Box::new(fix_trig_select(
                    trigger_name,
                    db_schema_name,
                    sel,
                    &[],
                )?)),
                InsertSource::DefaultValues => InsertSource::DefaultValues,
            };
            let table = fix_trig_qualified_relation(trigger_name, db_schema_name, &ins.table, &[])?;
            let mut upsert_clauses = Vec::with_capacity(ins.upsert_clauses.len());
            for uc in &ins.upsert_clauses {
                let target = match &uc.target {
                    Some(tgt) => {
                        let mut cols = Vec::with_capacity(tgt.columns.len());
                        for ic in &tgt.columns {
                            let expr = match &ic.expr {
                                Some(ie) => {
                                    Some(fix_trig_expr(trigger_name, db_schema_name, ie, &[])?)
                                }
                                None => None,
                            };
                            cols.push(IndexedColumn {
                                name: ic.name.clone(),
                                expr,
                                collation: ic.collation.clone(),
                                desc: ic.desc,
                                nulls_last: ic.nulls_last,
                            });
                        }
                        let where_clause = match &tgt.where_clause {
                            Some(tw) => Some(fix_trig_expr(trigger_name, db_schema_name, tw, &[])?),
                            None => None,
                        };
                        Some(UpsertTarget {
                            columns: cols,
                            where_clause,
                        })
                    }
                    None => None,
                };
                let action = match &uc.action {
                    UpsertAction::DoNothing => UpsertAction::DoNothing,
                    UpsertAction::DoUpdate {
                        assignments,
                        where_clause,
                    } => {
                        let mut fixed_asgns = Vec::with_capacity(assignments.len());
                        for asgn in assignments {
                            fixed_asgns.push(SetAssignment {
                                columns: asgn.columns.clone(),
                                row_value: asgn.row_value,
                                expr: fix_trig_expr(trigger_name, db_schema_name, &asgn.expr, &[])?,
                            });
                        }
                        let fixed_where = match where_clause {
                            Some(uw) => Some(fix_trig_expr(trigger_name, db_schema_name, uw, &[])?),
                            None => None,
                        };
                        UpsertAction::DoUpdate {
                            assignments: fixed_asgns,
                            where_clause: fixed_where,
                        }
                    }
                };
                upsert_clauses.push(UpsertClause { target, action });
            }
            Ok(TriggerStep::Insert(InsertStmt {
                ctes: Vec::new(),
                or_conflict: ins.or_conflict,
                table,
                alias: ins.alias.clone(),
                columns: ins.columns.clone(),
                source,
                upsert_clauses,
                returning: Vec::new(),
            }))
        }
        TriggerStep::Update(upd) => {
            // Match order in `sqlite3FixTriggerStep`: pWhere -> pExprList -> pSrc (target + FROM)
            let selection = match &upd.selection {
                Some(w) => Some(fix_trig_expr(trigger_name, db_schema_name, w, &[])?),
                None => None,
            };
            let mut assignments = Vec::with_capacity(upd.assignments.len());
            for asgn in &upd.assignments {
                assignments.push(SetAssignment {
                    columns: asgn.columns.clone(),
                    row_value: asgn.row_value,
                    expr: fix_trig_expr(trigger_name, db_schema_name, &asgn.expr, &[])?,
                });
            }
            let table = fix_trig_qualified_relation(trigger_name, db_schema_name, &upd.table, &[])?;
            let from = match &upd.from {
                Some(fc) => Some(fix_trig_from(trigger_name, db_schema_name, fc, &[])?),
                None => None,
            };
            Ok(TriggerStep::Update(UpdateStmt {
                ctes: Vec::new(),
                or_conflict: upd.or_conflict,
                table,
                alias: upd.alias.clone(),
                indexed_by: None,
                assignments,
                from,
                selection,
                returning: Vec::new(),
            }))
        }
        TriggerStep::Delete(del) => {
            let selection = match &del.selection {
                Some(w) => Some(fix_trig_expr(trigger_name, db_schema_name, w, &[])?),
                None => None,
            };
            let table = fix_trig_qualified_relation(trigger_name, db_schema_name, &del.table, &[])?;
            Ok(TriggerStep::Delete(DeleteStmt {
                ctes: Vec::new(),
                table,
                alias: del.alias.clone(),
                indexed_by: None,
                selection,
                returning: Vec::new(),
            }))
        }
    }
}

fn fix_trig_select(
    trigger_name: &str,
    db_schema_name: &str,
    select: &SelectStmt,
    outer_ctes: &[String],
) -> Result<SelectStmt> {
    let mut full_scope: Vec<String> = outer_ctes.to_vec();
    for cte in &select.ctes {
        full_scope.push(cte.name.clone());
    }
    let mut fixed_ctes = Vec::with_capacity(select.ctes.len());
    for cte in &select.ctes {
        fixed_ctes.push(CommonTableExpr {
            name: cte.name.clone(),
            columns: cte.columns.clone(),
            materialized: cte.materialized,
            query: fix_trig_select(trigger_name, db_schema_name, &cte.query, &full_scope)?,
        });
    }
    let head = fix_trig_core(trigger_name, db_schema_name, &select.body.head, &full_scope)?;
    let mut compounds = Vec::with_capacity(select.body.compounds.len());
    for (op, core) in &select.body.compounds {
        compounds.push((
            *op,
            fix_trig_core(trigger_name, db_schema_name, core, &full_scope)?,
        ));
    }
    let mut order_by = Vec::with_capacity(select.order_by.len());
    for ob in &select.order_by {
        order_by.push(OrderByTerm {
            expr: fix_trig_expr(trigger_name, db_schema_name, &ob.expr, &full_scope)?,
            desc: ob.desc,
            nulls_last: ob.nulls_last,
        });
    }
    let limit = match &select.limit {
        Some(lim) => Some(LimitClause {
            limit: fix_trig_expr(trigger_name, db_schema_name, &lim.limit, &full_scope)?,
            offset: match &lim.offset {
                Some(off) => Some(fix_trig_expr(
                    trigger_name,
                    db_schema_name,
                    off,
                    &full_scope,
                )?),
                None => None,
            },
        }),
        None => None,
    };
    Ok(SelectStmt {
        ctes: fixed_ctes,
        body: SelectBody { head, compounds },
        order_by,
        limit,
    })
}

fn fix_trig_core(
    trigger_name: &str,
    db_schema_name: &str,
    core: &SelectCore,
    in_scope_ctes: &[String],
) -> Result<SelectCore> {
    match core {
        SelectCore::Select {
            distinct,
            projection,
            from,
            selection,
            group_by,
            having,
        } => {
            let fixed_from = match from {
                Some(fc) => Some(fix_trig_from(
                    trigger_name,
                    db_schema_name,
                    fc,
                    in_scope_ctes,
                )?),
                None => None,
            };
            let mut fixed_proj = Vec::with_capacity(projection.len());
            for rc in projection {
                match rc {
                    ResultColumn::Star => fixed_proj.push(ResultColumn::Star),
                    ResultColumn::TableStar { schema, table } => {
                        if let Some(s) = schema {
                            check_allowed_trig_schema(trigger_name, db_schema_name, s)?;
                        }
                        fixed_proj.push(ResultColumn::TableStar {
                            schema: schema.clone(),
                            table: table.clone(),
                        });
                    }
                    ResultColumn::Expr {
                        expr,
                        alias,
                        span_text,
                    } => {
                        fixed_proj.push(ResultColumn::Expr {
                            expr: fix_trig_expr(trigger_name, db_schema_name, expr, in_scope_ctes)?,
                            alias: alias.clone(),
                            span_text: span_text.clone(),
                        });
                    }
                }
            }
            let fixed_sel = match selection {
                Some(w) => Some(fix_trig_expr(
                    trigger_name,
                    db_schema_name,
                    w,
                    in_scope_ctes,
                )?),
                None => None,
            };
            let mut fixed_gb = Vec::with_capacity(group_by.len());
            for g in group_by {
                fixed_gb.push(fix_trig_expr(
                    trigger_name,
                    db_schema_name,
                    g,
                    in_scope_ctes,
                )?);
            }
            let fixed_hav = match having {
                Some(h) => Some(fix_trig_expr(
                    trigger_name,
                    db_schema_name,
                    h,
                    in_scope_ctes,
                )?),
                None => None,
            };
            Ok(SelectCore::Select {
                distinct: *distinct,
                projection: fixed_proj,
                from: fixed_from,
                selection: fixed_sel,
                group_by: fixed_gb,
                having: fixed_hav,
            })
        }
        SelectCore::Values(rows) => {
            let mut out = Vec::with_capacity(rows.len());
            for row in rows {
                let mut row_out = Vec::with_capacity(row.len());
                for e in row {
                    row_out.push(fix_trig_expr(
                        trigger_name,
                        db_schema_name,
                        e,
                        in_scope_ctes,
                    )?);
                }
                out.push(row_out);
            }
            Ok(SelectCore::Values(out))
        }
    }
}

fn fix_trig_from(
    trigger_name: &str,
    db_schema_name: &str,
    fc: &FromClause,
    in_scope_ctes: &[String],
) -> Result<FromClause> {
    let head = fix_trig_tos(trigger_name, db_schema_name, &fc.head, in_scope_ctes)?;
    let mut joins = Vec::with_capacity(fc.joins.len());
    for j in &fc.joins {
        let right = fix_trig_tos(trigger_name, db_schema_name, &j.right, in_scope_ctes)?;
        let constraint = match &j.constraint {
            Some(JoinConstraint::On(e)) => Some(JoinConstraint::On(fix_trig_expr(
                trigger_name,
                db_schema_name,
                e,
                in_scope_ctes,
            )?)),
            Some(JoinConstraint::Using(cols)) => Some(JoinConstraint::Using(cols.clone())),
            None => None,
        };
        joins.push(JoinClause {
            join_type: j.join_type,
            natural: j.natural,
            right,
            constraint,
        });
    }
    Ok(FromClause { head, joins })
}

fn fix_trig_tos(
    trigger_name: &str,
    db_schema_name: &str,
    tos: &TableOrSubquery,
    in_scope_ctes: &[String],
) -> Result<TableOrSubquery> {
    match tos {
        TableOrSubquery::Table {
            name,
            table_args,
            alias,
            indexed_by,
        } => {
            let fixed_name =
                fix_trig_qualified_relation(trigger_name, db_schema_name, name, in_scope_ctes)?;
            let fixed_args = match table_args {
                Some(args) => {
                    let mut out = Vec::with_capacity(args.len());
                    for e in args {
                        out.push(fix_trig_expr(
                            trigger_name,
                            db_schema_name,
                            e,
                            in_scope_ctes,
                        )?);
                    }
                    Some(out)
                }
                None => None,
            };
            Ok(TableOrSubquery::Table {
                name: fixed_name,
                table_args: fixed_args,
                alias: alias.clone(),
                indexed_by: indexed_by.clone(),
            })
        }
        TableOrSubquery::Subquery { query, alias } => Ok(TableOrSubquery::Subquery {
            query: Box::new(fix_trig_select(
                trigger_name,
                db_schema_name,
                query,
                in_scope_ctes,
            )?),
            alias: alias.clone(),
        }),
    }
}

fn fix_trig_expr(
    trigger_name: &str,
    db_schema_name: &str,
    expr: &Expr,
    in_scope_ctes: &[String],
) -> Result<Expr> {
    match expr {
        Expr::Bind(_) => Err(SqliteError::new(
            ErrorCode::Error,
            "trigger cannot use variables",
        )),
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::OversizedHex(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::CurrentTimeKw(_) => Ok(expr.clone()),
        Expr::Column(ColumnRef {
            schema,
            table,
            column,
        }) => {
            if let Some(s) = schema {
                check_allowed_trig_schema(trigger_name, db_schema_name, s)?;
            }
            Ok(Expr::Column(ColumnRef {
                schema: schema.clone(),
                table: table.clone(),
                column: column.clone(),
            }))
        }
        Expr::Unary { op, expr: inner } => Ok(Expr::Unary {
            op: *op,
            expr: Box::new(fix_trig_expr(
                trigger_name,
                db_schema_name,
                inner,
                in_scope_ctes,
            )?),
        }),
        Expr::Binary { left, op, right } => Ok(Expr::Binary {
            left: Box::new(fix_trig_expr(
                trigger_name,
                db_schema_name,
                left,
                in_scope_ctes,
            )?),
            op: *op,
            right: Box::new(fix_trig_expr(
                trigger_name,
                db_schema_name,
                right,
                in_scope_ctes,
            )?),
        }),
        Expr::Between {
            expr: e,
            low,
            high,
            negated,
        } => Ok(Expr::Between {
            expr: Box::new(fix_trig_expr(
                trigger_name,
                db_schema_name,
                e,
                in_scope_ctes,
            )?),
            low: Box::new(fix_trig_expr(
                trigger_name,
                db_schema_name,
                low,
                in_scope_ctes,
            )?),
            high: Box::new(fix_trig_expr(
                trigger_name,
                db_schema_name,
                high,
                in_scope_ctes,
            )?),
            negated: *negated,
        }),
        Expr::InList {
            expr: e,
            list,
            negated,
        } => {
            let fixed_e = fix_trig_expr(trigger_name, db_schema_name, e, in_scope_ctes)?;
            let mut fixed_list = Vec::with_capacity(list.len());
            for item in list {
                fixed_list.push(fix_trig_expr(
                    trigger_name,
                    db_schema_name,
                    item,
                    in_scope_ctes,
                )?);
            }
            Ok(Expr::InList {
                expr: Box::new(fixed_e),
                list: fixed_list,
                negated: *negated,
            })
        }
        Expr::InSubquery {
            expr: e,
            subquery,
            negated,
        } => Ok(Expr::InSubquery {
            expr: Box::new(fix_trig_expr(
                trigger_name,
                db_schema_name,
                e,
                in_scope_ctes,
            )?),
            subquery: Box::new(fix_trig_select(
                trigger_name,
                db_schema_name,
                subquery,
                in_scope_ctes,
            )?),
            negated: *negated,
        }),
        Expr::InTable {
            expr: e,
            table,
            negated,
        } => Ok(Expr::InTable {
            expr: Box::new(fix_trig_expr(
                trigger_name,
                db_schema_name,
                e,
                in_scope_ctes,
            )?),
            table: fix_trig_qualified_relation(trigger_name, db_schema_name, table, in_scope_ctes)?,
            negated: *negated,
        }),
        Expr::Like {
            expr: e,
            pattern,
            escape,
            op,
            negated,
        } => Ok(Expr::Like {
            expr: Box::new(fix_trig_expr(
                trigger_name,
                db_schema_name,
                e,
                in_scope_ctes,
            )?),
            pattern: Box::new(fix_trig_expr(
                trigger_name,
                db_schema_name,
                pattern,
                in_scope_ctes,
            )?),
            escape: match escape {
                Some(esc) => Some(Box::new(fix_trig_expr(
                    trigger_name,
                    db_schema_name,
                    esc,
                    in_scope_ctes,
                )?)),
                None => None,
            },
            op: *op,
            negated: *negated,
        }),
        Expr::IsNull {
            expr: inner,
            negated,
        } => Ok(Expr::IsNull {
            expr: Box::new(fix_trig_expr(
                trigger_name,
                db_schema_name,
                inner,
                in_scope_ctes,
            )?),
            negated: *negated,
        }),
        Expr::Collate {
            expr: inner,
            collation,
        } => Ok(Expr::Collate {
            expr: Box::new(fix_trig_expr(
                trigger_name,
                db_schema_name,
                inner,
                in_scope_ctes,
            )?),
            collation: collation.clone(),
        }),
        Expr::Cast {
            expr: inner,
            type_name,
        } => Ok(Expr::Cast {
            expr: Box::new(fix_trig_expr(
                trigger_name,
                db_schema_name,
                inner,
                in_scope_ctes,
            )?),
            type_name: type_name.clone(),
        }),
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            let fixed_op = match operand {
                Some(op) => Some(Box::new(fix_trig_expr(
                    trigger_name,
                    db_schema_name,
                    op,
                    in_scope_ctes,
                )?)),
                None => None,
            };
            let mut fixed_branches = Vec::with_capacity(branches.len());
            for (c, v) in branches {
                fixed_branches.push((
                    fix_trig_expr(trigger_name, db_schema_name, c, in_scope_ctes)?,
                    fix_trig_expr(trigger_name, db_schema_name, v, in_scope_ctes)?,
                ));
            }
            let fixed_else = match else_expr {
                Some(el) => Some(Box::new(fix_trig_expr(
                    trigger_name,
                    db_schema_name,
                    el,
                    in_scope_ctes,
                )?)),
                None => None,
            };
            Ok(Expr::Case {
                operand: fixed_op,
                branches: fixed_branches,
                else_expr: fixed_else,
            })
        }
        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
        } => {
            let fixed_args = match args {
                FunctionArgs::Star => FunctionArgs::Star,
                FunctionArgs::Exprs(es) => {
                    let mut out = Vec::with_capacity(es.len());
                    for e in es {
                        out.push(fix_trig_expr(
                            trigger_name,
                            db_schema_name,
                            e,
                            in_scope_ctes,
                        )?);
                    }
                    FunctionArgs::Exprs(out)
                }
            };
            let fixed_filter = match filter {
                Some(f) => Some(Box::new(fix_trig_expr(
                    trigger_name,
                    db_schema_name,
                    f,
                    in_scope_ctes,
                )?)),
                None => None,
            };
            Ok(Expr::FunctionCall {
                name: name.clone(),
                distinct: *distinct,
                args: fixed_args,
                filter: fixed_filter,
            })
        }
        Expr::Exists { subquery, negated } => Ok(Expr::Exists {
            subquery: Box::new(fix_trig_select(
                trigger_name,
                db_schema_name,
                subquery,
                in_scope_ctes,
            )?),
            negated: *negated,
        }),
        Expr::Subquery(subquery) => Ok(Expr::Subquery(Box::new(fix_trig_select(
            trigger_name,
            db_schema_name,
            subquery,
            in_scope_ctes,
        )?))),
        Expr::RowValue(es) => {
            let mut out = Vec::with_capacity(es.len());
            for e in es {
                out.push(fix_trig_expr(
                    trigger_name,
                    db_schema_name,
                    e,
                    in_scope_ctes,
                )?);
            }
            Ok(Expr::RowValue(out))
        }
        Expr::Raise { action, message } => Ok(Expr::Raise {
            action: *action,
            message: match message {
                Some(m) => Some(Box::new(fix_trig_expr(
                    trigger_name,
                    db_schema_name,
                    m,
                    in_scope_ctes,
                )?)),
                None => None,
            },
        }),
    }
}

// ============================================================================
// `RAISE(...)` Outside-Trigger Guards (`TK_RAISE` in `sqlite/src/expr.c:5872`)
// ============================================================================

/// Returns `Err("RAISE() may only be used within a trigger-program")` if `expr` contains `Expr::Raise`.
pub fn reject_raise_outside_trigger_expr(expr: &Expr) -> Result<()> {
    if expr_contains_raise(expr) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "RAISE() may only be used within a trigger-program",
        ));
    }
    Ok(())
}

/// Returns `Err("RAISE() may only be used within a trigger-program")` if `select` contains `Expr::Raise`.
pub fn reject_raise_outside_trigger_select(select: &SelectStmt) -> Result<()> {
    if select_contains_raise(select) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "RAISE() may only be used within a trigger-program",
        ));
    }
    Ok(())
}

/// Returns `true` if `select` or any nested CTE/subquery/expression contains `Expr::Raise`.
pub fn select_contains_raise(select: &SelectStmt) -> bool {
    select
        .ctes
        .iter()
        .any(|cte| select_contains_raise(&cte.query))
        || core_contains_raise(&select.body.head)
        || select
            .body
            .compounds
            .iter()
            .any(|(_, c)| core_contains_raise(c))
        || select
            .order_by
            .iter()
            .any(|ob| expr_contains_raise(&ob.expr))
        || select.limit.as_ref().is_some_and(|lim| {
            expr_contains_raise(&lim.limit) || lim.offset.as_ref().is_some_and(expr_contains_raise)
        })
}

fn core_contains_raise(core: &SelectCore) -> bool {
    match core {
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            ..
        } => {
            projection.iter().any(
                |rc| matches!(rc, ResultColumn::Expr { expr, .. } if expr_contains_raise(expr)),
            ) || from.as_ref().is_some_and(from_contains_raise)
                || selection.as_ref().is_some_and(expr_contains_raise)
                || group_by.iter().any(expr_contains_raise)
                || having.as_ref().is_some_and(expr_contains_raise)
        }
        SelectCore::Values(rows) => rows.iter().any(|r| r.iter().any(expr_contains_raise)),
    }
}

pub(crate) fn from_contains_raise(fc: &FromClause) -> bool {
    tos_contains_raise(&fc.head)
        || fc.joins.iter().any(|j| {
            tos_contains_raise(&j.right)
                || matches!(
                    &j.constraint,
                    Some(JoinConstraint::On(e)) if expr_contains_raise(e)
                )
        })
}

fn tos_contains_raise(tos: &TableOrSubquery) -> bool {
    match tos {
        TableOrSubquery::Table { table_args, .. } => table_args
            .as_ref()
            .is_some_and(|args| args.iter().any(expr_contains_raise)),
        TableOrSubquery::Subquery { query, .. } => select_contains_raise(query),
    }
}

/// Returns `true` if `expr` or any nested subexpression/subquery contains `Expr::Raise`.
pub fn expr_contains_raise(expr: &Expr) -> bool {
    match expr {
        Expr::Raise { .. } => true,
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::OversizedHex(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Bind(_)
        | Expr::Column(_) => false,
        Expr::Unary { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => expr_contains_raise(inner),
        Expr::Binary { left, right, .. } => expr_contains_raise(left) || expr_contains_raise(right),
        Expr::Between {
            expr: e, low, high, ..
        } => expr_contains_raise(e) || expr_contains_raise(low) || expr_contains_raise(high),
        Expr::InList { expr: e, list, .. } => {
            expr_contains_raise(e) || list.iter().any(expr_contains_raise)
        }
        Expr::InSubquery {
            expr: e, subquery, ..
        } => expr_contains_raise(e) || select_contains_raise(subquery),
        Expr::InTable { expr: e, .. } => expr_contains_raise(e),
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            expr_contains_raise(e)
                || expr_contains_raise(pattern)
                || escape.as_deref().is_some_and(expr_contains_raise)
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand.as_deref().is_some_and(expr_contains_raise)
                || branches
                    .iter()
                    .any(|(c, v)| expr_contains_raise(c) || expr_contains_raise(v))
                || else_expr.as_deref().is_some_and(expr_contains_raise)
        }
        Expr::FunctionCall { args, filter, .. } => {
            matches!(args, FunctionArgs::Exprs(es) if es.iter().any(expr_contains_raise))
                || filter.as_deref().is_some_and(expr_contains_raise)
        }
        Expr::Exists { subquery, .. } | Expr::Subquery(subquery) => select_contains_raise(subquery),
        Expr::RowValue(es) => es.iter().any(expr_contains_raise),
    }
}

/// Checks whether a parsed [`TriggerSchema`] contains any deferred oversized hex integer literal
/// (`codeInteger` in `sqlite/src/expr.c:4448-4450`), failing at DML prepare/compile time.
pub fn validate_trigger_no_oversized_hex(trig: &TriggerSchema) -> Result<()> {
    if let Some(ref w) = trig.when_clause {
        check_expr_no_oversized_hex(w)?;
    }
    for step in &trig.body {
        match step {
            TriggerStep::Select(sel) => check_select_no_oversized_hex(sel)?,
            TriggerStep::Insert(ins) => {
                for cte in &ins.ctes {
                    check_select_no_oversized_hex(&cte.query)?;
                }
                match &ins.source {
                    InsertSource::DefaultValues => {}
                    InsertSource::Values(rows) => {
                        for row in rows {
                            for e in row {
                                check_expr_no_oversized_hex(e)?;
                            }
                        }
                    }
                    InsertSource::Select(sel) => check_select_no_oversized_hex(sel)?,
                }
                for uc in &ins.upsert_clauses {
                    if let Some(ref ut) = uc.target
                        && let Some(ref tw) = ut.where_clause
                    {
                        check_expr_no_oversized_hex(tw)?;
                    }
                    if let UpsertAction::DoUpdate {
                        ref assignments,
                        ref where_clause,
                    } = uc.action
                    {
                        for sa in assignments {
                            check_expr_no_oversized_hex(&sa.expr)?;
                        }
                        if let Some(dw) = where_clause {
                            check_expr_no_oversized_hex(dw)?;
                        }
                    }
                }
                for rc in &ins.returning {
                    if let ResultColumn::Expr { expr, .. } = rc {
                        check_expr_no_oversized_hex(expr)?;
                    }
                }
            }
            TriggerStep::Update(upd) => {
                for cte in &upd.ctes {
                    check_select_no_oversized_hex(&cte.query)?;
                }
                for sa in &upd.assignments {
                    check_expr_no_oversized_hex(&sa.expr)?;
                }
                if let Some(ref fc) = upd.from {
                    check_from_no_oversized_hex(fc)?;
                }
                if let Some(ref w) = upd.selection {
                    check_expr_no_oversized_hex(w)?;
                }
                for rc in &upd.returning {
                    if let ResultColumn::Expr { expr, .. } = rc {
                        check_expr_no_oversized_hex(expr)?;
                    }
                }
            }
            TriggerStep::Delete(del) => {
                for cte in &del.ctes {
                    check_select_no_oversized_hex(&cte.query)?;
                }
                if let Some(ref w) = del.selection {
                    check_expr_no_oversized_hex(w)?;
                }
                for rc in &del.returning {
                    if let ResultColumn::Expr { expr, .. } = rc {
                        check_expr_no_oversized_hex(expr)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn check_select_no_oversized_hex(sel: &SelectStmt) -> Result<()> {
    for cte in &sel.ctes {
        check_select_no_oversized_hex(&cte.query)?;
    }
    check_select_core_no_oversized_hex(&sel.body.head)?;
    for (_, rhs) in &sel.body.compounds {
        check_select_core_no_oversized_hex(rhs)?;
    }
    for ob in &sel.order_by {
        check_expr_no_oversized_hex(&ob.expr)?;
    }
    if let Some(ref lim) = sel.limit {
        check_expr_no_oversized_hex(&lim.limit)?;
        if let Some(ref off) = lim.offset {
            check_expr_no_oversized_hex(off)?;
        }
    }
    Ok(())
}

fn check_select_core_no_oversized_hex(core: &SelectCore) -> Result<()> {
    match core {
        SelectCore::Values(rows) => {
            for row in rows {
                for e in row {
                    check_expr_no_oversized_hex(e)?;
                }
            }
        }
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            ..
        } => {
            for rc in projection {
                if let ResultColumn::Expr { expr, .. } = rc {
                    check_expr_no_oversized_hex(expr)?;
                }
            }
            if let Some(fc) = from {
                check_from_no_oversized_hex(fc)?;
            }
            if let Some(w) = selection {
                check_expr_no_oversized_hex(w)?;
            }
            for g in group_by {
                check_expr_no_oversized_hex(g)?;
            }
            if let Some(h) = having {
                check_expr_no_oversized_hex(h)?;
            }
        }
    }
    Ok(())
}

fn check_from_no_oversized_hex(fc: &FromClause) -> Result<()> {
    check_tos_no_oversized_hex(&fc.head)?;
    for jc in &fc.joins {
        check_tos_no_oversized_hex(&jc.right)?;
        if let Some(JoinConstraint::On(ref e)) = jc.constraint {
            check_expr_no_oversized_hex(e)?;
        }
    }
    Ok(())
}

fn check_tos_no_oversized_hex(tos: &TableOrSubquery) -> Result<()> {
    match tos {
        TableOrSubquery::Table { table_args, .. } => {
            if let Some(args) = table_args {
                for a in args {
                    check_expr_no_oversized_hex(a)?;
                }
            }
        }
        TableOrSubquery::Subquery { query, .. } => check_select_no_oversized_hex(query)?,
    }
    Ok(())
}

fn check_expr_no_oversized_hex(expr: &Expr) -> Result<()> {
    match expr {
        Expr::OversizedHex(raw) => Err(SqliteError::new(
            ErrorCode::Error,
            format!("hex literal too big: {raw}"),
        )),
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Bind(_)
        | Expr::Column(_) => Ok(()),
        Expr::Unary { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => check_expr_no_oversized_hex(inner),
        Expr::Binary { left, right, .. } => {
            check_expr_no_oversized_hex(left)?;
            check_expr_no_oversized_hex(right)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            check_expr_no_oversized_hex(e)?;
            check_expr_no_oversized_hex(low)?;
            check_expr_no_oversized_hex(high)
        }
        Expr::InList { expr: e, list, .. } => {
            check_expr_no_oversized_hex(e)?;
            for item in list {
                check_expr_no_oversized_hex(item)?;
            }
            Ok(())
        }
        Expr::InSubquery {
            expr: e, subquery, ..
        } => {
            check_expr_no_oversized_hex(e)?;
            check_select_no_oversized_hex(subquery)
        }
        Expr::InTable { expr: e, .. } => check_expr_no_oversized_hex(e),
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            check_expr_no_oversized_hex(e)?;
            check_expr_no_oversized_hex(pattern)?;
            if let Some(esc) = escape {
                check_expr_no_oversized_hex(esc)?;
            }
            Ok(())
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                check_expr_no_oversized_hex(op)?;
            }
            for (c, v) in branches {
                check_expr_no_oversized_hex(c)?;
                check_expr_no_oversized_hex(v)?;
            }
            if let Some(el) = else_expr {
                check_expr_no_oversized_hex(el)?;
            }
            Ok(())
        }
        Expr::FunctionCall { args, filter, .. } => {
            if let FunctionArgs::Exprs(es) = args {
                for e in es {
                    check_expr_no_oversized_hex(e)?;
                }
            }
            if let Some(f) = filter {
                check_expr_no_oversized_hex(f)?;
            }
            Ok(())
        }
        Expr::Exists { subquery, .. } | Expr::Subquery(subquery) => {
            check_select_no_oversized_hex(subquery)
        }
        Expr::Raise { message, .. } => {
            if let Some(m) = message {
                check_expr_no_oversized_hex(m)?;
            }
            Ok(())
        }
        Expr::RowValue(es) => {
            for e in es {
                check_expr_no_oversized_hex(e)?;
            }
            Ok(())
        }
    }
}
