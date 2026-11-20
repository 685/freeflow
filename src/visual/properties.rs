use super::*;

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic()
                || matches!(byte, b'_' | b'$')
                || index > 0 && byte.is_ascii_digit()
        })
}

struct Target<'a> {
    open: usize,
    args: Vec<Arg<'a>>,
    arg: Option<Arg<'a>>,
    name: Option<String>,
    create: Option<(String, String)>,
}

pub(super) fn access(node: &Node<'_>, tokens: &[Token<'_>], name: &str) -> (bool, bool, bool) {
    target(node, tokens, name).map_or((false, false, false), |target| {
        (
            target.name.is_some() || target.arg.is_some(),
            target.arg.is_some(),
            target.name.is_some() && target.arg.is_some(),
        )
    })
}

fn target<'a>(node: &Node<'a>, tokens: &[Token<'a>], name: &str) -> Result<Target<'a>> {
    let known = node.spec.fields.iter().find(|item| {
        item.name == name
            || item
                .nest
                .as_ref()
                .is_some_and(|(outer, _)| format!("{outer}.{}", item.name) == name)
    });
    let mut parts: Vec<_> = name.split('.').collect();
    if name.len() > 180 || parts.len() > 8 || parts.iter().any(|part| !identifier(part)) {
        return Err(Problem::new(
            400,
            "Enter a property name, such as maxLines or style.fontSize.",
        ));
    }
    if let Some(item) = known {
        if let Some((outer, _)) = &item.nest {
            parts = vec![outer, &item.name];
        }
        if item.positional {
            let arg = node.args.iter().copied().find(|arg| arg.name.is_none());
            return Ok(Target {
                open: node.open,
                args: node.args.clone(),
                arg,
                name: None,
                create: None,
            });
        }
    }
    let mut open = node.open;
    let mut arguments = node.args.clone();
    for (index, part) in parts[..parts.len() - 1].iter().enumerate() {
        let Some(outer) = named(&arguments, part) else {
            if index == 0 && parts.len() == 2 {
                if let Some((outer, kind)) = known.and_then(|field| field.nest.as_ref()) {
                    return Ok(Target {
                        open,
                        args: arguments,
                        arg: None,
                        name: Some(parts[1].into()),
                        create: Some((outer.clone(), kind.clone())),
                    });
                }
            }
            return Err(Problem::new(
                422,
                format!(
                    "Set {} before editing a property inside it.",
                    parts[..=index].join(".")
                ),
            ));
        };
        let mut start = outer.expression;
        if matches!(at(tokens, start), "const" | "new") {
            start += 1;
        }
        let mut cursor = start;
        while cursor <= outer.span.last {
            if !identifier(at(tokens, cursor)) {
                break;
            }
            cursor += 1;
            let generic = type_end(tokens, cursor, outer.span.last + 1);
            if generic > cursor {
                cursor = generic + 1;
            }
            if at(tokens, cursor) != "." {
                break;
            }
            cursor += 1;
        }
        if cursor == start
            || at(tokens, cursor) != "("
            || tokens[cursor].close != Some(outer.span.last)
        {
            return Err(Problem::new(
                422,
                format!(
                    "Edit the {} expression as a whole; it is not an explicit constructor call.",
                    parts[..=index].join(".")
                ),
            ));
        }
        open = cursor;
        arguments = args(tokens, open);
    }
    let name = parts.last().unwrap().to_string();
    let matching: Vec<_> = arguments
        .iter()
        .copied()
        .filter(|arg| arg.name == Some(&name))
        .collect();
    if matching.len() > 1 {
        return Err(Problem::new(
            422,
            "Resolve duplicate properties in the code editor first.",
        ));
    }
    Ok(Target {
        open,
        arg: matching.first().copied(),
        args: arguments,
        name: Some(name),
        create: None,
    })
}

pub fn property_details(source: &str, id: &str, name: &str) -> Result<Value> {
    let (tokens, nodes) = inspect(source)?;
    let node = nodes
        .iter()
        .find(|node| node.id == id)
        .ok_or_else(|| Problem::new(409, "Select the widget again after the source refreshes."))?;
    let target = target(node, &tokens, name)?;
    let expression = target.arg.map(|arg| arg.expression(&tokens, source));
    if expression.is_some_and(|expression| expression.len() > 64 * 1024) {
        return Err(Problem::new(
            413,
            "This property is too large for the quick editor. Open the Dart file instead.",
        ));
    }
    let offset = target
        .arg
        .map_or(tokens[node.start].start, |arg| tokens[arg.expression].start);
    Ok(
        json!({"name":name,"expression":expression,"present":target.arg.is_some(),
        "removable":target.arg.is_some() && target.name.is_some(),
        "offset":source[..offset].encode_utf16().count(),"line":source[..offset].bytes().filter(|byte| *byte == b'\n').count()+1}),
    )
}

pub fn property_completion(source: &str, id: &str) -> Result<(String, usize)> {
    let (tokens, nodes) = inspect(source)?;
    let node = nodes
        .iter()
        .find(|node| node.id == id)
        .ok_or_else(|| Problem::new(409, "Select the widget again after the source refreshes."))?;
    let (offset, gap) = match node.args.last() {
        None => (tokens[node.open].end, "\n  "),
        Some(arg) => match arg.span.comma {
            Some(comma) => (tokens[comma].end, "\n  "),
            None => (tokens[arg.span.last].end, ",\n  "),
        },
    };
    let text = replace(source, offset, offset, gap);
    let cursor = text[..offset + gap.len()].encode_utf16().count();
    Ok((text, cursor))
}

fn expression(value: &Value) -> Result<String> {
    let code = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 64 * 1024 && !value.contains('\0'))
        .ok_or_else(|| Problem::new(422, "Enter one Dart expression of up to 64 KiB."))?;
    let wrapped = format!("_freeflow_({code}\n)");
    let tokens = tokenize(&wrapped)?;
    let invalid = || {
        Problem::new(
            422,
            "Enter one complete Dart expression. Keep statements inside a callback body.",
        )
    };
    if tokens.get(1).and_then(|token| token.close) != tokens.len().checked_sub(1)
        || tokens
            .iter()
            .any(|token| matches!(token.text, "(" | "[" | "{") && token.close.is_none())
    {
        return Err(invalid());
    }
    let parts = args(&tokens, 1);
    if parts.len() != 1 || parts[0].name.is_some() || parts[0].span.comma.is_some() {
        return Err(invalid());
    }
    let mut cursor = parts[0].expression;
    while cursor <= parts[0].span.last {
        if at(&tokens, cursor) == ";" {
            return Err(invalid());
        }
        cursor = tokens[cursor]
            .close
            .unwrap_or_else(|| type_end(&tokens, cursor, parts[0].span.last + 1))
            + 1;
    }
    let end = tokens[parts[0].span.last].end;
    let trailing_comment = wrapped[end..wrapped.len() - 2].contains("//");
    Ok(if trailing_comment {
        format!("{code}\n")
    } else {
        code.into()
    })
}

pub(super) fn edit_expression(
    source: &str,
    tokens: &[Token<'_>],
    node: &Node<'_>,
    command: &Value,
) -> Result<String> {
    let name = command["name"].as_str().unwrap_or_default();
    let target = target(node, tokens, name)?;
    if command["remove"] == true {
        if !command["expression"].is_null() {
            return Err(Problem::new(
                400,
                "Choose a new expression or remove the property.",
            ));
        }
        if target.name.is_none() {
            return Err(Problem::new(422, "The positional value cannot be removed."));
        }
        let Some(arg) = target.arg else {
            return Ok(source.into());
        };
        let index = target
            .args
            .iter()
            .position(|item| item.span.first == arg.span.first)
            .unwrap();
        let previous_comma = (arg.span.comma.is_none() && index > 0)
            .then(|| target.args[index - 1].span.comma)
            .flatten();
        let end = tokens[arg.span.comma.unwrap_or(arg.span.last)].end;
        let output = replace(source, tokens[arg.span.first].start, end, "");
        return Ok(match previous_comma {
            Some(comma) => replace(&output, tokens[comma].start, tokens[comma].end, ""),
            None => output,
        });
    }
    let value = expression(&command["expression"])?;
    if let Some(arg) = target.arg {
        return Ok(replace(
            source,
            tokens[arg.expression].start,
            tokens[arg.span.last].end,
            &value,
        ));
    }
    let name = target.name.ok_or_else(|| {
        Problem::new(
            422,
            "The positional value is missing. Edit this constructor in Code.",
        )
    })?;
    let value = match target.create {
        Some((outer, kind)) => format!("{outer}: {kind}({name}: {value})"),
        None => format!("{name}: {value}"),
    };
    Ok(append(
        source,
        tokens,
        target.open,
        target.args.last().map(|arg| arg.span),
        &value,
    ))
}
