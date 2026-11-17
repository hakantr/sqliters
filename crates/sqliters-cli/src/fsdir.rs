//! POSIX fsdir table in the SQLite shell fileio extension (ext/misc/fileio.c).
use std::fs::{self, File, Metadata, ReadDir};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::sync::{
    Arc,
    atomic::{AtomicI32, Ordering},
};

use sqliters::{
    ErrorCode, Result, SqliteError, VTabConstraintOp, VTabHostContext, VTabIndexInfo,
    VTabRiskFlags, Value, VirtualCursor, VirtualTable, VirtualTableModule,
};

pub struct FsDirModule {
    max_length: Arc<AtomicI32>,
}

impl FsDirModule {
    pub fn new(max_length: Arc<AtomicI32>) -> Self {
        Self { max_length }
    }
}

impl VirtualTableModule for FsDirModule {
    fn name(&self) -> &str {
        "fsdir"
    }

    fn risk_flags(&self) -> VTabRiskFlags {
        VTabRiskFlags::DirectOnly
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
        Err(SqliteError::new(ErrorCode::Error, "no such module: fsdir"))
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        _args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        ctx.declare_vtab(
            "CREATE TABLE x(name, mode, mtime, data, level, path HIDDEN, dir HIDDEN)",
        )?;
        Ok(Box::new(FsDirTable(Arc::clone(&self.max_length))))
    }
}

struct FsDirTable(Arc<AtomicI32>);

impl VirtualTable for FsDirTable {
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        use VTabConstraintOp::{Eq, Le, Lt};
        let mut path_and_dir = [None, None];
        let mut blocked = [false, false];
        let mut level = None;
        for (i, c) in info.constraints.iter().enumerate() {
            match (c.i_column, c.op) {
                (5 | 6, Eq) => {
                    let slot = (c.i_column - 5) as usize;
                    if c.usable {
                        path_and_dir[slot] = Some(i);
                        blocked[slot] = false;
                    } else if path_and_dir[slot].is_none() {
                        blocked[slot] = true;
                    }
                }
                (4, Eq | Le | Lt) if c.usable && level.is_none() => {
                    level = Some((i, if c.op == Lt { 4 } else { 8 }, c.op != Eq));
                }
                _ => {}
            }
        }
        if blocked.into_iter().any(|value| value) {
            return Err(SqliteError::new(
                ErrorCode::Constraint,
                "unusable fsdir constraint",
            ));
        }
        let [Some(path), dir] = path_and_dir else {
            info.idx_num = 0;
            info.estimated_rows = i64::from(i32::MAX);
            return Ok(());
        };
        info.constraint_usage[path].argv_index = 1;
        info.constraint_usage[path].omit = true;
        info.idx_num = 1;
        info.estimated_cost = 1.0e9;
        let mut argc = 1;
        if let Some(dir) = dir {
            argc += 1;
            info.constraint_usage[dir].argv_index = argc;
            info.constraint_usage[dir].omit = true;
            info.idx_num |= 2;
            info.estimated_cost /= 1.0e4;
        }
        if let Some((level, bit, omit)) = level {
            info.constraint_usage[level].argv_index = argc + 1;
            info.constraint_usage[level].omit = omit;
            info.idx_num |= bit;
            info.estimated_cost /= 1.0e4;
        }
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(FsDirCursor {
            max_length: Arc::clone(&self.0),
            current: None,
            levels: Vec::new(),
            base: 0,
            max_level: 0,
            rowid: 0,
        }))
    }
}

struct FileEntry {
    path: String,
    stat: Metadata,
}

impl FileEntry {
    fn open(path: String) -> Result<Self> {
        let stat = fs::symlink_metadata(&path)
            .map_err(|_| SqliteError::new(ErrorCode::Error, format!("cannot stat file: {path}")))?;
        Ok(Self { path, stat })
    }

    fn data(&self, max_length: i32) -> Result<Value> {
        if self.stat.is_dir() {
            return Ok(Value::Null);
        }
        if self.stat.is_symlink() {
            return Ok(fs::read_link(&self.path)
                .ok()
                .filter(|path| !path.as_os_str().is_empty())
                .map_or(Value::Null, |path| {
                    Value::Text(path.to_string_lossy().into_owned())
                }));
        }
        let Ok(mut file) = File::open(&self.path) else {
            return Ok(Value::Null);
        };
        let length = file
            .seek(SeekFrom::End(0))
            .map_err(|_| SqliteError::new(ErrorCode::IoErr, "disk I/O error"))?;
        if length > max_length as u64 {
            return Err(SqliteError::new(
                ErrorCode::TooBig,
                "string or blob too big",
            ));
        }
        let mut contents = Vec::new();
        contents
            .try_reserve_exact(length as usize)
            .map_err(|_| SqliteError::new(ErrorCode::NoMem, "out of memory"))?;
        contents.resize(length as usize, 0);
        file.rewind()
            .and_then(|_| file.read_exact(&mut contents))
            .map_err(|_| SqliteError::new(ErrorCode::IoErr, "disk I/O error"))?;
        Ok(Value::Blob(contents))
    }
}

struct Directory {
    path: String,
    entries: ReadDir,
}

struct FsDirCursor {
    max_length: Arc<AtomicI32>,
    current: Option<FileEntry>,
    levels: Vec<Directory>,
    base: usize,
    max_level: i64,
    rowid: i64,
}

fn as_c_path(value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let mut path = value.to_text_lossy();
    if let Some(index) = path.find('\0') {
        path.truncate(index);
    }
    Some(path)
}

impl VirtualCursor for FsDirCursor {
    fn filter(
        &mut self,
        _ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        _idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
        self.levels.clear();
        self.current = None;
        self.base = 0;
        self.rowid = 1;
        let Some(path) = args.first().filter(|_| idx_num & 1 != 0) else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "table function fsdir requires an argument",
            ));
        };
        let Some(mut path) = as_c_path(path) else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "table function fsdir requires a non-NULL argument",
            ));
        };
        let mut arg_index = 1;
        if idx_num & 2 != 0 {
            if let Some(base) = args.get(arg_index).and_then(as_c_path) {
                self.base = base.len() + 1;
                path = format!("{base}/{path}");
            }
            arg_index += 1;
        }
        self.max_level = if idx_num & 12 == 0 {
            0
        } else {
            let constraint = args.get(arg_index).map_or(0, Value::to_integer_lossy) as i32;
            i64::from(constraint.wrapping_add(i32::from(idx_num & 8 != 0)))
        };
        if self.max_level <= 0 {
            self.max_level = 1_000_000_000;
        }
        self.current = Some(FileEntry::open(path)?);
        Ok(())
    }

    fn set_lazy(&mut self, _lazy: bool) -> bool {
        true
    }

    fn next(&mut self) -> Result<()> {
        self.rowid += 1;
        if let Some(entry) = self.current.take()
            && entry.stat.is_dir()
            && self.levels.len() as i64 + 2 < self.max_level
        {
            let entries = fs::read_dir(&entry.path).map_err(|_| {
                SqliteError::new(
                    ErrorCode::Error,
                    format!("cannot read directory: {}", entry.path),
                )
            })?;
            self.levels.push(Directory {
                path: entry.path,
                entries,
            });
        }
        while let Some(level) = self.levels.last_mut() {
            if let Some(Ok(entry)) = level.entries.next() {
                let path = format!("{}/{}", level.path, entry.file_name().to_string_lossy());
                self.current = Some(FileEntry::open(path)?);
                return Ok(());
            }
            self.levels.pop();
        }
        Ok(())
    }

    fn eof(&self) -> bool {
        self.current.is_none()
    }

    fn column(&self, column: usize) -> Result<Value> {
        let Some(entry) = self.current.as_ref() else {
            return Ok(Value::Null);
        };
        Ok(match column {
            0 => Value::Text(entry.path[self.base..].to_owned()),
            1 => Value::Integer(i64::from(entry.stat.mode())),
            2 => Value::Integer(entry.stat.mtime()),
            3 => return entry.data(self.max_length.load(Ordering::Relaxed)),
            4 => Value::Integer(self.levels.len() as i64 + 1),
            _ => Value::Null,
        })
    }

    fn rowid(&self) -> Result<i64> {
        Ok(self.rowid)
    }
}
