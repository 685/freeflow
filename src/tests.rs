use super::*;
use serde_json::{Value, json};
use std::{fs, path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

#[test]
fn preview_hot_reload_requires_a_known_compatible_flutter_sdk() {
    let home = tempfile::tempdir().unwrap();
    let bin = home.path().join("flutter/bin");
    fs::create_dir_all(bin.join("cache")).unwrap();
    let launcher = if cfg!(windows) {
        "flutter.bat"
    } else {
        "flutter"
    };
    let mut sdk = flutter::Sdk {
        file: bin.join(launcher),
        version: None,
        mode: "external",
    };
    assert!(!sdk.web_hot_reload());
    let version = bin.join("cache/flutter.version.json");
    fs::write(&version, r#"{"frameworkVersion":"3.35.7"}"#).unwrap();
    assert!(!sdk.web_hot_reload());
    fs::write(&version, r#"{"frameworkVersion":"3.38.0"}"#).unwrap();
    assert!(sdk.web_hot_reload());
    fs::write(&version, r#"{"flutterVersion":"3.41.0-0.1.pre"}"#).unwrap();
    assert!(sdk.web_hot_reload());
    fs::write(&version, "malformed").unwrap();
    assert!(!sdk.web_hot_reload());
    sdk.version = Some("4.0.0".into());
    assert!(sdk.web_hot_reload());
    sdk.version = Some("3.37.0".into());
    assert!(!sdk.web_hot_reload());
}

#[test]
fn rapid_saves_and_deleted_libraries_keep_reload_state_and_source_consistent() {
    let (_home, projects, project) = fixture();
    let main = project.root.join("lib/main.dart");
    let previous = preview::stamps(&project.root).unwrap();
    let content = "import 'deleted.dart';\nvoid main() { print('rapid edit'); }\n";
    fs::write(&main, content).unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(10);
    fs::OpenOptions::new()
        .write(true)
        .open(&main)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let minimum = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 1;
    let next = preview::stamps(&project.root).unwrap();
    let prepared = preview::prepare_hot_reload(&project, &previous, next).unwrap();
    assert_eq!(fs::read_to_string(&main).unwrap(), content);
    assert!(
        fs::metadata(&main)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            >= minimum
    );
    let unchanged =
        preview::prepare_hot_reload(&project, &prepared, preview::stamps(&project.root).unwrap())
            .unwrap();
    assert!(unchanged == prepared);
    let library = project.root.join("lib/deleted.dart");
    fs::write(&library, "const value = 1;\n").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&main)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let previous = preview::stamps(&project.root).unwrap();
    fs::remove_file(&library).unwrap();
    let removed = preview::stamps(&project.root).unwrap();
    let mut missing = std::collections::HashSet::new();
    assert!(preview::missing_dart_sources(
        &previous,
        &removed,
        &mut missing
    ));
    assert!(preview::missing_dart_sources(
        &removed,
        &removed,
        &mut missing
    ));
    fs::write(&library, "const value = 2;\n").unwrap();
    let restored = preview::stamps(&project.root).unwrap();
    assert!(!preview::missing_dart_sources(
        &removed,
        &restored,
        &mut missing
    ));
    assert!(missing.is_empty());
    preview::prepare_hot_reload(&project, &removed, restored).unwrap();
    assert_eq!(
        projects.file(&project, "lib/main.dart").unwrap().content,
        content
    );
    assert_eq!(projects.edit_history(&project)["undoCount"], 0);
    assert_eq!(fs::read_to_string(library).unwrap(), "const value = 2;\n");
}

#[test]
fn widget_library_reads_resolved_dependencies_and_limits_private_constructors_to_their_file() {
    let (_home, projects, project) = fixture();
    fs::create_dir_all(project.root.join(".dart_tool")).unwrap();
    let dependency = project.root.join("vendor/badges");
    fs::create_dir_all(dependency.join("lib/src")).unwrap();
    fs::write(
        dependency.join("lib/badges.dart"),
        "export 'src/tag.dart' show DependencyTag;",
    )
    .unwrap();
    fs::write(dependency.join("lib/src/tag.dart"), "class DependencyTag extends StatelessWidget { DependencyTag.named(); } class HiddenTag extends Widget {}").unwrap();
    let directory = reqwest::Url::from_directory_path(&dependency)
        .unwrap()
        .to_string();
    fs::write(project.root.join(".dart_tool/package_config.json"), serde_json::to_string(&json!({"packages":[{"name":"badges","rootUri":directory.trim_end_matches('/'),"packageUri":"lib/"}]})).unwrap()).unwrap();
    fs::write(project.root.join("lib/local #.dart"), "class Badge extends StatelessWidget { Badge.named(); Badge._(); } class _PrivateBadge extends Widget {} abstract class State<T extends StatefulWidget> {} class LocalState extends State<Badge> {}").unwrap();
    let catalog =
        widget_library::list(&projects, &project, &json!({"path":"lib/main.dart"})).unwrap();
    let entries = catalog["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry["name"] == "DependencyTag.named"
                && entry["category"] == "Dependencies")
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry["name"] == "Badge.named" && entry["category"] == "Project")
    );
    assert!(!entries.iter().any(|entry| matches!(
        entry["name"].as_str(),
        Some("HiddenTag" | "Badge._" | "_PrivateBadge" | "LocalState" | "State")
    )));
    let local = widget_library::list(
        &projects,
        &project,
        &json!({"path":"lib/local #.dart","category":"Project"}),
    )
    .unwrap();
    assert!(
        local["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "Badge._")
    );
    assert!(
        local["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "_PrivateBadge")
    );
    assert!(widget_library::list(&projects, &project, &json!({"path":"../outside.dart"})).is_err());
}

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

const DESIGN_SOURCE: &str = "// 😀 Theme values\r\nimport 'package:flutter/material.dart' as f;\r\nconst baseSize = 8.0, largeSize = 24.0;\r\nclass AppDesign {\r\n  static const brand = f.Color(0xff112233);\r\n  static const translucent = f.Color(0x80112233);\r\n  static const heading = f.TextStyle(fontSize: 18, color: brand, decoration: f.TextDecoration.underline);\r\n  static const pagePadding = f.EdgeInsets.symmetric(horizontal: 16, vertical: 8);\r\n  static const corners = f.BorderRadius.only(topLeft: f.Radius.circular(/* keep radius */ 4));\r\n  static const fontFamily = 'Body';\r\n  static const _private = f.Color(0xff445566);\r\n  final instanceOnly = 15.0;\r\n  static f.Color get derived => brand;\r\n  void helper() { const localOnly = 99.0; }\r\n}\r\nvoid helper() { const localColor = f.Color(0xff000000); }\r\nconst words = 'Color(0xff123456)';\r\n";

#[test]
fn design_values_index_only_top_level_and_static_declarations_and_preserve_other_style_fields() {
    let values = visual::design_values(DESIGN_SOURCE).unwrap();
    let values = values.as_array().unwrap();
    assert_eq!(values.len(), 10, "{values:?}");
    assert!(
        values
            .iter()
            .all(|value| !value["name"].as_str().unwrap().contains("Only")
                && value["name"] != "localColor")
    );
    assert_eq!(
        values
            .iter()
            .find(|value| value["name"] == "AppDesign.brand")
            .unwrap()["value"],
        "#112233"
    );
    assert_eq!(
        values
            .iter()
            .find(|value| value["name"] == "AppDesign.translucent")
            .unwrap()["value"],
        "#80112233"
    );
    assert_eq!(
        values
            .iter()
            .find(|value| value["name"] == "AppDesign.derived")
            .unwrap()["constant"],
        false
    );
    let heading = values
        .iter()
        .find(|value| value["name"] == "AppDesign.heading")
        .unwrap();
    let changed = visual::edit_design_value(
        DESIGN_SOURCE,
        &json!({"id":heading["id"],"fields":{"fontSize":"30","fontWeight":"w600","color":""}}),
    )
    .unwrap();
    assert!(changed.contains("fontSize: 30, color: brand, decoration: f.TextDecoration.underline"));
    assert!(changed.contains("fontWeight: f.FontWeight.w600"));
    let brand = values
        .iter()
        .find(|value| value["name"] == "AppDesign.brand")
        .unwrap();
    let changed = visual::edit_design_value(
        DESIGN_SOURCE,
        &json!({"id":brand["id"],"fields":{"value":"#c0abcdef"}}),
    )
    .unwrap();
    assert!(changed.contains("brand = f.Color(0xc0abcdef)"));
    assert!(changed.contains("// 😀 Theme values\r\n"));
}

#[test]
fn design_value_layout_edits_preserve_constructor_shape_comments_and_unsafe_expression_guards() {
    let radius = "final corners = BorderRadius.circular(4);";
    let entry = visual::design_values(radius).unwrap();
    assert!(
        visual::edit_design_value(
            radius,
            &json!({"id":entry[0]["id"],"fields":{"value":"-1"}})
        )
        .is_err()
    );
    let values = visual::design_values(DESIGN_SOURCE).unwrap();
    let padding = values
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["name"] == "AppDesign.pagePadding")
        .unwrap();
    let changed = visual::edit_design_value(
        DESIGN_SOURCE,
        &json!({"id":padding["id"],"fields":{"horizontal":"22"}}),
    )
    .unwrap();
    assert!(changed.contains("f.EdgeInsets.symmetric(horizontal: 22, vertical: 8)"));
    let corner = values
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["name"] == "AppDesign.corners")
        .unwrap();
    let changed = visual::edit_design_value(
        DESIGN_SOURCE,
        &json!({"id":corner["id"],"fields":{"topLeft":"12","bottomRight":"16"}}),
    )
    .unwrap();
    assert!(changed.contains("f.Radius.circular(/* keep radius */ 12)"));
    assert!(changed.contains("bottomRight: f.Radius.circular(16)"));
    assert!(
        visual::edit_design_value(
            DESIGN_SOURCE,
            &json!({"id":corner["id"],"fields":{"bottomRight":"-1"}})
        )
        .is_err()
    );
    assert!(visual::edit_design_value(DESIGN_SOURCE,&json!({"id":padding["id"],"mode":"expression","expression":"f.EdgeInsets.all(4)); destroy(); x("})).is_err());
}

#[test]
fn design_binding_import_and_value_edit_are_reversible_and_check_both_revisions() {
    let (_home, projects, project) = fixture();
    fs::write(project.root.join("lib/theme.dart"), DESIGN_SOURCE).unwrap();
    let main = "// 😀\r\nimport 'package:flutter/material.dart';\r\nconst freeflowTheme = 3;\r\nWidget build() => const Text('Use theme');\r\n";
    fs::write(project.root.join("lib/main.dart"), main).unwrap();
    let tree = server::read_widgets(&projects, &project, "lib/main.dart").unwrap();
    let node = &tree["nodes"][0];
    let values = design_values::catalog(
        &projects,
        &project,
        &json!({"path":"lib/theme.dart","kind":"typography"}),
    )
    .unwrap();
    let value = &values["entries"][0];
    let request = json!({"path":"lib/main.dart","revision":tree["revision"],"id":node["id"],"name":"style","valuePath":"lib/theme.dart","valueRevision":value["revision"],"valueId":value["id"]});
    let bound = design_values::bind(&projects, &project, &request).unwrap();
    assert!(
        bound["source"]["content"]
            .as_str()
            .unwrap()
            .contains("import \"package:example_app/theme.dart\" as freeflowTheme2;\r\n")
    );
    assert!(
        bound["source"]["content"]
            .as_str()
            .unwrap()
            .contains("style: freeflowTheme2.AppDesign.heading")
    );
    assert_eq!(
        server::read_widgets(&projects, &project, "lib/main.dart").unwrap()["nodes"][0]["id"],
        bound["widget"]
    );
    let history = projects.edit_history(&project);
    projects
        .replay_edit(&project, "undo", history["revision"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        projects.file(&project, "lib/main.dart").unwrap().content,
        main
    );
    let edit = json!({"path":"lib/theme.dart","revision":value["revision"],"id":value["id"],"fields":{"fontSize":"32"}});
    design_values::write(&projects, &project, &edit).unwrap();
    assert_eq!(
        design_values::bind(&projects, &project, &request)
            .unwrap_err()
            .status,
        409
    );
    assert_eq!(
        projects.file(&project, "lib/main.dart").unwrap().content,
        main
    );
    assert_eq!(
        design_values::write(&projects, &project, &edit)
            .unwrap_err()
            .status,
        409
    );
}

#[test]
fn design_references_reuse_import_aliases_and_reject_private_values_parts_and_traversal() {
    let (_home, projects, project) = fixture();
    fs::write(project.root.join("lib/theme.dart"), DESIGN_SOURCE).unwrap();
    let main = "import 'package:flutter/material.dart';\nimport 'theme.dart' as styles;\nWidget build() => Text('Use theme');\n";
    fs::write(project.root.join("lib/main.dart"), main).unwrap();
    let tree = server::read_widgets(&projects, &project, "lib/main.dart").unwrap();
    let values = design_values::catalog(
        &projects,
        &project,
        &json!({"path":"lib/theme.dart","kind":"color"}),
    )
    .unwrap();
    let value = values["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["name"] == "AppDesign.brand")
        .unwrap();
    let mut input = json!({"path":"lib/main.dart","revision":tree["revision"],"id":tree["nodes"][0]["id"],"name":"color","valuePath":"lib/theme.dart","valueRevision":value["revision"],"valueId":value["id"]});
    let private = values["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["name"] == "AppDesign._private")
        .unwrap();
    input["valueId"] = private["id"].clone();
    assert_eq!(
        design_values::bind(&projects, &project, &input)
            .unwrap_err()
            .status,
        422
    );
    input["valueId"] = value["id"].clone();
    let result = design_values::bind(&projects, &project, &input).unwrap();
    let text = result["source"]["content"].as_str().unwrap();
    assert!(text.contains("color: styles.AppDesign.brand"));
    assert!(!text.contains("freeflowTheme"));
    assert!(
        design_values::read(
            &projects,
            &project,
            &json!({"path":"../outside.dart","id":"0"})
        )
        .is_err()
    );
    assert!(
        visual::design_import(
            "part of 'main.dart';\nWidget build() => Text('part');",
            &["theme.dart".into()],
            "AppDesign.brand"
        )
        .is_err()
    );
}

#[test]
fn design_colors_use_a_containers_decoration_without_creating_conflicting_arguments() {
    let (_home, projects, project) = fixture();
    fs::write(project.root.join("lib/theme.dart"), DESIGN_SOURCE).unwrap();
    let main = "import 'package:flutter/material.dart';\nWidget build()=>Container(decoration: const BoxDecoration(color: Colors.blue));\n";
    fs::write(project.root.join("lib/main.dart"), main).unwrap();
    let tree = server::read_widgets(&projects, &project, "lib/main.dart").unwrap();
    let container = tree["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["type"] == "Container")
        .unwrap();
    let color = container["properties"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["name"] == "color")
        .unwrap();
    assert_eq!(color["designEditable"], false);
    let values = design_values::catalog(
        &projects,
        &project,
        &json!({"kind":"color","query":"brand"}),
    )
    .unwrap();
    let value = &values["entries"][0];
    let mut input = json!({"path":"lib/main.dart","revision":tree["revision"],"id":container["id"],"name":"color","valuePath":value["path"],"valueId":value["id"],"valueRevision":value["revision"]});
    assert_eq!(
        design_values::bind(&projects, &project, &input)
            .unwrap_err()
            .status,
        422
    );
    assert_eq!(
        projects.file(&project, "lib/main.dart").unwrap().content,
        main
    );
    assert_eq!(projects.edit_history(&project)["undoCount"], 0);
    input["name"] = json!("decoration.color");
    let bound = design_values::bind(&projects, &project, &input).unwrap();
    assert!(
        bound["source"]["content"]
            .as_str()
            .unwrap()
            .contains("BoxDecoration(color: freeflowTheme.AppDesign.brand)")
    );
}

#[test]
fn design_binding_creates_nested_text_styles_in_the_widgets_import_namespace() {
    let (_home, projects, project) = fixture();
    fs::write(project.root.join("lib/theme#dark.dart"), DESIGN_SOURCE).unwrap();
    fs::write(
        project.root.join("lib/main.dart"),
        "import 'package:flutter/material.dart' as f;\nWidget build()=>const f.Text('Prefix');\n",
    )
    .unwrap();
    let values = design_values::catalog(
        &projects,
        &project,
        &json!({"path":"lib/theme#dark.dart","kind":"color","query":"brand"}),
    )
    .unwrap();
    let value = &values["entries"][0];
    let tree = server::read_widgets(&projects, &project, "lib/main.dart").unwrap();
    let result=design_values::bind(&projects,&project,&json!({"path":"lib/main.dart","revision":tree["revision"],"id":tree["nodes"][0]["id"],"name":"color","valuePath":value["path"],"valueId":value["id"],"valueRevision":value["revision"]})).unwrap();
    let content = result["source"]["content"].as_str().unwrap();
    assert!(
        content.contains("style: f.TextStyle(color: freeflowTheme.AppDesign.brand)"),
        "{content}"
    );
    assert!(content.contains("theme%23dark.dart"));
    let tree = server::read_widgets(&projects, &project, "lib/main.dart").unwrap();
    let color = tree["nodes"][0]["properties"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["name"] == "color")
        .unwrap();
    assert_eq!(color["expression"], "freeflowTheme.AppDesign.brand");
    assert!(
        design_values::catalog(&projects, &project, &json!({"path":"../outside.dart"})).is_err()
    );
}

#[test]
fn uploaded_assets_and_pubspec_undo_together_without_losing_existing_yaml() {
    use base64::Engine as _;
    let (_home, projects, project) = fixture();
    let manifest = "# Keep this comment\r\nname: example_app\r\ndependencies: {flutter: {sdk: flutter}}\r\nflutter:\r\n  uses-material-design: true # Keep this too\r\n  assets: [assets/existing/] # Existing assets\r\n  fonts:\r\n    - family: Existing\r\n      fonts:\r\n        - asset: assets/fonts/existing.ttf\r\n";
    fs::write(project.root.join("pubspec.yaml"), manifest).unwrap();
    let binary = [0, 255, 1, 128, 0, 9];
    let uploaded = projects
        .asset(
            &project,
            "test image.png",
            &base64::engine::general_purpose::STANDARD.encode(binary),
        )
        .unwrap();
    let path = uploaded["path"].as_str().unwrap();
    assert_eq!(path, "assets/images/test image.png");
    assert_eq!(fs::read(project.root.join(path)).unwrap(), binary);
    let after = projects.file(&project, "pubspec.yaml").unwrap();
    assert!(after.content.contains("# Keep this comment\r\n"));
    assert!(
        after
            .content
            .contains("uses-material-design: true # Keep this too")
    );
    assert!(after.content.contains("# Existing assets"));
    assert!(after.content.contains("family: Existing"));
    assert!(!after.content.replace("\r\n", "").contains('\n'));
    assert!(after.content.contains(path));
    let catalog = projects.assets(&project).unwrap();
    assert_eq!(catalog["entries"][0]["registered"], true);
    let history = projects.edit_history(&project);
    assert_eq!(history["undo"]["files"].as_array().unwrap().len(), 2);
    projects
        .replay_edit(&project, "undo", history["revision"].as_str().unwrap())
        .unwrap();
    assert!(!project.root.join(path).exists());
    assert_eq!(
        projects.file(&project, "pubspec.yaml").unwrap().content,
        manifest
    );
    let history = projects.edit_history(&project);
    projects
        .replay_edit(&project, "redo", history["revision"].as_str().unwrap())
        .unwrap();
    assert_eq!(fs::read(project.root.join(path)).unwrap(), binary);
    assert_eq!(
        projects.file(&project, "pubspec.yaml").unwrap().content,
        after.content
    );
    let catalog = projects.assets(&project).unwrap();
    projects
        .delete_asset(
            &project,
            path,
            catalog["entries"][0]["revision"].as_str().unwrap(),
            catalog["manifestRevision"].as_str().unwrap(),
        )
        .unwrap();
    assert!(!project.root.join(path).exists());
    assert!(
        !projects
            .file(&project, "pubspec.yaml")
            .unwrap()
            .content
            .contains(path)
    );
    let history = projects.edit_history(&project);
    projects
        .replay_edit(&project, "undo", history["revision"].as_str().unwrap())
        .unwrap();
    assert_eq!(fs::read(project.root.join(path)).unwrap(), binary);
    assert_eq!(
        projects.file(&project, "pubspec.yaml").unwrap().content,
        after.content
    );
}

#[test]
fn asset_conflicts_and_invalid_manifests_leave_files_and_history_untouched() {
    use base64::Engine as _;
    let (_home, projects, project) = fixture();
    let encoded = base64::engine::general_purpose::STANDARD.encode([1, 2, 3]);
    let uploaded = projects.asset(&project, "tone.wav", &encoded).unwrap();
    let path = uploaded["path"].as_str().unwrap();
    let before = projects.file(&project, "pubspec.yaml").unwrap();
    let history = projects.edit_history(&project);
    assert_eq!(
        projects
            .asset(&project, "tone.wav", &encoded)
            .unwrap_err()
            .status,
        409
    );
    assert_eq!(
        projects
            .asset(&project, "../outside.wav", &encoded)
            .unwrap_err()
            .status,
        400
    );
    assert_eq!(
        projects.file(&project, "pubspec.yaml").unwrap().revision,
        before.revision
    );
    assert_eq!(projects.edit_history(&project), history);
    let catalog = projects.assets(&project).unwrap();
    fs::write(
        project.root.join("pubspec.yaml"),
        format!("{}# external edit\n", before.content),
    )
    .unwrap();
    assert_eq!(
        projects
            .delete_asset(
                &project,
                path,
                catalog["entries"][0]["revision"].as_str().unwrap(),
                catalog["manifestRevision"].as_str().unwrap()
            )
            .unwrap_err()
            .status,
        409
    );
    assert!(project.root.join(path).is_file());
    assert_eq!(
        projects
            .replay_edit(&project, "undo", history["revision"].as_str().unwrap())
            .unwrap_err()
            .status,
        409
    );
    fs::write(
        project.root.join("pubspec.yaml"),
        "name: example_app\nflutter: {assets: invalid}\n",
    )
    .unwrap();
    assert_eq!(
        projects
            .asset(&project, "new.png", &encoded)
            .unwrap_err()
            .status,
        422
    );
    assert!(!project.root.join("assets/images/new.png").exists());
    assert_eq!(projects.edit_history(&project), history);
    assert!(
        projects
            .delete_asset(&project, "lib/main.dart", "x", "x")
            .is_err()
    );
}

#[test]
fn font_registration_directory_assets_and_structured_asset_entries_are_preserved() {
    use base64::Engine as _;
    let (_home, projects, project) = fixture();
    let manifest = "name: example_app\nflutter:\n  assets:\n    - assets/images/ # Covers direct images only\n    - path: assets/special.json\n      platforms: [web]\n  fonts:\n    - family: Body\n      fonts:\n        - asset: fonts/body.ttf\n          weight: 700\n";
    fs::write(project.root.join("pubspec.yaml"), manifest).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode([0, 1, 2, 3]);
    projects.asset(&project, "logo.png", &encoded).unwrap();
    assert_eq!(
        projects.file(&project, "pubspec.yaml").unwrap().content,
        manifest
    );
    let font = projects.asset(&project, "Body.ttf", &encoded).unwrap();
    assert_eq!(font["family"], "Body_2");
    let content = projects.file(&project, "pubspec.yaml").unwrap().content;
    assert!(content.contains("platforms: [web]"));
    assert!(content.contains("weight: 700"));
    let catalog = projects.assets(&project).unwrap();
    let entry = catalog["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["category"] == "fonts")
        .unwrap();
    projects
        .delete_asset(
            &project,
            font["path"].as_str().unwrap(),
            entry["revision"].as_str().unwrap(),
            catalog["manifestRevision"].as_str().unwrap(),
        )
        .unwrap();
    assert_eq!(
        projects.file(&project, "pubspec.yaml").unwrap().content,
        manifest
    );
    let catalog = projects.assets(&project).unwrap();
    let image = &catalog["entries"][0];
    projects
        .delete_asset(
            &project,
            image["path"].as_str().unwrap(),
            image["revision"].as_str().unwrap(),
            catalog["manifestRevision"].as_str().unwrap(),
        )
        .unwrap();
    assert_eq!(
        projects.file(&project, "pubspec.yaml").unwrap().content,
        manifest
    );
    fs::write(project.root.join("assets/legacy.json"), "{\"legacy\":true}").unwrap();
    let catalog = projects.assets(&project).unwrap();
    let legacy = catalog["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == "assets/legacy.json")
        .unwrap();
    assert_eq!(legacy["registered"], false);
    projects
        .register_asset(
            &project,
            "assets/legacy.json",
            legacy["revision"].as_str().unwrap(),
            catalog["manifestRevision"].as_str().unwrap(),
        )
        .unwrap();
    assert!(
        projects
            .file(&project, "pubspec.yaml")
            .unwrap()
            .content
            .contains("assets/legacy.json")
    );
    let history = projects.edit_history(&project);
    projects
        .replay_edit(&project, "undo", history["revision"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        projects.file(&project, "pubspec.yaml").unwrap().content,
        manifest
    );
    assert_eq!(
        fs::read_to_string(project.root.join("assets/legacy.json")).unwrap(),
        "{\"legacy\":true}"
    );
}

#[test]
fn asset_and_font_properties_keep_constructor_arguments_and_callbacks() {
    let source = "Widget build(context) => Column(children: [Image.asset('old.png', width: 48, errorBuilder: (context, error, stack) => const Text('fallback')), Image.network('https://example.com/image.png'), const Text('Title')]);";
    let tree = visual::outline(source).unwrap();
    let nodes = tree["nodes"].as_array().unwrap();
    let image = nodes
        .iter()
        .find(|node| node["type"] == "Image.asset")
        .unwrap();
    assert_eq!(image["properties"][0]["asset"], "images");
    let changed = visual::edit(source, &json!({"id":image["id"],"operation":"expression","name":"asset","expression":"'assets/images/new.png'"})).unwrap();
    assert!(changed.contains("Image.asset('assets/images/new.png', width: 48, errorBuilder:"));
    assert!(changed.contains("Image.network('https://example.com/image.png')"));
    let title = nodes
        .iter()
        .rev()
        .find(|node| node["type"] == "Text")
        .unwrap();
    let changed = visual::edit(source, &json!({"id":title["id"],"operation":"expression","name":"fontFamily","expression":"'Body'"})).unwrap();
    assert!(changed.contains("fontFamily: 'Body'"));
    assert!(changed.contains("errorBuilder:"));
}

#[test]
fn registering_existing_assets_creates_missing_flutter_configuration_and_only_undoes_the_declaration()
 {
    for extra in ["", "flutter:\n", "flutter: null\n"] {
        let (_home, projects, project) = fixture();
        let manifest =
            format!("name: example_app\ndependencies: {{flutter: {{sdk: flutter}}}}\n{extra}");
        fs::write(project.root.join("pubspec.yaml"), &manifest).unwrap();
        fs::create_dir(project.root.join("assets")).unwrap();
        fs::write(project.root.join("assets/data.json"), "{\"existing\":true}").unwrap();
        let catalog = projects.assets(&project).unwrap();
        assert!(catalog["manifestError"].is_null());
        projects
            .register_asset(
                &project,
                "assets/data.json",
                catalog["entries"][0]["revision"].as_str().unwrap(),
                catalog["manifestRevision"].as_str().unwrap(),
            )
            .unwrap();
        let context = projects
            .asset_context(&project, &json!({"category":"documents","query":"data"}))
            .unwrap();
        assert_eq!(context["entries"][0]["registered"], true);
        let history = projects.edit_history(&project);
        assert_eq!(history["undo"]["files"], json!(["pubspec.yaml"]));
        projects
            .replay_edit(&project, "undo", history["revision"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            projects.file(&project, "pubspec.yaml").unwrap().content,
            manifest
        );
        assert_eq!(
            fs::read_to_string(project.root.join("assets/data.json")).unwrap(),
            "{\"existing\":true}"
        );
    }
}

#[test]
fn project_component_imports_exports_and_inheritance_keep_widget_edits_available() {
    let (_home, projects, project) = fixture();
    fs::create_dir_all(project.root.join("lib/cards")).unwrap();
    fs::write(project.root.join("lib/cards/base.dart"), "import 'package:flutter/widgets.dart'; abstract class BaseTile extends StatelessWidget { const BaseTile(); }").unwrap();
    fs::write(project.root.join("lib/cards/tile.dart"), "import 'base.dart'; class Tile extends BaseTile { const Tile({required this.label}); const Tile.small({required this.label}); final String label; Widget build(context) => Text(label); }").unwrap();
    fs::write(
        project.root.join("lib/cards.dart"),
        "export 'cards/tile.dart';",
    )
    .unwrap();
    let source = "// 😀\r\nimport 'package:example_app/cards.dart' as ui; Widget build() => Column(children: [const ui.Tile(label: 'One'), const ui.Tile.small(label: 'Two')]);";
    fs::write(project.root.join("lib/main.dart"), source).unwrap();
    let tree = server::read_widgets(&projects, &project, "lib/main.dart").unwrap();
    let tiles: Vec<_> = tree["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|node| node["component"] == true)
        .collect();
    assert_eq!(tiles.len(), 2);
    assert_eq!(tiles[0]["type"], "Tile");
    assert_eq!(tiles[1]["type"], "Tile.small");
    assert_eq!(
        tiles[0]["symbol"],
        source[..source.find("Tile(label").unwrap()]
            .encode_utf16()
            .count()
    );
    assert_eq!(tiles[0]["movable"], true);
    let input = json!({"path":"lib/main.dart","revision":tree["revision"],"id":tiles[0]["id"],"name":"label"});
    assert_eq!(
        server::widget_property(&projects, &project, &input).unwrap()["expression"],
        "'One'"
    );
    assert_eq!(
        server::widget_snippet(&projects, &project, &input).unwrap()["code"],
        "const ui.Tile(label: 'One')"
    );
    server::write_widgets(&projects, &project, &json!({"path":"lib/main.dart","revision":tree["revision"],"command":{"id":tiles[0]["id"],"operation":"expression","name":"label","expression":"'Changed'"}})).unwrap();
    assert!(
        projects
            .file(&project, "lib/main.dart")
            .unwrap()
            .content
            .contains("ui.Tile(label: 'Changed')")
    );
    let updated = server::read_widgets(&projects, &project, "lib/main.dart").unwrap();
    server::write_widgets(&projects, &project, &json!({"path":"lib/main.dart","revision":updated["revision"],"command":{"id":tiles[0]["id"],"operation":"duplicate"}})).unwrap();
    assert_eq!(
        server::read_widgets(&projects, &project, "lib/main.dart").unwrap()["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| node["component"] == true)
            .count(),
        3
    );
    assert_eq!(tree["componentScopeTruncated"], false);
}

#[test]
fn component_definition_owners_include_state_builds_and_part_files() {
    let (_home, projects, project) = fixture();
    fs::create_dir_all(project.root.join("lib/parts")).unwrap();
    let source = "import 'package:flutter/widgets.dart'; class Page extends StatefulWidget { const Page(); State<Page> createState() => _PageState(); } class _PageState extends State<Page> { Widget build(context) => Column(children: [Text('Owned')]); }";
    fs::write(project.root.join("lib/main.dart"), source).unwrap();
    let tree = server::read_widgets(&projects, &project, "lib/main.dart").unwrap();
    let column = tree["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["type"] == "Column")
        .unwrap();
    assert_eq!(column["ownerComponent"]["name"], "Page");
    assert_eq!(column["ownerComponent"]["widget"], column["id"]);
    let definition = visual::component_at(
        source,
        source
            .find("Page() {")
            .unwrap_or(source.find("const Page()").unwrap() + 6) as u64,
        "Page",
    )
    .unwrap();
    assert!(definition.is_some());
    fs::write(project.root.join("lib/detail.dart"), "import 'package:flutter/widgets.dart'; part 'parts/detail_state.dart'; class Detail extends StatefulWidget { const Detail(); State<Detail> createState() => _DetailState(); }").unwrap();
    fs::write(project.root.join("lib/parts/detail_state.dart"), "part of '../detail.dart'; class _DetailState extends State<Detail> { Widget build(context) => const Text('Detail'); }").unwrap();
    let tree = server::read_widgets(&projects, &project, "lib/parts/detail_state.dart").unwrap();
    assert_eq!(tree["nodes"][0]["ownerComponent"]["name"], "Detail");
}

#[test]
fn component_scopes_handle_import_cycles_and_refresh_after_file_changes() {
    let (_home, projects, project) = fixture();
    fs::write(project.root.join("lib/a.dart"), "export 'b.dart'; class First extends StatelessWidget { Widget build(context) => Text('First'); }").unwrap();
    fs::write(project.root.join("lib/b.dart"), "export 'a.dart'; class Second extends StatelessWidget { Widget build(context) => Text('Second'); }").unwrap();
    fs::write(
        project.root.join("lib/main.dart"),
        "import 'a.dart'; Widget build() => Column(children: [First(), Second()]);",
    )
    .unwrap();
    let tree = server::read_widgets(&projects, &project, "lib/main.dart").unwrap();
    assert_eq!(tree["nodes"].as_array().unwrap().len(), 3);
    fs::write(project.root.join("lib/b.dart"), "export 'a.dart'; class Renamed extends StatelessWidget { Widget build(context) => Text('Second'); }").unwrap();
    let tree = server::read_widgets(&projects, &project, "lib/main.dart").unwrap();
    assert_eq!(tree["nodes"].as_array().unwrap().len(), 2);
}

#[cfg(unix)]
#[test]
fn component_imports_never_read_through_symlinks() {
    let (home, projects, project) = fixture();
    let outside = home.path().join("outside.dart");
    fs::write(
        &outside,
        "class Outside extends StatelessWidget { Widget build(context) => Text('Private'); }",
    )
    .unwrap();
    std::os::unix::fs::symlink(outside, project.root.join("lib/linked.dart")).unwrap();
    fs::write(
        project.root.join("lib/main.dart"),
        "import 'linked.dart'; Widget build() => Column(children: [Outside()]);",
    )
    .unwrap();
    let tree = server::read_widgets(&projects, &project, "lib/main.dart").unwrap();
    assert_eq!(tree["nodes"].as_array().unwrap().len(), 1);
}

#[test]
fn property_expressions_edit_callbacks_and_nested_values_without_replacing_other_source() {
    let source = "// 😀 keep UTF-16 IDs\r\nWidget build() => Text(title, style: const ui.TextStyle(fontSize: baseSize, color: Colors.red), maxLines: 2 /* keep this note */, semanticsLabel: 'keep');";
    let node = visual::outline(source).unwrap()["nodes"][0].clone();
    assert_eq!(node["type"], "Text");
    let details =
        visual::property_details(source, node["id"].as_str().unwrap(), "fontSize").unwrap();
    assert_eq!(details["expression"], "baseSize");
    assert_eq!(
        details["offset"],
        source[..source.find("baseSize").unwrap()]
            .encode_utf16()
            .count()
    );
    let changed = visual::edit(source, &json!({"id":node["id"],"operation":"expression","name":"fontSize","expression":"enabled ? 24 : 16"})).unwrap();
    assert_eq!(changed, source.replace("baseSize", "enabled ? 24 : 16"));
    let changed = visual::edit(source, &json!({"id":node["id"],"operation":"expression","name":"style.letterSpacing","expression":"1.5"})).unwrap();
    assert!(changed.contains("color: Colors.red, letterSpacing: 1.5)"));
    assert!(changed.contains("2 /* keep this note */"));
    let positional = visual::edit(source, &json!({"id":node["id"],"operation":"expression","name":"text","expression":"enabled ? 'On' : 'Off'"})).unwrap();
    assert_eq!(
        positional,
        source.replace("Text(title,", "Text(enabled ? 'On' : 'Off',")
    );
    let callback_source = "Widget build() => TextButton(onPressed: () { /* preserve */ print('before'); }, child: const Text('Keep'));";
    let button = visual::outline(callback_source).unwrap()["nodes"][0].clone();
    let callback = "() { setState(() => count++); print('after'); }";
    let changed = visual::edit(callback_source, &json!({"id":button["id"],"operation":"expression","name":"onPressed","expression":callback})).unwrap();
    assert_eq!(
        changed,
        callback_source.replace("() { /* preserve */ print('before'); }", callback)
    );
    assert_eq!(
        visual::property_details(&changed, button["id"].as_str().unwrap(), "onPressed").unwrap()["expression"],
        callback
    );
}

#[test]
fn property_resets_preserve_comments_and_cannot_remove_positional_values() {
    for source in [
        "Widget build() => Text('Keep', maxLines: 2);",
        "Widget build() => Text('Keep', /* before */ maxLines: 2 /* after */);",
        "Widget build() => Text('Keep', maxLines: 2, semanticsLabel: 'Keep',);",
        "Widget build() => Container(width: 10);",
    ] {
        let node = visual::outline(source).unwrap()["nodes"][0].clone();
        let name = if node["type"] == "Container" {
            "width"
        } else {
            "maxLines"
        };
        let changed = visual::edit(
            source,
            &json!({"id":node["id"],"operation":"expression","name":name,"remove":true}),
        )
        .unwrap();
        assert!(!changed.contains(&format!("{name}:")));
        if source.contains("before") {
            assert!(changed.contains("/* before */") && changed.contains("/* after */"));
        }
        assert!(
            !visual::outline(&changed).unwrap()["nodes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            visual::edit(
                source,
                &json!({"id":node["id"],"operation":"expression","name":"text","remove":true})
            )
            .is_err()
                || node["type"] == "Container"
        );
    }
    let source =
        "Widget build() => Text('Keep', style: const TextStyle(color: Colors.red, fontSize: 24));";
    let node = visual::outline(source).unwrap()["nodes"][0].clone();
    let changed = visual::edit(
        source,
        &json!({"id":node["id"],"operation":"expression","name":"fontSize","remove":true}),
    )
    .unwrap();
    assert_eq!(
        changed,
        "Widget build() => Text('Keep', style: const TextStyle(color: Colors.red ));"
    );
}

#[test]
fn property_expression_inputs_cannot_escape_the_selected_argument() {
    let source = "Widget build() => Text(title, style: sharedStyle);";
    let node = visual::outline(source).unwrap()["nodes"][0].clone();
    for expression in [
        "",
        "// comment only",
        "1, maxLines: 99",
        "1,",
        "Text('unclosed'",
        "1); runSomething(); f(",
        "1; runSomething()",
        "'unterminated",
        "[1, 2}",
    ] {
        assert!(visual::edit(source, &json!({"id":node["id"],"operation":"expression","name":"text","expression":expression})).is_err(), "{expression}");
    }
    for name in ["", "a..b", "a:b", "a/b", "a);evil", "2invalid"] {
        assert!(
            visual::edit(
                source,
                &json!({"id":node["id"],"operation":"expression","name":name,"expression":"1"})
            )
            .is_err()
        );
    }
    assert!(
        visual::edit(
            source,
            &json!({"id":node["id"],"operation":"expression","name":"fontSize","expression":"24"})
        )
        .is_err()
    );
    let changed = visual::edit(source, &json!({"id":node["id"],"operation":"expression","name":"text","expression":"title // keep a trailing comment"})).unwrap();
    assert!(changed.contains("title // keep a trailing comment\n, style: sharedStyle"));
    let (draft, offset) =
        visual::property_completion(source, node["id"].as_str().unwrap()).unwrap();
    let bytes = draft
        .char_indices()
        .find_map(|(index, _)| (draft[..index].encode_utf16().count() == offset).then_some(index))
        .unwrap();
    assert!(draft[..bytes].ends_with(",\n  "));
    assert_eq!(source, "Widget build() => Text(title, style: sharedStyle);");
}

#[test]
fn project_rules_follow_file_scope_and_remain_visible_to_the_editor() {
    let (_home, projects, project) = fixture();
    fs::create_dir_all(project.root.join("lib/features")).unwrap();
    fs::create_dir_all(project.root.join("lib/other")).unwrap();
    for (path, text) in [
        ("AGENTS.md", "Use Flutter."),
        ("CLAUDE.md", "Keep app state local."),
        (".cursorrules", "Use Dart formatting."),
        ("lib/AGENTS.md", "Prefer widgets."),
        (
            "lib/features/ARCHITECTURE.md",
            "Feature-specific convention.",
        ),
        ("lib/other/AGENTS.md", "Unrelated rule."),
    ] {
        fs::write(project.root.join(path), text).unwrap();
    }
    let root = project_rules::load(&project, None).unwrap();
    assert_eq!(root["files"].as_array().unwrap().len(), 3);
    let rules = project_rules::load(&project, Some("lib/features/new.dart")).unwrap();
    let paths: Vec<_> = rules["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|rule| rule["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "AGENTS.md",
            "CLAUDE.md",
            ".cursorrules",
            "lib/AGENTS.md",
            "lib/features/ARCHITECTURE.md"
        ]
    );
    assert_eq!(rules["files"][4]["scope"], "lib/features");
    assert_eq!(rules["truncated"], false);
    assert!(rules["skipped"].as_array().unwrap().is_empty());
    assert!(projects.file(&project, ".cursorrules").is_ok());
    assert!(
        projects
            .tree(&project)
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.path == ".cursorrules")
    );
    for path in [
        "../outside.dart",
        "lib/../../outside.dart",
        ".env",
        "lib\\main.dart",
    ] {
        assert!(project_rules::load(&project, Some(path)).is_err());
    }
}

#[test]
fn project_rules_bound_unicode_excerpts_and_skip_non_text_files() {
    let (_home, _projects, project) = fixture();
    fs::write(project.root.join("AGENTS.md"), "🦀".repeat(20_000)).unwrap();
    fs::write(project.root.join("CLAUDE.md"), [0xff, 0xfe, 0]).unwrap();
    let rules = project_rules::load(&project, None).unwrap();
    assert_eq!(rules["files"].as_array().unwrap().len(), 1);
    assert_eq!(rules["files"][0]["truncated"], true);
    assert!(rules["files"][0]["revision"].is_null());
    assert!(rules["files"][0]["content"].as_str().unwrap().len() <= 16 * 1024);
    assert_eq!(rules["skipped"][0]["path"], "CLAUDE.md");
    assert_eq!(rules["truncated"], true);
}

#[cfg(unix)]
#[test]
fn project_rules_never_follow_symbolic_links() {
    let (home, _projects, project) = fixture();
    let outside = home.path().join("private.md");
    fs::write(&outside, "outside project").unwrap();
    std::os::unix::fs::symlink(outside, project.root.join("AGENTS.md")).unwrap();
    let rules = project_rules::load(&project, None).unwrap();
    assert!(rules["files"].as_array().unwrap().is_empty());
    assert_eq!(rules["skipped"][0]["path"], "AGENTS.md");
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
fn widget_clipboard_preserves_callbacks_comments_and_import_prefixes() {
    let source = "// 🎯\nWidget build() => m.Column(children: [m.TextButton(onPressed: () { /* keep */ setState(() => count++); }, child: const m.Text('Run $count')), const m.Text('Last')]);";
    let tree = visual::outline(source).unwrap();
    let button = &tree["nodes"][1];
    let snippet = visual::snippet(source, button["id"].as_str().unwrap()).unwrap();
    assert_eq!(
        snippet["code"],
        "m.TextButton(onPressed: () { /* keep */ setState(() => count++); }, child: const m.Text('Run $count'))"
    );
    let duplicated =
        visual::edit(source, &json!({"id":button["id"],"operation":"duplicate"})).unwrap();
    assert_eq!(
        duplicated
            .matches(snippet["code"].as_str().unwrap())
            .count(),
        2
    );
    assert!(duplicated.ends_with("const m.Text('Last')]);"));
    let container = "Widget build() => Container();";
    let root = &visual::outline(container).unwrap()["nodes"][0];
    let pasted = visual::edit(
        container,
        &json!({"id":root["id"],"operation":"paste","code":snippet["code"]}),
    )
    .unwrap();
    assert!(pasted.contains("Container(child: m.TextButton("));
    assert!(pasted.contains("/* keep */ setState(() => count++);"));
    for invalid in [
        "Text('x'); runSomething()",
        "Text('x'), Text('y')",
        "Text('unterminated)",
        "Text([)]",
        "makeWidget()",
    ] {
        assert!(
            visual::edit(
                container,
                &json!({"id":root["id"],"operation":"paste","code":invalid})
            )
            .is_err(),
            "{invalid}"
        );
    }
    let generic = "Widget build() => Column(children: [const m.ValueTile<String>(value: 'a')]);\nclass ValueTile<T> extends StatelessWidget {}";
    let generic_tree = visual::outline(generic).unwrap();
    assert_eq!(
        visual::snippet(generic, generic_tree["nodes"][1]["id"].as_str().unwrap()).unwrap()["code"],
        "const m.ValueTile<String>(value: 'a')"
    );
}

#[test]
fn widget_moves_cross_parents_and_leave_required_children_valid() {
    let source = "// 🎯\nWidget build() => Row(children: [Column(children: [Text('a'), Text('b')]), Column(children: [Text('c')]), Expanded(child: Text('required'))]);";
    let tree = visual::outline(source).unwrap();
    let nodes = tree["nodes"].as_array().unwrap();
    let find = |label: &str| nodes.iter().find(|node| node["label"] == label).unwrap();
    let columns: Vec<_> = nodes
        .iter()
        .filter(|node| node["type"] == "Column")
        .collect();
    for (from, to, side) in [
        (find("a"), columns[1], "inside"),
        (find("c"), find("a"), "before"),
        (find("required"), columns[0], "inside"),
    ] {
        let moved = visual::edit(
            source,
            &json!({"id":from["id"],"operation":"move","target":to["id"],"side":side}),
        )
        .unwrap();
        assert_eq!(
            moved
                .matches(
                    from["label"]
                        .as_str()
                        .map(|label| format!("Text('{label}')"))
                        .unwrap()
                        .as_str()
                )
                .count(),
            1
        );
        if from["label"] == "required" {
            assert!(moved.contains("Expanded(child: const SizedBox.shrink())"));
        }
    }
    assert!(visual::edit(source, &json!({"id":columns[0]["id"],"operation":"move","target":find("a")["id"],"side":"after"})).is_err());
    let required = "Widget build() => m.Expanded(child: const m.Text('cut'));";
    let tree = visual::outline(required).unwrap();
    let cut = visual::edit(
        required,
        &json!({"id":tree["nodes"][1]["id"],"operation":"remove"}),
    )
    .unwrap();
    assert_eq!(
        cut,
        "Widget build() => m.Expanded(child: const m.SizedBox.shrink());"
    );
    let tree = visual::outline(&cut).unwrap();
    assert_eq!(tree["nodes"][0]["slots"], json!(["child"]));
    let pasted = visual::edit(
        &cut,
        &json!({"id":tree["nodes"][0]["id"],"operation":"paste","code":"const m.Text('back')"}),
    )
    .unwrap();
    assert_eq!(
        pasted,
        "Widget build() => m.Expanded(child: const m.Text('back'));"
    );
}

#[test]
fn widget_clipboard_checks_revisions_and_failed_moves_are_atomic() {
    let (_home, projects, project) = fixture();
    let source =
        "Widget build() => Row(children: [Text('copy'), Container(child: Text('filled'))]);";
    let path = "lib/main.dart";
    fs::write(project.root.join(path), source).unwrap();
    let outline = server::read_widgets(&projects, &project, path).unwrap();
    let input = json!({"path":path,"revision":outline["revision"],"id":outline["nodes"][1]["id"]});
    assert_eq!(
        server::widget_snippet(&projects, &project, &input).unwrap()["code"],
        "Text('copy')"
    );
    let blocked = server::write_widgets(&projects, &project, &json!({"path":path,"revision":outline["revision"],"command":{
        "id":outline["nodes"][1]["id"],"operation":"move","target":outline["nodes"][2]["id"],"side":"inside"
    }})).unwrap_err();
    assert_eq!(blocked.status.as_u16(), 422);
    assert_eq!(fs::read_to_string(project.root.join(path)).unwrap(), source);
    fs::write(project.root.join(path), format!("// changed\n{source}")).unwrap();
    assert_eq!(
        server::widget_snippet(&projects, &project, &input)
            .unwrap_err()
            .status
            .as_u16(),
        409
    );
    let stale = server::write_widgets(
        &projects,
        &project,
        &json!({"path":path,"revision":outline["revision"],"command":{
            "id":outline["nodes"][1]["id"],"operation":"remove"
        }}),
    )
    .unwrap_err();
    assert_eq!(stale.status.as_u16(), 409);
}

#[test]
fn visual_conditional_values_keep_conditions_comments_and_other_branches() {
    let source = r#"Widget build() => Text(
      enabled ? (count > 2 ? 'ready' : /* keep */ 'waiting') : userLabel,
      style: TextStyle(color: enabled ? Colors.green : Colors.red),
    );"#;
    let tree = visual::outline(source).unwrap();
    let node = &tree["nodes"][0];
    let properties = node["properties"].as_array().unwrap();
    let text = properties
        .iter()
        .find(|item| item["name"] == "text")
        .unwrap();
    assert_eq!(text["conditional"]["condition"], "enabled");
    assert_eq!(
        text["conditional"]["then"]["conditional"]["condition"],
        "count > 2"
    );
    assert_eq!(
        text["conditional"]["then"]["conditional"]["else"]["value"],
        "waiting"
    );
    assert_eq!(text["conditional"]["else"]["editable"], false);
    let changed = visual::edit(
        source,
        &json!({
            "id":node["id"],"operation":"property","name":"text","branch":"then/else",
            "value":"Updated $value"
        }),
    )
    .unwrap();
    assert!(changed.contains("count > 2 ? 'ready' : /* keep */ 'Updated \\$value'"));
    assert!(changed.contains(" : userLabel,"));
    assert_eq!(visual::edit(source, &json!({
        "id":node["id"],"operation":"property","name":"text","branch":"else","value":"Overwrite"
    })).unwrap_err().status.as_u16(), 422);
    let color = visual::edit(
        source,
        &json!({
            "id":node["id"],"operation":"property","name":"color","branch":"then","value":"#226633"
        }),
    )
    .unwrap();
    assert!(color.contains("enabled ? const Color(0xff226633) : Colors.red"));
    assert_eq!(visual::edit(source, &json!({
        "id":node["id"],"operation":"property","name":"text","branch":"then/else/then","value":"Invalid"
    })).unwrap_err().status.as_u16(), 422);
}

#[test]
fn widget_branches_and_builder_slots_follow_the_selected_constructor() {
    let source = "Widget build() => Column(children: [active ? const Text('On') : const Text('Off'), ListView.builder(itemCount: 2, itemBuilder: (context, index) => Text('Row'))]);";
    let outline = visual::outline(source).unwrap();
    let nodes = outline["nodes"].as_array().unwrap();
    let on = nodes.iter().find(|node| node["label"] == "On").unwrap();
    assert_eq!(on["removable"], false);
    let builder = nodes
        .iter()
        .find(|node| node["type"] == "ListView.builder")
        .unwrap();
    assert_eq!(builder["slots"], json!([]));
    let blocked = visual::edit(
        source,
        &json!({
            "id":builder["id"],"operation":"add","slot":"children","widget":"Divider"
        }),
    );
    assert!(blocked.is_err());
    let changed = visual::edit(
        source,
        &json!({
            "id":on["id"],"operation":"property","name":"text","value":"Enabled"
        }),
    )
    .unwrap();
    assert!(changed.contains("active ? const Text('Enabled') : const Text('Off')"));
    let safe = "Widget build() => Text(data?.label ?? 'fallback');";
    let property = &visual::outline(safe).unwrap()["nodes"][0]["properties"][0];
    assert!(property["conditional"].is_null());
    assert_eq!(property["editable"], false);
    let ternary =
        "Widget build() => Text((data?.ok ?? false) ? (a: 1, b: 2).toString() : 'fallback');";
    let property = &visual::outline(ternary).unwrap()["nodes"][0]["properties"][0];
    assert_eq!(property["conditional"]["else"]["editable"], true);
}

#[test]
fn saved_visual_edits_undo_redo_exact_source_and_branch_after_a_new_edit() {
    let (_home, projects, project) = fixture();
    let path = "lib/main.dart";
    let original = "// 🎯\r\nWidget build() => Column(children: [Text('A'), Text('B')]);\r\n";
    fs::write(project.root.join(path), original).unwrap();
    let replay = |action| {
        let history = projects.edit_history(&project);
        projects
            .replay_edit(&project, action, history["revision"].as_str().unwrap())
            .unwrap()
    };
    let tree = server::read_widgets(&projects, &project, path).unwrap();
    let wrapped = server::write_widgets(
        &projects,
        &project,
        &json!({"path":path,"revision":tree["revision"],"command":{
        "id":tree["nodes"][1]["id"],"operation":"wrap","widget":"Padding"
        }}),
    )
    .unwrap();
    let tree = server::read_widgets(&projects, &project, path).unwrap();
    let last = tree["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["label"] == "B")
        .unwrap();
    let removed = server::write_widgets(
        &projects,
        &project,
        &json!({"path":path,"revision":tree["revision"],"command":{
            "id":last["id"],"operation":"remove"
        }}),
    )
    .unwrap();
    assert_eq!(projects.edit_history(&project)["undoCount"], 2);
    replay("undo");
    assert_eq!(
        fs::read_to_string(project.root.join(path)).unwrap(),
        wrapped["content"]
    );
    replay("undo");
    assert_eq!(
        fs::read(project.root.join(path)).unwrap(),
        original.as_bytes()
    );
    replay("redo");
    let history = projects.edit_history(&project);
    let same = projects.file(&project, path).unwrap();
    projects
        .save(
            &project,
            path,
            same.content.clone(),
            same.revision.as_deref(),
        )
        .unwrap();
    assert_eq!(
        projects.edit_history(&project),
        history,
        "A no-op save must preserve Redo"
    );
    replay("redo");
    assert_eq!(
        fs::read_to_string(project.root.join(path)).unwrap(),
        removed["content"]
    );
    replay("undo");
    let before = projects.file(&project, path).unwrap();
    projects
        .save(
            &project,
            path,
            format!("{}// new edit\n", before.content),
            before.revision.as_deref(),
        )
        .unwrap();
    assert_eq!(projects.edit_history(&project)["redoCount"], 0);
}

#[test]
fn saved_multi_file_edits_replay_together_and_reject_external_changes_before_writing() {
    let (_home, projects, project) = fixture();
    let main = projects.file(&project, "lib/main.dart").unwrap();
    let old = project.root.join("lib/old.dart");
    fs::write(&old, "// old 🧩\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let old_source = projects.file(&project, "lib/old.dart").unwrap();
    projects
        .apply_edits(
            &project,
            &[
                projects::FileEdit {
                    path: main.path.clone(),
                    revision: main.revision.clone(),
                    content: Some("// changed\n".into()),
                },
                projects::FileEdit {
                    path: old_source.path.clone(),
                    revision: old_source.revision,
                    content: None,
                },
                projects::FileEdit {
                    path: "lib/new.dart".into(),
                    revision: None,
                    content: Some("// created\n".into()),
                },
            ],
        )
        .unwrap();
    let history = projects.edit_history(&project);
    assert_eq!(history["undoCount"], 1);
    assert_eq!(history["undo"]["files"].as_array().unwrap().len(), 3);
    projects
        .replay_edit(&project, "undo", history["revision"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        projects.file(&project, &main.path).unwrap().content,
        main.content
    );
    assert_eq!(fs::read_to_string(&old).unwrap(), old_source.content);
    assert!(!project.root.join("lib/new.dart").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&old).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    let history = projects.edit_history(&project);
    fs::write(project.root.join("lib/new.dart"), "// external work").unwrap();
    let conflict = projects
        .replay_edit(&project, "redo", history["revision"].as_str().unwrap())
        .unwrap_err();
    assert_eq!(conflict.status.as_u16(), 409);
    assert_eq!(projects.edit_history(&project), history);
    assert_eq!(
        projects.file(&project, &main.path).unwrap().content,
        main.content
    );
    assert!(old.is_file());
    assert_eq!(
        fs::read_to_string(project.root.join("lib/new.dart")).unwrap(),
        "// external work"
    );
    fs::remove_file(project.root.join("lib/new.dart")).unwrap();
    projects
        .replay_edit(&project, "redo", history["revision"].as_str().unwrap())
        .unwrap();
    assert!(!old.exists());
    assert_eq!(
        projects.file(&project, &main.path).unwrap().content,
        "// changed\n"
    );
    assert_eq!(
        projects.file(&project, "lib/new.dart").unwrap().content,
        "// created\n"
    );
}

#[test]
fn concurrent_undo_requests_only_apply_once_and_history_is_scoped_to_the_project() {
    let (home, projects, project) = fixture();
    let other_root = home.path().join("other");
    fs::create_dir_all(other_root.join("lib")).unwrap();
    fs::write(
        other_root.join("pubspec.yaml"),
        "name: other\ndependencies:\n  flutter:\n    sdk: flutter\n",
    )
    .unwrap();
    let other = projects
        .import_folder(other_root.to_str().unwrap())
        .unwrap();
    let projects = Arc::new(projects);
    let source = projects.file(&project, "lib/main.dart").unwrap();
    projects
        .save(
            &project,
            &source.path,
            "// saved\n".into(),
            source.revision.as_deref(),
        )
        .unwrap();
    projects
        .save(&other, "lib/main.dart", "// other project\n".into(), None)
        .unwrap();
    let revision = projects.edit_history(&project)["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let (projects, project, revision) =
                (projects.clone(), project.clone(), revision.clone());
            std::thread::spawn(move || projects.replay_edit(&project, "undo", &revision))
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .all(|error| error.status.as_u16() == 409)
    );
    assert_eq!(
        projects.file(&project, &source.path).unwrap().content,
        source.content
    );
    assert_eq!(projects.edit_history(&other)["undoCount"], 1);
    assert!(projects.replay_edit(&other, "undo", &revision).is_err());
    assert_eq!(
        projects.file(&other, "lib/main.dart").unwrap().content,
        "// other project\n"
    );
}

#[test]
fn non_utf8_files_cannot_create_a_lossy_undo_snapshot() {
    let (_home, projects, project) = fixture();
    let bytes = b"// invalid \xff\xfe\n";
    fs::write(project.root.join("lib/main.dart"), bytes).unwrap();
    let source = projects.file(&project, "lib/main.dart").unwrap();
    let error = projects
        .save(
            &project,
            "lib/main.dart",
            "// new".into(),
            source.revision.as_deref(),
        )
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 422);
    assert_eq!(fs::read(project.root.join("lib/main.dart")).unwrap(), bytes);
    assert_eq!(projects.edit_history(&project)["undoCount"], 0);
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
