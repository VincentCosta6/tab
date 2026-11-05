//! Prints every connector's actions and kinds, and the workflow templates, as
//! JSON. The browser mock (src/lib/mock-workflows.json) is generated from it:
//!
//! cargo run -p tab-core --example workflow_catalog > src/lib/mock-workflows.json

fn main() {
    let connectors: Vec<_> = tab_core::connectors::registry()
        .into_iter()
        .map(|f| f.descriptor())
        .map(|d| serde_json::json!({ "id": d.id, "name": d.name, "icon": d.icon, "kinds": d.kinds, "actions": d.actions }))
        .collect();
    let out = serde_json::json!({
        "connectors": connectors,
        "templates": tab_core::workflow::templates(),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
