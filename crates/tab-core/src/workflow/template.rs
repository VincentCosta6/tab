//! `{{path}}` templates over a run's context.
//!
//! A value that is exactly one template keeps its type (`"{{item.metadata.number}}"`
//! stays a number); anything else renders to text. A path that leads nowhere
//! renders as empty. No expressions, no functions: what you see is what runs.

use serde_json::Value;

/// Reads `a.b.0.c` from `ctx`. Array elements are addressed by index.
pub fn lookup(ctx: &Value, path: &str) -> Option<Value> {
    let mut cur = ctx;
    for part in path.trim().split('.').filter(|p| !p.is_empty()) {
        cur = match cur {
            Value::Object(m) => m.get(part)?,
            Value::Array(a) => a.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur.clone())
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Array(a) => a.iter().map(text).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

/// Every `{{...}}` in a string, trimmed.
fn spans(s: &str) -> Vec<(usize, usize, &str)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(open) = s[at..].find("{{") {
        let start = at + open;
        let Some(close) = s[start + 2..].find("}}") else {
            break;
        };
        let end = start + 2 + close + 2;
        out.push((start, end, s[start + 2..end - 2].trim()));
        at = end;
    }
    out
}

pub fn render_str(s: &str, ctx: &Value) -> Value {
    let found = spans(s);
    if found.is_empty() {
        return Value::String(s.to_string());
    }
    if found.len() == 1 && found[0].0 == 0 && found[0].1 == s.len() {
        return lookup(ctx, found[0].2).unwrap_or(Value::Null);
    }
    let mut out = String::new();
    let mut last = 0;
    for (start, end, path) in found {
        out.push_str(&s[last..start]);
        out.push_str(&lookup(ctx, path).map(|v| text(&v)).unwrap_or_default());
        last = end;
    }
    out.push_str(&s[last..]);
    Value::String(out)
}

/// Renders every string inside `v`.
pub fn render(v: &Value, ctx: &Value) -> Value {
    match v {
        Value::String(s) => render_str(s, ctx),
        Value::Array(a) => Value::Array(a.iter().map(|x| render(x, ctx)).collect()),
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, x)| (k.clone(), render(x, ctx))).collect())
        }
        other => other.clone(),
    }
}

/// The paths `v` reads, for validation.
pub fn refs(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => spans(s)
            .into_iter()
            .map(|(_, _, p)| p.to_string())
            .collect(),
        Value::Array(a) => a.iter().flat_map(refs).collect(),
        Value::Object(m) => m.values().flat_map(refs).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_typed_and_text() {
        let ctx = json!({ "item": { "title": "Fix it", "metadata": { "number": 12, "labels": ["a", "b"] } }, "steps": { "pr": { "state": "open" } } });
        assert_eq!(render_str("{{item.metadata.number}}", &ctx), json!(12));
        assert_eq!(
            render_str("#{{ item.metadata.number }}: {{item.title}}", &ctx),
            json!("#12: Fix it")
        );
        assert_eq!(
            render_str("{{item.metadata.labels}}", &ctx),
            json!(["a", "b"])
        );
        assert_eq!(
            render_str("labels {{item.metadata.labels}}", &ctx),
            json!("labels a, b")
        );
        assert_eq!(render_str("{{nope.x}}", &ctx), Value::Null);
        assert_eq!(render_str("x{{nope}}y", &ctx), json!("xy"));
        assert_eq!(render_str("{{ unclosed", &ctx), json!("{{ unclosed"));
        assert_eq!(lookup(&ctx, "item.metadata.labels.1"), Some(json!("b")));
        assert_eq!(
            refs(&json!({ "a": "{{steps.pr.state}} {{item.id}}" })),
            ["steps.pr.state", "item.id"]
        );
    }
}
