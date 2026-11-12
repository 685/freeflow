use crate::{
    dialogs::{self, View, close_current, current_view},
    dom::{
        self, Error, Result, escape as h, id, items, option, path_query, project_get, project_post,
        string, toast,
    },
    state,
};
use serde_json::{Value, json};

const FIELDS: [(&str, &str, &str); 3] = [
    ("seedColor", "Seed color", "#6750a4"),
    ("scaffoldBackgroundColor", "Screen background", "#f8f7f2"),
    ("fontFamily", "Font family", "Registered Flutter font"),
];

pub async fn open(path: Option<String>) -> Result {
    let project = crate::project_id()?;
    let path = path.unwrap_or_else(|| {
        state::read(|state| {
            if state
                .tree
                .iter()
                .any(|entry| entry["path"] == "lib/main.dart")
            {
                "lib/main.dart".into()
            } else {
                state.last_dart.clone()
            }
        })
    });
    let config = project_get(&project, &format!("theme{}", path_query(&path))).await?;
    if crate::current(&project) {
        render(&project, &config, "");
    }
    Ok(())
}

fn render(project: &str, config: &Value, selected: &str) {
    let path = string(&config["path"]);
    let paths: String = state::read(|state| {
        state
            .tree
            .iter()
            .map(|entry| string(&entry["path"]))
            .filter(|path| path.starts_with("lib/") && path.ends_with(".dart"))
            .map(|file| option(file, path))
            .collect()
    });
    let choices = items(&config["apps"]);
    let app = choices
        .iter()
        .find(|app| app["id"] == selected)
        .or_else(|| choices.first());
    let mut body = format!(
        r#"<div class="form">
        <label class="form-field"><span>Dart file</span><select id="theme-file">{paths}</select></label>
        <button type="button" class="button outline small" data-dialog-action="theme-file">Open file theme</button>"#
    );
    if let Some(app) = app {
        if choices.len() > 1 {
            let options: String = choices
                .iter()
                .map(|entry| {
                    format!(
                        r#"<option value="{}"{}>MaterialApp · line {}</option>"#,
                        h(string(&entry["id"])),
                        if entry["id"] == app["id"] {
                            " selected"
                        } else {
                            ""
                        },
                        entry["line"]
                    )
                })
                .collect();
            body.push_str(&format!(r#"<label class="form-field"><span>App widget</span><select id="theme-app">{options}</select></label>
                <button type="button" class="button outline small" data-dialog-action="theme-app">Select app widget</button>"#));
        }
        if app["editable"] == true {
            body.push_str(&format!(
                r#"<form class="form">
                <input type="hidden" name="id" value="{}"><div class="form-grid">"#,
                h(string(&app["id"]))
            ));
            for (name, label, placeholder) in FIELDS {
                let field = &app[name];
                let editable = field["editable"] == true;
                let literal = string(&field["value"]);
                let expression = string(&field["expression"]);
                let show_expression = !expression.is_empty() && literal.is_empty();
                let description = if show_expression {
                    format!(r#"aria-describedby="theme-current-{name}""#)
                } else {
                    String::new()
                };
                body.push_str(&format!(r#"<label class="form-field"><span>{label}</span>
                    <input name="{name}" aria-label="{label}" {description} value="{}" placeholder="{}" maxlength="100" {}>{}</label>"#,
                    h(literal), h(placeholder), if editable { "" } else { "disabled" },
                    if show_expression {
                        format!(r#"<small id="theme-current-{name}">Current Dart value · {}</small>"#, h(expression))
                    } else { String::new() }));
            }
            body.push_str(r#"</div><div class="form-error" role="alert"></div>
                <div class="form-footer"><button type="submit" class="button primary">Save theme</button></div></form>"#);
        } else {
            body.push_str("<p>This theme is defined by a custom Dart expression.</p>");
        }
    } else {
        body.push_str(
            "<p>This file has no MaterialApp. Choose the Dart file that builds your app.</p>",
        );
    }
    body.push_str(r#"<button type="button" class="button soft small" data-dialog-action="theme-agent">Edit theme with Agent</button></div>"#);
    dialogs::open(
        View::Theme {
            project: project.to_owned(),
            config: config.clone(),
        },
        "App theme",
        path,
        body,
    );
}

pub async fn act(version: u64, config: &Value, action: &str) -> Result {
    match action {
        "theme-file" => open(Some(dom::value(&id("theme-file")))).await?,
        "theme-app" => {
            if current_view(version) {
                render(&crate::project_id()?, config, &dom::value(&id("theme-app")));
            }
        }
        "theme-agent" => {
            close_current(version);
            crate::agent_panel::prepare(&format!(
                "Update the app theme. Start by inspecting {}. My design changes are: ",
                string(&config["path"])
            ))
            .await?;
        }
        _ => {}
    }
    Ok(())
}

pub async fn save(project: &str, version: u64, config: &Value, mut input: Value) -> Result {
    let path = string(&config["path"]);
    if state::read(|state| state.drafts.contains_key(&state.draft_key(path))) {
        return Err(Error::new(
            "Save the Dart draft before applying theme changes.",
        ));
    }
    if let Some(app) = items(&config["apps"])
        .iter()
        .find(|app| app["id"] == input["id"])
    {
        for (name, _, _) in FIELDS {
            if input[name] == app[name]["value"] || app[name]["editable"] != true {
                input[name] = Value::Null;
            }
        }
    }
    input["path"] = json!(path);
    input["revision"] = config["revision"].clone();
    project_post(project, "theme", input).await?;
    if state::read(|state| state.project_id() == project && state.current_file() == path) {
        crate::load_file(path, true).await?;
    }
    close_current(version);
    toast("App theme saved", false);
    Ok(())
}
