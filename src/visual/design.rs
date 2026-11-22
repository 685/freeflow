use super::*;

struct Design {
    name: String,
    owner: Option<String>,
    span: Span,
    kind: &'static str,
    integer: bool,
    constant: bool,
}

fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name.chars().enumerate().all(|(index, ch)| {
            ch.is_alphabetic() || matches!(ch, '_' | '$') || index > 0 && ch.is_numeric()
        })
}

fn call<'a>(tokens: &[Token<'a>], span: Span) -> Option<(&'a str, &'a str, usize, String)> {
    let mut first = span.first + usize::from(matches!(at(tokens, span.first), "const" | "new"));
    let mut prefix = String::new();
    if at(tokens, first + 1) == "."
        && at(tokens, first + 2)
            .chars()
            .next()
            .is_some_and(char::is_uppercase)
    {
        prefix = format!("{}.", at(tokens, first));
        first += 2;
    }
    let root = at(tokens, first);
    let (member, open) = if at(tokens, first + 1) == "." {
        (at(tokens, first + 2), first + 3)
    } else {
        ("", first + 1)
    };
    (at(tokens, open) == "(" && tokens[open].close == Some(span.last))
        .then_some((root, member, open, prefix))
}

fn number(text: &str) -> Option<Value> {
    let value = text.trim().replace('_', "").parse::<f64>().ok()?;
    serde_json::Number::from_f64(value).map(Value::Number)
}

fn color(text: &str) -> Option<Value> {
    static HEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:const\s+)?(?:[A-Za-z_$][\w$]*\.)?Color\(\s*0[xX]([a-fA-F0-9_]+)\s*\)$")
            .unwrap()
    });
    if let Some(capture) = HEX.captures(text.trim()) {
        let value = u32::from_str_radix(&capture[1].replace('_', ""), 16).ok()?;
        return Some(json!(if value >> 24 == 255 {
            format!("#{:06x}", value & 0xffffff)
        } else {
            format!("#{value:08x}")
        }));
    }
    decode_literal(text, &theme_field("seedColor"))
}

fn category(
    prefix: &[Token<'_>],
    tokens: &[Token<'_>],
    span: Span,
    name: &str,
) -> Option<&'static str> {
    let root = call(tokens, span).map(|call| call.0).unwrap_or_default();
    let declaration_type = prefix
        .iter()
        .rev()
        .skip(1)
        .find(|token| !matches!(token.text, "get" | "?"));
    let typed = |value| declaration_type.is_some_and(|token| token.text == value) || root == value;
    if typed("Color") {
        return Some("color");
    }
    if typed("TextStyle") {
        return Some("typography");
    }
    if typed("EdgeInsets") || typed("EdgeInsetsDirectional") {
        return Some("insets");
    }
    if typed("BorderRadius") || typed("BorderRadiusDirectional") {
        return Some("radius");
    }
    if ["double", "int", "num"].iter().any(|kind| typed(kind)) {
        return Some("number");
    }
    if typed("String") && name.to_ascii_lowercase().contains("font") {
        return Some("font");
    }
    None
}

fn declarations(source: &str, tokens: &[Token<'_>]) -> Vec<Design> {
    fn body(
        source: &str,
        tokens: &[Token<'_>],
        start: usize,
        end: usize,
        owner: Option<&str>,
        result: &mut Vec<Design>,
    ) {
        let mut first = start;
        while first < end {
            let mut cursor = first;
            let mut assignment = None;
            let mut skip = None;
            while cursor < end && at(tokens, cursor) != ";" {
                if at(tokens, cursor) == "=" && assignment.is_none() {
                    assignment = Some(cursor);
                }
                if at(tokens, cursor) == "{" && assignment.is_none() {
                    skip = tokens[cursor].close.map(|close| close + 1);
                    break;
                }
                cursor = tokens[cursor].close.unwrap_or(cursor) + 1;
            }
            if let Some(next) = skip {
                first = next;
                continue;
            }
            if cursor >= end || assignment.is_none() {
                first = cursor + 1;
                continue;
            }
            let assignment = assignment.unwrap();
            let prefix = &tokens[first..assignment];
            let getter = prefix.iter().any(|token| token.text == "get");
            let immutable = prefix
                .iter()
                .any(|token| matches!(token.text, "const" | "final"))
                || getter;
            let static_field = owner.is_none() || prefix.iter().any(|token| token.text == "static");
            if immutable && static_field {
                let mut part = first;
                let mut scan = first;
                while scan <= cursor {
                    if scan == cursor || at(tokens, scan) == "," {
                        if let Some(equal) = (part..scan).find(|index| at(tokens, *index) == "=") {
                            let name = at(tokens, equal.saturating_sub(1));
                            let expr =
                                equal + 1 + usize::from(getter && at(tokens, equal + 1) == ">");
                            if identifier(name) && expr < scan {
                                let span = Span {
                                    first: expr,
                                    last: scan - 1,
                                    comma: None,
                                };
                                let text = span.text(tokens, source);
                                let kind = category(prefix, tokens, span, name)
                                    .or_else(|| color(text).map(|_| "color"))
                                    .or_else(|| {
                                        (at(tokens, expr) == "Colors"
                                            || at(tokens, expr + 2) == "Colors"
                                                && at(tokens, expr + 1) == ".")
                                            .then_some("color")
                                    })
                                    .or_else(|| {
                                        (name.to_ascii_lowercase().contains("font")
                                            && decode_literal(text, &theme_field("fontFamily"))
                                                .is_some())
                                        .then_some("font")
                                    })
                                    .or_else(|| number(text).map(|_| "number"));
                                if let Some(kind) = kind.filter(|_| text.len() <= 64 * 1024) {
                                    result.push(Design {
                                        name: name.into(),
                                        owner: owner.map(str::to_owned),
                                        span,
                                        kind,
                                        integer: prefix.iter().any(|token| token.text == "int"),
                                        constant: prefix.iter().any(|token| token.text == "const")
                                            && !getter,
                                    });
                                }
                            }
                        }
                        part = scan + 1;
                    } else {
                        scan = tokens[scan].close.unwrap_or(scan);
                    }
                    scan += 1;
                }
            }
            first = cursor + 1;
        }
    }
    let mut result = vec![];
    body(source, tokens, 0, tokens.len(), None, &mut result);
    for class in components::classes(tokens) {
        body(
            source,
            tokens,
            class.open + 1,
            class.close,
            Some(&class.name),
            &mut result,
        );
    }
    result.sort_by_key(|item| tokens[item.span.first].start);
    result
}

fn value_field(
    name: &str,
    kind: &str,
    value: Option<Value>,
    expression: Option<&str>,
    editable: bool,
    position: Option<usize>,
) -> Value {
    json!({"name":name,"type":kind,"value":value,"expression":expression.map(|text| text.chars().take(220).collect::<String>()),
        "present":expression.is_some(),"editable":editable,"position":position})
}

fn decode(text: &str, kind: &str) -> Option<Value> {
    match kind {
        "color" => color(text),
        "number" | "radius" => number(text),
        "font" => decode_literal(text, &theme_field("fontFamily")),
        "weight" => text
            .trim()
            .strip_prefix("FontWeight.")
            .filter(|value| {
                matches!(
                    *value,
                    "normal"
                        | "bold"
                        | "w100"
                        | "w200"
                        | "w300"
                        | "w400"
                        | "w500"
                        | "w600"
                        | "w700"
                        | "w800"
                        | "w900"
                )
            })
            .map(|value| json!(value)),
        "fontStyle" => text
            .trim()
            .strip_prefix("FontStyle.")
            .filter(|value| matches!(*value, "normal" | "italic"))
            .map(|value| json!(value)),
        _ => None,
    }
}

fn describe(source: &str, tokens: &[Token<'_>], item: &Design) -> Value {
    let text = item.span.text(tokens, source);
    let mut fields = vec![];
    if matches!(item.kind, "color" | "number" | "font") {
        let value = decode(text, item.kind);
        fields.push(value_field(
            "value",
            item.kind,
            value.clone(),
            Some(text),
            value.is_some(),
            None,
        ));
    } else if let Some((root, member, open, _)) = call(tokens, item.span) {
        let arguments = args(tokens, open);
        let names: &[(&str, &str)] = match (root, member) {
            ("TextStyle", "") => &[
                ("fontSize", "number"),
                ("fontWeight", "weight"),
                ("fontStyle", "fontStyle"),
                ("fontFamily", "font"),
                ("color", "color"),
                ("backgroundColor", "color"),
                ("height", "number"),
                ("letterSpacing", "number"),
                ("wordSpacing", "number"),
            ],
            ("EdgeInsets" | "EdgeInsetsDirectional", "symmetric") => {
                &[("horizontal", "number"), ("vertical", "number")]
            }
            ("EdgeInsets", "only") => &[
                ("left", "number"),
                ("top", "number"),
                ("right", "number"),
                ("bottom", "number"),
            ],
            ("EdgeInsetsDirectional", "only") => &[
                ("start", "number"),
                ("top", "number"),
                ("end", "number"),
                ("bottom", "number"),
            ],
            ("BorderRadius", "only") => &[
                ("topLeft", "radius"),
                ("topRight", "radius"),
                ("bottomLeft", "radius"),
                ("bottomRight", "radius"),
            ],
            ("BorderRadiusDirectional", "only") => &[
                ("topStart", "radius"),
                ("topEnd", "radius"),
                ("bottomStart", "radius"),
                ("bottomEnd", "radius"),
            ],
            ("BorderRadius", "vertical") => &[("top", "radius"), ("bottom", "radius")],
            ("BorderRadius", "horizontal") => &[("left", "radius"), ("right", "radius")],
            _ => &[],
        };
        for (name, kind) in names {
            let arg = named(&arguments, name);
            let expression = arg.map(|arg| arg.expression(tokens, source));
            let value = if *kind == "radius" {
                arg.and_then(|arg| radius_value(tokens, source, arg))
            } else {
                expression.and_then(|text| decode(text, kind))
            };
            let paint_conflict = matches!(*name, "color" | "backgroundColor")
                && named(
                    &arguments,
                    if *name == "color" {
                        "foreground"
                    } else {
                        "background"
                    },
                )
                .is_some();
            fields.push(value_field(
                name,
                kind,
                value.clone(),
                expression,
                !paint_conflict && (arg.is_none() || value.is_some()),
                None,
            ));
        }
        let positions: &[&str] = match (root, member) {
            ("EdgeInsets" | "EdgeInsetsDirectional", "all")
            | ("BorderRadius" | "BorderRadiusDirectional", "circular" | "all") => &["value"],
            ("EdgeInsets", "fromLTRB") => &["left", "top", "right", "bottom"],
            ("EdgeInsetsDirectional", "fromSTEB") => &["start", "top", "end", "bottom"],
            _ => &[],
        };
        for (position, name) in positions.iter().enumerate() {
            if let Some(arg) = arguments
                .iter()
                .filter(|arg| arg.name.is_none())
                .nth(position)
                .copied()
            {
                let radius = root.starts_with("BorderRadius") && member == "all";
                let text = arg.expression(tokens, source);
                let value = if radius {
                    radius_value(tokens, source, arg)
                } else {
                    number(text)
                };
                fields.push(value_field(
                    name,
                    if radius { "radius" } else { "number" },
                    value.clone(),
                    Some(text),
                    value.is_some(),
                    Some(position),
                ));
            }
        }
    }
    let reference = item.owner.as_ref().map_or_else(
        || item.name.clone(),
        |owner| format!("{owner}.{}", item.name),
    );
    json!({"id":tokens[item.span.first].start.to_string(),"name":reference,"kind":item.kind,"integer":item.integer,"constant":item.constant,
        "value":fields.first().and_then(|field| field.get("value")).cloned(),
        "public":!item.name.starts_with('_') && item.owner.as_ref().is_none_or(|owner| !owner.starts_with('_')),
        "line":source[..tokens[item.span.first].start].bytes().filter(|byte| *byte==b'\n').count()+1,
        "expression":text,"fields":fields,"part":tokens.windows(2).any(|pair| pair[0].text=="part" && pair[1].text=="of")})
}

fn radius_value(tokens: &[Token<'_>], source: &str, arg: Arg<'_>) -> Option<Value> {
    if arg.expression(tokens, source).ends_with("Radius.zero") {
        return Some(json!(0));
    }
    let (root, member, open, _) = call(
        tokens,
        Span {
            first: arg.expression,
            ..arg.span
        },
    )?;
    (root == "Radius" && member == "circular")
        .then(|| {
            args(tokens, open)
                .first()
                .and_then(|arg| number(arg.expression(tokens, source)))
        })
        .flatten()
}

pub(crate) fn design_values(source: &str) -> Result<Value> {
    let tokens = tokenize(source)?;
    Ok(json!(
        declarations(source, &tokens)
            .iter()
            .take(500)
            .map(|item| describe(source, &tokens, item))
            .collect::<Vec<_>>()
    ))
}

fn literal(value: &Value, kind: &str, integer: bool) -> Result<String> {
    let text = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    match kind {
        "number" | "radius" => {
            let number = text
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite() && (!integer || value.fract() == 0.0))
                .ok_or_else(|| Problem::new(422, "Enter a finite number of the expected type."))?;
            if kind == "radius" && number < 0.0 {
                return Err(Problem::new(422, "Corner radii cannot be negative."));
            }
            Ok(if number.fract() == 0.0 {
                format!("{number:.0}")
            } else {
                number.to_string()
            })
        }
        "color" => {
            let text = text.trim();
            if let Some(hex) = text.strip_prefix('#').filter(|hex| {
                matches!(hex.len(), 6 | 8) && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            }) {
                return Ok(format!(
                    "Color(0x{}{})",
                    if hex.len() == 6 { "ff" } else { "" },
                    hex.to_ascii_lowercase()
                ));
            }
            encode_literal(&json!(text), &theme_field("seedColor"))
        }
        "font" => encode_literal(&json!(text), &theme_field("fontFamily")),
        "weight" | "fontStyle" => {
            let prefix = if kind == "weight" {
                "FontWeight"
            } else {
                "FontStyle"
            };
            let expression = format!("{prefix}.{text}");
            decode(&expression, kind)
                .ok_or_else(|| Problem::new(422, "Choose a supported text style value."))?;
            Ok(expression)
        }
        _ => Err(Problem::new(422, "Edit this value as a Dart expression.")),
    }
}

pub(crate) fn edit_design_value(source: &str, input: &Value) -> Result<String> {
    let id = input["id"].as_str().unwrap_or_default();
    let mut result = source.to_owned();
    let tokens = tokenize(source)?;
    let items = declarations(source, &tokens);
    let item = items
        .iter()
        .find(|item| tokens[item.span.first].start.to_string() == id)
        .ok_or_else(|| Problem::new(409, "Refresh the design values before editing this value."))?;
    if input["mode"] == "expression" {
        let expression = properties::expression(&input["expression"])?;
        return Ok(replace(
            source,
            tokens[item.span.first].start,
            tokens[item.span.last].end,
            &expression,
        ));
    }
    let entry = describe(source, &tokens, item);
    for field in entry["fields"].as_array().unwrap() {
        let name = field["name"].as_str().unwrap();
        let Some(value) = input["fields"]
            .get(name)
            .filter(|value| !value.is_null() && value.as_str().is_none_or(|text| !text.is_empty()))
        else {
            continue;
        };
        let old = &field["value"];
        if value == old
            || value.as_str().is_some_and(|text| {
                old.as_str() == Some(text)
                    || text
                        .parse::<f64>()
                        .ok()
                        .zip(old.as_f64())
                        .is_some_and(|(a, b)| a == b)
            })
        {
            continue;
        }
        if field["editable"] != true {
            return Err(Problem::new(
                422,
                "This field is defined by a Dart expression. Use its Dart editor.",
            ));
        }
        let kind = field["type"].as_str().unwrap();
        if item.kind == "radius"
            && value
                .as_str()
                .and_then(|value| value.parse::<f64>().ok())
                .or_else(|| value.as_f64())
                .is_some_and(|value| value < 0.0)
        {
            return Err(Problem::new(422, "Corner radii cannot be negative."));
        }
        let mut value = literal(value, kind, item.integer)?;
        let tokens = tokenize(&result)?;
        let current = declarations(&result, &tokens)
            .into_iter()
            .find(|current| tokens[current.span.first].start.to_string() == id)
            .unwrap();
        if let Some((_, _, _, prefix)) = call(&tokens, current.span) {
            if ["Color(", "FontWeight.", "FontStyle."]
                .iter()
                .any(|root| value.starts_with(root))
            {
                value = format!("{prefix}{value}");
            }
        }
        if matches!(current.kind, "color" | "number" | "font") {
            result = replace(
                &result,
                tokens[current.span.first].start,
                tokens[current.span.last].end,
                &value,
            );
        } else {
            let (_, _, open, prefix) = call(&tokens, current.span).ok_or_else(|| {
                Problem::new(422, "Use the Dart expression editor for this value.")
            })?;
            let value = if kind == "radius" {
                format!("{prefix}Radius.circular({value})")
            } else {
                value
            };
            if let Some(position) = field["position"].as_u64() {
                let arg = args(&tokens, open)
                    .into_iter()
                    .filter(|arg| arg.name.is_none())
                    .nth(position as usize)
                    .unwrap();
                if kind == "radius" {
                    if let Some((_, _, inner, _)) = call(
                        &tokens,
                        Span {
                            first: arg.expression,
                            ..arg.span
                        },
                    ) {
                        let number = args(&tokens, inner)[0];
                        let numeric = literal(input["fields"].get(name).unwrap(), "number", false)?;
                        result = replace(
                            &result,
                            tokens[number.expression].start,
                            tokens[number.span.last].end,
                            &numeric,
                        );
                        continue;
                    }
                }
                result = replace(
                    &result,
                    tokens[arg.expression].start,
                    tokens[arg.span.last].end,
                    &value,
                );
            } else {
                if kind == "radius" {
                    if let Some(arg) = named(&args(&tokens, open), name) {
                        if let Some(("Radius", "circular", inner, _)) = call(
                            &tokens,
                            Span {
                                first: arg.expression,
                                ..arg.span
                            },
                        ) {
                            let number = args(&tokens, inner)[0];
                            let numeric =
                                literal(input["fields"].get(name).unwrap(), "number", false)?;
                            result = replace(
                                &result,
                                tokens[number.expression].start,
                                tokens[number.span.last].end,
                                &numeric,
                            );
                            continue;
                        }
                    }
                }
                result = set_named(&result, &tokens, open, name, &value);
            }
        }
    }
    Ok(result)
}

pub(crate) fn design_reference(source: &str, input: &Value) -> Result<Value> {
    design_values(source)?
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == input["valueId"])
        .cloned()
        .ok_or_else(|| Problem::new(409, "Refresh the design values before using this value."))
}

pub(crate) fn design_binding(source: &str, command: &Value, scope: &Scope) -> Result<String> {
    let (tokens, nodes) = inspect_in(source, scope)?;
    let node = nodes
        .iter()
        .find(|node| command["id"] == node.id)
        .ok_or_else(|| Problem::new(409, "Select the widget again."))?;
    let name = command["name"].as_str().unwrap_or_default();
    if let Some(field) = node.spec.fields.iter().find(|field| {
        field.name == name
            || field
                .nest
                .as_ref()
                .is_some_and(|(outer, _)| format!("{outer}.{}", field.name) == name)
    }) {
        if !property(node, field, &tokens).editable {
            return Err(Problem::new(
                422,
                "Edit the containing style or decoration instead of setting this property directly.",
            ));
        }
    }
    edit_in(source, command, scope)
}

pub(crate) fn design_import(
    source: &str,
    uris: &[String],
    name: &str,
) -> Result<(String, String, usize)> {
    import_reference(source, uris, name, "freeflowTheme")
}

pub(crate) fn import_reference(
    source: &str,
    uris: &[String],
    name: &str,
    base: &str,
) -> Result<(String, String, usize)> {
    let tokens = tokenize(source)?;
    if tokens
        .windows(2)
        .any(|pair| pair[0].text == "part" && pair[1].text == "of")
    {
        return Err(Problem::new(
            422,
            "This file is part of another Dart library. Add the reference in its library file or use a value from this file.",
        ));
    }
    for index in 0..tokens.len() {
        if at(&tokens, index) != "import" {
            continue;
        }
        let Some(uri) = decode_literal(at(&tokens, index + 1), &theme_field("fontFamily"))
            .and_then(|value| value.as_str().map(str::to_owned))
        else {
            continue;
        };
        if !uris.contains(&uri) {
            continue;
        }
        let end = (index + 2..tokens.len())
            .find(|index| at(&tokens, *index) == ";")
            .unwrap_or(tokens.len());
        if tokens[index + 2..end]
            .iter()
            .any(|token| matches!(token.text, "if" | "deferred" | "show" | "hide"))
        {
            continue;
        }
        let prefix = tokens[index + 2..end]
            .windows(2)
            .find(|pair| pair[0].text == "as")
            .map(|pair| pair[1].text);
        if prefix == Some("_") {
            continue;
        }
        return Ok((
            source.into(),
            prefix.map_or_else(|| name.into(), |prefix| format!("{prefix}.{name}")),
            0,
        ));
    }
    let mut prefix = base.to_owned();
    let mut suffix = 2;
    while tokens.iter().any(|token| token.text == prefix) {
        prefix = format!("{base}{suffix}");
        suffix += 1;
    }
    let eol = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut insertion = usize::from(source.starts_with('\u{feff}')) * '\u{feff}'.len_utf8();
    if source[insertion..].starts_with("#!") {
        insertion = source.find('\n').map_or(source.len(), |line| line + 1);
    }
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token.text, "import" | "export" | "part") {
            insertion = token.start;
            break;
        }
        if token.text == "library" {
            insertion = tokens[index..]
                .iter()
                .find(|token| token.text == ";")
                .map_or(insertion, |token| token.end);
        }
    }
    let uri = serde_json::to_string(&uris[0]).unwrap().replace('$', "\\$");
    let import = format!(
        "{}import {uri} as {prefix};{eol}",
        if insertion > 0 && !source[..insertion].ends_with('\n') {
            eol
        } else {
            ""
        }
    );
    Ok((
        replace(source, insertion, insertion, &import),
        format!("{prefix}.{name}"),
        import.encode_utf16().count(),
    ))
}
