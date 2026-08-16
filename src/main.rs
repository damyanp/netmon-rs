#![windows_subsystem = "windows"]

mod bandwidth;
mod config;
mod device;
mod history;
mod monitor;
mod network_info;
mod notification;
mod setup;
mod ui;

use windows_reactor::*;

fn main() -> Result<()> {
    // Framework-dependent: make sure the Windows App Runtime is present.
    if let Err(e) = bootstrap() {
        setup::handle_missing_runtime(&e)?;
        bootstrap()?;
    }
    if let Err(e) = notification::initialize_app_identity() {
        eprintln!("failed to set application identity: {e}");
    }

    let cfg = config::Config::load();
    let init_window = cfg.window_mins;
    let shared = monitor::init_shared(&cfg);
    bandwidth::spawn(
        shared.clone(),
        cfg.history_max_age_ms,
        cfg.history_max_samples,
    );
    monitor::spawn(shared.clone(), cfg);

    App::new()
        .title("Network Monitor")
        .inner_size(1080.0, 780.0)
        .backdrop(Backdrop::Mica)
        .render(move |cx| ui::app(cx, shared.clone(), init_window))
}
