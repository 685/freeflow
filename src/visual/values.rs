use super::*;

const MAX_ROWS: usize = 120;

fn identifier(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic()
                || matches!(byte, b'_' | b'$')
                || index > 0 && byte.is_ascii_digit()
        })
}

fn type_parts(kind: &str) -> (&str, Vec<&str>) {
    let kind = kind.trim().trim_end_matches('?');
    let Some((base, arguments)) = kind.strip_suffix('>').and_then(|kind| kind.split_once('<'))
    else {
        return (kind, vec![]);
    };
    let mut arguments_start = 0;
    let mut depth = 0;
    let mut parts = vec![];
    for (index, ch) in arguments.char_indices() {
        match ch {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(arguments[arguments_start..index].trim());
                arguments_start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(arguments[arguments_start..].trim());
    (base, parts)
}

fn map_colon(tokens: &[Token<'_>], span: Span) -> Option<usize> {
    if matches!(at(tokens, span.first), "if" | "for" | ".") {
        return None;
    }
    let mut branches = 0;
    let mut index = span.first;
    while index <= span.last {
        if question(tokens, index) {
            branches += 1;
        } else if at(tokens, index) == ":" {
            if branches == 0 {
                return Some(index);
            }
            branches -= 1;
        }
        index = tokens[index]
            .close
            .unwrap_or_else(|| type_end(tokens, index, span.last + 1))
            + 1;
    }
    None
}

// Locate an entire literal or explicit call. A conditional, selector or
// computation outside the closing delimiter stays a single Dart expression.
fn opening(tokens: &[Token<'_>]) -> Option<usize> {
    let mut cursor = usize::from(matches!(at(tokens, 0), "const" | "new"));
    if at(tokens, cursor) == "<" {
        let end = type_end(tokens, cursor, tokens.len());
        if end == cursor {
            return None;
        }
        cursor = end + 1;
    }
    if matches!(at(tokens, cursor), "[" | "{") {
        return Some(cursor);
    }
    while identifier(at(tokens, cursor)) && !matches!(at(tokens, cursor), "if" | "for" | "switch") {
        cursor += 1;
        let end = type_end(tokens, cursor, tokens.len());
        if end > cursor {
            cursor = end + 1;
        }
        match at(tokens, cursor) {
            "." => cursor += 1,
            "(" => return Some(cursor),
            _ => break,
        }
    }
    None
}

fn structure(expression: &str, base: &str, types: &[&str], eol: &str) -> Result<Option<Value>> {
    let tokens = tokenize(expression)?;
    let Some(open) = opening(&tokens) else {
        return Ok(None);
    };
    let Some(close) = tokens[open]
        .close
        .filter(|close| *close + 1 == tokens.len())
    else {
        return Ok(None);
    };
    let spans = segments(&tokens, open, close);
    if spans.len() > MAX_ROWS {
        return Ok(None);
    }
    let typed = tokens
        .iter()
        .take(open)
        .find(|token| token.text == "<")
        .filter(|_| at(&tokens, open - usize::from(open > 0)) == ">")
        .map(|token| type_parts(&expression[token.start..tokens[open - 1].end]).1)
        .unwrap_or_default();
    let types = if typed.is_empty() { types } else { &typed };
    let kind = match tokens[open].text {
        "[" => "list",
        "(" => "call",
        "{" if base == "Map"
            || typed.len() == 2
            || spans.iter().any(|span| map_colon(&tokens, *span).is_some())
            || spans.is_empty() && base != "Set" && typed.is_empty() =>
        {
            "map"
        }
        _ => "set",
    };
    let mut boundary = tokens[open].end;
    let rows: Vec<_> = spans
        .iter()
        .map(|span| {
            let first = tokens[span.first].start;
            let end = tokens[span.last].end;
            let colon = match kind {
                "call"
                    if at(&tokens, span.first + 1) == ":"
                        && identifier(at(&tokens, span.first)) =>
                {
                    Some(span.first + 1)
                }
                "map" => map_colon(&tokens, *span),
                _ => None,
            }
            .filter(|colon| span.first < *colon && *colon < span.last);
            let (label, join, value) = colon.map_or((None, "", &expression[first..end]), |colon| {
                let split = tokens[colon - 1].end;
                let value = tokens[colon + 1].start;
                (
                    Some(&expression[first..split]),
                    &expression[split..value],
                    &expression[value..end],
                )
            });
            let comma = span.comma.map(|index| &tokens[index]);
            let row = json!({
                "before":&expression[boundary..first],"label":label,"join":join,
                "value":value,"after":comma.map_or("", |comma| &expression[end..comma.start])
            });
            boundary = comma.map_or(end, |comma| comma.end);
            row
        })
        .collect();
    let (label_type, item_type) = match kind {
        "map" => (types.first().copied(), types.get(1).copied()),
        "list" | "set" => (None, types.first().copied()),
        _ => (None, None),
    };
    let indent = rows.iter().find_map(|row| {
        let (_, indent) = row["before"].as_str()?.rsplit_once('\n')?;
        indent.trim().is_empty().then_some(indent)
    });
    Ok(Some(json!({
        "mode":kind,"rows":rows,"head":&expression[..tokens[open].end],"tail":&expression[boundary..],
        "trailingComma":spans.last().is_some_and(|span| span.comma.is_some()),
        "beforeNew":indent.map_or_else(|| " ".into(), |indent| format!("{eol}{indent}")),
        "labelType":label_type,"itemType":item_type
    })))
}

pub(crate) fn model(
    expression: &str,
    dart_type: &str,
    color_constructor: Option<&str>,
    eol: &str,
) -> Result<Value> {
    if expression.len() > 64 * 1024 || dart_type.len() > 512 || expression.contains('\0') {
        return Err(Problem::new(
            413,
            "Open large property values in the Dart editor.",
        ));
    }
    let (base, types) = type_parts(dart_type);
    let blank = expression.trim().is_empty();
    let scalar = match base {
        "String" if blank => Some(("text", json!(""))),
        "String" => {
            decode_literal(expression, &theme_field("fontFamily")).map(|value| ("text", value))
        }
        "bool" if blank || matches!(expression, "true" | "false") => {
            Some(("boolean", json!(expression == "true")))
        }
        "int" | "double" | "num" if blank || NUMBER.is_match(expression) => {
            Some(("number", json!(expression)))
        }
        "Color" if blank && color_constructor.is_some() => Some(("color", json!("#6750a4"))),
        _ => None,
    };
    if let Some((mode, value)) = scalar {
        return Ok(
            json!({"mode":mode,"value":value,"expression":expression,"colorConstructor":color_constructor}),
        );
    }
    if base == "Color" {
        let tokens = tokenize(expression)?;
        let open = opening(&tokens);
        if let Some(open) = open.filter(|open| *open > 0 && at(&tokens, open - 1) == "Color")
            && tokens[open].close == tokens.len().checked_sub(1)
            && let Some(color) = expression[tokens[open].end..tokens[tokens.len() - 1].start]
                .trim()
                .strip_prefix("0x")
            && color.len() == 8
            && color.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            let first = usize::from(matches!(at(&tokens, 0), "const" | "new"));
            let reference = &expression[tokens[first].start..tokens[open - 1].end];
            return Ok(
                json!({"mode":"color","value":format!("#{color}"),"expression":expression,"colorConstructor":reference}),
            );
        }
    }
    let code = if blank {
        match base {
            "List" | "Iterable" => "[]",
            "Map" | "Set" => "{}",
            _ => "",
        }
    } else {
        expression
    };
    if let Some(mut shape) = structure(code, base, &types, eol)? {
        shape["expression"] = json!(code);
        return Ok(shape);
    }
    Ok(json!({"mode":"dart","expression":expression}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_keep_type_prefix_trivia_and_nested_map_keys() {
        let text = "const <String, List<int>>{\r\n  // Keep me\r\n  (flag ? 'a' : 'b') /* key */ : [1, 2],\r\n  ...extra,\r\n}";
        let model = model(text, "Map<String, List<int>>?", None, "\r\n").unwrap();
        assert_eq!(model["mode"], "map");
        assert_eq!(model["head"], "const <String, List<int>>{");
        assert_eq!(model["rows"][0]["before"], "\r\n  // Keep me\r\n  ");
        assert_eq!(model["rows"][0]["label"], "(flag ? 'a' : 'b')");
        assert_eq!(model["rows"][0]["join"], " /* key */ : ");
        assert_eq!(model["rows"][1]["label"], Value::Null);
        assert_eq!(model["itemType"], "List<int>");
        assert_eq!(model["tail"], "\r\n}");
    }

    #[test]
    fn explicit_calls_appear_as_arguments_while_computed_values_stay_in_dart() {
        let model = |text| model(text, "", None, "\n").unwrap();
        assert_eq!(
            model("ui.Shadow(color: theme.color, blurRadius: 5)")["rows"][1]["label"],
            "blurRadius"
        );
        assert_eq!(
            model("builders.map((item) => item).toList().reversed")["mode"],
            "dart"
        );
        assert_eq!(model("flag ? [1] : [2]")["mode"], "dart");
        assert_eq!(model("makeList")["mode"], "dart");
    }
}
