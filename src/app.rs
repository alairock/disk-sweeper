//! The window: sidebar with disks, Overview / Explore / Biggest files / Cleanup screens.

use crate::cleanup::{self, Category};
use crate::platform::{self, Volume};
use crate::scan::{self, ago, human, Node, Progress, SavedInfo, ScanResult};
use crate::treemap;
use egui::{
    vec2, Align, Align2, Color32, FontId, Layout, Margin, Pos2, Rect, RichText, Rounding, Sense, Stroke, Ui,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

const BG: Color32 = Color32::from_rgb(0x17, 0x19, 0x1f);
const PANEL: Color32 = Color32::from_rgb(0x1f, 0x22, 0x2a);
const CARD: Color32 = Color32::from_rgb(0x27, 0x2b, 0x35);
const LINE: Color32 = Color32::from_rgb(0x34, 0x39, 0x45);
const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe8, 0xee);
const DIM: Color32 = Color32::from_rgb(0x95, 0x9b, 0xaa);
const ACCENT: Color32 = Color32::from_rgb(0x4f, 0x9d, 0xff);
const GOOD: Color32 = Color32::from_rgb(0x3d, 0xc9, 0x8a);
const DANGER: Color32 = Color32::from_rgb(0xf0, 0x5d, 0x5e);

const PALETTE: [Color32; 10] = [
    Color32::from_rgb(0x4f, 0x9d, 0xff),
    Color32::from_rgb(0x9b, 0x6c, 0xf6),
    Color32::from_rgb(0x3d, 0xc9, 0x8a),
    Color32::from_rgb(0xf5, 0xa6, 0x23),
    Color32::from_rgb(0xef, 0x6f, 0x9a),
    Color32::from_rgb(0x2e, 0xc4, 0xd6),
    Color32::from_rgb(0xe8, 0x7a, 0x4a),
    Color32::from_rgb(0x7e, 0xc8, 0x4b),
    Color32::from_rgb(0x6c, 0x7c, 0xf0),
    Color32::from_rgb(0xd9, 0x5f, 0xd0),
];
const FILE_COLOR: Color32 = Color32::from_rgb(0x5a, 0x63, 0x75);

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Overview,
    Explore,
    Biggest,
    Cleanup,
}

enum Confirm {
    /// A file or folder, by full path (looked up again when confirmed, so a refresh
    /// that happens while the dialog is open can't change what gets moved).
    Trash {
        path: PathBuf,
        size: u64,
        /// Part of an Xcode simulator: removed through Xcode's tool instead.
        runtime: Option<platform::SimRuntime>,
        /// macOS-protected; nothing can move it.
        protected: bool,
        /// A file with other hard links: moving this name frees nothing while they remain.
        shared: bool,
    },
    Clean,
    EmptyTrash,
}

enum ScanMsg {
    /// The last saved scan of this place, shown right away.
    Saved(ScanResult),
    /// The fresh scan finished.
    Fresh(ScanResult),
}

struct Scanning {
    root: PathBuf,
    progress: Arc<Progress>,
    rx: Receiver<ScanMsg>,
}

/// Re-reading one folder of the current scan.
struct Partial {
    path: PathBuf,
    /// The folder's new contents and the folders in it that couldn't be read.
    rx: Receiver<Option<(Node, Vec<PathBuf>)>>,
}

/// First-run screens.
#[derive(PartialEq, Clone, Copy)]
enum Setup {
    /// Windows: what the app does.
    Welcome,
    /// macOS: running from outside /Applications.
    Move,
    /// macOS: ask for Full Disk Access.
    Access,
}

struct Shot {
    path: PathBuf,
    frame: u32,
    started: std::time::Instant,
    requested: bool,
}

pub struct App {
    tab: Tab,
    volumes: Vec<Volume>,
    scan: Option<ScanResult>,
    scanning: Option<Scanning>,
    /// Folder names from the scan root to the folder being viewed.
    nav: Vec<String>,
    selected: Option<String>,
    selected_big: Option<PathBuf>,
    partial: Vec<Partial>,
    saved: Vec<SavedInfo>,
    /// Saved scans are off in check mode so the person's data is never touched.
    use_saved: bool,
    /// The scan changed since it was saved.
    dirty: bool,
    folder_input: String,
    confirm: Option<Confirm>,
    /// A note for the person: (text, is_good_news).
    message: Option<(String, bool)>,
    removing: Option<(platform::SimRuntime, Receiver<Result<(), String>>)>,
    /// What's in the Trash / Recycle Bin (None = the system won't say).
    trash_bytes: Option<u64>,
    /// Moved to the Trash since it was last measured.
    trashed_since: u64,
    trash_rx: Option<Receiver<Option<(u64, u64)>>>,
    /// When the disks' free space was last read.
    disks_read: std::time::Instant,
    /// When the scan is of a whole disk: that disk's other volumes the scan can't walk.
    hidden_volumes: Option<Vec<(String, u64)>>,
    /// Local Time Machine snapshots on the startup disk (None = unknown).
    snapshots: Option<usize>,
    cats: Vec<Category>,
    old_days: u32,
    measuring: Option<Receiver<Vec<Option<(u64, u64)>>>>,
    cleaning: Option<Receiver<(u64, u64)>>,
    last_clean: Option<String>,
    cleaned_paths: Vec<PathBuf>,
    shot: Option<Shot>,
    /// First-run screens still to show, in order.
    setup: Vec<Setup>,
    /// Full Disk Access (macOS); None = can't tell or not applicable.
    full_access: Option<bool>,
    /// The person opened the settings page, so offer a relaunch if access doesn't show up.
    opened_settings: bool,
    was_focused: bool,
    /// Screenshot mode with made-up data (see demo.rs): nothing is read from this machine.
    demo: bool,
}

/// Remembers that first-run setup was finished ("done") or skipped ("skipped").
fn setup_file() -> PathBuf {
    let dir = scan::saved_dir();
    dir.parent().map_or(dir.clone(), Path::to_path_buf).join("setup")
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, shot: Option<PathBuf>, shot_tab: Option<String>, demo: bool) -> Self {
        style(&cc.egui_ctx);
        let mut app = App {
            tab: Tab::Overview,
            volumes: platform::volumes(),
            scan: None,
            scanning: None,
            nav: Vec::new(),
            selected: None,
            selected_big: None,
            partial: Vec::new(),
            saved: Vec::new(),
            use_saved: shot.is_none(),
            dirty: false,
            folder_input: platform::home().to_string_lossy().to_string(),
            confirm: None,
            message: None,
            removing: None,
            trash_bytes: None,
            trashed_since: 0,
            trash_rx: None,
            disks_read: std::time::Instant::now(),
            hidden_volumes: None,
            snapshots: None,
            cats: cleanup::categories(),
            old_days: 30,
            measuring: None,
            cleaning: None,
            last_clean: None,
            cleaned_paths: Vec::new(),
            shot: shot.map(|path| Shot { path, frame: 0, started: std::time::Instant::now(), requested: false }),
            setup: Vec::new(),
            full_access: platform::full_disk_access(),
            opened_settings: false,
            was_focused: true,
            demo,
        };
        if app.use_saved && std::fs::read_to_string(setup_file()).is_err() {
            if cfg!(windows) {
                app.setup.push(Setup::Welcome);
            }
            if platform::outside_applications().is_some() {
                app.setup.push(Setup::Move);
            }
            if app.full_access == Some(false) {
                app.setup.push(Setup::Access);
            }
            if app.setup.is_empty() {
                app.finish_setup("done");
            }
        }
        if app.use_saved {
            app.saved = scan::list_saved(&scan::saved_dir());
            app.measure_trash();
        }
        if demo {
            app.volumes = crate::demo::volumes();
            app.folder_input = "/Users/demo".into();
            app.full_access = Some(true);
            app.saved = crate::demo::saved();
            app.scan = Some(crate::demo::scan());
            app.hidden_volumes = Some(crate::demo::hidden_volumes());
            app.snapshots = Some(2);
            app.trash_bytes = Some(2_150_000_000);
            for (c, m) in app.cats.iter_mut().zip(crate::demo::cleanup_sizes()) {
                c.measured = Some(Some(m));
            }
            app.tab = Tab::Explore;
        } else if app.shot.is_some() {
            // Show a filled-in Explore screen using a harmless folder (the current one).
            let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            app.start_scan(here);
        }
        if app.shot.is_some() {
            // Picture another screen (check mode only; nothing is measured or deleted).
            match shot_tab.as_deref() {
                Some("overview") => app.tab = Tab::Overview,
                Some("biggest") => app.tab = Tab::Biggest,
                Some("cleanup") => app.tab = Tab::Cleanup,
                // Check mode: show the delete dialog for a path (nothing is deleted in check mode).
                Some(t) if t.starts_with("trash=") => {
                    let path = PathBuf::from(&t["trash=".len()..]);
                    let size = std::fs::metadata(&path).map_or(0, |m| m.len());
                    app.ask_trash(path, size);
                }
                // First-run screens and the limited-access banner, as they'd look without access.
                Some(t) if t.starts_with("setup-") => {
                    app.full_access = Some(false);
                    app.setup = match t {
                        "setup-welcome" => vec![Setup::Welcome],
                        "setup-move" => vec![Setup::Move, Setup::Access],
                        _ => vec![Setup::Access],
                    };
                }
                Some("limited") => app.full_access = Some(false),
                Some("confirm") => {
                    app.tab = Tab::Cleanup;
                    app.confirm = Some(Confirm::Clean);
                }
                _ => {}
            }
        }
        app
    }

    fn start_scan(&mut self, root: PathBuf) {
        if let Some(s) = &self.scanning {
            s.progress.cancel.store(true, Ordering::Relaxed);
        }
        let progress = Arc::new(Progress::default());
        let (tx, rx) = channel();
        let p = progress.clone();
        let r = root.clone();
        let already_shown = self.scan.as_ref().map_or(false, |s| s.root == root);
        if !already_shown {
            self.nav.clear();
            self.selected = None;
        }
        let saved = self.use_saved.then(|| scan::saved_file(&scan::saved_dir(), &root));
        std::thread::spawn(move || {
            // Show the last saved scan instantly while the fresh one runs.
            if let Some(file) = &saved {
                if !already_shown {
                    if let Some(old) = scan::load_saved(file) {
                        let _ = tx.send(ScanMsg::Saved(old));
                    }
                }
            }
            let Some(fresh) = scan::scan(r, p) else { return };
            let bytes = saved.as_ref().map(|_| scan::encode(&fresh));
            if tx.send(ScanMsg::Fresh(fresh)).is_ok() {
                if let (Some(file), Some(bytes)) = (saved, bytes) {
                    let _ = scan::write_saved(&file, &bytes);
                }
            }
        });
        self.partial.clear();
        self.scanning = Some(Scanning { root, progress, rx });
        self.tab = Tab::Explore;
        self.confirm = None;
        self.message = None;
    }

    /// Re-read one folder of the current scan and splice it in.
    fn rescan_folder(&mut self, path: PathBuf) {
        if self.partial.iter().any(|p| p.path == path) {
            return;
        }
        // A scan saved by an older version counts hard links differently; splicing a fresh
        // folder into it could count shared data twice. Refresh the whole thing instead.
        if let Some(s) = self.scan.as_ref().filter(|s| s.legacy) {
            let root = s.root.clone();
            if self.scanning.as_ref().map_or(true, |sc| sc.root != root) {
                self.start_scan(root);
            }
            return;
        }
        let (tx, rx) = channel();
        let p = path.clone();
        std::thread::spawn(move || {
            let progress = Progress::default();
            let node = scan::scan_tree(&p, &progress);
            let _ = tx.send(node.map(|n| (n, scan::take_denied(&progress))));
        });
        self.partial.push(Partial { path, rx });
    }

    fn measure_trash(&mut self) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(platform::trash_size());
        });
        self.trash_rx = Some(rx);
    }

    fn in_trash(&self) -> u64 {
        self.trash_bytes.unwrap_or(0) + self.trashed_since
    }

    fn finish_setup(&mut self, how: &str) {
        self.setup.clear();
        if self.use_saved {
            let file = setup_file();
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(file, how);
        }
    }

    /// Look again for Full Disk Access (cheap); move on once it's there.
    fn check_access(&mut self) {
        if self.shot.is_some() || self.full_access != Some(false) {
            return;
        }
        self.full_access = platform::full_disk_access();
        if self.full_access == Some(true) {
            self.setup.retain(|s| *s != Setup::Access);
            if self.setup.is_empty() {
                self.finish_setup("done");
            }
            self.message = Some(("Full Disk Access is on. Rescan a disk to count everything.".into(), true));
        }
    }

    fn refresh_disks(&mut self) {
        if self.demo {
            return;
        }
        self.check_access();
        self.volumes = platform::volumes();
        self.disks_read = std::time::Instant::now();
        self.hidden_volumes = self
            .scan
            .as_ref()
            .filter(|s| self.volumes.iter().any(|v| v.path == s.root))
            .map(|s| platform::hidden_volumes(&s.root));
    }

    /// A new scan is showing: re-read what it can't see.
    fn scan_arrived(&mut self) {
        self.refresh_disks();
        self.snapshots = None;
        if let (Some(_), Some(s)) = (&self.hidden_volumes, &self.scan) {
            self.snapshots = platform::local_snapshots(&s.root);
        }
    }

    fn start_empty_trash(&mut self) {
        self.cleaned_paths = platform::trash_folder().into_iter().collect();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let o = cleanup::clean(cleanup::Kind::Trash, &[], 0);
            let _ = tx.send((o.freed, o.skipped));
        });
        self.cleaning = Some(rx);
    }

    fn save_now(&mut self) {
        // An older-format scan is never re-saved as current: the next full scan replaces it.
        if let (true, true, Some(s)) = (self.use_saved, self.dirty, self.scan.as_ref().filter(|s| !s.legacy)) {
            let file = scan::saved_file(&scan::saved_dir(), &s.root);
            let _ = scan::write_saved(&file, &scan::encode(s));
            self.dirty = false;
        }
    }

    fn start_measure(&mut self) {
        let jobs: Vec<_> = self.cats.iter().map(|c| (c.kind, c.paths.clone())).collect();
        let days = self.old_days;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let r = jobs.iter().map(|(k, p)| cleanup::measure(*k, p, days)).collect();
            let _ = tx.send(r);
        });
        for c in &mut self.cats {
            c.measured = None;
        }
        self.measuring = Some(rx);
    }

    fn start_clean(&mut self) {
        let jobs: Vec<_> = self.cats.iter().filter(|c| c.selected).map(|c| (c.kind, c.paths.clone())).collect();
        // Folders to re-read in the open scan once cleaning is done.
        self.cleaned_paths = jobs.iter().flat_map(|(k, p)| {
            let mut v = p.clone();
            if *k != cleanup::Kind::Contents {
                v.extend(platform::trash_folder());
            }
            v
        }).collect();
        let days = self.old_days;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let (mut freed, mut skipped) = (0, 0);
            for (k, p) in &jobs {
                let o = cleanup::clean(*k, p, days);
                freed += o.freed;
                skipped += o.skipped;
            }
            let _ = tx.send((freed, skipped));
        });
        self.cleaning = Some(rx);
    }

    fn poll(&mut self) {
        if let Some(s) = &self.scanning {
            match s.rx.try_recv() {
                Ok(ScanMsg::Saved(old)) => {
                    self.scan = Some(old);
                    self.scan_arrived();
                }
                Ok(ScanMsg::Fresh(fresh)) => {
                    // Keep the person's place: navigation is by name, so it survives the swap.
                    self.scan = Some(fresh);
                    self.scanning = None;
                    self.scan_arrived();
                    self.dirty = false;
                    if self.use_saved {
                        self.saved = scan::list_saved(&scan::saved_dir());
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.scanning = None,
                Err(_) => {}
            }
        }
        let mut done = Vec::new();
        for (i, job) in self.partial.iter().enumerate() {
            match job.rx.try_recv() {
                Ok(node) => done.push((i, node)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => done.push((i, None)),
                Err(_) => {}
            }
        }
        for (i, node) in done.into_iter().rev() {
            let job = self.partial.remove(i);
            let Some(scan) = &mut self.scan else { continue };
            if let Some(idx) = scan::find_path(&scan.tree, &scan.root, &job.path) {
                match node {
                    Some((fresh, denied)) => {
                        scan::replace(&mut scan.tree, &idx, fresh);
                        scan::replace_denied(&mut scan.denied, &job.path, denied);
                    }
                    None if !job.path.exists() => {
                        scan::remove(&mut scan.tree, &idx);
                        scan::replace_denied(&mut scan.denied, &job.path, Vec::new());
                    }
                    None => continue,
                }
                scan.biggest = scan::biggest_files(&scan.tree, &scan.root);
                self.dirty = true;
            }
        }
        let removed = self.removing.as_ref().and_then(|(_, rx)| rx.try_recv().ok());
        if let Some(r) = removed {
            let (rt, _) = self.removing.take().unwrap();
            match r {
                Ok(()) => {
                    self.forget(&rt.folder);
                    self.message = Some((
                        format!("Removed the {} ({}). macOS may take a moment to free the space.", rt.name, human(rt.size)),
                        true,
                    ));
                }
                Err(e) => self.message = Some((e, false)),
            }
        }
        if let Some(Ok(size)) = self.trash_rx.as_ref().map(|rx| rx.try_recv()) {
            if size.is_some() {
                self.trashed_since = 0;
            }
            self.trash_bytes = size.map(|s| s.0);
            self.trash_rx = None;
        }
        if let Some(rx) = &self.measuring {
            if let Ok(sizes) = rx.try_recv() {
                for (c, s) in self.cats.iter_mut().zip(sizes) {
                    c.measured = Some(s);
                }
                self.measuring = None;
            }
        }
        if let Some(rx) = &self.cleaning {
            if let Ok((freed, skipped)) = rx.try_recv() {
                let mut text = format!("Done — freed {}.", human(freed));
                if skipped > 0 {
                    text += &format!(" {skipped} item(s) were in use or protected and were left alone.");
                }
                if skipped > 0 && freed == 0 {
                    self.message = Some(("Nothing could be removed — the system blocked it.".into(), false));
                } else {
                    self.message = Some((text.clone(), true));
                }
                self.last_clean = Some(text);
                self.cleaning = None;
                self.refresh_disks();
                self.trashed_since = 0;
                self.measure_trash();
                self.start_measure();
                // Bring the open scan up to date with what was just cleaned.
                for p in std::mem::take(&mut self.cleaned_paths) {
                    let inside = self.scan.as_ref().map_or(false, |s| scan::find_path(&s.tree, &s.root, &p).is_some());
                    if inside {
                        self.rescan_folder(p);
                    }
                }
            }
        }
    }

    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.shot else { return };
        ctx.request_repaint();
        shot.frame += 1;
        let ready = self.scanning.is_none() || shot.started.elapsed() > std::time::Duration::from_secs(300);
        if !shot.requested && shot.frame >= 120 && ready {
            shot.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
        }
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = image {
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
            if let Some(parent) = shot.path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, bytes) {
                Some(buf) => {
                    if let Err(e) = buf.save(&shot.path) {
                        eprintln!("Couldn't save screenshot: {e}");
                    }
                }
                None => eprintln!("Screenshot had an unexpected size"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        // Keep free space current, including space macOS/Windows frees a little later.
        if self.disks_read.elapsed() > std::time::Duration::from_secs(2) {
            self.refresh_disks();
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
        if self.scanning.is_some() || self.removing.is_some() || !self.partial.is_empty() || self.measuring.is_some() || self.cleaning.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        // Coming back from System Settings: see if access was just turned on.
        let focused = ctx.input(|i| i.focused);
        if focused && !self.was_focused {
            self.check_access();
        }
        self.was_focused = focused;

        if let Some(&step) = self.setup.first() {
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(BG).inner_margin(Margin::symmetric(22.0, 18.0)))
                .show(ctx, |ui| self.setup_view(ui, step));
            self.handle_screenshot(ctx);
            return;
        }

        egui::SidePanel::left("side")
            .exact_width(236.0)
            .resizable(false)
            .frame(egui::Frame::none().fill(PANEL).inner_margin(Margin::same(14.0)))
            .show(ctx, |ui| self.sidebar(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG).inner_margin(Margin::symmetric(22.0, 18.0)))
            .show(ctx, |ui| {
                self.access_banner(ui);
                self.banner(ui);
                match self.tab {
                    Tab::Overview => self.overview(ui),
                    Tab::Explore => self.explore(ui),
                    Tab::Biggest => self.biggest(ui),
                    Tab::Cleanup => self.cleanup(ui),
                }
            });

        self.confirm_dialog(ctx);
        self.handle_screenshot(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some((_, rx)) = &self.removing {
            // Let an in-progress simulator removal finish before quitting.
            let _ = rx.recv();
        }
        self.save_now();
    }
}

// ---------------------------------------------------------------- sidebar

impl App {
    fn sidebar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
            logo(ui, r);
            ui.add_space(4.0);
            ui.label(RichText::new("Disk Sweeper").size(18.0).strong().color(TEXT));
        });
        ui.add_space(18.0);

        for (tab, icon, label) in [
            (Tab::Overview, "🏠", "Overview"),
            (Tab::Explore, "🗀", "Explore"),
            (Tab::Biggest, "📊", "Biggest files"),
            (Tab::Cleanup, "✨", "Cleanup"),
        ] {
            if nav_button(ui, self.tab == tab, icon, label) {
                self.tab = tab;
                if tab == Tab::Cleanup && self.measuring.is_none() && self.cats.iter().all(|c| c.measured.is_none()) {
                    self.start_measure();
                }
            }
        }

        ui.add_space(20.0);
        section_label(ui, "DISKS");
        let mut scan_target = None;
        let disks_h = (ui.available_height() - 90.0).max(80.0);
        let in_trash = self.in_trash();
        let mut empty = false;
        egui::ScrollArea::vertical().max_height(disks_h).auto_shrink([false, true]).show(ui, |ui| {
        for v in &self.volumes {
            let used = v.total.saturating_sub(v.free);
            let frac = used as f32 / v.total.max(1) as f32;
            let trash = if platform::holds_trash(v) { in_trash.min(used) } else { 0 };
            let resp = egui::Frame::none()
                .fill(CARD)
                .rounding(8.0)
                .inner_margin(Margin::same(10.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("🖴").size(15.0).color(DIM));
                        ui.label(RichText::new(&v.name).color(TEXT).strong());
                    });
                    usage_bar(ui, &v.path, frac, trash as f32 / v.total.max(1) as f32, 8.0);
                    ui.label(
                        RichText::new(format!("{} free of {}", human(v.free), human(v.total))).size(11.5).color(DIM),
                    );
                    if trash > 0 {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{} in {}", human(trash), platform::TRASH_NAME)).size(11.5).color(DIM));
                            empty |= ui
                                .add(egui::Button::new(RichText::new("Empty").size(11.5).color(ACCENT)).frame(false))
                                .on_hover_text(format!("Free this space by emptying the {}", platform::TRASH_NAME))
                                .clicked();
                        });
                    }
                })
                .response
                .interact(Sense::click())
                .on_hover_text("Scan this disk");
            if resp.clicked() {
                scan_target = Some(v.path.clone());
            }
            ui.add_space(6.0);
        }
        });
        if ui
            .add(egui::Button::new(RichText::new("🏠  Scan my home folder").color(TEXT)).min_size(vec2(ui.available_width(), 30.0)))
            .clicked()
        {
            scan_target = Some(platform::home());
        }
        if let Some(t) = scan_target {
            self.start_scan(t);
        }
        if empty && self.cleaning.is_none() {
            self.confirm = Some(Confirm::EmptyTrash);
        }

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.label(RichText::new("Every feature is free — scan and clean as often as you like.").size(11.0).color(DIM));
        });
    }
}

// ---------------------------------------------------------------- overview

impl App {
    fn overview(&mut self, ui: &mut Ui) {
        heading(ui, "Overview", "See what's using your disk and clear it out whenever you want.");
        ui.add_space(14.0);

        let mut scan_target = None;
        let mut empty = false;
        let in_trash = self.in_trash();
        let cols = if ui.available_width() > 760.0 { 2 } else { 1 };
        let card_w = (ui.available_width() - 14.0 * (cols as f32 - 1.0)) / cols as f32;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(14.0, 14.0);
            for v in &self.volumes {
                card(ui, card_w, |ui| {
                    let used = v.total.saturating_sub(v.free);
                    let frac = used as f32 / v.total.max(1) as f32;
                    let trash = if platform::holds_trash(v) { in_trash.min(used) } else { 0 };
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(92.0, 92.0), Sense::hover());
                        ring(ui, r, &v.path, frac);
                        ui.add_space(12.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&v.name).size(17.0).strong().color(TEXT));
                            ui.label(RichText::new(v.path.to_string_lossy()).size(11.5).color(DIM));
                            ui.add_space(6.0);
                            ui.label(RichText::new(format!("{} used", human(used))).color(TEXT));
                            ui.label(RichText::new(format!("{} free of {}", human(v.free), human(v.total))).color(DIM));
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if accent_button(ui, "Scan this disk").clicked() {
                                    scan_target = Some(v.path.clone());
                                }
                                if trash > 0 {
                                    empty |= ui
                                        .button(format!("Empty {} ({})", platform::TRASH_NAME, human(trash)))
                                        .clicked();
                                }
                            });
                        });
                    });
                });
            }
        });

        ui.add_space(14.0);
        card(ui, ui.available_width(), |ui| {
            ui.label(RichText::new("Scan a folder").size(15.0).strong().color(TEXT));
            ui.label(RichText::new("Type or paste any folder to see what's inside it.").color(DIM));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let edit = ui.add(egui::TextEdit::singleline(&mut self.folder_input).desired_width(ui.available_width() - 110.0));
                let go = accent_button(ui, "Scan folder").clicked()
                    || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if go {
                    let p = PathBuf::from(self.folder_input.trim());
                    if p.is_dir() {
                        scan_target = Some(p);
                    } else {
                        self.message = Some(("That folder doesn't exist.".into(), false));
                    }
                }
            });
        });

        if !self.saved.is_empty() {
            ui.add_space(14.0);
            card(ui, ui.available_width(), |ui| {
                ui.label(RichText::new("Recent scans").size(15.0).strong().color(TEXT));
                ui.label(RichText::new("Open instantly from the last scan; it refreshes in the background.").color(DIM));
                ui.add_space(4.0);
                for info in self.saved.iter().take(5) {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("🗀").color(DIM));
                        ui.add(egui::Label::new(RichText::new(info.root.to_string_lossy()).color(TEXT)).truncate(true));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Open").clicked() {
                                scan_target = Some(info.root.clone());
                            }
                            ui.label(
                                RichText::new(format!("{}  ·  {} files  ·  {}", human(info.size), info.files, ago(info.scanned_at)))
                                    .color(DIM),
                            );
                        });
                    });
                }
            });
        }

        ui.add_space(14.0);
        card(ui, ui.available_width(), |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("✨").size(26.0));
                ui.vertical(|ui| {
                    ui.label(RichText::new("Cleanup").size(15.0).strong().color(TEXT));
                    ui.label(
                        RichText::new("Clear caches, logs, temp files, old downloads and the Trash — any time, no limits.")
                            .color(DIM),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if accent_button(ui, "Open Cleanup").clicked() {
                        self.tab = Tab::Cleanup;
                        if self.measuring.is_none() {
                            self.start_measure();
                        }
                    }
                });
            });
        });

        if let Some(t) = scan_target {
            self.start_scan(t);
        }
        if empty && self.cleaning.is_none() {
            self.confirm = Some(Confirm::EmptyTrash);
        }
    }
}

// ---------------------------------------------------------------- explore

enum Act {
    Into(String),
    Up(usize),
    Select(String),
    Open(PathBuf),
    Reveal(PathBuf),
    Trash(PathBuf, u64),
    RescanAll,
    RescanHere(PathBuf),
}

impl App {
    /// Full-screen progress, only while there's nothing (not even a saved scan) to show yet.
    fn scanning_view(&mut self, ui: &mut Ui) -> bool {
        let Some(s) = &self.scanning else { return false };
        if self.scan.as_ref().map_or(false, |sc| sc.root == s.root) {
            return false;
        }
        heading(ui, "Scanning…", &s.root.to_string_lossy());
        ui.add_space(30.0);
        let mut stop = false;
        card(ui, ui.available_width(), |ui| {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(progress_text(&s.progress)).size(16.0).color(TEXT));
            });
            let cur = s.progress.current.lock().map(|c| c.clone()).unwrap_or_default();
            ui.add(egui::Label::new(RichText::new(cur).size(11.5).color(DIM)).truncate(true));
            ui.add_space(8.0);
            stop = ui.button("Stop").clicked();
        });
        if stop {
            self.stop_scan();
        }
        true
    }

    fn stop_scan(&mut self) {
        if let Some(s) = self.scanning.take() {
            s.progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn explore(&mut self, ui: &mut Ui) {
        if self.scanning_view(ui) {
            return;
        }
        let Some(scan) = &self.scan else {
            empty_state(ui, "Nothing scanned yet", "Pick a disk on the left, or scan your home folder.");
            return;
        };
        let mut act: Option<Act> = None;
        let idx = scan::resolve(&scan.tree, &self.nav);
        let depth = idx.len();
        let node = scan.tree.at(&idx);
        let here = scan.tree.child_path(&scan.root, &idx);
        let selected = self.selected.as_ref().and_then(|n| node.children.iter().position(|c| &c.name == n));

        // Breadcrumb
        ui.horizontal(|ui| {
            if ui.add_enabled(depth > 0, egui::Button::new("⬅ Up")).clicked() {
                act = Some(Act::Up(depth - 1));
            }
            ui.add_space(6.0);
            let root_name = short_root(&scan.root);
            if crumb(ui, &root_name, depth == 0) {
                act = Some(Act::Up(0));
            }
            for (d, name) in self.nav.iter().take(depth).enumerate() {
                ui.label(RichText::new("›").color(DIM));
                if crumb(ui, name, d + 1 == depth) {
                    act = Some(Act::Up(d + 1));
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(human(node.size)).size(24.0).strong().color(TEXT));
            ui.label(RichText::new(format!("in {} files", node.files)).color(DIM));
            ui.add_space(10.0);
            // Where these numbers come from and whether they're being refreshed.
            let refreshing = self.scanning.as_ref().filter(|s| s.root == scan.root);
            if let Some(s) = refreshing {
                ui.spinner();
                ui.label(
                    RichText::new(format!("Saved scan from {} · refreshing: {}", ago(scan.scanned_at), progress_text(&s.progress)))
                        .color(DIM),
                );
            } else if !self.partial.is_empty() {
                ui.spinner();
                ui.label(RichText::new("Updating…").color(DIM));
            } else {
                ui.label(RichText::new(format!("Scanned {}", ago(scan.scanned_at))).color(DIM));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if refreshing.is_some() {
                    if ui.button("Stop refresh").clicked() {
                        act = Some(Act::RescanAll); // handled as stop below
                    }
                } else {
                    if ui.button("Rescan all").on_hover_text("Scan everything again").clicked() {
                        act = Some(Act::RescanAll);
                    }
                    if depth > 0
                        && ui.button("Rescan this folder").on_hover_text("Quickly re-read just this folder").clicked()
                    {
                        act = Some(Act::RescanHere(here.clone()));
                    }
                }
                if ui.button(format!("Show in {}", platform::FILE_MANAGER)).clicked() {
                    act = Some(Act::Open(here.clone()));
                }
            });
        });
        if depth == 0 {
            self.not_scanned(ui);
        }
        ui.add_space(8.0);

        // Treemap
        let map_h = (ui.available_height() * 0.42).clamp(140.0, 330.0);
        let (map_rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), map_h), Sense::hover());
        draw_treemap(ui, map_rect, node, selected, &mut act);
        ui.add_space(10.0);

        // List
        let action_h = if selected.is_some() { 52.0 } else { 0.0 };
        let list_h = (ui.available_height() - action_h).max(80.0);
        let total = node.size.max(1);
        egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show_rows(
            ui,
            30.0,
            node.children.len(),
            |ui, range| {
                for i in range {
                    let c = &node.children[i];
                    let color = if c.is_dir { PALETTE[i % PALETTE.len()] } else { FILE_COLOR };
                    let r = size_row(ui, c, color, c.size as f32 / total as f32, selected == Some(i));
                    if r.double_clicked() && c.is_dir {
                        act = Some(Act::Into(c.name.clone()));
                    } else if r.clicked() {
                        act = Some(Act::Select(c.name.clone()));
                    }
                }
            },
        );
        if node.children.is_empty() {
            ui.label(RichText::new("This folder is empty (or couldn't be read).").color(DIM));
        }

        // Action bar for the selection
        if let Some(i) = selected {
            let c = &node.children[i];
            let path = here.join(&c.name);
            action_bar(ui, &c.name, c.size, |ui| {
                if danger_button(ui, &format!("Move to {}", platform::TRASH_NAME)).clicked() {
                    act = Some(Act::Trash(path.clone(), c.size));
                }
                if ui.button(format!("Show in {}", platform::FILE_MANAGER)).clicked() {
                    act = Some(Act::Reveal(path.clone()));
                }
                if c.is_dir {
                    if ui.button("Look inside").clicked() {
                        act = Some(Act::Into(c.name.clone()));
                    }
                } else if ui.button("Open").clicked() {
                    act = Some(Act::Open(path.clone()));
                }
            });
        }

        let root = scan.root.clone();
        let refreshing = self.scanning.as_ref().map_or(false, |s| s.root == root);
        match act {
            Some(Act::Into(name)) => {
                self.nav.truncate(depth);
                self.nav.push(name);
                self.selected = None;
            }
            Some(Act::Up(d)) => {
                self.nav.truncate(d);
                self.selected = None;
            }
            Some(Act::Select(name)) => self.selected = Some(name),
            Some(Act::Open(p)) => platform::open(&p),
            Some(Act::Reveal(p)) => platform::reveal(&p),
            Some(Act::Trash(p, size)) => self.ask_trash(p, size),
            Some(Act::RescanAll) if refreshing => self.stop_scan(),
            Some(Act::RescanAll) => self.start_scan(root),
            Some(Act::RescanHere(p)) => self.rescan_folder(p),
            None => {}
        }
    }
}

impl App {
    /// For a whole-disk scan: the part of the disk's used space the scan didn't find, and why.
    fn not_scanned(&self, ui: &mut Ui) {
        let Some(scan) = &self.scan else { return };
        // Whole-disk scans: how the disk's used space adds up.
        let disk = self.volumes.iter().find(|v| v.path == scan.root);
        if let (Some(hidden), Some(disk)) = (&self.hidden_volumes, disk) {
            let used = disk.total.saturating_sub(disk.free);
            let u = scan::unscanned(used, scan.tree.size, hidden.clone());
            if u.gap != 0 || u.overcount > 0 {
                self.disk_breakdown(ui, used, &u);
            }
        }
        if scan.unknown_alloc > 0 {
            ui.label(
                RichText::new(format!(
                    "{} compressed or sparse files couldn't be measured exactly and count at their full length.",
                    scan.unknown_alloc
                ))
                .color(DIM),
            );
        }
        // Any scan: folders that couldn't be read, even when the totals happen to look complete.
        if !scan.denied.is_empty() {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} folders couldn't be read, so they count as zero.", scan.denied.len()))
                        .color(TEXT),
                );
                if cfg!(target_os = "macos") {
                    if ui.button("Open Full Disk Access settings").clicked() {
                        platform::open(Path::new(platform::FULL_DISK_ACCESS_SETTINGS));
                    }
                    ui.label(RichText::new("Turn on Disk Sweeper there, then rescan.").size(11.5).color(DIM));
                } else if !platform::elevated() {
                    ui.label(RichText::new("Run Disk Sweeper as administrator to include them.").size(11.5).color(DIM));
                }
            });
            egui::CollapsingHeader::new(RichText::new("Show folders").color(DIM)).id_source("denied").show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                    for d in &scan.denied {
                        ui.add(egui::Label::new(RichText::new(d.to_string_lossy()).size(11.5).color(DIM)).truncate(true));
                    }
                });
            });
        }
    }

    fn disk_breakdown(&self, ui: &mut Ui, used: u64, u: &scan::Unscanned) {
        // The rows below always add up to this: volumes + hidden − over-counted = used − scanned.
        let title = if u.gap >= 0 {
            format!("Not scanned: {} of the {} used on this disk", human(u.gap as u64), human(used))
        } else {
            format!("The scan found {} more than the {} this disk reports using", human(u.gap.unsigned_abs()), human(used))
        };
        egui::CollapsingHeader::new(RichText::new(title).color(DIM)).id_source("not-scanned").default_open(true).show(ui, |ui| {
            let row = |ui: &mut Ui, size: String, name: &str| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(size).strong().color(TEXT));
                    ui.label(RichText::new(name).color(TEXT));
                });
            };
            for (name, size) in &u.volumes {
                row(ui, human(*size), name);
            }
            if !u.volumes.is_empty() {
                ui.label(
                    RichText::new("Separate volumes that share this disk's space; a scan can't look inside them.")
                        .size(11.5)
                        .color(DIM),
                );
            }
            if u.rest > 0 || u.overcount == 0 {
                row(ui, human(u.rest), "Hidden or protected");
                let why = if cfg!(target_os = "macos") {
                    let snaps = match self.snapshots {
                        Some(0) => " No Time Machine snapshots.".to_string(),
                        Some(n) => format!(" Includes {n} local Time Machine snapshot(s)."),
                        None => String::new(),
                    };
                    format!(
                        "Folders macOS didn't let Disk Sweeper read, files only the system can see (Spotlight index, \
                         system databases), and space macOS can free on its own.{snaps}"
                    )
                } else {
                    "Folders Disk Sweeper couldn't read, system files such as the page file and System Volume \
                     Information, and restore points."
                        .to_string()
                };
                ui.label(
                    RichText::new(format!("{why} Moving files between scans also shifts this number.")).size(11.5).color(DIM),
                );
            }
            if u.overcount > 0 {
                row(ui, format!("−{}", human(u.overcount)), "counted more than once by the scan");
                let why = if cfg!(target_os = "macos") {
                    "Some files share their space on disk (copies made with APFS clones), but the scan counts each \
                     one in full, so these figures are estimates."
                } else {
                    "Some files share their space on disk (hard links among small files, which aren't checked to keep \
                     scans fast), but the scan counts each one in full, so these figures are estimates."
                };
                ui.label(RichText::new(why).size(11.5).color(DIM));
            }
        });
    }
}

fn progress_text(p: &Progress) -> String {
    format!("{} files · {}", p.files.load(Ordering::Relaxed), human(p.bytes.load(Ordering::Relaxed)))
}

fn draw_treemap(ui: &mut Ui, area: Rect, node: &Node, selected: Option<usize>, act: &mut Option<Act>) {
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 8.0, CARD);
    const MAX: usize = 120;
    let shown = node.children.iter().take(MAX).filter(|c| c.size > 0).count();
    let mut sizes: Vec<f64> = node.children[..shown].iter().map(|c| c.size as f64).collect();
    let rest: u64 = node.children.iter().skip(shown).map(|c| c.size).sum();
    if rest > 0 {
        sizes.push(rest as f64);
    }
    if sizes.is_empty() {
        painter.text(area.center(), Align2::CENTER_CENTER, "Nothing to show", FontId::proportional(14.0), DIM);
        return;
    }
    let rects = treemap::layout(&sizes, area.shrink(4.0));
    for (i, r) in rects.iter().enumerate() {
        let tile = r.shrink(1.0);
        if tile.width() < 1.0 || tile.height() < 1.0 {
            continue;
        }
        let other = i >= shown;
        let child = (!other).then(|| &node.children[i]);
        let base = match child {
            Some(c) if c.is_dir => PALETTE[i % PALETTE.len()],
            Some(_) => FILE_COLOR,
            None => Color32::from_rgb(0x44, 0x48, 0x55),
        };
        let resp = ui.interact(tile, ui.id().with(("tile", i)), Sense::click());
        let fill = if resp.hovered() { lighten(base, 0.18) } else { base };
        painter.rect_filled(tile, 4.0, fill.gamma_multiply(0.9));
        if selected == Some(i) {
            painter.rect_stroke(tile, 4.0, Stroke::new(2.5, Color32::WHITE));
        }
        let name = child.map_or_else(|| format!("{} smaller items", node.children.len() - shown), |c| c.name.clone());
        let size = sizes[i] as u64;
        if tile.width() > 54.0 && tile.height() > 34.0 {
            let clip = ui.painter_at(tile.shrink(4.0));
            let pos = tile.left_top() + vec2(7.0, 6.0);
            clip.text(pos, Align2::LEFT_TOP, &name, FontId::proportional(13.0), Color32::WHITE);
            clip.text(
                pos + vec2(0.0, 16.0),
                Align2::LEFT_TOP,
                human(size),
                FontId::proportional(11.5),
                Color32::from_white_alpha(200),
            );
        }
        let resp = resp.on_hover_text(format!("{name}\n{}", human(size)));
        if let Some(c) = child {
            if resp.double_clicked() && c.is_dir {
                *act = Some(Act::Into(c.name.clone()));
            } else if resp.clicked() {
                *act = Some(Act::Select(c.name.clone()));
            }
        }
    }
}

fn size_row(ui: &mut Ui, n: &Node, color: Color32, frac: f32, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 6.0, ACCENT.gamma_multiply(0.25));
    } else if resp.hovered() {
        p.rect_filled(rect, 6.0, CARD);
    }
    let y = rect.center().y;
    p.rect_filled(Rect::from_center_size(Pos2::new(rect.left() + 12.0, y), vec2(10.0, 10.0)), 3.0, color);
    let icon = if n.is_dir { "🗀" } else { "🗋" };
    p.text(Pos2::new(rect.left() + 26.0, y), Align2::LEFT_CENTER, icon, FontId::proportional(14.0), DIM);

    let size_w = 90.0;
    let bar_w = (rect.width() * 0.22).clamp(80.0, 220.0);
    let name_right = rect.right() - size_w - bar_w - 24.0;
    let name_rect = Rect::from_min_max(Pos2::new(rect.left() + 46.0, rect.top()), Pos2::new(name_right, rect.bottom()));
    ui.painter_at(name_rect).text(
        Pos2::new(name_rect.left(), y),
        Align2::LEFT_CENTER,
        &n.name,
        FontId::proportional(14.0),
        TEXT,
    );
    let bar = Rect::from_min_size(Pos2::new(name_right + 12.0, y - 4.0), vec2(bar_w, 8.0));
    p.rect_filled(bar, 4.0, LINE);
    let mut fill = bar;
    fill.set_width((bar_w * frac).max(2.0));
    p.rect_filled(fill, 4.0, color);
    p.text(
        Pos2::new(rect.right() - 8.0, y),
        Align2::RIGHT_CENTER,
        human(n.size),
        FontId::proportional(13.5),
        TEXT,
    );
    resp
}

// ---------------------------------------------------------------- biggest files

impl App {
    fn biggest(&mut self, ui: &mut Ui) {
        if self.scanning_view(ui) {
            return;
        }
        let Some(scan) = &self.scan else {
            empty_state(ui, "Nothing scanned yet", "Scan a disk or folder and its largest files will show up here.");
            return;
        };
        heading(ui, "Biggest files", &format!("The largest files found in {}", scan.root.to_string_lossy()));
        ui.add_space(10.0);
        let max = scan.biggest.first().map_or(1, |f| f.size.max(1));
        let mut pick = None;
        let sel = self.selected_big.as_ref().and_then(|p| scan.biggest.iter().position(|f| &f.path == p));
        let action_h = if sel.is_some() { 52.0 } else { 0.0 };
        egui::ScrollArea::vertical()
            .max_height(ui.available_height() - action_h)
            .auto_shrink([false, false])
            .show_rows(ui, 40.0, scan.biggest.len(), |ui, range| {
                for i in range {
                    let f = &scan.biggest[i];
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                    let p = ui.painter();
                    if sel == Some(i) {
                        p.rect_filled(rect, 6.0, ACCENT.gamma_multiply(0.25));
                    } else if resp.hovered() {
                        p.rect_filled(rect, 6.0, CARD);
                    }
                    let name = f.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let folder = f.path.parent().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let text_rect = Rect::from_min_max(rect.min + vec2(10.0, 0.0), Pos2::new(rect.right() - 230.0, rect.bottom()));
                    let tp = ui.painter_at(text_rect);
                    tp.text(text_rect.left_top() + vec2(0.0, 4.0), Align2::LEFT_TOP, name, FontId::proportional(14.0), TEXT);
                    tp.text(text_rect.left_top() + vec2(0.0, 22.0), Align2::LEFT_TOP, folder, FontId::proportional(11.0), DIM);
                    let bar = Rect::from_min_size(Pos2::new(rect.right() - 220.0, rect.center().y - 4.0), vec2(120.0, 8.0));
                    p.rect_filled(bar, 4.0, LINE);
                    let mut fill = bar;
                    fill.set_width((120.0 * f.size as f32 / max as f32).max(2.0));
                    p.rect_filled(fill, 4.0, PALETTE[3]);
                    p.text(
                        Pos2::new(rect.right() - 8.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        human(f.size),
                        FontId::proportional(13.5),
                        TEXT,
                    );
                    if resp.clicked() {
                        pick = Some(i);
                    }
                }
            });
        if scan.biggest.is_empty() {
            ui.label(RichText::new("No files found.").color(DIM));
        }
        let mut trash = None;
        if let Some(f) = sel.and_then(|i| scan.biggest.get(i)) {
            let name = f.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            action_bar(ui, &name, f.size, |ui| {
                if danger_button(ui, &format!("Move to {}", platform::TRASH_NAME)).clicked() {
                    trash = Some((f.path.clone(), f.size));
                }
                if ui.button(format!("Show in {}", platform::FILE_MANAGER)).clicked() {
                    platform::reveal(&f.path);
                }
                if ui.button("Open").clicked() {
                    platform::open(&f.path);
                }
            });
        }
        if let Some(i) = pick {
            self.selected_big = scan.biggest.get(i).map(|f| f.path.clone());
        }
        if let Some((path, size)) = trash {
            self.ask_trash(path, size);
        }
    }
}

// ---------------------------------------------------------------- cleanup

impl App {
    fn cleanup(&mut self, ui: &mut Ui) {
        heading(ui, "Cleanup", "Pick what to clear, then clean — as often as you like.");
        ui.add_space(12.0);

        let selected_total: u64 =
            self.cats.iter().filter(|c| c.selected).filter_map(|c| c.measured.flatten()).map(|m| m.0).sum();
        let busy = self.cleaning.is_some();

        card(ui, ui.available_width(), |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Ready to free").color(DIM));
                    ui.label(RichText::new(human(selected_total)).size(30.0).strong().color(GOOD));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let any = self.cats.iter().any(|c| c.selected);
                    let btn = egui::Button::new(RichText::new("  Clean now  ").size(16.0).strong().color(Color32::WHITE))
                        .fill(GOOD)
                        .min_size(vec2(150.0, 42.0))
                        .rounding(8.0);
                    if ui.add_enabled(any && !busy, btn).clicked() {
                        self.confirm = Some(Confirm::Clean);
                    }
                    if ui.add_enabled(self.measuring.is_none() && !busy, egui::Button::new("Measure again")).clicked() {
                        self.start_measure();
                    }
                    if busy {
                        ui.spinner();
                        ui.label(RichText::new("Cleaning…").color(DIM));
                    } else if self.measuring.is_some() {
                        ui.spinner();
                        ui.label(RichText::new("Measuring…").color(DIM));
                    }
                });
            });
            if let Some(t) = &self.last_clean {
                ui.label(RichText::new(t).color(GOOD));
            }
        });
        ui.add_space(12.0);

        let mut remeasure = false;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for c in &mut self.cats {
                card(ui, ui.available_width(), |ui| {
                    ui.horizontal(|ui| {
                        ui.add_enabled(!busy, egui::Checkbox::without_text(&mut c.selected));
                        ui.vertical(|ui| {
                            ui.label(RichText::new(c.name).size(15.0).strong().color(TEXT));
                            ui.label(RichText::new(c.about).color(DIM));
                            if c.kind == cleanup::Kind::OldFiles {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Untouched for at least").color(DIM));
                                    let r = ui.add(egui::DragValue::new(&mut self.old_days).clamp_range(1..=3650).suffix(" days"));
                                    if r.drag_released() || r.lost_focus() {
                                        remeasure = true;
                                    }
                                });
                            }
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let (text, color) = match c.measured {
                                None => ("…".to_string(), DIM),
                                Some(None) => ("Needs permission".to_string(), DANGER),
                                Some(Some((b, f))) => (format!("{}  ·  {f} files", human(b)), TEXT),
                            };
                            ui.label(RichText::new(text).size(14.0).color(color)).on_hover_text(match c.measured {
                                Some(None) => "The system blocked access. On a Mac, allow Full Disk Access for this app in System Settings › Privacy & Security.",
                                _ => "",
                            });
                        });
                    });
                });
                ui.add_space(8.0);
            }
        });
        if remeasure && self.measuring.is_none() {
            self.start_measure();
        }
    }
}

// ---------------------------------------------------------------- confirm dialog

impl App {
    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &self.confirm else { return };
        let (title, body, button) = match confirm {
            Confirm::Trash { runtime: Some(rt), .. } => (
                format!("Remove the {}?", rt.name),
                format!(
                    "This file is part of an Xcode simulator. macOS protects it, so it can't go to the Trash — \
                     but Disk Sweeper can have Xcode's simulator tool remove it, freeing about {}.\n\n\
                     Simulators using it will stop working until you download it again in Xcode › Settings › Components.",
                    human(rt.size)
                ),
                "Remove simulator".to_string(),
            ),
            Confirm::Trash { path, protected: true, .. } => (
                format!("Can't move “{}”", path.file_name().unwrap_or_default().to_string_lossy()),
                platform::PROTECTED_TEXT.to_string(),
                String::new(),
            ),
            Confirm::Trash { path, size, shared, .. } => (
                format!("Move “{}” to the {}?", path.file_name().unwrap_or_default().to_string_lossy(), platform::TRASH_NAME),
                if *shared {
                    format!(
                        "This file is a hard link: the same data is also stored under another name, so moving this \
                         one won't free space until the other names are removed too. You can get it back from the {} \
                         until you empty it.",
                        platform::TRASH_NAME
                    )
                } else {
                    format!("{} will be moved. You can get it back from the {} until you empty it.", human(*size), platform::TRASH_NAME)
                },
                format!("Move to {}", platform::TRASH_NAME),
            ),
            Confirm::EmptyTrash => (
                format!("Empty the {}?", platform::TRASH_NAME),
                format!("{} will be deleted for good and the space freed.", human(self.in_trash())),
                format!("Empty {}", platform::TRASH_NAME),
            ),
            Confirm::Clean => {
                let names: Vec<_> = self.cats.iter().filter(|c| c.selected).map(|c| c.name).collect();
                (
                    "Clean now?".to_string(),
                    format!(
                        "This clears: {}.\nCaches, logs and temp files are deleted for good; old downloads go to the {}.",
                        names.join(", "),
                        platform::TRASH_NAME
                    ),
                    "Clean now".to_string(),
                )
            }
        };
        let mut result: Option<bool> = None;
        // Dim the window behind the dialog.
        let screen = ctx.screen_rect();
        egui::Area::new(egui::Id::new("dim")).fixed_pos(screen.min).order(egui::Order::Middle).show(ctx, |ui| {
            let r = ui.allocate_rect(screen, Sense::click());
            ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(140));
            if r.clicked() {
                result = Some(false);
            }
        });
        egui::Area::new(egui::Id::new("confirm"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
              egui::Frame::window(&ctx.style()).fill(PANEL).inner_margin(Margin::same(20.0)).rounding(12.0).show(ui, |ui| {
                ui.set_width(400.0);
                ui.label(RichText::new(title).size(17.0).strong().color(TEXT));
                ui.add_space(6.0);
                ui.label(RichText::new(body).color(DIM));
                ui.add_space(14.0);
                ui.allocate_ui_with_layout(vec2(ui.available_width(), 34.0), Layout::right_to_left(Align::Center), |ui| {
                    if button.is_empty() {
                        // Nothing can be done, so there's only a way out.
                        if accent_button(ui, "OK").clicked() {
                            result = Some(false);
                        }
                    } else {
                        if danger_button(ui, &button).clicked() {
                            result = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            result = Some(false);
                        }
                    }
                });
              });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            result = Some(false);
        }
        match result {
            Some(true) => {
                let confirm = self.confirm.take();
                self.run_confirmed(confirm);
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    fn ask_trash(&mut self, path: PathBuf, size: u64) {
        let runtime = platform::sim_runtime_for(&path);
        let protected = runtime.is_none() && platform::is_protected(&path);
        let shared = !path.is_dir() && platform::link_count(&path).map_or(false, |n| n > 1);
        self.confirm = Some(Confirm::Trash { path, size, runtime, protected, shared });
    }

    /// Something was removed: take it out of the open scan right away instead of rescanning.
    fn forget(&mut self, path: &Path) {
        if let Some(scan) = &mut self.scan {
            if let Some(idx) = scan::find_path(&scan.tree, &scan.root, path) {
                scan::remove(&mut scan.tree, &idx);
            }
            scan.biggest.retain(|f| !f.path.starts_with(path));
            self.dirty = true;
        }
        self.selected = None;
        self.selected_big = None;
        self.refresh_disks();
    }

    fn run_confirmed(&mut self, confirm: Option<Confirm>) {
        if self.shot.is_some() {
            return; // check mode never changes anything
        }
        match confirm {
            Some(Confirm::Trash { runtime: Some(rt), .. }) => {
                let (tx, rx) = channel();
                let id = rt.id.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(platform::delete_sim_runtime(&id));
                });
                self.removing = Some((rt, rx));
                self.message = None;
            }
            Some(Confirm::Trash { protected: true, .. }) => {}
            Some(Confirm::Trash { path, size, shared, .. }) => match (path.is_dir(), platform::move_to_trash(&path)) {
                (was_dir, Ok(())) => {
                    self.forget(&path);
                    self.measure_trash();
                    if shared {
                        self.message = Some((
                            format!("Moved to the {}. Its data stays on disk under its other names.", platform::TRASH_NAME),
                            true,
                        ));
                        return;
                    }
                    if was_dir {
                        // A folder may hold hard links whose data lives on elsewhere; the Trash's
                        // own measurement (running now) counts only what emptying it frees.
                        self.message = Some((
                            format!("Moved to the {0}. Empty the {0} to get the space back.", platform::TRASH_NAME),
                            true,
                        ));
                        return;
                    }
                    self.trashed_since += size;
                    self.message = Some((
                        format!(
                            "Moved to the {0}. The {1} comes back once the {0} is emptied (use “Empty {0}” on the left).",
                            platform::TRASH_NAME,
                            human(size)
                        ),
                        true,
                    ));
                }
                (_, Err(e)) => self.message = Some((e, false)),
            },
            Some(Confirm::Clean) => self.start_clean(),
            Some(Confirm::EmptyTrash) => self.start_empty_trash(),
            None => {}
        }
    }
}

impl App {
    /// Results and problems, shown above whichever screen is open.
    /// Shown while the app can't read everything (setup was skipped or access turned off).
    fn access_banner(&mut self, ui: &mut Ui) {
        if self.full_access != Some(false) {
            return;
        }
        let mut fix = false;
        egui::Frame::none()
            .fill(ACCENT.gamma_multiply(0.14))
            .stroke(Stroke::new(1.0, ACCENT.gamma_multiply(0.5)))
            .rounding(8.0)
            .inner_margin(Margin::symmetric(12.0, 8.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Limited access: some folders can't be read, so disk totals come up short.")
                            .color(TEXT),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        fix = ui.button("Fix").clicked();
                    });
                });
            });
        ui.add_space(10.0);
        if fix {
            self.setup = vec![Setup::Access];
        }
    }

    fn setup_view(&mut self, ui: &mut Ui, step: Setup) {
        let width = ui.available_width().min(560.0);
        ui.add_space((ui.available_height() * 0.18).max(10.0));
        let mut next = false;
        let mut skip = false;
        ui.vertical_centered(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
            logo(ui, r);
            ui.add_space(10.0);
            card(ui, width, |ui| match step {
                Setup::Welcome => {
                    ui.label(RichText::new("Welcome to Disk Sweeper").size(22.0).strong().color(TEXT));
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "Pick a disk to see what's taking up space, then clear out what you don't need. \
                             Anything you remove goes to the Recycle Bin first.",
                        )
                        .color(TEXT),
                    );
                    ui.add_space(6.0);
                    let note = if platform::elevated() {
                        "Running as administrator, so system folders can be measured too."
                    } else {
                        "Some system folders are admin-only. To include them, right-click Disk Sweeper and choose \
                         \"Run as administrator\"."
                    };
                    ui.label(RichText::new(note).size(12.0).color(DIM));
                    ui.add_space(12.0);
                    next = accent_button(ui, "Get started").clicked();
                }
                Setup::Move => {
                    ui.label(RichText::new("Move to Applications?").size(22.0).strong().color(TEXT));
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "Disk Sweeper is running from outside your Applications folder. Moving it there keeps \
                             it in one place, so macOS remembers the access you give it.",
                        )
                        .color(TEXT),
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if accent_button(ui, "Move to Applications").clicked() {
                            if let Some(bundle) = platform::outside_applications() {
                                match platform::move_to_applications(&bundle) {
                                    Ok(new) => platform::relaunch(Some(&new)),
                                    Err(e) => self.message = Some((e, false)),
                                }
                            }
                        }
                        next = ui.button("Not now").clicked();
                    });
                    if let Some((m, _)) = &self.message {
                        ui.label(RichText::new(m).color(DANGER));
                    }
                }
                Setup::Access => {
                    ui.label(RichText::new("Allow Full Disk Access").size(22.0).strong().color(TEXT));
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "macOS hides some folders (Mail, Messages, Photos, other apps' data) from apps until you \
                             allow it. Without access, those folders count as zero and your disk totals won't add up.",
                        )
                        .color(TEXT),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("In System Settings, turn on Disk Sweeper under Full Disk Access, then come back here.")
                            .size(12.0)
                            .color(DIM),
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if accent_button(ui, "Open System Settings").clicked() {
                            platform::open(Path::new(platform::FULL_DISK_ACCESS_SETTINGS));
                            self.opened_settings = true;
                        }
                        if self.opened_settings
                            && ui.button("Relaunch Disk Sweeper").on_hover_text("macOS sometimes applies it only after a restart").clicked()
                        {
                            // Nothing is saved, so setup picks up here if access still isn't on.
                            platform::relaunch(None);
                        }
                        skip = ui.button("Skip for now").clicked();
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Waiting for access… this screen moves on by itself.").size(12.0).color(DIM));
                    });
                }
            });
        });
        if next {
            self.message = None;
            self.setup.remove(0);
            if self.setup.is_empty() {
                self.finish_setup("done");
            }
        }
        if skip {
            self.finish_setup("skipped");
        }
    }

    fn banner(&mut self, ui: &mut Ui) {
        let (text, color, busy) = match (&self.removing, &self.message) {
            (Some((rt, _)), _) => (format!("Removing the {}…", rt.name), ACCENT, true),
            (None, Some((m, good))) => (m.clone(), if *good { GOOD } else { DANGER }, false),
            (None, None) => return,
        };
        let mut close = false;
        egui::Frame::none()
            .fill(color.gamma_multiply(0.18))
            .stroke(Stroke::new(1.0, color.gamma_multiply(0.6)))
            .rounding(8.0)
            .inner_margin(Margin::symmetric(12.0, 8.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    if busy {
                        ui.spinner();
                    }
                    ui.add(egui::Label::new(RichText::new(text).color(TEXT)).wrap(true));
                    if !busy {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            close = ui.small_button("✕").clicked();
                        });
                    }
                });
            });
        ui.add_space(10.0);
        if close {
            self.message = None;
        }
    }
}

// ---------------------------------------------------------------- small widgets

fn style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = Color32::from_rgb(0x12, 0x14, 0x19);
    v.widgets.inactive.weak_bg_fill = CARD;
    v.widgets.inactive.bg_fill = Color32::from_rgb(0x3a, 0x40, 0x4f);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x32, 0x37, 0x44);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(0x3a, 0x40, 0x4f);
    v.widgets.inactive.rounding = Rounding::same(6.0);
    v.widgets.hovered.rounding = Rounding::same(6.0);
    v.widgets.active.rounding = Rounding::same(6.0);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.6);
    ctx.set_visuals(v);
    let mut s = (*ctx.style()).clone();
    s.spacing.button_padding = vec2(12.0, 6.0);
    s.spacing.item_spacing = vec2(8.0, 6.0);
    ctx.set_style(s);
}

fn heading(ui: &mut Ui, title: &str, sub: &str) {
    ui.label(RichText::new(title).size(26.0).strong().color(TEXT));
    ui.add(egui::Label::new(RichText::new(sub).color(DIM)).truncate(true));
}

fn section_label(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).strong().color(DIM));
    ui.add_space(4.0);
}

fn card<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::none()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, LINE))
        .rounding(10.0)
        .inner_margin(Margin::same(16.0))
        .show(ui, |ui| {
            ui.set_width(width - 34.0);
            add(ui)
        })
        .inner
}

fn nav_button(ui: &mut Ui, active: bool, icon: &str, label: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    let p = ui.painter();
    if active {
        p.rect_filled(rect, 7.0, ACCENT.gamma_multiply(0.28));
    } else if resp.hovered() {
        p.rect_filled(rect, 7.0, CARD);
    }
    let c = if active { Color32::WHITE } else { TEXT };
    p.text(rect.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, icon, FontId::proportional(15.0), c);
    p.text(rect.left_center() + vec2(40.0, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(14.5), c);
    ui.add_space(2.0);
    resp.clicked()
}

fn crumb(ui: &mut Ui, text: &str, current: bool) -> bool {
    let rt = RichText::new(text).color(if current { TEXT } else { ACCENT });
    ui.add(egui::Label::new(if current { rt.strong() } else { rt }).sense(Sense::click())).clicked()
}

fn accent_button(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).color(Color32::WHITE).strong()).fill(ACCENT))
}

fn danger_button(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).color(Color32::WHITE).strong()).fill(DANGER))
}

fn action_bar(ui: &mut Ui, name: &str, size: u64, add: impl FnOnce(&mut Ui)) {
    ui.add_space(6.0);
    egui::Frame::none().fill(CARD).rounding(8.0).inner_margin(Margin::symmetric(12.0, 8.0)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(name).strong().color(TEXT));
            ui.label(RichText::new(human(size)).color(DIM));
            ui.with_layout(Layout::right_to_left(Align::Center), add);
        });
    });
}

/// Used-space bar. `trash` is the part of the used space sitting in the Trash (drawn lighter).
/// Changes slide smoothly, so a freed file is visible as the bar shrinking.
fn usage_bar(ui: &mut Ui, id: &Path, frac: f32, trash: f32, h: f32) {
    let ctx = ui.ctx().clone();
    let frac = ctx.animate_value_with_time(egui::Id::new(("bar", id)), frac.clamp(0.0, 1.0), 0.6);
    let trash = ctx.animate_value_with_time(egui::Id::new(("trash", id)), trash.clamp(0.0, frac), 0.6);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    ui.painter().rect_filled(rect, h / 2.0, LINE);
    let mut fill = rect;
    fill.set_width(rect.width() * frac);
    ui.painter().rect_filled(fill, h / 2.0, usage_color(frac));
    if trash > 0.0 {
        let t = Rect::from_min_max(Pos2::new(fill.right() - rect.width() * trash, rect.top()), fill.max);
        ui.painter().rect_filled(t, h / 2.0, Color32::from_rgb(0xc9, 0xcd, 0xd6));
    }
}

fn usage_color(frac: f32) -> Color32 {
    if frac > 0.9 {
        DANGER
    } else if frac > 0.75 {
        PALETTE[3]
    } else {
        ACCENT
    }
}

fn ring(ui: &mut Ui, rect: Rect, id: &Path, frac: f32) {
    let frac = ui.ctx().animate_value_with_time(egui::Id::new(("ring", id)), frac.clamp(0.0, 1.0), 0.6);
    let p = ui.painter();
    let c = rect.center();
    let r = rect.width().min(rect.height()) / 2.0 - 6.0;
    p.circle_stroke(c, r, Stroke::new(10.0, LINE));
    let steps = 64;
    let end = frac.clamp(0.0, 1.0) * std::f32::consts::TAU;
    let pts: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + end * i as f32 / steps as f32;
            c + vec2(a.cos(), a.sin()) * r
        })
        .collect();
    p.add(egui::Shape::line(pts, Stroke::new(10.0, usage_color(frac))));
    p.text(c, Align2::CENTER_CENTER, format!("{:.0}%", frac * 100.0), FontId::proportional(17.0), TEXT);
}

fn logo(ui: &mut Ui, r: Rect) {
    let p = ui.painter();
    p.rect_filled(r, 7.0, ACCENT);
    let c = r.center();
    p.circle_stroke(c, 8.0, Stroke::new(2.5, Color32::WHITE));
    p.line_segment([c, c + vec2(0.0, -8.0)], Stroke::new(2.5, Color32::WHITE));
    p.circle_filled(c, 2.5, Color32::WHITE);
}

fn empty_state(ui: &mut Ui, title: &str, sub: &str) {
    ui.add_space(ui.available_height() * 0.3);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("🗀").size(48.0).color(DIM));
        ui.label(RichText::new(title).size(20.0).strong().color(TEXT));
        ui.label(RichText::new(sub).color(DIM));
    });
}

fn lighten(c: Color32, t: f32) -> Color32 {
    let f = |v: u8| (v as f32 + (255.0 - v as f32) * t) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

fn short_root(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| root.to_string_lossy().to_string())
}
