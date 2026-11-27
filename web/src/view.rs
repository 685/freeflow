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
        for control in [
            "agent",
            "app-theme",
            "backends",
            "export",
            "run",
            "analyze",
            "test",
            "dependencies",
            "run-device",
            "phone-preview",
        ] {
            disabled(control, !open);
        }
        hidden("workspace-empty", open);
        toggle(
            &id("workspace-surfaces"),
            "split",
            state.mode == Mode::Split,
        );
        hidden("workspace-surfaces", !open);
        hidden("design-pane", !open || !state.mode.shows_preview());
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
        crate::agent_panel::layout(state);
        crate::device_panel::badge(state);
        crate::phone_preview::badge(state);
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
    let assets = state.assets_open && !state.widgets;
    for tab in all("[data-explorer]") {
        let active = tab.get_attribute("data-explorer").as_deref()
            == Some(if state.widgets {
                "widgets"
            } else if assets {
                "assets"
            } else {
                "files"
            });
        toggle(&tab, "active", active);
        attr(&tab, "aria-pressed", if active { "true" } else { "false" });
    }
    let rendered = state.showing_rendered_widgets();
    hidden(
        "tree-mode",
        !open || !state.widgets || !state.mode.shows_preview(),
    );
    hidden("tree-detail", !open || !state.widgets);
    for (name, active) in [
        ("source-widgets", !rendered),
        ("rendered-widgets", rendered),
        ("detailed-widgets", !state.simplified_widgets),
        ("simplified-widgets", state.simplified_widgets),
    ] {
        let button = id(name);
        toggle(&button, "active", active);
        attr(
            &button,
            "aria-pressed",
            if active { "true" } else { "false" },
        );
    }
    hidden("source-select-wrap", !open || !state.widgets || rendered);
    hidden("file-actions", !open || state.widgets || assets);
    hidden("asset-tools", !open || !assets);
    if !open {
        html(
            "explorer-tree",
            r#"<p class="tree-empty">Open a Flutter project to browse its widgets and files.</p>"#,
        );
        return;
    }
    if assets {
        crate::asset_panel::render(state);
        return;
    }
    let mut output = String::new();
    let mut groups: HashMap<&str, Vec<&Value>> = HashMap::new();
    if state.widgets {
        let nodes = if rendered {
            &state.rendered.nodes
        } else {
            let options: String = state
                .tree
                .iter()
                .filter(|entry| entry["type"] == "file")
                .map(|entry| string(&entry["path"]))
                .filter(|path| path.ends_with(".dart"))
                .map(|path| option(path, &state.last_dart))
                .collect();
            html("source-select", options);
            items(&state.visual["nodes"])
        };
        if nodes.is_empty() {
            let hint = if !rendered {
                "Select a Dart file with Flutter widget constructors."
            } else if state.inspect_ready {
                "Waiting for project widgets to render…"
            } else {
                "Run a compatible Flutter debug preview to see rendered widgets."
            };
            html(
                "explorer-tree",
                format!(r#"<p class="tree-empty">{hint}</p>"#),
            );
            return;
        }
        for node in nodes {
            groups
                .entry(string(&node["parent"]))
                .or_default()
                .push(node);
        }
        widget_rows(state, &groups, "", 0, rendered, &mut output);
        if rendered && state.rendered.truncated {
            output.push_str(r#"<p class="tree-empty">Showing part of this widget tree.</p>"#);
        }
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
    rendered: bool,
    output: &mut String,
) {
    if depth > 100 {
        return;
    }
    for original in groups.get(parent).into_iter().flatten() {
        let node = if state.simplified_widgets {
            visible_widget(state, groups, original, rendered)
        } else {
            original
        };
        let id = string(&node["id"]);
        let kind = string(&node["type"]);
        let selected = if rendered {
            state.rendered.selected == id
        } else {
            state.widget == id
        };
        let collapsed = if rendered {
            state.rendered.collapsed.contains(id)
        } else {
            state.collapsed.contains(id)
        };
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
        let attribute = if rendered {
            "data-rendered-widget"
        } else {
            "data-widget"
        };
        let definition = node["ownerComponent"]["widget"] == id;
        let component = node["component"] == true
            || definition
            || rendered
                && items(&state.visual["componentTypes"])
                    .iter()
                    .any(|name| name.as_str() == Some(kind.split('<').next().unwrap_or(kind)));
        let caption = if definition {
            format!("{} · {kind}", string(&node["ownerComponent"]["name"]))
        } else {
            kind.to_owned()
        };
        let placement = if rendered {
            String::new()
        } else {
            format!(r#" data-widget-placement="{}""#, h(string(&original["id"])))
        };
        output.push_str(&format!(r#"<button type="button" class="tree-row{}{}" {attribute}="{}"{placement} style="--depth:{}" draggable="{}" aria-pressed="{selected}" title="{title}">{chevron}<span class="tree-symbol">{}</span><span class="tree-label ellipsis">{}</span>{hint}</button>"#,
            if selected { " selected" } else { "" }, if component { " component-row" } else { "" }, h(id), depth.min(10), !rendered && original["removable"] == true, icon(if component { "component" } else { widget_icon(kind) }, 16), h(&caption)));
        if children && !collapsed {
            widget_rows(state, groups, id, depth + 1, rendered, output);
        }
    }
}

fn visible_widget<'a>(
    state: &State,
    groups: &HashMap<&str, Vec<&'a Value>>,
    mut node: &'a Value,
    rendered: bool,
) -> &'a Value {
    let selected = if rendered {
        &state.rendered.selected
    } else {
        &state.widget
    };
    for _ in 0..100 {
        let id = string(&node["id"]);
        let kind = string(&node["type"]);
        if id == selected
            || node["component"] == true
            || node["ownerComponent"]["widget"] == id
            || !crate::modifier_kind::is_modifier(kind)
            || rendered && state.rendered.truncated
        {
            break;
        }
        let Some([child]) = groups.get(id).map(Vec::as_slice) else {
            break;
        };
        if !rendered && node["modifierChild"] != child["id"] {
            break;
        }
        node = child;
    }
    node
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
    let mut tools = String::new();
    let design_kind = match string(&item["type"]) {
        "color" => "color",
        "number" | "integer" => "number",
        "spacing" => "insets",
        "string" if name == "fontFamily" => "font",
        _ if name == "style" => "typography",
        _ if name.ends_with("borderRadius") => "radius",
        _ => "",
    };
    if !design_kind.is_empty()
        && item["codeEditable"] == true
        && item["designEditable"] != false
        && !item["conditional"].is_object()
    {
        tools.push_str(&format!(r#"<button type="button" class="property-code" data-property-theme="{}" data-design-kind="{design_kind}" aria-label="Choose {} from Theme">{}Theme</button>"#,h(name),h(name),icon("palette",12)));
    }
    if item["codeEditable"] == true {
        if item["editable"] != true && !item["conditional"].is_object() {
            tools.push_str(&format!(r#"<button type="button" class="property-code" data-property-value="{}" aria-label="Edit {} value">Edit value</button>"#, h(name), h(name)));
        }
        tools.push_str(&format!(r#"<button type="button" class="property-code" data-property-code="{}" aria-label="Edit {} as Dart" title="Edit Dart expression">{}Dart</button>"#, h(name), h(name), icon("code", 12)));
    }
    if item["removable"] == true {
        tools.push_str(&format!(r#"<button type="button" class="property-code" data-property-reset="{}" aria-label="Reset {}">Reset</button>"#, h(name), h(name)));
    }
    if let Some(category) = item["asset"]
        .as_str()
        .filter(|_| !item["conditional"].is_object())
    {
        tools.push_str(&format!(r#"<button type="button" class="property-code" data-property-asset="{}" data-asset-category="{}" aria-label="Choose {} from Assets">{}Assets</button>"#, h(name), h(category), h(name), icon("image", 12)));
    }
    let search = format!(
        "{name} {} {} {} {}",
        string(&item["type"]),
        string(&item["expression"]),
        display(&item["value"]),
        item["conditional"]
    );
    format!(
        r#"<div class="property-card" data-property-search="{}">{}<div class="property-tools">{tools}</div></div>"#,
        h(&search),
        property_input(item, item, name, "")
    )
}

fn property_input(item: &Value, field: &Value, label: &str, branch: &str) -> String {
    let name = string(&item["name"]);
    let kind = string(&item["type"]);
    let conditional = &field["conditional"];
    if conditional.is_object() {
        let mut branches = String::new();
        for (key, caption) in [("then", "If true"), ("else", "If false")] {
            let path = if branch.is_empty() {
                key.to_owned()
            } else {
                format!("{branch}/{key}")
            };
            branches.push_str(&property_input(item, &conditional[key], caption, &path));
        }
        return format!(
            r#"<fieldset class="conditional-property"><legend>{}</legend><code title="Dart condition">{}</code>{branches}</fieldset>"#,
            h(label),
            h(string(&conditional["condition"])),
        );
    }
    let expression = string(&field["expression"]);
    let value = display(&field["value"]);
    let aria = if branch.is_empty() {
        name.to_owned()
    } else {
        let branch = branch
            .split('/')
            .map(|part| if part == "then" { "true" } else { "false" })
            .collect::<Vec<_>>()
            .join(" / ");
        format!("{name} when {branch}")
    };
    let data = format!(
        r#" data-property="{}" data-type="{}" data-branch="{}" aria-label="{}""#,
        h(name),
        h(kind),
        h(branch),
        h(&aria),
    );
    let input = if field["editable"] != true {
        format!(
            r#"<input aria-label="{}" readonly value="{}" title="{}">"#,
            h(&aria),
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
        h(label)
    )
}

fn modifiers(state: &State, selected: &Value) -> String {
    let mut base = selected;
    for _ in 0..100 {
        let Some(child) = base["modifierChild"].as_str().and_then(|id| state.node(id)) else {
            break;
        };
        base = child;
    }
    let mut chain = vec![];
    let mut node = base;
    for _ in 0..100 {
        let Some(parent) = node["parent"].as_str().and_then(|id| state.node(id)) else {
            break;
        };
        if parent["modifierChild"] != node["id"] {
            break;
        }
        chain.push(parent);
        node = parent;
    }
    let choices: String = if chain.is_empty() {
        String::new()
    } else {
        std::iter::once(base).chain(chain).map(|node| {
            let active = node["id"] == selected["id"];
            let kind = string(&node["type"]);
            format!(
                r#"<button type="button" class="button soft small modifier-choice" data-modifier-select="{}" aria-pressed="{active}" title="Edit {} properties">{}</button>"#,
                h(string(&node["id"])), h(kind), h(kind)
            )
        }).collect()
    };
    let add = if selected["libraryWrappable"] != false {
        format!(
            r#"<button type="button" data-widget-action="wrap" class="button outline small" title="Wrap this widget">{}Add modifier</button>"#,
            icon("plus", 14)
        )
    } else {
        String::new()
    };
    if add.is_empty() && choices.is_empty() {
        return String::new();
    }
    format!(
        r#"<section class="inspect-group modifiers" aria-label="Modifiers"><div class="modifiers-heading"><h3>Modifiers</h3>{add}</div><div class="modifier-choices">{choices}</div></section>"#
    )
}

pub fn inspector(state: &State) {
    crate::agent_panel::context(state);
    let rendered = state.showing_rendered_widgets();
    let node = state
        .widget()
        .filter(|_| !rendered || !state.rendered.selected.is_empty());
    let Some(node) = node else {
        let hint = if rendered {
            "Select an item in the rendered tree to open its Dart source and properties."
        } else {
            "Choose a widget in the tree to see its properties and layout controls."
        };
        html(
            "inspector",
            format!(
                r#"{}<div class="inspector-empty">{}<strong>Select a widget</strong><p>{hint}</p></div>"#,
                crate::component_panel::navigation(state),
                icon("cursor", 16)
            ),
        );
        return;
    };
    let kind = string(&node["type"]);
    let properties: String = items(&node["properties"]).iter().map(property).collect();
    let mut content = format!(
        r#"<div class="inspect-title"><div class="inspect-widget">{}{}</div><div class="inspect-code">{} · {}</div><div class="inspect-actions"><button type="button" data-widget-action="code" class="button soft small">{}Code</button>"#,
        icon(widget_icon(kind), 18),
        h(kind),
        h(string(&state.visual["path"])),
        node["line"],
        icon("code", 16)
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
    content.push_str(r#"<button type="button" class="button outline small" data-widget-action="switch-constructor">Change constructor</button>"#);
    content.push_str(&crate::widget_clipboard::controls(node, false));
    content.push_str(&crate::component_panel::controls(node, false));
    content.push_str(&modifiers(state, node));
    content.push_str(&format!(r#"<div class="property-toolbar"><input id="property-search" type="search" aria-label="Search properties" placeholder="Search properties…" value="{}"><button type="button" class="button outline small" data-property-value="">{}Add property</button></div>"#, h(&crate::property_panel::search()), icon("plus", 14)));
    if rendered {
        content.push_str(r#"<p class="inspect-note">Edits apply to every instance created by this Dart constructor.</p>"#);
    }
    if !properties.is_empty() {
        content.push_str(&format!(
            r#"<div class="inspect-group"><h3>Appearance &amp; layout</h3>{properties}</div>"#
        ));
    }
    if !items(&node["arguments"]).is_empty() {
        let arguments: String = items(&node["arguments"]).iter().map(property).collect();
        content.push_str(&format!(
            r#"<div class="inspect-group"><h3>Other properties</h3>{arguments}</div>"#
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
    if node["libraryChildren"] == true {
        content.push_str(&format!(r#"<div class="inspect-group"><button type="button" class="button outline" data-widget-action="library">{}Browse widget library</button></div>"#,icon("plus",16)));
    }
    html(
        "inspector",
        format!("{}{content}", crate::component_panel::navigation(state)),
    );
    crate::property_panel::filter();
}

pub fn widget_options(node: &Value, slot: &str) -> String {
    items(&node["slotChoices"][slot])
        .iter()
        .map(|entry| option(string(entry), ""))
        .collect()
}

pub fn editor_header(state: &State) {
    crate::saved_edits::controls(state);
    let file = state.current_file();
    let source = &state.source;
    let draft = state.drafts.get(&state.draft_key(file));
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
        !state.mode.shows_code() || !file.ends_with(".dart"),
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
}

pub fn editor(state: &State) {
    editor_header(state);
    let file = state.current_file();
    let source = &state.source;
    let editing = state.mode != Mode::Design && !state.project.is_null();
    let planning = state.mode == Mode::Plan && !state.plan_markdown;
    hidden("editor", !editing || source.is_null() || planning);
    hidden(
        "dart-tools",
        !state.mode.shows_code() || !file.ends_with(".dart") || source.is_null(),
    );
    hidden("asset-view", !editing || !source.is_null() || planning);
    hidden("plan-layout", state.mode != Mode::Plan);
    hidden("plan-screen-view", !planning);
    if planning {
        crate::planner::render(state);
    }
    if source.is_null() && !crate::asset_panel::previewable(file) {
        let label = if state.file_error.is_empty() {
            "Choose a source file."
        } else {
            &state.file_error
        };
        html("asset-view", format!("<p>{}</p>", h(label)));
    }
    let show_tabs = state.mode.shows_code() && !state.tabs.is_empty();
    hidden("editor-tabs", !show_tabs);
    if show_tabs {
        let tabs: String = state.tabs.iter().map(|tab| format!(r#"<button type="button" class="editor-tab{}" data-file="{}" title="{}">{}{}</button>"#,
            if tab == file { " active" } else { "" }, h(tab), h(tab), h(basename(tab)),
            if state.drafts.contains_key(&state.draft_key(tab)) { " •" } else { "" })).collect();
        html("editor-tabs", tabs);
    }
}

pub fn fit(state: &State) {
    crate::preview_canvas::render(state);
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
    let supports_inspector = job["result"]["inspector"] == true;
    let supports_reload = job["result"]["hotReload"] == true;
    let reload_count = job["result"]["reloadCount"].as_u64().unwrap_or_default();
    let job_id = string(&job["id"]);
    let key = format!(
        "{}:{job_id}:{status}:{url}:{epoch}:{failed}:{reloading}:{supports_reload}:{reload_count}",
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
    disabled("reload", !ready || reloading || !supports_reload);
    disabled("restart-preview", !ready || reloading);
    attr(
        &id("reload"),
        "title",
        if ready && !supports_reload {
            "Hot reload requires a Flutter 3.38 or newer debug preview. Use Hot restart with this preview."
        } else {
            "Hot reload — preserve app state"
        },
    );
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
                crate::preview_inspector::reset(state, supports_inspector);
                attr(&frame, "src", &href);
            } else if !reloading {
                crate::preview_inspector::request_tree(state);
            }
        }
    } else {
        crate::preview_inspector::reset(state, false);
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
    if !state.mode.shows_preview() {
        crate::preview_inspector::stop(state);
    }
    crate::preview_inspector::controls(state);
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
        "phone-preview" => "Phone preview",
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
