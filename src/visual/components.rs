use super::*;
use crate::{
    files,
    projects::{self, Project, Projects},
};
use std::collections::VecDeque;

const BASES: &[&str] = &[
    "Widget",
    "StatelessWidget",
    "StatefulWidget",
    "ConsumerWidget",
    "ConsumerStatefulWidget",
    "HookWidget",
    "HookConsumerWidget",
    "InheritedWidget",
    "ProxyWidget",
    "RenderObjectWidget",
    "SingleChildRenderObjectWidget",
    "MultiChildRenderObjectWidget",
];

#[derive(Default)]
pub(crate) struct Scope {
    pub(super) names: HashSet<String>,
    pub truncated: bool,
    pub(super) external: HashSet<String>,
}

pub(super) struct Class {
    pub(super) name: String,
    base: String,
    pub(super) name_token: usize,
    pub(super) open: usize,
    pub(super) close: usize,
    argument: Option<(String, usize)>,
}

// Declaration headers may be followed by `extends` or `implements`, unlike
// generic constructor expressions handled by `type_end`.
pub(super) fn declaration_type_end(tokens: &[Token<'_>], start: usize, limit: usize) -> usize {
    if at(tokens, start) != "<" {
        return start;
    }
    let mut depth = 0usize;
    for index in start..limit {
        match at(tokens, index) {
            "<" => depth += 1,
            ">" => {
                depth -= 1;
                if depth == 0 {
                    return index;
                }
            }
            _ => {}
        }
    }
    start
}

pub(super) fn classes(tokens: &[Token<'_>]) -> Vec<Class> {
    let mut result = vec![];
    let mut index = 0;
    while index < tokens.len() {
        if at(tokens, index) != "class" || at(tokens, index + 1).is_empty() {
            index += 1;
            continue;
        }
        let name = at(tokens, index + 1);
        let mut cursor = index + 2;
        let generic = declaration_type_end(tokens, cursor, tokens.len());
        if generic > cursor {
            cursor = generic + 1;
        }
        let mut base = "";
        let mut argument = None;
        while cursor < tokens.len() && !matches!(at(tokens, cursor), "{" | ";") {
            if at(tokens, cursor) == "extends" {
                let start = cursor + 1;
                let base_token = if at(tokens, start + 1) == "." {
                    start + 2
                } else {
                    start
                };
                base = at(tokens, base_token);
                if at(tokens, base_token + 1) == "<" {
                    argument = Some((at(tokens, base_token + 2).to_owned(), base_token + 2));
                }
            }
            cursor += 1;
        }
        if at(tokens, cursor) == "{" {
            if let Some(close) = tokens[cursor].close {
                result.push(Class {
                    name: name.into(),
                    base: base.into(),
                    name_token: index + 1,
                    open: cursor,
                    close,
                    argument,
                });
                index = close + 1;
                continue;
            }
        }
        index = cursor + 1;
    }
    result
}

pub(super) fn add_names(classes: &[Class], names: &mut HashSet<String>) {
    loop {
        let mut changed = false;
        for item in classes {
            if BASES.contains(&item.base.as_str())
                || CATALOG.widgets.contains_key(&item.base)
                || names.contains(&item.base)
            {
                changed |= names.insert(item.name.clone());
            }
        }
        if !changed {
            break;
        }
    }
}

fn class_value(source: &str, tokens: &[Token<'_>], class: &Class) -> Value {
    let offset = tokens[class.name_token].start;
    let mut build = None;
    let mut cursor = class.open + 1;
    while cursor < class.close {
        if at(tokens, cursor) == "build" && at(tokens, cursor + 1) == "(" {
            if let Some(close) = tokens[cursor + 1].close {
                let body = close + 1;
                if at(tokens, body) == "{" {
                    build = tokens[body].close.map(|end| (body, end));
                } else if at(tokens, body) == "=" && at(tokens, body + 1) == ">" {
                    let mut end = body + 2;
                    while end < class.close && at(tokens, end) != ";" {
                        end = tokens[end].close.unwrap_or(end) + 1;
                    }
                    build = Some((body, end.min(class.close)));
                }
            }
        }
        if build.is_some() {
            break;
        }
        cursor = tokens[cursor].close.unwrap_or(cursor) + 1;
    }
    json!({"name":class.name,"symbol":source[..offset].encode_utf16().count(),
        "line":source[..offset].bytes().filter(|byte| *byte == b'\n').count()+1,
        "buildStart":build.map(|(start, _)| source[..tokens[start].start].encode_utf16().count()),
        "buildEnd":build.map(|(_, end)| source[..tokens[end].end].encode_utf16().count()),
        "end":source[..tokens[class.close].end].encode_utf16().count()})
}

pub(super) fn owners(source: &str, tokens: &[Token<'_>], scope: &Scope) -> Vec<Value> {
    let classes = classes(tokens);
    let mut names = scope.names.clone();
    add_names(&classes, &mut names);
    let mut result: Vec<_> = classes
        .iter()
        .filter(|class| names.contains(&class.name))
        .map(|class| class_value(source, tokens, class))
        .collect();
    for class in &classes {
        if !matches!(class.base.as_str(), "State" | "ConsumerState") {
            continue;
        }
        if let Some((name, token)) = &class.argument {
            if names.contains(name) {
                let mut owner = class_value(source, tokens, class);
                owner["name"] = json!(name);
                owner["symbol"] = json!(source[..tokens[*token].start].encode_utf16().count());
                result.push(owner);
            }
        }
    }
    result
}

pub(crate) fn component_at(source: &str, offset: u64, expected: &str) -> Result<Option<Value>> {
    let tokens = tokenize(source)?;
    Ok(classes(&tokens)
        .iter()
        .find(|class| {
            class.name == expected
                && source[..tokens[class.name_token].start]
                    .encode_utf16()
                    .count() as u64
                    <= offset
                && offset <= source[..tokens[class.close].end].encode_utf16().count() as u64
        })
        .map(|class| class_value(source, &tokens, class)))
}

struct Directive {
    uri: String,
    visible: bool,
}

fn directives(tokens: &[Token<'_>]) -> Vec<Directive> {
    let mut result = vec![];
    let mut cursor = 0;
    while cursor < tokens.len() {
        let kind = at(tokens, cursor);
        if matches!(kind, "import" | "export" | "part") {
            cursor += 1;
            while cursor < tokens.len() && at(tokens, cursor) != ";" {
                let value = at(tokens, cursor)
                    .strip_prefix('r')
                    .unwrap_or(at(tokens, cursor));
                if value.len() >= 2
                    && matches!(value.as_bytes()[0], b'\'' | b'"')
                    && value.as_bytes().last() == Some(&value.as_bytes()[0])
                {
                    let uri = &value[1..value.len() - 1];
                    if !uri.contains(['$', '\\', '\n', '\r']) {
                        result.push(Directive {
                            uri: uri.into(),
                            visible: kind != "import",
                        });
                    }
                }
                cursor += 1;
            }
        } else if let Some(close) = tokens[cursor].close {
            cursor = close;
        }
        cursor += 1;
    }
    result
}

pub(crate) fn decoded_uri(uri: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let mut input = uri.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = (input.next()? as char).to_digit(16)?;
            let low = (input.next()? as char).to_digit(16)?;
            bytes.push((high * 16 + low) as u8);
        } else {
            bytes.push(byte);
        }
    }
    let path = String::from_utf8(bytes).ok()?;
    (!path.contains(['\0', '\\'])).then_some(path)
}

fn relative_import(from: &str, uri: &str, package: &str) -> Option<String> {
    let uri = decoded_uri(uri)?;
    let uri = uri.as_str();
    let path = if let Some(uri) = uri.strip_prefix("package:") {
        let (name, relative) = uri.split_once('/')?;
        if name != package {
            return None;
        }
        format!("lib/{relative}")
    } else {
        if uri.contains(':') || uri.starts_with('/') {
            return None;
        }
        let parent = from.rsplit_once('/').map_or("", |(parent, _)| parent);
        format!("{parent}/{uri}")
    };
    let mut parts = vec![];
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    let path = parts.join("/");
    (path.ends_with(".dart") && files::relative_parts(&path).is_ok()).then_some(path)
}

impl Scope {
    pub(crate) fn for_source(
        projects: &Projects,
        project: &Project,
        path: &str,
        source: &str,
    ) -> Self {
        let mut scope = Self {
            external: crate::widget_library::external_names(projects, project),
            ..Self::default()
        };
        let package = projects::package_name(project).unwrap_or_default();
        let mut queue = VecDeque::from([(path.to_owned(), true, true)]);
        let mut seen = HashMap::new();
        let mut all = vec![];
        let mut visible = HashSet::new();
        let mut bytes = 0;
        while let Some((path, exposed, root)) = queue.pop_front() {
            if seen
                .get(&path)
                .is_some_and(|previous| *previous || !exposed)
            {
                continue;
            }
            if seen.len() >= 192 || bytes > 8 * 1024 * 1024 {
                scope.truncated = true;
                break;
            }
            seen.insert(path.clone(), exposed);
            let owned;
            let text = if root {
                source
            } else {
                let Some(file) = files::project_file(&project.root, &path, false)
                    .ok()
                    .and_then(|target| files::read_source(&target, &path).ok())
                else {
                    continue;
                };
                owned = file.content;
                &owned
            };
            bytes += text.len();
            let Ok(tokens) = tokenize(text) else {
                continue;
            };
            let declared = classes(&tokens);
            if exposed {
                visible.extend(declared.iter().map(|class| class.name.clone()));
            }
            all.extend(declared);
            for directive in directives(&tokens) {
                if let Some(relative) = relative_import(&path, &directive.uri, &package) {
                    queue.push_back((relative, root || exposed && directive.visible, false));
                }
            }
        }
        scope.names.extend(scope.external.clone());
        add_names(&all, &mut scope.names);
        scope.names.retain(|name| visible.contains(name));
        scope
    }
}
