use crate::{
    dom::{
        self, all, attr, basename, disabled, display, escape as h, hidden, html, icon, id, image,
        items, option, string, text, toggle,
    },
    state::{self, Mode, State},
};
use serde_json::Value;
use std::collections::HashMap;

pub fn render() {
    state::read(|state| {
        let open = !state.project.is_null();
        let name = state.project["name"].as_str().unwrap_or("Projects");
        text("active-project", name);
        for control in ["backends", "export", "run", "analyze", "test"] {
            disabled(control, !open);
        }
        hidden("workspace-empty", open);
        hidden("design-pane", !open || state.mode != Mode::Design);
        hidden("editing-pane", !open || state.mode == Mode::Design);
        for mode in all("[data-mode]") {
            let active = mode.get_attribute("data-mode").as_deref() == Some(state.mode.name());
            toggle(&mode, "active", active);
            attr(&mode, "aria-pressed", if active { "true" } else { "false" });
        }
        let sdk = &state.flutter;
        let label = if sdk["found"] == true {
            format!("Flutter {}", sdk["version"].as_str().unwrap_or("SDK"))
        } else {
            "Install Flutter".into()
        };
        html(
            "sdk-status",
            format!(
                "{}<span>{}</span>{}",
                icon("flutter", 15),
                h(&label),
                if sdk["available"] == true {
                    r#"<span class="update-dot" title="Latest stable is available">●</span>"#
                } else {
                    ""
                }
            ),
        );
        explorer(state);
        inspector(state);
        editor(state);
    });
    state::change(|state| preview(state, true));
}

pub fn widget_icon(kind: &str) -> &str {
    let root = kind.split('.').next().unwrap_or(kind);
    match root {
        "Text" | "TextField" => "type",
        "Image" => "image",
        "Row" | "Column" | "Stack" | "ListView" | "Wrap" | "Scaffold" | "MaterialApp" => "layout",
        _ => "box",
    }
}

pub fn explorer(state: &State) {
    let open = !state.project.is_null();
    for tab in all("[data-explorer]") {
        let widgets = tab.get_attribute("data-explorer").as_deref() == Some("widgets");
        let active = widgets == state.widgets;
        toggle(&tab, "active", active);
        attr(&tab, "aria-pressed", if active { "true" } else { "false" });
    }
    hidden("source-select-wrap", !open || !state.widgets);
    hidden("file-actions", !open || state.widgets);
    if !open {
        html(
            "explorer-tree",
            r#"<p class="tree-empty">Open a Flutter project to browse its widgets and files.</p>"#,
        );
        return;
    }
    let mut output = String::new();
    let mut groups: HashMap<&str, Vec<&Value>> = HashMap::new();
    if state.widgets {
        let options: String = state
            .tree
            .iter()
            .filter(|entry| entry["type"] == "file")
            .map(|entry| string(&entry["path"]))
            .filter(|path| path.ends_with(".dart"))
            .map(|path| option(path, &state.last_dart))
            .collect();
        html("source-select", options);
        let nodes = items(&state.visual["nodes"]);
        if nodes.is_empty() {
            html(
                "explorer-tree",
                r#"<p class="tree-empty">Select a Dart file with Flutter widget constructors.</p>"#,
            );
            return;
        }
        for node in nodes {
            groups
                .entry(string(&node["parent"]))
                .or_default()
                .push(node);
        }
        widget_rows(state, &groups, "", 0, &mut output);
    } else {
        for entry in &state.tree {
            let path = string(&entry["path"]);
            let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            groups.entry(parent).or_default().push(entry);
        }
        file_rows(state, &groups, "", 0, &mut output);
    }
    html("explorer-tree", output);
}

fn widget_rows(
    state: &State,
    groups: &HashMap<&str, Vec<&Value>>,
    parent: &str,
    depth: u32,
    output: &mut String,
) {
    if depth > 100 {
        return;
    }
    for node in groups.get(parent).into_iter().flatten() {
        let id = string(&node["id"]);
        let kind = string(&node["type"]);
        let selected = state.widget == id;
        let collapsed = state.collapsed.contains(id);
        let children = groups.contains_key(id);
        let label = display(&node["label"])
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let hint = if label.is_empty() {
            String::new()
        } else {
            format!(r#"<span class="tree-hint ellipsis">{}</span>"#, h(&label))
        };
        let title = h(&format!(
            "{kind}{} · line {}",
            if label.is_empty() {
                String::new()
            } else {
                format!(" · {label}")
            },
            node["line"]
        ));
        let chevron = if children {
            format!(
                r#"<span class="chevron" data-collapse="{}">{}</span>"#,
                h(id),
                icon(if collapsed { "chevron" } else { "down" }, 16)
            )
        } else {
            r#"<span class="chevron"></span>"#.into()
        };
        output.push_str(&format!(r#"<button type="button" class="tree-row{}" data-widget="{}" style="--depth:{}" draggable="{}" aria-pressed="{selected}" title="{title}">{chevron}<span class="tree-symbol">{}</span><span class="tree-label ellipsis">{}</span>{hint}</button>"#,
            if selected { " selected" } else { "" }, h(id), depth.min(10), node["movable"] == true, icon(widget_icon(kind), 16), h(kind)));
        if children && !collapsed {
            widget_rows(state, groups, id, depth + 1, output);
        }
    }
}

fn file_rows(
    state: &State,
    groups: &HashMap<&str, Vec<&Value>>,
    parent: &str,
    depth: u32,
    output: &mut String,
) {
    for entry in groups.get(parent).into_iter().flatten() {
        let path = string(&entry["path"]);
        let dir = entry["type"] == "directory";
        let expanded = state.folders.contains(path);
        let active = !dir && path == state.current_file();
        let file_icon = if dir {
            "folder"
        } else if image(path) {
            "image"
        } else if path.ends_with(".dart") {
            "file-code"
        } else {
            "file"
        };
        output.push_str(&format!(r#"<button type="button" class="tree-row{}" data-{}="{}" style="--depth:{}" title="{}"><span class="chevron">{}</span><span class="tree-symbol">{}</span><span class="tree-label ellipsis">{}</span></button>"#,
            if active { " selected" } else { "" }, if dir { "folder" } else { "file" }, h(path), depth.min(12), h(path),
            if dir { icon(if expanded { "down" } else { "chevron" }, 16) } else { String::new() }, icon(file_icon, 16), h(basename(path))));
        if dir && expanded {
            file_rows(state, groups, path, depth + 1, output);
        }
    }
}

fn property(item: &Value) -> String {
    let name = string(&item["name"]);
    let kind = string(&item["type"]);
    let expression = string(&item["expression"]);
    let value = display(&item["value"]);
    let data = format!(
        r#" data-property="{}" data-type="{}" aria-label="{}""#,
        h(name),
        h(kind),
        h(name)
    );
    let input = if item["editable"] != true {
        format!(
            r#"<input aria-label="{}" readonly value="{}" title="{}">"#,
            h(name),
            h(if expression.is_empty() {
                "Code expression"
            } else {
                expression
            }),
            h(if expression.is_empty() {
                "Edit in Code"
            } else {
                expression
            })
        )
    } else if kind == "color" {
        format!(
            r#"<span class="color-input"><input type="color"{data} value="{}"><input type="text"{data} value="{}" placeholder="{}"></span>"#,
            h(if value.is_empty() { "#ada1ff" } else { &value }),
            h(&value),
            h(if expression.is_empty() {
                "#RRGGBB"
            } else {
                expression
            })
        )
    } else if matches!(kind, "select" | "boolean") {
        let options: String = if kind == "boolean" {
            ["true", "false"]
                .into_iter()
                .map(|entry| option(entry, &value))
                .collect()
        } else {
            items(&item["options"])
                .iter()
                .map(|entry| option(string(entry), &value))
                .collect()
        };
        format!(r#"<select{data}><option value="">Default</option>{options}</select>"#)
    } else if name == "text" {
        format!(r#"<textarea rows="2"{data}>{}</textarea>"#, h(&value))
    } else {
        let (input_type, step) = match kind {
            "integer" => ("number", r#" step="1" min="1""#),
            "number" | "spacing" => ("number", r#" step="any" min="0""#),
            _ => ("text", ""),
        };
        format!(
            r#"<input type="{input_type}"{data}{step} value="{}" placeholder="Default">"#,
            h(&value)
        )
    };
    format!(
        r#"<label class="property"><span class="property-name">{}</span>{input}</label>"#,
        h(name)
    )
}

pub fn inspector(state: &State) {
    let Some(node) = state.widget() else {
        html(
            "inspector",
            format!(
                r#"<div class="inspector-empty">{}<strong>Select a widget</strong><p>Choose a widget in the tree to see its properties and layout controls.</p></div>"#,
                icon("cursor", 16)
            ),
        );
        return;
    };
    let kind = string(&node["type"]);
    let properties: String = items(&node["properties"]).iter().map(property).collect();
    let mut content = format!(
        r#"<div class="inspect-title"><div class="inspect-widget">{}{}</div><div class="inspect-code">{} · {}</div><div class="inspect-actions"><button type="button" data-widget-action="code" class="button soft small">{}Code</button><button type="button" data-widget-action="wrap" class="button soft small"{}>{}Wrap</button>"#,
        icon(widget_icon(kind), 18),
        h(kind),
        h(string(&state.visual["path"])),
        node["line"],
        icon("code", 16),
        if node["wrappable"] == false {
            " disabled"
        } else {
            ""
        },
        icon("wrap", 16)
    );
    if node["movable"] == true {
        let siblings = state.siblings(node);
        let place = siblings
            .iter()
            .position(|sibling| sibling["id"] == node["id"])
            .unwrap_or_default();
        for (direction, enabled) in [("up", place > 0), ("down", place + 1 < siblings.len())] {
            content.push_str(&format!(r#"<button type="button" class="icon-button" data-move="{direction}" aria-label="Move widget {direction}"{}>{}</button>"#,
                if enabled { "" } else { " disabled" }, icon(direction, 16)));
        }
    }
    if node["removable"] == true {
        content.push_str(&format!(r#"<button type="button" class="icon-button" data-widget-action="remove" aria-label="Remove widget" title="Remove widget">{}</button>"#, icon("trash", 16)));
    }
    content.push_str("</div></div>");
    if !properties.is_empty() {
        content.push_str(&format!(
            r#"<div class="inspect-group"><h3>Appearance &amp; layout</h3>{properties}</div>"#
        ));
    }
    let slots = items(&node["slots"]);
    if let Some(first) = slots.first() {
        let options: String = slots
            .iter()
            .map(|entry| option(string(entry), ""))
            .collect();
        let widgets = widget_options(node, string(first));
        content.push_str(&format!(r#"<div class="inspect-group"><h3>Add widget</h3><select id="insert-slot" aria-label="Child slot">{options}</select><div class="add-row"><select id="insert-widget" aria-label="Widget to add">{widgets}</select><button type="button" data-widget-action="add" class="button outline small">{}Add</button></div></div>"#, icon("plus", 16)));
    }
    html("inspector", content);
}

pub fn widget_options(node: &Value, slot: &str) -> String {
    items(&node["slotChoices"][slot])
        .iter()
        .map(|entry| option(string(entry), ""))
        .collect()
}

pub fn editor(state: &State) {
    let file = state.current_file();
    let source = &state.source;
    let draft = state.drafts.get(&state.draft_key(file));
    let editing = state.mode != Mode::Design && !state.project.is_null();
    let (name, kind) = if state.mode == Mode::Plan {
        ("App plan", "plan")
    } else if image(file) {
        (file, "image")
    } else {
        (file, "file-code")
    };
    html(
        "editor-name",
        format!(
            r#"{}<span class="ellipsis" title="{}">{}{}</span>"#,
            icon(kind, 16),
            h(file),
            h(name),
            if draft.is_some() { " •" } else { "" }
        ),
    );
    disabled("save", source.is_null() || draft.is_none());
    hidden(
        "format",
        state.mode != Mode::Code || !file.ends_with(".dart"),
    );
    disabled("format", source.is_null());
    disabled("history", !source["revision"].is_string());
    let conflicted = draft.is_some_and(|draft| draft.revision != source["revision"]);
    hidden("editor-notice", !conflicted);
    if conflicted {
        html(
            "editor-notice",
            r#"<span>File changed on disk. Your draft is still here.</span><button type="button" id="reload-source" class="button outline small">Use disk version</button>"#,
        );
    }
    hidden("editor", !editing || source.is_null());
    hidden("asset-view", !editing || !source.is_null());
    if source.is_null() && !image(file) {
        let label = if state.file_error.is_empty() {
            "Choose a source file."
        } else {
            &state.file_error
        };
        html("asset-view", format!("<p>{}</p>", h(label)));
    }
    let show_tabs = state.mode == Mode::Code && !state.tabs.is_empty();
    hidden("editor-tabs", !show_tabs);
    if show_tabs {
        let tabs: String = state.tabs.iter().map(|tab| format!(r#"<button type="button" class="editor-tab{}" data-file="{}" title="{}">{}{}</button>"#,
            if tab == file { " active" } else { "" }, h(tab), h(tab), h(basename(tab)),
            if state.drafts.contains_key(&state.draft_key(tab)) { " •" } else { "" })).collect();
        html("editor-tabs", tabs);
    }
}

pub fn fit(state: &State) {
    let canvas = id("canvas");
    let frame = id("device-frame");
    let (width, height, bezel) = match state.device.as_str() {
        "tablet" => (768, 930, 14),
        "desktop" => (1280, 760, 8),
        _ => (390, 780, 12),
    };
    let width_frame = f64::from(width + bezel);
    let height_frame = f64::from(height + bezel);
    let scale = ((f64::from(canvas.client_width()) - 46.0) / width_frame)
        .min((f64::from(canvas.client_height()) - 63.0) / height_frame)
        .clamp(0.12, 1.0);
    frame.set_class_name(&format!("device-frame {}", state.device));
    let frame_style = frame.style();
    let _ = frame_style.set_property("width", &format!("{width_frame}px"));
    let _ = frame_style.set_property("height", &format!("{height_frame}px"));
    let _ = frame_style.set_property("transform", &format!("scale({scale})"));
    let canvas_style = id("canvas-content").style();
    let _ = canvas_style.set_property("width", &format!("{}px", width_frame * scale));
    let _ = canvas_style.set_property("height", &format!("{}px", height_frame * scale));
    let refreshing = if state.preview["result"]["reloading"] == true {
        r#"<span class="busy-text">Refreshing</span>"#
    } else {
        ""
    };
    html(
        "canvas-info",
        format!(
            "<span>{width} × {height}</span><span>·</span><span>{}%</span>{refreshing}",
            (scale * 100.0).round()
        ),
    );
}

pub fn preview(state: &mut State, force: bool) {
    let job = &state.preview;
    let status = string(&job["status"]);
    let url = string(&job["result"]["url"]);
    let epoch = job["result"]["epoch"].as_u64().unwrap_or(0);
    let ready = status == "ready" && !url.is_empty();
    let starting = matches!(status, "queued" | "running");
    let failed = status == "failed" || job["result"]["compileError"].is_string();
    let reloading = job["result"]["reloading"] == true;
    let job_id = string(&job["id"]);
    let key = format!(
        "{}:{job_id}:{status}:{url}:{epoch}:{failed}:{reloading}",
        state.project_id()
    );
    if !force && key == state.preview_key {
        return;
    }
    state.preview_key = key;
    let (symbol, action) = if ready || starting {
        ("stop", "Stop")
    } else {
        ("play", "Run")
    };
    html("run", format!("{}<span>{action}</span>", icon(symbol, 15)));
    disabled("reload", !ready || reloading);
    disabled("open-preview", !ready);
    let (class, label) = if failed {
        ("failed", "Build issue")
    } else if starting {
        ("working", "Starting")
    } else if reloading {
        ("working", "Refreshing")
    } else if ready {
        ("live", "Preview live")
    } else {
        ("", "Idle")
    };
    html(
        "preview-status",
        format!(r#"<span class="status-dot {class}"></span>{label}"#),
    );
    hidden("preview-frame", !ready);
    hidden("preview-empty", ready);
    let frame = id("preview-frame");
    if ready {
        if let Ok(next) = web_sys::Url::new(url) {
            next.search_params()
                .set("freeflow", &format!("{job_id}-{epoch}"));
            let href = next.href();
            if frame.get_attribute("src").as_deref() != Some(&href) {
                attr(&frame, "src", &href);
            }
        }
    } else {
        let _ = frame.remove_attribute("src");
        let (title, detail, button) = if starting {
            (
                "Starting Flutter",
                "Resolving packages and compiling the web preview.",
                String::new(),
            )
        } else if failed {
            (
                "Preview stopped",
                "Open the console to see the Flutter output.",
                format!(
                    r#"<button type="button" class="button outline" data-preview-action="console">{}Open console</button>"#,
                    icon("terminal", 16)
                ),
            )
        } else {
            (
                "Run your app",
                "Launch a local preview to interact with your Flutter app.",
                format!(
                    r#"<button type="button" class="button primary" data-preview-action="start">{}Run preview</button>"#,
                    icon("play", 16)
                ),
            )
        };
        let graphic = if starting {
            r#"<span class="spinner"></span>"#.into()
        } else {
            icon(if failed { "warning" } else { "preview" }, 38)
        };
        html(
            "preview-empty",
            format!("{graphic}<strong>{title}</strong><p>{detail}</p>{button}"),
        );
    }
    fit(state);
}

pub fn console_controls(state: &State) {
    hidden("console", !state.console_open);
    attr(
        &id("console-toggle"),
        "aria-expanded",
        if state.console_open { "true" } else { "false" },
    );
    if state.console_open {
        hidden("console-indicator", true);
    }
}

pub fn job_label(kind: &str) -> &str {
    match kind {
        "create" => "Create project",
        "preview" => "Flutter preview",
        "analyze" => "Analyze",
        "test" => "Tests",
        "format" => "Dart format",
        "flutter-sdk" => "Flutter SDK",
        "backend-firebase" => "Firebase",
        "backend-supabase" => "Supabase",
        "android-signing" => "Android signing",
        "export-web" => "Web export",
        "export-android" => "Google Play export",
        "export-apk" => "Android APK export",
        "export-ios" => "App Store export",
        "export-source" => "Flutter source export",
        _ => kind,
    }
}

pub fn console_logs(state: &State, job: &Value) {
    let options: String = state
        .logs
        .iter()
        .map(|entry| {
            let id = string(&entry["id"]);
            let selected = if id == state.console_job {
                " selected"
            } else {
                ""
            };
            format!(
                r#"<option value="{}"{selected}>{}</option>"#,
                h(id),
                h(job_label(string(&entry["kind"])))
            )
        })
        .collect();
    if id("console-job").inner_html() != options {
        html("console-job", options);
    }
    dom::set_value(&id("console-job"), &state.console_job);
    let lines = if job.is_null() {
        "Run a preview, analysis or export to see Flutter output here.".into()
    } else {
        let log = items(&job["log"])
            .iter()
            .map(|line| string(&line["message"]))
            .collect::<Vec<_>>()
            .join("\n");
        if log.is_empty() {
            "Waiting for command output…".into()
        } else {
            log
        }
    };
    let output = id("console-output");
    if output.text_content().as_deref() != Some(&lines) {
        let follow = output.scroll_height() - output.scroll_top() - output.client_height() < 24;
        output.set_text_content(Some(&lines));
        if follow {
            output.set_scroll_top(output.scroll_height());
        }
    }
    hidden(
        "cancel-job",
        job.is_null() || job["kind"] == "preview" || crate::finished(job),
    );
}
