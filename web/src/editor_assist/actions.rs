use super::*;
use crate::dialogs::{self, View};
use crate::dom::Error;
use std::rc::Rc;

pub(crate) struct Menu {
    document: Document,
    actions: Vec<Value>,
}

pub(super) async fn open(request: Option<Value>) -> Result {
    hide_popup();
    let Some(document) = Document::current() else {
        return Ok(());
    };
    let (start, end) = request
        .as_ref()
        .and_then(|request| range(&document.text, request))
        .unwrap_or((document.start, document.end));
    let result = project_post(
        &document.project,
        "editor",
        json!({
            "action":"code-actions","path":document.path,"text":document.text,
            "offset":start,"endOffset":end
        }),
    )
    .await?;
    if !document.still_current(request.is_none()) {
        return Ok(());
    }
    let actions = items(&result["actions"]).to_vec();
    let body = if actions.is_empty() {
        r#"<p class="dialog-help">No actions available at this position in the current file.</p>"#
            .into()
    } else {
        let options:String=actions.iter().enumerate().map(|(index,action)| {
            format!(r#"<button type="button" class="button outline" data-dialog-action="dart-action-{index}">{}</button>"#,h(string(&action["title"])))
        }).collect();
        format!(r#"<div class="form">{options}</div>"#)
    };
    let title = if request.is_some() {
        "Quick fixes"
    } else {
        "Dart code actions"
    };
    dialogs::open(
        View::DartActions {
            actions: Rc::new(Menu { document, actions }),
        },
        title,
        "Apply a suggestion to the current Dart editor.",
        body,
    );
    Ok(())
}

pub(crate) fn apply(menu: &Menu, version: u64, action: &str) -> Result {
    let Some(index) = action
        .strip_prefix("dart-action-")
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return Ok(());
    };
    if !menu.document.still_current(false) {
        return Err(Error::new(
            "The Dart buffer changed. Request code actions again.",
        ));
    }
    let source = &menu.document.text;
    let edits = menu
        .actions
        .get(index)
        .and_then(|action| action["edits"].as_array())
        .and_then(|edits| {
            edits
                .iter()
                .enumerate()
                .map(|(index, edit)| {
                    let (start, end) = range(source, &edit["range"])?;
                    Some((start, end, edit["text"].as_str()?, index == 0))
                })
                .collect::<Option<Vec<_>>>()
        })
        .and_then(|edits| apply_ranges(source, edits))
        .ok_or_else(|| Error::new("Dart returned an invalid edit for this source."))?;
    dialogs::close_current(version);
    let _ = input().focus();
    crate::editor::apply_edit(&edits.0, edits.1);
    Ok(())
}
