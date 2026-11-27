use crate::files::{Problem, Result};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

mod components;
mod design;
pub(crate) mod library;
mod properties;
pub(crate) mod values;
pub(crate) use components::Scope;
pub(crate) use components::component_at;
pub(crate) use design::import_reference;
pub(crate) use design::{
    design_binding, design_import, design_reference, design_values, edit_design_value,
};
#[cfg(test)]
pub use properties::{property_completion, property_details};
pub(crate) use properties::{property_completion_in, property_details_in};

#[derive(Deserialize)]
struct Field {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    positional: bool,
    prefix: Option<String>,
    options: Option<Vec<String>>,
    nest: Option<(String, String)>,
    min: Option<f64>,
    max: Option<f64>,
    asset: Option<String>,
}

#[derive(Default, Deserialize)]
struct Spec {
    slots: Vec<(String, bool)>,
    fields: Vec<Field>,
}

#[derive(Deserialize)]
struct Catalog {
    widgets: HashMap<String, Spec>,
    palette: Vec<(String, String)>,
    wrappers: Vec<(String, String)>,
}

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    serde_json::from_str(include_str!("widgets.json")).expect("valid widget catalog")
});
static EMPTY: LazyLock<Spec> = LazyLock::new(Spec::default);
static COLOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:const\s+)?Color\(0x[fF]{2}([a-fA-F0-9]{6})\)$").unwrap());
static NAMED_COLOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Colors\.[A-Za-z0-9_]+$").unwrap());
static RGB: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^#[a-f0-9]{6}$").unwrap());
static SPACING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:const\s+)?(?P<namespace>[A-Za-z_$][A-Za-z0-9_$]*\.)?EdgeInsets\.all\(\s*(?P<amount>[0-9]+(?:\.[0-9]+)?)\s*\)$",
    ).unwrap()
});
static NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-?(?:[0-9]+(?:\.[0-9]+)?|\.[0-9]+)$").unwrap());

#[derive(Debug)]
struct Token<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    close: Option<usize>,
}

fn is_string(data: &[u8], index: usize) -> bool {
    matches!(data.get(index), Some(b'\'' | b'"'))
        || (data.get(index) == Some(&b'r') && matches!(data.get(index + 1), Some(b'\'' | b'"')))
}

fn comment_end(data: &[u8], start: usize) -> usize {
    if data[start..].starts_with(b"//") {
        return data[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(data.len(), |offset| start + offset);
    }
    let mut depth = 1;
    let mut i = start + 2;
    while i < data.len() && depth > 0 {
        if data[i..].starts_with(b"/*") {
            depth += 1;
            i += 2;
        } else if data[i..].starts_with(b"*/") {
            depth -= 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    i
}

fn string_end(data: &[u8], start: usize, recursion: usize) -> Result<usize> {
    if recursion > 100 {
        return Err(Problem::new(
            422,
            "This Dart expression is too deeply nested for the visual editor.",
        ));
    }
    let raw = data[start] == b'r';
    let quote = start + usize::from(raw);
    let delimiter = if data[quote..].starts_with(&[data[quote]; 3]) {
        &data[quote..quote + 3]
    } else {
        &data[quote..quote + 1]
    };
    let mut i = quote + delimiter.len();
    while i < data.len() {
        if data[i..].starts_with(delimiter) {
            return Ok(i + delimiter.len());
        }
        if !raw && data[i] == b'\\' {
            i += 2;
        } else if !raw && data[i..].starts_with(&[b'$', b'{']) {
            i += 2;
            let mut depth = 1;
            while i < data.len() && depth > 0 {
                if data[i..].starts_with(b"//") || data[i..].starts_with(b"/*") {
                    i = comment_end(data, i);
                } else if is_string(data, i) {
                    i = string_end(data, i, recursion + 1)?;
                } else {
                    if data[i] == b'{' {
                        depth += 1;
                    }
                    if data[i] == b'}' {
                        depth -= 1;
                    }
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }
    Ok(data.len())
}

fn tokenize(source: &str) -> Result<Vec<Token<'_>>> {
    let mut tokens: Vec<Token<'_>> = vec![];
    let mut stack: Vec<usize> = vec![];
    let mut i = 0;
    let data = source.as_bytes();
    while i < data.len() {
        let ch = source[i..].chars().next().unwrap();
        if ch.is_whitespace() {
            i += ch.len_utf8();
            continue;
        }
        if data[i..].starts_with(b"//") || data[i..].starts_with(b"/*") {
            i = comment_end(data, i);
            continue;
        }
        let start = i;
        if is_string(data, i) {
            i = string_end(data, i, 0)?;
        } else {
            i += ch.len_utf8();
            if ch.is_ascii_alphabetic() || matches!(ch, '_' | '$') {
                while i < data.len()
                    && (data[i].is_ascii_alphanumeric() || matches!(data[i], b'_' | b'$'))
                {
                    i += 1;
                }
            }
        }
        let text = &source[start..i];
        let index = tokens.len();
        match text {
            "(" | "[" | "{" => stack.push(index),
            ")" | "]" | "}" => {
                if let Some(open) = stack.pop() {
                    if matches!(
                        (tokens[open].text, text),
                        ("(", ")") | ("[", "]") | ("{", "}")
                    ) {
                        tokens[open].close = Some(index);
                    }
                }
            }
            _ => {}
        }
        tokens.push(Token {
            text,
            start,
            end: i,
            close: None,
        });
    }
    Ok(tokens)
}

fn at<'a>(tokens: &[Token<'a>], index: usize) -> &'a str {
    tokens.get(index).map_or("", |token| token.text)
}

fn type_end(tokens: &[Token<'_>], start: usize, limit: usize) -> usize {
    if at(tokens, start) != "<" {
        return start;
    }
    let mut depth = 1;
    for i in start + 1..limit.min(start + 64) {
        match at(tokens, i) {
            "<" => depth += 1,
            ">" => {
                depth -= 1;
                if depth == 0 {
                    return if matches!(at(tokens, i + 1), "[" | "{" | "(" | ".") {
                        i
                    } else {
                        start
                    };
                }
            }
            "," | "?" => {}
            value
                if !value.is_empty()
                    && value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.')
                    }) => {}
            _ => return start,
        }
    }
    start
}

#[derive(Clone, Copy)]
struct Span {
    first: usize,
    last: usize,
    comma: Option<usize>,
}

impl Span {
    fn text<'a>(&self, tokens: &[Token<'_>], source: &'a str) -> &'a str {
        &source[tokens[self.first].start..tokens[self.last].end]
    }
}

const MAX_BRANCH_DEPTH: usize = 8;

fn enclosed(tokens: &[Token<'_>], mut span: Span) -> Span {
    while at(tokens, span.first) == "("
        && tokens[span.first].close == Some(span.last)
        && span.last > span.first + 1
    {
        span.first += 1;
        span.last -= 1;
    }
    span
}

fn question(tokens: &[Token<'_>], index: usize) -> bool {
    at(tokens, index) == "?"
        && !tokens.get(index + 1).is_some_and(|next| {
            tokens[index].end == next.start && matches!(next.text, "?" | "." | "[")
        })
        && !index.checked_sub(1).is_some_and(|previous| {
            at(tokens, previous) == "?" && tokens[previous].end == tokens[index].start
        })
}

struct Conditional {
    condition: Span,
    when_true: Span,
    when_false: Span,
}

fn conditional(tokens: &[Token<'_>], span: Span) -> Option<Conditional> {
    let span = enclosed(tokens, span);
    let mut test = None;
    let mut nested = 0;
    let mut index = span.first;
    while index <= span.last {
        if question(tokens, index) {
            if test.is_some() {
                nested += 1;
            } else {
                test = Some(index);
            }
        } else if at(tokens, index) == ":" {
            let question = test?;
            if nested == 0 {
                return (question > span.first && index > question + 1 && index < span.last).then(
                    || Conditional {
                        condition: Span {
                            first: span.first,
                            last: question - 1,
                            comma: None,
                        },
                        when_true: Span {
                            first: question + 1,
                            last: index - 1,
                            comma: None,
                        },
                        when_false: Span {
                            first: index + 1,
                            last: span.last,
                            comma: None,
                        },
                    },
                );
            }
            nested -= 1;
        }
        index = tokens[index]
            .close
            .unwrap_or_else(|| type_end(tokens, index, span.last + 1))
            + 1;
    }
    None
}

fn segments(tokens: &[Token<'_>], open: usize, close: usize) -> Vec<Span> {
    let mut output = vec![];
    let mut first = open + 1;
    let mut i = first;
    while i <= close {
        if i == close || at(tokens, i) == "," {
            if i > first {
                output.push(Span {
                    first,
                    last: i - 1,
                    comma: (i < close).then_some(i),
                });
            }
            first = i + 1;
        } else {
            i = tokens[i]
                .close
                .unwrap_or_else(|| type_end(tokens, i, close));
        }
        i += 1;
    }
    output
}

#[derive(Clone, Copy)]
struct Arg<'a> {
    span: Span,
    name: Option<&'a str>,
    expression: usize,
}

impl Arg<'_> {
    fn expression<'a>(&self, tokens: &[Token<'_>], source: &'a str) -> &'a str {
        &source[tokens[self.expression].start..tokens[self.span.last].end]
    }
}

fn args<'a>(tokens: &[Token<'a>], open: usize) -> Vec<Arg<'a>> {
    segments(tokens, open, tokens[open].close.unwrap_or(open))
        .into_iter()
        .map(|span| {
            let named = span.last > span.first + 1 && at(tokens, span.first + 1) == ":";
            Arg {
                name: named.then(|| at(tokens, span.first)),
                expression: span.first + if named { 2 } else { 0 },
                span,
            }
        })
        .collect()
}

fn decode_literal(expression: &str, item: &Field) -> Option<Value> {
    match item.kind.as_str() {
        "string" => {
            let data = expression.as_bytes();
            let quote = *data.first()?;
            if !matches!(quote, b'\'' | b'"')
                || data.len() < 2
                || data.last() != Some(&quote)
                || data.starts_with(&[quote; 3])
                || string_end(data, 0, 0).ok()? != data.len()
            {
                return None;
            }
            let mut decoded = String::new();
            let mut chars = expression[1..expression.len() - 1].chars();
            while let Some(ch) = chars.next() {
                match ch {
                    '$' => return None,
                    '\\' => decoded.push(match chars.next()? {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        ch @ ('\'' | '"' | '\\' | '$') => ch,
                        _ => return None,
                    }),
                    _ => decoded.push(ch),
                }
            }
            Some(json!(decoded))
        }
        "boolean" => match expression {
            "true" => Some(json!(true)),
            "false" => Some(json!(false)),
            _ => None,
        },
        "select" => {
            let value = expression
                .strip_prefix(item.prefix.as_deref()?)?
                .strip_prefix('.')?;
            item.options
                .as_ref()?
                .iter()
                .any(|option| option == value)
                .then(|| json!(value))
        }
        "color" => COLOR
            .captures(expression)
            .map(|cap| json!(format!("#{}", cap[1].to_ascii_lowercase()))),
        "spacing" => SPACING
            .captures(expression)?
            .name("amount")?
            .as_str()
            .parse::<f64>()
            .ok()
            .map(|v| json!(v)),
        _ if NUMBER.is_match(expression) => expression
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| json!(v)),
        _ => None,
    }
}

fn encode_literal(value: &Value, item: &Field) -> Result<String> {
    let encoded = match item.kind.as_str() {
        "string" => value
            .as_str()
            .filter(|text| text.encode_utf16().count() <= 4000)
            .map(|text| {
                let mut result = String::from("'");
                for ch in text.chars() {
                    match ch {
                        '\n' => result.push_str("\\n"),
                        '\r' => result.push_str("\\r"),
                        '\t' => result.push_str("\\t"),
                        '\\' | '\'' | '$' => {
                            result.push('\\');
                            result.push(ch);
                        }
                        _ => result.push(ch),
                    }
                }
                result.push('\'');
                result
            }),
        "boolean" => value.as_bool().map(|v| v.to_string()),
        "select" => value
            .as_str()
            .filter(|v| {
                item.options
                    .as_ref()
                    .is_some_and(|opts| opts.iter().any(|o| o == v))
            })
            .map(|v| format!("{}.{v}", item.prefix.as_deref().unwrap_or_default())),
        "color" => value
            .as_str()
            .filter(|v| RGB.is_match(v))
            .map(|v| format!("const Color(0xff{})", v[1..].to_ascii_lowercase())),
        kind => {
            let integer = kind == "integer";
            value
                .as_f64()
                .filter(|v| {
                    v.is_finite()
                        && *v >= item.min.unwrap_or(if integer { 1.0 } else { 0.0 })
                        && *v <= item.max.unwrap_or(10000.0)
                        && (!integer || v.fract() == 0.0)
                })
                .map(|v| {
                    if kind == "spacing" {
                        format!("const EdgeInsets.all({v})")
                    } else {
                        v.to_string()
                    }
                })
        }
    };
    encoded.ok_or_else(|| Problem::new(422, format!("Enter a valid value for {}.", item.name)))
}

struct List {
    open: usize,
    items: Vec<Span>,
}

fn list_for(tokens: &[Token<'_>], arg: Arg<'_>) -> Option<List> {
    let mut open = arg.expression;
    if at(tokens, open) == "const" {
        open += 1;
    }
    let end = type_end(tokens, open, arg.span.last + 1);
    if end > open {
        open = end + 1;
    }
    if at(tokens, open) != "[" || tokens[open].close != Some(arg.span.last) {
        return None;
    }
    Some(List {
        open,
        items: segments(tokens, open, arg.span.last),
    })
}

fn empty_child(tokens: &[Token<'_>], arg: Arg<'_>) -> bool {
    if arg.expression == arg.span.last && at(tokens, arg.expression) == "null" {
        return true;
    }
    let mut index = arg.expression;
    if matches!(at(tokens, index), "const" | "new") {
        index += 1;
    }
    if at(tokens, index + 1) == "." && at(tokens, index + 2) == "SizedBox" {
        index += 2;
    }
    at(tokens, index) == "SizedBox"
        && at(tokens, index + 1) == "."
        && at(tokens, index + 2) == "shrink"
        && at(tokens, index + 3) == "("
        && at(tokens, index + 4) == ")"
        && index + 4 == arg.span.last
}

struct Node<'a> {
    id: String,
    kind: &'a str,
    constructor: &'a str,
    open: usize,
    close: usize,
    start: usize,
    line: usize,
    column: usize,
    symbol: usize,
    component: bool,
    parent: Option<usize>,
    args: Vec<Arg<'a>>,
    spec: &'static Spec,
}

impl Node<'_> {
    fn namespace(&self, tokens: &[Token<'_>]) -> String {
        let first = self.start + usize::from(matches!(at(tokens, self.start), "const" | "new"));
        if at(tokens, first + 1) == "." && at(tokens, first + 2) == self.kind {
            format!("{}.", at(tokens, first))
        } else {
            String::new()
        }
    }
    fn slots(&self) -> &[(String, bool)] {
        if matches!(
            (self.kind, self.constructor),
            ("ListView" | "GridView", "builder" | "separated" | "custom")
        ) {
            return &[];
        }
        &self.spec.slots
    }
}

fn inspect(source: &str) -> Result<(Vec<Token<'_>>, Vec<Node<'_>>)> {
    inspect_in(source, &Scope::default())
}

fn inspect_in<'a>(source: &'a str, scope: &Scope) -> Result<(Vec<Token<'a>>, Vec<Node<'a>>)> {
    let tokens = tokenize(source)?;
    let mut custom = scope.names.clone();
    custom.extend(scope.external.clone());
    components::add_names(&components::classes(&tokens), &mut custom);
    let mut nodes: Vec<Node<'_>> = vec![];
    let mut parents: Vec<usize> = vec![];
    let (mut last_start, mut line, mut line_start, mut position) = (0, 1, 0, 0);
    for (i, token) in tokens.iter().enumerate() {
        let kind = token.text;
        if !CATALOG.widgets.contains_key(kind) && !custom.contains(kind) {
            continue;
        }
        let previous = i.checked_sub(1).map_or("", |prior| at(&tokens, prior));
        if matches!(previous, "class" | "factory") {
            continue;
        }
        let mut open = i + 1;
        let generic = type_end(&tokens, open, tokens.len());
        if generic > open {
            open = generic + 1;
        }
        let mut constructor = "";
        if at(&tokens, open) == "." && at(&tokens, open + 2) == "(" {
            constructor = at(&tokens, open + 1);
            open += 2;
        }
        if at(&tokens, open) != "(" {
            continue;
        }
        let Some(close) = tokens[open].close else {
            continue;
        };
        let mut start = i;
        if previous == "." && i >= 2 {
            start = i - 2;
        }
        if start > 0 && matches!(at(&tokens, start - 1), "const" | "new") {
            start -= 1;
        }
        if matches!(at(&tokens, close + 1), "{" | "=>")
            || (at(&tokens, close + 1) == ":"
                && !start
                    .checked_sub(1)
                    .is_some_and(|prior| question(&tokens, prior)))
            || (at(&tokens, close + 1) == "=" && at(&tokens, close + 2) == ">")
            || (custom.contains(kind) && at(&tokens, open + 1) == "{")
        {
            continue;
        }
        let offset = tokens[start].start;
        if offset >= last_start {
            let text = &source[last_start..offset];
            line += text.bytes().filter(|b| *b == b'\n').count();
            if let Some(last) = text.rfind('\n') {
                line_start = last_start + last + 1;
            }
            position += text.encode_utf16().count();
            last_start = offset;
        }
        while parents.last().is_some_and(|p| nodes[*p].close < open) {
            parents.pop();
        }
        let parent = parents.last().copied();
        parents.push(nodes.len());
        nodes.push(Node {
            id: position.to_string(),
            kind,
            constructor,
            open,
            close,
            start,
            line,
            column: source[line_start..token.start].encode_utf16().count() + 1,
            symbol: source[..token.start].encode_utf16().count(),
            component: custom.contains(kind) && !scope.external.contains(kind),
            parent,
            args: args(&tokens, open),
            spec: CATALOG
                .widgets
                .get(&format!("{kind}.{constructor}"))
                .or_else(|| CATALOG.widgets.get(kind))
                .unwrap_or(&EMPTY),
        });
    }
    Ok((tokens, nodes))
}

struct Property<'a> {
    arg: Option<Arg<'a>>,
    editable: bool,
    outer_exists: bool,
    nested: Option<(usize, Vec<Arg<'a>>)>,
}

fn property<'a>(node: &Node<'a>, item: &Field, tokens: &[Token<'a>]) -> Property<'a> {
    if let Some((name, kind)) = &item.nest {
        let outer = node.args.iter().find(|arg| arg.name == Some(name));
        let nested = outer.and_then(|outer| {
            let mut start = outer.expression;
            if matches!(at(tokens, start), "const" | "new") {
                start += 1;
            }
            if at(tokens, start + 1) == "." && at(tokens, start + 2) == kind {
                start += 2;
            }
            let open = start + 1;
            (at(tokens, start) == kind
                && at(tokens, open) == "("
                && tokens[open].close == Some(outer.span.last))
            .then(|| (open, args(tokens, open)))
        });
        return Property {
            arg: nested.as_ref().and_then(|(_, args)| {
                args.iter()
                    .copied()
                    .find(|arg| arg.name == Some(&item.name))
            }),
            editable: outer.is_none() || nested.is_some(),
            outer_exists: outer.is_some(),
            nested,
        };
    }
    let arg = node
        .args
        .iter()
        .copied()
        .find(|arg| arg.name == (!item.positional).then_some(item.name.as_str()));
    let conflict = node.kind == "Container"
        && item.name == "color"
        && node.args.iter().any(|arg| arg.name == Some("decoration"));
    Property {
        arg,
        editable: !conflict && (!item.positional || arg.is_some()),
        outer_exists: false,
        nested: None,
    }
}

struct Member<'a> {
    index: usize,
    items: Vec<Span>,
    list: Option<usize>,
    slot: Option<&'a str>,
}

fn member<'a>(
    node: &Node<'_>,
    parent: Option<&'a Node<'_>>,
    tokens: &[Token<'_>],
) -> Option<Member<'a>> {
    let parent = parent?;
    for (slot, multiple) in parent.slots() {
        let Some((index, arg)) = parent
            .args
            .iter()
            .copied()
            .enumerate()
            .find(|(_, arg)| arg.name == Some(slot))
        else {
            continue;
        };
        if !multiple && arg.expression == node.start && arg.span.last == node.close {
            return Some(Member {
                index,
                items: parent.args.iter().map(|arg| arg.span).collect(),
                list: None,
                slot: Some(slot),
            });
        }
        if *multiple {
            if let Some(list) = list_for(tokens, arg) {
                if let Some(index) = list
                    .items
                    .iter()
                    .position(|item| item.first == node.start && item.last == node.close)
                {
                    return Some(Member {
                        index,
                        items: list.items,
                        list: Some(list.open),
                        slot: Some(slot),
                    });
                }
            }
        }
    }
    None
}

fn slot_choices(node: &Node<'_>, slot: &str) -> Vec<&'static str> {
    match (node.kind, slot) {
        ("Scaffold", "appBar") => vec!["AppBar"],
        ("NavigationBar", "destinations") => vec!["NavigationDestination"],
        _ => CATALOG
            .palette
            .iter()
            .map(|(name, _)| name.as_str())
            .filter(|name| !matches!(*name, "AppBar" | "NavigationDestination"))
            .collect(),
    }
}

#[cfg(test)]
pub fn outline(source: &str) -> Result<Value> {
    outline_in(source, &Scope::default())
}

fn modifier_children<'a>(tokens: &[Token<'_>], nodes: &'a [Node<'_>]) -> Vec<Option<&'a str>> {
    let mut counts = vec![0usize; nodes.len()];
    let mut children = vec![None; nodes.len()];
    for node in nodes {
        let Some(index) = node.parent else { continue };
        counts[index] += 1;
        let parent = &nodes[index];
        if (parent.component || crate::modifier_kind::is_modifier(parent.kind))
            && parent.args.iter().any(|arg| {
                arg.name == Some("child")
                    && library::matches_widget(
                        tokens,
                        Span {
                            first: arg.expression,
                            ..arg.span
                        },
                        node,
                    )
            })
        {
            children[index] = Some(node.id.as_str());
        }
    }
    for (child, count) in children.iter_mut().zip(counts) {
        if count != 1 {
            *child = None;
        }
    }
    children
}

pub(crate) fn outline_in(source: &str, scope: &Scope) -> Result<Value> {
    let (tokens, nodes) = inspect_in(source, scope)?;
    let modifiers = modifier_children(&tokens, &nodes);
    let mut owners = components::owners(source, &tokens, scope);
    for owner in &mut owners {
        if let Some((start, end)) = owner["buildStart"].as_u64().zip(owner["buildEnd"].as_u64()) {
            owner["widget"] = nodes
                .iter()
                .find(|node| start <= node.symbol as u64 && (node.symbol as u64) < end)
                .map_or(Value::Null, |node| json!(node.id));
        }
    }
    let mut component_types = scope.names.clone();
    component_types.extend(
        owners
            .iter()
            .filter_map(|owner| owner["name"].as_str().map(str::to_owned)),
    );
    let mut component_types: Vec<_> = component_types.into_iter().collect();
    component_types.sort();
    let output: Vec<_> = nodes.iter().zip(modifiers).map(|(node, modifier)| {
        let properties: Vec<_> = node.spec.fields.iter().map(|item| {
            let context = property(node, item, &tokens);
            let span = context.arg.map(|arg| Span { first: arg.expression, ..arg.span });
            let mut entry = describe_property(&tokens, source, item, span, context.editable, 0);
            entry["name"] = json!(item.name);
            entry["type"] = json!(item.kind);
            entry["options"] = json!(item.options);
            entry["asset"] = json!(item.asset);
            let (editable, present, removable) = properties::access(node, &tokens, &item.name);
            entry["present"] = json!(present);
            entry["codeEditable"] = json!(editable);
            entry["designEditable"] = json!(context.editable);
            entry["removable"] = json!(removable);
            entry
        }).collect();
        let arguments: Vec<_> = node.args.iter().filter_map(|arg| {
            let name = arg.name?;
            if node.spec.fields.iter().any(|field| field.nest.is_none() && field.name == name) { return None; }
            Some(json!({"name":name,"type":"expression","expression":arg.expression(&tokens, source).chars().take(200).collect::<String>(),
                "editable":false,"codeEditable":true,"present":true,"removable":true}))
        }).collect();
        let slots: Vec<_> = node.slots().iter().filter(|(name, multiple)| {
            node.args.iter().copied().find(|arg| arg.name == Some(name))
                .is_none_or(|arg| if *multiple { list_for(&tokens, arg).is_some() } else { empty_child(&tokens, arg) })
        }).map(|(name, _)| name).collect();
        let choices: HashMap<_, _> = slots.iter().map(|name| (name, slot_choices(node, name))).collect();
        let parent = node.parent.map(|index| &nodes[index]);
        let membership = member(node, parent, &tokens);
        let typed_app_bar = node.kind == "AppBar" && parent.is_some_and(|parent| parent.kind == "Scaffold");
        let label = properties.iter().find(|item| matches!(item["name"].as_str(), Some("text" | "label")))
            .map(|item| &item["value"]);
        let kind = if node.constructor.is_empty() { node.kind.to_owned() } else { format!("{}.{}", node.kind, node.constructor) };
        let owner = owners.iter().find(|owner| owner["symbol"].as_u64().unwrap_or(u64::MAX) <= node.symbol as u64 && (node.symbol as u64) < owner["end"].as_u64().unwrap_or_default());
        json!({ "id": node.id, "type": kind, "line": node.line, "column": node.column,
            "symbol":node.symbol,"component":node.component,
            "ownerComponent":owner,
            "parent": parent.map(|parent| &parent.id), "label": label,
            "modifierChild": modifier, "libraryWrappable": library::can_wrap(&tokens, node),
            "slots": slots, "slotChoices": choices, "properties": properties, "arguments":arguments, "removable": membership.is_some(),
            "movable": membership.as_ref().is_some_and(|entry| entry.list.is_some()),
            "listSlot": membership.as_ref().and_then(|entry| entry.slot), "wrappable": !typed_app_bar, "libraryChildren": !CATALOG.widgets.contains_key(node.kind) || !slots.is_empty() })
    }).collect();
    Ok(
        json!({ "palette": CATALOG.palette.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        "wrappers": CATALOG.wrappers.iter().map(|(name, _)| name).collect::<Vec<_>>(), "nodes": output,
        "componentScopeTruncated":scope.truncated,"components":owners,"componentTypes":component_types }),
    )
}

fn literal_editable(expression: &str, item: &Field) -> bool {
    decode_literal(expression, item).is_some()
        || item.kind == "color" && NAMED_COLOR.is_match(expression)
}

fn describe_property(
    tokens: &[Token<'_>],
    source: &str,
    item: &Field,
    span: Option<Span>,
    editable: bool,
    depth: usize,
) -> Value {
    let span = span.map(|span| {
        if depth > 0 {
            enclosed(tokens, span)
        } else {
            span
        }
    });
    let expression = span.map(|span| span.text(tokens, source));
    let value = expression.and_then(|expr| decode_literal(expr, item));
    let constant =
        item.kind == "color" && expression.is_some_and(|expr| NAMED_COLOR.is_match(expr));
    let branch = span
        .filter(|_| editable && depth < MAX_BRANCH_DEPTH)
        .and_then(|span| conditional(tokens, span))
        .map(|branch| {
            json!({
                "condition": branch.condition.text(tokens, source).chars().take(200).collect::<String>(),
                "then": describe_property(tokens, source, item, Some(branch.when_true), editable, depth + 1),
                "else": describe_property(tokens, source, item, Some(branch.when_false), editable, depth + 1),
            })
        });
    json!({
        "editable": item.kind!="expression" && editable && (expression.is_none() || value.is_some() || constant),
        "value": value,
        "expression": expression.map(|expr| expr.chars().take(200).collect::<String>()),
        "conditional": branch,
    })
}

fn conditional_leaf(tokens: &[Token<'_>], arg: Arg<'_>, path: &str) -> Option<Span> {
    let mut span = Span {
        first: arg.expression,
        ..arg.span
    };
    for (depth, branch) in path.split('/').enumerate() {
        if depth >= MAX_BRANCH_DEPTH {
            return None;
        }
        let branches = conditional(tokens, span)?;
        span = match branch {
            "then" => branches.when_true,
            "else" => branches.when_false,
            _ => return None,
        };
    }
    Some(enclosed(tokens, span))
}

fn replace(source: &str, start: usize, end: usize, value: &str) -> String {
    let mut result = String::with_capacity(source.len() - (end - start) + value.len());
    result.push_str(&source[..start]);
    result.push_str(value);
    result.push_str(&source[end..]);
    result
}

fn append(
    source: &str,
    tokens: &[Token<'_>],
    open: usize,
    last: Option<Span>,
    value: &str,
) -> String {
    match last {
        None => replace(source, tokens[open].end, tokens[open].end, value),
        Some(item) => match item.comma {
            Some(comma) => replace(
                source,
                tokens[comma].end,
                tokens[comma].end,
                &format!(" {value},"),
            ),
            None => replace(
                source,
                tokens[item.last].end,
                tokens[item.last].end,
                &format!(", {value}"),
            ),
        },
    }
}

fn constructor(tokens: &[Token<'_>], arg: Arg<'_>, name: &[&str]) -> Option<usize> {
    let mut start = arg.expression;
    if matches!(at(tokens, start), "const" | "new") {
        start += 1;
    }
    let open = start + name.len();
    let matches = name
        .iter()
        .enumerate()
        .all(|(offset, part)| at(tokens, start + offset) == *part);
    (matches && at(tokens, open) == "(" && tokens[open].close == Some(arg.span.last))
        .then_some(open)
}

fn named<'a>(arguments: &[Arg<'a>], name: &str) -> Option<Arg<'a>> {
    arguments.iter().copied().find(|arg| arg.name == Some(name))
}

fn set_named(source: &str, tokens: &[Token<'_>], open: usize, name: &str, value: &str) -> String {
    let arguments = args(tokens, open);
    if let Some(arg) = named(&arguments, name) {
        return replace(
            source,
            tokens[arg.expression].start,
            tokens[arg.span.last].end,
            value,
        );
    }
    append(
        source,
        tokens,
        open,
        arguments.last().map(|arg| arg.span),
        &format!("{name}: {value}"),
    )
}

fn theme_field(name: &str) -> Field {
    Field {
        name: name.into(),
        kind: if name == "fontFamily" {
            "string"
        } else {
            "color"
        }
        .into(),
        positional: false,
        prefix: None,
        options: None,
        nest: None,
        min: None,
        max: None,
        asset: None,
    }
}

fn theme_value(
    source: &str,
    tokens: &[Token<'_>],
    arguments: &[Arg<'_>],
    name: &str,
    editable: bool,
) -> Value {
    let expression = named(arguments, name).map(|arg| arg.expression(tokens, source));
    let value = expression.and_then(|expression| decode_literal(expression, &theme_field(name)));
    json!({"value":value,"expression":expression.map(|text| text.chars().take(180).collect::<String>()),"editable":editable})
}

fn can_add_theme(tokens: &[Token<'_>], nodes: &[Node<'_>], node: &Node<'_>) -> bool {
    let mut current = Some(node);
    while let Some(widget) = current {
        if tokens[widget.start].text == "const" {
            return false;
        }
        current = widget.parent.map(|index| &nodes[index]);
    }
    true
}

pub fn themes(source: &str) -> Result<Value> {
    let (tokens, nodes) = inspect(source)?;
    Ok(json!(nodes.iter().filter(|node| node.kind == "MaterialApp").map(|node| {
        let theme = named(&node.args, "theme");
        let open = theme.and_then(|arg| constructor(&tokens, arg, &["ThemeData"]));
        let editable = open.is_some() || (theme.is_none() && can_add_theme(&tokens, &nodes, node));
        let arguments = open.map(|open| args(&tokens, open)).unwrap_or_default();
        let scheme = named(&arguments, "colorScheme");
        let scheme_open = scheme.and_then(|arg| constructor(&tokens, arg, &["ColorScheme", ".", "fromSeed"]));
        let colors = scheme_open.map(|open| args(&tokens, open)).unwrap_or_default();
        json!({"id":node.id,"line":node.line,"editable":editable,
            "seedColor":theme_value(source,&tokens,&colors,"seedColor", editable && (scheme.is_none() || scheme_open.is_some())),
            "scaffoldBackgroundColor":theme_value(source,&tokens,&arguments,"scaffoldBackgroundColor",editable),
            "fontFamily":theme_value(source,&tokens,&arguments,"fontFamily",editable)})
    }).collect::<Vec<_>>()))
}

pub fn edit_theme(source: &str, input: &Value) -> Result<String> {
    let id = input["id"].as_str().unwrap_or_default();
    let mut result = source.to_owned();
    for name in ["seedColor", "scaffoldBackgroundColor", "fontFamily"] {
        let Some(value) = input[name]
            .as_str()
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let value = value.trim();
        if value.len() > 100 || value.chars().any(char::is_control) {
            return Err(Problem::new(400, "Choose a valid theme value."));
        }
        let (tokens, nodes) = inspect(&result)?;
        let node = nodes
            .iter()
            .find(|node| node.id == id && node.kind == "MaterialApp")
            .ok_or_else(|| Problem::new(409, "Select a MaterialApp from the current source."))?;
        let theme = named(&node.args, "theme");
        let open = theme.and_then(|arg| constructor(&tokens, arg, &["ThemeData"]));
        let value = encode_literal(&json!(value), &theme_field(name))?;
        let args_for_new_theme = if name == "seedColor" {
            format!("colorScheme: ColorScheme.fromSeed(seedColor: {value})")
        } else {
            format!("{name}: {value}")
        };
        if let Some(open) = open {
            if name == "seedColor" {
                let arguments = args(&tokens, open);
                if let Some(scheme) = named(&arguments, "colorScheme") {
                    let scheme_open =
                        constructor(&tokens, scheme, &["ColorScheme", ".", "fromSeed"])
                            .ok_or_else(|| {
                                Problem::new(422, "Edit this ColorScheme expression in Dart.")
                            })?;
                    result = set_named(&result, &tokens, scheme_open, name, &value);
                } else {
                    let brightness = named(&arguments, "brightness")
                        .map(|arg| format!(", brightness: {}", arg.expression(&tokens, &result)))
                        .unwrap_or_default();
                    result = set_named(
                        &result,
                        &tokens,
                        open,
                        "colorScheme",
                        &format!("ColorScheme.fromSeed(seedColor: {value}{brightness})"),
                    );
                }
            } else {
                result = set_named(&result, &tokens, open, name, &value);
            }
        } else if theme.is_none() && can_add_theme(&tokens, &nodes, node) {
            result = set_named(
                &result,
                &tokens,
                node.open,
                "theme",
                &format!("ThemeData({args_for_new_theme})"),
            );
        } else {
            return Err(Problem::new(
                422,
                "This MaterialApp theme needs a Dart edit.",
            ));
        }
    }
    Ok(result)
}

#[cfg(test)]
pub fn snippet(source: &str, id: &str) -> Result<Value> {
    snippet_in(source, id, &Scope::default())
}

pub(crate) fn snippet_in(source: &str, id: &str, scope: &Scope) -> Result<Value> {
    let (tokens, nodes) = inspect_in(source, scope)?;
    let node = nodes
        .iter()
        .find(|node| node.id == id)
        .ok_or_else(|| Problem::new(409, "Select the widget again after the source refreshes."))?;
    Ok(
        json!({"code": &source[tokens[node.start].start..tokens[node.close].end], "type": node.kind}),
    )
}

fn pasted_widget(code: &str) -> Result<(&str, &str)> {
    let invalid = || {
        Problem::new(
            422,
            "Copy a complete Dart widget constructor before pasting.",
        )
    };
    let tokens = tokenize(code)?;
    let mut index = usize::from(matches!(at(&tokens, 0), "const" | "new"));
    let identifier = |value: &str| {
        !value.is_empty()
            && value.bytes().enumerate().all(|(i, byte)| {
                byte.is_ascii_alphabetic()
                    || matches!(byte, b'_' | b'$')
                    || i > 0 && byte.is_ascii_digit()
            })
    };
    let mut kind = "";
    loop {
        let name = at(&tokens, index);
        if !identifier(name) {
            return Err(invalid());
        }
        if name.starts_with(char::is_uppercase) {
            kind = name;
        }
        index += 1;
        let generic = type_end(&tokens, index, tokens.len());
        if generic > index {
            index = generic + 1;
        }
        if at(&tokens, index) != "." {
            break;
        }
        index += 1;
    }
    if kind.is_empty()
        || at(&tokens, index) != "("
        || tokens[index].close != tokens.len().checked_sub(1)
        || tokens
            .iter()
            .any(|token| matches!(token.text, "(" | "[" | "{") && token.close.is_none())
    {
        return Err(invalid());
    }
    Ok((&code[tokens[0].start..tokens.last().unwrap().end], kind))
}

fn insert_widget(
    source: &str,
    tokens: &[Token<'_>],
    node: &Node<'_>,
    parent: Option<&Node<'_>>,
    slot: &str,
    side: &str,
    code: &str,
) -> Result<String> {
    let (code, kind) = pasted_widget(code)?;
    let membership = member(node, parent, tokens);
    let has_slot = node.slots().iter().any(|(name, multiple)| {
        node.args
            .iter()
            .copied()
            .find(|arg| arg.name == Some(name))
            .is_none_or(|arg| {
                if *multiple {
                    list_for(tokens, arg).is_some()
                } else {
                    empty_child(tokens, arg)
                }
            })
    });
    if matches!(side, "before" | "after") || side.is_empty() && slot.is_empty() && !has_slot {
        let current = membership
            .filter(|entry| entry.list.is_some())
            .ok_or_else(|| {
                Problem::new(
                    422,
                    "Select a children list or an empty child slot for this widget.",
                )
            })?;
        let item = current.items[current.index];
        if parent.is_some_and(|parent| parent.kind == "NavigationBar")
            && current.slot == Some("destinations")
            && kind != "NavigationDestination"
        {
            return Err(Problem::new(
                422,
                "NavigationBar.destinations requires NavigationDestination widgets.",
            ));
        }
        return Ok(if side == "before" {
            replace(
                source,
                tokens[item.first].start,
                tokens[item.first].start,
                &format!("{code}, "),
            )
        } else {
            append(source, tokens, current.list.unwrap(), Some(item), code)
        });
    }
    if !matches!(side, "" | "inside") {
        return Err(Problem::new(
            400,
            "Choose before, after or inside for the destination.",
        ));
    }
    let (slot, multiple) = node
        .slots()
        .iter()
        .find(|(name, _)| name == slot)
        .or_else(|| {
            slot.is_empty()
                .then(|| {
                    node.slots().iter().find(|(name, multiple)| {
                        node.args
                            .iter()
                            .copied()
                            .find(|arg| arg.name == Some(name))
                            .is_none_or(|arg| {
                                if *multiple {
                                    list_for(tokens, arg).is_some()
                                } else {
                                    empty_child(tokens, arg)
                                }
                            })
                    })
                })
                .flatten()
        })
        .ok_or_else(|| {
            Problem::new(
                422,
                "Select a children list or an empty child slot for this widget.",
            )
        })?;
    if node.kind == "Scaffold" && slot == "appBar" && !matches!(kind, "AppBar" | "PreferredSize") {
        return Err(Problem::new(
            422,
            "Scaffold.appBar requires an AppBar or PreferredSize widget.",
        ));
    }
    if node.kind == "NavigationBar" && slot == "destinations" && kind != "NavigationDestination" {
        return Err(Problem::new(
            422,
            "NavigationBar.destinations requires NavigationDestination widgets.",
        ));
    }
    let arg = node.args.iter().copied().find(|arg| arg.name == Some(slot));
    match arg {
        Some(arg) if !*multiple && empty_child(tokens, arg) => Ok(replace(
            source,
            tokens[arg.expression].start,
            tokens[arg.span.last].end,
            code,
        )),
        None => Ok(append(
            source,
            tokens,
            node.open,
            node.args.last().map(|arg| arg.span),
            &if *multiple {
                format!("{slot}: [{code}]")
            } else {
                format!("{slot}: {code}")
            },
        )),
        Some(arg) if *multiple => {
            let list = list_for(tokens, arg)
                .ok_or_else(|| Problem::new(422, "This children expression needs a Dart edit."))?;
            Ok(append(
                source,
                tokens,
                list.open,
                list.items.last().copied(),
                code,
            ))
        }
        _ => Err(Problem::new(
            422,
            "This child slot is already filled. Select an empty slot or a children list.",
        )),
    }
}

fn remove_range(
    node: &Node<'_>,
    parent: Option<&Node<'_>>,
    tokens: &[Token<'_>],
) -> Result<(usize, usize, String)> {
    let current = member(node, parent, tokens)
        .ok_or_else(|| Problem::new(422, "That change needs to be made in the code editor."))?;
    let span = current.items[current.index];
    // Required single-child constructors must remain valid after a cut or move.
    if current.list.is_none()
        && matches!(
            (parent.map(|node| node.kind), current.slot),
            (
                Some(
                    "Expanded"
                        | "Flexible"
                        | "Positioned"
                        | "SafeArea"
                        | "ElevatedButton"
                        | "FilledButton"
                        | "OutlinedButton"
                        | "TextButton"
                ),
                Some("child")
            ) | (Some("IconButton" | "NavigationDestination"), Some("icon"))
        )
    {
        let parent = parent.unwrap();
        let index = parent.start + usize::from(matches!(at(tokens, parent.start), "const" | "new"));
        let prefix = if at(tokens, index + 1) == "." {
            format!("{}.", at(tokens, index))
        } else {
            String::new()
        };
        return Ok((
            tokens[node.start].start,
            tokens[node.close].end,
            format!("const {prefix}SizedBox.shrink()"),
        ));
    }
    let start = if span.comma.is_none() && current.index > 0 {
        tokens[current.items[current.index - 1].comma.unwrap_or(span.first)].start
    } else {
        tokens[span.first].start
    };
    Ok((
        start,
        tokens[span.comma.unwrap_or(span.last)].end,
        String::new(),
    ))
}

#[cfg(test)]
pub fn edit(source: &str, command: &Value) -> Result<String> {
    edit_in(source, command, &Scope::default())
}

pub(crate) fn edit_in(source: &str, command: &Value, scope: &Scope) -> Result<String> {
    if !command.is_object() {
        return Err(Problem::new(400, "Choose a visual edit."));
    }
    let (tokens, nodes) = inspect_in(source, scope)?;
    let field = |name| command[name].as_str().unwrap_or_default();
    let node = nodes
        .iter()
        .find(|node| node.id == field("id"))
        .ok_or_else(|| Problem::new(409, "Select the widget again after the source refreshes."))?;
    let parent = node.parent.map(|index| &nodes[index]);
    let append_node = |value| {
        append(
            source,
            &tokens,
            node.open,
            node.args.last().map(|arg| arg.span),
            value,
        )
    };
    match field("operation") {
        "expression" => properties::edit_expression(source, &tokens, node, command),
        "paste" => insert_widget(
            source,
            &tokens,
            node,
            parent,
            field("slot"),
            field("side"),
            field("code"),
        ),
        "duplicate" => insert_widget(
            source,
            &tokens,
            node,
            parent,
            "",
            "after",
            &source[tokens[node.start].start..tokens[node.close].end],
        ),
        "property" => {
            let item = node
                .spec
                .fields
                .iter()
                .find(|item| item.name == field("name"))
                .ok_or_else(|| Problem::new(400, "This property is unavailable for the widget."))?;
            let context = property(node, item, &tokens);
            let expression = context.arg.map(|arg| arg.expression(&tokens, source));
            let encode = |previous: Option<&str>| -> Result<String> {
                let mut value = encode_literal(&command["value"], item)?;
                if item.kind == "spacing" {
                    let namespace = previous.map_or_else(
                        || node.namespace(&tokens),
                        |expression| {
                            SPACING
                                .captures(expression)
                                .and_then(|captures| captures.name("namespace"))
                                .map_or_else(String::new, |prefix| prefix.as_str().to_owned())
                        },
                    );
                    value.insert_str("const ".len(), &namespace);
                }
                Ok(value)
            };
            if let Some(branch) = command.get("branch") {
                let leaf = context
                    .arg
                    .filter(|_| context.editable)
                    .and_then(|arg| conditional_leaf(&tokens, arg, branch.as_str()?))
                    .filter(|leaf| literal_editable(leaf.text(&tokens, source), item))
                    .ok_or_else(|| {
                        Problem::new(422, "Select an editable value in a conditional branch.")
                    })?;
                let value = encode(Some(leaf.text(&tokens, source)))?;
                return Ok(replace(
                    source,
                    tokens[leaf.first].start,
                    tokens[leaf.last].end,
                    &value,
                ));
            }
            if !context.editable || expression.is_some_and(|expr| !literal_editable(expr, item)) {
                return Err(Problem::new(
                    422,
                    "Edit this Dart expression in the code editor.",
                ));
            }
            let value = encode(expression)?;
            if let Some(arg) = context.arg {
                return Ok(replace(
                    source,
                    tokens[arg.expression].start,
                    tokens[arg.span.last].end,
                    &value,
                ));
            }
            if let Some((name, kind)) = &item.nest {
                if !context.outer_exists {
                    return Ok(append_node(&format!(
                        "{name}: {kind}({}: {value})",
                        item.name
                    )));
                }
            }
            let value = format!("{}: {value}", item.name);
            Ok(match context.nested {
                Some((open, args)) => append(
                    source,
                    &tokens,
                    open,
                    args.last().map(|arg| arg.span),
                    &value,
                ),
                None => append_node(&value),
            })
        }
        "add" => {
            let slot = field("slot");
            let widget = field("widget");
            let error = || Problem::new(400, "Select an available widget and slot.");
            let (_, multiple) = node
                .slots()
                .iter()
                .find(|(name, _)| name == slot)
                .ok_or_else(error)?;
            if !slot_choices(node, slot).contains(&widget) {
                return Err(error());
            }
            let (_, value) = CATALOG
                .palette
                .iter()
                .find(|(name, _)| name == widget)
                .ok_or_else(error)?;
            let Some(arg) = node.args.iter().copied().find(|arg| arg.name == Some(slot)) else {
                return Ok(append_node(&if *multiple {
                    format!("{slot}: [{value}]")
                } else {
                    format!("{slot}: {value}")
                }));
            };
            if !*multiple && empty_child(&tokens, arg) {
                return Ok(replace(
                    source,
                    tokens[arg.expression].start,
                    tokens[arg.span.last].end,
                    value,
                ));
            }
            let list = multiple
                .then(|| list_for(&tokens, arg))
                .flatten()
                .ok_or_else(|| {
                    Problem::new(
                        422,
                        "The slot is filled by a Dart expression. Edit it in Code.",
                    )
                })?;
            Ok(append(
                source,
                &tokens,
                list.open,
                list.items.last().copied(),
                value,
            ))
        }
        "wrap" => {
            let (_, wrapper) = CATALOG
                .wrappers
                .iter()
                .find(|(name, _)| name == field("widget"))
                .ok_or_else(|| Problem::new(400, "Select a supported wrapper."))?;
            if node.kind == "AppBar" && parent.is_some_and(|parent| parent.kind == "Scaffold") {
                return Err(Problem::new(
                    422,
                    "Scaffold.appBar requires an AppBar or another PreferredSizeWidget.",
                ));
            }
            let start = tokens[node.start].start;
            let end = tokens[node.close].end;
            let prefix = if tokens[node.start].text == "const" {
                "const "
            } else {
                ""
            };
            Ok(replace(
                source,
                start,
                end,
                &format!("{prefix}{wrapper}{})", &source[start..end]),
            ))
        }
        operation => {
            let unsupported =
                || Problem::new(422, "That change needs to be made in the code editor.");
            let current = member(node, parent, &tokens).ok_or_else(unsupported)?;
            if operation == "remove" {
                let (start, end, value) = remove_range(node, parent, &tokens)?;
                return Ok(replace(source, start, end, &value));
            }
            if operation != "move" {
                return Err(unsupported());
            }
            let target = nodes
                .iter()
                .find(|node| node.id == field("target"))
                .ok_or_else(unsupported)?;
            if node.start <= target.start && target.close <= node.close {
                return Err(Problem::new(
                    422,
                    "A widget cannot be moved into itself or one of its children.",
                ));
            }
            let target_parent = target.parent.map(|index| &nodes[index]);
            let target_member = member(target, target_parent, &tokens);
            if current.list.is_none()
                || target_member
                    .as_ref()
                    .is_none_or(|other| other.list != current.list)
                || !matches!(field("side"), "before" | "after")
            {
                let code = &source[tokens[node.start].start..tokens[node.close].end];
                let (start, end, value) = remove_range(node, parent, &tokens)?;
                let output = replace(source, start, end, &value);
                let offset = tokens[target.start].start;
                let offset = if end <= offset {
                    offset - (end - start) + value.len()
                } else {
                    offset
                };
                let target_id = output[..offset].encode_utf16().count().to_string();
                return edit_in(
                    &output,
                    &json!({"id":target_id,"operation":"paste","side":field("side"),"slot":field("slot"),"code":code}),
                    scope,
                );
            }
            let reorder = || Problem::new(422, "Reorder widgets within the same children list.");
            let other = nodes
                .iter()
                .find(|node| node.id == field("target"))
                .and_then(|node| member(node, parent, &tokens))
                .filter(|other| other.list == current.list)
                .ok_or_else(reorder)?;
            let after = match field("side") {
                "before" => false,
                "after" => true,
                _ => return Err(reorder()),
            };
            let (from, to) = (current.index, other.index);
            if from == to {
                return Ok(source.to_owned());
            }
            let first = from.min(to);
            let affected = &current.items[first..=from.max(to)];
            let mut values: Vec<_> = affected
                .iter()
                .map(|item| item.text(&tokens, source))
                .collect();
            let moved = values.remove(from - first);
            values.insert(
                to - first - usize::from(from < to) + usize::from(after),
                moved,
            );
            let mut output = source.to_owned();
            for (span, value) in affected.iter().zip(values).rev() {
                output.replace_range(tokens[span.first].start..tokens[span.last].end, value);
            }
            Ok(output)
        }
    }
}
