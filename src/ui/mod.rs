//! Top-level UI: header (title, overall status, a settings gear), a row of
//! per-target cards, and the latency chart. Owns the shared Direct2D device and
//! a 1 Hz dispatcher timer that refreshes the view. The settings pane slides in
//! as an overlay on top of the dashboard.

pub mod cards;
pub mod chart;
pub mod settings;

use std::time::Duration;

use windows_reactor::*;

use crate::config::{Target, WINDOW_MINS};
use crate::device::{Device, Gpu, gpu_context};
use crate::monitor::{Shared, now_ms};
use cards::card_element;
use chart::{ChartProps, chart_view};
use settings::{Editing, SettingsCtx, interval_to_index, settings_panel, window_to_index};

struct CardInfo {
    name: String,
    ip: String,
    index: usize,
    current: Option<u32>,
    loss: u32,
    measured: usize,
}

/// Render the whole app. `shared` is the monitor's live state; `init_window`
/// is the persisted display window loaded at startup.
pub fn app(cx: &mut RenderCx, shared: Shared, init_window: i64) -> Element {
    // Shared GPU device, (re)created on mount and on recovery requests.
    let (device, update_device) = cx.use_reducer::<Option<Device>>(None);
    let (recover_gen, bump_recover) = cx.use_reducer::<u32>(0);
    cx.use_effect(recover_gen, move || {
        update_device.call(|current| match Device::new() {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("failed to create shared device: {e}");
                current
            }
        });
    });
    let bump_recover = cx.use_memo((), move || bump_recover);
    let gpu = Gpu::new(device, bump_recover);

    // 1 Hz refresh: bump a counter so the view re-reads shared state.
    let (_tick, bump_tick) = cx.use_reducer::<u64>(0);
    let timer = cx.use_ref::<Option<DispatcherTimer>>(None);
    cx.use_effect((), move || {
        if timer.borrow().is_none() {
            match DispatcherTimer::new(Duration::from_millis(1000), move || {
                bump_tick.call(|n| n.wrapping_add(1))
            }) {
                Ok(t) => timer.set(Some(t)),
                Err(e) => eprintln!("failed to start refresh timer: {e}"),
            }
        }
    });

    // Settings state. Interval + targets mirror the monitor's shared state;
    // window is UI-only (drives the chart/cards time span).
    let (settings_open, set_settings_open) = cx.use_state(false);
    let (interval_idx, set_interval_idx) =
        cx.use_state(interval_to_index(shared.lock().unwrap().interval_ms));
    let (window_idx, set_window_idx) = cx.use_state(window_to_index(init_window));
    let (edit_targets, set_targets) =
        cx.use_state::<Vec<Target>>(shared.lock().unwrap().targets.clone());
    let (editing, set_editing) = cx.use_state(Editing::Closed);
    // Bumped to force the edit form to re-render (e.g. after resolving a MAC),
    // since a nested component can't re-render itself via its own state.
    let (form_tick, set_form_tick) = cx.use_state(0i32);
    let window_mins = WINDOW_MINS[window_idx.clamp(0, 4) as usize];

    // Snapshot the state needed to render.
    let (cards_info, revision, worst_loss) = {
        let st = shared.lock().unwrap();
        let cutoff = now_ms() - window_mins * 60_000;
        let win: Vec<&crate::history::Sample> =
            st.history.samples.iter().filter(|s| s.t >= cutoff).collect();
        let last = st.history.samples.last();
        let mut worst = 0u32;
        let infos: Vec<CardInfo> = st
            .targets
            .iter()
            .enumerate()
            .map(|(i, t)| {
                // Only samples that measured this target count toward loss.
                // Samples from before it was added simply have no data.
                let measured = win.iter().filter(|s| s.v.contains_key(&t.name)).count();
                let drops = win
                    .iter()
                    .filter(|s| matches!(s.v.get(&t.name), Some(None)))
                    .count();
                let loss = if measured > 0 { (drops * 100 / measured) as u32 } else { 0 };
                worst = worst.max(loss);
                let current = last.and_then(|s| s.v.get(&t.name).copied().flatten());
                CardInfo {
                    name: t.name.clone(),
                    ip: t.ip.clone(),
                    index: i,
                    current,
                    loss,
                    measured,
                }
            })
            .collect();
        (infos, st.revision, worst)
    };

    let (status_text, status_color) = if worst_loss == 0 {
        ("All hops healthy".to_string(), Color::rgb(0x3f, 0xb9, 0x50))
    } else if worst_loss < 10 {
        (format!("Minor loss ({worst_loss}%)"), Color::rgb(0xd2, 0x99, 0x22))
    } else {
        (format!("Packet loss up to {worst_loss}%"), Color::rgb(0xf8, 0x51, 0x49))
    };

    let gear = button("")
        .icon(Symbol::Setting)
        .subtle()
        .on_click(set_settings_open.setter(true))
        .grid_column(1);

    let header = grid((
        hstack((
            text_block("Network Monitor").font_size(18.0).bold(),
            text_block(status_text).foreground(status_color),
        ))
        .spacing(16.0)
        .grid_column(0),
        gear,
    ))
    .columns([GridLength::STAR, GridLength::Auto]);

    let cards_row = hstack(
        cards_info
            .iter()
            .map(|c| {
                card_element(
                    &shared,
                    &c.name,
                    &c.ip,
                    c.index,
                    c.current,
                    c.loss,
                    c.measured,
                    revision,
                    window_mins,
                )
            })
            .collect::<Vec<Element>>(),
    )
    .spacing(16.0);

    let chart = component(
        chart_view,
        ChartProps {
            shared: shared.clone(),
            revision,
            window_mins,
        },
    );

    let dashboard = vstack((
        header,
        cards_row,
        text_block("Latency over time (ms) - red marks = packet dropped")
            .foreground(Color::rgb(0x8b, 0x94, 0x9e))
            .font_size(14.0),
        chart,
    ))
    .spacing(16.0)
    .padding(Thickness::uniform(24.0));

    let overlay: Element = if settings_open {
        settings_panel(SettingsCtx {
            shared: shared.clone(),
            interval_idx,
            window_idx,
            targets: edit_targets,
            editing,
            set_open: set_settings_open,
            set_interval_idx,
            set_window_idx,
            set_targets,
            set_editing,
            form_tick,
            set_form_tick,
        })
    } else {
        Element::Empty
    };

    grid((dashboard, overlay))
        .provide(&gpu_context(), Some(gpu))
        .into()
}
