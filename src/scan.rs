//! Folder scanner: builds a size tree of everything under a root, reading folders in
//! parallel. Also saves/loads finished scans so reopening a disk is instant.

use rayon::prelude::*;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fs;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct Node {
    pub name: String,
    pub size: u64,
    pub files: u64,
    pub is_dir: bool,
    pub children: Vec<Node>,
}

impl Node {
    pub fn child_path(&self, root: &Path, idx: &[usize]) -> PathBuf {
        let mut p = root.to_path_buf();
        let mut n = self;
        for &i in idx {
            n = &n.children[i];
            p.push(&n.name);
        }
        p
    }

    pub fn at(&self, idx: &[usize]) -> &Node {
        idx.iter().fold(self, |n, &i| &n.children[i])
    }

    fn at_mut(&mut self, idx: &[usize]) -> &mut Node {
        idx.iter().fold(self, |n, &i| &mut n.children[i])
    }
}

#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    pub cancel: AtomicBool,
    pub current: Mutex<String>,
    /// Folders the system wouldn't let us read (no Full Disk Access, root-only, …).
    pub denied: Mutex<Vec<PathBuf>>,
    /// Sparse/compressed files whose real size couldn't be read; counted at full length (Windows).
    pub unknown_alloc: AtomicU64,
}

pub struct BigFile {
    pub path: PathBuf,
    pub size: u64,
}

pub struct ScanResult {
    pub root: PathBuf,
    pub tree: Node,
    pub biggest: Vec<BigFile>,
    pub scanned_at: SystemTime,
    /// Folders that couldn't be read, so their contents are missing from the sizes.
    pub denied: Vec<PathBuf>,
    /// Files counted at full length because their real (sparse) size couldn't be read.
    pub unknown_alloc: u64,
    /// Saved by an older version that counted hard links differently: fine to look at, but
    /// re-reading one folder into it would mix the two ways, so it needs a full rescan first.
    pub legacy: bool,
}

impl ScanResult {
    pub fn new(root: PathBuf, tree: Node, scanned_at: SystemTime, denied: Vec<PathBuf>) -> Self {
        let biggest = biggest_files(&tree, &root);
        ScanResult { root, tree, biggest, scanned_at, denied, unknown_alloc: 0, legacy: false }
    }
}

const BIGGEST_KEEP: usize = 200;

/// True if a scan never walks into `path` (it or a folder above it is skipped).
#[cfg(target_os = "macos")]
pub fn skipped(path: &Path) -> bool {
    path.ancestors().any(skip)
}

/// Folders that are other disks or virtual and would be double-counted.
fn skip(path: &Path) -> bool {
    if cfg!(target_os = "macos") {
        matches!(
            path.to_str(),
            Some("/System/Volumes" | "/Volumes" | "/dev" | "/net" | "/home" | "/cores")
        )
    } else if cfg!(target_os = "linux") {
        matches!(path.to_str(), Some("/proc" | "/sys" | "/dev" | "/run"))
    } else {
        false
    }
}

/// What a scan needs to know about one file to charge it fairly.
struct FileFacts {
    /// Space its data takes on disk (sparse/compressed files count less).
    bytes: u64,
    /// Names (hard links) sharing that data.
    links: u64,
    /// Identity of the data, shared by all its names (None if not looked up).
    id: Option<(u64, u64)>,
    /// A sparse/compressed file whose real size couldn't be read, so `bytes` is its full length.
    unknown_alloc: bool,
}

#[cfg(unix)]
fn facts(md: &fs::Metadata, _path: &Path) -> FileFacts {
    use std::os::unix::fs::MetadataExt;
    FileFacts { bytes: md.blocks() * 512, links: md.nlink().max(1), id: Some((md.dev(), md.ino())), unknown_alloc: false }
}

#[cfg(windows)]
fn facts(md: &fs::Metadata, path: &Path) -> FileFacts {
    use std::os::windows::fs::MetadataExt;
    const SPARSE: u32 = 0x200;
    const COMPRESSED: u32 = 0x800;
    let packed = md.file_attributes() & (SPARSE | COMPRESSED) != 0;
    let plain = FileFacts { bytes: md.len(), links: 1, id: None, unknown_alloc: false };
    // Hard links and real sizes are only known by opening the file, so skip that for small
    // plain files, where a hard link changes the total by little.
    if !packed && md.len() < WIN_LINK_CHECK_MIN {
        return plain;
    }
    match crate::platform::file_info(path) {
        Some(i) => FileFacts { bytes: i.allocated, links: i.links.max(1), id: Some(i.id), unknown_alloc: false },
        None => FileFacts { unknown_alloc: packed, ..plain },
    }
}

#[cfg(windows)]
const WIN_LINK_CHECK_MIN: u64 = 64 * 1024;

#[cfg(not(any(unix, windows)))]
fn facts(md: &fs::Metadata, _path: &Path) -> FileFacts {
    FileFacts { bytes: md.len(), links: 1, id: None, unknown_alloc: false }
}

/// What one name of a file is charged in size totals.
///
/// Hard-linked data is split evenly across its N names, so a folder's size doesn't depend on
/// which other folders were scanned with it (rescanning one folder can't double-count). The
/// bytes left over from the split go to the first name the scan meets, so a whole scan adds up
/// exactly.
fn share(bytes: u64, links: u64, first: bool) -> u64 {
    let n = links.max(1);
    bytes / n + if first { bytes % n } else { 0 }
}

/// Charges files during one scan, remembering which hard-linked data it has met.
#[derive(Default)]
struct Charger {
    seen: Mutex<HashSet<(u64, u64)>>,
}

impl Charger {
    fn charge(&self, f: &FileFacts) -> u64 {
        if f.links <= 1 {
            return f.bytes;
        }
        let first = f.id.map_or(true, |id| self.seen.lock().map_or(true, |mut s| s.insert(id)));
        share(f.bytes, f.links, first)
    }
}

/// Bytes deleting `path` (a file or a whole folder) would give back: data counts only when
/// every one of its names is inside. Read it right before deleting, so names already removed
/// are accounted for.
pub fn reclaimable(path: &Path) -> u64 {
    fn visit(path: &Path, multi: &mut HashMap<(u64, u64), (u64, u64, u64)>, total: &mut u64) {
        let Ok(md) = fs::symlink_metadata(path) else { return };
        if md.is_dir() {
            for e in fs::read_dir(path).into_iter().flatten().flatten() {
                visit(&e.path(), multi, total);
            }
            return;
        }
        let f = facts(&md, path);
        match f.id.filter(|_| f.links > 1) {
            None => *total += f.bytes,
            Some(id) => multi.entry(id).or_insert((0, f.links, f.bytes)).0 += 1,
        }
    }
    let (mut multi, mut total) = (HashMap::new(), 0);
    visit(path, &mut multi, &mut total);
    total + multi.values().filter(|(seen, links, _)| seen >= links).map(|v| v.2).sum::<u64>()
}

/// Scan a folder. Returns None if it was stopped.
pub fn scan_tree(path: &Path, progress: &Progress) -> Option<Node> {
    let name = path.file_name().map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy()).to_string();
    let w = Walker { p: progress, foreign: crate::platform::foreign_devices(path), charger: Charger::default() };
    let mut tree = w.walk(path, name);
    if progress.cancel.load(Ordering::Relaxed) {
        return None;
    }
    sort_tree(&mut tree);
    Some(tree)
}

pub fn scan(root: PathBuf, progress: Arc<Progress>) -> Option<ScanResult> {
    let mut tree = scan_tree(&root, &progress)?;
    tree.name = root.to_string_lossy().to_string();
    let denied = take_denied(&progress);
    let mut r = ScanResult::new(root, tree, SystemTime::now(), denied);
    r.unknown_alloc = progress.unknown_alloc.load(Ordering::Relaxed);
    Some(r)
}

/// The folders a finished scan couldn't read, sorted.
pub fn take_denied(p: &Progress) -> Vec<PathBuf> {
    let mut d = std::mem::take(&mut *p.denied.lock().unwrap_or_else(|e| e.into_inner()));
    d.sort();
    d
}

/// After re-reading the folder `under`: swap its old unreadable-folder entries for the new ones.
pub fn replace_denied(denied: &mut Vec<PathBuf>, under: &Path, fresh: Vec<PathBuf>) {
    denied.retain(|d| !d.starts_with(under));
    denied.extend(fresh);
    denied.sort();
}

struct Walker<'a> {
    p: &'a Progress,
    /// Device ids of other disks, which the walk doesn't enter.
    #[cfg_attr(not(unix), allow(dead_code))]
    foreign: Vec<u64>,
    charger: Charger,
}

impl Walker<'_> {
    /// A folder that's really another disk mounted here.
    #[cfg(unix)]
    fn foreign(&self, e: &fs::DirEntry) -> bool {
        use std::os::unix::fs::MetadataExt;
        // Compared as 32-bit: that's what the mount table reports.
        !self.foreign.is_empty() && e.metadata().map_or(false, |m| self.foreign.contains(&(m.dev() as u32 as u64)))
    }

    #[cfg(not(unix))]
    fn foreign(&self, _e: &fs::DirEntry) -> bool {
        false
    }
}

impl Walker<'_> {
    fn walk(&self, path: &Path, name: String) -> Node {
        let p = self.p;
        let mut node = Node { name, size: 0, files: 0, is_dir: true, children: Vec::new() };
        if p.cancel.load(Ordering::Relaxed) {
            return node;
        }
        if let Ok(mut cur) = p.current.try_lock() {
            *cur = path.to_string_lossy().to_string();
        }
        let entries = match fs::read_dir(path) {
            Ok(e) => e,
            Err(e) => {
                if e.kind() == io::ErrorKind::PermissionDenied {
                    if let Ok(mut d) = p.denied.lock() {
                        d.push(path.to_path_buf());
                    }
                }
                return node;
            }
        };
        let entries: Vec<fs::DirEntry> = entries.flatten().collect();
        // Sub-folders (and big folders' files) are read on all CPU cores at once.
        node.children = entries
            .into_par_iter()
            .filter_map(|e| {
                // The entry's own type/metadata never follows links (no loops, no double counting),
                // and on Windows it comes free with the folder listing.
                let ft = e.file_type().ok()?;
                let name = e.file_name().to_string_lossy().into_owned();
                if ft.is_dir() {
                    let child_path = e.path();
                    if skip(&child_path) || self.foreign(&e) {
                        return None;
                    }
                    Some(self.walk(&child_path, name))
                } else {
                    let size = e.metadata().map_or(0, |m| {
                        let f = facts(&m, &e.path());
                        if f.unknown_alloc {
                            p.unknown_alloc.fetch_add(1, Ordering::Relaxed);
                        }
                        self.charger.charge(&f)
                    });
                    Some(Node { name, size, files: 1, is_dir: false, children: Vec::new() })
                }
            })
            .collect();
        let (mut own_files, mut own_bytes) = (0, 0);
        for c in &node.children {
            node.size += c.size;
            node.files += c.files;
            if !c.is_dir {
                own_files += 1;
                own_bytes += c.size;
            }
        }
        p.files.fetch_add(own_files, Ordering::Relaxed);
        p.bytes.fetch_add(own_bytes, Ordering::Relaxed);
        node
    }
}

fn sort_tree(n: &mut Node) {
    n.children.sort_by(|a, b| b.size.cmp(&a.size));
    n.children.par_iter_mut().filter(|c| c.is_dir).for_each(sort_tree);
}

/// The largest files in a tree, biggest first.
pub fn biggest_files(tree: &Node, root: &Path) -> Vec<BigFile> {
    fn visit(n: &Node, path: &mut PathBuf, heap: &mut BinaryHeap<Reverse<(u64, PathBuf)>>) {
        for c in &n.children {
            if c.is_dir {
                path.push(&c.name);
                visit(c, path, heap);
                path.pop();
            } else if c.size > 0 && (heap.len() < BIGGEST_KEEP || c.size > heap.peek().map_or(0, |r| r.0 .0)) {
                heap.push(Reverse((c.size, path.join(&c.name))));
                if heap.len() > BIGGEST_KEEP {
                    heap.pop();
                }
            }
        }
    }
    let mut heap = BinaryHeap::new();
    visit(tree, &mut root.to_path_buf(), &mut heap);
    let mut v: Vec<BigFile> = heap.into_iter().map(|Reverse((size, path))| BigFile { path, size }).collect();
    v.sort_by(|a, b| b.size.cmp(&a.size));
    v
}

// ---------- finding and changing parts of the tree ----------

/// Index path for a list of folder names; stops at the first one that's gone.
pub fn resolve(tree: &Node, names: &[String]) -> Vec<usize> {
    let mut idx = Vec::new();
    let mut n = tree;
    for name in names {
        match n.children.iter().position(|c| &c.name == name) {
            Some(i) => {
                idx.push(i);
                n = &n.children[i];
            }
            None => break,
        }
    }
    idx
}

pub fn find_path(tree: &Node, root: &Path, path: &Path) -> Option<Vec<usize>> {
    let rel = path.strip_prefix(root).ok()?;
    let names: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    let idx = resolve(tree, &names);
    (idx.len() == names.len()).then_some(idx)
}

/// Add a size/file-count change to every folder above `idx`, then re-sort them by size.
fn adjust_ancestors(tree: &mut Node, parents: &[usize], dsize: i128, dfiles: i128) {
    let apply = |n: &mut Node| {
        n.size = (n.size as i128 + dsize).max(0) as u64;
        n.files = (n.files as i128 + dfiles).max(0) as u64;
    };
    for depth in 0..=parents.len() {
        apply(tree.at_mut(&parents[..depth]));
    }
    // Deepest first, so the indices above stay valid while sorting.
    for depth in (0..=parents.len()).rev() {
        tree.at_mut(&parents[..depth]).children.sort_by(|a, b| b.size.cmp(&a.size));
    }
}

/// Remove a node by index path and subtract its size from every ancestor.
pub fn remove(tree: &mut Node, idx: &[usize]) {
    let Some((&last, parents)) = idx.split_last() else { return };
    let removed = tree.at_mut(parents).children.remove(last);
    adjust_ancestors(tree, parents, -(removed.size as i128), -(removed.files as i128));
}

/// Swap in a freshly scanned folder and fix the sizes of every folder above it.
pub fn replace(tree: &mut Node, idx: &[usize], mut fresh: Node) {
    let Some((_, parents)) = idx.split_last() else {
        fresh.name = std::mem::take(&mut tree.name);
        *tree = fresh;
        return;
    };
    let old = tree.at_mut(idx);
    fresh.name = std::mem::take(&mut old.name);
    let dsize = fresh.size as i128 - old.size as i128;
    let dfiles = fresh.files as i128 - old.files as i128;
    *old = fresh;
    adjust_ancestors(tree, parents, dsize, dfiles);
}

// ---------- saved scans ----------

const MAGIC: &[u8; 8] = b"DSWEEP03";
/// Older saved scans; still listed and shown, but marked `legacy` (see ScanResult).
/// v1 has no unreadable-folder list; v2 has one but counted hard links differently.
const MAGIC_V1: &[u8; 8] = b"DSWEEP01";
const MAGIC_V2: &[u8; 8] = b"DSWEEP02";

pub struct SavedInfo {
    pub root: PathBuf,
    pub scanned_at: SystemTime,
    pub size: u64,
    pub files: u64,
}

pub fn saved_dir() -> PathBuf {
    let home = crate::platform::home();
    if cfg!(target_os = "macos") {
        // Not under Caches: the Cleanup screen empties that.
        home.join("Library/Application Support/Disk Sweeper/scans")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map_or_else(|| home.join("AppData\\Roaming"), PathBuf::from).join("Disk Sweeper\\scans")
    } else {
        home.join(".local/share/disk-sweeper/scans")
    }
}

/// One file per scanned folder, named after a hash of its path.
pub fn saved_file(dir: &Path, root: &Path) -> PathBuf {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in root.to_string_lossy().bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    dir.join(format!("{h:016x}.scan"))
}

/// Serialize a scan in memory (fast), so the file write can happen afterwards.
pub fn encode(r: &ScanResult) -> Vec<u8> {
    let (root, tree, at) = (&r.root, &r.tree, r.scanned_at);
    fn node(out: &mut Vec<u8>, n: &Node) {
        out.push(n.is_dir as u8);
        str(out, &n.name);
        out.extend_from_slice(&n.size.to_le_bytes());
        if n.is_dir {
            out.extend_from_slice(&n.files.to_le_bytes());
            out.extend_from_slice(&(n.children.len() as u32).to_le_bytes());
            for c in &n.children {
                node(out, c);
            }
        }
    }
    fn str(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }
    let mut out = Vec::with_capacity(tree.files as usize * 32 + 64);
    out.extend_from_slice(MAGIC);
    str(&mut out, &root.to_string_lossy());
    out.extend_from_slice(&at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs().to_le_bytes());
    out.extend_from_slice(&tree.size.to_le_bytes());
    out.extend_from_slice(&tree.files.to_le_bytes());
    node(&mut out, tree);
    out.extend_from_slice(&(r.denied.len() as u32).to_le_bytes());
    for d in &r.denied {
        str(&mut out, &d.to_string_lossy());
    }
    out.extend_from_slice(&r.unknown_alloc.to_le_bytes());
    out
}

/// Write via a temp file + rename, so a crash never leaves a half-written scan.
pub fn write_saved(file: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("tmp");
    {
        let mut w = BufWriter::new(fs::File::create(&tmp)?);
        w.write_all(bytes)?;
        w.flush()?;
    }
    fs::rename(&tmp, file)
}

struct Reader<R: Read>(R);

impl<R: Read> Reader<R> {
    fn u64(&mut self) -> io::Result<u64> {
        let mut b = [0; 8];
        self.0.read_exact(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }
    fn u32(&mut self) -> io::Result<u32> {
        let mut b = [0; 4];
        self.0.read_exact(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }
    fn str(&mut self) -> io::Result<String> {
        let len = self.u32()? as usize;
        if len > 1 << 16 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut b = vec![0; len];
        self.0.read_exact(&mut b)?;
        String::from_utf8(b).map_err(|_| io::ErrorKind::InvalidData.into())
    }
    /// The summary, and the format version (1–3).
    fn header(&mut self) -> io::Result<(SavedInfo, u8)> {
        let mut m = [0; 8];
        self.0.read_exact(&mut m)?;
        let version = match &m {
            m if m == MAGIC => 3,
            m if m == MAGIC_V2 => 2,
            m if m == MAGIC_V1 => 1,
            _ => return Err(io::ErrorKind::InvalidData.into()),
        };
        let root = PathBuf::from(self.str()?);
        let scanned_at = UNIX_EPOCH + Duration::from_secs(self.u64()?);
        Ok((SavedInfo { root, scanned_at, size: self.u64()?, files: self.u64()? }, version))
    }
    fn node(&mut self, depth: usize) -> io::Result<Node> {
        let mut flag = [0; 1];
        self.0.read_exact(&mut flag)?;
        let is_dir = flag[0] == 1;
        let name = self.str()?;
        let size = self.u64()?;
        if !is_dir {
            return Ok(Node { name, size, files: 1, is_dir, children: Vec::new() });
        }
        if depth > 4096 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let files = self.u64()?;
        let count = self.u32()? as usize;
        let mut children = Vec::with_capacity(count.min(1 << 16));
        for _ in 0..count {
            children.push(self.node(depth + 1)?);
        }
        Ok(Node { name, size, files, is_dir, children })
    }
}

/// Just the summary of a saved scan (cheap; used for the "Recent scans" list).
pub fn read_info(file: &Path) -> Option<SavedInfo> {
    Reader(BufReader::new(fs::File::open(file).ok()?)).header().ok().map(|h| h.0)
}

/// A whole saved scan; None if it's missing, from another version or damaged.
pub fn load_saved(file: &Path) -> Option<ScanResult> {
    let mut r = Reader(BufReader::with_capacity(1 << 20, fs::File::open(file).ok()?));
    let (info, version) = r.header().ok()?;
    let tree = r.node(0).ok()?;
    let n = if version == 1 { 0 } else { r.u32().ok()? };
    let denied = (0..n).map(|_| r.str().map(PathBuf::from)).collect::<io::Result<Vec<_>>>().ok()?;
    let unknown_alloc = if version >= 3 { r.u64().ok()? } else { 0 };
    let mut s = ScanResult::new(info.root, tree, info.scanned_at, denied);
    s.unknown_alloc = unknown_alloc;
    s.legacy = version < 3;
    Some(s)
}

pub fn list_saved(dir: &Path) -> Vec<SavedInfo> {
    let mut v: Vec<SavedInfo> = fs::read_dir(dir)
        .map(|es| {
            es.flatten()
                .filter(|e| e.path().extension().map_or(false, |x| x == "scan"))
                .filter_map(|e| read_info(&e.path()))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| b.scanned_at.cmp(&a.scanned_at));
    v
}

// ---------- what a whole-disk scan couldn't see ----------

/// A disk's used space that a scan of it didn't find (`gap` = used − scanned), split into
/// other volumes on the same disk (measured exactly), the rest (unreadable folders,
/// system-only files, purgeable space) and what the scan over-counted.
///
/// The rows always add up to the header: `volumes + rest − overcount == gap`. A negative gap
/// means the scan found more than the disk reports using.
pub struct Unscanned {
    pub gap: i64,
    pub volumes: Vec<(String, u64)>,
    pub rest: u64,
    /// Space the scan counted more than once (APFS clones on macOS, unchecked hard links on
    /// Windows): files that share space each count in full.
    pub overcount: u64,
}

pub fn unscanned(used: u64, scanned: u64, volumes: Vec<(String, u64)>) -> Unscanned {
    let gap = used as i64 - scanned as i64;
    let in_volumes: i64 = volumes.iter().map(|v| v.1 as i64).sum();
    let rest = (gap - in_volumes).max(0) as u64;
    let overcount = (in_volumes - gap).max(0) as u64;
    Unscanned { gap, volumes, rest, overcount }
}

// ---------- helpers ----------

/// Total size of a path (used by cleanup to measure folders).
pub fn size_of(path: &Path, older_than: Option<SystemTime>) -> (u64, u64) {
    fn visit(path: &Path, older_than: Option<SystemTime>, c: &Charger) -> (u64, u64) {
        let Ok(md) = fs::symlink_metadata(path) else { return (0, 0) };
        if !md.is_dir() {
            if let Some(limit) = older_than {
                if md.modified().map_or(true, |m| m > limit) {
                    return (0, 0);
                }
            }
            return (c.charge(&facts(&md, path)), 1);
        }
        let mut total = (0, 0);
        if let Ok(entries) = fs::read_dir(path) {
            for e in entries.flatten() {
                let (b, f) = visit(&e.path(), older_than, c);
                total.0 += b;
                total.1 += f;
            }
        }
        total
    }
    visit(path, older_than, &Charger::default())
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} bytes")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn ago(t: SystemTime) -> String {
    let s = SystemTime::now().duration_since(t).unwrap_or_default().as_secs();
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::current_dir().unwrap().join("target").join(name);
        let _ = fs::remove_dir_all(&d);
        for (f, n) in [("a/x.bin", 5000), ("a/b/y.bin", 9000), ("a/b/c/z.bin", 3000), ("top.bin", 7000)] {
            let p = d.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, vec![7u8; n]).unwrap();
        }
        d
    }

    #[test]
    fn parallel_totals_match_simple_walk() {
        let d = scratch("scratch-scan");
        let r = scan(d.clone(), Default::default()).unwrap();
        assert_eq!((r.tree.size, r.tree.files), size_of(&d, None));
        assert_eq!(r.tree.files, 4);
        assert_eq!(r.biggest[0].path, d.join("a/b/y.bin"));
        assert_eq!(r.biggest.len(), 4);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn saved_scan_round_trips_and_rejects_damage() {
        let d = scratch("scratch-save");
        let r = scan(d.clone(), Default::default()).unwrap();
        let file = saved_file(&d.join("saved"), &d);
        write_saved(&file, &encode(&r)).unwrap();
        let back = load_saved(&file).unwrap();
        assert_eq!((back.root.clone(), back.tree.size, back.tree.files), (d.clone(), r.tree.size, r.tree.files));
        assert_eq!(back.tree.children[0].name, r.tree.children[0].name);
        assert_eq!(back.denied, r.denied);
        assert!(!back.legacy);
        // Older-format scans still list and open, marked legacy so a folder rescan can't mix
        // their hard-link accounting with the new one.
        let new = encode(&r);
        let mut v2 = new[..new.len() - 8].to_vec(); // no unknown-allocation count
        v2[..8].copy_from_slice(MAGIC_V2);
        let mut v1 = v2[..v2.len() - 4].to_vec(); // no denied list either
        v1[..8].copy_from_slice(MAGIC_V1);
        for bytes in [v1, v2] {
            let old = d.join("saved/old.scan");
            write_saved(&old, &bytes).unwrap();
            assert_eq!(read_info(&old).unwrap().files, r.tree.files);
            let loaded = load_saved(&old).unwrap();
            assert_eq!((loaded.tree.size, loaded.legacy), (r.tree.size, true));
            fs::remove_file(&old).unwrap();
        }
        assert_eq!(list_saved(&d.join("saved")).len(), 1);
        let bytes = fs::read(&file).unwrap();
        fs::write(&file, &bytes[..bytes.len() - 5]).unwrap();
        assert!(load_saved(&file).is_none());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn unscanned_adds_up_to_the_disk() {
        let adds_up = |u: &Unscanned| {
            u.volumes.iter().map(|v| v.1 as i64).sum::<i64>() + u.rest as i64 - u.overcount as i64 == u.gap
        };
        let (total, free, scanned) = (494_000u64, 4_000, 369_000);
        let u = unscanned(total - free, scanned, vec![("Preboot".into(), 11_000), ("VM".into(), 7_500)]);
        assert_eq!((u.gap, u.rest, u.overcount), (121_000, 102_500, 0));
        assert!(adds_up(&u));
        // Clones made the scan find 95 of 100 used, but 10 more sit in system volumes.
        let u = unscanned(100, 95, vec![("VM".into(), 10)]);
        assert_eq!((u.gap, u.rest, u.overcount), (5, 0, 5));
        assert!(adds_up(&u));
        // The scan found more than the disk uses at all.
        let u = unscanned(100, 150, vec![("VM".into(), 10)]);
        assert_eq!((u.gap, u.rest, u.overcount), (-50, 0, 60));
        assert!(adds_up(&u));
    }

    #[test]
    fn link_shares_add_up_exactly() {
        for (bytes, links) in [(4096u64, 3u64), (1, 2), (8192, 7), (0, 4), (12345, 1)] {
            let total: u64 = (0..links).map(|i| share(bytes, links, i == 0)).sum();
            assert_eq!(total, bytes, "{bytes} over {links}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_count_once_and_unreadable_folders_are_listed() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("scratch-links");
        fs::hard_link(d.join("top.bin"), d.join("a/top-link.bin")).unwrap();
        let locked = d.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("secret.bin"), vec![1u8; 9000]).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let mut r = scan(d.clone(), Default::default()).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let blocks = |p: &Path| {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(p).unwrap().blocks() * 512
        };
        let all = blocks(&d.join("top.bin")) + blocks(&d.join("a/x.bin")) + blocks(&d.join("a/b/y.bin")) + blocks(&d.join("a/b/c/z.bin"));
        // 5 names for 4 files' data (one hard-linked twice), the locked one unseen.
        assert_eq!((r.tree.files, r.tree.size), (5, all));
        assert_eq!(r.denied, vec![locked.clone()]);

        // Rescanning just the folder with the second link doesn't charge the shared data twice.
        let idx = find_path(&r.tree, &d, &d.join("a")).unwrap();
        let p = Progress::default();
        replace(&mut r.tree, &idx, scan_tree(&d.join("a"), &p).unwrap());
        assert_eq!(r.tree.size, all);

        // Deleting one name frees nothing; the folder holding both frees the data once.
        assert_eq!(reclaimable(&d.join("a/top-link.bin")), 0);
        assert_eq!(reclaimable(&d), all + blocks(&locked.join("secret.bin")));

        // Rescanning the once-locked folder brings in its contents and clears it from the list.
        let p = Progress::default();
        let idx = find_path(&r.tree, &d, &locked).unwrap();
        replace(&mut r.tree, &idx, scan_tree(&locked, &p).unwrap());
        replace_denied(&mut r.denied, &locked, take_denied(&p));
        assert!(r.denied.is_empty());
        assert_eq!(r.tree.size, all + blocks(&locked.join("secret.bin")));
        fs::remove_dir_all(&d).unwrap();
    }

    /// Machine-specific, slow: scan the whole startup disk and show how its used space adds up.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn whole_disk_adds_up() {
        let r = scan(PathBuf::from("/"), Default::default()).unwrap();
        let (total, free) = crate::platform::space(Path::new("/")).unwrap();
        let u = unscanned(total - free, r.tree.size, crate::platform::hidden_volumes(Path::new("/")));
        let gb = |b: u64| b as f64 / 1e9;
        println!("disk {:.1} GB, free {:.1}, used {:.1}", gb(total), gb(free), gb(total - free));
        println!("scanned {:.1} GB in {} files; {} folders unreadable", gb(r.tree.size), r.tree.files, r.denied.len());
        for c in &r.tree.children {
            println!("  {:>7.1} GB  /{}", gb(c.size), c.name);
        }
        let sims = find_path(&r.tree, &r.root, Path::new("/Library/Developer/CoreSimulator/Volumes")).map(|i| r.tree.at(&i).size);
        println!("simulator volumes counted: {:?}", sims);
        for (n, b) in &u.volumes {
            println!("  volume {n}: {:.2} GB", gb(*b));
        }
        println!("not scanned {:.1} GB: hidden/protected {:.1} GB, over-counted {:.1} GB", u.gap as f64 / 1e9, gb(u.rest), gb(u.overcount));
        for d in r.denied.iter().filter(|d| d.components().count() <= 6).take(60) {
            println!("  denied {}", d.display());
        }
        assert_eq!(sims.unwrap_or(0), 0);
    }

    #[test]
    fn rescanning_a_folder_fixes_parent_sizes() {
        let d = scratch("scratch-rescan");
        let mut r = scan(d.clone(), Default::default()).unwrap();
        fs::write(d.join("a/b/new.bin"), vec![1u8; 50_000]).unwrap();
        let idx = find_path(&r.tree, &d, &d.join("a/b")).unwrap();
        let fresh = scan_tree(&d.join("a/b"), &Progress::default()).unwrap();
        replace(&mut r.tree, &idx, fresh);
        assert_eq!((r.tree.size, r.tree.files), size_of(&d, None));
        let a = &r.tree.children[resolve(&r.tree, &["a".into()])[0]];
        assert_eq!((a.size, a.files), size_of(&d.join("a"), None));
        assert_eq!(r.tree.children[0].name, "a"); // re-sorted: "a" is now biggest
        fs::remove_dir_all(&d).unwrap();
    }
}
