#![windows_subsystem = "windows"]

mod config;
mod device;
mod history;
mod monitor;
mod setup;
mod ui;

use windows_reactor::*;

fn main() -> Result<()> {
    // Framework-dependent: make sure the Windows App Runtime is present.
    if let Err(e) = bootstrap() {
        setup::handle_missing_runtime(&e)?;
        bootstrap()?;
    }

    let cfg = config::Config::load();
    let init_window = cfg.window_mins;
    let shared = monitor::init_shared(&cfg);
    monitor::spawn(shared.clone(), cfg);

    App::new()
        .title("Network Monitor")
        .inner_size(1080.0, 780.0)
        .backdrop(Backdrop::Mica)
        .render(move |cx| ui::app(cx, shared.clone(), init_window))
}
