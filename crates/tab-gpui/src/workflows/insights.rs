//! Insights: the Factory (every workflow at a glance), one workflow's
//! Insights panel, and the badges on each step of the canvas. The
//! counterparts of `insights/Factory.tsx`, `InsightsPanel.tsx`,
//! `StepBadges.tsx` and `parts.tsx`.

use chrono::{Local, NaiveDate, Offset};
use gpui::{
    AnyElement, Context, Hsla, IntoElement, ParentElement, SharedString, Styled, div, prelude::*,
    px,
};
use gpui_component::Sizable;
use gpui_component::button::Button as GButton;
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use tab_core::workflow::insights::FactoryWorkflow;
use tab_core::workflow::stats::{Counts, DayStats, Health, Stats, StepStats, VersionStats};
use tab_core::workflow::{StepKind, Workflow};

use super::charts::{Better, delta, legend, palette, ring, split_bar, stacked_bars, window_days};
use super::format::*;
use super::{Panel, View, WorkflowsTab};
use crate::components::{Button, badge, icon, segmented};
use crate::theme::{FONT_MONO, Theme, Tone, size, theme};

fn local_day(at: chrono::DateTime<chrono::Utc>) -> NaiveDate {
    at.with_timezone(&Local::now().offset().fix()).date_naive()
}

/// Per day: on its own, with help, failed.
fn outcome(days: &[DayStats]) -> impl Fn(NaiveDate) -> Vec<f64> + '_ {
    move |d| {
        days.iter()
            .find(|x| x.day == d)
            .map_or(vec![0., 0., 0.], |x| {
                vec![
                    x.counts.unattended as f64,
                    (x.counts.succeeded - x.counts.unattended) as f64,
                    x.counts.failed as f64,
                ]
            })
    }
}

fn outcome_series(t: &Theme) -> Vec<(&'static str, Hsla)> {
    let p = palette(t);
    vec![
        ("Succeeded on its own", p.ok),
        ("Succeeded with help", p.attended),
        ("Failed", p.failed),
    ]
}

fn tone_color(t: &Theme, tone: Tone) -> Hsla {
    // Rings use the chart tokens where a tone has one.
    let p = palette(t);
    if tone.fg == t.success.fg {
        p.ok
    } else if tone.fg == t.danger.fg {
        p.failed
    } else if tone.fg == t.warning.fg {
        p.unplanned
    } else {
        t.fg_subtle
    }
}

fn label_of(w: &Workflow, id: &str, fallback: &str) -> String {
    if !fallback.is_empty() {
        return fallback.to_string();
    }
    w.steps
        .iter()
        .find(|s| s.id == id && !s.label.is_empty())
        .map_or(id.to_string(), |s| s.label.clone())
}

fn small_label(t: &Theme, s: impl Into<SharedString>) -> AnyElement {
    div()
        .text_size(px(size::TEXT_XS))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(t.fg_subtle)
        .child(s.into())
        .into_any_element()
}

fn heading(t: &Theme, title: &str, sub: &str) -> AnyElement {
    div()
        .flex()
        .items_baseline()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(size::TEXT_MD))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.to_string()),
        )
        .child(
            div()
                .text_size(px(size::TEXT_SM))
                .text_color(t.fg_subtle)
                .child(sub.to_string()),
        )
        .into_any_element()
}

fn stat(
    t: &Theme,
    label: &str,
    value: String,
    hint: Option<String>,
    extra: Option<AnyElement>,
    big: bool,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .min_w_0()
        .child(small_label(t, label.to_uppercase()))
        .child(
            div()
                .text_size(px(if big { size::TEXT_2XL } else { size::TEXT_XL }))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(value),
        )
        .when_some(hint, |el, h| {
            el.child(
                div()
                    .text_size(px(size::TEXT_SM))
                    .text_color(t.fg_subtle)
                    .child(h),
            )
        })
        .children(extra)
        .into_any_element()
}

/// The three rings every view shows: on its own (the headline), succeeded,
/// and people stepping in, split into planned and unplanned.
fn rate_rings(
    t: &Theme,
    c: &Counts,
    size_px: f32,
    headline: f32,
    tone: Hsla,
    deltas: [AnyElement; 3],
) -> AnyElement {
    let p = palette(t);
    let thin = c.runs < MIN_RUNS;
    let sub = thin.then(|| {
        if c.runs > 0 {
            plural(c.runs, "run")
        } else {
            "no runs".into()
        }
    });
    let un = unattended_rate(c);
    let ok = success_rate(c);
    let planned = rate(c.attended - c.unplanned, c.runs).unwrap_or(0.);
    let unplanned = rate(c.unplanned, c.runs).unwrap_or(0.);
    let center = |r: Option<f64>| if thin { "–".to_string() } else { pct(r) };
    let fig = |ring: AnyElement, caption: AnyElement, d: AnyElement| {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.))
            .child(ring)
            .child(caption)
            .child(d)
    };
    let caption = |s: &str| {
        div()
            .text_size(px(size::TEXT_SM))
            .text_color(t.fg_muted)
            .child(s.to_string())
            .into_any_element()
    };
    let [d0, d1, d2] = deltas;
    let split = (!thin && c.attended > 0).then(|| {
        legend(
            t,
            &[
                (&format!("{} planned", pct(Some(planned))), p.planned),
                (&format!("{} unplanned", pct(Some(unplanned))), p.unplanned),
            ],
        )
    });
    div()
        .flex()
        .items_start()
        .gap(px(18.))
        .child(fig(
            ring(
                t,
                headline,
                None,
                vec![(un.unwrap_or(0.), tone)],
                thin,
                center(un),
                sub.clone(),
            ),
            caption("Succeeded on its own"),
            d0,
        ))
        .child(fig(
            ring(
                t,
                size_px,
                None,
                vec![(ok.unwrap_or(0.), p.ok)],
                thin,
                center(ok),
                sub.clone(),
            ),
            caption("Succeeded"),
            d1,
        ))
        .child(fig(
            ring(
                t,
                size_px,
                None,
                vec![(planned, p.planned), (unplanned, p.unplanned)],
                thin,
                if thin {
                    "–".into()
                } else {
                    pct(Some(planned + unplanned))
                },
                sub,
            ),
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(2.))
                .child(caption("People stepped in"))
                .children(split)
                .into_any_element(),
            d2,
        ))
        .into_any_element()
}

fn card(t: &Theme) -> gpui::Div {
    div()
        .p(px(16.))
        .rounded(px(size::RADIUS_LG))
        .border_1()
        .border_color(t.border)
        .bg(t.bg_raised)
}

fn health_badge(t: &Theme, h: Health) -> AnyElement {
    let tone = health_tone(t, h);
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .px(px(7.))
        .py(px(2.))
        .rounded_full()
        .bg(tone.bg)
        .text_color(tone.fg)
        .text_size(px(size::TEXT_SM))
        .child(icon(health_icon(h), 11., tone.fg))
        .child(health_label(h))
        .into_any_element()
}

fn spend_value(s: &tab_core::workflow::stats::Spend) -> (String, Option<&'static str>) {
    if has_api(s) {
        (usd(s.cost_usd), None)
    } else if has_plan(s) {
        (tokens(plan_tokens(s)), Some(" plan tokens"))
    } else {
        ("$0.00".into(), None)
    }
}

/// A step's median time split into waiting for a runner, working, and
/// waiting on a person.
fn time_parts(t: &Theme, st: &StepStats) -> Vec<(f64, Hsla)> {
    let p = palette(t);
    if st.queue_ms.n > 0 {
        let human = st.human_wait_ms.p50;
        vec![
            (st.queue_ms.p50 as f64, t.fg_subtle),
            (st.work_ms.p50.saturating_sub(human) as f64, p.attended),
            (human as f64, p.unplanned),
        ]
    } else {
        vec![(st.ms.p50 as f64, p.attended)]
    }
}

fn total_time(t: &Theme, st: &StepStats) -> f64 {
    time_parts(t, st).iter().map(|p| p.0).sum()
}

/// What a step did lately, on its node: median time, how often it failed,
/// what it spent.
pub fn step_badges(t: &Theme, s: &StepStats, days: u32) -> AnyElement {
    if s.runs == 0 {
        return div().into_any_element();
    }
    let fail = rate(s.failed, s.runs).unwrap_or(0.);
    let thin = s.runs < MIN_RUNS;
    let time = if s.queue_ms.n > 0 {
        s.queue_ms.p50 + s.work_ms.p50
    } else {
        s.ms.p50
    };
    let tone = if thin {
        t.neutral
    } else if fail >= 0.2 {
        t.danger
    } else if fail > 0.05 {
        t.warning
    } else {
        t.success
    };
    let glyph = if thin {
        "dot"
    } else if tone.fg == t.success.fg {
        "check"
    } else {
        "alert"
    };
    let chip = |glyph: Option<&str>, s: String, tone: Option<Tone>| {
        let (fg, bg) = tone.map_or((t.fg_muted, t.bg_sunken), |x| (x.fg, x.bg));
        div()
            .flex()
            .items_center()
            .gap(px(3.))
            .px(px(6.))
            .py(px(1.))
            .rounded(px(size::RADIUS_SM))
            .bg(bg)
            .text_color(fg)
            .text_size(px(size::TEXT_XS))
            .when_some(glyph.map(str::to_string), |el, g| {
                el.child(icon(&g, 10., fg))
            })
            .child(s)
    };
    let spend = spend_short(&s.spend, true);
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .pl(px(38.))
        .child(chip(Some("clock"), duration(time), None))
        .child(chip(
            Some(glyph),
            if thin {
                plural(s.runs, "run")
            } else if s.failed > 0 {
                format!("{} fail", pct(Some(fail)))
            } else {
                "0% fail".into()
            },
            Some(tone),
        ))
        .when(spend != "Nothing", |el| el.child(chip(None, spend, None)))
        .child(
            div()
                .text_size(px(size::TEXT_XS))
                .text_color(t.fg_subtle)
                .child(format!("last {days} days")),
        )
        .into_any_element()
}

impl WorkflowsTab {
    fn window_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity().downgrade();
        segmented(
            "wf-window",
            &[(7u32, "7 days"), (30u32, "30 days")],
            self.days,
            cx,
            move |d, _, cx| {
                this.update(cx, |v, cx| v.set_days(d, cx)).ok();
            },
        )
    }

    pub(super) fn factory_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        let picker = self.window_picker(cx);
        let mut body = div().flex().flex_col().gap(px(16.)).p(px(16.));
        match self.factory.clone() {
            None => body = body.child(note(&t, "Adding up runs…")),
            Some(f) => {
                let tt = &f.totals;
                let pp = &f.previous_totals;
                let enough = pp.counts.runs >= MIN_RUNS && tt.counts.runs >= MIN_RUNS;
                let tone = tone_color(
                    &t,
                    rate_tone(&t, unattended_rate(&tt.counts), tt.counts.runs),
                );
                let rings = rate_rings(
                    &t,
                    &tt.counts,
                    84.,
                    112.,
                    tone,
                    [
                        delta(
                            &t,
                            unattended_rate(&tt.counts),
                            unattended_rate(&pp.counts),
                            true,
                            Better::Up,
                            enough,
                            "vs before",
                        ),
                        delta(
                            &t,
                            success_rate(&tt.counts),
                            success_rate(&pp.counts),
                            true,
                            Better::Up,
                            enough,
                            "vs before",
                        ),
                        delta(
                            &t,
                            intervention_rate(&tt.counts),
                            intervention_rate(&pp.counts),
                            true,
                            Better::Down,
                            enough,
                            "vs before",
                        ),
                    ],
                );
                let days = window_days(f.from, f.days);
                let series = outcome_series(&t);
                let colors: Vec<Hsla> = series.iter().map(|s| s.1).collect();
                let chart = div()
                    .flex_1()
                    .min_w(px(200.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(small_label(&t, "EVERY DAY"))
                    .child(stacked_bars(
                        &t,
                        &days,
                        &outcome(&tt.days),
                        &colors,
                        150.,
                        &[],
                        true,
                    ))
                    .child(legend(&t, &series));
                let hint_runs = [
                    (tt.in_progress > 0).then(|| format!("{} running", tt.in_progress)),
                    (tt.held_back > 0).then(|| format!("{} held back", tt.held_back)),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                let (spend, unit) = spend_value(&tt.spend);
                let spend_hint = if has_api(&tt.spend) && has_plan(&tt.spend) {
                    format!(
                        "+ {} plan tokens ({})",
                        tokens(plan_tokens(&tt.spend)),
                        plan_share(tt.spend.plan_share)
                    )
                } else if has_plan(&tt.spend) {
                    format!("{} of plan allowance", plan_share(tt.spend.plan_share))
                } else {
                    "API keys".into()
                };
                let spend_delta = if has_api(&tt.spend) {
                    delta(
                        &t,
                        Some(tt.spend.cost_usd),
                        Some(pp.spend.cost_usd),
                        false,
                        Better::Down,
                        pp.counts.runs > 0,
                        "vs before",
                    )
                } else {
                    delta(
                        &t,
                        Some(plan_tokens(&tt.spend) as f64),
                        Some(plan_tokens(&pp.spend) as f64),
                        false,
                        Better::Down,
                        pp.counts.runs > 0,
                        "vs before",
                    )
                };
                let stats = div()
                    .w(px(190.))
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .child(stat(
                        &t,
                        "Runs",
                        tt.counts.runs.to_string(),
                        (!hint_runs.is_empty()).then_some(hint_runs),
                        Some(delta(
                            &t,
                            Some(tt.counts.runs as f64),
                            Some(pp.counts.runs as f64),
                            false,
                            Better::None,
                            pp.counts.runs > 0,
                            "vs before",
                        )),
                        false,
                    ))
                    .child(stat(
                        &t,
                        "Median time, start to finish",
                        if tt.duration_ms.n > 0 {
                            duration(tt.duration_ms.p50)
                        } else {
                            "–".into()
                        },
                        (tt.duration_ms.n > 0)
                            .then(|| format!("Slow end {}", duration(tt.duration_ms.p95))),
                        Some(delta(
                            &t,
                            (tt.duration_ms.n > 0).then_some(tt.duration_ms.p50 as f64),
                            (pp.duration_ms.n > 0).then_some(pp.duration_ms.p50 as f64),
                            false,
                            Better::Down,
                            enough,
                            "vs before",
                        )),
                        false,
                    ))
                    .child(stat(
                        &t,
                        "Spend",
                        format!("{spend}{}", unit.unwrap_or("")),
                        Some(spend_hint),
                        Some(spend_delta),
                        false,
                    ));
                body = body.child(
                    card(&t)
                        .flex()
                        .flex_wrap()
                        .items_start()
                        .gap(px(20.))
                        .child(rings)
                        .child(chart)
                        .child(stats),
                );
                let need = f
                    .workflows
                    .iter()
                    .filter(|w| matches!(w.health, Health::Failing | Health::Degraded))
                    .count() as u32;
                body = body.child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(size::TEXT_LG))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Workflows"),
                        )
                        .child(
                            div()
                                .text_size(px(size::TEXT_SM))
                                .text_color(if need > 0 { t.warning.fg } else { t.fg_subtle })
                                .child(if need > 0 {
                                    format!(
                                        "{need} need{} a look, worst first",
                                        if need == 1 { "s" } else { "" }
                                    )
                                } else if !f.workflows.is_empty() {
                                    "Everything with enough runs is healthy".into()
                                } else {
                                    String::new()
                                }),
                        ),
                );
                if f.workflows.is_empty() {
                    body = body.child(note(
                        &t,
                        "No workflows yet. Build one and its runs show up here.",
                    ));
                }
                let mut grid = div().flex().flex_wrap().gap(px(12.));
                for w in &f.workflows {
                    let def = self.workflows.iter().find(|x| x.id == w.id).cloned();
                    grid = grid.child(self.factory_card(&t, w, def, f.from, f.days, cx));
                }
                body = body.child(grid);
            }
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(16.))
                    .py(px(10.))
                    .border_b_1()
                    .border_color(t.border)
                    .bg(t.bg_raised)
                    .child(
                        div()
                            .text_size(px(size::TEXT_LG))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Factory"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(size::TEXT_SM))
                            .text_color(t.fg_subtle)
                            .child(format!(
                                "How every workflow ran, in the last {} days",
                                self.days
                            )),
                    )
                    .child(picker),
            )
            .child(
                div()
                    .id("wf-factory-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
            .into_any_element()
    }

    fn factory_card(
        &self,
        t: &Theme,
        w: &FactoryWorkflow,
        def: Option<Workflow>,
        from: chrono::DateTime<chrono::Utc>,
        days: u32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let s = &w.stats;
        let thin = s.counts.runs < MIN_RUNS;
        let un = unattended_rate(&s.counts);
        let ring_color = match w.health {
            Health::Unknown => tone_color(t, rate_tone(t, un, s.counts.runs)),
            Health::Healthy => palette(t).ok,
            Health::Degraded => palette(t).unplanned,
            Health::Failing => palette(t).failed,
        };
        let series = outcome_series(t);
        let colors: Vec<Hsla> = series.iter().map(|s| s.1).collect();
        let slots = window_days(from, days);
        let chart = if s.counts.runs > 0 {
            stacked_bars(t, &slots, &outcome(&s.days), &colors, 58., &[], false)
        } else {
            div()
                .h(px(58.))
                .flex()
                .items_center()
                .text_size(px(size::TEXT_SM))
                .text_color(t.fg_subtle)
                .child(format!("No runs in the last {days} days"))
                .into_any_element()
        };
        let caption = if s.counts.runs > 0 {
            format!(
                "{} · {} succeeded",
                plural(s.counts.runs, "run"),
                pct(success_rate(&s.counts))
            )
        } else if w.enabled {
            "Waiting for its trigger".into()
        } else {
            "Turned off".into()
        };
        let dl = |label: &str, value: String| {
            div()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(small_label(t, label.to_uppercase()))
                .child(div().text_size(px(size::TEXT_MD)).child(value))
        };
        let top = s.failures.first().cloned();
        let unplanned = rate(s.counts.unplanned, s.counts.runs);
        let problem = match (&w.health, &top) {
            (Health::Failing | Health::Degraded, Some(f)) => {
                let tone = if w.health == Health::Failing {
                    t.danger
                } else {
                    t.warning
                };
                let name = def
                    .as_ref()
                    .map_or(f.step_id.clone(), |d| label_of(d, &f.step_id, &f.label));
                Some((
                    tone,
                    if w.health == Health::Failing {
                        "octagon"
                    } else {
                        "alert"
                    },
                    format!("{name} failed {}: {}", plural(f.count, "time"), f.error),
                ))
            }
            (Health::Healthy | Health::Unknown, _)
                if !thin && unplanned.is_some_and(|u| u >= 0.25) =>
            {
                Some((
                    t.warning,
                    "user",
                    format!("A person had to step in on {} of runs", pct(unplanned)),
                ))
            }
            _ => None,
        };
        let id = w.id.clone();
        div()
            .id(SharedString::from(format!("wf-card-{}", w.id)))
            .w(px(340.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(14.))
            .rounded(px(size::RADIUS_LG))
            .border_1()
            .border_color(t.border)
            .bg(t.bg_raised)
            .cursor_pointer()
            .hover(|s| s.border_color(t.border_strong).shadow_sm())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(size::TEXT_MD))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(w.name.clone()),
                    )
                    .when(!w.enabled, |el| {
                        el.child(badge("Off", t.neutral.fg, t.neutral.bg))
                    })
                    .child(health_badge(t, w.health)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(ring(
                        t,
                        72.,
                        None,
                        vec![(un.unwrap_or(0.), ring_color)],
                        thin,
                        if thin { "–".into() } else { pct(un) },
                        Some(if thin {
                            if s.counts.runs > 0 {
                                plural(s.counts.runs, "run")
                            } else {
                                "no runs".into()
                            }
                        } else {
                            "on its own".into()
                        }),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(chart)
                            .child(
                                div()
                                    .text_size(px(size::TEXT_SM))
                                    .text_color(t.fg_subtle)
                                    .child(caption),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(dl(
                        "Stepped in",
                        if s.counts.runs > 0 {
                            pct(intervention_rate(&s.counts))
                        } else {
                            "–".into()
                        },
                    ))
                    .child(dl(
                        "Median",
                        if s.duration_ms.n > 0 {
                            duration(s.duration_ms.p50)
                        } else {
                            "–".into()
                        },
                    ))
                    .child(dl(
                        "Spend",
                        if spend_short(&s.spend, true) == "Nothing" {
                            "–".into()
                        } else {
                            spend_short(&s.spend, true)
                        },
                    )),
            )
            .when_some(problem, |el, (tone, glyph, words)| {
                el.child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(6.))
                        .px(px(8.))
                        .py(px(6.))
                        .rounded(px(size::RADIUS_MD))
                        .bg(tone.bg)
                        .text_color(tone.fg)
                        .text_size(px(size::TEXT_SM))
                        .child(div().pt(px(1.)).child(icon(glyph, 12., tone.fg)))
                        .child(div().flex_1().min_w_0().child(words)),
                )
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Some(wf) = this.workflows.iter().find(|x| x.id == id).cloned() {
                    this.open(wf, Panel::Insights, window, cx);
                }
            }))
            .into_any_element()
    }

    pub(super) fn insights_panel(&mut self, w: &Workflow, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        if w.id.is_empty() {
            return div()
                .p(px(16.))
                .child(note(
                    &t,
                    "Save the workflow to start keeping its runs. Insights add them up once it has run.",
                ))
                .into_any_element();
        }
        if self.insights.as_ref().is_none_or(|i| i.workflow_id != w.id) {
            self.load_insights(cx);
        }
        let picker = self.window_picker(cx);
        let data = self.insights.clone();
        let bar = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .children(data.as_ref().map(|d| health_badge(&t, d.health)))
            .child(
                div()
                    .flex_1()
                    .text_size(px(size::TEXT_SM))
                    .text_color(t.fg_subtle)
                    .child(format!("Last {} days", self.days)),
            )
            .child(picker);
        let mut out = div().flex().flex_col().gap(px(18.)).p(px(16.)).child(bar);
        let Some(data) = data else {
            return out.child(note(&t, "Adding up runs…")).into_any_element();
        };
        let s = &data.stats;
        let p = &data.previous;
        if s.counts.runs == 0 {
            let mut words = String::new();
            if s.in_progress > 0 {
                words.push_str(&format!("{} still going. ", plural(s.in_progress, "run")));
            }
            if p.counts.runs > 0 {
                words.push_str(&format!(
                    "It ran {} in the {} days before. ",
                    plural(p.counts.runs, "time"),
                    data.days
                ));
            }
            words.push_str(if w.enabled {
                "Rates, the assembly line and costs show here once it has run."
            } else {
                "It's turned off. Turn it on and its runs add up here."
            });
            return out
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(8.))
                        .py(px(24.))
                        .child(icon("gauge", 26., t.fg_subtle))
                        .child(
                            div()
                                .text_size(px(size::TEXT_MD))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(format!("No finished runs in the last {} days", data.days)),
                        )
                        .child(note(&t, words)),
                )
                .into_any_element();
        }
        let thin = s.counts.runs < MIN_RUNS;
        let enough = p.counts.runs >= MIN_RUNS && !thin;
        if thin {
            out = out.child(note(
                &t,
                format!(
                    "Only {} so far. Rates show once there are {MIN_RUNS}; the numbers below are what there is.",
                    plural(s.counts.runs, "finished run")
                ),
            ));
        }
        let tone = tone_color(&t, rate_tone(&t, unattended_rate(&s.counts), s.counts.runs));
        out = out.child(rate_rings(
            &t,
            &s.counts,
            70.,
            88.,
            tone,
            [
                delta(
                    &t,
                    unattended_rate(&s.counts),
                    unattended_rate(&p.counts),
                    true,
                    Better::Up,
                    enough,
                    "vs before",
                ),
                delta(
                    &t,
                    success_rate(&s.counts),
                    success_rate(&p.counts),
                    true,
                    Better::Up,
                    enough,
                    "vs before",
                ),
                delta(
                    &t,
                    intervention_rate(&s.counts),
                    intervention_rate(&p.counts),
                    true,
                    Better::Down,
                    enough,
                    "vs before",
                ),
            ],
        ));
        let hint = [
            (s.in_progress > 0).then(|| format!("{} running", s.in_progress)),
            (s.held_back > 0).then(|| format!("{} held back", s.held_back)),
            (s.loop_stopped > 0).then(|| format!("{} loop-stopped", s.loop_stopped)),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let spend = spend_short(&s.spend, true);
        out = out.child(
            div()
                .flex()
                .gap(px(16.))
                .child(div().flex_1().child(stat(
                    &t,
                    "Runs",
                    s.counts.runs.to_string(),
                    (!hint.is_empty()).then_some(hint),
                    None,
                    false,
                )))
                .child(
                    div().flex_1().child(stat(
                        &t,
                        "Median time",
                        if s.duration_ms.n > 0 {
                            duration(s.duration_ms.p50)
                        } else {
                            "–".into()
                        },
                        (s.duration_ms.n > 0)
                            .then(|| format!("slow end {}", duration(s.duration_ms.p95))),
                        Some(delta(
                            &t,
                            (s.duration_ms.n > 0).then_some(s.duration_ms.p50 as f64),
                            (p.duration_ms.n > 0).then_some(p.duration_ms.p50 as f64),
                            false,
                            Better::Down,
                            p.duration_ms.n >= MIN_RUNS,
                            "vs before",
                        )),
                        false,
                    )),
                )
                .child(div().flex_1().child(stat(
                    &t,
                    "Spend",
                    if spend == "Nothing" {
                        "–".into()
                    } else {
                        spend
                    },
                    Some(if has_plan(&s.spend) {
                        format!("{} of plan allowance", plan_share(s.spend.plan_share))
                    } else if has_api(&s.spend) {
                        format!("{} a run", usd(s.spend.cost_usd / s.counts.runs as f64))
                    } else {
                        "No agent steps ran".into()
                    }),
                    None,
                    false,
                ))),
        );
        // Every day, with the days a new version first ran marked.
        let days = window_days(data.from, data.days);
        let start = local_day(data.from);
        let markers: Vec<(NaiveDate, String)> = s
            .versions
            .iter()
            .filter(|v| v.first_run.is_some() && s.versions.iter().any(|o| o.version < v.version))
            .map(|v| {
                (
                    local_day(v.first_run.unwrap_or(data.from)),
                    format!("v{}", v.version),
                )
            })
            .filter(|m| m.0 >= start)
            .collect();
        let series = outcome_series(&t);
        let colors: Vec<Hsla> = series.iter().map(|x| x.1).collect();
        out = out.child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(heading(&t, "Every day", ""))
                .child(stacked_bars(
                    &t,
                    &days,
                    &outcome(&s.days),
                    &colors,
                    140.,
                    &markers,
                    true,
                ))
                .child(legend(&t, &series)),
        );
        if has_api(&s.spend) || has_plan(&s.spend) {
            out = out.child(self.spend_trend(&t, s, &days));
        }
        out = out.child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(heading(
                    &t,
                    "Assembly line",
                    "median time per step, and where it goes",
                ))
                .child(assembly_line(&t, w, s)),
        );
        if !s.models.is_empty() {
            out = out.child(models(&t, w, s));
        }
        if s.versions.len() > 1 {
            out = out.child(self.versions_table(&t, &s.versions, cx));
        }
        out = out.child(self.failures(&t, w, s, cx));
        out.into_any_element()
    }

    fn spend_trend(&self, t: &Theme, s: &Stats, days: &[NaiveDate]) -> AnyElement {
        let p = palette(t);
        let mut row = div().flex().flex_col().gap(px(10.));
        if has_api(&s.spend) {
            let per_day = |d: NaiveDate| {
                vec![
                    s.days
                        .iter()
                        .find(|x| x.day == d)
                        .map_or(0., |x| x.spend.cost_usd),
                ]
            };
            row = row.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(legend(
                        t,
                        &[(
                            &format!(
                                "Billed through API keys: {} · {} a run",
                                usd(s.spend.cost_usd),
                                usd(s.spend.cost_usd / s.counts.runs.max(1) as f64)
                            ),
                            p.attended,
                        )],
                    ))
                    .child(stacked_bars(
                        t,
                        days,
                        &per_day,
                        &[p.attended],
                        70.,
                        &[],
                        false,
                    )),
            );
        }
        if has_plan(&s.spend) {
            let per_day = |d: NaiveDate| {
                vec![
                    s.days
                        .iter()
                        .find(|x| x.day == d)
                        .map_or(0., |x| plan_tokens(&x.spend) as f64),
                ]
            };
            row = row.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(legend(
                        t,
                        &[(
                            &format!(
                                "On a subscription: {} tokens · {}",
                                tokens(plan_tokens(&s.spend)),
                                plan_share(s.spend.plan_share)
                            ),
                            p.planned,
                        )],
                    ))
                    .child(stacked_bars(
                        t,
                        days,
                        &per_day,
                        &[p.planned],
                        70.,
                        &[],
                        false,
                    )),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(heading(
                t,
                "Spend",
                "API keys cost dollars; subscriptions use plan allowance, never priced",
            ))
            .child(row)
            .into_any_element()
    }

    fn versions_table(
        &self,
        t: &Theme,
        versions: &[VersionStats],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let a_n = self.versions.0.unwrap_or(versions[1].version);
        let b_n = self.versions.1.unwrap_or(versions[0].version);
        let va = versions
            .iter()
            .find(|v| v.version == a_n)
            .unwrap_or(&versions[1])
            .clone();
        let vb = versions
            .iter()
            .find(|v| v.version == b_n)
            .unwrap_or(&versions[0])
            .clone();
        let enough = va.counts.runs >= MIN_RUNS && vb.counts.runs >= MIN_RUNS;
        let vs = format!("vs v{}", va.version);
        let rate_cell = |v: &VersionStats, f: fn(&Counts) -> Option<f64>| {
            if v.counts.runs < MIN_RUNS {
                "–".to_string()
            } else {
                pct(f(&v.counts))
            }
        };
        let per_run = |v: &VersionStats, x: f64| {
            if v.counts.runs > 0 {
                x / v.counts.runs as f64
            } else {
                0.
            }
        };
        let dur = |v: &VersionStats, p95: bool| {
            if v.duration_ms.n == 0 {
                "–".to_string()
            } else {
                duration(if p95 {
                    v.duration_ms.p95
                } else {
                    v.duration_ms.p50
                })
            }
        };
        let mut rows: Vec<(String, String, String, Option<AnyElement>)> = vec![
            (
                "First run in window".into(),
                va.first_run
                    .map_or("–".into(), |d| local_day(d).format("%b %-d").to_string()),
                vb.first_run
                    .map_or("–".into(), |d| local_day(d).format("%b %-d").to_string()),
                None,
            ),
            (
                "Runs".into(),
                va.counts.runs.to_string(),
                vb.counts.runs.to_string(),
                None,
            ),
        ];
        for (label, f, better) in [
            (
                "Succeeded on its own",
                unattended_rate as fn(&Counts) -> Option<f64>,
                Better::Up,
            ),
            ("Succeeded", success_rate, Better::Up),
            ("People stepped in", intervention_rate, Better::Down),
        ] {
            rows.push((
                label.into(),
                rate_cell(&va, f),
                rate_cell(&vb, f),
                Some(delta(
                    t,
                    f(&vb.counts),
                    f(&va.counts),
                    true,
                    better,
                    enough,
                    &vs,
                )),
            ));
        }
        rows.push((
            "Median time".into(),
            dur(&va, false),
            dur(&vb, false),
            Some(delta(
                t,
                Some(vb.duration_ms.p50 as f64),
                Some(va.duration_ms.p50 as f64),
                false,
                Better::Down,
                enough,
                &vs,
            )),
        ));
        rows.push((
            "Slow end (p95)".into(),
            dur(&va, true),
            dur(&vb, true),
            Some(delta(
                t,
                Some(vb.duration_ms.p95 as f64),
                Some(va.duration_ms.p95 as f64),
                false,
                Better::Down,
                enough,
                &vs,
            )),
        ));
        if has_api(&va.spend) || has_api(&vb.spend) {
            let (a, b) = (
                per_run(&va, va.spend.cost_usd),
                per_run(&vb, vb.spend.cost_usd),
            );
            rows.push((
                "API spend a run".into(),
                usd(a),
                usd(b),
                Some(delta(t, Some(b), Some(a), false, Better::Down, enough, &vs)),
            ));
        }
        if has_plan(&va.spend) || has_plan(&vb.spend) {
            let (a, b) = (
                per_run(&va, plan_tokens(&va.spend) as f64),
                per_run(&vb, plan_tokens(&vb.spend) as f64),
            );
            rows.push((
                "Plan tokens a run".into(),
                tokens(a.round() as u64),
                tokens(b.round() as u64),
                Some(delta(t, Some(b), Some(a), false, Better::Down, enough, &vs)),
            ));
        }
        let pick = |id: &'static str, current: u32, first: bool, cx: &mut Context<Self>| {
            let this = cx.entity().downgrade();
            let opts: Vec<(u32, String)> = versions
                .iter()
                .map(|v| {
                    (
                        v.version,
                        format!("v{} · {}", v.version, plural(v.counts.runs, "run")),
                    )
                })
                .collect();
            let label = opts
                .iter()
                .find(|o| o.0 == current)
                .map_or(format!("v{current}"), |o| o.1.clone());
            GButton::new(id)
                .outline()
                .xsmall()
                .label(label)
                .dropdown_menu(move |mut menu, _, _| {
                    for (n, l) in &opts {
                        let (this, n) = (this.clone(), *n);
                        menu =
                            menu.item(PopupMenuItem::new(l.clone()).on_click(move |_, _, cx| {
                                this.update(cx, |v, cx| {
                                    if first {
                                        v.versions.0 = Some(n);
                                    } else {
                                        v.versions.1 = Some(n);
                                    }
                                    cx.notify();
                                })
                                .ok();
                            }));
                    }
                    menu
                })
        };
        let row = |cells: [AnyElement; 4]| {
            let [a, b, c, d] = cells;
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .py(px(5.))
                .border_b_1()
                .border_color(t.border)
                .text_size(px(size::TEXT_SM))
                .child(div().flex_1().min_w_0().child(a))
                .child(div().w(px(90.)).flex().justify_end().child(b))
                .child(div().w(px(90.)).flex().justify_end().child(c))
                .child(div().w(px(100.)).flex().justify_end().child(d))
        };
        let txt = |s: String| div().child(s).into_any_element();
        let mut table = div().flex().flex_col().child(row([
            txt(String::new()),
            pick("wf-ver-a", va.version, true, cx).into_any_element(),
            pick("wf-ver-b", vb.version, false, cx).into_any_element(),
            small_label(t, "CHANGE"),
        ]));
        for (label, a, b, d) in rows {
            table = table.child(row([
                txt(label),
                txt(a),
                txt(b),
                d.unwrap_or_else(|| txt(String::new())),
            ]));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(heading(t, "Versions", "side by side"))
            .child(table)
            .into_any_element()
    }

    fn failures(&self, t: &Theme, w: &Workflow, s: &Stats, cx: &mut Context<Self>) -> AnyElement {
        let now = chrono::Utc::now();
        let ago = |at| {
            let a = crate::components::ago(at, now);
            if a == "now" { a } else { format!("{a} ago") }
        };
        let mut list = div().flex().flex_col().gap(px(8.));
        if s.failures.is_empty() {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(size::TEXT_SM))
                    .text_color(t.success.fg)
                    .child(icon("check", 13., t.success.fg))
                    .child("Nothing failed in this window."),
            );
        }
        for (n, f) in s.failures.iter().enumerate() {
            let run = f.sample_run.clone();
            let when = if f.first_seen != f.last_seen {
                format!("Last {} · first {}", ago(f.last_seen), ago(f.first_seen))
            } else {
                format!("Last {}", ago(f.last_seen))
            };
            list = list.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .p(px(10.))
                    .rounded(px(size::RADIUS_MD))
                    .border_1()
                    .border_color(t.border)
                    .child(
                        div()
                            .text_size(px(size::TEXT_LG))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(t.danger.fg)
                            .child(format!("×{}", f.count)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .text_size(px(size::TEXT_MD))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(icon("octagon", 12., t.danger.fg))
                                    .child(label_of(w, &f.step_id, &f.label)),
                            )
                            .child(
                                div()
                                    .font_family(FONT_MONO)
                                    .text_size(px(size::TEXT_XS))
                                    .text_color(t.fg_muted)
                                    .child(f.error.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(size::TEXT_XS))
                                    .text_color(t.fg_subtle)
                                    .child(when),
                            ),
                    )
                    .child(
                        Button::new(("wf-failure-run", n))
                            .icon("external")
                            .label("Run")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.panel = Panel::Runs;
                                this.view = View::Builder;
                                this.open_run = Some(run.clone());
                                cx.notify();
                            })),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(heading(
                t,
                "What fails",
                if s.failures.is_empty() {
                    ""
                } else {
                    "most often first"
                },
            ))
            .child(list)
            .into_any_element()
    }
}

/// The steps in the workflow's order (ones it no longer has last), each
/// with how often it got done, its median time split by where it went,
/// and what it spent. The slowest and costliest are marked.
fn assembly_line(t: &Theme, w: &Workflow, s: &Stats) -> AnyElement {
    let mut ids: Vec<String> = w.steps.iter().map(|x| x.id.clone()).collect();
    for st in &s.steps {
        if !ids.contains(&st.id) {
            ids.push(st.id.clone());
        }
    }
    let ran: Vec<&StepStats> = s.steps.iter().filter(|x| x.runs >= MIN_RUNS).collect();
    let max_time = ran.iter().map(|x| total_time(t, x)).fold(1., f64::max);
    let slowest = (ran.len() > 1)
        .then(|| {
            ran.iter()
                .max_by(|a, b| total_time(t, a).total_cmp(&total_time(t, b)))
                .map(|x| x.id.clone())
        })
        .flatten();
    let by_dollars = s.steps.iter().any(|x| x.spend.cost_usd > 0.);
    let cost = |x: &StepStats| {
        if by_dollars {
            x.spend.cost_usd
        } else {
            plan_tokens(&x.spend) as f64
        }
    };
    let costly: Vec<&&StepStats> = ran.iter().filter(|x| cost(x) > 0.).collect();
    let costliest = (costly.len() > 1)
        .then(|| {
            costly
                .iter()
                .max_by(|a, b| cost(a).total_cmp(&cost(b)))
                .map(|x| x.id.clone())
        })
        .flatten();
    let p = palette(t);
    let mut line = div().flex().flex_col().gap(px(6.));
    for (i, id) in ids.iter().enumerate() {
        let st = s.steps.iter().find(|x| &x.id == id);
        let gone = !w.steps.iter().any(|x| &x.id == id);
        let name = label_of(w, id, st.map_or("", |x| x.label.as_str()));
        let is_agent = w
            .steps
            .iter()
            .any(|x| &x.id == id && matches!(x.kind, StepKind::Agent { .. }));
        let mut station = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(10.))
            .rounded(px(size::RADIUS_MD))
            .border_1()
            .border_color(if Some(id) == slowest.as_ref() {
                t.warning.fg
            } else if Some(id) == costliest.as_ref() {
                t.accent.fg
            } else {
                t.border
            })
            .when(gone, |el| el.opacity(0.6))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .size(px(18.))
                            .rounded_full()
                            .bg(t.bg_sunken)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(size::TEXT_XS))
                            .text_color(t.fg_muted)
                            .child((i + 1).to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(size::TEXT_MD))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(name.clone()),
                    )
                    .when(is_agent, |el| el.child(icon("bot", 12., t.fg_subtle)))
                    .when(Some(id) == slowest.as_ref(), |el| {
                        el.child(badge("Slowest", t.warning.fg, t.warning.bg))
                    })
                    .when(Some(id) == costliest.as_ref(), |el| {
                        el.child(badge("Costliest", t.accent.fg, t.accent.bg))
                    }),
            );
        match st.filter(|x| x.runs > 0) {
            Some(st) => {
                let ok = rate(st.ok, st.runs);
                let fail = rate(st.failed, st.runs);
                let fail_color = match fail {
                    Some(f) if f >= 0.2 => t.danger.fg,
                    Some(f) if f > 0.05 => t.warning.fg,
                    _ => t.fg_subtle,
                };
                let spend = spend_short(&st.spend, false);
                station = station
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(ring(
                                t,
                                44.,
                                Some(5.),
                                vec![(ok.unwrap_or(0.), tone_color(t, rate_tone(t, ok, st.runs)))],
                                st.runs < MIN_RUNS,
                                if st.runs < MIN_RUNS {
                                    "–".into()
                                } else {
                                    pct(ok)
                                },
                                None,
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_size(px(size::TEXT_LG))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(duration(total_time(t, st) as u64)),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(size::TEXT_SM))
                                            .text_color(fail_color)
                                            .child(if st.failed == 0 {
                                                "No failures".to_string()
                                            } else if st.runs < MIN_RUNS {
                                                format!("{} of {} failed", st.failed, st.runs)
                                            } else {
                                                format!("{} failed", pct(fail))
                                            }),
                                    ),
                            ),
                    )
                    .child(split_bar(t, &time_parts(t, st), max_time))
                    .child(
                        div()
                            .flex()
                            .gap(px(10.))
                            .text_size(px(size::TEXT_SM))
                            .text_color(t.fg_muted)
                            .child(if spend == "Nothing" {
                                "No spend".to_string()
                            } else {
                                spend
                            })
                            .when(st.interventions > 0, |el| {
                                el.child(
                                    div()
                                        .text_color(t.warning.fg)
                                        .child(format!("{} stepped in", st.interventions)),
                                )
                            })
                            .when(st.fallbacks > 0, |el| {
                                el.child(
                                    div()
                                        .text_color(t.fg_subtle)
                                        .child(plural(st.fallbacks, "fallback")),
                                )
                            }),
                    );
            }
            None => {
                station = station.child(
                    div()
                        .text_size(px(size::TEXT_SM))
                        .text_color(t.fg_subtle)
                        .child(match st {
                            Some(x) if x.skipped > 0 => {
                                format!("Skipped {}", plural(x.skipped, "time"))
                            }
                            _ => "Not reached".into(),
                        }),
                );
            }
        }
        if i > 0 {
            line = line.child(div().ml(px(18.)).w(px(2.)).h(px(8.)).bg(t.border_strong));
        }
        line = line.child(station);
    }
    line.child(legend(
        t,
        &[
            ("Waiting for a runner", t.fg_subtle),
            ("Working", p.attended),
            ("Waiting on a person", p.unplanned),
        ],
    ))
    .into_any_element()
}

/// What each model choice got, per agent step.
fn models(t: &Theme, w: &Workflow, s: &Stats) -> AnyElement {
    let mut steps: Vec<String> = Vec::new();
    for m in &s.models {
        if !steps.contains(&m.step_id) {
            steps.push(m.step_id.clone());
        }
    }
    let mut out = div().flex().flex_col().gap(px(10.)).child(heading(
        t,
        "Models",
        "what each choice got, per agent step",
    ));
    let row = |cells: Vec<AnyElement>, head: bool| {
        let mut r = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .py(px(4.))
            .border_b_1()
            .border_color(t.border)
            .text_size(px(if head { size::TEXT_XS } else { size::TEXT_SM }))
            .when(head, |el| el.text_color(t.fg_subtle));
        for (i, c) in cells.into_iter().enumerate() {
            r = r.child(if i == 0 {
                div().flex_1().min_w_0().child(c)
            } else {
                div().w(px(58.)).flex().justify_end().child(c)
            });
        }
        r
    };
    let txt = |s: String| div().child(s).into_any_element();
    for step in steps {
        let mut ms: Vec<_> = s
            .models
            .iter()
            .filter(|m| m.step_id == step)
            .cloned()
            .collect();
        ms.sort_by_key(|m| std::cmp::Reverse(m.runs));
        let rated: Vec<_> = ms.iter().filter(|m| m.runs >= MIN_RUNS).collect();
        let best = (rated.len() > 1)
            .then(|| {
                rated
                    .iter()
                    .max_by(|a, b| {
                        rate(a.unattended, a.runs)
                            .unwrap_or(0.)
                            .total_cmp(&rate(b.unattended, b.runs).unwrap_or(0.))
                    })
                    .map(|m| (m.harness.clone(), m.model.clone()))
            })
            .flatten();
        let mut table = div().flex().flex_col().child(row(
            ["MODEL", "RUNS", "OK", "ALONE", "WORK", "SPEND"]
                .into_iter()
                .map(|s| txt(s.into()))
                .collect(),
            true,
        ));
        for m in &ms {
            let is_best = best.as_ref() == Some(&(m.harness.clone(), m.model.clone()));
            let sub = [m.harness.replace('_', " "), m.family.clone()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            let spend = spend_short(&m.spend, true);
            table = table.child(row(
                vec![
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .child(if m.model.is_empty() {
                                    "Default".to_string()
                                } else {
                                    m.model.clone()
                                })
                                .when(is_best, |el| {
                                    el.child(badge("Best", t.success.fg, t.success.bg))
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(size::TEXT_XS))
                                .text_color(t.fg_subtle)
                                .child(sub),
                        )
                        .into_any_element(),
                    txt(m.runs.to_string()),
                    txt(if m.runs < MIN_RUNS {
                        "–".into()
                    } else {
                        pct(rate(m.ok, m.runs))
                    }),
                    txt(if m.runs < MIN_RUNS {
                        "–".into()
                    } else {
                        pct(rate(m.unattended, m.runs))
                    }),
                    txt(if m.work_ms.n > 0 {
                        duration(m.work_ms.p50)
                    } else {
                        "–".into()
                    }),
                    txt(if spend == "Nothing" {
                        "–".into()
                    } else {
                        spend
                    }),
                ],
                false,
            ));
        }
        out = out.child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(small_label(t, label_of(w, &step, "").to_uppercase()))
                .child(table),
        );
    }
    out.into_any_element()
}

fn note(t: &Theme, s: impl Into<SharedString>) -> AnyElement {
    div()
        .text_size(px(size::TEXT_SM))
        .text_color(t.fg_subtle)
        .child(s.into())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_without_runs_are_zero() {
        let f = outcome(&[]);
        assert_eq!(
            f(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()),
            vec![0., 0., 0.]
        );
    }
}
