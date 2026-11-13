//! Workflows. The definitions, validation and templates are pure and live in
//! [`tab_domain::workflow`] (re-exported here); this module adds the
//! [`runtime`] that runs them off the desktop engine's event stream.

pub use tab_domain::workflow::*;

pub mod command;
pub mod insights_store;
pub mod items;
pub mod runtime;

/// The builder assistant's chat key for one workflow: each workflow gets its
/// own conversation, and an unsaved draft uses `workflows:new` until the
/// assistant saves it (then that chat moves to the new id).
pub fn chat_key(workflow_id: &str) -> String {
    if workflow_id.is_empty() {
        "workflows:new".into()
    } else {
        format!("workflows:{workflow_id}")
    }
}

/// The builder assistant's chat title and brief, with the workflow open in
/// the builder (`None` for a new one). The open workflow goes in the brief
/// (refreshed every turn), not in the person's message, so the transcript
/// shows only what they typed.
pub fn chat_brief(open: Option<&Workflow>) -> anyhow::Result<(String, String)> {
    let open_now = match open {
        Some(w) => format!(
            "\n\nOpen in the builder now ({}):\n```json\n{}\n```",
            if w.id.is_empty() {
                "a new draft, not saved yet; leave id out when you save it"
            } else {
                "saved; pass its id to change it"
            },
            serde_json::to_string_pretty(w)?
        ),
        None => "\n\nNothing is open yet: this is a new workflow.".into(),
    };
    let brief = "This chat is the assistant in tab's workflow builder. The person describes what they want \
        automated and you build it as a tab workflow: a trigger and ordered steps that call connection actions. \
        Start with tab_list_actions to see what each connection can start on and do; never invent actions or \
        inputs. Save with tab_save_workflow (pass the open workflow's id to change it, leave id out for a new one); \
        the builder shows what you save right away. Then dry-run it with tab_test_workflow on a real item from \
        tab_search when one fits, and say in a sentence or two what it does and what the dry run showed. \
        Prefer deterministic steps: filters and conditions on fields over anything vague. Keep triggers narrow \
        (a connection and kinds) so workflows can't feed each other. You can't turn workflows on or allow \
        destructive steps (merge, close, move mail); the person does that in the builder, so tell them when it's needed. \
        This chat is about the one workflow open below; work on others only if the person asks.";
    let title = match open.map(|w| w.name.trim()).filter(|n| !n.is_empty()) {
        Some(n) => format!("Workflow builder: {n}"),
        None => "Workflow builder".into(),
    };
    Ok((title, format!("{brief}{open_now}")))
}

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
            let errors: Vec<_> = validate(&t, &catalog, &[], &[])
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
        let issues = validate(&bad, &catalog, &[], &[]);
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
