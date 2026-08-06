//! Latency-over-time chart, drawn on demand into a `CanvasImageSource` with
//! Direct2D/DirectWrite. Only redraws when the data revision, window, or size
//! changes — no per-frame swap chain.

use std::collections::HashSet;

use windows::core::*;
use windows_canvas::{CanvasImageSource, ColorF, DrawingSession, Rect, TextAlignment, TextFormat};
use windows_numerics::Vector2;
use windows_reactor::*;

use crate::device::{Gpu, gpu_context};
use crate::monitor::{Shared, now_ms};

/// Fallback size, used before the real layout size is known.
pub const W: i32 = 960;
pub const H: i32 = 320;

/// Series colors, matching the original web dashboard.
pub const COLORS: [(u8, u8, u8); 5] = [
    (0x58, 0xa6, 0xff),
    (0x3f, 0xb9, 0x50),
    (0xd2, 0x99, 0x22),
    (0xf7, 0x78, 0xba),
    (0xa3, 0x71, 0xf7),
];

fn color(r: u8, g: u8, b: u8) -> ColorF {
    ColorF::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
}

/// Props are compared cheaply: the chart rebuilds only when the frame counter,
/// window, or draw size changes, not on every re-render. `frame` advances on the
/// UI timer so the plot scrolls smoothly even while no new sample has landed.
#[derive(Clone)]
pub struct ChartProps {
    pub shared: Shared,
    pub frame: u64,
    pub window_mins: i64,
    pub width: i32,
    pub height: i32,
}

impl PartialEq for ChartProps {
    fn eq(&self, other: &Self) -> bool {
        self.frame == other.frame
            && self.window_mins == other.window_mins
            && self.width == other.width
            && self.height == other.height
    }
}

/// Reactor component: draws the chart into a surface using the shared device.
pub fn chart_view(props: &ChartProps, cx: &mut RenderCx) -> Element {
    let gpu = cx.use_context(&gpu_context());
    let device = gpu.as_ref().and_then(Gpu::device);
    let surface = cx.use_ref::<Option<CanvasImageSource>>(None);

    let (w, h) = (props.width.max(160), props.height.max(120));
    let props = props.clone();
    let dev = device.clone();
    let gpu_effect = gpu.clone();
    let surface_effect = surface.clone();
    cx.use_effect(
        (device.clone(), props.frame, props.window_mins, w, h),
        move || match dev.as_ref() {
            Some(dev) => match build_surface(dev, &props, w, h) {
                Ok(Some(sis)) => surface_effect.set(Some(sis)),
                Ok(None) => {
                    if let Some(g) = gpu_effect.as_ref() {
                        g.request_recovery();
                    }
                }
                Err(e) => eprintln!("chart: draw failed: {e}"),
            },
            None => surface_effect.set(None),
        },
    );

    match surface.borrow().clone() {
        Some(sis) => Image::new(sis.image_source())
            .width(w as f64)
            .height(h as f64)
            .into(),
        None => text_block("Preparing chart\u{2026}").into(),
    }
}

/// Snapshot of what the chart needs, taken under the lock so drawing runs
/// lock-free. Each value is `None` if the target wasn't measured in that sample
/// (they are pinged round-robin, so most samples cover a single target),
/// `Some(None)` for a dropped packet, or `Some(Some(ms))` for a reading.
///
/// The x axis always spans the full `t_start..t_end` window, whether or not
/// there is data to fill it, so the plot scrolls rather than stretching.
struct ChartData {
    names: Vec<String>,
    samples: Vec<(i64, Vec<Option<Option<u32>>>)>,
    t_start: i64,
    t_end: i64,
}

fn snapshot(props: &ChartProps) -> ChartData {
    let st = props.shared.lock().unwrap();
    let names: Vec<String> = st.targets.iter().map(|t| t.name.clone()).collect();
    let t_end = now_ms();
    let t_start = t_end - props.window_mins * 60_000;

    let all = &st.history.samples;
    let first_visible = all.partition_point(|s| s.t < t_start);

    // Carry in each series' last reading from before the window so its line
    // enters at the left edge instead of starting part-way across the plot.
    let mut lead = Vec::new();
    let mut carried: HashSet<&str> = HashSet::new();
    for sample in all[..first_visible].iter().rev() {
        if carried.len() >= names.len() {
            break;
        }
        if sample.v.keys().any(|k| !carried.contains(k.as_str())) {
            carried.extend(sample.v.keys().map(String::as_str));
            lead.push(sample);
        }
    }
    lead.reverse();

    let samples = lead
        .into_iter()
        .chain(all[first_visible..].iter())
        .map(|s| {
            let vals = names.iter().map(|n| s.v.get(n).copied()).collect();
            (s.t, vals)
        })
        .collect();

    ChartData {
        names,
        samples,
        t_start,
        t_end,
    }
}

fn build_surface(
    device: &crate::device::Device,
    props: &ChartProps,
    w: i32,
    h: i32,
) -> Result<Option<CanvasImageSource>> {
    let data = snapshot(props);

    let surface = CanvasImageSource::new(device.gpu_device(), w as f32, h as f32, 1.0)?;
    let mut draw_result: Result<()> = Ok(());
    let presented = surface.draw(
        ColorF::new(
            0x16 as f32 / 255.0,
            0x1b as f32 / 255.0,
            0x22 as f32 / 255.0,
            1.0,
        ),
        |session| {
            draw_result = draw_chart(session, &data, w as f32, h as f32);
        },
    )?;
    draw_result?;

    if presented {
        Ok(Some(surface))
    } else {
        Ok(None)
    }
}

fn draw_chart(session: &DrawingSession<'_>, data: &ChartData, w: f32, h: f32) -> Result<()> {
    let (pad_l, pad_r, pad_t, pad_b) = (44.0_f32, 12.0_f32, 12.0_f32, 24.0_f32);
    let plot_w = w - pad_l - pad_r;
    let plot_h = h - pad_t - pad_b;

    let label_fmt_trailing =
        TextFormat::new("Segoe UI", 11.0)?.with_alignment(TextAlignment::Trailing);
    let label_fmt_center = TextFormat::new("Segoe UI", 11.0)?.with_alignment(TextAlignment::Center);

    let grid_brush = session.create_solid_brush(ColorF::new(1.0, 1.0, 1.0, 0.06))?;
    let text_brush = session.create_solid_brush(color(0x8b, 0x94, 0x9e))?;
    let drop_brush = session.create_solid_brush(ColorF::new(0.97, 0.32, 0.29, 0.25))?;

    // Vertical scale: 10% headroom above the largest latency, floor of 50ms.
    let max_val = data
        .samples
        .iter()
        .flat_map(|(_, v)| v.iter().filter_map(|x| x.flatten()))
        .max()
        .unwrap_or(0)
        .max(50) as f32;
    let max_y = max_val * 1.1;

    // Horizontal gridlines + y labels.
    for g in 0..=4 {
        let y = pad_t + (g as f32 / 4.0) * plot_h;
        session.draw_line(
            Vector2 { x: pad_l, y },
            Vector2 { x: w - pad_r, y },
            &grid_brush,
            1.0,
        );
        let val = (max_y * (1.0 - g as f32 / 4.0)).round() as i32;
        let rect = Rect::new(0.0, y - 8.0, pad_l - 6.0, y + 8.0);
        session.draw_text(&format!("{val}ms"), &label_fmt_trailing, &rect, &text_brush);
    }

    // The x axis is pinned to the window, not to the samples we happen to have,
    // so the plot scrolls left at a steady rate instead of stretching to fit.
    let window_ms = (data.t_end - data.t_start).max(1);
    let span = window_ms as f32;
    let x_at = |t: i64| -> f32 { pad_l + ((t - data.t_start) as f32 / span) * plot_w };
    let y_at = |v: u32| -> f32 { pad_t + plot_h - (v as f32 / max_y) * plot_h };

    // Drop markers: a faint vertical bar wherever a measured target dropped.
    // Samples that predate a target (no data) are left blank, not marked.
    for (t, vals) in data.samples.iter() {
        if *t >= data.t_start && vals.iter().any(|v| matches!(v, Some(None))) {
            let x = x_at(*t);
            let rect = Rect::new(x - 1.0, pad_t, x + 1.0, pad_t + plot_h);
            session.fill_rect(&rect, &drop_brush);
        }
    }

    // Series polylines.
    for (si, _name) in data.names.iter().enumerate() {
        let (r, g, b) = COLORS[si % COLORS.len()];
        let brush = session.create_solid_brush(color(r, g, b))?;
        let mut prev: Option<Vector2> = None;
        for (t, vals) in data.samples.iter() {
            match vals.get(si).copied() {
                Some(Some(Some(v))) => {
                    let pt = Vector2 {
                        x: x_at(*t),
                        y: y_at(v),
                    };
                    if let Some(p0) = prev
                        && let Some(p0) = clip_left(p0, pt, pad_l)
                    {
                        session.draw_line(p0, pt, &brush, 1.8);
                    }
                    prev = Some(pt);
                }
                // A dropped packet breaks the line...
                Some(Some(None)) => prev = None,
                // ...but a sample that simply didn't measure this target (they
                // are pinged round-robin, one per sample) is skipped so the
                // series stays connected across its neighbours' samples.
                _ => {}
            }
        }
    }

    // X axis time labels, evenly spaced across the window.
    for g in 0..=4 {
        let t = data.t_start + (window_ms * g) / 4;
        let x = x_at(t);
        let rect = Rect::new(x - 34.0, h - pad_b + 2.0, x + 34.0, h);
        session.draw_text(
            &time_label(t, window_ms),
            &label_fmt_center,
            &rect,
            &text_brush,
        );
    }

    Ok(())
}

/// Trim a segment that starts left of the plot area so it enters exactly at the
/// axis. Returns `None` when the whole segment is off-screen.
fn clip_left(p0: Vector2, p1: Vector2, min_x: f32) -> Option<Vector2> {
    if p0.x >= min_x {
        return Some(p0);
    }
    if p1.x <= min_x {
        return None;
    }
    let f = (min_x - p0.x) / (p1.x - p0.x);
    Some(Vector2 {
        x: min_x,
        y: p0.y + (p1.y - p0.y) * f,
    })
}

/// Local time from epoch millis. Short windows get seconds too, otherwise every
/// label on a one-minute chart would read the same.
fn time_label(t_ms: i64, window_ms: i64) -> String {
    let secs = (t_ms + local_offset_ms()) / 1000;
    let day_secs = secs.rem_euclid(86_400);
    let (h, m, s) = (day_secs / 3600, (day_secs % 3600) / 60, day_secs % 60);
    if window_ms <= 10 * 60_000 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{h:02}:{m:02}")
    }
}

/// Offset from UTC to local time, in ms, computed once. Compares the OS local
/// and system clocks (sampled back-to-back, so at most one day apart).
fn local_offset_ms() -> i64 {
    use std::sync::LazyLock;
    use windows::minwinbase::SYSTEMTIME;
    use windows::sysinfoapi::{GetLocalTime, GetSystemTime};
    static OFFSET: LazyLock<i64> = LazyLock::new(|| unsafe {
        let l = GetLocalTime();
        let u = GetSystemTime();
        let to_secs = |s: &SYSTEMTIME| -> i64 {
            s.wDay as i64 * 86_400
                + s.wHour as i64 * 3600
                + s.wMinute as i64 * 60
                + s.wSecond as i64
        };
        (to_secs(&l) - to_secs(&u)) * 1000
    });
    *OFFSET
}
