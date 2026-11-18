//! Per-OS bits: disks, Trash / Recycle Bin, showing files in Finder / Explorer.

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Volume {
    pub name: String,
    pub path: PathBuf,
    pub total: u64,
    pub free: u64,
}

pub fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub const FILE_MANAGER: &str = if cfg!(windows) { "Explorer" } else { "Finder" };
pub const TRASH_NAME: &str = if cfg!(windows) { "Recycle Bin" } else { "Trash" };

// ---------- disks ----------

#[cfg(unix)]
pub fn space(path: &Path) -> Option<(u64, u64)> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let frsize = st.f_frsize as u64;
    Some((st.f_blocks as u64 * frsize, st.f_bavail as u64 * frsize))
}

#[cfg(target_os = "macos")]
fn read_only(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return true };
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    unsafe { libc::statvfs(c.as_ptr(), &mut st) == 0 && st.f_flag & libc::ST_RDONLY != 0 }
}

#[cfg(windows)]
pub fn space(path: &Path) -> Option<(u64, u64)> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let w = wide(path.as_os_str());
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) };
    (ok != 0).then_some((total, avail))
}

#[cfg(target_os = "macos")]
pub fn volumes() -> Vec<Volume> {
    let mut out = Vec::new();
    if let Some((total, free)) = space(Path::new("/")) {
        out.push(Volume { name: "Main disk".into(), path: "/".into(), total, free });
    }
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        for e in entries.flatten() {
            let p = e.path();
            // The startup disk also appears here as a link back to "/".
            if std::fs::symlink_metadata(&p).map_or(true, |m| m.file_type().is_symlink()) {
                continue;
            }
            if let Some((total, free)) = space(&p) {
                // Skip installer images (read-only) and system volumes that share the main disk.
                let shares_main = out.first().map_or(false, |m: &Volume| m.total == total && m.free == free);
                if total > 0 && !shares_main && !read_only(&p) {
                    let name = e.file_name().to_string_lossy().to_string();
                    out.push(Volume { name, path: p, total, free });
                }
            }
        }
    }
    out
}

// ---------- mounts (macOS): what a scan can't or shouldn't walk into ----------

#[cfg(target_os = "macos")]
struct Mount {
    path: PathBuf,
    /// Volumes that share disk space: an APFS container ("/dev/disk3"), or the volume's own
    /// device for anything else. None for things that aren't disks (devfs, autofs, …).
    group: Option<String>,
    /// The filesystem's device id, as files on it report it (st_dev).
    dev: u32,
}

/// Space one volume uses (`df` reads it the same way). statfs's counts are for the whole
/// shared APFS disk, so they can't tell the volumes apart.
#[cfg(target_os = "macos")]
fn volume_used(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut req: libc::attrlist = unsafe { std::mem::zeroed() };
    req.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    req.volattr = libc::ATTR_VOL_INFO | libc::ATTR_VOL_SPACEUSED;
    #[repr(C, packed(4))]
    struct Reply {
        length: u32,
        used: libc::off_t,
    }
    let mut reply = Reply { length: 0, used: 0 };
    let size = std::mem::size_of::<Reply>();
    let ok = unsafe { libc::getattrlist(c.as_ptr(), (&mut req as *mut libc::attrlist).cast(), (&mut reply as *mut Reply).cast(), size, 0) } == 0;
    (ok && reply.length as usize >= size).then(|| reply.used.max(0) as u64)
}

#[cfg(target_os = "macos")]
fn mounts() -> Vec<Mount> {
    use std::ffi::CStr;
    // getfsstat with our own buffer (getmntinfo's buffer is shared and not thread-safe).
    let n = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if n <= 0 {
        return Vec::new();
    }
    let mut buf: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; n as usize + 8];
    let bytes = (buf.len() * std::mem::size_of::<libc::statfs>()) as libc::c_int;
    let n = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    let text = |a: &[libc::c_char]| unsafe { CStr::from_ptr(a.as_ptr()) }.to_string_lossy().into_owned();
    buf[..n.max(0) as usize]
        .iter()
        .map(|s| {
            let device = text(&s.f_mntfromname);
            let group = group_of(&device, &text(&s.f_fstypename));
            // Read from the mount table itself: no need to touch the mount (a stale network
            // share could hang a stat).
            Mount { path: PathBuf::from(text(&s.f_mntonname)), group, dev: fsid_dev(s.f_fsid) }
        })
        .collect()
}

/// The APFS container (whole disk) a volume lives in: "/dev/disk3s1s1" -> "/dev/disk3".
/// None for things that aren't disks (devfs, autofs, file providers).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn container(device: &str) -> Option<&str> {
    let digits = device.strip_prefix("/dev/disk")?.bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0).then(|| &device[..9 + digits])
}

/// The device id part of a filesystem id (libc keeps its fields private; it's two i32s).
#[cfg(target_os = "macos")]
fn fsid_dev(id: libc::fsid_t) -> u32 {
    let parts: [i32; 2] = unsafe { std::mem::transmute(id) };
    parts[0] as u32
}

/// Only APFS volumes share their disk's space; separate partitions (disk4s1, disk4s2) don't.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn group_of(device: &str, fs_type: &str) -> Option<String> {
    if fs_type == "apfs" {
        container(device).map(str::to_string)
    } else {
        device.starts_with("/dev/").then(|| device.to_string())
    }
}

#[cfg(target_os = "macos")]
fn mount_of<'a>(path: &Path, ms: &'a [Mount]) -> Option<&'a Mount> {
    ms.iter().filter(|m| path.starts_with(&m.path)).max_by_key(|m| m.path.as_os_str().len())
}

/// Device ids of every mounted filesystem that isn't part of `root`'s disk (e.g. Xcode's
/// simulator images under /Library/Developer/CoreSimulator/Volumes). A scan stops at folders
/// on these devices, however it reaches them (firmlinks give some mounts several paths).
#[cfg(target_os = "macos")]
pub fn foreign_devices(root: &Path) -> Vec<u64> {
    use std::os::unix::fs::MetadataExt;
    let ms = mounts();
    let Some(home) = mount_of(root, &ms) else { return Vec::new() };
    let root_dev = std::fs::metadata(root).map(|m| m.dev() as u32).ok();
    ms.iter()
        .filter(|m| home.group.is_none() || m.group != home.group)
        .map(|m| m.dev)
        .filter(|d| Some(*d) != root_dev)
        .map(u64::from)
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub fn foreign_devices(_root: &Path) -> Vec<u64> {
    Vec::new()
}

/// Other volumes on the same disk as `root` that a scan of `root` never walks
/// (macOS: Preboot, VM swap, Recovery, Update), with the space each uses.
#[cfg(target_os = "macos")]
pub fn hidden_volumes(root: &Path) -> Vec<(String, u64)> {
    let ms = mounts();
    let Some(home) = mount_of(root, &ms) else { return Vec::new() };
    let Some(group) = &home.group else { return Vec::new() };
    ms.iter()
        .filter(|m| m.path != home.path && m.group.as_ref() == Some(group))
        // The Data volume is reached from "/" through firmlinks (/Users, /Library, …).
        .filter(|m| !(root == Path::new("/") && m.path == Path::new("/System/Volumes/Data")))
        .filter(|m| !m.path.starts_with(root) || crate::scan::skipped(&m.path))
        .map(|m| {
            let name = m.path.file_name().map_or_else(|| m.path.to_string_lossy(), |n| n.to_string_lossy()).to_string();
            let name = match name.as_str() {
                "VM" => "VM (memory swap & sleep image)".to_string(),
                "Preboot" => "Preboot (startup files)".to_string(),
                "Update" => "Update (system updates)".to_string(),
                _ => name,
            };
            (name, volume_used(&m.path).unwrap_or(0))
        })
        .filter(|(_, used)| *used > 0)
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub fn hidden_volumes(_root: &Path) -> Vec<(String, u64)> {
    Vec::new()
}

/// Number of local Time Machine snapshots on the disk mounted at `volume` (None if unknown).
#[cfg(target_os = "macos")]
pub fn local_snapshots(volume: &Path) -> Option<usize> {
    let out = Command::new("tmutil").arg("listlocalsnapshots").arg(volume).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).lines().filter(|l| l.starts_with("com.apple")).count())
}

#[cfg(not(target_os = "macos"))]
pub fn local_snapshots(_volume: &Path) -> Option<usize> {
    None
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn volumes() -> Vec<Volume> {
    space(Path::new("/"))
        .map(|(total, free)| vec![Volume { name: "Main disk".into(), path: "/".into(), total, free }])
        .unwrap_or_default()
}

#[cfg(windows)]
pub fn volumes() -> Vec<Volume> {
    use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .filter_map(|i| {
            let letter = (b'A' + i) as char;
            let path = PathBuf::from(format!("{letter}:\\"));
            let (total, free) = space(&path)?;
            (total > 0).then(|| Volume { name: format!("Drive {letter}:"), path, total, free })
        })
        .collect()
}

// ---------- show / open ----------

pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut arg = std::ffi::OsString::from("/select,\"");
        arg.push(path.as_os_str());
        arg.push("\"");
        let _ = Command::new("explorer").raw_arg(arg).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
}

pub fn open(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(path).spawn();
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = Command::new("xdg-open").arg(path).spawn();
}

// ---------- Trash ----------

#[cfg(windows)]
fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// The error `move_to_trash` returns when Windows deleted an item for good instead of
/// recycling it. Callers must show it and stop; it is never just a skipped item.
pub const DELETED_NOT_RECYCLED: &str =
    "Windows deleted an item permanently instead of moving it to the Recycle Bin. Disk Sweeper stopped.";

/// What Windows says about one drive's Recycle Bin, gathered before moving an item there.
#[cfg_attr(not(windows), allow(dead_code))]
pub struct RecycleFacts {
    /// A fixed local drive (not removable, network, optical or RAM disk).
    pub fixed: bool,
    /// SHQueryRecycleBinW answered for the drive.
    pub bin_answers: bool,
    /// The "don't move files to the Recycle Bin" policy is on.
    pub no_recycle_policy: bool,
    /// The drive's "remove files immediately" setting (None = couldn't read it).
    pub nuke_on_delete: Option<u32>,
    /// The drive's Recycle Bin size limit in MB (None = couldn't read it).
    pub max_capacity_mb: Option<u32>,
    /// A size limit set by policy, in bytes.
    pub policy_cap: Option<u64>,
    /// A setting couldn't be read for a reason other than "not set" (e.g. access denied).
    pub unreadable: bool,
    /// Bytes the item takes (None = some part of it couldn't be measured).
    pub size: Option<u64>,
}

/// A registry DWORD read: the value, None when it simply isn't set (ERROR_FILE_NOT_FOUND),
/// or Err for any other failure, which callers must treat as "unknown", never as "not set".
#[cfg_attr(not(windows), allow(dead_code))]
pub fn reg_dword_result(code: u32, data: u32) -> Result<Option<u32>, u32> {
    const ERROR_FILE_NOT_FOUND: u32 = 2;
    match code {
        0 => Ok(Some(data)),
        ERROR_FILE_NOT_FOUND => Ok(None),
        e => Err(e),
    }
}

/// Logical size of a file or folder, failing if any part can't be read (no guessing).
/// Links and junctions count as themselves and aren't followed.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn strict_size(path: &Path) -> std::io::Result<u64> {
    let md = std::fs::symlink_metadata(path)?;
    if !md.is_dir() {
        return Ok(md.len());
    }
    let mut total = 0;
    for e in std::fs::read_dir(path)? {
        total += strict_size(&e?.path())?;
    }
    Ok(total)
}

/// Ok only when everything says the item will land in the Recycle Bin; anything unknown refuses.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn recycle_verdict(f: &RecycleFacts) -> Result<(), String> {
    let fix = "It was left where it is; delete it in Explorer if you're sure.";
    if !f.fixed {
        return Err(format!("This drive has no Recycle Bin (removable or network drive). {fix}"));
    }
    if !f.bin_answers {
        return Err(format!("Windows didn't confirm this drive has a Recycle Bin. {fix}"));
    }
    if f.unreadable {
        return Err(format!("Couldn't read this drive's Recycle Bin settings or policies. {fix}"));
    }
    if f.no_recycle_policy || f.nuke_on_delete.map_or(false, |n| n != 0) {
        return Err(format!("The Recycle Bin is set to delete files immediately. {fix}"));
    }
    let (Some(mb), Some(_)) = (f.max_capacity_mb, f.nuke_on_delete) else {
        return Err(format!(
            "Couldn't read this drive's Recycle Bin settings (opening Recycle Bin › Properties once sets them up). {fix}"
        ));
    };
    let Some(size) = f.size else {
        return Err(format!("Couldn't measure all of it to check it fits in the Recycle Bin. {fix}"));
    };
    let cap = (mb as u64 * 1024 * 1024).min(f.policy_cap.unwrap_or(u64::MAX));
    if size >= cap {
        return Err(format!("It's too big for this drive's Recycle Bin. {fix}"));
    }
    Ok(())
}

/// Move a file or folder to the Trash / Recycle Bin so it can be restored.
///
/// Windows can't strictly promise to recycle, so this is defensive: it runs on its own
/// single-threaded-apartment thread (refusing if COM won't set up that way), first checks the
/// drive's Recycle Bin can take the item (refusing when unsure), vetoes any delete Windows
/// announces as non-recycling, and reports `DELETED_NOT_RECYCLED` if one happens anyway.
#[cfg(windows)]
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    path.file_name().ok_or("Can't move a whole disk to the Recycle Bin")?;
    let p = path.to_path_buf();
    std::thread::spawn(move || recycle_on_sta_thread(&p))
        .join()
        .unwrap_or_else(|_| Err("Moving to the Recycle Bin failed unexpectedly; nothing was changed.".into()))
}

#[cfg(windows)]
fn recycle_facts(path: &Path) -> RecycleFacts {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW};
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};
    use windows_sys::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
    const DRIVE_FIXED: u32 = 3;
    // Missing → Ok(None); any other failure flags the facts unreadable (so the move is refused).
    fn dword(f: &mut RecycleFacts, root: HKEY, key: &str, value: &str) -> Option<u32> {
        let (k, v) = (wide(key.as_ref()), wide(value.as_ref()));
        let mut data = 0u32;
        let mut len = 4u32;
        let r = unsafe {
            RegGetValueW(root, k.as_ptr(), v.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), &mut data as *mut u32 as _, &mut len)
        };
        reg_dword_result(r, data).unwrap_or_else(|_| {
            f.unreadable = true;
            None
        })
    }
    let mut f = RecycleFacts {
        fixed: false,
        bin_answers: false,
        no_recycle_policy: false,
        nuke_on_delete: None,
        max_capacity_mb: None,
        policy_cap: None,
        unreadable: false,
        size: strict_size(path).ok(),
    };
    let w = wide(path.as_os_str());
    let mut root = [0u16; 512];
    if unsafe { GetVolumePathNameW(w.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return f;
    }
    f.fixed = unsafe { GetDriveTypeW(root.as_ptr()) } == DRIVE_FIXED;
    let mut info: SHQUERYRBINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHQUERYRBINFO>() as u32;
    f.bin_answers = unsafe { SHQueryRecycleBinW(root.as_ptr(), &mut info) } == 0;
    let policies = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";
    for h in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        if dword(&mut f, h, policies, "NoRecycleFiles").map_or(false, |v| v != 0) {
            f.no_recycle_policy = true;
        }
    }
    // A size policy can be set per user and per machine: the smaller one wins.
    let pcts: Vec<u32> =
        [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE].into_iter().filter_map(|h| dword(&mut f, h, policies, "RecycleBinSize")).collect();
    if let Some(&pct) = pcts.iter().min() {
        let root_path = PathBuf::from(String::from_utf16_lossy(&root[..root.iter().position(|&c| c == 0).unwrap_or(0)]));
        f.policy_cap = Some(space(&root_path).map_or(0, |(total, _)| total / 100 * pct as u64));
    }
    // Per-drive settings live under the volume's GUID: "\\?\Volume{GUID}\" → "{GUID}".
    let mut vol = [0u16; 64];
    if unsafe { GetVolumeNameForVolumeMountPointW(root.as_ptr(), vol.as_mut_ptr(), vol.len() as u32) } != 0 {
        let name = String::from_utf16_lossy(&vol[..vol.iter().position(|&c| c == 0).unwrap_or(0)]);
        if let (Some(a), Some(b)) = (name.find('{'), name.rfind('}')) {
            let key = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{}", &name[a..=b]);
            f.max_capacity_mb = dword(&mut f, HKEY_CURRENT_USER, &key, "MaxCapacity");
            // Only read once the drive's settings exist; an absent value means the default (recycle).
            if f.max_capacity_mb.is_some() {
                f.nuke_on_delete = Some(dword(&mut f, HKEY_CURRENT_USER, &key, "NukeOnDelete").unwrap_or(0));
            }
        }
    }
    f
}

#[cfg(windows)]
fn recycle_on_sta_thread(path: &Path) -> Result<(), String> {
    use std::cell::Cell;
    use std::rc::Rc;
    use windows::core::{implement, Result as WinResult, HRESULT, PCWSTR};
    use windows::Win32::Foundation::E_ABORT;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows::Win32::UI::Shell::{
        FileOperation, IFileOperation, IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem,
        SHCreateItemFromParsingName, FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NO_CONNECTED_ELEMENTS,
        FOF_NO_UI, TSF_DELETE_RECYCLE_IF_POSSIBLE,
    };

    #[implement(IFileOperationProgressSink)]
    struct Sink {
        /// Windows was about to delete without recycling, and the sink said no.
        refused: Rc<Cell<bool>>,
        /// A delete went through but nothing landed in the Recycle Bin.
        permanent: Rc<Cell<bool>>,
    }

    #[allow(non_snake_case)]
    impl IFileOperationProgressSink_Impl for Sink_Impl {
        fn PreDeleteItem(&self, dwflags: u32, _item: Option<&IShellItem>) -> WinResult<()> {
            if dwflags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
                self.refused.set(true);
                return Err(E_ABORT.into());
            }
            Ok(())
        }
        fn PostDeleteItem(&self, _f: u32, _item: Option<&IShellItem>, hr: HRESULT, recycled: Option<&IShellItem>) -> WinResult<()> {
            if hr.is_ok() && recycled.is_none() {
                self.permanent.set(true);
                return Err(E_ABORT.into()); // stop anything else in this operation
            }
            Ok(())
        }
        fn StartOperations(&self) -> WinResult<()> { Ok(()) }
        fn FinishOperations(&self, _hr: HRESULT) -> WinResult<()> { Ok(()) }
        fn PreRenameItem(&self, _f: u32, _i: Option<&IShellItem>, _n: &PCWSTR) -> WinResult<()> { Ok(()) }
        fn PostRenameItem(&self, _f: u32, _i: Option<&IShellItem>, _n: &PCWSTR, _hr: HRESULT, _c: Option<&IShellItem>) -> WinResult<()> { Ok(()) }
        fn PreMoveItem(&self, _f: u32, _i: Option<&IShellItem>, _d: Option<&IShellItem>, _n: &PCWSTR) -> WinResult<()> { Ok(()) }
        fn PostMoveItem(&self, _f: u32, _i: Option<&IShellItem>, _d: Option<&IShellItem>, _n: &PCWSTR, _hr: HRESULT, _c: Option<&IShellItem>) -> WinResult<()> { Ok(()) }
        fn PreCopyItem(&self, _f: u32, _i: Option<&IShellItem>, _d: Option<&IShellItem>, _n: &PCWSTR) -> WinResult<()> { Ok(()) }
        fn PostCopyItem(&self, _f: u32, _i: Option<&IShellItem>, _d: Option<&IShellItem>, _n: &PCWSTR, _hr: HRESULT, _c: Option<&IShellItem>) -> WinResult<()> { Ok(()) }
        fn PreNewItem(&self, _f: u32, _d: Option<&IShellItem>, _n: &PCWSTR) -> WinResult<()> { Ok(()) }
        fn PostNewItem(&self, _f: u32, _d: Option<&IShellItem>, _n: &PCWSTR, _t: &PCWSTR, _a: u32, _hr: HRESULT, _c: Option<&IShellItem>) -> WinResult<()> { Ok(()) }
        fn UpdateProgress(&self, _total: u32, _done: u32) -> WinResult<()> { Ok(()) }
        fn ResetTimer(&self) -> WinResult<()> { Ok(()) }
        fn PauseTimer(&self) -> WinResult<()> { Ok(()) }
        fn ResumeTimer(&self) -> WinResult<()> { Ok(()) }
    }

    // IFileOperation needs a single-threaded apartment. This thread is new, so anything other
    // than a clean STA setup (RPC_E_CHANGED_MODE included) means something is off: refuse.
    if unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.is_err() {
        return Err("Couldn't prepare Windows' file operation, so nothing was moved.".into());
    }
    let outcome = (|| {
        recycle_verdict(&recycle_facts(path))?;
        let wide_path = wide(path.as_os_str());
        let (refused, permanent) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
        let sink: IFileOperationProgressSink = Sink { refused: refused.clone(), permanent: permanent.clone() }.into();
        let result = unsafe {
            (|| -> WinResult<bool> {
                let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
                op.SetOperationFlags(FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOFX_EARLYFAILURE | FOF_NO_CONNECTED_ELEMENTS)?;
                let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(wide_path.as_ptr()), None)?;
                op.DeleteItem(&item, &sink)?;
                let performed = op.PerformOperations();
                let aborted = op.GetAnyOperationsAborted().map_or(true, |b| b.as_bool());
                performed.map(|()| !aborted)
            })()
        };
        drop(sink);
        if permanent.get() {
            return Err(DELETED_NOT_RECYCLED.to_string());
        }
        if refused.get() {
            return Err("Windows said it couldn't recycle this, so it was left where it is.".into());
        }
        match result {
            Ok(true) if !path.exists() => Ok(()),
            Ok(_) => Err("Windows didn't move it to the Recycle Bin; it was left where it is.".into()),
            Err(e) => Err(format!("Windows couldn't move it to the Recycle Bin ({})", e.message())),
        }
    })();
    unsafe { CoUninitialize() };
    outcome
}

#[cfg(unix)]
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    path.file_name().ok_or("Can't move a whole disk to the Trash")?;
    let mut dirs = vec![trash_dir()];
    // Files on other disks go to that disk's own Trash.
    #[cfg(target_os = "macos")]
    if let Ok(rest) = path.strip_prefix("/Volumes") {
        if let Some(vol) = rest.components().next() {
            let uid = unsafe { libc::getuid() };
            dirs.insert(0, Path::new("/Volumes").join(vol).join(".Trashes").join(uid.to_string()));
        }
    }
    let mut last_err = String::new();
    for dir in dirs {
        match move_into(path, &dir) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = e.to_string(),
        }
    }
    if is_protected(path) {
        return Err(PROTECTED_TEXT.into());
    }
    if last_err.contains("ermission denied") || last_err.contains("not permitted") {
        return Err("The system didn't allow moving it: it belongs to the system or to another user.".into());
    }
    Err(format!("Couldn't move it to the Trash: {last_err}"))
}

/// Move `path` into `dir`, renaming to "name 2", "name 3"… so nothing there is ever replaced.
#[cfg(unix)]
pub fn move_into(path: &Path, dir: &Path) -> std::io::Result<()> {
    let name = path.file_name().unwrap_or_default();
    let stem = Path::new(name).file_stem().unwrap_or(name).to_string_lossy().to_string();
    let ext = Path::new(name).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let _ = std::fs::create_dir_all(dir);
    for n in 1..10_000 {
        let target = if n == 1 { dir.join(name) } else { dir.join(format!("{stem} {n}{ext}")) };
        match rename_new(path, &target) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            r => return r,
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "too many files with this name"))
}

/// Rename that fails instead of replacing an existing target.
#[cfg(target_os = "macos")]
fn rename_new(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let f = CString::new(from.as_os_str().as_bytes())?;
    let t = CString::new(to.as_os_str().as_bytes())?;
    if unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn rename_new(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    std::fs::rename(from, to)
}

#[cfg(unix)]
pub fn trash_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join(".Trash")
    } else {
        home().join(".local/share/Trash/files")
    }
}

/// Whether this disk is the one that holds the Trash / Recycle Bin.
pub fn holds_trash(v: &Volume) -> bool {
    if cfg!(windows) {
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()).to_uppercase();
        v.path.to_string_lossy().to_uppercase().starts_with(&sys)
    } else {
        v.path == Path::new("/")
    }
}

/// The folder deleted files go to on the main disk.
pub fn trash_folder() -> Option<PathBuf> {
    #[cfg(unix)]
    return Some(trash_dir());
    #[cfg(windows)]
    return std::env::var_os("SystemDrive").map(|d| PathBuf::from(d).join("\\$Recycle.Bin"));
}

/// Size of the Trash / Recycle Bin. None when the system won't let us look.
#[cfg(unix)]
pub fn trash_size() -> Option<(u64, u64)> {
    let dir = trash_dir();
    std::fs::read_dir(&dir).ok()?;
    // What emptying it would give back (a hard link whose other name lives on frees nothing).
    Some((crate::scan::reclaimable(&dir), crate::scan::size_of(&dir, None).1))
}

#[cfg(windows)]
pub fn trash_size() -> Option<(u64, u64)> {
    use windows_sys::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
    let mut info: SHQUERYRBINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHQUERYRBINFO>() as u32;
    let r = unsafe { SHQueryRecycleBinW(std::ptr::null(), &mut info) };
    (r == 0).then_some((info.i64Size as u64, info.i64NumItems as u64))
}

#[cfg(unix)]
pub fn empty_trash() -> Result<(), String> {
    let dir = trash_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(es) => es,
        // macOS may not let apps look inside the Trash; Finder can still empty it.
        #[cfg(target_os = "macos")]
        Err(_) => {
            let ok = Command::new("osascript")
                .args(["-e", "tell application \"Finder\" to empty trash"])
                .status()
                .map_or(false, |s| s.success());
            return if ok { Ok(()) } else { Err("macOS didn't allow emptying the Trash.".into()) };
        }
        #[cfg(not(target_os = "macos"))]
        Err(e) => return Err(e.to_string()),
    };
    for e in entries.flatten() {
        let p = e.path();
        let _ = if p.is_dir() && !p.is_symlink() {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
    }
    Ok(())
}

#[cfg(windows)]
pub fn empty_trash() -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::{
        SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND,
    };
    let r = unsafe {
        SHEmptyRecycleBinW(0, std::ptr::null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND)
    };
    // S_OK, or "already empty"
    if r == 0 || r == -2147418113 {
        Ok(())
    } else {
        Err(format!("Windows couldn't empty the Recycle Bin (code {r})"))
    }
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn same_name_never_overwrites() {
        let base = std::env::current_dir().unwrap().join("target/scratch-trash");
        let _ = std::fs::remove_dir_all(&base);
        let bin = base.join("bin");
        for (folder, body) in [("a", "first"), ("b", "second"), ("c", "third")] {
            std::fs::create_dir_all(base.join(folder)).unwrap();
            let f = base.join(folder).join("photo.jpg");
            std::fs::write(&f, body).unwrap();
            super::move_into(&f, &bin).unwrap();
        }
        let read = |n: &str| std::fs::read_to_string(bin.join(n)).unwrap();
        assert_eq!((read("photo.jpg"), read("photo 2.jpg"), read("photo 3.jpg")), ("first".into(), "second".into(), "third".into()));
        std::fs::remove_dir_all(&base).unwrap();
    }
}

// ---------- protected files and Xcode simulators (macOS) ----------

pub const PROTECTED_TEXT: &str =
    "macOS protects this file because it's part of the system (System Integrity Protection), so no app can move it to the Trash.";

/// True for files macOS guards with System Integrity Protection.
#[cfg(target_os = "macos")]
pub fn is_protected(path: &Path) -> bool {
    use std::os::macos::fs::MetadataExt;
    const SF_RESTRICTED: u32 = 0x0008_0000;
    std::fs::symlink_metadata(path).map_or(false, |m| m.st_flags() & SF_RESTRICTED != 0)
}

#[cfg(not(target_os = "macos"))]
pub fn is_protected(_path: &Path) -> bool {
    false
}

/// A downloaded Xcode simulator (e.g. "iOS 26.3.1 Simulator"). macOS keeps these in protected
/// folders; only Xcode's own simulator tool may remove them.
#[derive(Clone)]
pub struct SimRuntime {
    pub id: String,
    pub name: String,
    /// The whole downloaded package on disk.
    pub folder: PathBuf,
    pub size: u64,
}

/// The simulator a file belongs to, if any (asks `xcrun simctl`).
#[cfg(target_os = "macos")]
pub fn sim_runtime_for(path: &Path) -> Option<SimRuntime> {
    let p = path.to_string_lossy();
    if !(p.contains("SimulatorRuntime") || p.starts_with("/Library/Developer/CoreSimulator")) {
        return None;
    }
    let out = Command::new("xcrun").args(["simctl", "runtime", "list", "-j"]).output().ok()?;
    let list: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    list.as_object()?.iter().find_map(|(id, r)| {
        if r["deletable"].as_bool() == Some(false) {
            return None;
        }
        let image = PathBuf::from(r["path"].as_str()?);
        // Downloaded simulators live in ".../<hash>.asset/AssetData/..." — remove the whole package.
        let folder = r["parentMountPath"]
            .as_str()
            .map(Path::new)
            .and_then(|m| if m.ends_with("AssetData") { m.parent() } else { Some(m) })
            .filter(|f| image.starts_with(f))
            .map_or(image.clone(), Path::to_path_buf);
        if !path.starts_with(&folder) {
            return None;
        }
        let platform = r["runtimeIdentifier"].as_str().and_then(|s| s.rsplit('.').next()?.split('-').next()).unwrap_or("Simulator");
        let version = r["version"].as_str().unwrap_or_default();
        Some(SimRuntime {
            id: id.clone(),
            name: format!("{platform} {version} Simulator"),
            folder,
            size: r["sizeBytes"].as_u64().unwrap_or(0),
        })
    })
}

#[cfg(not(target_os = "macos"))]
pub fn sim_runtime_for(_path: &Path) -> Option<SimRuntime> {
    None
}

pub fn delete_sim_runtime(id: &str) -> Result<(), String> {
    let out = Command::new("xcrun")
        .args(["simctl", "runtime", "delete", id])
        .output()
        .map_err(|e| format!("Couldn't run Xcode's simulator tool: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(format!("Xcode's simulator tool couldn't remove it. {why}"))
    }
}

#[cfg(test)]
mod recycle_tests {
    use super::*;

    fn ok_facts() -> RecycleFacts {
        RecycleFacts {
            fixed: true,
            bin_answers: true,
            no_recycle_policy: false,
            nuke_on_delete: Some(0),
            max_capacity_mb: Some(10_000),
            policy_cap: None,
            unreadable: false,
            size: Some(5_000_000),
        }
    }

    #[test]
    fn recycles_only_when_everything_checks_out() {
        assert!(recycle_verdict(&ok_facts()).is_ok());
        let refused = |f: RecycleFacts| assert!(recycle_verdict(&f).is_err());
        refused(RecycleFacts { fixed: false, ..ok_facts() });
        refused(RecycleFacts { bin_answers: false, ..ok_facts() });
        refused(RecycleFacts { no_recycle_policy: true, ..ok_facts() });
        refused(RecycleFacts { nuke_on_delete: Some(1), ..ok_facts() });
        // Unknown settings refuse rather than guess.
        refused(RecycleFacts { nuke_on_delete: None, ..ok_facts() });
        refused(RecycleFacts { max_capacity_mb: None, ..ok_facts() });
        // Too big for the bin (its own limit, or a policy limit).
        refused(RecycleFacts { size: Some(10_000 * 1024 * 1024), ..ok_facts() });
        refused(RecycleFacts { policy_cap: Some(1_000_000), ..ok_facts() });
        // A setting that couldn't be read, or an item that couldn't be fully measured.
        refused(RecycleFacts { unreadable: true, ..ok_facts() });
        refused(RecycleFacts { size: None, ..ok_facts() });
    }

    #[test]
    fn registry_errors_are_not_treated_as_unset() {
        assert_eq!(reg_dword_result(0, 7), Ok(Some(7)));
        assert_eq!(reg_dword_result(2, 0), Ok(None)); // ERROR_FILE_NOT_FOUND: not set
        assert_eq!(reg_dword_result(5, 0), Err(5)); // ERROR_ACCESS_DENIED
        assert_eq!(reg_dword_result(13, 0), Err(13)); // ERROR_INVALID_DATA (wrong type)
    }

    #[cfg(unix)]
    #[test]
    fn strict_size_fails_on_anything_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::current_dir().unwrap().join("target/scratch-strict-size");
        let _ = std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("locked")).unwrap();
        std::fs::write(dir.join("a.bin"), vec![0u8; 1000]).unwrap();
        std::fs::write(dir.join("locked/b.bin"), vec![0u8; 500]).unwrap();
        assert_eq!(strict_size(&dir).unwrap(), 1500);
        std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let locked = strict_size(&dir);
        std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(locked.is_err(), "an unreadable folder must fail, not count as zero");
        assert!(strict_size(&dir.join("missing")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod mount_tests {
    #[test]
    fn container_of_device() {
        assert_eq!(super::container("/dev/disk3s1s1"), Some("/dev/disk3"));
        assert_eq!(super::container("/dev/disk12s5"), Some("/dev/disk12"));
        assert_eq!(super::container("devfs"), None);
        assert_eq!(super::container("map auto_home"), None);
        // Partitions of one physical disk are separate; APFS volumes share their container.
        assert_eq!(super::group_of("/dev/disk4s1", "hfs").as_deref(), Some("/dev/disk4s1"));
        assert_ne!(super::group_of("/dev/disk4s1", "hfs"), super::group_of("/dev/disk4s2", "msdos"));
        assert_eq!(super::group_of("/dev/disk3s5", "apfs"), super::group_of("/dev/disk3s1s1", "apfs"));
        assert_eq!(super::group_of("map auto_home", "autofs"), None);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod mac_tests {
    /// Machine-specific: shows what a scan of "/" skips and which volumes it can't see.
    #[test]
    #[ignore]
    fn mounts_on_this_mac() {
        let root = std::path::Path::new("/");
        use std::os::unix::fs::MetadataExt;
        let dev = |p: &str| std::fs::metadata(p).unwrap().dev();
        println!("hidden: {:?}", super::hidden_volumes(root));
        println!("snapshots: {:?}", super::local_snapshots(root));
        // Same answer whichever way the disk is reached (firmlink or the Data volume's own path).
        for r in ["/", "/System/Volumes/Data/Library", "/Library"] {
            let foreign = super::foreign_devices(std::path::Path::new(r));
            for sim in std::fs::read_dir("/Library/Developer/CoreSimulator/Volumes").unwrap().flatten() {
                assert!(foreign.contains(&sim.metadata().unwrap().dev()), "{r}: {:?}", sim.path());
            }
            assert!(!foreign.contains(&dev("/nix")) && !foreign.contains(&dev("/Users")) && !foreign.contains(&dev("/")));
        }
        assert!(super::hidden_volumes(root).iter().all(|v| v.1 < 100_000_000_000));
    }

    /// Machine-specific: run with `cargo test -- --ignored` on a Mac with Xcode simulators.
    #[test]
    #[ignore]
    fn finds_simulator_for_protected_dmg() {
        let dir = std::path::Path::new("/System/Library/AssetsV2/com_apple_MobileAsset_iOSSimulatorRuntime");
        let pkg = std::fs::read_dir(dir).unwrap().flatten().find(|e| e.path().extension().map_or(false, |x| x == "asset")).unwrap().path();
        let dmg = std::fs::read_dir(pkg.join("AssetData/Restore")).unwrap().flatten().next().unwrap().path();
        assert!(super::is_protected(&dmg));
        let rt = super::sim_runtime_for(&dmg).expect("simulator found");
        println!("{} {} {} {}", rt.id, rt.name, rt.folder.display(), rt.size);
        assert_eq!(rt.folder, pkg);
        assert!(super::sim_runtime_for(std::path::Path::new("/System/Library/AssetsV2")).is_none());
    }
}

// ---------- first-run setup ----------

/// Whether the app has Full Disk Access, found by trying folders macOS guards with it.
/// None when none of them exist (can't tell; treated as fine).
#[cfg(target_os = "macos")]
pub fn full_disk_access() -> Option<bool> {
    let home = home();
    ["Library/Safari", "Library/Mail", "Library/Messages", "Library/Application Support/com.apple.TCC"]
        .iter()
        .map(|p| home.join(p))
        .find(|p| std::fs::symlink_metadata(p).is_ok())
        .map(|p| std::fs::read_dir(p).is_ok())
}

#[cfg(not(target_os = "macos"))]
pub fn full_disk_access() -> Option<bool> {
    None
}

pub const FULL_DISK_ACCESS_SETTINGS: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// The .app bundle this program runs from (None when run outside one, e.g. `cargo run`).
#[cfg(target_os = "macos")]
pub fn app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().find(|p| p.extension().map_or(false, |e| e == "app")).map(Path::to_path_buf)
}

#[cfg(not(target_os = "macos"))]
pub fn app_bundle() -> Option<PathBuf> {
    None
}

/// Running as a bundle somewhere other than /Applications (e.g. Downloads, a disk image).
pub fn outside_applications() -> Option<PathBuf> {
    app_bundle().filter(|b| !b.starts_with("/Applications"))
}

/// Copy the running bundle into /Applications (an older copy there goes to the Trash first),
/// then move this one to the Trash. Returns the new location.
#[cfg(target_os = "macos")]
pub fn move_to_applications(bundle: &Path) -> Result<PathBuf, String> {
    let name = bundle.file_name().ok_or("Couldn't tell the app's name")?;
    let target = Path::new("/Applications").join(name);
    if target.exists() {
        move_to_trash(&target)?;
    }
    let ok = Command::new("ditto").arg(bundle).arg(&target).status().map_or(false, |s| s.success());
    if !ok {
        return Err("Couldn't copy the app into Applications.".into());
    }
    // The copy is what matters; leaving the old one behind is harmless.
    let _ = move_to_trash(bundle);
    Ok(target)
}

#[cfg(not(target_os = "macos"))]
pub fn move_to_applications(_bundle: &Path) -> Result<PathBuf, String> {
    Err("Only needed on macOS".into())
}

/// Start a fresh copy of the app (from `bundle` if given) and quit this one.
pub fn relaunch(bundle: Option<&Path>) -> ! {
    #[cfg(target_os = "macos")]
    {
        if let Some(b) = bundle.map(Path::to_path_buf).or_else(app_bundle) {
            // Wait for this copy to quit so the new one isn't just brought to the front.
            let script = format!("sleep 0.5; open -n '{}'", b.to_string_lossy().replace('\'', "'\\''"));
            let _ = Command::new("sh").args(["-c", &script]).spawn();
            std::process::exit(0);
        }
    }
    let _ = bundle;
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe).spawn();
    }
    std::process::exit(0)
}

/// Windows: running "as administrator", which lets it read more system folders.
#[cfg(windows)]
pub fn elevated() -> bool {
    unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
}

#[cfg(not(windows))]
pub fn elevated() -> bool {
    false
}

// ---------- file sizes ----------

/// How many names (hard links) a file has. None if it can't be read.
#[cfg(unix)]
pub fn link_count(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).ok().map(|m| m.nlink())
}

#[cfg(windows)]
pub fn link_count(path: &Path) -> Option<u64> {
    file_info(path).map(|i| i.links)
}

/// What Windows knows about a file once it's opened.
#[cfg(windows)]
pub struct FileInfo {
    /// Bytes really taken on disk (less for sparse/compressed files).
    pub allocated: u64,
    pub links: u64,
    /// Volume serial + file index: the same for every name of the file.
    pub id: (u64, u64),
}

#[cfg(windows)]
pub fn file_info(path: &Path) -> Option<FileInfo> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileStandardInfo, GetFileInformationByHandle, GetFileInformationByHandleEx, BY_HANDLE_FILE_INFORMATION,
        FILE_STANDARD_INFO,
    };
    // Opened through std, which handles paths longer than MAX_PATH. Access 0 = attributes only,
    // so it never blocks other programs using the file.
    let f = std::fs::OpenOptions::new().access_mode(0).share_mode(7).open(path).ok()?;
    let h = f.as_raw_handle() as _;
    let mut by: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let mut std_info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandle(h, &mut by) != 0
            && GetFileInformationByHandleEx(
                h,
                FileStandardInfo,
                (&mut std_info as *mut FILE_STANDARD_INFO).cast(),
                std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
            ) != 0
    };
    ok.then(|| FileInfo {
        allocated: std_info.AllocationSize.max(0) as u64,
        links: by.nNumberOfLinks as u64,
        id: (by.dwVolumeSerialNumber as u64, ((by.nFileIndexHigh as u64) << 32) | by.nFileIndexLow as u64),
    })
}
