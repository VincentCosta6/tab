//! The decision for every agent action in a live tab: allow, ask, confirm,
//! refuse, hand over to the person, or wait for them to watch. Plain code:
//! nothing a page says and nothing a model answers is an input, except the
//! element's labels used for *matching* against the plan the person approved
//! (a page can make a match fail, never make a non-match pass, because
//! values must still equal the plan's). See `docs/agent-browser.md`.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use url::Url;

use super::AgentBrowsing;
use super::plan::{Do, Match, Plan, label_match, norm};
use super::taint::Taint;

/// An element in a live page, as tab's isolated-world script reports it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Element {
    /// `e12`: the handle the agent uses.
    #[serde(rename = "ref")]
    pub id: String,
    pub tag: String,
    #[serde(default)]
    pub role: String,
    /// `type` of an input or button.
    #[serde(default)]
    pub kind: String,
    /// Accessible name, label, placeholder, text, title: what a step's label
    /// is matched against.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Password, one-time code, card number or CVC: never typed by an agent.
    #[serde(default)]
    pub secret: bool,
    /// A search box: typing is just navigation.
    #[serde(default)]
    pub search: bool,
    /// Where a link goes (absolute), if it's a real link.
    #[serde(default)]
    pub href: Option<String>,
    /// Clicking it sends a form.
    #[serde(default)]
    pub submits: bool,
    /// A disclosure: tab, summary, `aria-expanded`.
    #[serde(default)]
    pub toggles: bool,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub disabled: bool,
}

impl Element {
    fn label_refs(&self) -> Vec<&str> {
        self.labels.iter().map(String::as_str).collect()
    }
    pub fn name(&self) -> &str {
        self.labels.first().map(String::as_str).unwrap_or("")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Act<'a> {
    Click(&'a Element),
    Type(&'a Element, &'a str),
    Select(&'a Element, &'a str),
    Navigate(&'a Url),
}

/// What the gate knows besides the action.
pub struct Ctx<'a> {
    pub level: AgentBrowsing,
    /// The approved plan, if any.
    pub plan: Option<&'a Plan>,
    pub taint: &'a Taint,
    /// Links that appeared on pages the agent read.
    pub seen_links: &'a HashSet<String>,
    /// The page the action happens on.
    pub page: &'a Url,
    /// This tab carries a login the person lent.
    pub lent: bool,
    /// The person is looking at this tab now.
    pub watching: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Go ahead. `step` is the plan step it matched; `near` when the label
    /// matched only nearly (the critic checks those).
    Allow {
        step: Option<usize>,
        near: bool,
    },
    /// Ask the person first, saying why.
    Ask(String),
    /// A consequential action: the person confirms in one click.
    Confirm(String),
    /// Wait until the person is watching (or ask them while they're away).
    Away(String),
    /// The person does this step themselves.
    Handover(String),
    Refuse(String),
}

impl Verdict {
    pub fn allowed(&self) -> bool {
        matches!(self, Verdict::Allow { .. })
    }
}

/// Words on a button, or in a page's path, that mean the click can't be
/// taken back, costs money, or gives something away.
const CONSEQUENTIAL: &[&str] = &[
    "delete",
    "remove",
    "destroy",
    "erase",
    "terminate",
    "close account",
    "cancel subscription",
    "transfer",
    "pay",
    "payment",
    "purchase",
    "buy",
    "checkout",
    "check out",
    "place order",
    "order now",
    "subscribe",
    "upgrade",
    "downgrade",
    "authorize",
    "authorise",
    "allow access",
    "grant",
    "approve",
    "accept",
    "revoke",
    "rotate",
    "regenerate",
    "reset",
    "disable",
    "deactivate",
    "suspend",
    "send",
    "invite",
    "share",
    "publish",
    "merge",
    "deploy",
    "confirm",
    "make public",
    "change owner",
    "leave",
];
const CONSEQUENTIAL_PATHS: &[&str] = &[
    "billing",
    "checkout",
    "payment",
    "payments",
    "pay",
    "purchase",
    "oauth",
    "authorize",
    "consent",
    "danger",
    "delete",
];
/// Buttons that only show, hide or move between views.
const HARMLESS: &[&str] = &[
    "next",
    "continue",
    "back",
    "previous",
    "show",
    "show more",
    "more",
    "less",
    "expand",
    "collapse",
    "close",
    "cancel",
    "menu",
    "details",
    "view",
    "filter",
    "sort",
    "search",
    "open",
    "learn more",
    "help",
    "skip",
    "dismiss",
    "got it",
    "ok",
];

fn has_word(label: &str, word: &str) -> bool {
    format!(" {} ", norm(label)).contains(&format!(" {word} "))
}

/// Whether clicking `e` on `page` is consequential.
pub fn consequential(e: &Element, page: &Url) -> bool {
    let words = e
        .labels
        .iter()
        .any(|l| CONSEQUENTIAL.iter().any(|w| has_word(l, w)));
    let path = page
        .path_segments()
        .into_iter()
        .flatten()
        .any(|seg| CONSEQUENTIAL_PATHS.contains(&seg.to_ascii_lowercase().as_str()));
    words || (path && (e.submits || !e.toggles && e.href.is_none()))
}

/// Whether a click only shows, hides or moves around: a disclosure, or a
/// non-submitting button named like one.
pub fn harmless(e: &Element) -> bool {
    if e.submits {
        return false;
    }
    e.toggles
        || e.role == "tab"
        || e.tag == "summary"
        || e.labels
            .first()
            .is_some_and(|l| HARMLESS.contains(&norm(l).as_str()))
}

fn site(u: &Url) -> String {
    super::site(u).unwrap_or_default()
}

fn mutating_on_write(ctx: &Ctx, consequential: bool, v: Verdict) -> Verdict {
    // Watch mode: a write with a lent login, or anything consequential,
    // waits for the person to watch.
    if (ctx.lent || consequential)
        && !ctx.watching
        && !matches!(v, Verdict::Refuse(_) | Verdict::Handover(_))
    {
        return Verdict::Away(
            "the person isn't watching this tab; it waits for them (they're notified)".into(),
        );
    }
    v
}

pub fn decide(ctx: &Ctx, act: &Act) -> Verdict {
    use AgentBrowsing::*;
    let on = site(ctx.page);
    let plan_write = |s: &str| ctx.plan.is_some_and(|p| p.is_write(s));
    match ctx.level {
        Off => {
            return Verdict::Refuse(
                "agents can't use the browser; the person turns it on in Settings → Browser".into(),
            );
        }
        ReadOnly if !matches!(act, Act::Navigate(_)) => {
            return Verdict::Refuse(
                "the browser is read-only for agents here: no clicking or typing".into(),
            );
        }
        _ => {}
    }
    match act {
        Act::Navigate(u) => {
            if let Err(e) = super::check_static(u) {
                return Verdict::Refuse(e.to_string());
            }
            let to = site(u);
            if ctx.lent && !plan_write(&to) {
                return Verdict::Refuse(format!(
                    "this tab carries the person's {on} login, so it stays on the plan's write sites; open {to} in a new tab with tab_browser_open"
                ));
            }
            if plan_write(&to) {
                return Verdict::Allow {
                    step: None,
                    near: false,
                };
            }
            if ctx.taint.found_in(u.as_str()).is_some() {
                return Verdict::Refuse(format!(
                    "that address carries something read on a write site; it can't go to {to}"
                ));
            }
            // Before anything was read on a write site there's nothing to
            // carry off. After, an address it made up (not a link it saw,
            // not a bare site) is asked about: taint can miss short strings.
            let bare = u.path() == "/" && u.query().is_none();
            if ctx.taint.is_empty()
                || ctx.seen_links.contains(u.as_str())
                || bare
                || ctx.level == ReadOnly
            {
                return Verdict::Allow {
                    step: None,
                    near: false,
                };
            }
            Verdict::Ask(format!("go to an address it made up on {to}"))
        }
        Act::Click(e) => {
            if e.secret {
                return Verdict::Handover("that's a secret field; the person fills it in".into());
            }
            // A real link that doesn't submit is navigation.
            if let Some(href) = e.href.as_deref().and_then(|h| Url::parse(h).ok())
                && !e.submits
                && matches!(href.scheme(), "http" | "https")
            {
                return decide(ctx, &Act::Navigate(&href));
            }
            let big = consequential(e, ctx.page);
            if !plan_write(&on) {
                if harmless(e) && !big {
                    return Verdict::Allow {
                        step: None,
                        near: false,
                    };
                }
                return match (ctx.level, ctx.plan) {
                    (Plan, None) => Verdict::Refuse(
                        "propose a plan with tab_browser_plan first; without one, agents only read"
                            .into(),
                    ),
                    _ => Verdict::Refuse(format!(
                        "{on} isn't a write site in the plan: only reading and harmless clicks there"
                    )),
                };
            }
            let v = if ctx.level == Ask {
                Verdict::Ask(format!("click \"{}\" on {on}", e.name()))
            } else {
                let plan = ctx.plan.expect("plan_write implies a plan");
                let best = plan
                    .steps
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| matches!(s.action, Do::Click | Do::Submit))
                    .map(|(i, s)| {
                        (
                            i,
                            label_match(s.what.as_deref().unwrap_or(""), &e.label_refs()),
                        )
                    })
                    .max_by_key(|(_, m)| *m);
                match best {
                    Some((i, Match::Exact)) => Verdict::Allow {
                        step: Some(i),
                        near: false,
                    },
                    Some((i, Match::Near)) => Verdict::Allow {
                        step: Some(i),
                        near: true,
                    },
                    _ if harmless(e) && !big => Verdict::Allow {
                        step: None,
                        near: false,
                    },
                    _ => Verdict::Ask(format!(
                        "click \"{}\" on {on}, which isn't in the plan",
                        e.name()
                    )),
                }
            };
            let v = match v {
                Verdict::Allow { .. } if big => {
                    Verdict::Confirm(format!("click \"{}\" on {on}", e.name()))
                }
                v => v,
            };
            if harmless(e) && !big {
                return v;
            }
            mutating_on_write(ctx, big, v)
        }
        Act::Type(e, value) | Act::Select(e, value) => {
            if e.secret {
                return Verdict::Handover("that's a secret field; the person types it".into());
            }
            if !plan_write(&on) {
                if ctx.taint.found_in(value).is_some() {
                    return Verdict::Refuse(format!(
                        "that text carries something read on a write site; it can't go to {on}"
                    ));
                }
                if e.search && ctx.level != ReadOnly {
                    return Verdict::Allow {
                        step: None,
                        near: false,
                    };
                }
                return Verdict::Refuse(format!(
                    "{on} isn't a write site in the plan: agents don't type there (except into search)"
                ));
            }
            let v = if ctx.level == Ask {
                Verdict::Ask(format!("type \"{value}\" into \"{}\" on {on}", e.name()))
            } else {
                let plan = ctx.plan.expect("plan_write implies a plan");
                let want = if matches!(act, Act::Type(..)) {
                    Do::Type
                } else {
                    Do::Select
                };
                let mut planned: Option<&str> = None;
                let mut verdict = None;
                for (i, s) in plan
                    .steps
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.action == want)
                {
                    let m = label_match(s.field.as_deref().unwrap_or(""), &e.label_refs());
                    if m == Match::No {
                        continue;
                    }
                    if s.value.as_deref() == Some(*value) {
                        verdict = Some(Verdict::Allow {
                            step: Some(i),
                            near: m == Match::Near,
                        });
                        if m == Match::Exact {
                            break;
                        }
                    } else {
                        planned = planned.or(s.value.as_deref());
                    }
                }
                verdict.unwrap_or_else(|| match planned {
                    Some(p) => Verdict::Ask(format!(
                        "type \"{value}\" into \"{}\" on {on}; the plan said \"{p}\"",
                        e.name()
                    )),
                    None => Verdict::Ask(format!(
                        "type \"{value}\" into \"{}\" on {on}, which isn't in the plan",
                        e.name()
                    )),
                })
            };
            mutating_on_write(ctx, false, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn dns() -> Plan {
        serde_json::from_value::<Plan>(serde_json::json!({
            "goal": "Point www at the app",
            "write": ["dash.cloudflare.com"],
            "steps": [
                { "do": "click", "what": "Add record" },
                { "do": "type", "field": "Name", "value": "www" },
                { "do": "type", "field": "Target", "value": "app.example.com" },
                { "do": "submit", "what": "Save" },
                { "do": "click", "what": "Delete record" }
            ]
        }))
        .unwrap()
        .validate()
        .unwrap()
    }

    fn el(labels: &[&str]) -> Element {
        Element {
            id: "e1".into(),
            tag: "button".into(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    struct World {
        plan: Plan,
        taint: Taint,
        seen: HashSet<String>,
        page: Url,
    }

    fn world() -> World {
        let mut taint = Taint::default();
        taint.add_page("Zone id 023e105f4ecef8ad9ca31a8372d0c353 owner alice@example.com");
        World {
            plan: dns(),
            taint,
            seen: HashSet::from(["https://developers.cloudflare.com/dns/".to_string()]),
            page: Url::parse("https://dash.cloudflare.com/abc/example.com/dns").unwrap(),
        }
    }

    fn ctx<'a>(w: &'a World, level: AgentBrowsing, plan: bool) -> Ctx<'a> {
        Ctx {
            level,
            plan: plan.then_some(&w.plan),
            taint: &w.taint,
            seen_links: &w.seen,
            page: &w.page,
            lent: false,
            watching: true,
        }
    }

    #[test]
    fn planned_steps_with_exact_values_go_through() {
        let w = world();
        let c = ctx(&w, AgentBrowsing::Plan, true);
        assert_eq!(
            decide(&c, &Act::Click(&el(&["Add record"]))),
            Verdict::Allow {
                step: Some(0),
                near: false
            }
        );
        assert_eq!(
            decide(&c, &Act::Click(&el(&["Add record now"]))),
            Verdict::Allow {
                step: Some(0),
                near: true
            }
        );
        assert!(decide(&c, &Act::Type(&el(&["Name"]), "www")).allowed());
        assert!(
            matches!(decide(&c, &Act::Type(&el(&["Name"]), "api")), Verdict::Ask(m) if m.contains("the plan said \"www\""))
        );
        assert!(matches!(
            decide(&c, &Act::Type(&el(&["Comment"]), "hi")),
            Verdict::Ask(_)
        ));
        let mut save = el(&["Save"]);
        save.submits = true;
        assert!(decide(&c, &Act::Click(&save)).allowed());
        assert!(matches!(
            decide(&c, &Act::Click(&el(&["Export zone"]))),
            Verdict::Ask(_)
        ));
    }

    #[test]
    fn consequential_clicks_always_confirm_even_when_planned() {
        let w = world();
        let c = ctx(&w, AgentBrowsing::Plan, true);
        assert!(matches!(
            decide(&c, &Act::Click(&el(&["Delete record"]))),
            Verdict::Confirm(_)
        ));
        let mut pay = el(&["Save"]);
        pay.submits = true;
        let billing = Url::parse("https://dash.cloudflare.com/abc/billing").unwrap();
        let c = Ctx {
            page: &billing,
            ..ctx(&w, AgentBrowsing::Plan, true)
        };
        assert!(matches!(decide(&c, &Act::Click(&pay)), Verdict::Confirm(_)));
    }

    #[test]
    fn secrets_are_handed_to_the_person() {
        let w = world();
        let mut pw = el(&["Password"]);
        pw.secret = true;
        for level in [AgentBrowsing::Plan, AgentBrowsing::Ask] {
            let c = ctx(&w, level, true);
            assert!(matches!(
                decide(&c, &Act::Type(&pw, "hunter22")),
                Verdict::Handover(_)
            ));
            assert!(matches!(decide(&c, &Act::Click(&pw)), Verdict::Handover(_)));
        }
    }

    #[test]
    fn without_a_plan_only_reading_and_harmless_clicks() {
        let w = world();
        let c = ctx(&w, AgentBrowsing::Plan, false);
        assert!(
            matches!(decide(&c, &Act::Click(&el(&["Add record"]))), Verdict::Refuse(m) if m.contains("tab_browser_plan"))
        );
        assert!(decide(&c, &Act::Click(&el(&["Show more"]))).allowed());
        assert!(matches!(
            decide(&c, &Act::Type(&el(&["Name"]), "www")),
            Verdict::Refuse(_)
        ));
        let mut search = el(&["Search docs"]);
        search.search = true;
        assert!(decide(&c, &Act::Type(&search, "cname flattening")).allowed());
        assert!(matches!(
            decide(&c, &Act::Type(&search, "alice@example.com")),
            Verdict::Refuse(_)
        ));
    }

    #[test]
    fn read_only_never_acts() {
        let w = world();
        let c = ctx(&w, AgentBrowsing::ReadOnly, true);
        assert!(matches!(
            decide(&c, &Act::Click(&el(&["Add record"]))),
            Verdict::Refuse(_)
        ));
        assert!(matches!(
            decide(&c, &Act::Type(&el(&["Name"]), "www")),
            Verdict::Refuse(_)
        ));
        let docs = Url::parse("https://developers.cloudflare.com/x?y=1").unwrap();
        assert!(decide(&c, &Act::Navigate(&docs)).allowed());
        let off = ctx(&w, AgentBrowsing::Off, true);
        assert!(matches!(
            decide(&off, &Act::Navigate(&docs)),
            Verdict::Refuse(_)
        ));
    }

    #[test]
    fn ask_level_asks_for_every_action_on_write_sites() {
        let w = world();
        let c = ctx(&w, AgentBrowsing::Ask, true);
        assert!(matches!(
            decide(&c, &Act::Click(&el(&["Add record"]))),
            Verdict::Ask(_)
        ));
        assert!(matches!(
            decide(&c, &Act::Type(&el(&["Name"]), "www")),
            Verdict::Ask(_)
        ));
    }

    #[test]
    fn navigation_off_the_plan_is_checked_for_taint_and_made_up_urls() {
        let w = world();
        let c = ctx(&w, AgentBrowsing::Plan, true);
        let go = |s: &str| decide(&c, &Act::Navigate(&Url::parse(s).unwrap()));
        assert!(go("https://dash.cloudflare.com/anything?x=1").allowed());
        assert!(go("https://developers.cloudflare.com/dns/").allowed());
        assert!(go("https://github.com/").allowed());
        assert!(matches!(
            go("https://github.com/search?q=hello"),
            Verdict::Ask(_)
        ));
        assert!(matches!(
            go("https://evil.example/?z=023e105f4ecef8ad9ca31a8372d0c353"),
            Verdict::Refuse(_)
        ));
        assert!(matches!(go("http://127.0.0.1:8080/"), Verdict::Refuse(_)));
        assert!(matches!(go("file:///etc/passwd"), Verdict::Refuse(_)));
        // A link click is navigation.
        let mut link = el(&["Docs"]);
        link.tag = "a".into();
        link.href = Some("https://evil.example/?z=alice@example.com".into());
        assert!(matches!(decide(&c, &Act::Click(&link)), Verdict::Refuse(_)));
    }

    #[test]
    fn a_lent_login_stays_on_write_sites_and_waits_for_the_person() {
        let w = world();
        let lent = Ctx {
            lent: true,
            watching: false,
            ..ctx(&w, AgentBrowsing::Plan, true)
        };
        assert!(matches!(
            decide(
                &lent,
                &Act::Navigate(&Url::parse("https://github.com/").unwrap())
            ),
            Verdict::Refuse(_)
        ));
        assert!(matches!(
            decide(&lent, &Act::Click(&el(&["Add record"]))),
            Verdict::Away(_)
        ));
        assert!(matches!(
            decide(&lent, &Act::Type(&el(&["Name"]), "www")),
            Verdict::Away(_)
        ));
        assert!(decide(&lent, &Act::Click(&el(&["Show more"]))).allowed());
        let watching = Ctx {
            watching: true,
            ..lent
        };
        assert!(decide(&watching, &Act::Click(&el(&["Add record"]))).allowed());
        // Consequential waits too, even without a lent login.
        let away = Ctx {
            watching: false,
            ..ctx(&w, AgentBrowsing::Plan, true)
        };
        assert!(matches!(
            decide(&away, &Act::Click(&el(&["Delete record"]))),
            Verdict::Away(_)
        ));
        assert!(decide(&away, &Act::Click(&el(&["Add record"]))).allowed());
    }

    #[test]
    fn read_sites_refuse_forms_and_risky_clicks() {
        let w = world();
        let docs = Url::parse("https://developers.cloudflare.com.evil.net/page").unwrap();
        let c = Ctx {
            page: &docs,
            ..ctx(&w, AgentBrowsing::Plan, true)
        };
        let mut submit = el(&["Add record"]);
        submit.submits = true;
        assert!(matches!(
            decide(&c, &Act::Click(&submit)),
            Verdict::Refuse(_)
        ));
        assert!(matches!(
            decide(&c, &Act::Type(&el(&["Name"]), "www")),
            Verdict::Refuse(_)
        ));
        assert!(matches!(
            decide(&c, &Act::Click(&el(&["Delete"]))),
            Verdict::Refuse(_)
        ));
    }

    fn arb_element() -> impl Strategy<Value = Element> {
        (
            prop::sample::select(vec![
                "Add record",
                "Name",
                "Target",
                "Save",
                "Delete record",
                "Show more",
                "Pay now",
                "x",
            ]),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
        )
            .prop_map(|(l, secret, submits, toggles, search)| Element {
                id: "e".into(),
                tag: "button".into(),
                labels: vec![l.to_string()],
                secret,
                submits,
                toggles,
                search,
                ..Default::default()
            })
    }

    fn arb_level() -> impl Strategy<Value = AgentBrowsing> {
        prop::sample::select(vec![
            AgentBrowsing::Off,
            AgentBrowsing::ReadOnly,
            AgentBrowsing::Plan,
            AgentBrowsing::Ask,
        ])
    }

    proptest! {
        /// Without an approved plan, nothing that writes is ever allowed.
        #[test]
        fn no_write_without_a_plan(e in arb_element(), level in arb_level(), value in "[a-z0-9@.]{1,20}", lent: bool, watching: bool) {
            let w = world();
            let c = Ctx { lent, watching, ..ctx(&w, level, false) };
            prop_assert!(!decide(&c, &Act::Type(&e, &value)).allowed() || e.search);
            if decide(&c, &Act::Click(&e)).allowed() {
                prop_assert!(harmless(&e) && !consequential(&e, &w.page));
            }
        }

        /// Secret fields are never typed by an agent, at any level.
        #[test]
        fn secrets_never_typed(mut e in arb_element(), level in arb_level(), value in ".{1,20}", plan: bool) {
            e.secret = true;
            let w = world();
            let c = ctx(&w, level, plan);
            prop_assert!(!decide(&c, &Act::Type(&e, &value)).allowed());
        }

        /// What was read on a write site never leaves for another site.
        #[test]
        fn taint_never_reaches_another_site(prefix in "[a-z]{0,6}", host in "[a-z]{3,10}", level in arb_level(), plan: bool) {
            let w = world();
            let c = ctx(&w, level, plan);
            let u = Url::parse(&format!("https://{host}.example/?q={prefix}023e105f4ecef8ad9ca31a8372d0c353")).unwrap();
            prop_assert!(!decide(&c, &Act::Navigate(&u)).allowed());
            let mut search = el(&["Search"]);
            search.search = true;
            let other = Url::parse(&format!("https://{host}.example/")).unwrap();
            let c = Ctx { page: &other, ..c };
            let typed = format!("{prefix} alice@example.com");
            prop_assert!(!decide(&c, &Act::Type(&search, &typed)).allowed());
        }

        /// A value the plan doesn't have is never typed without asking.
        #[test]
        fn unplanned_values_ask(value in "[a-z0-9.]{1,20}") {
            prop_assume!(!["www", "app.example.com"].contains(&value.as_str()));
            let w = world();
            let c = ctx(&w, AgentBrowsing::Plan, true);
            prop_assert!(!decide(&c, &Act::Type(&el(&["Name"]), &value)).allowed());
            prop_assert!(!decide(&c, &Act::Type(&el(&["Target"]), &value)).allowed());
        }

        /// Narrowing a plan never allows something the wider plan didn't.
        #[test]
        fn narrowing_never_widens(e in arb_element(), drop in 0usize..5, value in prop::sample::select(vec!["www", "app.example.com", "zzz"])) {
            let w = world();
            let mut narrow = w.plan.clone();
            narrow.steps.remove(drop);
            let wide = ctx(&w, AgentBrowsing::Plan, true);
            let n = Ctx { plan: Some(&narrow), ..ctx(&w, AgentBrowsing::Plan, true) };
            if decide(&n, &Act::Click(&e)).allowed() {
                prop_assert!(decide(&wide, &Act::Click(&e)).allowed());
            }
            if decide(&n, &Act::Type(&e, value)).allowed() {
                prop_assert!(decide(&wide, &Act::Type(&e, value)).allowed());
            }
        }
    }
}
