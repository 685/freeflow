use crate::{
    dialogs::{self, View, current_view},
    dom::{self, Error, Result, escape as h, id, items, project_get, project_post, string, toast},
    state,
};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::HtmlInputElement;

pub async fn open(target: &str) -> Result {
    let project = crate::project_id()?;
    let records = project_get(&project, "artifacts").await?;
    let receipt = project_get(&project, "store-upload").await?;
    let signing = if target == "android" {
        project_get(&project, "signing").await?
    } else {
        Value::Null
    };
    if !crate::current(&project) {
        return Ok(());
    }
    let title = if target == "android" {
        "Upload to Google Play"
    } else {
        "Upload to App Store Connect"
    };
    let name = state::read(|state| string(&state.project["name"]).to_owned());
    let releases: String = items(&records)
        .iter()
        .filter(|artifact| artifact["target"] == target && artifact["debugSigned"] != true)
        .take(30)
        .map(|artifact| {
            format!(
                r#"<option value="{}">{} · {}</option>"#,
                h(string(&artifact["id"])),
                h(string(&artifact["name"])),
                h(string(&artifact["date"])),
            )
        })
        .collect();
    let fields = if target == "android" {
        format!(
            r#"<p class="dialog-help">Use a service account authorized for this app in Play Console. Upload the first release through Play Console to enable API updates.</p>
            <label class="form-field"><span>Package name</span><input name="package" required maxlength="200" value="{}" placeholder="com.example.app"></label>
            <label class="form-field"><span>Release track</span><input name="track" required maxlength="80" value="internal" placeholder="internal or production"></label>
            <label class="form-field"><span>Google service account JSON</span><input id="store-key" type="file" accept=".json,application/json" required></label>
            <label class="check-row"><input name="draft" type="checkbox" checked> Save as draft</label>
            <label class="check-row"><input name="holdReview" type="checkbox"> Keep changes out of automatic review</label>"#,
            h(string(&signing["appId"])),
        )
    } else {
        r#"<p class="dialog-help">Create the app in App Store Connect first. Apple processes the upload before the build appears in TestFlight and App Store Connect.</p>
        <div class="form-grid">
            <label class="form-field"><span>Key ID</span><input name="keyId" required maxlength="10"></label>
            <label class="form-field"><span>Issuer ID</span><input name="issuer" required maxlength="36"></label>
        </div>
        <label class="form-field"><span>App Store Connect private key</span><input id="store-key" type="file" accept=".p8" required></label>"#.into()
    };
    let last = receipt
        .get(target)
        .filter(|receipt| receipt.is_object())
        .map(|receipt| {
            let name = h(string(&receipt["name"]));
            let date = h(string(&receipt["date"]));
            format!(r#"<p class="dialog-help">Last accepted upload · {name} · {date}</p>"#)
        })
        .unwrap_or_default();
    let details = if releases.is_empty() {
        r#"<p>Build a signed release export first.</p>"#.into()
    } else {
        format!(
            r#"<form class="form">
            <label class="form-field"><span>Signed release build</span><select aria-label="Signed release build" name="artifact" required>{releases}</select></label>
            {fields}
            <div class="form-error" role="alert"></div>
            <div class="form-footer"><button type="submit" class="button primary">Upload build</button></div>
        </form>"#
        )
    };
    let body = format!(
        r#"{details}{last}<div class="form-footer"><button type="button" class="button soft" data-dialog-action="store-back">Back to export</button></div>"#
    );
    dialogs::open(
        View::StoreUpload {
            project,
            target: target.into(),
        },
        title,
        &name,
        body,
    );
    Ok(())
}

pub async fn submit(project: &str, target: &str, version: u64, mut input: Value) -> Result {
    let field: HtmlInputElement = id("store-key").unchecked_into();
    let file = field
        .files()
        .and_then(|files| files.get(0))
        .ok_or_else(|| Error::new("Choose the key file issued by your store."))?;
    if file.size() > (64 * 1024) as f64 {
        return Err(Error::new("The store key exceeds 64 KiB."));
    }
    if target == "android"
        && input["draft"] != true
        && !dom::win().confirm_with_message(&format!(
            "Submit this build to the {} track for {}?",
            string(&input["track"]),
            string(&input["package"])
        ))?
    {
        return Ok(());
    }
    let key = JsFuture::from(file.text())
        .await?
        .as_string()
        .unwrap_or_default();
    input["key"] = json!(key);
    input["target"] = json!(target);
    let job = project_post(project, "store-upload", input).await?;
    field.set_value("");
    crate::track(&job, true);
    let receipt = crate::wait_job(job).await?;
    if current_view(version) && crate::current(project) {
        open(target).await?;
    }
    let message = if target == "android" {
        format!(
            "Google Play accepted version {} on {}.",
            receipt["version"],
            string(&receipt["track"])
        )
    } else {
        "App Store Connect accepted the build for processing.".into()
    };
    toast(&message, false);
    Ok(())
}
