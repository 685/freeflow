use super::*;
use crate::dialogs::{self, View};
use serde_json::Value;
use web_sys::HtmlElement;

const TITLE: &str = "## Theme";
const COLORS: [(&str, &str); 3] = [
    ("primary", "Primary"),
    ("surface", "Background"),
    ("ink", "Text"),
];
const FONTS: [(&str, &str, &str); 4] = [
    (
        "System sans-serif",
        "system-ui, sans-serif",
        "System sans-serif",
    ),
    ("Verdana", "Verdana, sans-serif", "Verdana"),
    ("Georgia", "Georgia, serif", "Georgia"),
    ("Consolas", "Consolas, monospace", "Consolas"),
];
const PALETTES: [(&str, [&str; 3]); 4] = [
    ("Violet", ["#6546C9", "#F6F4FC", "#211B33"]),
    ("Citrus", ["#875600", "#FFFAE6", "#302500"]),
    ("Forest", ["#176345", "#ECF8EF", "#112E24"]),
    ("Midnight", ["#A6B9FF", "#192139", "#F2F5FF"]),
];

fn hex(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn font(value: &str) -> Option<&'static str> {
    FONTS
        .iter()
        .find(|(name, _, _)| *name == value)
        .map(|(_, css, _)| *css)
}

fn saved<'a>(doc: &'a Document, label: &str) -> Option<&'a str> {
    let (start, end) = doc.section(TITLE)?;
    let prefix = format!("- {label}: ");
    doc.lines[start..end]
        .iter()
        .find_map(|line| line.strip_prefix(&prefix))
}

fn set(doc: &mut Document, label: &str, value: &str) {
    let (start, end) = doc.ensure_section(TITLE);
    let prefix = format!("- {label}: ");
    let line = format!("{prefix}{value}");
    if let Some(index) = (start..end).find(|index| doc.lines[*index].starts_with(&prefix)) {
        doc.lines[index] = line;
    } else {
        let index = (start..end)
            .rev()
            .find(|index| !doc.lines[*index].is_empty())
            .map_or(start + 1, |index| index + 1);
        doc.lines.insert(index.min(end), line);
    }
}

fn select(
    name: &str,
    label: &str,
    current: &str,
    choices: impl Iterator<Item = (&'static str, &'static str)>,
) -> String {
    let rows: String = choices
        .map(|(value, caption)| {
            format!(
                r#"<option value="{value}"{}>{caption}</option>"#,
                if value == current { " selected" } else { "" }
            )
        })
        .collect();
    format!(
        r#"<label class="form-field"><span>{label}</span><select name="{name}" aria-label="{label}">{rows}</select></label>"#
    )
}

fn open() {
    let Some((project, generation, plan)) = state::read(|state| {
        (state.mode == Mode::Plan && !state.source.is_null()).then(|| {
            (
                state.project_id().to_owned(),
                state.generation,
                Document::from(source(state)),
            )
        })
    }) else {
        return;
    };
    let (palette, default) = PALETTES
        .iter()
        .find(|(name, _)| saved(&plan, "Palette") == Some(*name))
        .unwrap_or(&PALETTES[0]);
    let color_controls:String=COLORS.iter().zip(default).map(|((key,label),fallback)|{
        let value=saved(&plan,label).filter(|value|hex(value)).unwrap_or(fallback);
        format!(r#"<label class="form-field"><span>{label}</span><input type="color" name="{key}" value="{}"></label>"#,h(value))
    }).collect();
    let active_palette = saved(&plan, "Palette").unwrap_or(palette);
    let palettes = select(
        "palette",
        "Palette",
        active_palette,
        PALETTES
            .iter()
            .map(|(name, _)| (*name, *name))
            .chain([("Custom", "Custom")]),
    );
    let heading = select(
        "heading",
        "Heading font",
        saved(&plan, "Heading font").unwrap_or(FONTS[0].0),
        FONTS.iter().map(|(name, _, label)| (*name, *label)),
    );
    let body = select(
        "bodyFont",
        "Body font",
        saved(&plan, "Body font").unwrap_or(FONTS[0].0),
        FONTS.iter().map(|(name, _, label)| (*name, *label)),
    );
    let screens = plan.screens();
    let name = screens.first().map_or("Home", |screen| screen.name);
    let summary = screens
        .first()
        .map_or("", |screen| plan.line_value(screen.summary, SUMMARY));
    let summary = if summary.is_empty() {
        String::new()
    } else {
        format!("<p>{}</p>", h(summary))
    };
    dialogs::open(
        View::PlanTheme {
            project,
            generation,
        },
        "Theme editor",
        "",
        format!(
            r#"<form id="plan-theme-form" class="form">
          <div id="plan-theme-preview" aria-label="Theme preview"><strong>{}</strong>{summary}<span>Continue</span></div>
          {palettes}<div class="plan-theme-colors">{color_controls}</div>{heading}{body}
          <div class="form-error" role="alert"></div><div class="form-footer"><button class="button primary" type="submit">Apply theme to plan</button></div>
        </form>"#,
            h(name)
        ),
    );
    preview();
}

fn field(name: &str) -> String {
    dom::find(&format!("#plan-theme-form [name={name}]"))
        .map(|node| dom::value(&node))
        .unwrap_or_default()
}

fn style(name: &str, value: &str) {
    let preview: HtmlElement = id("plan-theme-preview");
    let _ = preview.style().set_property(name, value);
}

fn preview() {
    for (key, _) in COLORS {
        let value = field(key);
        if hex(&value) {
            style(&format!("--theme-{key}"), &value);
        }
    }
    style(
        "--theme-heading",
        font(&field("heading")).unwrap_or(FONTS[0].1),
    );
    style(
        "--theme-body",
        font(&field("bodyFont")).unwrap_or(FONTS[0].1),
    );
}

pub fn apply(project: &str, generation: u64, version: u64, input: &Value) -> Result {
    if !state::read(|state| {
        state.project_id() == project && state.generation == generation && state.mode == Mode::Plan
    }) {
        return Err(Error::new("Open the app plan again."));
    }
    if COLORS.iter().any(|(key, _)| !hex(string(&input[*key])))
        || font(string(&input["heading"])).is_none()
        || font(string(&input["bodyFont"])).is_none()
    {
        return Err(Error::new("Choose a color and font for the plan."));
    }
    let palette = string(&input["palette"]);
    let palette = PALETTES
        .iter()
        .find(|(name, _)| *name == palette)
        .map_or("Custom", |(name, _)| *name);
    change(
        |doc| {
            set(doc, "Palette", palette);
            for (key, label) in COLORS {
                set(doc, label, string(&input[key]));
            }
            set(doc, "Heading font", string(&input["heading"]));
            set(doc, "Body font", string(&input["bodyFont"]));
        },
        true,
    );
    dialogs::close_current(version);
    Ok(())
}

pub(super) fn initialize() {
    dom::click("plan-theme", open);
    dom::listen(&id("modal-body"), "input", |event| {
        let Some(target) = closest(&event, "#plan-theme-form [name]") else {
            return;
        };
        let name = target.get_attribute("name").unwrap_or_default();
        if name == "palette" {
            let palette = dom::value(&target);
            if let Some((_, colors)) = PALETTES.iter().find(|(name, _)| *name == palette) {
                for ((key, _), value) in COLORS.iter().zip(colors) {
                    if let Some(field) = dom::find(&format!("#plan-theme-form [name={key}]")) {
                        dom::set_value(&field, value);
                    }
                }
            }
        } else if COLORS.iter().any(|(key, _)| *key == name)
            && let Some(palette) = dom::find("#plan-theme-form [name=palette]")
        {
            dom::set_value(&palette, "Custom");
        }
        preview();
    });
}
