//! Cleanup: well-known junk locations that are safe to clear, free and on demand.

use crate::platform;
use crate::scan::size_of;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// Delete everything inside these folders (the folders stay).
    Contents,
    /// Move files older than N days to the Trash (recoverable).
    OldFiles,
    /// Empty the Trash / Recycle Bin.
    Trash,
    /// Delete only Explorer's thumbnail cache files (thumbcache_*.db) directly in these folders.
    #[cfg_attr(not(windows), allow(dead_code))]
    ThumbCache,
}

/// Explorer's thumbnail databases: `thumbcache_*.db`. Other files in that folder (icon
/// cache, search data, …) are left alone.
pub fn is_thumb_cache(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("thumbcache_") && n.ends_with(".db")
}

fn thumb_cache_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for e in entries.flatten() {
            if e.file_type().map_or(false, |t| t.is_file()) && is_thumb_cache(&e.file_name().to_string_lossy()) {
                v.push(e.path());
            }
        }
    }
    v
}

/// The order categories run in: the Trash is emptied first, so anything this same run moves to
/// the Trash (old downloads) is still there to get back afterwards.
pub fn run_order(kinds: &[Kind]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..kinds.len()).collect();
    idx.sort_by_key(|&i| kinds[i] != Kind::Trash);
    idx
}

pub struct Category {
    pub name: &'static str,
    pub about: &'static str,
    pub kind: Kind,
    pub paths: Vec<PathBuf>,
    pub selected: bool,
    /// (bytes, files); None = not measured yet; Some(None) = no access.
    pub measured: Option<Option<(u64, u64)>>,
}

fn cat(name: &'static str, about: &'static str, kind: Kind, paths: Vec<PathBuf>, selected: bool) -> Category {
    Category { name, about, kind, paths, selected, measured: None }
}

pub fn categories() -> Vec<Category> {
    let h = platform::home();
    let mut v = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let lib = h.join("Library");
        v.push(cat("App caches", "Temporary data apps rebuild by themselves.", Kind::Contents, vec![lib.join("Caches")], true));
        v.push(cat("Logs", "Diagnostic logs from apps.", Kind::Contents, vec![lib.join("Logs")], true));
        v.push(cat(
            "Developer leftovers",
            "Xcode build data and old simulators' caches.",
            Kind::Contents,
            vec![lib.join("Developer/Xcode/DerivedData"), lib.join("Developer/CoreSimulator/Caches")],
            true,
        ));
        v.push(cat("Temporary files", "Your user's temp folder.", Kind::Contents, vec![std::env::temp_dir()], false));
    }
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| h.join("AppData\\Local"));
        let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
        v.push(cat("Temporary files", "Your user's temp folder.", Kind::Contents, vec![std::env::temp_dir()], true));
        v.push(cat("Windows temp", "System temp folder (some files may be in use).", Kind::Contents, vec![windir.join("Temp")], true));
        v.push(cat("Crash dumps", "Memory dumps left by crashed apps.", Kind::Contents, vec![local.join("CrashDumps")], true));
        v.push(cat(
            "Thumbnail cache",
            "Picture previews Explorer rebuilds when needed (thumbcache files only).",
            Kind::ThumbCache,
            vec![local.join("Microsoft\\Windows\\Explorer")],
            false,
        ));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        v.push(cat("App caches", "Temporary data apps rebuild by themselves.", Kind::Contents, vec![h.join(".cache")], true));
    }
    v.push(cat(
        "Old downloads",
        "Files in Downloads you haven't touched in a while (moved to the Trash, so you can get them back).",
        Kind::OldFiles,
        vec![h.join("Downloads")],
        false,
    ));
    v.push(cat(
        if cfg!(windows) { "Recycle Bin" } else { "Trash" },
        "Permanently removes what's already in it.",
        Kind::Trash,
        vec![],
        false,
    ));
    v
}

fn cutoff(days: u32) -> SystemTime {
    SystemTime::now() - Duration::from_secs(days as u64 * 86_400)
}

/// Bytes and file count that cleaning this category would free.
pub fn measure(kind: Kind, paths: &[PathBuf], days: u32) -> Option<(u64, u64)> {
    match kind {
        Kind::Trash => platform::trash_size(),
        Kind::ThumbCache => {
            let mut total = (0, 0);
            for p in thumb_cache_files(paths) {
                total.0 += crate::scan::reclaimable(&p);
                total.1 += 1;
            }
            Some(total)
        }
        Kind::OldFiles => {
            std::fs::read_dir(paths.first()?).ok()?;
            let mut total = (0, 0);
            for p in old_items(paths, cutoff(days)) {
                let (b, f) = size_of(&p, None);
                total.0 += b;
                total.1 += f;
            }
            Some(total)
        }
        Kind::Contents => {
            let mut total = (0, 0);
            let mut any = false;
            for p in paths {
                if std::fs::read_dir(p).is_ok() {
                    any = true;
                    let (b, f) = size_of(p, None);
                    total.0 += b;
                    total.1 += f;
                }
            }
            // A folder that doesn't exist simply has nothing to clean.
            if any || paths.iter().all(|p| !p.exists()) {
                Some(total)
            } else {
                None
            }
        }
    }
}

pub struct Outcome {
    /// Deleted for good: disk space given back.
    pub freed: u64,
    /// Moved to the Trash: still on disk until the Trash is emptied.
    pub trashed: u64,
    pub skipped: u64,
}

pub fn clean(kind: Kind, paths: &[PathBuf], days: u32) -> Outcome {
    let mut out = Outcome { freed: 0, trashed: 0, skipped: 0 };
    match kind {
        Kind::Trash => {
            let before = platform::trash_size().map_or(0, |s| s.0);
            if platform::empty_trash().is_ok() {
                let after = platform::trash_size().map_or(0, |s| s.0);
                out.freed = before.saturating_sub(after);
            } else {
                out.skipped += 1;
            }
        }
        Kind::Contents => {
            for dir in paths {
                let Ok(entries) = std::fs::read_dir(dir) else { continue };
                for e in entries.flatten() {
                    remove_tree(&e.path(), &mut out);
                }
            }
        }
        Kind::ThumbCache => {
            for p in thumb_cache_files(paths) {
                remove_tree(&p, &mut out);
            }
        }
        Kind::OldFiles => {
            for p in old_items(paths, cutoff(days)) {
                let size = crate::scan::reclaimable(&p);
                match platform::move_to_trash(&p) {
                    Ok(()) => out.trashed += size,
                    Err(_) => out.skipped += 1,
                }
            }
        }
    }
    out
}

/// Top-level items in these folders where nothing inside changed since `limit`.
fn old_items(dirs: &[PathBuf], limit: SystemTime) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name();
            if name.to_string_lossy().starts_with('.') || has_recent(&p, limit) {
                continue;
            }
            v.push(p);
        }
    }
    v
}

fn has_recent(path: &Path, limit: SystemTime) -> bool {
    let Ok(md) = std::fs::symlink_metadata(path) else { return true };
    if md.modified().map_or(true, |m| m > limit) {
        return true;
    }
    md.is_dir()
        && std::fs::read_dir(path).map_or(true, |es| es.flatten().any(|e| has_recent(&e.path(), limit)))
}

/// Delete a file or folder; files that are in use or protected are skipped.
fn remove_tree(path: &Path, out: &mut Outcome) {
    let Ok(md) = std::fs::symlink_metadata(path) else { return };
    if md.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for e in entries.flatten() {
                remove_tree(&e.path(), out);
            }
        }
        let _ = std::fs::remove_dir(path);
    } else {
        // Read right before unlinking: only the last name of hard-linked data frees it.
        let size = crate::scan::reclaimable(path);
        match std::fs::remove_file(path) {
            Ok(()) => out.freed += size,
            Err(_) => out.skipped += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clears_contents_but_keeps_folder() {
        let dir = std::env::current_dir().unwrap().join("target/scratch-clean");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub/deeper")).unwrap();
        std::fs::write(dir.join("a.bin"), vec![1u8; 10_000]).unwrap();
        std::fs::write(dir.join("sub/deeper/b.bin"), vec![1u8; 20_000]).unwrap();
        let (bytes, files) = measure(Kind::Contents, &[dir.clone()], 30).unwrap();
        assert_eq!(files, 2);
        assert!(bytes >= 30_000);
        let out = clean(Kind::Contents, &[dir.clone()], 30);
        assert_eq!(out.freed, bytes);
        assert!(dir.is_dir());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        // Freshly made files are never "old downloads".
        std::fs::write(dir.join("new.bin"), b"x").unwrap();
        assert_eq!(measure(Kind::OldFiles, &[dir.clone()], 1), Some((0, 0)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn trash_is_emptied_before_anything_moves_into_it() {
        let kinds = [Kind::Contents, Kind::OldFiles, Kind::Contents, Kind::Trash];
        let order = run_order(&kinds);
        assert!(kinds[order[0]] == Kind::Trash);
        assert_eq!(order, vec![3, 0, 1, 2]); // the rest keep their order
        assert_eq!(run_order(&[Kind::OldFiles]), vec![0]);
    }

    #[test]
    fn thumb_cache_only_touches_thumbcache_files() {
        assert!(is_thumb_cache("thumbcache_256.db"));
        assert!(is_thumb_cache("THUMBCACHE_IDX.DB"));
        assert!(!is_thumb_cache("iconcache_256.db"));
        assert!(!is_thumb_cache("thumbcache_256.db.bak"));
        assert!(!is_thumb_cache("ExplorerStartupLog.etl"));

        let dir = std::env::current_dir().unwrap().join("target/scratch-thumbcache");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("thumbcache_sub")).unwrap();
        std::fs::write(dir.join("thumbcache_96.db"), vec![1u8; 5_000]).unwrap();
        std::fs::write(dir.join("iconcache_96.db"), b"keep").unwrap();
        std::fs::write(dir.join("other.txt"), b"keep").unwrap();
        std::fs::write(dir.join("thumbcache_sub/thumbcache_1.db"), b"keep").unwrap();
        let (_, files) = measure(Kind::ThumbCache, &[dir.clone()], 30).unwrap();
        assert_eq!(files, 1);
        let out = clean(Kind::ThumbCache, &[dir.clone()], 30);
        assert_eq!(out.skipped, 0);
        assert!(!dir.join("thumbcache_96.db").exists());
        for keep in ["iconcache_96.db", "other.txt", "thumbcache_sub/thumbcache_1.db"] {
            assert!(dir.join(keep).exists(), "{keep} should be untouched");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn hard_linked_file_is_freed_once() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::current_dir().unwrap().join("target/scratch-clean-links");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("big.bin"), vec![1u8; 300_000]).unwrap();
        std::fs::hard_link(dir.join("big.bin"), dir.join("sub/link.bin")).unwrap();
        let bytes = std::fs::metadata(dir.join("big.bin")).unwrap().blocks() * 512;
        let out = clean(Kind::Contents, &[dir.clone()], 30);
        assert_eq!(out.freed, bytes);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn removing_updates_parent_sizes() {
        use crate::scan::{remove, Node};
        let leaf = |n: &str, s| Node { name: n.into(), size: s, files: 1, is_dir: false, children: vec![] };
        let mut t = Node {
            name: "r".into(), size: 30, files: 3, is_dir: true,
            children: vec![Node { name: "d".into(), size: 20, files: 2, is_dir: true, children: vec![leaf("x", 15), leaf("y", 5)] }, leaf("z", 10)],
        };
        remove(&mut t, &[0, 0]);
        let d = t.children.iter().find(|c| c.name == "d").unwrap();
        assert_eq!((t.size, t.files, d.size, d.children.len()), (15, 2, 5, 1));
        assert_eq!(t.children[0].name, "z"); // re-sorted: "z" is now the biggest
    }
}
