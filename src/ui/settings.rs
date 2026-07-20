//! Slide-out settings panel: monitoring controls (ping interval + display
//! window), a clear-data action, and a target list. Editing a target opens a
//! dedicated form (full-width fields, committed on Save) rather than editing in
//! place — in-place controlled inputs raced with the per-keystroke persistence
//! and truncated longer values like MAC addresses.

use windows_reactor::*;

use crate::config::{self, Target, WINDOW_MINS, clamp_interval};
use crate::monitor::{self, Shared};

pub const INTERVAL_MS: [u32; 6] = [1000, 2000, 5000, 10000, 30000, 60000];
const INTERVAL_LABELS: [&str; 6] = ["1 s", "2 s", "5 s", "10 s", "30 s", "60 s"];
const WINDOW_LABELS: [&str; 5] = ["10 min", "30 min", "1 hour", "3 hours", "6 hours"];

const MUTED: Color = Color::rgb(0x8b, 0x94, 0x9e);
const PANEL_BG: Color = Color::rgb(0x0d, 0x11, 0x17);
const FIELD_W: f64 = 600.0;

/// Which target (if any) the edit form is open for.
#[derive(Clone, PartialEq)]
pub enum Editing {
    Closed,
    New,
    Row(usize),
}

pub fn interval_to_index(ms: u32) -> i32 {
    INTERVAL_MS.iter().position(|&x| x == ms).unwrap_or(0) as i32
}

pub fn window_to_index(mins: i64) -> i32 {
    WINDOW_MINS.iter().position(|&x| x == mins).unwrap_or(0) as i32
}

/// State + setters the panel needs to read current values and apply changes.
pub struct SettingsCtx {
    pub shared: Shared,
    pub interval_idx: i32,
    pub window_idx: i32,
    pub targets: Vec<Target>,
    pub editing: Editing,
    pub set_open: SetState<bool>,
    pub set_interval_idx: SetState<i32>,
    pub set_window_idx: SetState<i32>,
    pub set_targets: SetState<Vec<Target>>,
    pub set_editing: SetState<Editing>,
    pub form_tick: i32,
    pub set_form_tick: SetState<i32>,
}

/// Build the settings overlay panel.
pub fn settings_panel(ctx: SettingsCtx) -> Element {
    let interval_ms = clamp_interval(INTERVAL_MS[ctx.interval_idx.clamp(0, 5) as usize]);
    let window_mins = WINDOW_MINS[ctx.window_idx.clamp(0, 4) as usize];

    let content: Element = if matches!(ctx.editing, Editing::Closed) {
        list_view(&ctx, interval_ms, window_mins)
    } else {
        component(
            edit_form,
            FormProps {
                shared: ctx.shared.clone(),
                editing: ctx.editing.clone(),
                targets: ctx.targets.clone(),
                interval_ms,
                window_mins,
                set_targets: ctx.set_targets.clone(),
                set_editing: ctx.set_editing.clone(),
                form_tick: ctx.form_tick,
                set_form_tick: ctx.set_form_tick.clone(),
            },
        )
    };

    scroll_viewer(content)
        .background(PANEL_BG)
        .padding(Thickness::uniform(24.0))
        .width(700.0)
        .horizontal_alignment(HorizontalAlignment::Right)
        .vertical_alignment(VerticalAlignment::Stretch)
        .into()
}

/// The default panel view: monitoring controls, clear-data, and the target list.
fn list_view(ctx: &SettingsCtx, interval_ms: u32, window_mins: i64) -> Element {
    let interval_combo = ComboBox::new(INTERVAL_LABELS)
        .header("Ping every")
        .selected_index(ctx.interval_idx)
        .on_selection_changed({
            let shared = ctx.shared.clone();
            let targets = ctx.targets.clone();
            let set_interval_idx = ctx.set_interval_idx.clone();
            move |i: i32| {
                if i >= 0 {
                    let ms = clamp_interval(INTERVAL_MS[i.clamp(0, 5) as usize]);
                    shared.lock().unwrap().interval_ms = ms;
                    config::save_settings(ms, window_mins, &targets);
                    set_interval_idx.call(i);
                }
            }
        });

    let window_combo = ComboBox::new(WINDOW_LABELS)
        .header("Window")
        .selected_index(ctx.window_idx)
        .on_selection_changed({
            let targets = ctx.targets.clone();
            let set_window_idx = ctx.set_window_idx.clone();
            move |i: i32| {
                if i >= 0 {
                    let mins = WINDOW_MINS[i.clamp(0, 4) as usize];
                    config::save_settings(interval_ms, mins, &targets);
                    set_window_idx.call(i);
                }
            }
        });

    let clear_button = button("Clear data").icon(Symbol::Delete).on_click({
        let shared = ctx.shared.clone();
        move || monitor::clear_history(&shared)
    });

    let count = ctx.targets.len();
    let rows: Vec<Element> = ctx
        .targets
        .iter()
        .enumerate()
        .map(|(r, t)| target_row(ctx, r, t, count, interval_ms, window_mins))
        .collect();

    let add_button = button("Add target").icon(Symbol::Add).on_click({
        let set_editing = ctx.set_editing.clone();
        move || set_editing.call(Editing::New)
    });

    let header = grid((
        text_block("Settings").font_size(18.0).bold().grid_column(0),
        button("")
            .icon(Symbol::Cancel)
            .subtle()
            .on_click(ctx.set_open.setter(false))
            .grid_column(1),
    ))
    .columns([GridLength::STAR, GridLength::Auto]);

    vstack((
        header,
        text_block("Monitoring").font_size(14.0).bold(),
        hstack((interval_combo, window_combo)).spacing(16.0),
        text_block("History for the current session only \u{2014} cleared on restart.")
            .foreground(MUTED)
            .font_size(12.0),
        clear_button,
        text_block("Targets").font_size(14.0).bold(),
        vstack(rows).spacing(8.0),
        add_button,
    ))
    .spacing(16.0)
    .into()
}

/// One read-only target row with edit / reorder / delete actions.
fn target_row(
    ctx: &SettingsCtx,
    r: usize,
    target: &Target,
    count: usize,
    interval_ms: u32,
    window_mins: i64,
) -> Element {
    // Reorder / delete write straight through; only field edits need the form.
    let apply = {
        let shared = ctx.shared.clone();
        let set_targets = ctx.set_targets.clone();
        move |new: Vec<Target>| {
            shared.lock().unwrap().targets = new.clone();
            config::save_settings(interval_ms, window_mins, &new);
            set_targets.call(new);
        }
    };

    let mac_label = match &target.mac {
        Some(m) if !m.is_empty() => m.clone(),
        _ => "\u{2014}".to_string(),
    };

    let info = vstack((
        text_block(target.name.clone()).bold(),
        text_block(target.ip.clone())
            .foreground(MUTED)
            .font_size(12.0),
        text_block(format!("MAC: {mac_label}"))
            .foreground(MUTED)
            .font_size(11.0),
    ))
    .spacing(2.0)
    .width(380.0);

    let edit = button("").icon(Symbol::Edit).subtle().on_click({
        let set_editing = ctx.set_editing.clone();
        move || set_editing.call(Editing::Row(r))
    });

    let up = button("\u{2191}").subtle().enabled(r > 0).on_click({
        let apply = apply.clone();
        let targets = ctx.targets.clone();
        move || {
            let mut new = targets.clone();
            new.swap(r - 1, r);
            apply(new);
        }
    });

    let down = button("\u{2193}")
        .subtle()
        .enabled(r + 1 < count)
        .on_click({
            let apply = apply.clone();
            let targets = ctx.targets.clone();
            move || {
                let mut new = targets.clone();
                new.swap(r, r + 1);
                apply(new);
            }
        });

    let remove = button("").icon(Symbol::Delete).subtle().on_click({
        let apply = apply.clone();
        let targets = ctx.targets.clone();
        move || {
            let mut new = targets.clone();
            new.remove(r);
            apply(new);
        }
    });

    hstack((info, edit, up, down, remove)).spacing(8.0).into()
}

/// Props for the edit form. Setters are excluded from equality (their identity
/// is stable), so the form only re-renders when the edited data changes.
#[derive(Clone)]
struct FormProps {
    shared: Shared,
    editing: Editing,
    targets: Vec<Target>,
    interval_ms: u32,
    window_mins: i64,
    set_targets: SetState<Vec<Target>>,
    set_editing: SetState<Editing>,
    form_tick: i32,
    set_form_tick: SetState<i32>,
}

impl PartialEq for FormProps {
    fn eq(&self, o: &Self) -> bool {
        self.editing == o.editing
            && self.targets == o.targets
            && self.interval_ms == o.interval_ms
            && self.window_mins == o.window_mins
            && self.form_tick == o.form_tick
    }
}

/// Full-detail edit form for a single target. Fields live in refs, not state:
/// this component is nested under wrapper elements that don't change while
/// editing, so a use_state write wouldn't re-render it (the reconciler skips
/// clean subtrees). Refs capture keystrokes without needing a re-render; the
/// WinUI text boxes hold the visible text, and Save reads the refs.
fn edit_form(props: &FormProps, cx: &mut RenderCx) -> Element {
    let source = match props.editing {
        Editing::Row(i) => props.targets.get(i).cloned(),
        _ => None,
    };
    let init_name = source.as_ref().map(|t| t.name.clone()).unwrap_or_default();
    let init_host = source.as_ref().map(|t| t.ip.clone()).unwrap_or_default();
    let init_mac = source
        .as_ref()
        .and_then(|t| t.mac.clone())
        .unwrap_or_default();

    let name = cx.use_ref(init_name);
    let host = cx.use_ref(init_host);
    let mac = cx.use_ref(init_mac);
    let status = cx.use_ref(String::new());

    let title = if matches!(props.editing, Editing::New) {
        "Add target"
    } else {
        "Edit target"
    };

    let save = button("Save").icon(Symbol::Save).accent().on_click({
        let props = props.clone();
        let (name, host, mac) = (name.clone(), host.clone(), mac.clone());
        move || {
            let target = Target {
                name: name.borrow().trim().to_string(),
                ip: host.borrow().trim().to_string(),
                mac: {
                    let m = mac.borrow();
                    let m = m.trim();
                    if m.is_empty() {
                        None
                    } else {
                        Some(m.to_string())
                    }
                },
            };
            let mut new = props.targets.clone();
            match props.editing {
                Editing::New => new.push(target),
                Editing::Row(i) if i < new.len() => new[i] = target,
                _ => new.push(target),
            }
            props.shared.lock().unwrap().targets = new.clone();
            config::save_settings(props.interval_ms, props.window_mins, &new);
            props.set_targets.call(new);
            props.set_editing.call(Editing::Closed);
        }
    });

    let cancel = button("Cancel").on_click({
        let set_editing = props.set_editing.clone();
        move || set_editing.call(Editing::Closed)
    });

    // Fill the MAC from the ARP table for the currently-entered host. Bumps the
    // form tick so the (nested) form re-renders and shows the resolved value.
    let resolve = button("Resolve").icon(Symbol::Sync).on_click({
        let host = host.clone();
        let mac = mac.clone();
        let status = status.clone();
        let set_form_tick = props.set_form_tick.clone();
        let tick = props.form_tick;
        move || {
            let ip = host.borrow().trim().to_string();
            if ip.is_empty() {
                status.set("Enter a host/IP first.".into());
            } else if let Some(found) = monitor::resolve_mac_for_ip(&ip, 1000) {
                mac.set(found);
                status.set(String::new());
            } else {
                status.set(format!("No MAC found for {ip} (must be on your LAN)."));
            }
            set_form_tick.call(tick + 1);
        }
    });

    let name_val = name.get_cloned();
    let host_val = host.get_cloned();
    let mac_val = mac.get_cloned();
    let status_val = status.get_cloned();

    vstack((
        text_block(title).font_size(18.0).bold(),
        text_box(name_val)
            .header("Name")
            .placeholder_text("e.g. Gateway")
            .width(FIELD_W)
            .on_text_changed(move |t: String| name.set(t)),
        text_box(host_val)
            .header("Host / IP")
            .placeholder_text("e.g. 192.168.1.1 or example.com")
            .width(FIELD_W)
            .on_text_changed(move |t: String| host.set(t)),
        text_box(mac_val)
            .header("MAC address (optional \u{2014} keeps the IP pinned if it changes)")
            .placeholder_text("e.g. 0C-EF-15-C1-57-90")
            .width(FIELD_W)
            .on_text_changed(move |t: String| mac.set(t)),
        hstack((
            resolve,
            text_block(status_val).foreground(MUTED).font_size(12.0),
        ))
        .spacing(12.0),
        hstack((save, cancel)).spacing(8.0),
    ))
    .spacing(16.0)
    .into()
}
