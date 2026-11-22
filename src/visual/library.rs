use super::*;
use serde::{Deserialize, Serialize};

pub(crate) fn checked_expression(code: &str) -> Result<String> {
    properties::expression(&json!(code))
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Constructor {
    pub name: String,
    pub constant: bool,
    pub factory: bool,
    pub signature: String,
}

#[derive(Clone)]
pub(crate) struct Declaration {
    pub name: String,
    pub bases: Vec<String>,
    pub abstract_class: bool,
    pub line: usize,
    pub constructors: Vec<Constructor>,
}

#[derive(Clone)]
pub(crate) struct Export {
    pub uri: String,
    pub show: Vec<String>,
    pub hide: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct Unit {
    pub classes: Vec<Declaration>,
    pub exports: Vec<Export>,
    pub part: bool,
}

pub(crate) fn unit(source: &str) -> Result<Unit> {
    let tokens = tokenize(source)?;
    let mut exports = vec![];
    let mut part = false;
    let mut cursor = 0;
    while cursor < tokens.len() {
        if matches!(at(&tokens, cursor), "export" | "part") {
            if at(&tokens, cursor + 1) == "of" {
                part = true;
            }
            let literal = at(&tokens, cursor + 1)
                .strip_prefix('r')
                .unwrap_or(at(&tokens, cursor + 1));
            if literal.len() > 2
                && matches!(literal.as_bytes()[0], b'\'' | b'"')
                && literal.as_bytes().last() == Some(&literal.as_bytes()[0])
            {
                let uri = &literal[1..literal.len() - 1];
                if !uri.contains(['$', '\\', '\n', '\r']) {
                    let mut directive = Export {
                        uri: uri.into(),
                        show: vec![],
                        hide: vec![],
                    };
                    let mut mode = "";
                    let mut index = cursor + 2;
                    while index < tokens.len() && at(&tokens, index) != ";" {
                        let text = at(&tokens, index);
                        if matches!(text, "show" | "hide") {
                            mode = text;
                        } else if identifier(text) {
                            if mode == "show" {
                                directive.show.push(text.into());
                            }
                            if mode == "hide" {
                                directive.hide.push(text.into());
                            }
                        }
                        index += 1;
                    }
                    exports.push(directive);
                }
            }
        }
        cursor = tokens[cursor].close.unwrap_or(cursor) + 1;
    }
    let classes = components::classes(&tokens)
        .into_iter()
        .map(|class| {
            let name_index = class.name_token;
            let header_start = (0..name_index.saturating_sub(1))
                .rev()
                .find(|index| matches!(at(&tokens, *index), ";" | "}" | "{"))
                .map_or(0, |index| index + 1);
            let abstract_class = tokens[header_start..name_index]
                .iter()
                .any(|token| matches!(token.text, "abstract" | "sealed"));
            let mut bases = vec![];
            let mut index = name_index + 1;
            let generic = components::declaration_type_end(&tokens, index, class.open);
            if generic > index {
                index = generic + 1;
            }
            let mut type_list = false;
            while index < class.open {
                let text = at(&tokens, index);
                if matches!(text, "extends" | "implements" | "with") {
                    type_list = true;
                    index += 1;
                    continue;
                }
                if type_list && identifier(text) {
                    let value = if at(&tokens, index + 1) == "." {
                        index += 2;
                        at(&tokens, index)
                    } else {
                        text
                    };
                    bases.push(value.to_owned());
                    let end = components::declaration_type_end(&tokens, index + 1, class.open);
                    if end > index + 1 {
                        index = end;
                    }
                    type_list = false;
                } else if text == "," {
                    type_list = true;
                }
                index += 1;
            }
            let mut constructors = vec![];
            let mut cursor = class.open + 1;
            while cursor < class.close {
                let start = cursor;
                while at(&tokens, cursor) == "@" {
                    cursor += 1;
                    while identifier(at(&tokens, cursor)) || at(&tokens, cursor) == "." {
                        cursor += 1;
                    }
                    if at(&tokens, cursor) == "(" {
                        cursor = tokens[cursor].close.unwrap_or(cursor) + 1;
                    }
                }
                while matches!(at(&tokens, cursor), "const" | "factory" | "external") {
                    cursor += 1;
                }
                if at(&tokens, cursor) == class.name {
                    let mut open = cursor + 1;
                    let mut name = String::new();
                    if at(&tokens, open) == "." {
                        name = at(&tokens, open + 1).into();
                        open += 2;
                    }
                    if at(&tokens, open) == "(" {
                        if let Some(close) = tokens[open].close {
                            constructors.push(Constructor {
                                name: name.clone(),
                                constant: tokens[start..cursor]
                                    .iter()
                                    .any(|token| token.text == "const"),
                                factory: tokens[start..cursor]
                                    .iter()
                                    .any(|token| token.text == "factory"),
                                signature: format!(
                                    "{}{}{}",
                                    class.name,
                                    if name.is_empty() {
                                        String::new()
                                    } else {
                                        format!(".{name}")
                                    },
                                    &source[tokens[open].start..tokens[close].end]
                                ),
                            });
                            cursor = close + 1;
                        }
                    }
                }
                while cursor < class.close {
                    if at(&tokens, cursor) == ";" {
                        cursor += 1;
                        break;
                    }
                    if at(&tokens, cursor) == "{" {
                        cursor = tokens[cursor].close.unwrap_or(cursor) + 1;
                        break;
                    }
                    cursor = tokens[cursor].close.unwrap_or(cursor) + 1;
                }
                if cursor <= start {
                    cursor = start + 1;
                }
            }
            if constructors.is_empty() && !abstract_class {
                constructors.push(Constructor {
                    name: String::new(),
                    constant: false,
                    factory: false,
                    signature: format!("{}()", class.name),
                });
            }
            Declaration {
                name: class.name,
                bases,
                abstract_class,
                line: source[..tokens[name_index].start]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    + 1,
                constructors,
            }
        })
        .collect();
    Ok(Unit {
        classes,
        exports,
        part,
    })
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.chars().enumerate().all(|(index, ch)| {
            ch.is_alphabetic() || matches!(ch, '_' | '$') || index > 0 && ch.is_numeric()
        })
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Parameter {
    pub name: String,
    pub kind: String,
    pub named: bool,
    pub required: bool,
}

pub(crate) fn parameters(signature: &Value) -> Result<Vec<Parameter>> {
    if signature["parameters"]
        .as_array()
        .is_some_and(|values| values.len() > 100)
    {
        return Err(Problem::new(
            422,
            "This constructor has too many parameters. Add it in Code.",
        ));
    }
    let label = signature["label"]
        .as_str()
        .ok_or_else(|| Problem::new(422, "Dart could not resolve this constructor."))?;
    let tokens = tokenize(label)?;
    let open = tokens
        .iter()
        .position(|token| token.text == "(")
        .ok_or_else(|| Problem::new(422, "Dart could not resolve this constructor."))?;
    let close = tokens[open]
        .close
        .ok_or_else(|| Problem::new(422, "Dart returned an incomplete constructor signature."))?;
    let mut groups = vec![];
    let mut cursor = open + 1;
    while cursor < close {
        if matches!(at(&tokens, cursor), "{" | "[") {
            if let Some(end) = tokens[cursor].close {
                groups.push((
                    tokens[cursor].start,
                    tokens[end].end,
                    at(&tokens, cursor) == "{",
                ));
            }
        }
        cursor = tokens[cursor].close.unwrap_or(cursor) + 1;
    }
    let mut offset = 0;
    let mut result = vec![];
    for parameter in signature["parameters"]
        .as_array()
        .into_iter()
        .flatten()
        .take(100)
    {
        let value = parameter["label"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| {
                let range = parameter["label"].as_array()?;
                let start = range.first()?.as_u64()? as usize;
                let end = range.get(1)?.as_u64()? as usize;
                String::from_utf16(
                    &label
                        .encode_utf16()
                        .skip(start)
                        .take(end.checked_sub(start)?)
                        .collect::<Vec<_>>(),
                )
                .ok()
            })
            .ok_or_else(|| Problem::new(422, "Dart returned an unsupported parameter label."))?;
        let start = label[offset..]
            .find(&value)
            .map(|start| offset + start)
            .ok_or_else(|| Problem::new(422, "Refresh the constructor parameters."))?;
        offset = start + value.len();
        let named = groups
            .iter()
            .any(|(begin, end, named)| *named && *begin < start && start < *end);
        let optional = groups
            .iter()
            .any(|(begin, end, named)| !*named && *begin < start && start < *end);
        let declaration = value.split(" = ").next().unwrap_or(&value).trim();
        let required = declaration.starts_with("required ") || !named && !optional;
        let declaration = declaration.strip_prefix("required ").unwrap_or(declaration);
        let (kind, name) = declaration
            .rsplit_once(' ')
            .ok_or_else(|| Problem::new(422, "Dart returned an unsupported parameter type."))?;
        if !identifier(name) {
            return Err(Problem::new(
                422,
                "Dart returned an unsupported parameter name.",
            ));
        }
        result.push(Parameter {
            name: name.into(),
            kind: kind.into(),
            named,
            required,
        });
    }
    Ok(result)
}

pub(crate) fn slot_available(
    source: &str,
    id: &str,
    name: &str,
    multiple: bool,
    position: Option<usize>,
    scope: &Scope,
) -> Result<bool> {
    let (tokens, nodes) = inspect_in(source, scope)?;
    let node = nodes
        .iter()
        .find(|node| node.id == id)
        .ok_or_else(|| Problem::new(409, "Select the widget again."))?;
    if position.is_some_and(|position| {
        node.args.iter().filter(|arg| arg.name.is_none()).count() < position
    }) {
        return Ok(false);
    }
    Ok((if let Some(position) = position {
        node.args
            .iter()
            .filter(|arg| arg.name.is_none())
            .nth(position)
    } else {
        node.args.iter().find(|arg| arg.name == Some(name))
    })
    .is_none_or(|arg| {
        if multiple {
            list_for(&tokens, *arg).is_some()
        } else {
            empty_child(&tokens, *arg)
        }
    }))
}

pub(crate) fn insert(
    source: &str,
    id: &str,
    slot: &str,
    multiple: bool,
    position: Option<usize>,
    code: &str,
    scope: &Scope,
) -> Result<(String, String)> {
    let code = properties::expression(&json!(code))?;
    let (tokens, nodes) = inspect_in(source, scope)?;
    let node = nodes
        .iter()
        .find(|node| node.id == id)
        .ok_or_else(|| Problem::new(409, "Select the widget again."))?;
    let marker = format!("/*freeflow-insert-{}*/", uuid::Uuid::new_v4());
    let value = format!("{marker}{code}");
    let argument = if let Some(position) = position {
        node.args
            .iter()
            .copied()
            .filter(|arg| arg.name.is_none())
            .nth(position)
    } else {
        node.args.iter().copied().find(|arg| arg.name == Some(slot))
    };
    let mut result = match argument {
        None => {
            let value = if multiple {
                format!("[{value}]")
            } else {
                value.clone()
            };
            let value = if position.is_some() {
                value
            } else {
                format!("{slot}: {value}")
            };
            if let Some(first) = node
                .args
                .iter()
                .find(|arg| position.is_some() && arg.name.is_some())
            {
                replace(
                    source,
                    tokens[first.span.first].start,
                    tokens[first.span.first].start,
                    &format!("{value}, "),
                )
            } else {
                append(
                    source,
                    &tokens,
                    node.open,
                    node.args.last().map(|arg| arg.span),
                    &value,
                )
            }
        }
        Some(arg) if !multiple && empty_child(&tokens, arg) => replace(
            source,
            tokens[arg.expression].start,
            tokens[arg.span.last].end,
            &value,
        ),
        Some(arg) if multiple => {
            let list = list_for(&tokens, arg).ok_or_else(|| {
                Problem::new(
                    422,
                    "This child list uses a Dart expression. Edit it in Code.",
                )
            })?;
            append(
                source,
                &tokens,
                list.open,
                list.items.last().copied(),
                &value,
            )
        }
        _ => {
            return Err(Problem::new(
                409,
                "This child slot is already filled. Select an empty slot or wrap its widget.",
            ));
        }
    };
    let point = result.find(&marker).unwrap();
    let tokens = tokenize(&result)?;
    let mut edits = vec![];
    for (index, token) in tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| token.text == "const" && token.start < point)
    {
        let mut cursor = index + 1;
        if at(&tokens, cursor) == "<" {
            cursor = type_end(&tokens, cursor, tokens.len()) + 1;
        }
        if identifier(at(&tokens, cursor)) {
            cursor += 1;
            let end = type_end(&tokens, cursor, tokens.len());
            if end > cursor {
                cursor = end + 1;
            }
            while at(&tokens, cursor) == "." && identifier(at(&tokens, cursor + 1)) {
                cursor += 2;
            }
        }
        if matches!(at(&tokens, cursor), "(" | "[" | "{")
            && tokens
                .get(cursor)
                .and_then(|token| token.close)
                .is_some_and(|end| tokens[end].end > point)
        {
            let end = if result[token.end..tokens[index + 1].start].trim().is_empty() {
                tokens[index + 1].start
            } else {
                token.end
            };
            edits.push((token.start, end));
        }
    }
    for (start, end) in edits.into_iter().rev() {
        result.replace_range(start..end, "");
    }
    let point = result.find(&marker).unwrap();
    result.replace_range(point..point + marker.len(), "");
    let selected = result[..point].encode_utf16().count().to_string();
    Ok((result, selected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_metadata_separates_generic_bounds_factories_and_methods() {
        let source = "export 'one.dart' show Tile, Other hide Other; part 'piece.dart';\n\
            abstract class State<T extends StatefulWidget> { Widget build(); }\n\
            abstract class Tile<T extends Widget> extends StatelessWidget implements PreferredSizeWidget {\n\
              const Tile({required this.child}); factory Tile.named() = RealTile;\n\
              Widget build(context) => Text('Tile.fake()');\n\
            } class RealTile extends Tile<Widget> { RealTile._(); RealTile.named(); }\n\
            class Implicit extends Widget { final String text = 'fake'; }";
        let unit = unit(source).unwrap();
        assert_eq!(unit.exports.len(), 2);
        assert_eq!(unit.exports[0].show, ["Tile", "Other"]);
        assert_eq!(unit.exports[0].hide, ["Other"]);
        assert!(unit.classes[0].bases.is_empty());
        let tile = &unit.classes[1];
        assert!(tile.abstract_class);
        assert_eq!(tile.bases, ["StatelessWidget", "PreferredSizeWidget"]);
        assert_eq!(tile.constructors.len(), 2);
        assert!(tile.constructors[0].constant);
        assert!(tile.constructors[1].factory);
        assert_eq!(tile.constructors[1].name, "named");
        assert_eq!(unit.classes[2].constructors[0].name, "_");
        assert_eq!(unit.classes[3].constructors[0].signature, "Implicit()");
        assert!(
            super::unit("part of 'main.dart'; class Piece extends Widget {}")
                .unwrap()
                .part
        );
    }

    #[test]
    fn resolved_signatures_preserve_optional_named_and_function_parameters() {
        let args = parameters(&json!({"label":"Tile(String text, [Widget? child, void Function(int, String)? callback = null])", "parameters":[{"label":"String text"},{"label":"Widget? child"},{"label":"void Function(int, String)? callback = null"}]})).unwrap();
        assert!(args[0].required && !args[0].named);
        assert!(!args[1].required && !args[1].named);
        assert_eq!(args[2].kind, "void Function(int, String)?");
        let label = "Tile({required Widget label, Key? key = null})";
        let start = label.find("required").unwrap();
        let end = start + "required Widget label".len();
        let args = parameters(&json!({"label":label,"parameters":[{"label":[start,end]},{"label":"Key? key = null"}]})).unwrap();
        assert!(args[0].required && args[0].named);
        assert_eq!(args[0].name, "label");
        assert!(!args[1].required && args[1].named);
        assert!(
            parameters(
                &json!({"label":"Tile()", "parameters":vec![json!({"label":"Widget child"});101]})
            )
            .is_err()
        );
    }

    fn node(source: &str, kind: &str, scope: &Scope) -> String {
        inspect_in(source, scope)
            .unwrap()
            .1
            .into_iter()
            .find(|node| node.kind == kind)
            .unwrap()
            .id
    }

    #[test]
    fn child_insertion_preserves_comments_utf16_and_const_declarations() {
        let source = "// 😀\r\nconst untouched = 5;\r\nWidget build() => const Column(children: const <Widget>[/* keep */ Text('Old'),]);\r\n";
        let scope = Scope::default();
        let id = node(source, "Column", &scope);
        let (updated, selected) = insert(
            source,
            &id,
            "children",
            true,
            None,
            "Text(counter.toString())",
            &scope,
        )
        .unwrap();
        assert!(updated.contains("const untouched = 5;\r\n"));
        assert!(updated.contains("/* keep */ Text('Old'),"));
        assert!(!updated.contains("const Column"));
        assert!(!updated.contains("const <Widget>"));
        assert_eq!(
            selected,
            updated[..updated.find("Text(counter").unwrap()]
                .encode_utf16()
                .count()
                .to_string()
        );
        assert!(
            inspect_in(&updated, &scope)
                .unwrap()
                .1
                .iter()
                .any(|node| node.id == selected && node.kind == "Text")
        );
        assert!(!updated.contains("freeflow-insert-"));
        assert!(
            insert(
                source,
                &id,
                "children",
                true,
                None,
                "Text('x'); runBadCode()",
                &scope
            )
            .is_err()
        );
    }

    #[test]
    fn child_slots_refuse_occupied_or_computed_values_and_allow_positional_widgets() {
        let mut scope = Scope::default();
        scope.names.insert("Host".into());
        for (source, available) in [
            ("Host(child: null)", true),
            ("Host(child: const SizedBox.shrink())", true),
            ("Host(child: Text('Keep'))", false),
        ] {
            let id = node(source, "Host", &scope);
            assert_eq!(
                slot_available(source, &id, "child", false, None, &scope).unwrap(),
                available
            );
            assert_eq!(
                insert(source, &id, "child", false, None, "Text('New')", &scope).is_ok(),
                available
            );
        }
        let source = "Host(children: computedWidgets)";
        let id = node(source, "Host", &scope);
        assert!(!slot_available(source, &id, "children", true, None, &scope).unwrap());
        assert!(insert(source, &id, "children", true, None, "Text('New')", &scope).is_err());
        let source = "Host(key: aKey)";
        let id = node(source, "Host", &scope);
        assert!(!slot_available(source, &id, "child", false, Some(1), &scope).unwrap());
        let (updated, _) =
            insert(source, &id, "child", false, Some(0), "Text('New')", &scope).unwrap();
        assert_eq!(updated, "Host(Text('New'), key: aKey)");
    }
}
