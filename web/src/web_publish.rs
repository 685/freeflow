use crate::{
    dialogs::{self, View, current_view},
    dom::{self, Error, Result, escape as h, project_get, project_post, string, toast},
    state,
};
use serde_json::{Value, json};

pub async fn open() -> Result {
    let project = crate::project_id()?;
    let data = project_get(&project, "web-publish").await?;
    if crate::current(&project) {
        render(&project, &data);
    }
    Ok(())
}

fn button(action: &str, label: &str, class: &str) -> String {
    format!(
        r#"<button class="button {class}" type="button" data-dialog-action="{action}">{label}</button>"#
    )
}

fn render(project: &str, data: &Value) {
    let name = state::read(|state| string(&state.project["name"]).to_owned());
    let site = &data["site"];
    let busy = data["job"].is_object();
    let enabled = data["hasToken"] == true && !busy;
    let connected = site.is_object();
    let published = site["publishedAt"].is_string();
    let pending = site["pending"] == true;
    let disabled = site["disabled"] == true;
    let caption = if busy {
        "Hosting job in progress"
    } else if pending {
        "Deployment pending"
    } else if disabled {
        "Unpublished"
    } else if published {
        "Published"
    } else {
        "Ready to publish"
    };
    let url = site["url"]
        .as_str()
        .filter(|url| url.starts_with("https://") && published && !disabled);
    let status = match url {
        Some(url) => format!(
            r#"<div class="form-field"><strong>{caption}</strong><a target="_blank" rel="noopener noreferrer" href="{url}">{url}</a></div>"#,
            url = h(url),
        ),
        None => format!("<p><strong>{caption}</strong></p>"),
    };
    let mut actions = String::new();
    if enabled {
        if !pending {
            let action = if disabled || !published {
                "Publish"
            } else {
                "Update deployment"
            };
            actions.push_str(&button("publish", action, "primary"));
        }
        if connected {
            actions.push_str(&button("refresh", "Refresh status", "outline"));
        }
        if site["uncertainUpload"] == true {
            actions.push_str(&button("clear-pending", "Clear upload status", "soft"));
        }
        if connected && (!disabled || pending) && (published || pending) {
            actions.push_str(&button("unpublish", "Unpublish", "soft"));
        }
    }
    if url.is_some() {
        actions.push_str(&button("copy", "Copy link", "soft"));
    }
    if busy {
        actions.push_str(&button("console", "Open console", "outline"));
        actions.push_str(&button("reload-dialog", "Refresh dialog", "soft"));
    }
    let readonly = if connected { " readonly" } else { "" };
    let disabled = if busy { " disabled" } else { "" };
    let access = if data["hasToken"] == true {
        "Saved. Enter a new token to replace it."
    } else {
        "Netlify personal access token"
    };
    let forget = if data["hasToken"] == true {
        r#"<label class="check-row"><input type="checkbox" name="forgetToken"> Remove the saved access token</label>"#
    } else {
        ""
    };
    let contents = format!(
        r#"<div class="form">
            <p>Build a public website on your Netlify team.</p>
            {status}
            <p id="publish-status" class="dialog-help" role="status" hidden></p>
            <div class="form-footer">{actions}</div>
            <details{}><summary>Netlify settings</summary>
                <form class="form">
                    <label class="form-field"><span>Team slug</span><input name="team" required maxlength="63" value="{}"{readonly}{disabled}></label>
                    <label class="form-field"><span>Site name</span><input name="name" maxlength="63" value="{}" placeholder="Generated if blank"{readonly}{disabled}></label>
                    <label class="form-field"><span>Access token</span><input name="token" type="password" autocomplete="new-password" maxlength="512" placeholder="{access}"{disabled}></label>
                    {forget}
                    <div class="form-row"><a href="https://app.netlify.com/user/applications" target="_blank" rel="noopener noreferrer">Manage Netlify access tokens</a></div>
                    <div class="form-error" role="alert"></div>
                    <div class="form-footer"><button class="button outline" type="submit"{disabled}>Save hosting settings</button></div>
                </form>
            </details>
            <div class="form-footer">{}</div>
        </div>"#,
        if !enabled && !busy { " open" } else { "" },
        h(string(&data["team"])),
        h(string(&data["name"])),
        button("export", "Back to export", "soft"),
    );
    dialogs::open(
        View::WebPublish {
            project: project.into(),
        },
        "Publish website",
        &name,
        contents,
    );
}

pub async fn configure(project: &str, version: u64, mut data: Value) -> Result {
    data["action"] = json!("configure");
    let settings = project_post(project, "web-publish", data).await?;
    if current_view(version) && crate::current(project) {
        render(project, &settings);
        toast("Netlify settings saved.", false);
    }
    Ok(())
}

pub async fn act(project: &str, version: u64, action: &str) -> Result {
    match action {
        "export" => return dialogs::exports("web").await,
        "reload-dialog" => return open().await,
        "console" => {
            dialogs::close_current(version);
            crate::console(true);
            return Ok(());
        }
        "copy" => {
            let data = project_get(project, "web-publish").await?;
            return dialogs::copy(string(&data["site"]["url"]), "Website link").await;
        }
        "publish" | "unpublish" | "refresh" | "clear-pending" => {}
        _ => return Ok(()),
    }
    if action == "publish" && state::read(|state| state.drafts.keys().any(|(id, _)| id == project))
    {
        return Err(Error::new(
            "Save the project's open drafts before publishing.",
        ));
    }
    if action == "unpublish"
        && !dom::win().confirm_with_message(
            "Unpublish this website? Its public link will stop serving the app.",
        )?
    {
        return Ok(());
    }
    if action == "clear-pending" && !dom::win().confirm_with_message("Clear the local upload status? Check that Netlify did not receive the previous upload before trying again.")? {
        return Ok(());
    }
    let job = project_post(project, "web-publish", json!({"action":action})).await?;
    crate::track(&job, true);
    if current_view(version) {
        for element in dom::all("#modal-body button:not([data-dialog-action=export])") {
            let _ = element.set_attribute("disabled", "");
        }
        dom::hidden("publish-status", false);
        dom::text(
            "publish-status",
            match action {
                "publish" => "Building and publishing the website…",
                "unpublish" => "Unpublishing the website…",
                _ => "Checking the deployment…",
            },
        );
    }
    let result = crate::wait_job(job).await;
    if current_view(version) && crate::current(project) {
        match &result {
            Ok(data) => render(project, data),
            Err(error) => {
                let data = project_get(project, "web-publish").await?;
                if current_view(version) {
                    render(project, &data);
                    dom::text("publish-status", &error.message);
                    dom::hidden("publish-status", false);
                }
            }
        }
    }
    if result.is_ok() {
        toast(
            match action {
                "publish" => "Website published.",
                "unpublish" => "Website unpublished.",
                "clear-pending" => "Local upload status cleared.",
                _ => "Hosting status refreshed.",
            },
            false,
        );
    }
    result.map(|_| ())
}
