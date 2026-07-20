//! Per-target status cards: name, IP, current latency (or drop), packet-loss %,
//! and a small sparkline drawn into its own `CanvasImageSource` with the shared
//! device.

use windows::core::*;
use windows_canvas::{CanvasImageSource, ColorF, Rect};
use windows_numerics::Vector2;
use windows_reactor::*;

use crate::device::{Gpu, gpu_context};
use crate::monitor::{Shared, now_ms};
use crate::ui::chart::COLORS;

const SPARK_H: i32 = 40;

fn d2d_color(r: u8, g: u8, b: u8, a: f32) -> ColorF {
    ColorF::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a)
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
        text_block(ip)
            .foreground(Color::rgb(0x8b, 0x94, 0x9e))
            .font_size(11.0),
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
    let surface = cx.use_ref::<Option<CanvasImageSource>>(None);

    let w = props.width.max(80);
    let props = props.clone();
    let dev = device.clone();
    let gpu_effect = gpu.clone();
    let surface_effect = surface.clone();
    cx.use_effect(
        (device.clone(), props.revision, props.window_mins, w),
        move || match dev.as_ref() {
            Some(dev) => match build_spark(dev, &props, w) {
                Ok(Some(sis)) => surface_effect.set(Some(sis)),
                Ok(None) => {
                    if let Some(g) = gpu_effect.as_ref() {
                        g.request_recovery();
                    }
                }
                Err(e) => eprintln!("spark: draw failed: {e}"),
            },
            None => surface_effect.set(None),
        },
    );

    match surface.borrow().clone() {
        Some(sis) => Image::new(sis.image_source())
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
) -> Result<Option<CanvasImageSource>> {
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

    let surface = CanvasImageSource::new(device.gpu_device(), spark_w as f32, SPARK_H as f32, 1.0)?;
    let (r, g, b) = COLORS[props.index % COLORS.len()];
    let mut draw_result: Result<()> = Ok(());
    let presented = surface.draw(
        ColorF::new(
            0x16 as f32 / 255.0,
            0x1b as f32 / 255.0,
            0x22 as f32 / 255.0,
            1.0,
        ),
        |session| {
            draw_result = (|| -> Result<()> {
                let (w, h) = (spark_w as f32, SPARK_H as f32);
                let max = vals
                    .iter()
                    .filter_map(|v| v.flatten())
                    .max()
                    .unwrap_or(0)
                    .max(50) as f32;
                let n = vals.len();
                let line = session.create_solid_brush(d2d_color(r, g, b, 1.0))?;
                let drop = session.create_solid_brush(d2d_color(0xf8, 0x51, 0x49, 0.5))?;

                let x_at = |i: usize| -> f32 {
                    if n <= 1 {
                        0.0
                    } else {
                        (i as f32 / (n - 1) as f32) * w
                    }
                };
                let mut prev: Option<Vector2> = None;
                for (i, v) in vals.iter().enumerate() {
                    match v {
                        Some(Some(v)) => {
                            let y = h - (*v as f32 / max) * (h - 4.0) - 2.0;
                            let pt = Vector2 { x: x_at(i), y };
                            if let Some(p0) = prev {
                                session.draw_line(p0, pt, &line, 1.5);
                            }
                            prev = Some(pt);
                        }
                        Some(None) => {
                            let x = x_at(i);
                            let rect = Rect::new(x - 1.0, 0.0, x + 1.0, h);
                            session.fill_rect(&rect, &drop);
                            prev = None;
                        }
                        None => prev = None,
                    }
                }
                Ok(())
            })();
        },
    )?;
    draw_result?;

    if presented {
        Ok(Some(surface))
    } else {
        Ok(None)
    }
}
