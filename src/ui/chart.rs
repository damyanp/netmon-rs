//! Latency-over-time chart, drawn on demand into a `CanvasImageSource` with
//! Direct2D/DirectWrite. Only redraws when the data revision, window, or size
//! changes — no per-frame swap chain.

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

/// Props are compared cheaply: the chart rebuilds only when the revision,
/// window, or draw size changes, not on every re-render.
#[derive(Clone)]
pub struct ChartProps {
    pub shared: Shared,
    pub revision: u64,
    pub window_mins: i64,
    pub width: i32,
    pub height: i32,
}

impl PartialEq for ChartProps {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision
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
        (device.clone(), props.revision, props.window_mins, w, h),
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

/// Snapshot of what the chart needs, taken under the lock so drawing runs lock-free.
/// Each value is `None` if the target wasn't measured in that sample (no data),
/// `Some(None)` for a dropped packet, or `Some(Some(ms))` for a reading.
struct ChartData {
    names: Vec<String>,
    samples: Vec<(i64, Vec<Option<Option<u32>>>)>,
}

fn snapshot(props: &ChartProps) -> ChartData {
    let st = props.shared.lock().unwrap();
    let names: Vec<String> = st.targets.iter().map(|t| t.name.clone()).collect();
    let cutoff = now_ms() - props.window_mins * 60_000;
    let samples = st
        .history
        .samples
        .iter()
        .filter(|s| s.t >= cutoff)
        .map(|s| {
            let vals = names.iter().map(|n| s.v.get(n).copied()).collect();
            (s.t, vals)
        })
        .collect();
    ChartData { names, samples }
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

    let n = data.samples.len();
    let x_at = |i: usize| -> f32 {
        if n <= 1 {
            pad_l
        } else {
            pad_l + (i as f32 / (n - 1) as f32) * plot_w
        }
    };
    let y_at = |v: u32| -> f32 { pad_t + plot_h - (v as f32 / max_y) * plot_h };

    // Drop markers: a faint vertical bar wherever a measured target dropped.
    // Samples that predate a target (no data) are left blank, not marked.
    for (i, (_, vals)) in data.samples.iter().enumerate() {
        if vals.iter().any(|v| matches!(v, Some(None))) {
            let x = x_at(i);
            let rect = Rect::new(x - 1.0, pad_t, x + 1.0, pad_t + plot_h);
            session.fill_rect(&rect, &drop_brush);
        }
    }

    // Series polylines.
    for (si, _name) in data.names.iter().enumerate() {
        let (r, g, b) = COLORS[si % COLORS.len()];
        let brush = session.create_solid_brush(color(r, g, b))?;
        let mut prev: Option<Vector2> = None;
        for (i, (_, vals)) in data.samples.iter().enumerate() {
            match vals.get(si).copied().flatten() {
                Some(Some(v)) => {
                    let pt = Vector2 {
                        x: x_at(i),
                        y: y_at(v),
                    };
                    if let Some(p0) = prev {
                        session.draw_line(p0, pt, &brush, 1.8);
                    }
                    prev = Some(pt);
                }
                _ => prev = None,
            }
        }
    }

    // X axis time labels.
    if n > 1 {
        for g in 0..=4 {
            let idx = ((g as f32 / 4.0) * (n - 1) as f32).round() as usize;
            let x = x_at(idx);
            let rect = Rect::new(x - 30.0, h - pad_b + 2.0, x + 30.0, h);
            session.draw_text(
                &time_label(data.samples[idx].0),
                &label_fmt_center,
                &rect,
                &text_brush,
            );
        }
    }

    Ok(())
}

/// Local HH:MM from epoch millis.
fn time_label(t_ms: i64) -> String {
    let secs = (t_ms + local_offset_ms()) / 1000;
    let day_secs = secs.rem_euclid(86_400);
    let (h, m) = (day_secs / 3600, (day_secs % 3600) / 60);
    format!("{h:02}:{m:02}")
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
