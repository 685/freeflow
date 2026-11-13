use crate::{
    dom::{self, escape as h},
    input_changed,
};
use gloo_timers::callback::Timeout;
use regex::Regex;
use std::{cell::Cell, collections::HashSet, sync::LazyLock};
use wasm_bindgen::JsCast;
use web_sys::{HtmlElement, HtmlTextAreaElement, KeyboardEvent};

static TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r#"(?s://[^\r\n]*|/\*.*?\*/|r?(?:""".*?"""|'''.*?'''|"(?:\\.|[^"\\\r\n])*"|'(?:\\.|[^'\\\r\n])*'))|\b(?:0[xX][a-fA-F0-9]+|[0-9]+(?:\.[0-9]+)?)\b|[a-zA-Z_$][a-zA-Z0-9_$]*"#
).expect("Dart highlighting pattern")
});
static WORDS: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    "abstract as assert async await break case catch class const continue covariant default deferred do dynamic else enum export extends extension external factory false final finally for Function get hide if implements import in interface is late library mixin new null on operator part required rethrow return sealed set show static super switch sync this throw true try typedef var void while with yield"
    .split_whitespace().collect()
});

thread_local! {
    static DART: Cell<bool> = const { Cell::new(true) };
    static PAINT_PENDING: Cell<bool> = const { Cell::new(false) };
    static LINES: Cell<usize> = const { Cell::new(0) };
}

fn input() -> HtmlTextAreaElement {
    dom::find("#editor textarea")
        .expect("code input")
        .unchecked_into()
}
fn element(selector: &str) -> HtmlElement {
    dom::find(selector)
        .expect("editor element")
        .unchecked_into()
}

fn highlight(source: &str) -> String {
    if source.len() > 180_000 {
        return h(source);
    }
    let mut index = 0;
    let mut output = String::new();
    for token in TOKENS.find_iter(source) {
        output.push_str(&h(&source[index..token.start()]));
        let value = token.as_str();
        let literal = value.strip_prefix('r').unwrap_or(value);
        let kind = if value.starts_with('/') {
            "comment"
        } else if literal.starts_with(['"', '\'']) {
            "string"
        } else if value.as_bytes()[0].is_ascii_digit() {
            "number"
        } else if WORDS.contains(value) {
            "key"
        } else if value
            .trim_start_matches('_')
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_uppercase)
        {
            "class"
        } else {
            ""
        };
        if kind.is_empty() {
            output.push_str(&h(value));
        } else {
            output.push_str(&format!(r#"<span class="tok-{kind}">{}</span>"#, h(value)));
        }
        index = token.end();
    }
    output.push_str(&h(&source[index..]));
    output
}

fn scroll() {
    let input = input();
    let x = -input.scroll_left();
    let y = -input.scroll_top();
    let _ = element(".highlight")
        .style()
        .set_property("transform", &format!("translate({x}px,{y}px)"));
    let _ = element(".line-numbers")
        .style()
        .set_property("transform", &format!("translateY({y}px)"));
}

fn paint() {
    let content = input().value();
    let mut html = if DART.get() {
        highlight(&content)
    } else {
        h(&content)
    };
    html.push('\n');
    element(".highlight").set_inner_html(&html);
    let lines = (content.bytes().filter(|b| *b == b'\n').count() + 1).min(22_000);
    if LINES.replace(lines) != lines {
        let numbers: String = (1..=lines).map(|i| format!("{i}\n")).collect();
        element(".line-numbers").set_text_content(Some(&numbers));
    }
    scroll();
}

fn changed() {
    if !PAINT_PENDING.replace(true) {
        Timeout::new(16, || {
            PAINT_PENDING.set(false);
            paint();
        })
        .forget();
    }
    input_changed(input().value());
    crate::editor_assist::changed();
}

pub fn initialize() {
    dom::html(
        "editor",
        r#"<div class="editor-surface"><div class="line-gutter"><pre class="line-numbers" aria-hidden="true"></pre></div><div class="code-area"><pre class="highlight" aria-hidden="true"></pre><textarea class="editor-input" aria-label="Source code" wrap="off" spellcheck="false" autocomplete="off" autocapitalize="off"></textarea></div></div>"#,
    );
    let field = input();
    dom::listen(&field, "input", |_| changed());
    dom::listen(&field, "scroll", |_| scroll());
    dom::listen(&field, "keydown", |event| {
        let Ok(key) = event.dyn_into::<KeyboardEvent>() else {
            return;
        };
        if crate::editor_assist::key(&key) {
            return;
        }
        if key.key() == "Tab"
            && !key.shift_key()
            && !key.ctrl_key()
            && !key.meta_key()
            && !key.alt_key()
        {
            key.prevent_default();
            let field = input();
            let start = field.selection_start().ok().flatten().unwrap_or_default();
            if field.set_range_text("  ").is_ok() {
                let end = start.saturating_add(2);
                let _ = field.set_selection_range(end, end);
            }
            changed();
        }
    });
}

pub fn set(content: &str, dart: bool, keep: bool) {
    let input = input();
    let position = if keep {
        input.selection_start().ok().flatten().unwrap_or(0)
    } else {
        0
    };
    let (top, left) = if keep {
        (input.scroll_top(), input.scroll_left())
    } else {
        (0, 0)
    };
    DART.set(dart);
    dom::attr(
        &input,
        "aria-label",
        if dart { "Source code" } else { "App plan" },
    );
    input.set_value(content);
    let _ = input.set_selection_range(position, position);
    input.set_scroll_top(top);
    input.set_scroll_left(left);
    paint();
    crate::editor_assist::source_ready(dart);
}

pub fn apply_edit(content: &str, position: u32) {
    let field = input();
    field.set_value(content);
    let _ = field.set_selection_range(position, position);
    changed();
}

pub fn go_to(line: u64, position: u32) {
    let input = input();
    let _ = input.focus();
    let _ = input.set_selection_range(position, position);
    input.set_scroll_top(
        line.saturating_sub(4)
            .saturating_mul(22)
            .min(i32::MAX as u64) as i32,
    );
    scroll();
}
