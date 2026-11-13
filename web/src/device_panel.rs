use crate::{
    dialogs::{self, View, current_view, showing_devices},
    dom::{self, Result, escape as h, html, id, items, project_get, project_post, string},
    state::{self, State},
};
use serde_json::{Value, json};

fn active(session: &Value) -> bool {
    matches!(
        session["status"].as_str(),
        Some("queued" | "running" | "ready")
    )
}

pub fn badge(state: &State) {
    dom::toggle(&id("run-device"), "running", active(&state.device_run));
}

pub async fn open() -> Result {
    let project = crate::project_id()?;
    dialogs::open(
        View::Devices {
            project: project.clone(),
        },
        "Run on device",
        "",
        "<p>Looking for connected devices and running simulators…</p>".into(),
    );
    let result = project_get(&project, "devices").await;
    if !showing_devices(&project) || !crate::current(&project) {
        return Ok(());
    }
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            html(
                "modal-body",
                format!(
                    "<p>{}</p>{}",
                    h(&error.message),
                    button("refresh", "Refresh devices", false)
                ),
            );
            return Ok(());
        }
    };
    update(&project, result["session"].clone());
    let mut options = String::new();
    for device in items(&result["devices"]) {
        let platform = string(&device["platform"]);
        let name = h(string(&device["name"]));
        let kind = if device["emulator"] == true {
            " · Simulator"
        } else {
            ""
        };
        options.push_str(&format!(
            r#"<option value="{}">{name} · {}{kind}</option>"#,
            h(string(&device["id"])),
            h(platform)
        ));
    }
    let launch = if options.is_empty() {
        "<p>Flutter found no devices. Connect a phone or start a simulator, then refresh the list.</p>".into()
    } else {
        format!(
            r#"<form class="form">
            <label class="form-field"><span>Device</span><select name="device" aria-label="Device" required>{options}</select></label>
            <details><summary>Run settings</summary><div class="form">
              <label class="form-field"><span>Flavor</span><input name="flavor" maxlength="40" placeholder="development"></label>
              <label class="form-field"><span>Dart defines</span><textarea name="defines" rows="3" maxlength="8000" placeholder="APP_ENV=development"></textarea></label>
            </div></details>
            <div class="form-error" role="alert"></div>
            <div class="git-actions"><button type="submit" id="device-run-button" class="button primary">Run app</button></div>
        </form>"#
        )
    };
    html(
        "modal-body",
        format!(
            r#"<div id="device-session" class="device-session"></div>
        <div class="device-targets">{launch}<div>{}</div></div>"#,
            button("refresh", "Refresh devices", false)
        ),
    );
    state::read(|state| render_session(&state.device_run));
    Ok(())
}

fn button(action: &str, label: &str, disabled: bool) -> String {
    format!(
        r#"<button type="button" class="button outline small" data-dialog-action="device-{action}"{}>{label}</button>"#,
        if disabled { " disabled" } else { "" }
    )
}

fn render_session(session: &Value) {
    if dom::find("#device-session").is_none() {
        return;
    }
    let running = active(session);
    if dom::find("#device-run-button").is_some() {
        dom::disabled("device-run-button", running);
        dom::hidden("device-run-button", running);
    }
    if session.is_null() {
        html("device-session", "");
        return;
    }
    let details = &session["result"];
    let status = match session["status"].as_str() {
        Some("queued" | "running") => "Starting Flutter",
        Some("ready") if details["reloading"] == true => "Applying change",
        Some("ready") => "Running",
        Some("failed") => "Flutter stopped with an error",
        _ => "Stopped",
    };
    let reload = session["status"] == "ready" && details["reloading"] != true;
    let mut controls = String::new();
    if running {
        controls.push_str(&button(
            "reload",
            "Hot reload",
            !reload || details["capabilities"]["hotReload"] != true,
        ));
        controls.push_str(&button(
            "restart",
            "Hot restart",
            !reload || details["capabilities"]["hotRestart"] != true,
        ));
        controls.push_str(&button("stop", "Stop", false));
    }
    if let Some(url) = details["debugUrl"]
        .as_str()
        .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
    {
        controls.push_str(&format!(r#"<a href="{}" target="_blank" rel="noopener noreferrer" class="button outline small">DevTools</a>"#,h(url)));
    }
    let error = session["error"]
        .as_str()
        .or(details["compileError"].as_str())
        .map(|message| format!(r#"<p class="git-conflict">{}</p>"#, h(message)))
        .unwrap_or_default();
    let logs = items(&session["log"]);
    let output = logs[logs.len().saturating_sub(12)..]
        .iter()
        .map(|line| h(string(&line["message"])))
        .collect::<Vec<_>>()
        .join("\n");
    let log = if output.is_empty() {
        String::new()
    } else {
        format!(r#"<pre class="code-card">{output}</pre>"#)
    };
    let name = details["device"].as_str().unwrap_or_default();
    html(
        "device-session",
        format!(
            r#"<div><strong>{status}</strong> <span class="muted">{}</span></div>
        <div class="git-actions">{controls}</div>{error}{log}"#,
            h(name)
        ),
    );
}

fn update(project: &str, session: Value) {
    state::change(|state| {
        if state.project_id() == project && state.device_run != session {
            state.device_run = session;
            badge(state);
            if showing_devices(project) {
                render_session(&state.device_run);
            }
        }
    });
}

pub async fn poll(project: &str) -> Result {
    if showing_devices(project) || state::read(|state| active(&state.device_run)) {
        refresh(project).await?;
    }
    Ok(())
}

pub async fn refresh(project: &str) -> Result {
    update(project, project_get(project, "device").await?);
    Ok(())
}
pub async fn start(project: &str, version: u64, mut input: Value) -> Result {
    input["action"] = json!("start");
    let job = project_post(project, "device", input).await?;
    crate::track(&job, false);
    if current_view(version) {
        update(project, job);
    }
    Ok(())
}

pub async fn act(project: &str, version: u64, action: &str) -> Result {
    match action.strip_prefix("device-") {
        Some("refresh") => open().await?,
        Some(action @ ("reload" | "restart" | "stop")) => {
            let job = project_post(project, "device", json!({"action":action})).await?;
            if current_view(version) {
                update(project, job);
            }
        }
        _ => {}
    }
    Ok(())
}
