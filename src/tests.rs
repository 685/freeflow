use super::*;
use serde_json::{Value, json};
use std::{fs, path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

fn fixture() -> (tempfile::TempDir, projects::Projects, projects::Project) {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("app");
    fs::create_dir_all(root.join("lib")).unwrap();
    fs::write(
        root.join("pubspec.yaml"),
        "name: example_app\n\ndependencies:\n  flutter:\n    sdk: flutter\n",
    )
    .unwrap();
    fs::write(
        root.join("lib/main.dart"),
        include_str!("../templates/daymark.dart"),
    )
    .unwrap();
    fs::write(root.join(".env"), "PRIVATE_VALUE=excluded").unwrap();
    let projects = projects::Projects::new(home.path().join("data"));
    let project = projects.import_folder(root.to_str().unwrap()).unwrap();
    (home, projects, project)
}

#[test]
fn live_inspector_prefers_the_matching_widget_column_and_project() {
    let (_home, projects, project) = fixture();
    let path = "lib/main.dart";
    let source = "Widget build() => Column(children: [Text('first'), Text('second')]);";
    fs::write(project.root.join(path), source).unwrap();
    let outline = visual::outline(source).unwrap();
    let second = outline["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|node| node["type"] == "Text")
        .nth(1)
        .unwrap();
    let location = json!({"file":"package:example_app/main.dart","line":second["line"],
        "column":second["column"],"type":"Text"});
    let hit = inspector::resolve(&projects, &project, &json!({"locations":[
        {"file":"package:unrelated/main.dart","line":second["line"],"column":second["column"],"type":"Text"},
        location,
    ]})).unwrap();
    assert_eq!(hit["widget"], second["id"]);
    assert_eq!(hit["path"], path);
    let outside = inspector::resolve(
        &projects,
        &project,
        &json!({"locations":[
            {"file":"package:example_app/../.env","line":1,"column":1,"type":"Text"},
        ]}),
    )
    .unwrap();
    assert!(outside.is_null());
}

#[test]
fn project_batch_checks_all_revisions_before_applying_an_agent_edit() {
    let (_home, projects, project) = fixture();
    let first = projects.file(&project, "lib/main.dart").unwrap();
    let edit = |revision| projects::FileEdit {
        path: first.path.clone(),
        revision,
        content: Some("void main() {}\n".into()),
    };
    let created = || projects::FileEdit {
        path: "lib/new.dart".into(),
        revision: None,
        content: Some("const value = 1;\n".into()),
    };
    let edits = [created(), edit(Some("outdated".into()))];
    assert_eq!(
        projects
            .apply_edits(&project, &edits)
            .unwrap_err()
            .status
            .as_u16(),
        409
    );
    assert!(!project.root.join("lib/new.dart").exists());
    projects
        .apply_edits(&project, &[created(), edit(first.revision.clone())])
        .unwrap();
    let added = projects.file(&project, "lib/new.dart").unwrap();
    let updated = projects.file(&project, &first.path).unwrap();
    projects
        .apply_edits(
            &project,
            &[
                projects::FileEdit {
                    path: added.path,
                    revision: added.revision,
                    content: None,
                },
                projects::FileEdit {
                    path: first.path.clone(),
                    revision: updated.revision,
                    content: Some(first.content.clone()),
                },
            ],
        )
        .unwrap();
    assert_eq!(
        projects.file(&project, &first.path).unwrap().revision,
        first.revision
    );
    assert!(!project.root.join("lib/new.dart").exists());
}

#[test]
fn theme_updates_preserve_other_theme_and_color_scheme_arguments() {
    let source = "Widget build() => MaterialApp(theme: ThemeData(useMaterial3: true, colorScheme: ColorScheme.fromSeed(seedColor: old, brightness: Brightness.dark)));";
    let apps = visual::themes(source).unwrap();
    let input = json!({"id":apps[0]["id"],"seedColor":"#35547e",
        "scaffoldBackgroundColor":"#f3f5f8","fontFamily":"L'été"});
    let changed = visual::edit_theme(source, &input).unwrap();
    assert!(changed.contains("useMaterial3: true"));
    assert!(changed.contains("brightness: Brightness.dark"));
    let theme = visual::themes(&changed).unwrap();
    assert_eq!(theme[0]["seedColor"]["value"], "#35547e");
    assert_eq!(theme[0]["scaffoldBackgroundColor"]["value"], "#f3f5f8");
    assert_eq!(theme[0]["fontFamily"]["value"], "L'été");
}

#[test]
fn visual_edits_preserve_dart_expressions_and_siblings() {
    let before = include_str!("../templates/daymark.dart");
    let outline = visual::outline(before).unwrap();
    let nodes = outline["nodes"].as_array().unwrap();
    let title = nodes
        .iter()
        .find(|node| node["label"] == "Make today\ncount.")
        .unwrap();
    let text = "Heute ist dein Tag.\nIt's $10. 🎯";
    let updated = visual::edit(
        before,
        &json!({"id":title["id"],"operation":"property","name":"text","value":text}),
    )
    .unwrap();
    assert!(
        visual::outline(&updated).unwrap()["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["id"] == title["id"] && node["label"] == text)
    );
    let dynamic = nodes
        .iter()
        .find(|node| {
            node["properties"].as_array().unwrap().iter().any(|field| {
                field["expression"]
                    .as_str()
                    .is_some_and(|text| text.contains("_completed.length"))
                    && field["name"] == "text"
            })
        })
        .unwrap();
    let blocked = visual::edit(
        before,
        &json!({"id":dynamic["id"],"operation":"property","name":"text","value":"Other"}),
    )
    .unwrap_err();
    assert_eq!(blocked.status.as_u16(), 422);

    let source =
        "Widget build(BuildContext context) => const Column(children: [Text('a'), Text('b')]);";
    let tree = visual::outline(source).unwrap();
    let nodes = tree["nodes"].as_array().unwrap();
    let moved = visual::edit(
        source,
        &json!({"id":nodes[1]["id"],"operation":"move","target":nodes[2]["id"],"side":"after"}),
    )
    .unwrap();
    assert_eq!(
        moved,
        "Widget build(BuildContext context) => const Column(children: [Text('b'), Text('a')]);"
    );
    let added = visual::edit(
        source,
        &json!({"id":nodes[0]["id"],"operation":"add","slot":"children","widget":"Divider"}),
    )
    .unwrap();
    assert!(added.contains("Divider()"));
}

#[test]
fn project_revisions_history_and_source_archives() {
    let (temp, projects, project) = fixture();
    let first = projects.file(&project, "lib/main.dart").unwrap();
    let edited = projects
        .save(
            &project,
            &first.path,
            format!("{}\n// Edit\n", first.content),
            first.revision.as_deref(),
        )
        .unwrap();
    assert_ne!(edited.revision, first.revision);
    let error = projects
        .save(
            &project,
            &first.path,
            "stale".into(),
            first.revision.as_deref(),
        )
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 409);
    assert_eq!(
        projects
            .file(&project, "../main.dart")
            .unwrap_err()
            .status
            .as_u16(),
        400
    );
    assert_eq!(
        projects.file(&project, ".env").unwrap_err().status.as_u16(),
        400
    );
    let entries = projects.history(&project, &first.path).unwrap();
    let old = projects
        .history_version(&project, &first.path, entries[0]["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(old["content"], first.content);

    let path = temp.path().join("source.zip");
    archive::zip_folder(&project.root, &path, true, &CancellationToken::new()).unwrap();
    let zip = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    assert!(zip.file_names().any(|name| name == "lib/main.dart"));
    assert!(!zip.file_names().any(|name| name == ".env"));
}

#[test]
fn legacy_project_metadata_and_settings_open_without_conversion() {
    let (_temp, projects, project) = fixture();
    files::write_json(
        &projects.home.join("projects.json"),
        &json!([{
            "id":project.id, "name":"Existing app", "root":project.root, "template":"imported",
            "createdAt":"2026-10-21T09:02:01.000Z"
        }]),
    )
    .unwrap();
    files::write_json(
        &projects.home.join("settings.json"),
        &json!({"offline":true}),
    )
    .unwrap();
    let record = projects.get(&project.id).unwrap();
    assert_eq!(record.name, "Existing app");
    assert_eq!(record.created_at, "2026-10-21T09:02:01.000Z");
    let settings = flutter::Flutter::new(projects.home.clone())
        .unwrap()
        .settings()
        .unwrap();
    assert!(settings.offline);
    assert!(settings.managed_auto_update);
}

#[test]
fn backends_generate_initialization_and_reject_privileged_keys() {
    let config = backends::validate(
        "supabase",
        &json!({"url":"https://demo.supabase.co","key":"sb_publishable_example"}),
    )
    .unwrap()
    .unwrap();
    assert_eq!(config["url"], "https://demo.supabase.co");
    assert!(
        backends::validate(
            "supabase",
            &json!({"url":config["url"],"key":"sb_secret_example"})
        )
        .is_err()
    );
    let source = backends::generate(true, true);
    assert!(source.contains("Firebase.initializeApp"));
    assert!(source.contains("Supabase.initialize"));
    assert!(source.contains("FIREBASE_IOS_APP_ID"));
    assert!(!source.contains(&config["key"]));
}

#[tokio::test]
async fn workbench_routes_enforce_local_token_and_project_scope() {
    let (temp, projects, project) = fixture();
    let assets = temp.path().join("web");
    fs::create_dir(&assets).unwrap();
    for file in ["freeflow_web.js", "freeflow_web_bg.wasm"] {
        fs::write(assets.join(file), "").unwrap();
    }
    files::write_json(
        &projects.home.join("settings.json"),
        &json!({"offline":true}),
    )
    .unwrap();
    let listener = server::bind(0).await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let server = tokio::spawn(server::serve(
        listener,
        projects.home.clone(),
        assets,
        std::env::current_exe().unwrap(),
        true,
        stop.clone().cancelled_owned(),
    ));
    let session = wait_for(|| {
        files::read_json::<files::Session>(&projects.home.join("session.json"))
            .ok()
            .flatten()
    })
    .await;
    let client = mcp::client(Duration::from_secs(5));
    let status = client
        .get(format!("{origin}/api/state"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status.as_u16(), 401);
    let status = client
        .get(format!("{origin}/api/state"))
        .header("Origin", "https://example.invalid")
        .header("X-Freeflow-Token", &session.token)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status.as_u16(), 403);
    let body = json!({"name":"read_file","scope":project.id,"arguments":{"path":"lib/main.dart"}});
    let response = client
        .post(format!("{origin}/api/mcp"))
        .header("X-Freeflow-Token", &session.token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let file: Value = response.json().await.unwrap();
    assert!(file["content"].as_str().unwrap().contains("Daymark"));
    let mut wrong = body;
    wrong["arguments"]["projectId"] = json!("01234567");
    let response = client
        .post(format!("{origin}/api/mcp"))
        .header("X-Freeflow-Token", &session.token)
        .json(&wrong)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 403);
    stop.cancel();
    server.await.unwrap().unwrap();
}

async fn wait_for<T>(mut check: impl FnMut() -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(result) = check() {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .expect("operation should finish")
}

#[cfg(unix)]
fn fake_flutter(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(
        path,
        r#"#!/bin/sh
if [ "$3" = pub ]; then exit 0; fi
printf '%s\n' "$9" > "$TMPDIR/preview-port"
while [ ! -f "$TMPDIR/announce" ]; do sleep 0.1; done
printf '%s\n' '[{"event":"app.webLaunchUrl"}]'
while :; do sleep 1; done
"#,
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(windows)]
fn fake_flutter(path: &Path) {
    let lines = [
        "@echo off",
        "if \"%~3\"==\"pub\" exit /b 0",
        "> \"%TEMP%\\preview-port\" echo %~9",
        ":waiting",
        "if exist \"%TEMP%\\announce\" goto ready",
        "ping.exe 127.0.0.1 -n 2 >nul",
        "goto waiting",
        ":ready",
        "echo [{\"event\":\"app.webLaunchUrl\"}]",
        ":hold",
        "ping.exe 127.0.0.1 -n 2 >nul",
        "goto hold",
    ];
    fs::write(path, lines.join("\r\n")).unwrap();
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn preview_waits_for_flutter_compilation_even_if_http_responds() {
    let (temp, projects, project) = fixture();
    let path = temp.path().join(if cfg!(windows) {
        "fake flutter.cmd"
    } else {
        "fake-flutter"
    });
    fake_flutter(&path);
    files::write_json(
        &projects.home.join("settings.json"),
        &json!({"flutterPath":path,"offline":true}),
    )
    .unwrap();
    let tmp = projects.home.join("tmp/flutter");
    let flutter = Arc::new(flutter::Flutter::new(projects.home.clone()).unwrap());
    let jobs = jobs::Jobs::new(Arc::new(projects), flutter, None);
    jobs.start_preview(project.clone()).unwrap();
    let port = wait_for(|| {
        fs::read_to_string(tmp.join("preview-port"))
            .ok()
            .and_then(|value| value.trim().parse::<u16>().ok())
    })
    .await;
    let listener = server::bind(port).await.unwrap();
    let app = axum::Router::new().route("/", axum::routing::get(|| async { "<!doctype html>" }));
    let http = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let url = format!("http://127.0.0.1:{port}/");
    let response = mcp::client(Duration::from_secs(2))
        .get(&url)
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "<!doctype html>");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(jobs.preview(&project.id)["status"], "running");
    assert!(jobs.preview(&project.id)["result"]["url"].is_null());
    fs::write(tmp.join("announce"), "").unwrap();
    let ready = wait_for(|| {
        let view = jobs.preview(&project.id);
        (view["status"] == "ready").then_some(view)
    })
    .await;
    assert_eq!(ready["result"]["url"], url);
    assert_eq!(jobs.stop_preview(&project.id).await["status"], "stopped");
    http.abort();
    jobs.close().await;
}

#[cfg(windows)]
#[tokio::test]
async fn windows_batch_launcher_handles_spaced_paths() {
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().join("folder & tools");
    fs::create_dir(&folder).unwrap();
    let program = folder.join("check.cmd");
    fs::write(&program, "@echo off\r\necho %~1\r\n").unwrap();
    let mut lines = vec![];
    let command = processes::ProcessCommand::new(program, ["an argument"]);
    let result = processes::run(&command, Some(&folder), &CancellationToken::new(), |line| {
        lines.push(line)
    })
    .await
    .unwrap();
    assert!(result.success());
    assert_eq!(lines, ["an argument"]);
}
