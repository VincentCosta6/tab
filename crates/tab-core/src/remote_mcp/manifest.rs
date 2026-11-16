//! Queue recipes as data (see `docs/recipe-manifests.md`).
//!
//! A manifest is a JSON file that describes a service tab knows the MCP
//! server of: its catalog entry, and optionally how to fill the queue from
//! it: which of the server's tools to call, and how their answers map to
//! queue items. One engine ([`ManifestRecipe`]) runs every manifest.
//!
//! The built-in manifests (`manifests/*.json`) are compiled in. The
//! person's own go in `<data>/recipes/*.json` ([`load_dir`]); one with the
//! same id as a built-in takes its place. Manifests that don't ship with
//! tab may only call tools the server marks read-only.
//!
//! Strings in a manifest are templates: `{path}` reads the current item
//! (`{status.type}`) or a variable (`{$org.slug}`), `??` falls back
//! (`{url ?? permalink}`), `'text'` is a literal and `|` applies a filter
//! (`name`, `lower`, `short_id`, `nodash`, `prefix`, `clip:N`). A template with a
//! missing value is missing as a whole; where a manifest gives a list of
//! templates, the first that isn't missing wins. There are no conditions
//! or loops beyond a step's `for_each`: what needs code belongs in a
//! connector plugin.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::CatalogEntry;
use super::plugin::{Plugin, PluginSpec};
use super::sync::{Found, QueueChoice, Recipe, Session, clip, list, name_of, time_of};
use crate::model::{Block, EntityRef, Field, IncomingEvent, KindInfo, Link, Tone};

/// The manifest format this tab reads.
pub const VERSION: u32 = 1;
/// At most this many steps per manifest...
const MAX_STEPS: usize = 10;
/// ...and this many tool calls per look.
const MAX_CALLS: usize = 50;
/// Looks no more often than this.
const MIN_INTERVAL: Duration = Duration::from_secs(60);

/// The built-ins, in catalog order.
const BUILT_IN: &[(&str, &str)] = &[
    ("linear.json", include_str!("manifests/linear.json")),
    ("notion.json", include_str!("manifests/notion.json")),
    ("atlassian.json", include_str!("manifests/atlassian.json")),
    ("sentry.json", include_str!("manifests/sentry.json")),
    ("vercel.json", include_str!("manifests/vercel.json")),
    ("stripe.json", include_str!("manifests/stripe.json")),
];

// ---------------------------------------------------------------------------
// The format
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// [`VERSION`].
    pub manifest: u32,
    pub id: String,
    pub name: String,
    /// The MCP server's address.
    pub url: String,
    /// What agents can do with it, for the catalog.
    pub description: String,
    #[serde(default = "work")]
    pub category: String,
    #[serde(default = "grey")]
    pub color: String,
    /// For the marketplace: which release of the manifest this is.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    /// How it fills the queue. Without one, it gives agents tools only.
    #[serde(default)]
    pub queue: Option<Queue>,
}

fn work() -> String {
    "work".into()
}

fn grey() -> String {
    "#6b7280".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Queue {
    /// What comes in and when it's marked done, for the connection's page.
    pub description: String,
    /// How often to look: `"30s"`, `"5m"`, `"1h"`.
    #[serde(default = "five_minutes")]
    pub interval: String,
    pub kinds: Vec<KindSpec>,
    /// What the person can pick from; the first is the default. Each sets
    /// variables the steps read.
    #[serde(default)]
    pub choices: Vec<ChoiceSpec>,
    /// A search the person types instead, read as `$query`.
    #[serde(default)]
    pub search: Option<SearchSpec>,
    /// Later looks ask only for what changed (`$since`), so what's missing
    /// from a list just didn't change.
    #[serde(default)]
    pub incremental: Option<IncrementalSpec>,
    /// Run in order; the last one's list becomes the items.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// How the last step's list maps to items.
    #[serde(default)]
    pub item: Option<ItemSpec>,
    /// Asks about an item that dropped out of the list.
    #[serde(default)]
    pub check: Option<CheckSpec>,
    /// A program that does the looking instead of `steps`, `item` and
    /// `check` ([`super::plugin`]). Only for manifests in the person's own
    /// folder.
    #[serde(default)]
    pub plugin: Option<PluginSpec>,
}

fn five_minutes() -> String {
    "5m".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindSpec {
    pub kind: String,
    pub label: String,
    pub icon: String,
    #[serde(default)]
    pub tone: Tone,
    /// Where items of this kind live upstream (a Sentry project, a Linear
    /// team), so tab can route them to a project. Written to the graph as
    /// an entity with `rel = "container"`.
    #[serde(default)]
    pub container: Option<ContainerSpec>,
    /// Items of this kind that share a container stack into one queue row
    /// when many are open at once (a project's errors).
    #[serde(default)]
    pub stack: bool,
}

/// See [`KindSpec::container`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerSpec {
    /// The entity kind, `<service>.<what>`: `sentry.project`, `linear.team`.
    pub kind: String,
    pub key: Tpl,
    pub label: Tpl,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceSpec {
    pub value: String,
    pub label: String,
    #[serde(default)]
    pub vars: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchSpec {
    /// The placeholder, and the search when the person leaves it empty.
    pub hint: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncrementalSpec {
    /// How far before the last look `$since` reaches, for clock skew.
    #[serde(default = "no_time")]
    pub overlap: String,
}

fn no_time() -> String {
    "0s".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// How later steps' `for_each` refer to its list.
    #[serde(default)]
    pub name: Option<String>,
    /// The first of these tools the server has.
    pub call: Vec<String>,
    /// Templates; an argument whose template is missing is left out.
    #[serde(default)]
    pub args: Map<String, Value>,
    /// Where the list is in the answer, when it isn't the answer itself.
    #[serde(default)]
    pub list: Vec<String>,
    /// Calls once per combination, binding each variable in turn.
    #[serde(default)]
    pub for_each: Vec<ForEach>,
    /// At most this many from the step.
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub page: Option<PageSpec>,
    /// The error when the server has none of `call`. Without one, the step
    /// finds nothing.
    #[serde(default)]
    pub if_missing: Option<String>,
    /// The error when the step finds nothing.
    #[serde(default)]
    pub if_empty: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForEach {
    pub var: String,
    /// An earlier step's name, or `$var` for a list variable.
    #[serde(rename = "in")]
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageSpec {
    /// The next page's cursor, read from the answer.
    pub cursor: String,
    /// Whether there's another page; without it, a cursor means yes.
    #[serde(default)]
    pub more: Option<String>,
    /// The argument the cursor goes in.
    pub arg: String,
    #[serde(default = "three")]
    pub max: usize,
}

fn three() -> usize {
    3
}

/// One template or several, the first that isn't missing winning.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Tpl {
    One(String),
    Any(Vec<String>),
}

impl Tpl {
    fn all(&self) -> &[String] {
        match self {
            Tpl::One(s) => std::slice::from_ref(s),
            Tpl::Any(v) => v,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemSpec {
    /// Unique within the connection; becomes `external_id`.
    pub id: Tpl,
    /// The first kind when left out.
    #[serde(default)]
    pub kind: Option<KindPick>,
    pub title: Tpl,
    #[serde(default)]
    pub body: Option<Tpl>,
    #[serde(default)]
    pub url: Option<Tpl>,
    #[serde(default)]
    pub actor: Option<Tpl>,
    #[serde(default)]
    pub context: Option<Tpl>,
    /// When it happened (RFC 3339); now when missing.
    #[serde(default)]
    pub time: Option<Tpl>,
    /// Finished upstream when any of these holds.
    #[serde(default)]
    pub resolved: Vec<Cond>,
    #[serde(default)]
    pub fields: Vec<FieldSpec>,
    #[serde(default)]
    pub fields_as: FieldsAs,
    #[serde(default)]
    pub text: Option<TextSpec>,
    /// The label of a link to `url`.
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub entities: Vec<EntitySpec>,
    /// Kept with the item, for `check` (`$metadata.…`). Missing is null.
    #[serde(default)]
    pub metadata: BTreeMap<String, Tpl>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum KindPick {
    Fixed(String),
    Map {
        from: Tpl,
        map: BTreeMap<String, String>,
        default: String,
    },
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FieldsAs {
    #[default]
    Fields,
    Badges,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cond {
    pub value: Tpl,
    /// Holds when the value is one of these (any case)...
    #[serde(default, rename = "in")]
    pub one_of: Vec<String>,
    /// ...or, with `set`, when there is a value at all.
    #[serde(default)]
    pub set: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldSpec {
    pub label: String,
    pub value: Tpl,
    #[serde(default)]
    pub tone: Tone,
    /// A tone per value, over `tone`.
    #[serde(default)]
    pub tones: BTreeMap<String, Tone>,
    /// Values that leave the field out.
    #[serde(default)]
    pub skip: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextSpec {
    pub value: Tpl,
    #[serde(default)]
    pub markdown: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntitySpec {
    pub kind: String,
    pub key: Tpl,
    pub label: Tpl,
    #[serde(default = "rel_in")]
    pub rel: String,
}

fn rel_in() -> String {
    "in".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckSpec {
    pub call: Vec<String>,
    /// Templates over `$metadata` and `$external_id`.
    #[serde(default)]
    pub args: Map<String, Value>,
    /// Where the thing is in the answer, when it's wrapped.
    #[serde(default)]
    pub unwrap: Vec<String>,
    /// Present when the answer is the thing (otherwise tab can't tell).
    pub known: Tpl,
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

const FILTERS: &[&str] = &[
    "name", "lower", "short_id", "nodash", "clip", "prefix", "not",
];

/// What a template reads: the current thing, and variables.
struct Scope<'a> {
    item: Option<&'a Value>,
    vars: &'a Map<String, Value>,
}

/// `{…}` parts of a template, or an error for unbalanced braces.
fn parts(tpl: &str) -> Result<Vec<(bool, &str)>, String> {
    let mut out = Vec::new();
    let mut rest = tpl;
    while let Some(open) = rest.find('{') {
        if open > 0 {
            out.push((false, &rest[..open]));
        }
        let close = rest[open..]
            .find('}')
            .ok_or_else(|| format!("unclosed {{ in \"{tpl}\""))?;
        out.push((true, &rest[open + 1..open + close]));
        rest = &rest[open + close + 1..];
    }
    if rest.contains('}') {
        return Err(format!("unopened }} in \"{tpl}\""));
    }
    if !rest.is_empty() {
        out.push((false, rest));
    }
    Ok(out)
}

/// Checks a template's paths and filters.
fn check_tpl(tpl: &str) -> Result<(), String> {
    for (expr, text) in parts(tpl)? {
        if !expr {
            continue;
        }
        for alt in text.split("??").map(str::trim) {
            if alt.is_empty() {
                return Err(format!("empty value in \"{tpl}\""));
            }
            if alt.starts_with('\'') {
                if alt.len() < 2 || !alt.ends_with('\'') {
                    return Err(format!("unclosed ' in \"{tpl}\""));
                }
                continue;
            }
            for f in alt.split('|').skip(1).map(str::trim) {
                let (name, arg) = f.split_once(':').unwrap_or((f, ""));
                if !FILTERS.contains(&name) {
                    return Err(format!("unknown filter \"{name}\" in \"{tpl}\""));
                }
                if name == "clip" && arg.parse::<usize>().is_err() {
                    return Err(format!("clip needs a length (clip:300) in \"{tpl}\""));
                }
                if name == "not" && arg.is_empty() {
                    return Err(format!("not needs a value (not:Error) in \"{tpl}\""));
                }
            }
        }
    }
    Ok(())
}

/// A value that counts: not null, not an empty string.
fn present(v: Value) -> Option<Value> {
    match &v {
        Value::Null => None,
        Value::String(s) if s.trim().is_empty() => None,
        _ => Some(v),
    }
}

fn lookup(path: &str, sc: &Scope<'_>) -> Option<Value> {
    let (root, rest) = match path.strip_prefix('$') {
        Some(p) => {
            let (var, rest) = p.split_once('.').unwrap_or((p, ""));
            (sc.vars.get(var)?, rest)
        }
        None => (sc.item?, path),
    };
    let mut v = root;
    for seg in rest.split('.').filter(|s| !s.is_empty()) {
        v = match v {
            Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => v.get(seg)?,
        };
    }
    present(v.clone())
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn filter(v: Value, f: &str) -> Option<Value> {
    let (name, arg) = f.split_once(':').unwrap_or((f, ""));
    let s = match name {
        "name" => name_of(&v)?,
        "lower" => text(&v)?.to_lowercase(),
        "short_id" => text(&v).filter(|s| looks_like_short_id(s))?,
        "nodash" => text(&v)?.replace('-', ""),
        // `TAB-123` → `TAB`: a Linear or Jira key's team.
        "prefix" => text(&v)?.split_once('-')?.0.to_string(),
        "clip" => clip(&text(&v)?, arg.parse().ok()?),
        "not" => text(&v).filter(|s| s != arg)?,
        _ => return None,
    };
    present(Value::String(s))
}

fn eval(expr: &str, sc: &Scope<'_>) -> Option<Value> {
    expr.split("??").map(str::trim).find_map(|alt| {
        if let Some(lit) = alt.strip_prefix('\'').and_then(|a| a.strip_suffix('\'')) {
            return Some(Value::String(lit.to_string()));
        }
        let mut it = alt.split('|').map(str::trim);
        let mut v = lookup(it.next()?, sc)?;
        for f in it {
            v = filter(v, f)?;
        }
        Some(v)
    })
}

/// A template as text; missing when any part is.
fn render_one(tpl: &str, sc: &Scope<'_>) -> Option<String> {
    let mut out = String::new();
    for (expr, part) in parts(tpl).ok()? {
        if expr {
            out.push_str(&text(&eval(part, sc)?)?);
        } else {
            out.push_str(part);
        }
    }
    Some(out)
}

fn render(tpl: &Tpl, sc: &Scope<'_>) -> Option<String> {
    tpl.all().iter().find_map(|t| render_one(t, sc))
}

/// A template as a value: a lone `{…}` keeps its JSON type (a number, a
/// list), anything else is text.
fn value_of(tpl: &str, sc: &Scope<'_>) -> Option<Value> {
    match parts(tpl).ok()?.as_slice() {
        [(true, expr)] => eval(expr, sc),
        _ => render_one(tpl, sc).map(Value::String),
    }
}

fn value(tpl: &Tpl, sc: &Scope<'_>) -> Option<Value> {
    tpl.all().iter().find_map(|t| value_of(t, sc))
}

/// Tool arguments: strings are templates, and a missing one is left out.
fn args(spec: &Map<String, Value>, sc: &Scope<'_>) -> Value {
    fn fill(v: &Value, sc: &Scope<'_>) -> Option<Value> {
        match v {
            Value::String(t) => value_of(t, sc),
            Value::Array(a) => Some(Value::Array(a.iter().filter_map(|x| fill(x, sc)).collect())),
            Value::Object(o) => Some(Value::Object(
                o.iter()
                    .filter_map(|(k, x)| Some((k.clone(), fill(x, sc)?)))
                    .collect(),
            )),
            other => Some(other.clone()),
        }
    }
    Value::Object(
        spec.iter()
            .filter_map(|(k, v)| Some((k.clone(), fill(v, sc)?)))
            .collect(),
    )
}

fn holds(c: &Cond, sc: &Scope<'_>) -> bool {
    if c.set {
        return value(&c.value, sc).is_some();
    }
    render(&c.value, sc).is_some_and(|v| c.one_of.iter().any(|o| o.eq_ignore_ascii_case(&v)))
}

/// `PROJ-12` or `PROJ-1A2`: letters, a dash, then letters and digits.
pub(crate) fn looks_like_short_id(s: &str) -> bool {
    let Some((a, b)) = s.rsplit_once('-') else {
        return false;
    };
    !a.is_empty()
        && !b.is_empty()
        && a.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && b.chars().all(|c| c.is_ascii_alphanumeric())
}

/// `"30s"`, `"5m"`, `"1h"`.
fn duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (n, unit) = s.split_at(s.len().saturating_sub(1));
    let n: u64 = n
        .parse()
        .map_err(|_| format!("\"{s}\" isn't a duration like 5m"))?;
    Ok(Duration::from_secs(match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        _ => return Err(format!("\"{s}\" isn't a duration like 5m")),
    }))
}

// ---------------------------------------------------------------------------
// Checking a manifest
// ---------------------------------------------------------------------------

impl Manifest {
    pub fn parse(json: &str) -> Result<Manifest, String> {
        let m: Manifest = serde_json::from_str(json).map_err(|e| e.to_string())?;
        m.validate()?;
        Ok(m)
    }

    fn validate(&self) -> Result<(), String> {
        if self.manifest != VERSION {
            return Err(format!(
                "manifest version {} isn't one this tab reads ({VERSION})",
                self.manifest
            ));
        }
        if !super::valid_id(&self.id) {
            return Err(format!("\"{}\" can't be an id", self.id));
        }
        if self.name.trim().is_empty() || self.url.trim().is_empty() {
            return Err("a manifest needs a name and a url".into());
        }
        let Some(q) = &self.queue else {
            return Ok(());
        };
        let interval = duration(&q.interval)?;
        if interval < MIN_INTERVAL {
            return Err("the queue can't look more often than once a minute".into());
        }
        if let Some(i) = &q.incremental {
            duration(&i.overlap)?;
        }
        if q.kinds.is_empty() {
            return Err("the queue needs at least one kind".into());
        }
        // A plugin can name containers itself.
        if let Some(k) = q
            .kinds
            .iter()
            .find(|k| k.stack && k.container.is_none() && q.plugin.is_none())
        {
            return Err(format!(
                "kind \"{}\" stacks by its container, so it needs one",
                k.kind
            ));
        }
        if let Some(p) = &q.plugin {
            if !q.steps.is_empty() || q.item.is_some() || q.check.is_some() {
                return Err("a queue with a plugin has no steps, item or check".into());
            }
            if p.command.trim().is_empty() {
                return Err("the plugin needs a command".into());
            }
            duration(&p.timeout)?;
        } else if q.steps.is_empty() || q.steps.len() > MAX_STEPS {
            return Err(format!("the queue needs 1 to {MAX_STEPS} steps"));
        }
        let mut values: Vec<&str> = Vec::new();
        for c in &q.choices {
            if values.contains(&c.value.as_str()) {
                return Err(format!("choice \"{}\" is there twice", c.value));
            }
            values.push(&c.value);
        }
        let mut names: Vec<&str> = Vec::new();
        for (i, s) in q.steps.iter().enumerate() {
            if s.call.is_empty() {
                return Err(format!("step {} calls nothing", i + 1));
            }
            for f in &s.for_each {
                if !f.source.starts_with('$') && !names.contains(&f.source.as_str()) {
                    return Err(format!(
                        "step {} goes through \"{}\", which isn't an earlier step",
                        i + 1,
                        f.source
                    ));
                }
            }
            check_args(&s.args)?;
            if let Some(p) = &s.page {
                check_tpl(&p.cursor)?;
                if let Some(m) = &p.more {
                    check_tpl(m)?;
                }
            }
            if let Some(n) = &s.name {
                names.push(n);
            }
        }
        let Some(it) = &q.item else {
            if q.plugin.is_none() {
                return Err("the queue needs an item".into());
            }
            return Ok(());
        };
        let mut tpls: Vec<&Tpl> = vec![&it.id, &it.title];
        tpls.extend(
            [&it.body, &it.url, &it.actor, &it.context, &it.time]
                .into_iter()
                .flatten(),
        );
        tpls.extend(it.resolved.iter().map(|c| &c.value));
        tpls.extend(it.fields.iter().map(|f| &f.value));
        tpls.extend(it.text.iter().map(|t| &t.value));
        tpls.extend(it.entities.iter().flat_map(|e| [&e.key, &e.label]));
        tpls.extend(it.metadata.values());
        for c in q.kinds.iter().filter_map(|k| k.container.as_ref()) {
            if !c.kind.contains('.') {
                return Err(format!(
                    "a container's kind is <service>.<what>, like sentry.project, not \"{}\"",
                    c.kind
                ));
            }
            tpls.extend([&c.key, &c.label]);
        }
        if let Some(c) = &q.check {
            tpls.push(&c.known);
            check_args(&c.args)?;
        }
        for t in tpls {
            for s in t.all() {
                check_tpl(s)?;
            }
        }
        let kinds: Vec<&str> = q.kinds.iter().map(|k| k.kind.as_str()).collect();
        let picked: Vec<&str> = match &it.kind {
            None => vec![],
            Some(KindPick::Fixed(k)) => vec![k],
            Some(KindPick::Map { map, default, from }) => {
                for s in from.all() {
                    check_tpl(s)?;
                }
                map.values()
                    .map(String::as_str)
                    .chain([default.as_str()])
                    .collect()
            }
        };
        if let Some(k) = picked.iter().find(|k| !kinds.contains(k)) {
            return Err(format!("item kind \"{k}\" isn't one of the queue's kinds"));
        }
        Ok(())
    }
}

fn check_args(a: &Map<String, Value>) -> Result<(), String> {
    fn walk(v: &Value) -> Result<(), String> {
        match v {
            Value::String(s) => check_tpl(s),
            Value::Array(a) => a.iter().try_for_each(walk),
            Value::Object(o) => o.values().try_for_each(walk),
            _ => Ok(()),
        }
    }
    a.values().try_for_each(walk)
}

// ---------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------

/// Runs one manifest's queue.
pub struct ManifestRecipe {
    m: &'static Manifest,
    q: &'static Queue,
    choices: &'static [QueueChoice],
    interval: Duration,
    overlap: Option<chrono::Duration>,
    /// Shipped with tab (and reviewed with it): may call any tool.
    built_in: bool,
    plugin: Option<Plugin>,
}

impl ManifestRecipe {
    /// `dir` is the manifest's folder, where its plugin runs.
    fn new(m: &'static Manifest, built_in: bool, dir: Option<&Path>) -> Option<ManifestRecipe> {
        let q = m.queue.as_ref()?;
        let plugin = match &q.plugin {
            Some(spec) => Some(Plugin {
                name: &m.name,
                spec,
                dir: dir?.to_path_buf(),
                timeout: duration(&spec.timeout).ok()?,
                built_in,
            }),
            None => None,
        };
        let choices: Vec<QueueChoice> = q
            .choices
            .iter()
            .map(|c| QueueChoice {
                value: &c.value,
                label: &c.label,
            })
            .collect();
        Some(ManifestRecipe {
            m,
            q,
            choices: Box::leak(choices.into_boxed_slice()),
            interval: duration(&q.interval).ok()?,
            overlap: q
                .incremental
                .as_ref()
                .and_then(|i| duration(&i.overlap).ok())
                .and_then(|d| chrono::Duration::from_std(d).ok()),
            built_in,
            plugin,
        })
    }

    /// The variables a look starts with: the person's choice (or search),
    /// and `$since`.
    fn vars(
        &self,
        choice: Option<&str>,
        query: Option<&str>,
        since: Option<DateTime<Utc>>,
    ) -> Map<String, Value> {
        let mut vars = Map::new();
        let choice = self
            .q
            .choices
            .iter()
            .find(|c| Some(c.value.as_str()) == choice)
            .or(self.q.choices.first());
        if let Some(c) = choice {
            vars.extend(c.vars.clone());
        }
        if let Some(s) = &self.q.search {
            vars.insert(
                "query".into(),
                Value::String(query.unwrap_or(&s.hint).to_string()),
            );
        }
        if let (Some(t), Some(o)) = (since, self.overlap) {
            vars.insert("since".into(), Value::String((t - o).to_rfc3339()));
        }
        vars
    }

    async fn call(&self, s: &Session<'_>, tool: &str, args: Value) -> anyhow::Result<Value> {
        anyhow::ensure!(
            self.built_in || s.read_only(tool),
            "{} can't call {tool}: the server doesn't mark it read-only",
            self.m.name
        );
        s.call(tool, args).await
    }

    /// One item, or `None` when it has no id or title.
    fn item(&self, v: &Value, vars: &Map<String, Value>) -> Option<Found> {
        let it = self.q.item.as_ref()?;
        let sc = Scope {
            item: Some(v),
            vars,
        };
        let id = render(&it.id, &sc)?;
        let title = render(&it.title, &sc)?;
        let kind = match &it.kind {
            None => self.q.kinds[0].kind.clone(),
            Some(KindPick::Fixed(k)) => k.clone(),
            Some(KindPick::Map { from, map, default }) => render(from, &sc)
                .and_then(|v| map.get(&v).cloned())
                .unwrap_or_else(|| default.clone()),
        };
        let get = |t: &Option<Tpl>| t.as_ref().and_then(|t| render(t, &sc));
        let url = get(&it.url);
        let fields: Vec<Field> = it
            .fields
            .iter()
            .filter_map(|f| {
                let v = render(&f.value, &sc).filter(|v| !f.skip.contains(v))?;
                let tone = f.tones.get(&v).copied().unwrap_or(f.tone);
                Some(Field::new(f.label.clone(), v).tone(tone))
            })
            .collect();
        let mut blocks = Vec::new();
        if !fields.is_empty() {
            blocks.push(match it.fields_as {
                FieldsAs::Fields => Block::Fields { items: fields },
                FieldsAs::Badges => Block::Badges { items: fields },
            });
        }
        if let Some(t) = &it.text
            && let Some(text) = render(&t.value, &sc)
        {
            blocks.push(Block::Text {
                text,
                markdown: t.markdown,
            });
        }
        if let (Some(label), Some(u)) = (&it.link, &url) {
            blocks.push(Block::Links {
                items: vec![Link {
                    label: label.clone(),
                    url: u.clone(),
                }],
            });
        }
        let container = self
            .q
            .kinds
            .iter()
            .find(|k| k.kind == kind)
            .and_then(|k| k.container.as_ref())
            .and_then(|c| {
                Some(EntityRef::new(
                    &c.kind,
                    render(&c.key, &sc)?,
                    render(&c.label, &sc)?,
                    crate::model::CONTAINER_REL,
                ))
            });
        let entities = it
            .entities
            .iter()
            .filter_map(|e| {
                Some(EntityRef {
                    kind: e.kind.clone(),
                    key: render(&e.key, &sc)?,
                    label: render(&e.label, &sc)?,
                    rel: e.rel.clone(),
                })
            })
            .chain(container)
            .collect();
        let metadata: Map<String, Value> = it
            .metadata
            .iter()
            .map(|(k, t)| (k.clone(), value(t, &sc).unwrap_or(Value::Null)))
            .collect();
        Some(Found {
            resolved: it.resolved.iter().any(|c| holds(c, &sc)),
            event: IncomingEvent {
                source: String::new(),
                external_id: id,
                kind,
                title,
                body: get(&it.body),
                url,
                actor: get(&it.actor),
                context: get(&it.context),
                occurred_at: get(&it.time)
                    .and_then(|t| time_of(&Value::String(t)))
                    .unwrap_or_else(Utc::now),
                blocks,
                entities,
                metadata: Value::Object(metadata),
            },
        })
    }
}

/// Every combination of a step's `for_each`, outer first.
fn bindings(
    step: &Step,
    vars: &Map<String, Value>,
    lists: &HashMap<String, Vec<Value>>,
) -> Vec<Map<String, Value>> {
    let mut out = vec![Map::new()];
    for f in &step.for_each {
        let items: Vec<Value> = match f.source.strip_prefix('$') {
            Some(v) => match vars.get(v) {
                Some(Value::Array(a)) => a.clone(),
                Some(Value::Null) | None => Vec::new(),
                Some(one) => vec![one.clone()],
            },
            None => lists.get(&f.source).cloned().unwrap_or_default(),
        };
        out = out
            .into_iter()
            .flat_map(|b| {
                items.iter().map(move |x| {
                    let mut b = b.clone();
                    b.insert(f.var.clone(), x.clone());
                    b
                })
            })
            .collect();
    }
    out
}

#[async_trait]
impl Recipe for ManifestRecipe {
    fn kinds(&self) -> Vec<KindInfo> {
        self.q
            .kinds
            .iter()
            .map(|k| KindInfo {
                stack: k.stack,
                ..KindInfo::new(&k.kind, &k.label, &k.icon, k.tone)
            })
            .collect()
    }

    fn description(&self) -> &'static str {
        &self.q.description
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn incremental(&self) -> bool {
        self.q.incremental.is_some()
    }

    fn query_hint(&self) -> Option<&'static str> {
        self.q.search.as_ref().map(|s| s.hint.as_str())
    }

    fn queue_choices(&self) -> &'static [QueueChoice] {
        self.choices
    }

    async fn poll(
        &self,
        s: &Session<'_>,
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<Found>> {
        if let Some(p) = &self.plugin {
            let kinds: Vec<&str> = self.q.kinds.iter().map(|k| k.kind.as_str()).collect();
            let mut out = p.poll(s, &kinds, since).await?;
            if !(self.incremental() && since.is_some()) {
                out.retain(|f| !f.resolved);
            }
            return Ok(out);
        }
        let vars = self.vars(s.choice, s.query, since);
        let mut lists: HashMap<String, Vec<Value>> = HashMap::new();
        let mut out = Vec::new();
        let mut calls = 0;
        let last = self.q.steps.len() - 1;
        for (i, step) in self.q.steps.iter().enumerate() {
            let names: Vec<&str> = step.call.iter().map(String::as_str).collect();
            let Some(tool) = s.pick(&names) else {
                if let Some(e) = &step.if_missing {
                    anyhow::bail!("{e}");
                }
                if let Some(n) = &step.name {
                    lists.insert(n.clone(), Vec::new());
                }
                continue;
            };
            let mut found: Vec<Value> = Vec::new();
            let mut items: Vec<Found> = Vec::new();
            for b in bindings(step, &vars, &lists) {
                let mut vars = vars.clone();
                vars.extend(b);
                let mut a = args(
                    &step.args,
                    &Scope {
                        item: None,
                        vars: &vars,
                    },
                );
                for _ in 0..step.page.as_ref().map_or(1, |p| p.max.max(1)) {
                    calls += 1;
                    anyhow::ensure!(
                        calls <= MAX_CALLS,
                        "{} made more than {MAX_CALLS} calls in one look",
                        self.m.name
                    );
                    let v = self.call(s, tool, a.clone()).await?;
                    let keys: Vec<&str> = step.list.iter().map(String::as_str).collect();
                    for x in list(&v, &keys) {
                        if i == last {
                            items.extend(self.item(x, &vars));
                        } else {
                            found.push(x.clone());
                        }
                    }
                    let Some(p) = &step.page else { break };
                    let sc = Scope {
                        item: Some(&v),
                        vars: &vars,
                    };
                    let more = match &p.more {
                        Some(m) => render_one(m, &sc).as_deref() == Some("true"),
                        None => true,
                    };
                    match render_one(&p.cursor, &sc) {
                        Some(c) if more => a[p.arg.as_str()] = Value::String(c),
                        _ => break,
                    }
                }
            }
            if let Some(n) = step.limit {
                found.truncate(n);
                items.truncate(n);
            }
            if found.is_empty()
                && items.is_empty()
                && let Some(e) = &step.if_empty
            {
                anyhow::bail!("{e}");
            }
            if let Some(n) = &step.name {
                lists.insert(n.clone(), found);
            }
            out.extend(items);
        }
        // What already finished only matters to a look that lists changes:
        // the first look, and lists of everything open, skip it.
        if !(self.incremental() && since.is_some()) {
            out.retain(|f| !f.resolved);
        }
        Ok(out)
    }

    async fn check(&self, s: &Session<'_>, item: &IncomingEvent) -> anyhow::Result<Option<bool>> {
        if let Some(p) = &self.plugin {
            return p.check(s, item).await;
        }
        let (Some(c), Some(it)) = (&self.q.check, &self.q.item) else {
            return Ok(None);
        };
        let names: Vec<&str> = c.call.iter().map(String::as_str).collect();
        let Some(tool) = s.pick(&names) else {
            return Ok(None);
        };
        let mut vars = Map::new();
        vars.insert("metadata".into(), item.metadata.clone());
        vars.insert(
            "external_id".into(),
            Value::String(item.external_id.clone()),
        );
        let a = args(
            &c.args,
            &Scope {
                item: None,
                vars: &vars,
            },
        );
        let v = self.call(s, tool, a).await?;
        let thing = c
            .unwrap
            .iter()
            .map(|k| &v[k.as_str()])
            .find(|x| x.is_object())
            .unwrap_or(&v);
        let sc = Scope {
            item: Some(thing),
            vars: &vars,
        };
        if render(&c.known, &sc).is_none() {
            return Ok(None);
        }
        Ok(Some(it.resolved.iter().any(|c| holds(c, &sc))))
    }
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// Where a manifest came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    BuiltIn,
    /// The person's own folder.
    Yours,
    /// Installed from the marketplace ([`super::marketplace`]).
    Marketplace,
}

struct Entry {
    m: &'static Manifest,
    recipe: Option<&'static ManifestRecipe>,
    source: Source,
    path: Option<PathBuf>,
}

fn entry(m: Manifest, source: Source, path: Option<PathBuf>) -> Entry {
    // Manifests live as long as tab: a reload leaks the old ones, which is
    // a few kilobytes on a rare click.
    let m: &'static Manifest = Box::leak(Box::new(m));
    let dir = path.as_deref().and_then(Path::parent);
    let recipe =
        ManifestRecipe::new(m, source == Source::BuiltIn, dir).map(|r| &*Box::leak(Box::new(r)));
    Entry {
        m,
        recipe,
        source,
        path,
    }
}

fn built_ins() -> Vec<Entry> {
    BUILT_IN
        .iter()
        .map(|(file, json)| {
            let m =
                Manifest::parse(json).unwrap_or_else(|e| panic!("built-in manifest {file}: {e}"));
            entry(m, Source::BuiltIn, None)
        })
        .collect()
}

struct Registry {
    entries: Vec<Entry>,
    /// The person's folder, once loaded.
    dir: Option<PathBuf>,
    errors: Vec<LoadError>,
}

static REGISTRY: LazyLock<RwLock<Registry>> = LazyLock::new(|| {
    RwLock::new(Registry {
        entries: built_ins(),
        dir: None,
        errors: Vec::new(),
    })
});

/// A manifest that didn't load, and why.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LoadError {
    pub path: PathBuf,
    pub error: String,
}

/// One manifest tab has, for the Connections page.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ManifestInfo {
    pub id: String,
    pub name: String,
    pub source: Source,
    pub fills_queue: bool,
    /// Runs a plugin rather than steps.
    pub plugin: bool,
    pub path: Option<PathBuf>,
    /// It takes the place of the built-in with the same id.
    pub replaces_built_in: bool,
}

/// What's loaded, from where, and what didn't load.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Status {
    pub dir: Option<PathBuf>,
    pub manifests: Vec<ManifestInfo>,
    pub errors: Vec<LoadError>,
}

/// The folder marketplace installs go in, under the person's.
pub const MARKETPLACE_DIR: &str = "marketplace";

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths
}

/// Loads the person's manifests from `dir` (`*.json`) and the
/// marketplace's from `dir/marketplace`, replacing the ones loaded before.
/// Theirs beat the marketplace's, which beat the built-ins, by id. A missing
/// folder is no manifests.
pub fn load_dir(dir: &Path) -> Status {
    let mut errors = Vec::new();
    let mut theirs: Vec<(Manifest, Source, PathBuf)> = Vec::new();
    let found = json_files(dir)
        .into_iter()
        .map(|p| (p, Source::Yours))
        .chain(
            json_files(&dir.join(MARKETPLACE_DIR))
                .into_iter()
                .map(|p| (p, Source::Marketplace)),
        );
    for (path, source) in found {
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|json| Manifest::parse(&json))
            .and_then(|m| {
                if source == Source::Marketplace
                    && m.queue.as_ref().is_some_and(|q| q.plugin.is_some())
                {
                    return Err("manifests from the marketplace can't run plugins".into());
                }
                match theirs.iter().find(|(t, _, _)| t.id == m.id) {
                    Some((_, Source::Yours, _)) if source == Source::Marketplace => Err(format!(
                        "your own manifest for \"{}\" takes its place",
                        m.id
                    )),
                    Some(_) => Err(format!(
                        "another manifest here already has the id \"{}\"",
                        m.id
                    )),
                    None => Ok(m),
                }
            });
        match parsed {
            Ok(m) => theirs.push((m, source, path)),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "manifest didn't load");
                errors.push(LoadError { path, error });
            }
        }
    }
    let mut entries: Vec<Entry> = built_ins()
        .into_iter()
        .filter(|b| !theirs.iter().any(|(t, _, _)| t.id == b.m.id))
        .collect();
    entries.extend(
        theirs
            .into_iter()
            .map(|(m, source, path)| entry(m, source, Some(path))),
    );
    let mut r = REGISTRY.write().unwrap();
    *r = Registry {
        entries,
        dir: Some(dir.to_path_buf()),
        errors,
    };
    status_of(&r)
}

/// Loads the person's folder again (after they changed it).
pub fn reload() -> Option<Status> {
    let dir = REGISTRY.read().unwrap().dir.clone()?;
    Some(load_dir(&dir))
}

/// What's loaded now.
pub fn status() -> Status {
    status_of(&REGISTRY.read().unwrap())
}

fn status_of(r: &Registry) -> Status {
    let built_in: Vec<String> = BUILT_IN
        .iter()
        .filter_map(|(f, _)| f.strip_suffix(".json").map(str::to_string))
        .collect();
    Status {
        dir: r.dir.clone(),
        manifests: r
            .entries
            .iter()
            .map(|e| ManifestInfo {
                id: e.m.id.clone(),
                name: e.m.name.clone(),
                source: e.source,
                fills_queue: e.recipe.is_some(),
                plugin: e.recipe.is_some_and(|r| r.plugin.is_some()),
                path: e.path.clone(),
                replaces_built_in: e.source != Source::BuiltIn && built_in.contains(&e.m.id),
            })
            .collect(),
        errors: r.errors.clone(),
    }
}

/// A recipe from one manifest, outside the registry.
#[cfg(test)]
pub(crate) fn recipe_from(
    json: &str,
    source: Source,
    path: Option<PathBuf>,
) -> &'static dyn Recipe {
    entry(Manifest::parse(json).unwrap(), source, path)
        .recipe
        .unwrap()
}

/// The recipe for a catalog entry, if its manifest fills the queue.
pub fn recipe(id: &str) -> Option<&'static dyn Recipe> {
    REGISTRY
        .read()
        .unwrap()
        .entries
        .iter()
        .find(|e| e.m.id == id)
        .and_then(|e| e.recipe)
        .map(|r| r as &'static dyn Recipe)
}

/// Every service tab knows, from the manifests.
pub fn catalog() -> Vec<CatalogEntry> {
    catalog_of(&REGISTRY.read().unwrap().entries)
}

fn catalog_of(entries: &[Entry]) -> Vec<CatalogEntry> {
    entries
        .iter()
        .map(|e| {
            let m = e.m;
            let r = e.recipe;
            CatalogEntry {
                id: &m.id,
                name: &m.name,
                url: &m.url,
                description: &m.description,
                category: &m.category,
                color: &m.color,
                fills_queue: r.is_some(),
                kinds: r.map(|r| r.kinds()).unwrap_or_default(),
                queue_query: r.and_then(|r| r.query_hint()),
                queue_choices: r.map(|r| r.queue_choices()).unwrap_or_default(),
                queue_description: r.map(|r| r.description()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn built_in(id: &str) -> ManifestRecipe {
        let (_, json) = BUILT_IN
            .iter()
            .find(|(f, _)| f.strip_suffix(".json") == Some(id))
            .unwrap();
        let m: &'static Manifest = Box::leak(Box::new(Manifest::parse(json).unwrap()));
        ManifestRecipe::new(m, true, None).unwrap()
    }

    /// Items from one answer of the last step, as a look would map them.
    fn map(r: &ManifestRecipe, v: &Value, vars: Map<String, Value>) -> Vec<Found> {
        let step = r.q.steps.last().unwrap();
        let keys: Vec<&str> = step.list.iter().map(String::as_str).collect();
        list(v, &keys)
            .into_iter()
            .filter_map(|x| r.item(x, &vars))
            .collect()
    }

    #[test]
    fn built_ins_load() {
        // Not `catalog()`: another test loads manifests of its own.
        let cat = catalog_of(&built_ins());
        let ids: Vec<&str> = cat.iter().map(|c| c.id).collect();
        assert_eq!(
            ids,
            [
                "linear",
                "notion",
                "atlassian",
                "sentry",
                "vercel",
                "stripe"
            ]
        );
        let find = |id: &str| cat.iter().find(|c| c.id == id).unwrap();
        let sentry = find("sentry");
        assert!(sentry.fills_queue);
        assert_eq!(sentry.queue_choices[0].value, "mine_unassigned");
        assert!(sentry.queue_description.is_some());
        let stripe = find("stripe");
        assert!(!stripe.fills_queue && stripe.queue_description.is_none());
    }

    #[test]
    fn templates_read_paths_fall_back_and_filter() {
        let vars: Map<String, Value> =
            serde_json::from_value(json!({ "org": { "slug": "acme" } })).unwrap();
        let item = json!({ "a": { "b": 3 }, "empty": "", "s": "ENG-1", "u": "1f-2e",
            "st": { "name": "Done" }, "long": "abcdef", "list": [1, 2] });
        let sc = Scope {
            item: Some(&item),
            vars: &vars,
        };
        let r = |t: &str| render_one(t, &sc);
        assert_eq!(r("{$org.slug}/{a.b}").as_deref(), Some("acme/3"));
        assert_eq!(r("{empty ?? missing ?? 'x'}").as_deref(), Some("x"));
        assert_eq!(r("{missing}"), None);
        assert_eq!(r("no braces").as_deref(), Some("no braces"));
        assert_eq!(r("{st|name|lower}").as_deref(), Some("done"));
        assert_eq!(r("{s|short_id}").as_deref(), Some("ENG-1"));
        assert_eq!(r("{missing|short_id ?? u|nodash}").as_deref(), Some("1f2e"));
        assert_eq!(r("{long|clip:3}").as_deref(), Some("abc…"));
        assert_eq!(value_of("{list}", &sc), Some(json!([1, 2])));
        assert_eq!(value_of("{a.b}", &sc), Some(json!(3)));
        assert_eq!(
            args(
                &serde_json::from_value(json!({ "n": "{a.b}", "gone": "{missing}", "k": 1 }))
                    .unwrap(),
                &sc
            ),
            json!({ "n": 3, "k": 1 })
        );
        assert!(check_tpl("{a | nope}").is_err());
        assert!(check_tpl("{a").is_err());
        assert!(check_tpl("{a|clip}").is_err());
        assert!(check_tpl("{a ?? 'x' ?? b|clip:4}").is_ok());
    }

    #[test]
    fn bad_manifests_say_why() {
        let base = json!({ "manifest": 1, "id": "thing", "name": "Thing", "url": "https://x", "description": "d" });
        let with = |patch: Value| {
            let mut m = base.clone();
            m.as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            Manifest::parse(&m.to_string())
        };
        assert!(with(json!({})).is_ok());
        assert!(
            with(json!({ "manifest": 2 }))
                .unwrap_err()
                .contains("version")
        );
        assert!(with(json!({ "id": "tab" })).unwrap_err().contains("id"));
        assert!(with(json!({ "colour": "red" })).is_err());
        let queue = |q: Value| {
            let mut full = json!({
                "description": "d", "kinds": [{ "kind": "thing.x", "label": "X", "icon": "x" }],
                "steps": [{ "call": ["list"] }], "item": { "id": "{id}", "title": "{title}" },
            });
            full.as_object_mut()
                .unwrap()
                .extend(q.as_object().unwrap().clone());
            with(json!({ "queue": full }))
        };
        assert!(queue(json!({})).is_ok());
        assert!(
            queue(json!({ "interval": "10s" }))
                .unwrap_err()
                .contains("once a minute")
        );
        assert!(queue(json!({ "interval": "soon" })).is_err());
        assert!(queue(json!({ "steps": [] })).is_err());
        assert!(
            queue(json!({ "steps": [{ "call": ["a"], "for_each": [{ "var": "x", "in": "nowhere" }] }] }))
                .unwrap_err()
                .contains("earlier step")
        );
        assert!(queue(json!({ "item": { "id": "{id|bogus}", "title": "t" } })).is_err());
        assert!(
            queue(json!({ "item": { "id": "{id}", "title": "t", "kind": "other.kind" } }))
                .unwrap_err()
                .contains("kind")
        );
    }

    #[test]
    fn plugins_replace_steps_and_choices_sit_beside_a_search() {
        let base = |queue: Value| {
            json!({ "manifest": 1, "id": "thing", "name": "Thing", "url": "https://x", "description": "d",
                "queue": queue })
            .to_string()
        };
        let kinds = json!([{ "kind": "thing.x", "label": "X", "icon": "x" }]);
        let plugin = json!({ "command": "node", "args": ["p.js"] });
        assert!(
            Manifest::parse(&base(
                json!({ "description": "d", "kinds": kinds, "plugin": plugin })
            ))
            .is_ok()
        );
        let both = base(
            json!({ "description": "d", "kinds": kinds, "plugin": plugin,
            "steps": [{ "call": ["a"] }] }),
        );
        assert!(Manifest::parse(&both).unwrap_err().contains("plugin"));
        let no_item =
            base(json!({ "description": "d", "kinds": kinds, "steps": [{ "call": ["a"] }] }));
        assert!(Manifest::parse(&no_item).unwrap_err().contains("item"));

        let m: &'static Manifest = Box::leak(Box::new(
            Manifest::parse(&base(json!({ "description": "d", "kinds": kinds,
                "choices": [{ "value": "open", "label": "Open", "vars": { "state": "open" } },
                            { "value": "all", "label": "All", "vars": { "state": "any" } }],
                "search": { "hint": "bugs" },
                "steps": [{ "call": ["a"] }], "item": { "id": "{id}", "title": "{title}" } })))
            .unwrap(),
        ));
        let r = ManifestRecipe::new(m, false, None).unwrap();
        let v = r.vars(Some("all"), Some("crash"), None);
        assert_eq!(
            (v["state"].as_str(), v["query"].as_str()),
            (Some("any"), Some("crash"))
        );
        let v = r.vars(None, None, None);
        assert_eq!(
            (v["state"].as_str(), v["query"].as_str()),
            (Some("open"), Some("bugs"))
        );
    }

    #[test]
    fn linear_maps_issues_and_spots_finished_ones() {
        let r = built_in("linear");
        let v = json!({ "issues": [
            { "id": "abc", "identifier": "ENG-12", "title": "Fix login", "url": "https://linear.app/x/issue/ENG-12",
              "status": "In Progress", "priority": { "value": 2, "name": "High" }, "team": "Engineering",
              "updatedAt": "2026-11-08T10:00:00.000Z", "description": "Steps…" },
            { "id": "def", "identifier": "ENG-13", "title": "Old", "status": "Done",
              "completedAt": "2026-11-07T10:00:00.000Z" },
            { "id": "ghi", "identifier": "ENG-14", "title": "Other", "status": { "name": "Won't do", "type": "canceled" } },
            { "title": "no id" },
        ] });
        let found = map(&r, &v, Map::new());
        assert_eq!(found.len(), 3);
        let a = &found[0];
        assert!(!a.resolved);
        assert_eq!(a.event.external_id, "abc");
        assert_eq!(a.event.kind, "linear.issue");
        assert_eq!(a.event.title, "ENG-12 Fix login");
        assert_eq!(a.event.context.as_deref(), Some("Engineering"));
        // Routed by team, keyed by the team's key so `ENG-12` in a branch
        // name finds it.
        let c = a
            .event
            .entities
            .iter()
            .find(|x| x.rel == "container")
            .unwrap();
        assert_eq!(
            (c.key.as_str(), c.label.as_str()),
            ("linear:team:ENG", "Engineering")
        );
        assert_eq!(
            a.event.occurred_at.to_rfc3339(),
            "2026-11-08T10:00:00+00:00"
        );
        let Block::Badges { items } = &a.event.blocks[0] else {
            panic!("{:?}", a.event.blocks)
        };
        assert!(
            items
                .iter()
                .any(|f| f.label == "Priority" && f.value == "High" && f.tone == Tone::Warning)
        );
        assert!(found[1].resolved);
        assert!(found[2].resolved);
        // Newer servers: the key in `id`, the UUID in `uuid`, `statusType`.
        let one = |v: Value| map(&r, &json!([v]), Map::new()).pop().unwrap();
        let n = one(
            json!({ "id": "ENG-1", "uuid": "u-1", "title": "New", "status": "Shipped",
            "statusType": "completed", "archivedAt": null }),
        );
        assert_eq!(n.event.external_id, "u-1");
        assert_eq!(n.event.title, "ENG-1 New");
        assert_eq!(n.event.metadata["identifier"], "ENG-1");
        assert!(n.event.entities.iter().any(|x| x.key == "linear:team:ENG"));
        assert!(n.resolved);
        let open = one(json!({ "id": "ENG-2", "title": "Open", "status": "Todo",
            "statusType": "unstarted", "completedAt": null }));
        assert!(!open.resolved);
        // Older servers: a UUID in `id` and no key keeps the title as is.
        let old = one(json!({ "id": "4b1c9e2a", "title": "Plain" }));
        assert_eq!(old.event.title, "Plain");
        assert!(old.event.metadata["identifier"].is_null());
        assert!(!old.event.entities.iter().any(|x| x.rel == "container"));
    }

    #[test]
    fn linear_asks_for_changes_since_the_last_look() {
        let r = built_in("linear");
        assert!(r.incremental());
        let t = DateTime::parse_from_rfc3339("2026-11-08T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let vars = r.vars(None, None, Some(t));
        assert_eq!(vars["since"], "2026-11-08T09:55:00+00:00");
        let a = args(
            &r.q.steps[0].args,
            &Scope {
                item: None,
                vars: &vars,
            },
        );
        assert_eq!(a["updatedAt"], "2026-11-08T09:55:00+00:00");
        // The first look has no `since`, so it asks for everything open.
        let first = args(
            &r.q.steps[0].args,
            &Scope {
                item: None,
                vars: &r.vars(None, None, None),
            },
        );
        assert!(first.get("updatedAt").is_none());
        assert_eq!(first["assignee"], "me");
    }

    #[test]
    fn sentry_maps_issues() {
        let r = built_in("sentry");
        let vars: Map<String, Value> = serde_json::from_value(
            json!({ "org": { "slug": "acme", "regionUrl": "https://us.sentry.io" } }),
        )
        .unwrap();
        let j = json!({ "query": "is:unresolved assigned:me", "issues": [
            { "shortId": "WEB-1", "title": "Boom", "url": "https://acme.sentry.io/issues/WEB-1", "status": "unresolved",
              "occurrences": 42, "usersImpacted": 3, "culprit": "app/foo.js in bar", "projectSlug": "web",
              "lastSeen": "2026-11-08T09:00:00Z" },
            { "shortId": "API-9", "status": "resolved" },
        ] });
        let found = map(&r, &j, vars);
        let e = &found[0].event;
        assert_eq!(e.external_id, "acme/WEB-1");
        assert_eq!(e.title, "WEB-1 Boom");
        assert_eq!(
            e.url.as_deref(),
            Some("https://acme.sentry.io/issues/WEB-1")
        );
        assert_eq!(e.context.as_deref(), Some("web"));
        assert_eq!(e.entities[0].key, "sentry:acme/web");
        // Its Sentry project is the container items route by.
        let c = e.entities.iter().find(|x| x.rel == "container").unwrap();
        assert_eq!(
            (c.kind.as_str(), c.key.as_str(), c.label.as_str()),
            ("sentry.project", "sentry:acme/web", "web")
        );
        assert_eq!(e.metadata["region_url"], "https://us.sentry.io");
        let Block::Fields { items } = &e.blocks[0] else {
            panic!()
        };
        assert_eq!(items[0].label, "Status");
        assert_eq!(items[0].tone, Tone::Danger);
        assert!(items.iter().any(|f| f.label == "Events" && f.value == "42"));
        assert!(!found[0].resolved);
        // No title or project: the short id, a link built from it, the org.
        let b = &found[1];
        assert!(b.resolved);
        assert_eq!(b.event.title, "API-9 API-9");
        assert_eq!(
            b.event.url.as_deref(),
            Some("https://acme.sentry.io/issues/API-9/")
        );
        assert_eq!(b.event.context.as_deref(), Some("acme"));
    }

    #[test]
    fn sentry_issues_stack_by_project_and_skip_vague_titles() {
        let r = built_in("sentry");
        assert!(r.kinds()[0].stack);
        let vars: Map<String, Value> =
            serde_json::from_value(json!({ "org": { "slug": "acme" } })).unwrap();
        let j = json!({ "issues": [
            { "shortId": "WEB-8", "title": "Error: Error", "culprit": "src/pay.ts in charge", "projectSlug": "web" },
            { "shortId": "WEB-9", "title": "Error: Error" },
        ] });
        let found = map(&r, &j, vars);
        assert_eq!(found[0].event.title, "WEB-8 src/pay.ts in charge");
        // Nothing better to say: the vague title stays.
        assert_eq!(found[1].event.title, "WEB-9 Error: Error");
    }

    #[test]
    fn a_stack_needs_a_container() {
        let m = |container: bool| {
            let mut kind =
                json!({ "kind": "acme.alert", "label": "Alert", "icon": "alert", "stack": true });
            if container {
                kind["container"] = json!({ "kind": "acme.service", "key": "acme:{service}", "label": "{service}" });
            }
            json!({ "manifest": 1, "id": "acme", "name": "Acme", "url": "https://mcp.acme.dev",
                "description": "d", "queue": { "description": "d", "kinds": [kind],
                    "steps": [{ "call": ["list_alerts"], "list": ["alerts"] }],
                    "item": { "id": "{id}", "title": "{title}" } } })
            .to_string()
        };
        assert!(Manifest::parse(&m(true)).is_ok());
        let err = Manifest::parse(&m(false)).unwrap_err();
        assert!(err.contains("needs one"), "{err}");
        let bad = m(true).replace("{title}", "{title|not:}");
        assert!(
            Manifest::parse(&bad)
                .unwrap_err()
                .contains("not needs a value")
        );
    }

    #[test]
    fn sentry_searches_for_the_persons_choice() {
        let r = built_in("sentry");
        let mine = "is:unresolved assigned:me";
        let unassigned = "is:unresolved is:unassigned";
        let searches = |q: Option<&str>| r.vars(q, None, None)["searches"].clone();
        assert_eq!(searches(None), json!([mine, unassigned]));
        assert_eq!(searches(Some("mine_unassigned")), json!([mine, unassigned]));
        assert_eq!(searches(Some("mine")), json!([mine]));
        assert_eq!(searches(Some("all")), json!(["is:unresolved"]));
        assert_eq!(searches(Some("whatever")), json!([mine, unassigned]));
        // Every org, then every search within it.
        let mut lists = HashMap::new();
        lists.insert(
            "orgs".to_string(),
            vec![json!({ "slug": "a" }), json!({ "slug": "b" })],
        );
        let b = bindings(&r.q.steps[1], &r.vars(None, None, None), &lists);
        let pairs: Vec<(Value, Value)> = b
            .iter()
            .map(|b| (b["org"]["slug"].clone(), b["search"].clone()))
            .collect();
        assert_eq!(
            pairs,
            [
                (json!("a"), json!(mine)),
                (json!("a"), json!(unassigned)),
                (json!("b"), json!(mine)),
                (json!("b"), json!(unassigned)),
            ]
        );
    }

    #[test]
    fn notion_maps_search_results() {
        let r = built_in("notion");
        assert_eq!(r.query_hint(), Some("tasks assigned to me"));
        assert_eq!(r.vars(None, None, None)["query"], "tasks assigned to me");
        assert_eq!(r.vars(None, Some("my docs"), None)["query"], "my docs");
        let v = json!({ "results": [
            { "id": "1f2e", "title": "Launch plan", "url": "https://www.notion.so/Launch-1f2e", "type": "page",
              "highlight": "Owner: you", "timestamp": "2026-11-01T00:00:00.000Z" },
            { "id": "db-1", "title": "", "type": "database" },
        ], "type": "workspace_search" });
        let found = map(&r, &v, Map::new());
        assert_eq!(found[0].event.title, "Launch plan");
        assert_eq!(found[0].event.kind, "notion.page");
        assert_eq!(found[0].event.body.as_deref(), Some("Owner: you"));
        assert_eq!(found[1].event.title, "Untitled");
        assert_eq!(found[1].event.kind, "notion.database");
        assert_eq!(
            found[1].event.url.as_deref(),
            Some("https://www.notion.so/db1")
        );
    }

    #[test]
    fn the_persons_manifests_load_and_can_replace_a_built_in() {
        let dir = std::env::temp_dir().join(format!("tab-recipes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mine = json!({ "manifest": 1, "id": "acme", "name": "Acme", "url": "https://mcp.acme.dev",
            "description": "Agents can read Acme.", "queue": {
                "description": "Acme tickets assigned to you.",
                "kinds": [{ "kind": "acme.ticket", "label": "Acme ticket", "icon": "alert" }],
                "steps": [{ "call": ["list_tickets"], "list": ["tickets"] }],
                "item": { "id": "{id}", "title": "{title}" } } });
        std::fs::write(dir.join("acme.json"), mine.to_string()).unwrap();
        let vercel = json!({ "manifest": 1, "id": "vercel", "name": "My Vercel", "url": "https://mcp.vercel.com",
            "description": "Mine." });
        std::fs::write(dir.join("vercel.json"), vercel.to_string()).unwrap();
        std::fs::write(dir.join("broken.json"), "{ nope").unwrap();
        let market = dir.join(MARKETPLACE_DIR);
        std::fs::create_dir_all(&market).unwrap();
        let listed = json!({ "manifest": 1, "id": "listed", "name": "Listed", "url": "https://mcp.listed.dev",
            "description": "From the marketplace.", "version": "1.0.0" });
        std::fs::write(market.join("listed.json"), listed.to_string()).unwrap();
        let sneaky = json!({ "manifest": 1, "id": "sneaky", "name": "Sneaky", "url": "https://x",
            "description": "d", "queue": { "description": "d",
                "kinds": [{ "kind": "s.x", "label": "X", "icon": "x" }],
                "plugin": { "command": "sh", "args": ["evil.sh"] } } });
        std::fs::write(market.join("sneaky.json"), sneaky.to_string()).unwrap();
        std::fs::write(dir.join("notes.txt"), "not a manifest").unwrap();

        let status = load_dir(&dir);
        let errors = &status.errors;
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].path.ends_with("broken.json"));
        assert!(errors[1].path.ends_with("sneaky.json") && errors[1].error.contains("plugins"));
        let info = |id: &str| {
            status
                .manifests
                .iter()
                .find(|m| m.id == id)
                .cloned()
                .unwrap()
        };
        assert_eq!(info("listed").source, Source::Marketplace);
        assert_eq!(info("acme").source, Source::Yours);
        assert!(info("vercel").replaces_built_in);
        assert_eq!(info("sentry").source, Source::BuiltIn);
        let cat = catalog();
        let acme = cat.iter().find(|c| c.id == "acme").unwrap();
        assert!(acme.fills_queue);
        assert_eq!(
            acme.queue_description,
            Some("Acme tickets assigned to you.")
        );
        assert_eq!(
            cat.iter().find(|c| c.id == "vercel").unwrap().name,
            "My Vercel"
        );
        assert_eq!(cat.iter().filter(|c| c.id == "vercel").count(), 1);
        assert!(recipe("acme").is_some());
        assert!(recipe("sentry").is_some());

        // Loading again replaces what was loaded: removing a file removes it.
        std::fs::remove_file(dir.join("acme.json")).unwrap();
        load_dir(&dir);
        assert!(recipe("acme").is_none());
        load_dir(&dir.join("missing"));
        assert_eq!(
            catalog().iter().find(|c| c.id == "vercel").unwrap().name,
            "Vercel"
        );
    }
}
