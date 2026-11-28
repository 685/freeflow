use crate::{
    dom::{
        self, Error, Result, closest, disabled, escape as h, html, icon, id, project_route, send,
        string,
    },
    state::{self, Mode, PLAN, State},
    view,
};
use serde_json::json;
use wasm_bindgen::JsCast;
use web_sys::HtmlInputElement;

pub(crate) mod theme;

const GOAL: &str = "## Goal";
const SCREENS: &str = "## Screens";
const SUMMARY: &str = "  - Summary: ";
const LINKS: &str = "  - Opens: ";

struct Document {
    lines: Vec<String>,
    separator: &'static str,
}

struct Screen<'a> {
    line: usize,
    end: usize,
    name: &'a str,
    ready: bool,
    summary: Option<usize>,
    links: Option<usize>,
}

impl Document {
    fn from(content: &str) -> Self {
        let separator = if content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        Self {
            lines: content.split(separator).map(str::to_owned).collect(),
            separator,
        }
    }

    fn section(&self, title: &str) -> Option<(usize, usize)> {
        let mut fenced = false;
        let mut start = None;
        for (line, text) in self.lines.iter().enumerate() {
            if text.trim_start().starts_with("\x60\x60\x60") {
                fenced = !fenced;
            }
            if fenced || !text.starts_with("## ") {
                continue;
            }
            if let Some(start) = start {
                return Some((start, line));
            }
            if text.trim_end() == title {
                start = Some(line + 1);
            }
        }
        start.map(|start| (start, self.lines.len()))
    }

    fn ensure_section(&mut self, title: &str) -> (usize, usize) {
        if let Some(bounds) = self.section(title) {
            return bounds;
        }
        if self.lines.last().is_some_and(|line| !line.is_empty()) {
            self.lines.push(String::new());
        }
        self.lines
            .extend([title.into(), String::new(), String::new()]);
        let end = self.lines.len();
        (end - 2, end)
    }

    fn screens(&self) -> Vec<Screen<'_>> {
        let Some((start, end)) = self.section(SCREENS) else {
            return vec![];
        };
        (start..end)
            .filter_map(|line| {
                let text = self.lines[line].strip_prefix("- ")?;
                let (name, ready) = text
                    .strip_prefix("[x] ")
                    .or_else(|| text.strip_prefix("[X] "))
                    .map_or_else(
                        || (text.strip_prefix("[ ] ").unwrap_or(text), false),
                        |name| (name, true),
                    );
                let (mut next, mut summary, mut links) = (line + 1, None, None);
                while next < end {
                    let text = &self.lines[next];
                    if text.starts_with(SUMMARY) && summary.is_none() {
                        summary = Some(next);
                    } else if text.starts_with(LINKS) && links.is_none() {
                        links = Some(next);
                    } else {
                        break;
                    }
                    next += 1;
                }
                Some(Screen {
                    line,
                    end: next,
                    name,
                    ready,
                    summary,
                    links,
                })
            })
            .collect()
    }

    fn line_value(&self, line: Option<usize>, prefix: &str) -> &str {
        line.and_then(|line| self.lines[line].strip_prefix(prefix))
            .unwrap_or_default()
    }

    fn goal(&self) -> String {
        self.section(GOAL).map_or_else(String::new, |(start, end)| {
            self.lines[start..end].join("\n").trim().to_owned()
        })
    }

    fn set_goal(&mut self, goal: &str) {
        let (start, end) = self.ensure_section(GOAL);
        let block = std::iter::once(String::new())
            .chain(goal.trim_end().lines().map(str::to_owned))
            .chain([String::new()]);
        self.lines.splice(start..end, block);
    }

    fn add(&mut self) {
        let (start, mut end) = self.ensure_section(SCREENS);
        while end > start && self.lines[end - 1].is_empty() {
            end -= 1;
        }
        let count = self.screens().len();
        self.lines
            .insert(end, format!("- [ ] Screen {}", count + 1));
    }

    fn update(&mut self, index: usize, field: &str, value: &str, checked: bool) {
        let screens = self.screens();
        let Some(screen) = screens.get(index) else {
            return;
        };
        let (line, end, summary, links) = (screen.line, screen.end, screen.summary, screen.links);
        match field {
            "remove" => {
                self.lines.drain(line..end);
            }
            "name" | "ready" => {
                let ready = if field == "ready" {
                    checked
                } else {
                    screen.ready
                };
                let name = if field == "name" { value } else { screen.name };
                self.lines[line] = format!("- [{}] {name}", if ready { "x" } else { " " });
            }
            "summary" | "links" => {
                let (current, prefix) = if field == "summary" {
                    (summary, SUMMARY)
                } else {
                    (links, LINKS)
                };
                match (current, value.is_empty()) {
                    (Some(current), true) => {
                        self.lines.remove(current);
                    }
                    (Some(current), false) => self.lines[current] = format!("{prefix}{value}"),
                    (None, false) => self.lines.insert(end, format!("{prefix}{value}")),
                    (None, true) => {}
                }
            }
            _ => {}
        }
    }

    fn content(&self) -> String {
        self.lines.join(self.separator)
    }
}

fn source(state: &State) -> &str {
    state
        .drafts
        .get(&state.draft_key(PLAN))
        .map_or(string(&state.source["content"]), |draft| &draft.content)
}

pub fn render(state: &State) {
    let loaded = !state.source.is_null();
    dom::text(
        "plan-layout",
        if state.plan_markdown {
            "Screens"
        } else {
            "Markdown"
        },
    );
    for name in ["plan-add", "plan-theme", "plan-draft", "plan-generate"] {
        disabled(name, !loaded);
    }
    if !loaded {
        html(
            "plan-screens",
            format!(
                "<p>{}</p>",
                h(if state.file_error.is_empty() {
                    "Loading app plan…"
                } else {
                    &state.file_error
                })
            ),
        );
        return;
    }
    let plan = Document::from(source(state));
    let mut body = format!(
        r#"<label class="form-field plan-goal"><span>App goal</span><textarea id="plan-goal" rows="3" maxlength="10000" placeholder="What should this app do?">{}</textarea></label>"#,
        h(&plan.goal())
    );
    for (index, screen) in plan.screens().iter().enumerate() {
        body.push_str(&format!(
            r#"<article class="plan-screen" data-screen="{index}">
              <div class="inline-actions">{}<button type="button" class="icon-button" data-plan-remove aria-label="Remove planned screen" title="Remove screen">{}</button></div>
              <label class="form-field"><span>Screen name</span><input data-plan-field="name" maxlength="100" value="{}" placeholder="Screen name"></label>
              <label class="form-field"><span>Summary</span><input data-plan-field="summary" maxlength="700" value="{}" placeholder="Main content and actions"></label>
              <label class="form-field"><span>Opens</span><input data-plan-field="links" maxlength="300" value="{}" placeholder="Linked screens"></label>
              <label class="check-row"><input type="checkbox" data-plan-field="ready"{}>Ready to build</label>
            </article>"#,
            icon("phone",18), icon("trash",15), h(screen.name), h(plan.line_value(screen.summary, SUMMARY)),
            h(plan.line_value(screen.links, LINKS)), if screen.ready { " checked" } else { "" }
        ));
    }
    html("plan-screens", body);
}

fn change(edit: impl FnOnce(&mut Document), rerender: bool) {
    state::change(|state| {
        if state.mode != Mode::Plan || state.source.is_null() {
            return;
        }
        let mut doc = Document::from(source(state));
        edit(&mut doc);
        let content = doc.content();
        crate::editor::set(&content, false, true);
        crate::set_draft(state, content);
        view::editor_header(state);
        if rerender {
            render(state);
        }
    });
}

async fn prepare(build: bool) -> Result {
    let project = crate::project_id()?;
    crate::save_draft().await?;
    if build {
        let pending = state::read(|state| {
            (state.mode == Mode::Plan
                && !state.source.is_null()
                && state.source["revision"].is_null())
            .then(|| source(state).to_owned())
        });
        if let Some(text) = pending {
            send(
                &project_route(&project, "file"),
                "PUT",
                json!({"path":PLAN,"revision":null,"content":text}),
            )
            .await?;
            crate::load_file(PLAN, true).await?;
        }
        if !crate::current(&project) {
            return Err(Error::new("Open the project with the app plan."));
        }
        crate::agent_panel::prepare("Read FREEFLOW_PLAN.md and build the Flutter app from the planned screens, user flows and goal. Apply the palette and typography from the Theme section to the global Flutter theme. Inspect existing code first and preserve app behavior that the plan keeps.").await
    } else {
        crate::agent_panel::prepare("Draft or refine FREEFLOW_PLAN.md for this Flutter app. Update only that plan. Use ## Goal and ## Screens with one checklist item per screen, followed by indented Summary and Opens bullets. My app idea is: ").await
    }
}

pub fn initialize() {
    theme::initialize();
    dom::click("plan-layout", || {
        state::change(|state| {
            state.plan_markdown = !state.plan_markdown;
            view::editor(state);
            dom::text(
                "plan-layout",
                if state.plan_markdown {
                    "Screens"
                } else {
                    "Markdown"
                },
            );
        });
    });
    dom::click("plan-add", || {
        change(Document::add, true);
        if let Some(field) = dom::all("[data-plan-field=name]")
            .pop()
            .and_then(|field| field.dyn_into::<HtmlInputElement>().ok())
        {
            let _ = field.focus();
            field.select();
        }
    });
    dom::click("plan-draft", || dom::spawn(prepare(false)));
    dom::click("plan-generate", || dom::spawn(prepare(true)));
    let screens = id("plan-screens");
    dom::listen(&screens, "input", |event| {
        let Some(field) = dom::target(&event) else {
            return;
        };
        let value = dom::value(&field);
        if field.id() == "plan-goal" {
            change(|doc| doc.set_goal(&value), false);
        } else if let Some((index, name)) = field
            .closest("[data-screen]")
            .ok()
            .flatten()
            .and_then(|screen| screen.get_attribute("data-screen")?.parse().ok())
            .zip(field.get_attribute("data-plan-field"))
        {
            let checked = field
                .dyn_ref::<HtmlInputElement>()
                .is_some_and(HtmlInputElement::checked);
            change(|doc| doc.update(index, &name, &value, checked), false);
        }
    });
    dom::listen(&screens, "click", |event| {
        if closest(&event, "[data-plan-remove]").is_none() {
            return;
        }
        if let Some(index) = closest(&event, "[data-screen]")
            .and_then(|node| node.get_attribute("data-screen"))
            .and_then(|value| value.parse().ok())
        {
            change(|doc| doc.update(index, "remove", "", false), true);
        }
    });
}
