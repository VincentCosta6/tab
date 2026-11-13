//! Small charts for Insights and the Factory: a progress ring drawn as
//! paths, daily stacked bars, a split bar, a legend and the change against
//! the window before. The counterpart of `insights/charts.tsx`; colors are
//! the chart tokens from `tokens.css`.

use chrono::{Duration, Local, NaiveDate, Offset};
use gpui::{
    AnyElement, App, Bounds, Hsla, IntoElement, ParentElement, PathBuilder, Pixels, Styled, Window,
    canvas, div, point, prelude::*, px, rgb,
};

use crate::theme::{Theme, size};

/// The chart tokens.
#[derive(Clone, Copy)]
pub struct Palette {
    pub ok: Hsla,
    pub attended: Hsla,
    pub failed: Hsla,
    pub planned: Hsla,
    pub unplanned: Hsla,
    pub track: Hsla,
}

pub fn palette(t: &Theme) -> Palette {
    let c = |hex: u32| -> Hsla { rgb(hex).into() };
    if t.dark {
        Palette {
            ok: c(0x16a34a),
            attended: c(0x6366f1),
            failed: c(0xef4444),
            planned: c(0x0284c7),
            unplanned: c(0xd97706),
            track: c(0x26262b),
        }
    } else {
        Palette {
            ok: c(0x15803d),
            attended: c(0x4f46e5),
            failed: c(0xc2261d),
            planned: c(0x0284c7),
            unplanned: c(0xd97706),
            track: c(0xe8e8ec),
        }
    }
}

/// A circular ring: `segments` are shares of the circle drawn clockwise
/// from 12 o'clock. `empty` draws only a dashed track: too little to say.
pub fn ring(
    t: &Theme,
    size_px: f32,
    stroke: Option<f32>,
    segments: Vec<(f64, Hsla)>,
    empty: bool,
    center: String,
    sub: Option<String>,
) -> AnyElement {
    let sw = stroke.unwrap_or_else(|| (size_px / 10.).round().max(4.));
    let track = palette(t).track;
    let font = (size_px * 0.22).round().max(11.);
    div()
        .relative()
        .flex_none()
        .size(px(size_px))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds: Bounds<Pixels>, _, window: &mut Window, _: &mut App| {
                    let r = (f32::from(bounds.size.width) - sw) / 2.;
                    let (cx, cy) = (
                        f32::from(bounds.origin.x) + f32::from(bounds.size.width) / 2.,
                        f32::from(bounds.origin.y) + f32::from(bounds.size.height) / 2.,
                    );
                    let arc = |from: f64, to: f64, dashed: bool| -> Option<gpui::Path<Pixels>> {
                        let mut b = PathBuilder::stroke(px(sw));
                        if dashed {
                            let dash = (std::f32::consts::TAU * r / 40.).max(2.);
                            b = b.dash_array(&[px(dash), px(dash)]);
                        }
                        let steps = ((to - from) * 120.).ceil().max(2.) as usize;
                        for i in 0..=steps {
                            let a = (from + (to - from) * i as f64 / steps as f64)
                                * std::f64::consts::TAU
                                - std::f64::consts::FRAC_PI_2;
                            let p = point(px(cx + r * a.cos() as f32), px(cy + r * a.sin() as f32));
                            if i == 0 {
                                b.move_to(p);
                            } else {
                                b.line_to(p);
                            }
                        }
                        b.build().ok()
                    };
                    if let Some(p) = arc(0., 1., empty) {
                        window.paint_path(p, track);
                    }
                    if empty {
                        return;
                    }
                    let gap = if segments.iter().filter(|s| s.0 > 0.).count() > 1 {
                        0.008
                    } else {
                        0.
                    };
                    let mut at = 0.;
                    for (v, color) in &segments {
                        let v = v.clamp(0., 1.);
                        if v - gap > 0.003
                            && let Some(p) = arc(at, at + v - gap, false)
                        {
                            window.paint_path(p, *color);
                        }
                        at += v;
                    }
                },
            )
            .absolute()
            .size_full(),
        )
        .child(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_size(px(font))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(t.fg)
                        .child(center),
                )
                .when_some(sub, |el, s| {
                    el.child(
                        div()
                            .text_size(px(size::TEXT_XS))
                            .text_color(t.fg_subtle)
                            .child(s),
                    )
                }),
        )
        .into_any_element()
}

/// The window's days in the person's time, oldest first, so empty days
/// still get a slot.
pub fn window_days(from: chrono::DateTime<chrono::Utc>, days: u32) -> Vec<NaiveDate> {
    let offset = Local::now().offset().fix();
    let start = from.with_timezone(&offset).date_naive();
    (0..days as i64)
        .map(|i| start + Duration::days(i))
        .collect()
}

pub fn day_label(d: NaiveDate) -> String {
    d.format("%b %-d").to_string()
}

/// Daily stacked bars: one column per day, one segment per series, the
/// tallest day at full height. `markers` put a label over their day.
pub fn stacked_bars(
    t: &Theme,
    days: &[NaiveDate],
    values: &dyn Fn(NaiveDate) -> Vec<f64>,
    colors: &[Hsla],
    height: f32,
    markers: &[(NaiveDate, String)],
    axis: bool,
) -> AnyElement {
    let totals: Vec<f64> = days.iter().map(|d| values(*d).iter().sum()).collect();
    let max = totals.iter().cloned().fold(0., f64::max).max(1e-9);
    let mut cols = div().flex().items_end().gap(px(2.)).h(px(height)).w_full();
    for (i, d) in days.iter().enumerate() {
        let vs = values(*d);
        let mut col = div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .justify_end()
            .relative();
        if totals[i] == 0. {
            col = col.child(div().h(px(2.)).rounded(px(1.)).bg(palette(t).track));
        }
        for (k, v) in vs.iter().enumerate().rev() {
            if *v > 0. {
                let h = (*v / max) as f32 * (height - 14.);
                col = col.child(
                    div()
                        .h(px(h.max(2.)))
                        .bg(colors.get(k).copied().unwrap_or(t.fg_subtle)),
                );
            }
        }
        if let Some((_, label)) = markers.iter().find(|m| m.0 == *d) {
            col = col.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .text_size(px(size::TEXT_XS))
                    .text_color(t.fg_muted)
                    .child(label.clone()),
            );
        }
        cols = cols.child(col);
    }
    let mut out = div().flex().flex_col().gap(px(4.)).w_full().child(cols);
    if axis && !days.is_empty() {
        let picks = if days.len() <= 3 {
            (0..days.len()).collect::<Vec<_>>()
        } else {
            vec![0, days.len() / 2, days.len() - 1]
        };
        out = out.child(
            div()
                .flex()
                .justify_between()
                .text_size(px(size::TEXT_XS))
                .text_color(t.fg_subtle)
                .children(picks.into_iter().map(|i| div().child(day_label(days[i])))),
        );
    }
    out.into_any_element()
}

/// Swatches and words under a chart, so color isn't the only cue.
pub fn legend(t: &Theme, series: &[(&str, Hsla)]) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .gap_x(px(12.))
        .gap_y(px(4.))
        .text_size(px(size::TEXT_SM))
        .text_color(t.fg_muted)
        .children(series.iter().map(|(label, color)| {
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .child(div().size(px(8.)).rounded(px(2.)).bg(*color))
                .child(label.to_string())
        }))
        .into_any_element()
}

/// A horizontal bar split into parts; `max` sets the full width, so bars
/// line up against each other.
pub fn split_bar(t: &Theme, parts: &[(f64, Hsla)], max: f64) -> AnyElement {
    let mut bar = div()
        .flex()
        .h(px(8.))
        .w_full()
        .rounded(px(4.))
        .overflow_hidden()
        .bg(palette(t).track);
    for (v, c) in parts {
        if *v > 0. && max > 0. {
            bar = bar.child(div().h_full().w(gpui::relative((*v / max) as f32)).bg(*c));
        }
    }
    bar.into_any_element()
}

/// Which way a number moved since the window before.
#[derive(Clone, Copy, PartialEq)]
pub enum Better {
    Up,
    Down,
    None,
}

/// How a number moved since the previous window: points for rates, a
/// percentage for the rest.
pub fn delta(
    t: &Theme,
    now: Option<f64>,
    before: Option<f64>,
    is_rate: bool,
    better: Better,
    enough: bool,
    vs: &str,
) -> AnyElement {
    let quiet = |s: &str| {
        div()
            .text_size(px(size::TEXT_XS))
            .text_color(t.fg_subtle)
            .child(s.to_string())
            .into_any_element()
    };
    let (Some(now), Some(before)) = (now, before) else {
        return quiet(if before.is_none_or(|b| b == 0.) {
            "nothing earlier to compare"
        } else {
            "too few runs to compare"
        });
    };
    if !enough {
        return quiet(if before == 0. {
            "nothing earlier to compare"
        } else {
            "too few runs to compare"
        });
    }
    let (diff, words) = if is_rate {
        let d = ((now - before) * 100.).round() as i64;
        (
            d,
            format!("{} pt{}", d.abs(), if d.abs() == 1 { "" } else { "s" }),
        )
    } else {
        if before == 0. {
            return quiet("new this window");
        }
        let d = ((now - before) / before * 100.).round() as i64;
        (d, format!("{}%", d.abs()))
    };
    if diff == 0 {
        return quiet("= same as before");
    }
    let up = diff > 0;
    let color = match better {
        Better::None => t.fg_muted,
        b if (b == Better::Up) == up => t.success.fg,
        _ => t.danger.fg,
    };
    div()
        .flex()
        .gap(px(4.))
        .text_size(px(size::TEXT_XS))
        .child(
            div()
                .text_color(color)
                .child(format!("{} {words}", if up { "▲" } else { "▼" })),
        )
        .child(div().text_color(t.fg_subtle).child(vs.to_string()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_has_a_slot_for_every_day() {
        let from = chrono::Utc::now() - Duration::days(6);
        let days = window_days(from, 7);
        assert_eq!(days.len(), 7);
        assert_eq!(days[6] - days[0], Duration::days(6));
    }
}
