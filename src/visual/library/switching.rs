use super::*;
use std::ops::Range;

pub(crate) struct Argument<'a> {
    pub name: Option<&'a str>,
    pub expression: &'a str,
    leading: &'a str,
    label_gap: &'a str,
    value_gap: &'a str,
    trailing: &'a str,
}

impl Argument<'_> {
    fn write(&self, output: &mut String, name: Option<&str>) {
        output.push_str(self.leading);
        if let Some(name) = name {
            output.push_str(name);
            output.push_str(self.label_gap);
            output.push(':');
            if self.name.is_none() {
                output.push(' ');
            }
            output.push_str(self.value_gap);
        } else {
            // Preserve comments between a removed label and its value.
            for gap in [self.label_gap, self.value_gap] {
                if !gap.trim().is_empty() {
                    output.push_str(gap);
                }
            }
        }
        output.push_str(self.expression);
        output.push_str(self.trailing);
    }
}

pub(crate) enum ValueSource {
    Existing(usize),
    Expression(String),
}

pub(crate) struct Destination<'a> {
    pub name: Option<&'a str>,
    pub value: ValueSource,
}

pub(crate) struct Call<'a> {
    source: &'a str,
    pub id: &'a str,
    pub class: &'a str,
    pub constructor: &'a str,
    pub namespace: String,
    pub symbol: usize,
    pub arguments: Vec<Argument<'a>>,
    start: usize,
    after_const: Option<usize>,
    class_end: usize,
    suffix: Option<(Range<usize>, Range<usize>)>,
    open: usize,
    close: usize,
    tail: &'a str,
    trailing_comma: bool,
}

impl<'a> Call<'a> {
    pub(crate) fn locate(source: &'a str, id: &'a str, scope: &Scope) -> Result<Self> {
        let (tokens, nodes) = inspect_in(source, scope)?;
        let node = selected_node(&nodes, id)?;
        let first = node.start + usize::from(matches!(at(&tokens, node.start), "const" | "new"));
        let namespace = node.namespace(&tokens);
        let kind = first + if namespace.is_empty() { 0 } else { 2 };
        let raw_name = node.constructor;
        let suffix = (!raw_name.is_empty()).then(|| {
            let dot = &tokens[node.open - 2];
            let name = &tokens[node.open - 1];
            (dot.start..dot.end, name.start..name.end)
        });
        let class_end = suffix.as_ref().map_or(node.open, |_| node.open - 2) - 1;
        let mut boundary = tokens[node.open].end;
        let arguments = node
            .args
            .iter()
            .map(|arg| {
                let first = &tokens[arg.span.first];
                let expression = &tokens[arg.expression];
                let last = &tokens[arg.span.last];
                let comma = arg.span.comma.map(|index| &tokens[index]);
                let (label_gap, value_gap) = if arg.name.is_some() {
                    let colon = &tokens[arg.span.first + 1];
                    (
                        &source[first.end..colon.start],
                        &source[colon.end..expression.start],
                    )
                } else {
                    ("", "")
                };
                let value = Argument {
                    name: arg.name,
                    expression: arg.expression(&tokens, source),
                    leading: &source[boundary..first.start],
                    label_gap,
                    value_gap,
                    trailing: comma.map_or("", |comma| &source[last.end..comma.start]),
                };
                boundary = comma.map_or(last.end, |comma| comma.end);
                value
            })
            .collect();
        let close = tokens[node.close].start;
        let symbol = suffix
            .as_ref()
            .map_or(tokens[kind].start, |(_, name)| name.start);
        Ok(Self {
            source,
            id,
            class: node.kind,
            constructor: if raw_name == "new" { "" } else { raw_name },
            namespace,
            symbol: source[..symbol].encode_utf16().count(),
            arguments,
            start: tokens[node.start].start,
            after_const: (at(&tokens, node.start) == "const").then(|| tokens[first].start),
            class_end: tokens[class_end].end,
            suffix,
            open: tokens[node.open].start,
            close,
            tail: &source[boundary..close],
            trailing_comma: node
                .args
                .last()
                .is_some_and(|argument| argument.span.comma.is_some()),
        })
    }

    fn prefix(&self, constructor: &str) -> String {
        let source = self.source;
        if let Some((dot, name)) = &self.suffix {
            if constructor.is_empty() {
                format!(
                    "{}{}{}",
                    &source[..dot.start],
                    &source[dot.end..name.start],
                    &source[name.end..self.open]
                )
            } else {
                replace(&source[..self.open], name.start, name.end, constructor)
            }
        } else if constructor.is_empty() {
            source[..self.open].to_owned()
        } else {
            replace(
                &source[..self.open],
                self.class_end,
                self.class_end,
                &format!(".{constructor}"),
            )
        }
    }

    pub(crate) fn signature_probe(&self, constructor: Option<&str>) -> (String, usize) {
        let mut prefix = constructor.map_or_else(
            || self.source[..self.open].to_owned(),
            |name| self.prefix(name),
        );
        let cursor = prefix.encode_utf16().count() + 1;
        prefix.push_str(&self.source[self.open..]);
        (prefix, cursor)
    }

    pub(crate) fn rewrite(
        &self,
        constructor: &Constructor,
        arguments: &[Destination<'_>],
    ) -> Result<(String, String)> {
        let mut result = self.prefix(&constructor.name);
        result.push('(');
        let line_break = if self.source.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let indent = self.arguments.iter().find_map(|argument| {
            let (_, indent) = argument.leading.rsplit_once('\n')?;
            indent.trim().is_empty().then_some(indent)
        });
        for (index, argument) in arguments.iter().enumerate() {
            if index > 0 {
                result.push(',');
            }
            match &argument.value {
                ValueSource::Existing(old) => {
                    self.arguments[*old].write(&mut result, argument.name)
                }
                ValueSource::Expression(code) => {
                    if let Some(indent) = indent {
                        result.push_str(line_break);
                        result.push_str(indent);
                    } else if index > 0 {
                        result.push(' ');
                    }
                    if let Some(name) = argument.name {
                        result.push_str(name);
                        result.push_str(": ");
                    }
                    result.push_str(code);
                }
            }
        }
        if !arguments.is_empty() && self.trailing_comma {
            result.push(',');
        }
        result.push_str(self.tail);
        result.push_str(&self.source[self.close..]);
        if constructor.constant {
            return Ok((result, self.id.to_owned()));
        }
        let marker = format!("/*freeflow-constructor-{}*/", uuid::Uuid::new_v4());
        let at = self.after_const.unwrap_or(self.start);
        result.insert_str(at, &marker);
        // Discard the call's own const separately. Its opening parenthesis is
        // after the marker, unlike an enclosing constant expression.
        if let Some(after_const) = self.after_const {
            let end = if self.source[self.start + 5..after_const].trim().is_empty() {
                after_const
            } else {
                self.start + 5
            };
            result.replace_range(self.start..end, "");
        }
        finish_edit(result, &marker)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constructor(name: &str, constant: bool) -> Constructor {
        Constructor {
            name: name.into(),
            symbol: 0,
            constant,
            factory: false,
            signature: String::new(),
        }
    }

    #[test]
    fn switching_keeps_qualified_type_arguments_comments_and_utf16_selection() {
        let before = "// 😀\r\nfinal widget = const ui.Badge<int> /* class */ . /* name */ primary(\r\n  title /* label */: /* value */ '👋',\r\n  child: ui.Text('Keep me'),\r\n);";
        let mut scope = Scope::default();
        scope.names.insert("Badge".into());
        let id = before[..before.find("const ui.Badge").unwrap()]
            .encode_utf16()
            .count()
            .to_string();
        let call = Call::locate(before, &id, &scope).unwrap();
        assert_eq!(call.constructor, "primary");
        assert_eq!(
            call.symbol,
            before[..before.find("primary").unwrap()]
                .encode_utf16()
                .count()
        );
        let args = [
            Destination {
                name: None,
                value: ValueSource::Existing(0),
            },
            Destination {
                name: Some("label"),
                value: ValueSource::Existing(1),
            },
        ];
        let (result, selected) = call.rewrite(&constructor("", true), &args).unwrap();
        assert_eq!(selected, id);
        assert_eq!(
            result,
            "// 😀\r\nfinal widget = const ui.Badge<int> /* class */  /* name */ (\r\n   /* label */ /* value */ '👋',\r\n  label: ui.Text('Keep me'),\r\n);"
        );
    }

    #[test]
    fn switching_to_non_const_updates_enclosing_expressions_and_keeps_other_const_calls() {
        let before = "final a = const Column(children: const <Widget>[const /* widget */ SizedBox(width: 4), const Text('Keep')]);";
        let id = before.find("const /* widget */").unwrap().to_string();
        let call = Call::locate(before, &id, &Scope::default()).unwrap();
        let (result, selected) = call
            .rewrite(
                &constructor("square", false),
                &[Destination {
                    name: Some("dimension"),
                    value: ValueSource::Existing(0),
                }],
            )
            .unwrap();
        assert_eq!(
            result,
            "final a = Column(children: <Widget>[ /* widget */ SizedBox.square(dimension: 4), const Text('Keep')]);"
        );
        assert_eq!(selected, result.find("SizedBox").unwrap().to_string());
    }
}
