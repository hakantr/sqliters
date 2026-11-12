//! `json_each`, `json_tree`, `jsonb_each`, and `jsonb_tree` eponymous virtual table modules (`sqlite/src/json.c`).
#![forbid(unsafe_code)]

use sqliters_types::json::{JsonWalkRow, build_json_walk_rows_into};
use sqliters_types::{
    ErrorCode, Result, SqliteError, VTabConfigOption, VTabConstraintOp, VTabHostContext,
    VTabIndexInfo, VTabRiskFlags, Value, VirtualCursor, VirtualTable, VirtualTableModule,
};

const JSON_EACH_SCHEMA: &str =
    "CREATE TABLE x(key,value,type,atom,id,parent,fullkey,path,json HIDDEN,root HIDDEN)";

const JEACH_JSON: i32 = 8;
const JEACH_ROOT: i32 = 9;

const IDX_HAS_JSON: i32 = 0x01;
const IDX_HAS_ROOT: i32 = 0x02;

/// Built-in `json_each(json [, root])` eponymous-only virtual table module.
#[derive(Debug, Default, Clone, Copy)]
pub struct JsonEachModule;

/// Built-in `json_tree(json [, root])` eponymous-only virtual table module.
#[derive(Debug, Default, Clone, Copy)]
pub struct JsonTreeModule;

/// Built-in `jsonb_each(json [, root])` eponymous-only virtual table module.
#[derive(Debug, Default, Clone, Copy)]
pub struct JsonbEachModule;

/// Built-in `jsonb_tree(json [, root])` eponymous-only virtual table module.
#[derive(Debug, Default, Clone, Copy)]
pub struct JsonbTreeModule;

macro_rules! impl_json_walk_module {
    ($ty:ty, $name:literal, $is_tree:expr, $as_blob:expr) => {
        impl VirtualTableModule for $ty {
            fn name(&self) -> &str {
                $name
            }

            fn risk_flags(&self) -> VTabRiskFlags {
                VTabRiskFlags::Innocuous
            }

            fn is_eponymous(&self) -> bool {
                true
            }

            fn is_eponymous_only(&self) -> bool {
                true
            }

            fn create(
                &self,
                _ctx: &mut dyn VTabHostContext,
                _args: &[String],
            ) -> Result<Box<dyn VirtualTable>> {
                Err(SqliteError::new(
                    ErrorCode::Error,
                    concat!("no such module: ", $name),
                ))
            }

            fn connect(
                &self,
                ctx: &mut dyn VTabHostContext,
                _args: &[String],
            ) -> Result<Box<dyn VirtualTable>> {
                ctx.declare_vtab(JSON_EACH_SCHEMA)?;
                let _ = ctx.set_vtab_config(VTabConfigOption::Innocuous);
                Ok(Box::new(JsonWalkTable {
                    is_tree: $is_tree,
                    as_blob_containers: $as_blob,
                }))
            }
        }
    };
}

impl_json_walk_module!(JsonEachModule, "json_each", false, false);
impl_json_walk_module!(JsonTreeModule, "json_tree", true, false);
impl_json_walk_module!(JsonbEachModule, "jsonb_each", false, true);
impl_json_walk_module!(JsonbTreeModule, "jsonb_tree", true, true);

#[derive(Debug, Clone, Copy)]
struct JsonWalkTable {
    is_tree: bool,
    as_blob_containers: bool,
}

impl VirtualTable for JsonWalkTable {
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        let mut json_idx: Option<usize> = None;
        let mut root_idx: Option<usize> = None;
        let mut unusable_mask = 0i32;

        for (i, c) in info.constraints.iter().enumerate() {
            if c.op != VTabConstraintOp::Eq {
                continue;
            }
            match c.i_column {
                JEACH_JSON => {
                    if c.usable {
                        json_idx = Some(i);
                    } else {
                        unusable_mask |= IDX_HAS_JSON;
                    }
                }
                JEACH_ROOT => {
                    if c.usable {
                        root_idx = Some(i);
                    } else {
                        unusable_mask |= IDX_HAS_ROOT;
                    }
                }
                _ => {}
            }
        }

        let mut idx_num = 0i32;
        let mut argv_pos = 0usize;
        if let Some(ji) = json_idx {
            argv_pos += 1;
            info.constraint_usage[ji].argv_index = argv_pos;
            info.constraint_usage[ji].omit = true;
            idx_num |= IDX_HAS_JSON;
        }
        if let Some(ri) = root_idx {
            argv_pos += 1;
            info.constraint_usage[ri].argv_index = argv_pos;
            info.constraint_usage[ri].omit = true;
            idx_num |= IDX_HAS_ROOT;
        }

        if (unusable_mask & !idx_num) != 0 {
            return Err(SqliteError::new(
                ErrorCode::Constraint,
                "unusable json_each/json_tree constraint",
            ));
        }

        if (idx_num & IDX_HAS_JSON) == 0 {
            info.estimated_cost = 1.0e99;
            info.estimated_rows = 2_147_483_647;
        } else {
            info.estimated_cost = 1.0;
            info.estimated_rows = 25;
            if info.order_by.len() == 1 && info.order_by[0].i_column == -1 && !info.order_by[0].desc
            {
                info.order_by_consumed = true;
            }
        }
        info.idx_num = idx_num;
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(JsonWalkCursor {
            is_tree: self.is_tree,
            as_blob_containers: self.as_blob_containers,
            rows: Vec::new(),
            scratch_jsonb: Vec::new(),
            pos: 0,
            raw_json: Value::Null,
            raw_root: Value::Null,
        }))
    }
}

#[derive(Debug)]
struct JsonWalkCursor {
    is_tree: bool,
    as_blob_containers: bool,
    rows: Vec<JsonWalkRow>,
    scratch_jsonb: Vec<u8>,
    pos: usize,
    raw_json: Value,
    raw_root: Value,
}

impl VirtualCursor for JsonWalkCursor {
    fn filter(
        &mut self,
        _ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        _idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
        self.rows.clear();
        self.pos = 0;
        self.raw_json = Value::Null;
        self.raw_root = Value::Null;

        if (idx_num & IDX_HAS_JSON) == 0 {
            return Ok(());
        }

        let json_val = args.first().cloned().unwrap_or(Value::Null);
        let root_opt = if (idx_num & IDX_HAS_ROOT) != 0 {
            args.get(1).cloned()
        } else {
            None
        };
        self.raw_json = json_val.clone();
        self.raw_root = match &root_opt {
            Some(v) => v.clone(),
            None => Value::Text("$".to_owned()),
        };

        build_json_walk_rows_into(
            &json_val,
            root_opt.as_ref(),
            self.is_tree,
            self.as_blob_containers,
            &mut self.scratch_jsonb,
            &mut self.rows,
        )?;
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        if self.pos < self.rows.len() {
            self.pos += 1;
        }
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.rows.len()
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        let Some(row) = self.rows.get(self.pos) else {
            return Ok(Value::Null);
        };
        let val = match col_idx {
            0 => row.key.clone(),
            1 => row.value.value.clone(),
            2 => Value::Text(row.type_name.to_owned()),
            3 => row.atom.clone(),
            4 => Value::Integer(row.id),
            5 => row.parent.clone(),
            6 => Value::Text(row.fullkey.clone()),
            7 => Value::Text(row.path.clone()),
            8 => self.raw_json.clone(),
            9 => self.raw_root.clone(),
            _ => Value::Null,
        };
        Ok(val)
    }

    fn column_subtype(&self, col_idx: usize) -> u8 {
        if col_idx == 1
            && let Some(row) = self.rows.get(self.pos)
        {
            return row.value.subtype;
        }
        0
    }

    fn rowid(&self) -> Result<i64> {
        Ok(self.pos as i64)
    }
}
