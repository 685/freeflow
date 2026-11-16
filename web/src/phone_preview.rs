use crate::{
    dialogs::{self, View, showing_phone_preview},
    dom::{self, Result, escape as h, html, id, project_get, project_post, string},
    state::{self, State},
};
use serde_json::{Value, json};

const ROUTE: &str = "phone-preview";

fn active(job: &Value) -> bool {
    matches!(job["status"].as_str(), Some("queued" | "running" | "ready"))
}

pub fn badge(state: &State) {
    let button = id(ROUTE);
    let sharing = active(&state.phone_preview["job"]);
    dom::toggle(&button, "running", sharing);
    dom::attr(
        &button,
        "title",
        if sharing {
            "Manage phone preview (sharing)"
        } else {
            "Preview on phone"
        },
    );
}

fn button(action: &str, label: &str, class: &str, disabled: bool) -> String {
    format!(
        r#"<button type="button" class="button {class}" data-dialog-action="{action}"{}>{label}</button>"#,
        if disabled { " disabled" } else { "" }
    )
}

fn render(data: &Value) {
    let job = &data["job"];
    let details = &job["result"];
    let reloading =
        details["reloading"] == true || matches!(job["status"].as_str(), Some("queued"));
    let error = job["error"]
        .as_str()
        .or(details["compileError"].as_str())
        .map(|error| format!(r#"<p class="form-error" role="alert">{}</p>"#, h(error)))
        .unwrap_or_default();
    let controls = if active(job) {
        let share = details["url"]
            .as_str()
            .zip(details["qr"].as_str())
            .filter(|(url, qr)| {
                url.starts_with("http://") && qr.starts_with("data:image/svg+xml;base64,")
            });
        let share = if let Some((url, qr)) = share {
            format!(
                r#"<div class="phone-share">
                    <img src="{}" width="176" height="176" alt="QR code for the phone preview">
                    <div class="form">
                      <p>Scan the code with a phone on the same network. Saved changes rebuild the preview.</p>
                      <a href="{}" target="_blank" rel="noopener noreferrer" class="phone-link">{}</a>
                      <div>{}</div>
                    </div>
                </div>"#,
                h(qr),
                h(url),
                h(url),
                button("copy", "Copy link", "outline", false)
            )
        } else {
            String::new()
        };
        let status = if reloading {
            "Building the Flutter preview…"
        } else if details["compileError"].is_string() {
            "Waiting for a successful build"
        } else {
            "Sharing on your local network"
        };
        format!(
            r#"<div class="form">
                <p role="status">{status}</p>
                {share}{error}
                <details><summary>Connection help</summary><p>Use the same network on both devices. If Windows blocks the connection, allow Freeflow through the firewall on your private network.</p></details>
                <div class="form-row">{}{}{}</div>
            </div>"#,
            button("reload", "Rebuild preview", "outline", reloading),
            button("console", "Open console", "soft", false),
            button("stop", "Stop sharing", "soft", false),
        )
    } else {
        let address = string(&data["address"]);
        format!(
            r#"<form class="form">
                <p>Open a live Flutter web preview on a phone connected to the same network.</p>
                <details{open}><summary>Network address</summary>
                  <label class="form-field"><span>Computer’s IPv4 address</span>
                    <input name="address" inputmode="decimal" maxlength="15" value="{address}" placeholder="192.168.1.12">
                  </label>
                </details>
                {error}
                <div class="form-error" role="alert"></div>
                <div class="form-footer"><button type="submit" class="button primary">Start sharing</button></div>
            </form>"#,
            open = if address.is_empty() { " open" } else { "" },
            address = h(address),
        )
    };
    html("modal-body", controls);
}

fn update(project: &str, data: Value, force: bool) {
    state::change(|state| {
        if state.project_id() == project && (force || state.phone_preview != data) {
            state.phone_preview = data;
            badge(state);
            if showing_phone_preview(project) {
                render(&state.phone_preview);
            }
        }
    });
}

pub async fn refresh(project: &str) -> Result {
    update(project, project_get(project, ROUTE).await?, false);
    Ok(())
}

pub async fn poll(project: &str) -> Result {
    if showing_phone_preview(project) || state::read(|state| active(&state.phone_preview["job"])) {
        refresh(project).await?;
    }
    Ok(())
}

pub async fn open() -> Result {
    let project = crate::project_id()?;
    let name = state::read(|state| string(&state.project["name"]).to_owned());
    dialogs::open(
        View::PhonePreview {
            project: project.clone(),
        },
        "Preview on phone",
        &name,
        r#"<p role="status">Checking the preview…</p>"#.into(),
    );
    update(&project, project_get(&project, ROUTE).await?, true);
    Ok(())
}

pub async fn submit(project: &str, mut input: Value) -> Result {
    input["action"] = json!("start");
    let job = project_post(project, ROUTE, input).await?;
    crate::track(&job, false);
    refresh(project).await
}

pub async fn act(project: &str, version: u64, action: &str) -> Result {
    match action {
        "copy" => {
            let url = state::read(|state| {
                string(&state.phone_preview["job"]["result"]["url"]).to_owned()
            });
            dialogs::copy(&url, "Phone preview link").await
        }
        "console" => {
            let job = state::read(|state| state.phone_preview["job"].clone());
            dialogs::close_current(version);
            crate::track(&job, true);
            crate::console(true);
            Ok(())
        }
        "reload" | "stop" => {
            project_post(project, ROUTE, json!({"action":action})).await?;
            refresh(project).await
        }
        _ => Ok(()),
    }
}
