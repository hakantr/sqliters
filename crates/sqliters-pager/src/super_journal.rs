//! Two-phase commit super-journal (master-journal) naming, binary trailer codec,
//! order-independent recovery helpers, and multi-database coordinator ported from
//! `sqlite/src/pager.c` (`lines 1298-1816, 2583-3060, 6485-6802`),
//! `sqlite/src/vdbeaux.c` (`lines 2963-3189`), and `sqlite/src/util.c` (`lines 2036-2046`).

use std::path::{Path, PathBuf};

use sqliters_types::{Result, SqliteError, extended};
use sqliters_vfs::{IOCAP_SEQUENTIAL, OpenFlags, SyncFlags, Vfs, VfsFile};

use crate::{JOURNAL_MAGIC, MIN_JOURNAL_HEADER_BYTES, Pager, TransactionState};

/// Maximum super-journal pathname byte allowance (`1 + mxPathname`) passed to
/// [`read_super_journal`] during hot-journal recovery and `pager_delsuper` (`pager.c:2669, 2895`).
pub const SUPER_JOURNAL_MAX_PATH_BYTES: u64 = 4097;

/// Maximum collision retries before `vdbeCommit` unlinks a stale super-journal candidate
/// (`sqlite/src/vdbeaux.c:3066-3069`).
pub const SUPER_JOURNAL_COLLISION_RETRY_LIMIT: usize = 100;

/// Checks whether `name` matches either of the two super-journal filename patterns accepted by
/// `pagerIsSuperJrnlName` (`sqlite/src/pager.c:1298-1316`):
///
/// 1. 8.3 mode: `<stem>.9XX` (`bytes[n-4] == b'.' && bytes[n-3] == b'9'` + 2 ASCII hex digits).
/// 2. Standard mode: `<db>-mjXXXXXX9XX` (`&bytes[n-12..n-9] == b"-mj" && bytes[n-3] == b'9'` +
///    9 ASCII hex digits).
#[must_use]
pub fn is_super_journal_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.contains(&0) {
        return false;
    }
    let n = bytes.len();
    if n >= 4
        && bytes[n - 4] == b'.'
        && bytes[n - 3] == b'9'
        && bytes[n - 2].is_ascii_hexdigit()
        && bytes[n - 1].is_ascii_hexdigit()
    {
        return true;
    }
    if n >= 12
        && &bytes[n - 12..n - 9] == b"-mj"
        && bytes[n - 3] == b'9'
        && bytes[n - 9..n].iter().all(u8::is_ascii_hexdigit)
    {
        return true;
    }
    false
}

/// Shortens the file extension of `target` to its last 3 characters when the extension exceeds
/// 3 bytes (`sqlite3FileSuffix3` in `sqlite/src/util.c:2036-2046`).
///
/// For example, `"test.db-mj7F331F9FA"` becomes `"test.9FA"`.
pub fn apply_8_3_suffix(target: &mut String) {
    let bytes = target.as_bytes();
    let sz = bytes.len();
    if sz == 0 {
        return;
    }
    let mut i = sz - 1;
    while i > 0 && bytes[i] != b'/' && bytes[i] != b'\\' && bytes[i] != b'.' {
        i -= 1;
    }
    if bytes[i] == b'.' && sz > i + 4 && target.is_char_boundary(sz - 3) {
        let tail = [bytes[sz - 3], bytes[sz - 2], bytes[sz - 1]];
        if let Ok(tail_str) = std::str::from_utf8(&tail) {
            target.replace_range((i + 1)..sz, tail_str);
        }
    }
}

/// Formats a candidate super-journal filename from `main_db_filename` and 32 random bits
/// (`sqlite/src/vdbeaux.c:3076-3081`).
#[must_use]
pub fn format_super_journal_name(
    main_db_filename: &str,
    random_u32: u32,
    enable_8_3: bool,
) -> String {
    let mut out = format!(
        "{main_db_filename}-mj{:06X}9{:02X}",
        (random_u32 >> 8) & 0x00ff_ffff,
        random_u32 & 0xff
    );
    if enable_8_3 {
        apply_8_3_suffix(&mut out);
    }
    out
}

/// Computes the super-journal filename checksum where each UTF-8 byte is interpreted as a signed
/// 8-bit integer (`writeSuperJournal` / `readSuperJournal` in `sqlite/src/pager.c:1368, 1746-1775`).
#[inline]
#[must_use]
pub fn super_journal_checksum(name_utf8: &[u8]) -> u32 {
    let mut cksum: u32 = 0;
    for &b in name_utf8 {
        cksum = cksum.wrapping_add((b as i8) as i32 as u32);
    }
    cksum
}

/// Returns the sector-aligned journal offset at or immediately following `journal_off`
/// (`journalHdrOffset` in `sqlite/src/pager.c:1403-1413`).
#[inline]
#[must_use]
pub fn journal_hdr_offset(journal_off: u64, sector_size: u32) -> u64 {
    if journal_off == 0 {
        0
    } else {
        let hdr_sz = u64::from(sector_size.max(MIN_JOURNAL_HEADER_BYTES as u32));
        ((journal_off - 1) / hdr_sz + 1) * hdr_sz
    }
}

#[inline]
pub(crate) fn read_exact_from_vfs(file: &dyn VfsFile, buf: &mut [u8], offset: u64) -> Result<()> {
    let n = file.read_at(buf, offset)?;
    if n < buf.len() {
        return Err(SqliteError::with_extended(
            extended::SQLITE_IOERR_SHORT_READ,
            "short read while reading super-journal file or trailer",
        ));
    }
    Ok(())
}

/// Reads and validates an optional super-journal filename trailer at the end of rollback journal
/// handle `jfile` (`readSuperJournal` in `sqlite/src/pager.c:1338-1386`).
///
/// Underlying VFS `file_size()` and `read_at()` I/O errors are propagated as `Err(...)`, whereas
/// absent, out-of-range, or corrupted trailers return `Ok(None)` (`SQLITE_OK` with a `NULL`
/// super-journal pointer).
pub fn read_super_journal(jfile: &dyn VfsFile, max_super_bytes: u64) -> Result<Option<String>> {
    let sz_j = jfile.file_size()?;
    if sz_j < 16 {
        return Ok(None);
    }

    let mut len_bytes = [0u8; 4];
    read_exact_from_vfs(jfile, &mut len_bytes, sz_j - 16)?;
    let len = u32::from_be_bytes(len_bytes);
    let len_u64 = u64::from(len);
    if len == 0 || len_u64 >= max_super_bytes || len_u64 > sz_j - 16 {
        return Ok(None);
    }

    let mut cksum_bytes = [0u8; 4];
    read_exact_from_vfs(jfile, &mut cksum_bytes, sz_j - 12)?;
    let cksum = u32::from_be_bytes(cksum_bytes);

    let mut magic = [0u8; 8];
    read_exact_from_vfs(jfile, &mut magic, sz_j - 8)?;

    let mut buf = vec![0u8; len as usize];
    read_exact_from_vfs(jfile, &mut buf, sz_j - 16 - len_u64)?;

    if magic != JOURNAL_MAGIC || buf.contains(&0) || super_journal_checksum(&buf) != cksum {
        return Ok(None);
    }
    let Ok(name_str) = std::str::from_utf8(&buf) else {
        return Ok(None);
    };
    if !is_super_journal_name(name_str) {
        return Ok(None);
    }

    Ok(Some(name_str.to_owned()))
}

/// Selects a unique `-mj...` (or `.9XX`) super-journal filename, opens it exclusively with
/// [`OpenFlags::SUPER_JOURNAL`], writes NUL-terminated UTF-8 `participant_journals`, and syncs the
/// super-journal file (unless [`IOCAP_SEQUENTIAL`] is set on the device), matching `vdbeCommit`
/// (`sqlite/src/vdbeaux.c:3058-3131`).
///
/// If writing or syncing `pSuperJrnl` fails, the handle is closed and the candidate file is
/// deleted (`sync_dir = false`) before returning the error.
pub fn create_super_journal_file(
    vfs: &dyn Vfs,
    main_db_filename: &str,
    enable_8_3: bool,
    participant_journals: &[&Path],
) -> Result<(String, Box<dyn VfsFile>)> {
    if main_db_filename.is_empty() {
        return Err(SqliteError::misuse(
            "super-journal creation requires a non-empty main database filename",
        ));
    }

    let mut retry_count: usize = 0;
    let mut z_super = String::new();
    loop {
        if retry_count > SUPER_JOURNAL_COLLISION_RETRY_LIMIT {
            let _ = vfs.delete(Path::new(&z_super), false);
            break;
        }
        retry_count += 1;
        let mut rnd = [0u8; 4];
        vfs.randomness(&mut rnd);
        let i_random = u32::from_ne_bytes(rnd);
        z_super = format_super_journal_name(main_db_filename, i_random, enable_8_3);
        if !vfs.exists(Path::new(&z_super))? {
            break;
        }
    }

    let mut p_super = vfs.open(
        Some(Path::new(&z_super)),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::EXCLUSIVE | OpenFlags::SUPER_JOURNAL,
    )?;

    let mut offset: u64 = 0;
    for &jpath in participant_journals {
        if jpath.as_os_str().is_empty() {
            continue;
        }
        let Some(j_str) = jpath.to_str() else {
            let _ = p_super.close();
            let _ = vfs.delete(Path::new(&z_super), false);
            return Err(SqliteError::misuse(
                "participant journal path is not valid UTF-8",
            ));
        };
        let bytes = j_str.as_bytes();
        let mut entry = Vec::with_capacity(bytes.len() + 1);
        entry.extend_from_slice(bytes);
        entry.push(0);
        if let Err(e) = p_super.write_at(&entry, offset) {
            let _ = p_super.close();
            let _ = vfs.delete(Path::new(&z_super), false);
            return Err(e);
        }
        offset += entry.len() as u64;
    }

    if (p_super.device_characteristics() & IOCAP_SEQUENTIAL) == 0
        && let Err(e) = p_super.sync(SyncFlags::Normal)
    {
        let _ = p_super.close();
        let _ = vfs.delete(Path::new(&z_super), false);
        return Err(e);
    }

    Ok((z_super, p_super))
}

/// Coordinates a multi-pager commit following `vdbeCommit` (`sqlite/src/vdbeaux.c:2963-3185`):
///
/// 1. Counts durable rollback write participants (`n_trans`, via
///    [`Pager::needs_super_journal_in_write_tx`]) and acquires [`sqliters_vfs::LockLevel::Exclusive`]
///    on every non-WAL write participant (`sqlite3PagerExclusiveLock`).
/// 2. When `main_db_path` is absent/empty or `n_trans <= 1`, executes the simple two-step commit
///    (`commit_phase_one(None)` followed by `commit_phase_two(retain_read, false)`).
/// 3. When `main_db_path` is non-empty and `n_trans > 1`, creates and syncs the `-mj...`
///    super-journal listing all named write participants' journal paths, executes
///    `commit_phase_one(Some(&z_super))` on every participant, closes the super-journal handle,
///    atomically commits by unlinking `z_super` with `sync_dir = true`, and runs benign phase-two
///    journal cleanup (`commit_phase_two(retain_read, true)`) across all participants.
pub fn commit_pagers_two_phase(
    vfs: &dyn Vfs,
    main_db_path: Option<&Path>,
    enable_8_3: bool,
    pagers: &mut [&mut Pager],
    retain_reads: &[bool],
) -> Result<()> {
    let mut n_trans = 0usize;
    let mut need_xcommit = false;

    for pager in pagers.iter_mut() {
        if pager.transaction_state() == TransactionState::Write {
            need_xcommit = true;
            if pager.needs_super_journal_in_write_tx() {
                n_trans += 1;
            }
            pager.acquire_exclusive_lock()?;
        }
    }

    let main_db_str = main_db_path
        .and_then(Path::to_str)
        .filter(|s| !s.is_empty());

    let Some(main_str) = main_db_str.filter(|_| n_trans > 1) else {
        if need_xcommit {
            for pager in pagers.iter_mut() {
                if pager.transaction_state() == TransactionState::Write {
                    pager.commit_phase_one(None)?;
                }
            }
        }
        for (i, pager) in pagers.iter_mut().enumerate() {
            let retain = retain_reads.get(i).copied().unwrap_or(false);
            pager.commit_phase_two(retain, false)?;
        }
        return Ok(());
    };

    let participant_journals: Vec<PathBuf> = pagers
        .iter()
        .filter(|p| p.transaction_state() == TransactionState::Write)
        .filter_map(|p| p.journal_path().map(Path::to_path_buf))
        .collect();
    let participant_refs: Vec<&Path> = participant_journals.iter().map(PathBuf::as_path).collect();

    let (z_super, mut p_super) =
        create_super_journal_file(vfs, main_str, enable_8_3, &participant_refs)?;

    let mut phase_one_err: Option<SqliteError> = None;
    for pager in pagers.iter_mut() {
        if let Err(e) = pager.commit_phase_one(Some(&z_super)) {
            phase_one_err = Some(e);
            break;
        }
    }
    let close_res = p_super.close();
    if let Some(err) = phase_one_err {
        // Leave `z_super` on disk in case any participant already wrote its super-journal trailer
        // (`vdbeaux.c:3135-3154`).
        return Err(err);
    }
    close_res?;

    // Unlinking `z_super` with `sync_dir = true` is the atomic multi-file commit point!
    vfs.delete(Path::new(&z_super), true)?;

    for (i, pager) in pagers.iter_mut().enumerate() {
        let retain = retain_reads.get(i).copied().unwrap_or(false);
        let _ = pager.commit_phase_two(retain, true);
    }

    Ok(())
}
