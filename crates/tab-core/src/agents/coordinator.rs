//! A project's coordinator: one long-running chat per project that watches
//! its queue, threads and resources, answers questions about the project
//! (the business, its customers, its numbers) and starts or steers threads.
//! It reads the project's `coordinator` resources, which other chats can't.
//! Monitoring, auto-compaction and the vault come next (docs/projects-hub.md).

use crate::model::Project;

/// Keyed sessions with this prefix and a project id are that project's
/// coordinator. The key is set by tab, never by an agent, so it's what
/// access to `coordinator` resources is checked against.
pub const COORDINATOR_KEY_PREFIX: &str = "coordinator:";

pub fn key(project_id: &str) -> String {
    format!("{COORDINATOR_KEY_PREFIX}{project_id}")
}

pub fn title(project: &Project) -> String {
    format!("Coordinator · {}", project.name)
}

/// What the coordinator is told about its job, on top of tab's usual
/// instructions and the project's notes.
pub fn brief(project: &Project) -> String {
    format!(
        "You are the coordinator for the project \"{name}\". The person runs this project through you: \
         its queue, the threads agents work on, and everything it knows (websites, documents, email, \
         repos, services). Answer questions about the project from what tab knows: tab_project for its \
         about, rules, repos, resources and notes; tab_search with project \"{name}\" for its items and \
         email; read a resource's link when you need what's in it. Name the items and resources you \
         rely on. You may read this project's coordinator-only resources; never copy their contents \
         into other threads, tasks or notes, and tell the person when a thread would need them. Keep \
         track of what's open, what's waiting on whom and what changed, and when the person asks for \
         work, start a task in this project for it rather than doing long work here. Save durable \
         facts with tab_project_note.",
        name = project.name
    )
}
