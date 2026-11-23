use super::*;

fn target(context: &Context) -> Value {
    json!({"path":context.path, "revision":context.revision, "id":context.widget})
}

fn picker(context: Context, choices: Rc<Value>) {
    let active = string(&choices["current"]);
    let entries = items(&choices["entries"]);
    let body = if choices["loading"] == true {
        r#"<p class="dialog-help" role="status">Finding constructors in Dart…</p>"#.into()
    } else if let Some(error) = choices["error"].as_str() {
        format!(r#"<p class="form-error" role="alert">{}</p>"#, h(error))
    } else if entries.is_empty() {
        format!(
            r#"<p class="dialog-help">{} has no other available constructors.</p>"#,
            h(active)
        )
    } else {
        let options: String = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                format!(
                    r#"<option value="{index}">{}</option>"#,
                    h(string(&entry["name"]))
                )
            })
            .collect();
        format!(
            r#"<div class="form"><label class="form-field"><span>New constructor</span><select id="switch-constructor-choice" autofocus>{options}</select></label><p id="switch-constructor-problem" class="form-error" role="alert"></p><div class="form-footer">{}</div></div>"#,
            action("switch-configure", "Configure", "primary")
        )
    };
    let description = if entries.is_empty() {
        String::new()
    } else {
        format!("Currently using {active}.")
    };
    dialogs::open(
        View::WidgetConstructorPicker { context, choices },
        "Change constructor",
        &description,
        body,
    );
}

pub(crate) async fn open() -> Result {
    let context = Context::capture().await?;
    picker(context.clone(), Rc::new(json!({"loading":true})));
    let version = dialogs::version();
    let result = dom::project_post(&context.project, "widget-constructors", target(&context)).await;
    if dialogs::current_view(version) && context.current() {
        let choices = result.unwrap_or_else(|error| json!({"error":error.message}));
        picker(context, Rc::new(choices));
    }
    Ok(())
}

pub(crate) async fn choose(
    context: &Context,
    choices: Rc<Value>,
    version: u64,
    name: &str,
    button: &Element,
) -> Result {
    if name != "switch-configure" {
        return Ok(());
    }
    if !context.current() {
        return Err(Error::new("Select the widget again."));
    }
    let selector = id("switch-constructor-choice");
    let index = dom::value(&selector).parse::<usize>().ok();
    let Some(entry) = index.and_then(|index| items(&choices["entries"]).get(index)) else {
        return Err(Error::new("Choose a constructor."));
    };
    current_definition(entry)?;
    let mut input = target(context);
    input["constructor"] = entry["constructor"].clone();
    input["entryRevision"] = entry["revision"].clone();
    dom::text("switch-constructor-problem", "");
    dom::attr(button, "disabled", "");
    dom::attr(&selector, "disabled", "");
    button.set_text_content(Some("Loading parameters…"));
    let result = dom::project_post(&context.project, "widget-constructor-options", input).await;
    let _ = selector.remove_attribute("disabled");
    let _ = button.remove_attribute("disabled");
    button.set_text_content(Some("Configure"));
    if dialogs::current_view(version) && context.current() {
        match result {
            Ok(config) => form(context.clone(), choices, config),
            Err(error) => dom::text("switch-constructor-problem", &error.message),
        }
    }
    Ok(())
}

fn form(context: Context, choices: Rc<Value>, config: Value) {
    let title = format!("Switch to {}", string(&config["entry"]["name"]));
    let description = format!(
        "Choose what to keep from {}. Dart checks the file before saving.",
        string(&config["current"])
    );
    let parameters = form_parameters(&config, "");
    let body = format!(
        r#"<form class="form widget-constructor-form">{parameters}<div id="switch-constructor-review" class="form-field" hidden><strong>Values to remove</strong><ul id="switch-constructor-removed"></ul><label class="check-row"><input id="switch-constructor-confirm" type="checkbox">Remove these arguments when switching</label></div><div id="widget-library-check" class="dialog-help" role="status"></div><div class="form-error" role="alert"></div><div class="form-footer">{}{}<button type="submit" class="button primary">Switch constructor</button></div></form>"#,
        action("switch-back", "Back", "soft"),
        action("switch-check", "Check Dart", "outline")
    );
    dialogs::open(
        View::WidgetSwitch {
            context,
            choices,
            config,
        },
        &title,
        &description,
        body,
    );
    FORM.with_borrow(refresh_removed);
}

pub(super) fn refresh_removed(config: &Value) {
    let Some(review) = dom::find("#switch-constructor-review") else {
        return;
    };
    let actual = arguments(config, None);
    let kept: Vec<_> = actual
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, argument)| {
            (argument["mode"] == "keep").then(|| string(&argument["value"]))
        })
        .collect();
    let removed: String = items(&config["existing"])
        .iter()
        .filter_map(|old| {
            let name = string(&old["name"]);
            (!kept.contains(&name)).then(|| {
                format!(
                    "<li><code>{}</code> · <code>{}{}</code></li>",
                    h(name),
                    h(string(&old["expression"])),
                    if old["truncated"] == true { "…" } else { "" }
                )
            })
        })
        .collect();
    let list = id("switch-constructor-removed");
    let confirm: HtmlInputElement = id("switch-constructor-confirm").unchecked_into();
    let empty = removed.is_empty();
    if list.get_attribute("data-removed").as_deref() != Some(&removed) {
        dom::attr(&list, "data-removed", &removed);
        list.set_inner_html(&removed);
        confirm.set_checked(false);
    }
    confirm.set_required(!empty);
    if empty {
        dom::attr(&review, "hidden", "");
    } else {
        let _ = review.remove_attribute("hidden");
    }
}

fn request(context: &Context, config: &Value) -> Value {
    let mut input = target(context);
    let confirm: HtmlInputElement = id("switch-constructor-confirm").unchecked_into();
    input["constructor"] = config["entry"]["constructor"].clone();
    input["entryRevision"] = config["entry"]["revision"].clone();
    input["signatureRevision"] = config["signatureRevision"].clone();
    input["arguments"] = arguments(config, None);
    input["allowDrop"] = json!(confirm.checked());
    input
}

pub(crate) async fn act(
    context: &Context,
    choices: Rc<Value>,
    config: &Value,
    version: u64,
    action: &str,
) -> Result {
    if action == "switch-back" {
        picker(context.clone(), choices);
    } else if action == "switch-check" {
        if !context.current() {
            return Err(Error::new("Select the widget again."));
        }
        current_definition(&config["entry"])?;
        dom::text("widget-library-check", "Checking Dart…");
        let proposed = request(context, config);
        let result = dom::project_post(
            &context.project,
            "widget-constructor-check",
            proposed.clone(),
        )
        .await;
        if dialogs::current_view(version)
            && context.current()
            && proposed == request(context, config)
        {
            show_check(result);
        }
    }
    Ok(())
}

pub(crate) async fn save(context: &Context, config: &Value, version: u64) -> Result {
    if !context.current() {
        return Err(Error::new("Select the widget again."));
    }
    crate::visual_context()?;
    current_definition(&config["entry"])?;
    let result = dom::project_post(
        &context.project,
        "widget-constructor-switch",
        request(context, config),
    )
    .await?;
    saved(context, &result, version, "Constructor switched").await
}
