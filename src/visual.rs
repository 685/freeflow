use crate::files::{Problem, Result};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

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
    Regex::new(r"^(?:const\s+)?EdgeInsets\.all\(\s*([0-9]+(?:\.[0-9]+)?)\s*\)$").unwrap()
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
            .get(1)?
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

struct Node<'a> {
    id: String,
    kind: &'a str,
    constructor: &'a str,
    open: usize,
    close: usize,
    start: usize,
    line: usize,
    parent: Option<usize>,
    args: Vec<Arg<'a>>,
    spec: &'static Spec,
}

fn inspect(source: &str) -> Result<(Vec<Token<'_>>, Vec<Node<'_>>)> {
    let tokens = tokenize(source)?;
    let custom: HashSet<_> = tokens
        .windows(4)
        .filter(|w| {
            w[0].text == "class"
                && w[2].text == "extends"
                && matches!(
                    w[3].text,
                    "StatelessWidget" | "StatefulWidget" | "ConsumerWidget" | "HookWidget"
                )
        })
        .map(|w| w[1].text)
        .collect();
    let mut nodes: Vec<Node<'_>> = vec![];
    let mut parents: Vec<usize> = vec![];
    let (mut last_start, mut line, mut position) = (0, 1, 0);
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
        if matches!(at(&tokens, close + 1), ":" | "{" | "=>")
            || (at(&tokens, close + 1) == "=" && at(&tokens, close + 2) == ">")
            || (custom.contains(kind) && at(&tokens, open + 1) == "{")
        {
            continue;
        }
        let start = if matches!(previous, "const" | "new") {
            i - 1
        } else {
            i
        };
        let offset = tokens[start].start;
        if offset >= last_start {
            let text = &source[last_start..offset];
            line += text.bytes().filter(|b| *b == b'\n').count();
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
            parent,
            args: args(&tokens, open),
            spec: CATALOG.widgets.get(kind).unwrap_or(&EMPTY),
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
    for (slot, multiple) in &parent.spec.slots {
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
                slot: None,
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

pub fn outline(source: &str) -> Result<Value> {
    let (tokens, nodes) = inspect(source)?;
    let output: Vec<_> = nodes.iter().map(|node| {
        let properties: Vec<_> = node.spec.fields.iter().map(|item| {
            let context = property(node, item, &tokens);
            let expression = context.arg.map(|arg| arg.expression(&tokens, source));
            let value = expression.and_then(|expr| decode_literal(expr, item));
            let constant = item.kind == "color" && expression.is_some_and(|expr| NAMED_COLOR.is_match(expr));
            json!({ "name": item.name, "type": item.kind, "options": item.options,
                "editable": context.editable && (expression.is_none() || value.is_some() || constant),
                "value": value, "expression": expression.map(|expr| expr.chars().take(200).collect::<String>()) })
        }).collect();
        let slots: Vec<_> = node.spec.slots.iter().filter(|(name, multiple)| {
            node.args.iter().copied().find(|arg| arg.name == Some(name))
                .is_none_or(|arg| *multiple && list_for(&tokens, arg).is_some())
        }).map(|(name, _)| name).collect();
        let choices: HashMap<_, _> = slots.iter().map(|name| (name, slot_choices(node, name))).collect();
        let parent = node.parent.map(|index| &nodes[index]);
        let membership = member(node, parent, &tokens);
        let typed_app_bar = node.kind == "AppBar" && parent.is_some_and(|parent| parent.kind == "Scaffold");
        let label = properties.iter().find(|item| matches!(item["name"].as_str(), Some("text" | "label")))
            .map(|item| &item["value"]);
        let kind = if node.constructor.is_empty() { node.kind.to_owned() } else { format!("{}.{}", node.kind, node.constructor) };
        json!({ "id": node.id, "type": kind, "line": node.line, "parent": parent.map(|parent| &parent.id), "label": label,
            "slots": slots, "slotChoices": choices, "properties": properties, "removable": membership.is_some(),
            "movable": membership.as_ref().is_some_and(|entry| entry.list.is_some()),
            "listSlot": membership.as_ref().and_then(|entry| entry.slot), "wrappable": !typed_app_bar })
    }).collect();
    Ok(
        json!({ "palette": CATALOG.palette.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        "wrappers": CATALOG.wrappers.iter().map(|(name, _)| name).collect::<Vec<_>>(), "nodes": output }),
    )
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

pub fn edit(source: &str, command: &Value) -> Result<String> {
    if !command.is_object() {
        return Err(Problem::new(400, "Choose a visual edit."));
    }
    let (tokens, nodes) = inspect(source)?;
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
        "property" => {
            let item = node
                .spec
                .fields
                .iter()
                .find(|item| item.name == field("name"))
                .ok_or_else(|| Problem::new(400, "This property is unavailable for the widget."))?;
            let context = property(node, item, &tokens);
            let expression = context.arg.map(|arg| arg.expression(&tokens, source));
            if !context.editable
                || expression.is_some_and(|expr| {
                    decode_literal(expr, item).is_none()
                        && !(item.kind == "color" && NAMED_COLOR.is_match(expr))
                })
            {
                return Err(Problem::new(
                    422,
                    "Edit this Dart expression in the code editor.",
                ));
            }
            let value = encode_literal(&command["value"], item)?;
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
                .spec
                .slots
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
            let span = current.items[current.index];
            if operation == "remove" {
                let start = if span.comma.is_none() && current.index > 0 {
                    tokens[current.items[current.index - 1].comma.unwrap_or(span.first)].start
                } else {
                    tokens[span.first].start
                };
                return Ok(replace(
                    source,
                    start,
                    tokens[span.comma.unwrap_or(span.last)].end,
                    "",
                ));
            }
            if operation != "move" || current.list.is_none() {
                return Err(unsupported());
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
