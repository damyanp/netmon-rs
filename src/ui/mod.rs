//! Top-level UI: header (title, overall status, a settings gear), a row of
//! per-target cards, and the latency chart. Owns the shared Direct2D device and
//! a 1 Hz dispatcher timer that refreshes the view. The settings pane slides in
//! as an overlay on top of the dashboard.

pub mod cards;
pub mod chart;
pub mod settings;
mod window_icon;

use std::time::Duration;

use windows_reactor::*;

use crate::config::{Target, WINDOW_MINS};
use crate::device::{Device, Gpu, gpu_context};
use crate::monitor::{Shared, now_ms};
use cards::card_element;
use chart::{ChartProps, chart_view};
use settings::{Editing, SettingsCtx, interval_to_index, settings_panel, window_to_index};

/// UI refresh period. Also the chart's scroll granularity.
const REFRESH_MS: u64 = 500;

struct CardInfo {
    name: String,
    ip: String,
    index: usize,
    current: Option<u32>,
    loss: u32,
    measured: usize,
}

/// Live ping-pacing state, read from the monitor for display.
struct Pace {
    auto: bool,
    manual_ms: u32,
    current_ms: u32,
    clean_needed: Option<u32>,
}

/// Render a ping interval for display. All the offered values are whole seconds.
fn fmt_interval(ms: u32) -> String {
    format!("{} s", (ms as f64 / 1000.0).round() as u32)
}

/// Short pace badge for the header, e.g. `Auto: every 1 s`.
fn pace_label(auto_interval: bool, current_interval_ms: u32) -> String {
    let every = fmt_interval(current_interval_ms);
    if auto_interval {
        format!("Auto: every {every}")
    } else {
        format!("Every {every}")
    }
}

/// The longer explanation shown in the settings panel.
fn pace_status(auto_interval: bool, current_interval_ms: u32, clean_needed: Option<u32>) -> String {
    let every = fmt_interval(current_interval_ms);
    if !auto_interval {
        return format!(
            "Pinging each target every {every}, one at a time spread across the interval."
        );
    }
    match clean_needed {
        Some(1) => {
            format!("Auto: pinging every {every} after a drop - 1 more clean round to slow down.")
        }
        Some(n) => {
            format!(
                "Auto: pinging every {every} after a drop - {n} more clean rounds to slow down."
            )
        }
        None => format!("Auto: pinging every {every} while healthy - any drop switches to 1 s."),
    }
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

    // Adopt the exe's embedded icon for the window caption + taskbar (once).
    cx.use_effect((), window_icon::set_app_window_icon);

    // 2 Hz refresh: bump a counter so the view re-reads shared state. The chart
    // keys its redraw off this, so it scrolls smoothly between samples.
    let (tick, bump_tick) = cx.use_reducer::<u64>(0);
    let timer = cx.use_ref::<Option<DispatcherTimer>>(None);
    cx.use_effect((), move || {
        if timer.borrow().is_none() {
            match DispatcherTimer::new(Duration::from_millis(REFRESH_MS), move || {
                bump_tick.call(|n| n.wrapping_add(1))
            }) {
                Ok(t) => timer.set(Some(t)),
                Err(e) => eprintln!("failed to start refresh timer: {e}"),
            }
        }
    });

    let (network_info, set_network_info) = cx.use_async_state(Default::default());
    cx.use_effect(tick / (60_000 / REFRESH_MS), move || {
        std::thread::spawn(move || set_network_info.call(crate::network_info::query()));
    });

    // Settings state mirrors the monitor's shared state. Window also drives the
    // chart/cards time span.
    let (settings_open, set_settings_open) = cx.use_state(false);
    let (init_auto, init_interval_ms) = {
        let st = shared.lock().unwrap();
        (st.auto_interval, st.interval_ms)
    };
    let (interval_idx, set_interval_idx) =
        cx.use_state(interval_to_index(init_auto, init_interval_ms));
    let (window_idx, set_window_idx) = cx.use_state(window_to_index(init_window));
    let (alert_threshold, set_alert_threshold) =
        cx.use_state(shared.lock().unwrap().packet_loss_alert_threshold);
    let (notification_status, set_notification_status) = cx.use_state(String::new());
    let (edit_targets, set_targets) =
        cx.use_state::<Vec<Target>>(shared.lock().unwrap().targets.clone());
    let (editing, set_editing) = cx.use_state(Editing::Closed);
    // Bumped to force the edit form to re-render (e.g. after resolving a MAC),
    // since a nested component can't re-render itself via its own state.
    let (form_tick, set_form_tick) = cx.use_state(0i32);
    let window_mins = WINDOW_MINS[window_idx.clamp(0, WINDOW_MINS.len() as i32 - 1) as usize];

    // Snapshot the state needed to render.
    let (cards_info, worst_loss, pace) = {
        let st = shared.lock().unwrap();
        let cutoff = now_ms() - window_mins * 60_000;
        let mut worst = 0u32;
        let infos: Vec<CardInfo> = st
            .targets
            .iter()
            .enumerate()
            .map(|(i, t)| {
                // Only samples that measured this target count toward loss.
                // Samples from before it was added simply have no data.
                let (measured, loss) = crate::monitor::target_loss(&st.history, &t.name, cutoff);
                worst = worst.max(loss);
                // Targets are pinged round-robin, so the newest sample usually
                // belongs to a different one — walk back to this target's own.
                let current = st
                    .history
                    .samples
                    .iter()
                    .rev()
                    .find_map(|s| s.v.get(&t.name).copied())
                    .flatten();
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
        let pace = Pace {
            auto: st.auto_interval,
            manual_ms: st.interval_ms,
            current_ms: st.current_interval_ms,
            clean_needed: st.auto_clean_needed,
        };
        (infos, worst, pace)
    };

    let (status_text, status_color) = if worst_loss == 0 {
        ("All hops healthy".to_string(), Color::rgb(0x3f, 0xb9, 0x50))
    } else if worst_loss < 10 {
        (
            format!("Minor loss ({worst_loss}%)"),
            Color::rgb(0xd2, 0x99, 0x22),
        )
    } else {
        (
            format!("Packet loss up to {worst_loss}%"),
            Color::rgb(0xf8, 0x51, 0x49),
        )
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
            text_block(pace_label(pace.auto, pace.current_ms))
                .foreground(Color::rgb(0x8b, 0x94, 0x9e)),
        ))
        .spacing(16.0)
        .grid_column(0),
        gear,
    ))
    .columns([GridLength::STAR, GridLength::Auto]);

    // Responsive layout: cards share the available width equally and their
    // sparklines scale with them. Below a minimum card width (or a short window),
    // drop the cards and show just the combined chart with a color legend.
    let size = cx.use_inner_size();
    let pad = 24.0_f64;
    let gap = 16.0_f64;
    let n = (cards_info.len() + 1).max(1) as f64;
    let avail_w = size.width - 2.0 * pad;
    let card_w = ((avail_w - (n - 1.0) * gap) / n).max(80.0);
    let compact = size.width > 0.0 && (card_w < 170.0 || size.height < 520.0);

    let device_card = vstack((
        text_block("This device").foreground(Color::rgb(0x8b, 0x94, 0x9e)),
        text_block(network_info.adapter_name())
            .foreground(Color::rgb(0x8b, 0x94, 0x9e))
            .font_size(11.0),
        text_block(network_info.primary_address())
            .font_size(20.0)
            .bold(),
        text_block(network_info.connection_details(now_ms()))
            .foreground(Color::rgb(0x8b, 0x94, 0x9e))
            .font_size(11.0),
    ))
    .spacing(4.0)
    .padding(Thickness::uniform(16.0))
    .background(Color::rgb(0x16, 0x1b, 0x22))
    .width(card_w);

    let chart_w = if avail_w > 160.0 {
        avail_w
    } else {
        chart::W as f64
    };
    let chart_h = if compact {
        // Fill the vertical space left under the header + legend.
        (size.height - 150.0).max(180.0)
    } else {
        chart::H as f64
    };

    let chart = component(
        chart_view,
        ChartProps {
            shared: shared.clone(),
            frame: tick,
            window_mins,
            width: chart_w.round() as i32,
            height: chart_h.round() as i32,
        },
    );

    let dashboard = if compact {
        let legend = hstack(
            cards_info
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let (r, g, b) = chart::COLORS[i % chart::COLORS.len()];
                    hstack((
                        text_block("\u{25CF}").foreground(Color::rgb(r, g, b)),
                        text_block(c.name.clone()).foreground(Color::rgb(0x8b, 0x94, 0x9e)),
                    ))
                    .spacing(6.0)
                    .into()
                })
                .collect::<Vec<Element>>(),
        )
        .spacing(16.0);

        vstack((header, legend, chart))
            .spacing(16.0)
            .padding(Thickness::uniform(24.0))
            .with_key("dashboard-compact")
    } else {
        let cards = std::iter::once(device_card.into())
            .chain(cards_info.iter().map(|c| {
                card_element(
                    &shared,
                    &c.name,
                    &c.ip,
                    c.index,
                    c.current,
                    c.loss,
                    c.measured,
                    tick,
                    window_mins,
                    card_w,
                )
            }))
            .collect::<Vec<Element>>();
        let cards_row = hstack(cards).spacing(16.0);

        vstack((
            header,
            cards_row,
            text_block("Latency over time (ms) - red marks = packet dropped")
                .foreground(Color::rgb(0x8b, 0x94, 0x9e))
                .font_size(14.0),
            chart,
        ))
        .spacing(16.0)
        .padding(Thickness::uniform(24.0))
        .with_key("dashboard-normal")
    };

    let overlay: Element = if settings_open {
        settings_panel(SettingsCtx {
            shared: shared.clone(),
            interval_idx,
            interval_ms: pace.manual_ms,
            auto_status: pace_status(pace.auto, pace.current_ms, pace.clean_needed),
            window_idx,
            alert_threshold,
            targets: edit_targets,
            editing,
            notification_status,
            set_open: set_settings_open,
            set_interval_idx,
            set_window_idx,
            set_alert_threshold,
            set_targets,
            set_editing,
            set_notification_status,
            form_tick,
            set_form_tick,
        })
    } else {
        Element::Empty
    };

    grid((dashboard, overlay)).provide(&gpu_context(), Some(gpu))
}
