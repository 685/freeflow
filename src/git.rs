use crate::{
    files::{Problem, Result},
    jobs::{Job, Jobs, Mode},
    mcp::required,
    processes::{self, ProcessCommand},
    projects::{Project, Projects},
};
use regex::Regex;
use reqwest::Url;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Output,
    sync::{Arc, LazyLock},
};

const MAX_DIFF: u64 = 512 * 1024;
static SSH_REMOTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9_][A-Za-z0-9_.-]*@[A-Za-z0-9][A-Za-z0-9.-]*:[A-Za-z0-9._/-]+$").unwrap()
});

fn binary() -> Option<PathBuf> {
    processes::executable(if cfg!(windows) { "git.exe" } else { "git" })
}

fn require() -> Result<PathBuf> {
    binary().ok_or_else(|| Problem::new(422, "Install Git and make it available on your PATH."))
}

fn remote_url(value: &str) -> Result<&str> {
    let value = value.trim();
    let valid = value.len() <= 1500
        && !value.chars().any(char::is_whitespace)
        && (SSH_REMOTE.is_match(value)
            || Url::parse(value).is_ok_and(|url| {
                url.host_str().is_some()
                    && url.path().len() > 1
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && url.password().is_none()
                    && match url.scheme() {
                        "https" => url.username().is_empty(),
                        "ssh" => true,
                        _ => false,
                    }
            }));
    if valid {
        Ok(value)
    } else {
        Err(Problem::new(
            400,
            "Enter a Git repository HTTPS or SSH URL without a password or token.",
        ))
    }
}

fn command(binary: &Path, args: impl IntoIterator<Item = impl Into<OsString>>) -> ProcessCommand {
    let mut command = ProcessCommand::new(
        binary,
        ["--no-pager", "--literal-pathspecs", "-c", "color.ui=false"],
    );
    command.args.extend(args.into_iter().map(Into::into));
    command.env.extend([
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("GIT_ALLOW_PROTOCOL".into(), "https:ssh:file".into()),
        ("GIT_MERGE_AUTOEDIT".into(), "no".into()),
    ]);
    command
}

struct Repo<'a> {
    binary: PathBuf,
    root: &'a Path,
}

impl<'a> Repo<'a> {
    fn new(binary: PathBuf, project: &'a Project) -> Self {
        Self {
            binary,
            root: &project.root,
        }
    }

    async fn read(&self, args: &[&str], limit: u64) -> Result<Output> {
        let mut command = command(&self.binary, args.iter().copied());
        command.env.push(("GIT_OPTIONAL_LOCKS".into(), "0".into()));
        command.capture(self.root, limit).await
    }

    async fn text(&self, args: &[&str], limit: u64) -> Result<String> {
        let output = self.read(args, limit).await?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(Problem::new(
                422,
                error.trim().chars().take(600).collect::<String>(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    async fn location(&self) -> Result<Location> {
        let top = self
            .read(&["rev-parse", "--show-toplevel"], 16 * 1024)
            .await?;
        if top.status.success() {
            let path = String::from_utf8_lossy(&top.stdout);
            return Ok(
                if dunce::canonicalize(path.trim_end_matches(['\r', '\n']))? == self.root {
                    Location::Here
                } else {
                    Location::Parent
                },
            );
        }
        if crate::files::exists(&self.root.join(".git"))? {
            return Err(Problem::new(
                422,
                "Git could not open this repository. Check Git’s repository ownership and configuration.",
            ));
        }
        Ok(Location::Missing)
    }

    async fn run(&self, job: &Job, args: &[&str]) -> Result<()> {
        job.exec(&command(&self.binary, args.iter().copied()), self.root)
            .await
    }

    async fn status(&self) -> Result<Status> {
        let output = self
            .read(
                &[
                    "status",
                    "--porcelain=v2",
                    "-z",
                    "--branch",
                    "--untracked-files=all",
                ],
                3 * 1024 * 1024,
            )
            .await?;
        if !output.status.success() {
            return Err(Problem::new(422, "Git could not read the working tree."));
        }
        Ok(status(&output.stdout))
    }

    async fn origin(&self) -> Result<Option<String>> {
        let output = self
            .read(&["remote", "get-url", "origin"], 16 * 1024)
            .await?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
    }

    async fn upstream(&self, branch: &str) -> Result<Option<(String, String)>> {
        let head = format!("refs/heads/{branch}");
        let config = self
            .text(
                &[
                    "for-each-ref",
                    "--format=%(upstream:remotename)%00%(upstream:remoteref)",
                    &head,
                ],
                16 * 1024,
            )
            .await?;
        Ok(config
            .lines()
            .next()
            .and_then(|line| line.split_once('\0'))
            .filter(|(remote, reference)| {
                !remote.is_empty() && reference.starts_with("refs/heads/")
            })
            .map(|(remote, reference)| (remote.to_owned(), reference.to_owned())))
    }
}

#[derive(PartialEq, Eq)]
enum Location {
    Here,
    Parent,
    Missing,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Change {
    path: String,
    old_path: Option<String>,
    code: String,
    conflict: bool,
    staged: bool,
    unstaged: bool,
}

#[derive(Default, Serialize)]
struct Status {
    branch: String,
    head: String,
    upstream: String,
    ahead: u32,
    behind: u32,
    changes: Vec<Change>,
}

fn status(bytes: &[u8]) -> Status {
    let mut result = Status::default();
    let mut records = bytes.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        let Ok(record) = std::str::from_utf8(record) else {
            if record.starts_with(b"2 ") {
                records.next();
            }
            continue;
        };
        if let Some((key, value)) = record
            .strip_prefix("# ")
            .and_then(|header| header.split_once(' '))
        {
            match key {
                "branch.head" => {
                    result.branch = if value == "(detached)" { "" } else { value }.into()
                }
                "branch.oid" => result.head = if value == "(initial)" { "" } else { value }.into(),
                "branch.upstream" => result.upstream = value.into(),
                "branch.ab" => {
                    if let Some((ahead, behind)) = value.split_once(' ') {
                        result.ahead = ahead.trim_start_matches('+').parse().unwrap_or_default();
                        result.behind = behind.trim_start_matches('-').parse().unwrap_or_default();
                    }
                }
                _ => {}
            }
            continue;
        }
        let count = match record.as_bytes().first() {
            Some(b'1') => 9,
            Some(b'2') => 10,
            Some(b'u') => 11,
            Some(b'?') => 2,
            _ => continue,
        };
        let rename = count == 10;
        let conflict = count == 11;
        let old_path = rename
            .then(|| {
                records
                    .next()
                    .and_then(|entry| std::str::from_utf8(entry).ok())
                    .map(str::to_owned)
            })
            .flatten();
        let mut fields = record.splitn(count, ' ');
        fields.next();
        let code = if count == 2 {
            "??"
        } else {
            fields.next().unwrap_or("..")
        };
        let Some(path) = fields.last() else { continue };
        let staged = !conflict && code != "??" && !code.starts_with('.');
        let unstaged = conflict || code == "??" || !code.ends_with('.');
        result.changes.push(Change {
            path: path.to_owned(),
            old_path,
            code: code.to_owned(),
            conflict,
            staged,
            unstaged,
        });
    }
    result
}

pub async fn state(project: &Project) -> Result<Value> {
    let Some(binary) = binary() else {
        return Ok(json!({"available":false, "initialized":false}));
    };
    let repo = Repo::new(binary, project);
    match repo.location().await? {
        Location::Missing => {
            return Ok(json!({"available":true,"initialized":false,"canInit":true}));
        }
        Location::Parent => {
            return Ok(json!({"available":true,"initialized":false,"canInit":false}));
        }
        Location::Here => {}
    }
    let (status, refs, origin, merge_file) = tokio::try_join!(
        repo.status(),
        repo.text(
            &[
                "for-each-ref",
                "--format=%(refname) %(symref)",
                "refs/heads",
                "refs/remotes"
            ],
            384 * 1024
        ),
        repo.origin(),
        repo.text(
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "MERGE_HEAD"
            ],
            16 * 1024
        ),
    )?;
    let mut branches = Vec::new();
    let mut remote_branches = Vec::new();
    for line in refs.lines().filter_map(|line| line.strip_suffix(' ')) {
        if let Some(branch) = line.strip_prefix("refs/heads/") {
            branches.push(branch);
        } else if let Some(branch) = line.strip_prefix("refs/remotes/") {
            remote_branches.push(branch);
        }
    }
    let merging = crate::files::exists(Path::new(merge_file.trim_end_matches(['\r', '\n'])))?;
    Ok(json!({
        "available":true,"initialized":true,"status":status,
        "branches":branches,"remoteBranches":remote_branches,"merging":merging,
        "hasOrigin":origin.is_some(),"origin":origin.as_deref().and_then(|url| remote_url(url).ok()),
    }))
}

async fn own_repo(project: &Project) -> Result<Repo<'_>> {
    let repo = Repo::new(require()?, project);
    if repo.location().await? != Location::Here {
        return Err(Problem::new(
            422,
            "Open a Git repository at the Flutter project root.",
        ));
    }
    Ok(repo)
}

fn change<'a>(status: &'a Status, path: &str) -> Result<&'a Change> {
    status
        .changes
        .iter()
        .find(|entry| entry.path == path)
        .ok_or_else(|| {
            Problem::new(
                404,
                "That path no longer has a pending change. Refresh the changes list.",
            )
        })
}

fn change_paths(entry: &Change) -> Vec<&str> {
    let mut paths = vec![entry.path.as_str()];
    paths.extend(entry.old_path.as_deref());
    paths
}

pub async fn diff(projects: &Projects, project: &Project, path: &str) -> Result<Value> {
    let repo = own_repo(project).await?;
    let status = repo.status().await?;
    let entry = change(&status, path)?;
    if entry.code == "??" {
        let source = projects
            .file(project, path)
            .ok()
            .filter(|file| file.content.len() <= MAX_DIFF as usize);
        let new = source.map(|file| {
            let lines: String = file
                .content
                .lines()
                .map(|line| format!("+{line}\n"))
                .collect();
            format!("+++ {path}\n{lines}")
        });
        return Ok(
            json!({"staged":"","working":new.unwrap_or_else(|| "New file. Text preview unavailable.".into())}),
        );
    }
    let mut staged = vec!["diff", "--cached", "--no-ext-diff", "--no-textconv", "--"];
    staged.extend(change_paths(entry));
    let mut working = vec!["diff", "--no-ext-diff", "--no-textconv", "--"];
    working.extend(change_paths(entry));
    let (staged, working) =
        tokio::try_join!(repo.text(&staged, MAX_DIFF), repo.text(&working, MAX_DIFF))?;
    let mut result = json!({"staged":staged,"working":working});
    if entry.conflict {
        let ours = format!(":2:{path}");
        let theirs = format!(":3:{path}");
        let ours = ["show", &ours];
        let theirs = ["show", &theirs];
        let (local, incoming) =
            tokio::join!(repo.text(&ours, MAX_DIFF), repo.text(&theirs, MAX_DIFF));
        result["local"] = json!(local.ok());
        result["incoming"] = json!(incoming.ok());
    }
    Ok(result)
}

pub fn clone_project(jobs: &Arc<Jobs>, url: &str) -> Result<Value> {
    let binary = require()?;
    let url = remote_url(url)?.to_owned();
    jobs.enqueue(
        "git-clone",
        None,
        Mode::Global,
        move |jobs, job| async move {
            let root = jobs.projects.home.join("projects");
            std::fs::create_dir_all(&root)?;
            let folder = tempfile::Builder::new()
                .prefix("clone-")
                .tempdir_in(&root)?;
            let args: Vec<OsString> = ["clone", "--no-recurse-submodules", "--", &url]
                .into_iter()
                .map(Into::into)
                .collect();
            let command = command(
                &binary,
                args.into_iter()
                    .chain([folder.path().as_os_str().to_owned()]),
            );
            job.exec(&command, &root).await?;
            let project = jobs
                .projects
                .import_folder(&folder.path().to_string_lossy())?;
            let _ = folder.keep();
            Ok(json!({"project":project}))
        },
    )
}

pub fn operation(jobs: &Arc<Jobs>, project: Project, input: &Value) -> Result<Value> {
    let action = required(input, "action")?;
    let mode = match action {
        "switch" | "track" | "pull" | "abort" | "resolve" | "discard" => Mode::PausePreview,
        "init" | "remote" | "disconnect" | "branch" | "fetch" | "stage" | "unstage" | "commit"
        | "push" => Mode::Exclusive,
        _ => return Err(Problem::new(400, "Choose a Git action.")),
    };
    let binary = require()?;
    let input = input.clone();
    let target = project.clone();
    jobs.enqueue(
        format!("git-{action}"),
        Some(project),
        mode,
        move |_, job| async move {
            let repo = Repo::new(binary, &target);
            let action = required(&input, "action")?;
            let location = repo.location().await?;
            if action == "init" {
                if location != Location::Missing {
                    return Err(Problem::new(
                        409,
                        "This project is already inside a Git repository.",
                    ));
                }
                repo.run(&job, &["init", "--initial-branch=main"]).await?;
            } else {
                if location != Location::Here {
                    return Err(Problem::new(
                        422,
                        "Open a Git repository at the Flutter project root.",
                    ));
                }
                run(&repo, &job, action, &input).await?;
            }
            Ok(json!({}))
        },
    )
}

async fn run(repo: &Repo<'_>, job: &Job, action: &str, input: &Value) -> Result<()> {
    match action {
        "remote" => {
            let url = remote_url(required(input, "url")?)?;
            let operation = if repo.origin().await?.is_some() {
                "set-url"
            } else {
                "add"
            };
            repo.run(job, &["remote", operation, "origin", url]).await
        }
        "disconnect" => repo.run(job, &["remote", "remove", "origin"]).await,
        "fetch" => {
            repo.run(
                job,
                &["fetch", "--prune", "--no-recurse-submodules", "origin"],
            )
            .await
        }
        "abort" => repo.run(job, &["merge", "--abort"]).await,
        "branch" | "switch" | "track" => {
            let branch = required(input, "branch")?;
            let prefix = if action == "track" {
                "refs/remotes"
            } else {
                "refs/heads"
            };
            if branch.is_empty() || branch.starts_with('-') || branch.len() > 240 {
                return Err(Problem::new(400, "Enter a valid Git branch name."));
            }
            repo.text(&["check-ref-format", &format!("{prefix}/{branch}")], 2048)
                .await
                .map_err(|_| Problem::new(400, "Enter a valid Git branch name."))?;
            match action {
                "branch" => repo.run(job, &["switch", "-c", branch]).await,
                "track" => repo.run(job, &["switch", "--track", "--", branch]).await,
                _ => repo.run(job, &["switch", "--no-guess", "--", branch]).await,
            }
        }
        "stage" | "unstage" | "resolve" | "discard" => {
            let status = repo.status().await?;
            let entry = change(&status, required(input, "path")?)?;
            let mut args = match action {
                "stage" => vec!["add", "--all", "--"],
                "unstage" if status.head.is_empty() => vec!["rm", "-f", "--cached", "--"],
                "unstage" => vec!["restore", "--staged", "--"],
                "discard" if entry.unstaged && entry.code != "??" && !entry.conflict => {
                    vec!["restore", "--worktree", "--"]
                }
                "resolve" if entry.conflict => {
                    let (side, index) = match input["side"].as_str() {
                        Some("local") => ("--ours", "2"),
                        Some("incoming") => ("--theirs", "3"),
                        _ => return Err(Problem::new(400, "Choose a conflict version.")),
                    };
                    let reference = format!(":{index}:{}", entry.path);
                    if !repo
                        .read(&["cat-file", "-e", &reference], 2048)
                        .await?
                        .status
                        .success()
                    {
                        return repo.run(job, &["rm", "--", &entry.path]).await;
                    }
                    repo.run(job, &["checkout", side, "--", &entry.path])
                        .await?;
                    vec!["add", "--"]
                }
                _ => {
                    return Err(Problem::new(
                        409,
                        "This change cannot use the selected action.",
                    ));
                }
            };
            args.extend(change_paths(entry));
            repo.run(job, &args).await
        }
        "commit" => {
            let message = required(input, "message")?.trim();
            if message.is_empty() {
                repo.run(job, &["commit", "--no-edit"]).await
            } else if message.len() <= 4000 && !message.contains('\0') {
                repo.run(job, &["commit", "-m", message]).await
            } else {
                Err(Problem::new(400, "Enter a shorter commit message."))
            }
        }
        "push" | "pull" => {
            let status = repo.status().await?;
            if status.branch.is_empty() {
                return Err(Problem::new(
                    409,
                    "Switch to a branch before syncing changes.",
                ));
            }
            let head = format!("refs/heads/{}", status.branch);
            let upstream = repo.upstream(&status.branch).await?;
            let (remote, branch) = upstream
                .as_ref()
                .map(|(remote, branch)| (remote.as_str(), branch.as_str()))
                .unwrap_or(("origin", &head));
            if action == "pull" {
                repo.run(
                    job,
                    &[
                        "-c",
                        "merge.autoStash=false",
                        "pull",
                        "--no-rebase",
                        "--no-edit",
                        "--no-recurse-submodules",
                        "--",
                        remote,
                        branch,
                    ],
                )
                .await
            } else {
                let refspec = format!("{head}:{branch}");
                let mut args = vec!["push", "--recurse-submodules=no"];
                if upstream.is_none() {
                    args.push("--set-upstream");
                }
                args.extend(["--", remote, &refspec]);
                repo.run(job, &args).await
            }
        }
        _ => unreachable!("validated Git action"),
    }
}
