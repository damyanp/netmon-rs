//! Per-target status cards: name, IP, current latency (or drop), packet-loss %,
//! and a small sparkline drawn into its own `SurfaceImageSource` with the shared
//! device.

use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::core::*;
use windows_numerics::{Matrix3x2, Vector2};
use windows_reactor::*;

use crate::device::{Gpu, gpu_context, is_device_lost};
use crate::monitor::{Shared, now_ms};
use crate::ui::chart::COLORS;

const SPARK_H: i32 = 40;

fn d2d_color(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}

/// Status color by packet loss, matching the web dashboard thresholds.
fn loss_color(loss_pct: u32) -> Color {
    if loss_pct == 0 {
        Color::rgb(0x3f, 0xb9, 0x50)
    } else if loss_pct < 10 {
        Color::rgb(0xd2, 0x99, 0x22)
    } else {
        Color::rgb(0xf8, 0x51, 0x49)
    }
}

/// Build one card element, `card_w` DIPs wide. Sparkline data is read live from
/// `shared`.
#[allow(clippy::too_many_arguments)]
pub fn card_element(
    shared: &Shared,
    name: &str,
    ip: &str,
    index: usize,
    current: Option<u32>,
    loss_pct: u32,
    sample_count: usize,
    revision: u64,
    window_mins: i64,
    card_w: f64,
) -> Element {
    let latency = match current {
        Some(ms) => format!("{ms} ms"),
        None => "-- drop".to_string(),
    };

    let spark_w = (card_w - 32.0).max(80.0).round() as i32;
    let spark = component(
        spark_view,
        SparkProps {
            shared: shared.clone(),
            index,
            revision,
            window_mins,
            width: spark_w,
        },
    );

    vstack((
        hstack((
            text_block(name).foreground(Color::rgb(0x8b, 0x94, 0x9e)),
            text_block("\u{25CF}").foreground(loss_color(loss_pct)),
        ))
        .spacing(8.0),
        text_block(ip).foreground(Color::rgb(0x8b, 0x94, 0x9e)).font_size(11.0),
        text_block(latency).font_size(28.0).bold(),
        text_block(format!("{loss_pct}% loss ({sample_count} samples)"))
            .foreground(Color::rgb(0x8b, 0x94, 0x9e))
            .font_size(12.0),
        spark,
    ))
    .spacing(4.0)
    .padding(Thickness::uniform(16.0))
    .background(Color::rgb(0x16, 0x1b, 0x22))
    .width(card_w)
    .into()
}

#[derive(Clone)]
pub struct SparkProps {
    pub shared: Shared,
    pub index: usize,
    pub revision: u64,
    pub window_mins: i64,
    pub width: i32,
}

impl PartialEq for SparkProps {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
            && self.revision == other.revision
            && self.window_mins == other.window_mins
            && self.width == other.width
    }
}

fn spark_view(props: &SparkProps, cx: &mut RenderCx) -> Element {
    let gpu = cx.use_context(&gpu_context());
    let device = gpu.as_ref().and_then(Gpu::device);
    let (surface, set_surface) = cx.use_state::<Option<SurfaceImageSource>>(None);

    let w = props.width.max(80);
    let props = props.clone();
    let dev = device.clone();
    let gpu_effect = gpu.clone();
    cx.use_effect((device.clone(), props.revision, props.window_mins, w), move || {
        match dev.as_ref() {
            Some(dev) => match build_spark(dev, &props, w) {
                Ok(sis) => set_surface.call(Some(sis)),
                Err(e) if is_device_lost(e.code()) => {
                    if let Some(g) = gpu_effect.as_ref() {
                        g.request_recovery();
                    }
                }
                Err(e) => eprintln!("spark: draw failed: {e}"),
            },
            None => set_surface.call(None),
        }
    });

    match surface {
        Some(sis) => Image::new(sis.into())
            .width(w as f64)
            .height(SPARK_H as f64)
            .into(),
        None => text_block("").height(SPARK_H as f64).into(),
    }
}

fn build_spark(
    device: &crate::device::Device,
    props: &SparkProps,
    spark_w: i32,
) -> Result<SurfaceImageSource> {
    let vals: Vec<Option<Option<u32>>> = {
        let st = props.shared.lock().unwrap();
        let name = st.targets.get(props.index).map(|t| t.name.clone());
        let cutoff = now_ms() - props.window_mins * 60_000;
        match name {
            Some(name) => st
                .history
                .samples
                .iter()
                .filter(|s| s.t >= cutoff)
                .map(|s| s.v.get(&name).copied())
                .collect(),
            None => Vec::new(),
        }
    };

    let surface = SurfaceImageSource::new(spark_w, SPARK_H)?;
    surface.set_device(device.d2d_device())?;
    let (ctx, (ox, oy)) = surface.begin_draw::<ID2D1DeviceContext>(0, 0, spark_w, SPARK_H)?;

    let (r, g, b) = COLORS[props.index % COLORS.len()];
    unsafe {
        ctx.SetTransform(&Matrix3x2::translation(ox as f32, oy as f32));
        ctx.Clear(Some(&d2d_color(0x16, 0x1b, 0x22, 1.0)));

        let (w, h) = (spark_w as f32, SPARK_H as f32);
        let max = vals.iter().filter_map(|v| v.flatten()).max().unwrap_or(0).max(50) as f32;
        let n = vals.len();
        let line = ctx.CreateSolidColorBrush(&d2d_color(r, g, b, 1.0), None)?;
        let drop = ctx.CreateSolidColorBrush(&d2d_color(0xf8, 0x51, 0x49, 0.5), None)?;

        let x_at = |i: usize| -> f32 {
            if n <= 1 { 0.0 } else { (i as f32 / (n - 1) as f32) * w }
        };
        let mut prev: Option<Vector2> = None;
        for (i, v) in vals.iter().enumerate() {
            match v {
                Some(Some(v)) => {
                    let y = h - (*v as f32 / max) * (h - 4.0) - 2.0;
                    let pt = Vector2 { x: x_at(i), y };
                    if let Some(p0) = prev {
                        ctx.DrawLine(p0, pt, &line, 1.5, None);
                    }
                    prev = Some(pt);
                }
                Some(None) => {
                    let x = x_at(i);
                    let rect = D2D_RECT_F { left: x - 1.0, top: 0.0, right: x + 1.0, bottom: h };
                    ctx.FillRectangle(&rect, &drop);
                    prev = None;
                }
                None => prev = None,
            }
        }
    }

    surface.end_draw()?;
    Ok(surface)
}
