use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::SyncSender;

use chrono::Utc;
use warp_errors::report_error;
use warpui::{Entity, ModelContext, SingletonEntity};

use crate::persistence::ModelEvent;
use crate::persistence::model::Project;

#[derive(Debug)]
pub enum ProjectEvent {
    Added {
        #[expect(unused, reason = "TODO(jparker): #pod-code-mode wip")]
        path: PathBuf,
    },
    #[expect(unused, reason = "TODO(jparker): #pod-code-mode wip")]
    Removed { path: PathBuf },
    #[expect(unused, reason = "TODO(jparker): #pod-code-mode wip")]
    Updated { path: PathBuf },
}

/// Registry keys are canonicalized so trailing-slash and symlink variants
/// dedup to one row. A repo-mode remote key (`ssh://…`) names a directory on
/// *another* machine, so it is stored exactly as formatted (KTD3):
/// `dunce::canonicalize` resolves against the local filesystem and would
/// corrupt or reject it.
fn canonicalize_registry_key(path: PathBuf) -> PathBuf {
    if repo_mode::is_remote_path(&path) {
        return path;
    }
    dunce::canonicalize(&path).unwrap_or(path)
}

pub struct ProjectManagementModel {
    projects: HashMap<PathBuf, Project>,
    model_event_sender: Option<SyncSender<ModelEvent>>,
}

impl Entity for ProjectManagementModel {
    type Event = ProjectEvent;
}

impl SingletonEntity for ProjectManagementModel {}

impl ProjectManagementModel {
    /// Create a new Projects model with persisted data
    pub fn new(
        persisted_projects: Vec<Project>,
        model_event_sender: Option<SyncSender<ModelEvent>>,
        _ctx: &mut ModelContext<Self>,
    ) -> Self {
        log::debug!("Loading {} persisted projects", persisted_projects.len());

        let projects = persisted_projects
            .into_iter()
            .map(|project| (PathBuf::from(&project.path), project))
            .collect();

        Self {
            projects,
            model_event_sender,
        }
    }

    /// Add a project to the list. If it already exists, update the last_opened_ts.
    pub fn upsert_project(&mut self, path: PathBuf, ctx: &mut ModelContext<Self>) {
        let path = canonicalize_registry_key(path);
        let now = Utc::now().naive_utc();

        let project = if let Some(existing_project) = self.projects.get_mut(&path) {
            // Update existing project's last opened time
            existing_project.last_opened_ts = Some(now);
            existing_project.clone()
        } else {
            // Create new project
            let project = Project {
                path: path.to_string_lossy().to_string(),
                added_ts: now,
                last_opened_ts: Some(now),
                manual_position: None,
            };
            self.projects.insert(path.clone(), project.clone());
            project
        };
        self.save_project(project);
        ctx.emit(ProjectEvent::Added { path });
    }

    /// Remove a project from the list and persist the deletion.
    pub fn remove_project(&mut self, path: PathBuf, ctx: &mut ModelContext<Self>) {
        let path = canonicalize_registry_key(path);
        if self.projects.remove(&path).is_some() {
            self.delete_project(&path);
            ctx.emit(ProjectEvent::Removed { path });
        }
    }

    pub fn all_projects(&self) -> impl Iterator<Item = &Project> {
        self.projects.values()
    }

    /// Projects in manual order: everything carrying a manual position first,
    /// in that order, then everything without one.
    ///
    /// The backing store is a `HashMap`, so the column alone gives no ordered
    /// read — this accessor is what provides it. The unpositioned tail keeps
    /// the registry's own default (most recently used first, path as a
    /// tiebreaker) so the read is deterministic rather than hash-ordered.
    #[allow(dead_code, reason = "TODO: the repo-mode view wires this up next")]
    pub fn projects_in_manual_order(&self) -> Vec<&Project> {
        let mut ordered: Vec<&Project> = self.projects.values().collect();
        ordered.sort_by(|left, right| {
            match (left.manual_position, right.manual_position) {
                (Some(left_position), Some(right_position)) => left_position.cmp(&right_position),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => right.last_used_at().cmp(&left.last_used_at()),
            }
            .then_with(|| left.path.cmp(&right.path))
        });
        ordered
    }

    /// Hand the list over to a manual order, numbering the given paths from
    /// zero. Paths that are not in the registry are skipped, so a stale order
    /// from another window cannot resurrect a removed repository. Registry
    /// entries the caller leaves out keep whatever position they already had.
    #[allow(dead_code, reason = "TODO: the repo-mode view wires this up next")]
    pub fn set_manual_order(&mut self, ordered_paths: Vec<PathBuf>, ctx: &mut ModelContext<Self>) {
        let mut repositioned = Vec::new();
        let mut next_position = 0;
        for path in ordered_paths {
            let path = canonicalize_registry_key(path);
            let Some(project) = self.projects.get_mut(&path) else {
                continue;
            };
            project.manual_position = Some(next_position);
            next_position += 1;
            repositioned.push((path, project.clone()));
        }

        for (path, project) in repositioned {
            self.save_project(project);
            ctx.emit(ProjectEvent::Updated { path });
        }
    }

    /// Discard the manual order and give the list back to its default
    /// ordering.
    #[allow(dead_code, reason = "TODO: the repo-mode view wires this up next")]
    pub fn clear_manual_order(&mut self, ctx: &mut ModelContext<Self>) {
        let cleared: Vec<PathBuf> = self
            .projects
            .iter_mut()
            .filter_map(|(path, project)| project.manual_position.take().map(|_| path.clone()))
            .collect();

        self.clear_persisted_manual_order();
        for path in cleared {
            ctx.emit(ProjectEvent::Updated { path });
        }
    }

    /// Save a project to the database
    fn save_project(&self, project: Project) {
        if let Some(sender) = &self.model_event_sender {
            let event = ModelEvent::UpsertProject { project };
            if let Err(err) = sender.send(event) {
                report_error!(
                    anyhow::Error::new(err).context("Failed to save project to database")
                );
            }
        }
    }

    /// Clear every persisted manual position.
    ///
    /// This cannot go through [`Self::save_project`]: `Project` derives
    /// `AsChangeset` without `treat_none_as_null`, so diesel skips `None`
    /// fields on update and the upsert would leave the old positions in the
    /// database for the next launch to read back.
    fn clear_persisted_manual_order(&self) {
        if let Some(sender) = &self.model_event_sender
            && let Err(err) = sender.send(ModelEvent::ClearProjectManualOrder)
        {
            report_error!(
                anyhow::Error::new(err).context("Failed to clear project manual order in database")
            );
        }
    }

    fn delete_project(&self, path: &std::path::Path) {
        if let Some(sender) = &self.model_event_sender {
            let event = ModelEvent::DeleteProject {
                path: path.to_string_lossy().into_owned(),
            };
            if let Err(err) = sender.send(event) {
                report_error!(
                    anyhow::Error::new(err).context("Failed to delete project from database")
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "projects_tests.rs"]
mod tests;
