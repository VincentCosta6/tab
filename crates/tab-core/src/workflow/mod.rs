//! Workflows. The definitions, validation and templates are pure and live in
//! [`tab_domain::workflow`] (re-exported here); this module adds the
//! [`runtime`] that runs them off the desktop engine's event stream.

pub use tab_domain::workflow::*;

pub mod command;
pub mod runtime;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ConnectorInfo;
    use serde_json::{Map, json};

    #[test]
    fn validation_reads_the_catalog() {
        let catalog: Vec<ConnectorInfo> = crate::connectors::registry()
            .into_iter()
            .map(|f| ConnectorInfo {
                descriptor: f.descriptor(),
                enabled: true,
                status: crate::model::ConnectorStatus::Starting,
            })
            .collect();
        for mut t in templates() {
            fill_defaults(&mut t, &catalog);
            let errors: Vec<_> = validate(&t, &catalog, &[])
                .into_iter()
                .filter(|i| i.error)
                .collect();
            assert!(errors.is_empty(), "{}: {errors:?}", t.name);
        }
        let mut bad = templates().remove(1);
        if let StepKind::Action { input, .. } = &mut bad.steps[2].kind {
            input.insert("title".into(), json!("{{steps.later.x}}"));
        }
        bad.steps.push(Step {
            id: "merge".into(),
            label: String::new(),
            when: vec![],
            continue_on_error: false,
            kind: StepKind::Action {
                connector: "github".into(),
                action: "pr.merge".into(),
                input: Map::new(),
            },
        });
        let issues = validate(&bad, &catalog, &[]);
        let msgs: Vec<_> = issues.iter().map(|i| i.message.as_str()).collect();
        assert!(
            msgs.iter().any(|m| m.contains("hasn't run yet")),
            "{msgs:?}"
        );
        assert!(
            msgs.iter().any(|m| m.contains("Repository is required")),
            "{msgs:?}"
        );
        assert!(msgs.iter().any(|m| m.contains("destructive")), "{msgs:?}");
    }
}
