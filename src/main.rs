mod app;
mod client;
mod clock;
mod config;
mod desktop;
mod helper;
mod i18n;
mod import;
mod instance;
mod keys;
mod logfile;
mod logo;
mod notify;
mod proto;
mod store;
mod system_theme;
mod tray;
mod update;
mod waker;
mod zip;

use std::{cell::RefCell, rc::Rc, time::Duration};

const USAGE: &str = "AmneziaWG GUI for Linux

Usage:
  awg-gui                 start the graphical interface
  awg-gui --minimized     start in the system tray (used for autostart)
  awg-gui --install       add the app to the desktop application menu
  awg-gui --import FILE…  import .conf / .zip files / files with vpn:// keys and exit
  awg-gui --version
  awg-gui --helper        (internal) privileged helper, started via pkexec
  awg-gui --boot          (internal) restore the tunnel at boot, run by awg-gui-boot.service
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--helper") => std::process::exit(helper::run()),
        Some("--boot") => std::process::exit(helper::boot()),
        Some("--version" | "-V") => {
            println!("awg-gui {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("--help" | "-h") => {
            print!("{USAGE}");
            return;
        }
        Some("--install") => {
            match desktop::install() {
                Ok(()) => println!("installed ~/.local/share/applications/awg-gui.desktop"),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("--import") => std::process::exit(cli_import(&args[1..])),
        Some("--minimized") => {}
        // (packaging) render the logo as PNG: --render-icon SIZE FILE
        Some("--render-icon") if args.len() == 3 => {
            let size: usize = args[1].parse().unwrap_or(256);
            if let Err(e) = desktop::write_png(std::path::Path::new(&args[2]), &logo::color_image(size)) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
            return;
        }
        Some(other) => {
            eprintln!("unknown argument: {other}\n\n{USAGE}");
            std::process::exit(2);
        }
        None => {}
    }

    let minimized = args.first().map(String::as_str) == Some("--minimized");

    // Already running? Ask it to show its window.
    if instance::notify("show") {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let waker = waker::Waker::default();
    if let Err(e) = instance::listen(tx.clone(), waker.clone()) {
        eprintln!("warning: single-instance socket: {e}");
    }
    desktop::ensure_installed();
    desktop::refresh_autostart();
    let tray = tray::Tray::spawn(tx, waker.clone());
    let sys_theme = system_theme::spawn(waker.clone());
    let app = Rc::new(RefCell::new(app::App::new(rx, waker, tray, sys_theme)));

    let mut show = !minimized || !app.borrow().has_tray();
    loop {
        if show {
            let options = eframe::NativeOptions {
                viewport: eframe::egui::ViewportBuilder::default()
                    .with_title("AmneziaWG")
                    .with_app_id("awg-gui")
                    .with_inner_size([900.0, 620.0])
                    .with_min_inner_size([640.0, 420.0])
                    .with_icon(logo::window_icon()),
                ..Default::default()
            };
            let a = app.clone();
            let res = eframe::run_native(
                "AmneziaWG",
                options,
                Box::new(move |cc| {
                    a.borrow_mut().attach(&cc.egui_ctx);
                    Ok(Box::new(Shell(a)))
                }),
            );
            app.borrow_mut().detach();
            if let Err(e) = res {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
            // Closing the window hides the app to the tray (if there is one).
            if !app.borrow().has_tray() {
                break;
            }
            show = false;
        }
        let mut a = app.borrow_mut();
        if a.quit {
            break;
        }
        if a.show_requested {
            show = true;
            continue;
        }
        a.wait(Duration::from_millis(500));
    }
}

/// eframe owns its `App` box; the real state outlives windows, so share it.
struct Shell(Rc<RefCell<app::App>>);

impl eframe::App for Shell {
    fn logic(&mut self, ctx: &eframe::egui::Context, frame: &mut eframe::Frame) {
        eframe::App::logic(&mut *self.0.borrow_mut(), ctx, frame);
    }
    fn ui(&mut self, ui: &mut eframe::egui::Ui, frame: &mut eframe::Frame) {
        eframe::App::ui(&mut *self.0.borrow_mut(), ui, frame);
    }
}

fn cli_import(files: &[String]) -> i32 {
    let store = match store::Store::open() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let mut rc = 0;
    let mut added: Vec<String> = vec![];
    for f in files {
        match import::from_file(std::path::Path::new(f)) {
            Ok(cands) => {
                for c in cands {
                    let conf = match config::validate(&c.text) {
                        Ok(conf) => conf,
                        Err(e) => {
                            eprintln!("{f}: {e}");
                            rc = 1;
                            continue;
                        }
                    };
                    for (k, v) in conf.scripts() {
                        eprintln!("{f}: warning: runs as root: {k} = {v}");
                    }
                    let name = store.unique_name(&c.name_hint, &added);
                    match store.save(&name, &c.text) {
                        Ok(()) => {
                            println!("imported {name}");
                            added.push(name);
                        }
                        Err(e) => {
                            eprintln!("{name}: {e}");
                            rc = 1;
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("{e}");
                rc = 1;
            }
        }
    }
    if !added.is_empty() {
        instance::notify("reload");
    }
    rc
}
