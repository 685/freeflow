use crate::{
    files::{self, Problem, Result},
    projects::Project,
};
use serde_json::{Value, json};
use std::{fs::File, io::Read};

const NAMES: &[&str] = &["AGENTS.md", "CLAUDE.md", "ARCHITECTURE.md", ".cursorrules"];
const MAX_FILES: usize = 32;
const MAX_FILE: usize = 16 * 1024;
const MAX_TOTAL: usize = 48 * 1024;

pub fn load(project: &Project, path: Option<&str>) -> Result<Value> {
    let mut scopes = vec![String::new()];
    if let Some(path) = path.filter(|path| !path.is_empty()) {
        let parts = files::relative_parts(path)?;
        if path.len() > 1024 || parts.len() > 32 {
            return Err(Problem::new(
                400,
                "Choose a shorter project-relative file path.",
            ));
        }
        for end in 1..parts.len() {
            scopes.push(parts[..end].join("/"));
        }
    }
    let mut candidates = vec![];
    let mut skipped = vec![];
    for scope in scopes {
        for name in NAMES {
            let relative = if scope.is_empty() {
                (*name).to_owned()
            } else {
                format!("{scope}/{name}")
            };
            match files::project_file(&project.root, &relative, false) {
                Ok(target) => candidates.push((relative, scope.clone(), target)),
                Err(error) if error.status.as_u16() == 404 => {}
                Err(error) => skipped.push(json!({"path":relative,"reason":error.message})),
            }
        }
    }
    let omitted = candidates.len().saturating_sub(MAX_FILES);
    if omitted > 0 {
        let roots = candidates
            .iter()
            .take_while(|(_, scope, _)| scope.is_empty())
            .count();
        candidates.drain(roots..roots + omitted);
    }
    let budget = (MAX_TOTAL / candidates.len().max(1)).min(MAX_FILE);
    let mut entries = vec![];
    for (relative, scope, target) in candidates {
        let read = (|| -> Result<_> {
            let mut bytes = vec![];
            File::open(target)?
                .take((budget + 1) as u64)
                .read_to_end(&mut bytes)?;
            let truncated = bytes.len() > budget;
            if bytes.contains(&0) {
                return Err(Problem::new(415, "Rule files must contain UTF-8 text."));
            }
            let end = bytes.len().min(budget);
            let text = match std::str::from_utf8(&bytes[..end]) {
                Ok(text) => text,
                Err(error) if truncated && error.error_len().is_none() => {
                    std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap()
                }
                _ => return Err(Problem::new(415, "Rule files must contain UTF-8 text.")),
            };
            Ok(
                json!({"path":relative,"scope":if scope.is_empty() { "." } else { &scope },
                "content":text.trim_start_matches('\u{feff}'),"truncated":truncated,
                "revision":(!truncated).then(|| files::revision(&bytes))}),
            )
        })();
        match read {
            Ok(entry) => entries.push(entry),
            Err(error) => skipped.push(json!({"path":relative,"reason":error.message})),
        }
    }
    let truncated = omitted > 0 || entries.iter().any(|entry| entry["truncated"] == true);
    Ok(json!({"files":entries,"skipped":skipped,"omittedFiles":omitted,"truncated":truncated}))
}
