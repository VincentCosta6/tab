//! Projects: their notes, repos and threads, and each one's coordinator, a
//! long-running chat that knows the whole project.

use chrono::Utc;
use tab_core::agents::coordinator;
use tab_core::model::{Project, ProjectNote, ProjectRepo, QueueItem, Session};

use crate::chat::Chat;
use crate::{Backend, err};

type Result<T> = std::result::Result<T, String>;

impl Backend {
    /// Tells every view a project's profile, notes or repos changed.
    fn project_changed(&self, project_id: &str) {
        let _ = self.events.send(tab_core::EngineEvent::ProjectChanged {
            project_id: project_id.into(),
        });
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        self.store.projects().map_err(err)
    }

    /// Makes a project (no `id`) or renames and recolors one.
    pub fn project_save(
        &self,
        id: Option<&str>,
        name: &str,
        color: &str,
        icon: &str,
    ) -> Result<Project> {
        let p = self
            .store
            .project_save(id, name, color, icon, Utc::now())
            .map_err(err)?;
        self.project_changed(&p.id);
        Ok(p)
    }

    pub fn project_delete(&self, id: &str) -> Result<()> {
        self.store.project_delete(id).map_err(err)
    }

    /// The person writing what a project is about, or its rules.
    pub fn project_set_profile(
        &self,
        id: &str,
        about: Option<&str>,
        rules: Option<&str>,
    ) -> Result<Option<Project>> {
        let p = self
            .store
            .project_set_profile(id, about, rules, Utc::now())
            .map_err(err)?;
        self.project_changed(id);
        Ok(p)
    }

    /// The project's recent threads, open ones first.
    pub fn project_items(&self, project_id: &str) -> Result<Vec<QueueItem>> {
        self.store.project_items(project_id, None, 50).map_err(err)
    }

    pub fn project_notes(&self, project_id: &str) -> Result<Vec<ProjectNote>> {
        self.store.project_notes(project_id).map_err(err)
    }

    /// The person adding a note (no `id`) or rewriting one.
    pub fn project_note_save(
        &self,
        project_id: &str,
        id: Option<&str>,
        topic: &str,
        text: &str,
    ) -> Result<()> {
        match id {
            Some(id) => {
                self.store.note_edit(id, text, Utc::now()).map_err(err)?;
            }
            None => {
                self.store
                    .note_save(project_id, topic, text, "user", None, &[], Utc::now())
                    .map_err(err)?;
            }
        }
        self.project_changed(project_id);
        Ok(())
    }

    pub fn project_note_delete(&self, id: &str) -> Result<()> {
        self.store.note_delete(id).map_err(err)?;
        Ok(())
    }

    pub fn project_repos(&self, project_id: &str) -> Result<Vec<ProjectRepo>> {
        self.store.project_repos(project_id).map_err(err)
    }

    pub fn project_repo_save(
        &self,
        project_id: &str,
        repo: &str,
        path: Option<&str>,
        note: Option<&str>,
    ) -> Result<ProjectRepo> {
        let r = self
            .store
            .repo_save(project_id, repo, path, note, "user", Utc::now())
            .map_err(err)?;
        self.project_changed(project_id);
        Ok(r)
    }

    pub fn project_repo_delete(&self, project_id: &str, id: &str) -> Result<()> {
        self.store.repo_delete(project_id, id).map_err(err)?;
        self.project_changed(project_id);
        Ok(())
    }

    /// A project's coordinator chat.
    pub fn coordinator_open(&self, project_id: &str) -> Result<Chat> {
        let session = self
            .store
            .session_for_key(&coordinator::key(project_id))
            .map_err(err)?;
        self.chat_of(session)
    }

    /// A message to a project's coordinator. It steers a turn in flight, or
    /// starts the next one.
    pub fn coordinator_send(
        &self,
        project_id: &str,
        text: &str,
        attachments: &[String],
    ) -> Result<Session> {
        let mut text = text.trim().to_string();
        if text.is_empty() && attachments.is_empty() {
            return Err("message is empty".into());
        }
        if text.is_empty() {
            text = "Have a look at this.".into();
        }
        if self.agents.config().kind.is_none() {
            return Err("Set up an agent first (Settings → Agent).".into());
        }
        let project = self
            .store
            .project_find(project_id)
            .map_err(err)?
            .ok_or("That project was deleted.")?;
        let cwd = self.agents.workdir();
        let session = self
            .agents
            .session_for_key(
                &coordinator::key(&project.id),
                &coordinator::title(&project),
                &cwd,
                &coordinator::brief(&project),
            )
            .map_err(err)?;
        if !self
            .agents
            .steer_with(&session.id, &text, attachments)
            .map_err(err)?
        {
            self.agents
                .send_with(&session.id, &text, attachments)
                .map_err(err)?;
        }
        Ok(session)
    }
}
