//! Made-up disk contents for screenshots (`--screenshot <file> --demo`), so pictures of the app
//! never show a real person's files, folder names or disks.

use crate::platform::Volume;
use crate::scan::{Node, ScanResult, SavedInfo};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

const GB: u64 = 1_000_000_000;
const MB: u64 = 1_000_000;

pub fn volumes() -> Vec<Volume> {
    vec![
        Volume { name: "Macintosh HD".into(), path: "/".into(), total: 994 * GB, free: 465 * GB },
        Volume { name: "Backup".into(), path: "/Volumes/Backup".into(), total: 2000 * GB, free: 1240 * GB },
    ]
}

/// Other volumes in the startup disk's container, which a scan of "/" can't walk.
pub fn hidden_volumes() -> Vec<(String, u64)> {
    vec![("VM".into(), 4 * GB), ("Preboot".into(), 7 * GB), ("System".into(), 11 * GB), ("Recovery".into(), GB)]
}

pub fn saved() -> Vec<SavedInfo> {
    let ago = |h: u64| SystemTime::now() - Duration::from_secs(h * 3600);
    vec![
        SavedInfo { root: "/".into(), scanned_at: ago(26), size: 489 * GB, files: 2_412_880 },
        SavedInfo { root: "/Users/demo/Projects".into(), scanned_at: ago(72), size: 61 * GB, files: 498_310 },
    ]
}

/// (bytes, files) per cleanup category, in `cleanup::categories()` order (extra entries are ignored).
pub fn cleanup_sizes() -> Vec<(u64, u64)> {
    vec![(3_840 * MB, 41_210), (612 * MB, 1_904), (14_300 * MB, 96_455), (920 * MB, 3_118), (5_270 * MB, 37), (2_150 * MB, 212)]
}

fn file(name: &str, size: u64) -> Node {
    Node { name: name.into(), size, files: 1, is_dir: false, children: Vec::new() }
}

/// A folder whose size and file count add up from its children.
fn dir(name: &str, mut children: Vec<Node>) -> Node {
    children.sort_by(|a, b| b.size.cmp(&a.size));
    let size = children.iter().map(|c| c.size).sum();
    let files = children.iter().map(|c| c.files).sum();
    Node { name: name.into(), size, files, is_dir: true, children }
}

/// A folder of many small files, shown only as its totals.
fn blob(name: &str, files: u64, size: u64) -> Node {
    Node { name: name.into(), size, files, is_dir: true, children: Vec::new() }
}

pub fn scan() -> ScanResult {
    let home = dir(
        "demo",
        vec![
            dir(
                "Movies",
                vec![
                    file("Family trip 2025.mov", 38_400 * MB),
                    file("Drone footage raw.mp4", 21_700 * MB),
                    file("Screen recording.mov", 6_120 * MB),
                    blob("Clips", 140, 9 * GB),
                ],
            ),
            dir(
                "Library",
                vec![
                    blob("Caches", 41_210, 3_840 * MB),
                    blob("Logs", 1_904, 612 * MB),
                    dir(
                        "Developer",
                        vec![
                            dir("Xcode", vec![blob("DerivedData", 96_455, 14_300 * MB)]),
                            dir("CoreSimulator", vec![file("iOS Simulator Runtime.dmg", 8_900 * MB), blob("Devices", 22_000, 9 * GB)]),
                        ],
                    ),
                    blob("Application Support", 180_000, 46 * GB),
                    blob("Containers", 95_000, 21 * GB),
                    blob("Mail", 240_000, 12 * GB),
                ],
            ),
            dir(
                "Projects",
                vec![
                    blob("website", 88_000, 3_100 * MB),
                    dir("game-prototype", vec![file("assets.pak", 4_800 * MB), blob("src", 61_000, 6 * GB)]),
                    dir(
                        "ml-experiments",
                        vec![file("model-checkpoint.bin", 7_400 * MB), file("dataset.parquet", 5_900 * MB), blob("runs", 12_000, 5 * GB)],
                    ),
                    blob("photo-tools", 40_000, 2_300 * MB),
                ],
            ),
            dir("Pictures", vec![blob("Photos Library.photoslibrary", 64_000, 88 * GB), blob("Wallpapers", 900, 4 * GB)]),
            dir(
                "Downloads",
                vec![
                    file("Ubuntu-24.04-desktop.iso", 6_100 * MB),
                    file("Installer.dmg", 1_450 * MB),
                    file("old-backup.zip", 9_800 * MB),
                    blob("Unsorted", 630, 5 * GB),
                ],
            ),
            blob("Music", 12_400, 34 * GB),
            dir("Documents", vec![file("Archive 2019.zip", 3_200 * MB), blob("Work", 22_000, 17 * GB)]),
            blob("Desktop", 310, 2 * GB),
            blob(".Trash", 212, 2_150 * MB),
        ],
    );
    let users = dir("Users", vec![home, blob("Shared", 1_200, 3 * GB)]);
    let root = dir(
        "",
        vec![
            users,
            dir(
                "Applications",
                vec![
                    blob("Xcode.app", 410_000, 13 * GB),
                    blob("Photo Editor.app", 9_000, 4 * GB),
                    blob("Office Suite.app", 30_000, 8 * GB),
                    blob("Utilities", 220_000, 19 * GB),
                ],
            ),
            blob("Library", 310_000, 24 * GB),
            dir("private", vec![file("sleepimage", 2 * GB), blob("var", 88_000, 10 * GB)]),
            blob("opt", 96_000, 6 * GB),
            blob("usr", 61_000, 2 * GB),
        ],
    );
    let denied = vec![PathBuf::from("/Library/Application Support/com.apple.TCC"), PathBuf::from("/private/var/db/example")];
    ScanResult::new("/".into(), root, SystemTime::now() - Duration::from_secs(120), denied)
}
