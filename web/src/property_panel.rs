use crate::{
    dialogs::{self, View},
    dom::{self, Error, Result, escape as h, id, items, project_post, string},
    state,
};
use serde_json::{Value, json};
use std::cell::RefCell;

thread_local! { static SEARCH: RefCell<String> = RefCell::default(); }

#[derive(Clone)]
pub(crate) struct Context {
    project: String,
    path: String,
    revision: Value,
    widget: String,
    generation: u64,
}

impl Context {
    async fn capture() -> Result<Self> {
        let generation = state::read(|state| state.generation);
        for _ in 0..80 {
            let (current, pending) = state::read(|state| {
                (
                    state.generation == generation,
                    state.edit_history_busy || state.edit_history_writes > 0,
                )
            });
            if !current {
                return Err(Error::new("The project changed. Select its widget again."));
            }
            if !pending {
                break;
            }
            gloo_timers::future::TimeoutFuture::new(25).await;
        }
        if state::read(|state| state.edit_history_busy || state.edit_history_writes > 0) {
            return Err(Error::new(
                "The previous edit is still refreshing. Try again shortly.",
            ));
        }
        let (project, path, revision) = crate::visual_context()?;
        let (widget, generation) = state::read(|state| {
            (
                state.widget().map(|node| string(&node["id"]).to_owned()),
                state.generation,
            )
        });
        Ok(Self {
            project,
            path,
            revision,
            widget: widget.ok_or_else(|| Error::new("Select a widget first."))?,
            generation,
        })
    }

    fn current(&self) -> bool {
        state::read(|state| {
            state.project_id() == self.project
                && state.generation == self.generation
                && state.visual["path"] == self.path
                && state.widget().is_some_and(|node| node["id"] == self.widget)
        })
    }

    async fn request(&self, action: &str, name: &str, extra: Value) -> Result<Value> {
        let mut input = json!({"action":action,"path":self.path,"revision":self.revision,"id":self.widget,"name":name});
        if let Some(extra) = extra.as_object() {
            input.as_object_mut().unwrap().extend(extra.clone());
        }
        project_post(&self.project, "widget-property", input).await
    }

    async fn save(&self, name: &str, expression: Value, remove: bool) -> Result {
        if !self.current() {
            return Err(Error::new(
                "Select the widget again before saving this property.",
            ));
        }
        crate::visual_edit_at(self.project.clone(), self.path.clone(), self.revision.clone(),
            json!({"id":self.widget,"operation":"expression","name":name,"expression":expression,"remove":remove})).await
    }
}

pub fn search() -> String {
    SEARCH.with_borrow(Clone::clone)
}

pub fn filter() {
    let term = search().to_lowercase();
    for card in dom::all("#inspector [data-property-search]") {
        let text = card
            .get_attribute("data-property-search")
            .unwrap_or_default()
            .to_lowercase();
        dom::attr(&card, "hidden", "");
        if term.is_empty() || text.contains(&term) {
            let _ = card.remove_attribute("hidden");
        }
    }
}

pub async fn reset(name: String) -> Result {
    Context::capture()
        .await?
        .save(&name, Value::Null, true)
        .await
}

pub async fn open(name: String) -> Result {
    let context = Context::capture().await?;
    let data = if name.is_empty() {
        Value::Null
    } else {
        context.request("read", &name, Value::Null).await?
    };
    if !context.current() {
        return Ok(());
    }
    let adding = name.is_empty();
    dialogs::open(
        View::WidgetProperty {
            context: context.clone(),
        },
        if adding {
            "Add property"
        } else {
            "Edit property"
        },
        "Edit this widget’s Dart value, expression or callback.",
        format!(
            r#"<form class="form property-editor">
        <label class="form-field"><span>Property</span><input name="property" list="property-suggestions" value="{}" placeholder="maxLines or style.fontSize" required {}><datalist id="property-suggestions"></datalist></label>
        {}
        <label class="form-field"><span>Dart expression</span><textarea name="expression" rows="9" maxlength="65536" required {} placeholder="'Hello', 24, Colors.blue or () {{ … }}">{}</textarea></label>
        <p class="dialog-help">Use a dot to edit a value inside an existing constructor, such as style.letterSpacing. Changes join the project’s Undo history.</p>
        <div id="property-check" class="property-check" role="status"></div><div class="form-error" role="alert"></div>
        <div class="form-footer"><button type="button" class="button outline" data-dialog-action="property-check">Check Dart</button><button type="button" class="button outline" data-dialog-action="property-remove" {}>Reset property</button><button type="submit" class="button primary">Save property</button></div>
        </form>"#,
            h(&name),
            if adding { "autofocus" } else { "readonly" },
            if adding {
                "<p id=\"property-suggestion-status\" class=\"dialog-help\">Loading properties from Dart…</p>"
            } else {
                ""
            },
            if adding { "" } else { "autofocus" },
            h(string(&data["expression"])),
            if data["removable"] == true {
                ""
            } else {
                "hidden"
            }
        ),
    );
    if adding {
        let version = dialogs::version();
        dom::spawn(async move {
            let result = context.request("suggest", "", Value::Null).await;
            if dialogs::current_view(version) {
                match result {
                    Ok(result) => {
                        let options: String = items(&result["items"])
                            .iter()
                            .filter_map(|item| {
                                let name =
                                    string(&item["label"]).trim_end().strip_suffix(':')?.trim();
                                Some(format!(
                                    "<option value=\"{}\">{}</option>",
                                    h(name),
                                    h(string(&item["detail"]))
                                ))
                            })
                            .collect();
                        let empty = options.is_empty();
                        dom::html("property-suggestions", options);
                        dom::text(
                            "property-suggestion-status",
                            if empty {
                                "No additional parameters found here. You can enter a property name."
                            } else {
                                "Suggestions come from this constructor’s Dart parameters. You can also enter a property name."
                            },
                        );
                    }
                    Err(_) => dom::text(
                        "property-suggestion-status",
                        "Dart suggestions are unavailable. Enter the property name to continue.",
                    ),
                }
            }
            Ok(())
        });
    }
    Ok(())
}

pub async fn submit(context: &Context, version: u64, input: Value) -> Result {
    context
        .save(
            string(&input["property"]).trim(),
            input["expression"].clone(),
            false,
        )
        .await?;
    dialogs::close_current(version);
    Ok(())
}

pub async fn act(context: &Context, version: u64, action: &str) -> Result {
    let Some(name) = dom::find("#modal [name=property]").map(|field| dom::value(&field)) else {
        return Ok(());
    };
    let expression = dom::find("#modal [name=expression]")
        .map(|field| dom::value(&field))
        .unwrap_or_default();
    match action {
        "property-remove" => {
            context.save(&name, Value::Null, true).await?;
            dialogs::close_current(version);
        }
        "property-check" => {
            dom::text("property-check", "Checking Dart…");
            let result = context
                .request("check", &name, json!({"expression":expression}))
                .await;
            if dialogs::current_view(version)
                && dom::find("#modal [name=property]")
                    .is_some_and(|field| dom::value(&field) == name)
                && dom::find("#modal [name=expression]")
                    .is_some_and(|field| dom::value(&field) == expression)
            {
                match result {
                    Ok(result) => {
                        let diagnostics = items(&result["diagnostics"]);
                        if diagnostics.is_empty() {
                            dom::text("property-check", "Dart found no problems in this file.");
                        } else {
                            let rows: String = diagnostics
                                .iter()
                                .map(|item| format!("<li>{}</li>", h(string(&item["message"]))))
                                .collect();
                            dom::html("property-check", format!("<ul>{rows}</ul>"));
                        }
                    }
                    Err(error) => dom::text("property-check", &error.message),
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn initialize() {
    dom::listen(&id("modal-body"), "input", |event| {
        if dom::target(&event).is_some_and(|field| {
            matches!(
                field.get_attribute("name").as_deref(),
                Some("expression" | "property")
            )
        }) && dom::find("#property-check").is_some()
        {
            dom::text("property-check", "");
        }
    });
    dom::listen(&id("inspector"), "input", |event| {
        if let Some(field) = dom::target(&event).filter(|field| field.id() == "property-search") {
            SEARCH.with_borrow_mut(|term| *term = dom::value(&field));
            filter();
        }
    });
    dom::listen(&id("inspector"), "click", |event| {
        if let Some(element) = dom::closest(&event, "[data-property-code]") {
            dom::spawn(open(
                element
                    .get_attribute("data-property-code")
                    .unwrap_or_default(),
            ));
        } else if let Some(element) = dom::closest(&event, "[data-property-reset]") {
            dom::spawn(reset(
                element
                    .get_attribute("data-property-reset")
                    .unwrap_or_default(),
            ));
        }
    });
}
