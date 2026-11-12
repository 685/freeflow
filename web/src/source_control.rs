use crate::{
    dialogs::{self, View, close_current, current_view},
    dom::{
        self, Error, Result, api, escape as h, hidden, html, id, items, post, project_get,
        project_post, string, toast,
    },
    state,
};
use serde_json::{Value, json};
use web_sys::Element;

pub async fn open() -> Result {
    let Ok(project) = crate::project_id() else {
        clone_dialog();
        return Ok(());
    };
    let git = project_get(&project, "git").await?;
    if crate::current(&project) {
        render(&project, &git, "");
    }
    Ok(())
}

pub fn clone_dialog() {
    dialogs::open(
        View::GitClone,
        "Clone Flutter repository",
        "Git uses the credentials set up on this computer.",
        r#"<form class="form">
            <label class="form-field"><span>Repository URL</span><input name="url" required autofocus placeholder="https://github.com/owner/app.git"></label>
            <div class="form-error" role="alert"></div>
            <div class="form-footer"><button class="button primary" type="submit">Clone project</button></div>
        </form>"#.into(),
    );
}

pub async fn clone_project(version: u64, input: Value) -> Result {
    let job = post("/projects/clone", input).await?;
    crate::track(&job, true);
    let result = crate::wait_job(job).await?;
    crate::refresh_state().await?;
    if current_view(version) {
        close_current(version);
        crate::activate(result["project"].clone()).await?;
    }
    toast("Repository cloned", false);
    Ok(())
}

fn button(action: &str, label: &str, path: &str, disabled: bool) -> String {
    format!(
        r#"<button class="button soft small" type="button" data-dialog-action="{action}" data-git-path="{}"{}>{label}</button>"#,
        h(path),
        if disabled { " disabled" } else { "" },
    )
}

fn form(action: &str, label: &str, fields: &str, disabled: bool) -> String {
    format!(
        r#"<form class="git-actions"><input type="hidden" name="action" value="{action}">{fields}<button class="button outline small" type="submit"{}>{label}</button></form>"#,
        if disabled { " disabled" } else { "" }
    )
}

fn render(project: &str, git: &Value, message: &str) {
    let name = state::read(|state| string(&state.project["name"]).to_owned());
    let content = if git["available"] != true {
        "<p>Install Git to manage this project’s source.</p>".into()
    } else if git["initialized"] != true {
        if git["canInit"] == true {
            format!(
                "<p>Start tracking changes to this project with Git.</p>{}",
                button("init", "Initialize Git", "", false)
            )
        } else {
            "<p>This Flutter project is inside a Git repository rooted in another folder.</p>"
                .into()
        }
    } else {
        repository(git)
    };
    dialogs::open(
        View::Git {
            project: project.to_owned(),
        },
        "Source control",
        &name,
        format!(
            r#"<div class="git-panel">
                <p id="git-feedback" class="git-feedback" role="status">{}</p>
                <fieldset id="git-controls" class="git-controls"><div>{content}</div></fieldset>
                <div id="git-diff" class="git-diff" hidden></div>
                <div class="git-actions">{}{}</div>
            </div>"#,
            h(message),
            button("refresh", "Refresh changes", "", false),
            button("clone", "Clone repository", "", false)
        ),
    );
}

fn repository(git: &Value) -> String {
    let status = &git["status"];
    let branch = status["branch"]
        .as_str()
        .filter(|branch| !branch.is_empty());
    let upstream = string(&status["upstream"]);
    let remote = git["hasOrigin"] == true;
    let syncing = branch.is_some() && (remote || !upstream.is_empty());
    let changes = items(&status["changes"]);
    let conflicts = changes.iter().any(|entry| entry["conflict"] == true);
    let merging = git["merging"] == true;
    let staged = changes.iter().any(|entry| entry["staged"] == true);
    let remote_url = string(&git["origin"]);
    let mut list = String::new();
    for change in changes {
        let path = string(&change["path"]);
        let conflict = change["conflict"] == true;
        let path_text = h(path);
        let code = h(string(&change["code"]));
        let diff = button("diff", "Diff", path, false);
        let stage = if change["unstaged"] == true {
            button(
                "stage",
                if conflict { "Mark resolved" } else { "Stage" },
                path,
                false,
            )
        } else {
            String::new()
        };
        let unstage = if change["staged"] == true && !merging {
            button("unstage", "Unstage", path, false)
        } else {
            String::new()
        };
        list.push_str(&format!(
            r#"<div class="dialog-item"><div class="dialog-item-text"><strong class="ellipsis{}" title="{path_text}">{path_text}</strong><small>{}</small></div><code>{code}</code><div class="git-actions">{diff}{stage}{unstage}</div></div>"#,
            if conflict { " git-conflict" } else { "" }, h(string(&change["oldPath"])),
        ));
    }
    if list.is_empty() {
        list = "<p>Working tree is clean.</p>".into();
    }
    let mut options = String::from(r#"<option value="">Select a branch</option>"#);
    for item in items(&git["branches"]).iter().map(string) {
        if Some(item) != branch {
            options.push_str(&branch_option("local", item));
        }
    }
    for item in items(&git["remoteBranches"]).iter().map(string) {
        options.push_str(&branch_option("remote", item));
    }
    let sync = format!(
        "{}{}{}{}",
        button("fetch", "Fetch", "", !remote),
        button(
            "pull",
            "Pull",
            "",
            !syncing || merging || status["head"] == ""
        ),
        button(
            "push",
            "Push",
            "",
            !syncing || merging || status["head"] == ""
        ),
        if merging {
            button("abort", "Abort merge", "", false)
        } else {
            String::new()
        }
    );
    let commit = form(
        "commit",
        if merging {
            "Commit merge"
        } else {
            "Commit staged changes"
        },
        &format!(
            r#"<input name="message" aria-label="Commit message" placeholder="Commit message" maxlength="2000" {}>"#,
            if merging { "" } else { "required" }
        ),
        conflicts || (!merging && !staged),
    );
    format!(
        r#"<section class="git-section"><div><strong>{}</strong> <small class="dialog-help">{}</small></div>
            <div class="git-actions">{sync}</div>
            <details><summary>Branches</summary><div class="git-section">{}{}</div></details>
            <details><summary>Remote repository</summary><div class="git-section">{}{}</div></details></section>
            <section class="git-section"><strong>Changes</strong><div class="dialog-list git-changes">{list}</div>{commit}</section>"#,
        h(branch.unwrap_or("Detached HEAD")),
        if upstream.is_empty() {
            String::new()
        } else {
            format!(
                "{} · {} ahead · {} behind",
                h(upstream),
                status["ahead"],
                status["behind"]
            )
        },
        form(
            "switch",
            "Switch",
            &format!(r#"<select name="branch" aria-label="Branch" required>{options}</select>"#),
            merging
        ),
        form(
            "branch",
            "Create and switch",
            r#"<input name="branch" aria-label="New branch name" placeholder="New branch name" required>"#,
            merging
        ),
        form(
            "remote",
            if remote {
                "Update origin"
            } else {
                "Connect origin"
            },
            &format!(
                r#"<input name="url" aria-label="Remote URL" value="{}" placeholder="Git HTTPS or SSH URL" required>"#,
                h(remote_url)
            ),
            false
        ),
        if remote {
            button("disconnect", "Disconnect origin", "", false)
        } else {
            String::new()
        }
    )
}

fn branch_option(kind: &str, name: &str) -> String {
    let name = h(name);
    format!(r#"<option value="{kind}:{name}">{name}</option>"#)
}

pub async fn submit(project: &str, version: u64, mut input: Value) -> Result {
    if input["action"] == "switch" {
        let (kind, branch) = string(&input["branch"])
            .split_once(':')
            .ok_or_else(|| Error::new("Choose a branch."))?;
        let branch = branch.to_owned();
        input["action"] = json!(if kind == "remote" { "track" } else { "switch" });
        input["branch"] = json!(branch);
    }
    operate(project, version, input).await
}

pub async fn act(project: &str, version: u64, action: &str, element: &Element) -> Result {
    let path = element.get_attribute("data-git-path").unwrap_or_default();
    match action {
        "clone" => clone_dialog(),
        "refresh" => {
            let git = project_get(project, "git").await?;
            if current_view(version) {
                render(project, &git, "");
            }
        }
        "diff" => {
            let diff = project_get(project, &format!("git-diff{}", dom::path_query(&path))).await?;
            if current_view(version) {
                render_diff(&path, &diff);
            }
        }
        "edit-conflict" => {
            close_current(version);
            crate::select(path, crate::code_mode()).await?;
        }
        "local" | "incoming" => {
            operate(
                project,
                version,
                json!({"action":"resolve","path":path,"side":action}),
            )
            .await?;
        }
        "disconnect" | "discard" | "abort" => {
            let prompt = match action {
                "disconnect" => "Disconnect the origin remote from this project?",
                "abort" => "Abort this merge and discard its conflict resolutions?",
                _ => "Discard this file’s unstaged edits?",
            };
            if dom::win().confirm_with_message(prompt)? {
                operate(project, version, json!({"action":action,"path":path})).await?;
            }
        }
        "init" | "fetch" | "pull" | "push" | "stage" | "unstage" => {
            operate(project, version, json!({"action":action,"path":path})).await?;
        }
        _ => {}
    }
    Ok(())
}

fn render_diff(path: &str, diff: &Value) {
    let mut body = format!("<h3>{}</h3>", h(path));
    if diff.get("local").is_some() {
        let local = diff["local"].as_str().unwrap_or("No file in this version.");
        let incoming = diff["incoming"]
            .as_str()
            .unwrap_or("No file in this version.");
        body.push_str(&format!(
            r#"<div class="git-conflicts"><div><h3>Local version</h3><pre class="code-card">{}</pre></div><div><h3>Incoming version</h3><pre class="code-card">{}</pre></div></div>
            <div class="git-actions">{}{}{}</div>"#,
            h(local), h(incoming),
            button("local", "Accept local", path, false), button("incoming", "Accept incoming", path, false),
            button("edit-conflict", "Edit conflict", path, false)
        ));
    }
    for (key, title) in [("working", "Working tree"), ("staged", "Staged")] {
        if let Some(text) = diff[key].as_str().filter(|text| !text.is_empty()) {
            body.push_str(&format!(
                r#"<div><h3>{title}</h3><pre class="code-card">{}</pre></div>"#,
                h(text)
            ));
        }
    }
    if diff["local"].is_null()
        && diff["working"] == ""
        && diff["staged"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    {
        body.push_str(&button("unstage", "Unstage file", path, false));
    }
    if diff.get("local").is_none() && string(&diff["working"]).starts_with("diff --git ") {
        body.push_str(&button("discard", "Discard unstaged edits", path, false));
    }
    html("git-diff", body);
    hidden("git-diff", false);
    id("git-diff").scroll_into_view();
}

async fn operate(project: &str, version: u64, input: Value) -> Result {
    let action = string(&input["action"]);
    let updates_source = matches!(
        action,
        "switch" | "track" | "pull" | "abort" | "resolve" | "discard"
    );
    if updates_source && state::read(|state| state.drafts.keys().any(|(id, _)| id == project)) {
        return Err(Error::new(
            "Save the project’s code drafts before changing the working tree.",
        ));
    }
    if !current_view(version) || id("git-controls").has_attribute("disabled") {
        return Ok(());
    }
    dom::attr(&id("git-controls"), "disabled", "");
    dom::text("git-feedback", "Running Git…");
    let outcome = async {
        let job = project_post(project, "git", input.clone()).await?;
        crate::track(&job, false);
        let job_id = string(&job["id"]).to_owned();
        match crate::wait_job(job).await {
            Ok(_) => Ok(()),
            Err(error) => {
                let logs = api(&format!("/jobs/{job_id}")).await.ok().and_then(|job| {
                    items(&job["log"]).iter().rev().find_map(|line| {
                        let message = line["message"].as_str()?;
                        (message != error.message && !message.is_empty())
                            .then(|| message.to_owned())
                    })
                });
                Err(Error::new(logs.unwrap_or(error.message)))
            }
        }
    }
    .await;
    if updates_source && crate::current(project) {
        let _ = crate::refresh_tree(project).await;
        let file = state::read(|state| state.current_file().to_owned());
        let _ = crate::load_file(&file, true).await;
    }
    if !current_view(version) {
        return outcome;
    }
    let _ = id("git-controls").remove_attribute("disabled");
    let git = project_get(project, "git").await?;
    if current_view(version) {
        let text = outcome
            .err()
            .map_or_else(|| "Git changes updated.".into(), |error| error.message);
        render(project, &git, &text);
    }
    Ok(())
}
