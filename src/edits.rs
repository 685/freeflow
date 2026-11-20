use crate::files::{self, Problem, Result};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};

const MAX_STEPS: usize = 50;
const MAX_BYTES: usize = 32 * 1024 * 1024;

pub struct ChangedFile {
    pub path: String,
    pub before: Option<Vec<u8>>,
    pub after: Option<Vec<u8>>,
    pub mode: u32,
}

pub struct Change {
    sequence: u64,
    pub files: Vec<ChangedFile>,
}

impl Change {
    fn summary(&self) -> Value {
        json!({"files": self.files.iter().map(|file| &file.path).collect::<Vec<_>>()})
    }

    fn bytes(&self) -> usize {
        self.files
            .iter()
            .map(|file| {
                file.before.as_ref().map_or(0, Vec::len) + file.after.as_ref().map_or(0, Vec::len)
            })
            .sum()
    }
}

#[derive(Default)]
struct Timeline {
    revision: String,
    past: VecDeque<Change>,
    future: Vec<Change>,
}

impl Timeline {
    fn changed(&mut self) {
        self.revision = files::short_id(16);
    }

    fn status(&self) -> Value {
        json!({"revision":self.revision, "undo":self.past.back().map(Change::summary),
            "redo":self.future.last().map(Change::summary), "undoCount":self.past.len(),
            "redoCount":self.future.len()})
    }
}

#[derive(Default)]
pub struct History {
    projects: HashMap<String, Timeline>,
    sequence: u64,
}

impl History {
    pub fn status(&self, project: &str) -> Value {
        self.projects
            .get(project)
            .map_or_else(|| Timeline::default().status(), Timeline::status)
    }

    pub fn forget(&mut self, project: &str) {
        self.projects.remove(project);
    }

    pub fn record(&mut self, project: &str, files: Vec<ChangedFile>) {
        if files.is_empty() {
            return;
        }
        self.sequence += 1;
        let timeline = self.projects.entry(project.into()).or_default();
        timeline.future.clear();
        let change = Change {
            sequence: self.sequence,
            files,
        };
        if change.bytes() > MAX_BYTES {
            timeline.past.clear();
            timeline.changed();
            return;
        }
        timeline.past.push_back(change);
        timeline.changed();
        while timeline.past.len() > MAX_STEPS {
            timeline.past.pop_front();
        }
        self.trim(MAX_BYTES);
    }

    fn trim(&mut self, limit: usize) {
        while self
            .projects
            .values()
            .map(|timeline| {
                timeline
                    .past
                    .iter()
                    .chain(&timeline.future)
                    .map(Change::bytes)
                    .sum::<usize>()
            })
            .sum::<usize>()
            > limit
        {
            let Some((project, past)) = self
                .projects
                .iter()
                .flat_map(|(id, timeline)| {
                    timeline
                        .past
                        .front()
                        .map(|change| (id, true, change.sequence))
                        .into_iter()
                        .chain(
                            timeline
                                .future
                                .last()
                                .map(|change| (id, false, change.sequence)),
                        )
                })
                .min_by_key(|(_, _, sequence)| *sequence)
                .map(|(id, past, _)| (id.clone(), past))
            else {
                break;
            };
            let timeline = self.projects.get_mut(&project).unwrap();
            if past {
                timeline.past.pop_front();
            } else {
                timeline.future.clear();
            }
            timeline.changed();
        }
    }

    pub fn pending(&self, project: &str, action: &str, revision: &str) -> Result<&Change> {
        let timeline = self
            .projects
            .get(project)
            .ok_or_else(|| Problem::new(409, "There are no saved edits to undo or redo."))?;
        if timeline.revision != revision {
            return Err(Problem::new(
                409,
                "The edit history changed. Refresh and try again.",
            ));
        }
        match action {
            "undo" => timeline.past.back(),
            "redo" => timeline.future.last(),
            _ => return Err(Problem::new(400, "Choose Undo or Redo.")),
        }
        .ok_or_else(|| Problem::new(409, "There are no saved edits to undo or redo."))
    }

    pub fn complete(&mut self, project: &str, action: &str) {
        let timeline = self
            .projects
            .get_mut(project)
            .expect("validated edit history");
        if action == "undo" {
            timeline
                .future
                .push(timeline.past.pop_back().expect("validated undo"));
        } else {
            timeline
                .past
                .push_back(timeline.future.pop().expect("validated redo"));
        }
        timeline.changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_keeps_valid_undo_and_redo_chains_with_a_global_memory_limit() {
        let mut history = History::default();
        let record = || {
            vec![ChangedFile {
                path: "main.dart".into(),
                before: Some(b"old".to_vec()),
                after: Some(b"new".to_vec()),
                mode: 0o644,
            }]
        };
        for _ in 0..MAX_STEPS + 2 {
            history.record("one", record());
        }
        assert_eq!(history.status("one")["undoCount"], MAX_STEPS);
        history.complete("one", "undo");
        history.complete("one", "undo");
        history.record("two", record());
        history.trim(12);
        assert_eq!(history.status("one")["undoCount"], 0);
        assert_eq!(history.status("one")["redoCount"], 0);
        assert_eq!(history.status("two")["undoCount"], 1);
        history.record("one", record());
        history.complete("one", "undo");
        history.record("one", record());
        assert_eq!(history.status("one")["redoCount"], 0);
    }
}
