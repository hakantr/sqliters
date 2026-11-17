#![cfg(unix)]

use sqliters_vfs::{OpenFlags, OsVfs, Vfs, VfsFile};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "sqliters-fetch-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn file(&self) -> Box<dyn VfsFile> {
        OsVfs::new()
            .open(
                Some(&self.0.join("test.db")),
                OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
            )
            .unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn mmap_limit_waits_for_the_last_fetch_to_return() {
    let scratch = Scratch::new();
    let file = scratch.file();
    file.write_at(&vec![0x5a; 16384], 0).unwrap();
    assert_eq!(file.mmap_size(65536).unwrap(), Some(0));
    let first = file.fetch(4096, 4096).unwrap().unwrap();
    let second = file.fetch(4096, 4096).unwrap().unwrap();
    assert_eq!(first.data(), &[0x5a; 4096]);
    assert_eq!(file.mmap_size(0).unwrap(), Some(65536));
    assert_eq!(file.mmap_size(-1).unwrap(), Some(65536));
    drop(first);
    file.mmap_size(0).unwrap();
    assert_eq!(file.mmap_size(-1).unwrap(), Some(65536));
    drop(second);
    assert_eq!(file.mmap_size(0).unwrap(), Some(65536));
    assert_eq!(file.mmap_size(-1).unwrap(), Some(0));
    assert!(file.fetch(4096, 4096).unwrap().is_none());
}

#[test]
fn fetch_preserves_c_eof_buffer_and_explicit_invalidation() {
    let scratch = Scratch::new();
    let file = scratch.file();
    file.write_at(&vec![0x3c; 8192], 0).unwrap();
    file.mmap_size(i64::MAX).unwrap();
    assert_eq!(file.mmap_size(-1).unwrap(), Some(0x7fff0000));
    assert!(file.fetch(4096, 4096).unwrap().is_none());
    assert!(file.fetch(4096 - 256, 4096).unwrap().is_some());
    file.write_at(&vec![0x7f; 8192], 8192).unwrap();
    // unixFetch keeps the existing mapping until xUnfetch(NULL), even after
    // all individual references have returned and the file has grown.
    assert!(file.fetch(8192, 4096).unwrap().is_none());
    file.unfetch_all().unwrap();
    let page = file.fetch(8192, 4096).unwrap().unwrap();
    assert_eq!(page.data(), &[0x7f; 4096]);
}

#[test]
fn remapping_and_file_writes_produce_fresh_fetch_bytes() {
    let scratch = Scratch::new();
    let file = scratch.file();
    file.write_at(&vec![1; 16384], 0).unwrap();
    file.mmap_size(8192).unwrap();
    assert!(file.fetch(8192, 4096).unwrap().is_none());
    file.mmap_size(16384).unwrap();
    let old = file.fetch(8192, 4096).unwrap().unwrap();
    assert_eq!(old.data(), &[1; 4096]);
    drop(old);
    file.write_at(&[9; 4096], 8192).unwrap();
    let fresh = file.fetch(8192, 4096).unwrap().unwrap();
    assert_eq!(fresh.data(), &[9; 4096]);
    drop(fresh);
    file.mmap_size(4096).unwrap();
    assert!(file.fetch(4096, 512).unwrap().is_none());
}

#[test]
fn truncate_caps_the_existing_mapping_even_after_the_file_regrows() {
    let scratch = Scratch::new();
    let file = scratch.file();
    file.write_at(&[1; 16384], 0).unwrap();
    file.mmap_size(16384).unwrap();
    let lease = file.fetch(8192, 4096).unwrap().unwrap();

    file.truncate(8192).unwrap();
    assert!(file.fetch(8192, 4096).unwrap().is_none());
    assert_eq!(lease.data(), &[1; 4096]);
    drop(lease);

    file.write_at(&[2; 8192], 8192).unwrap();
    assert!(file.fetch(8192, 4096).unwrap().is_none());
    file.unfetch_all().unwrap();
    assert_eq!(file.fetch(8192, 4096).unwrap().unwrap().data(), &[2; 4096]);
}
