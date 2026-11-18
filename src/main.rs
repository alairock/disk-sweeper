#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod cleanup;
mod demo;
mod platform;
mod scan;
mod treemap;

use std::path::PathBuf;

/// The window/taskbar icon: the full macOS-style icon on a Mac, the tile on Windows.
fn window_icon() -> egui::IconData {
    let png: &[u8] = if cfg!(target_os = "macos") {
        include_bytes!("../assets/icon-256.png")
    } else {
        include_bytes!("../assets/ui/logo-256.png")
    };
    let img = image::load_from_memory(png).expect("embedded icon").into_rgba8();
    let (width, height) = img.dimensions();
    egui::IconData { rgba: img.into_raw(), width, height }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let shot: Option<PathBuf> = args
        .iter()
        .position(|a| a == "--screenshot")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    // Timing aid: `--scan-time <folder>` scans once, prints how long it took and exits.
    if let Some(dir) = args.iter().position(|a| a == "--scan-time").and_then(|i| args.get(i + 1)) {
        let t = std::time::Instant::now();
        let r = scan::scan(dir.into(), Default::default()).expect("scan");
        println!("{} files, {} in {:.2?}", r.tree.files, scan::human(r.tree.size), t.elapsed());
        return Ok(());
    }
    let shot_tab = args.iter().position(|a| a == "--tab").and_then(|i| args.get(i + 1)).cloned();
    // `--demo` (with --screenshot only): made-up disks and files, so pictures show no real data.
    let demo = shot.is_some() && args.iter().any(|a| a == "--demo");

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Disk Sweeper")
        .with_inner_size([1180.0, 760.0])
        .with_min_inner_size([820.0, 520.0])
        .with_icon(window_icon());

    let mut options = eframe::NativeOptions::default();
    if shot.is_some() {
        // Check mode: don't steal focus from whatever the person is doing.
        viewport = viewport.with_active(false);
        #[cfg(windows)]
        {
            viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnBottom);
        }
        #[cfg(target_os = "macos")]
        {
            options.event_loop_builder = Some(Box::new(|b| {
                use winit::platform::macos::EventLoopBuilderExtMacOS;
                b.with_activate_ignoring_other_apps(false);
            }));
        }
    }
    options.viewport = viewport;

    eframe::run_native("Disk Sweeper", options, Box::new(move |cc| Box::new(app::App::new(cc, shot, shot_tab, demo))))
}
