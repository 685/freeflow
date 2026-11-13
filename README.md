# Freeflow

Freeflow is an MIT-licensed Flutter workbench that runs on your computer. Create, import or clone a Flutter project, edit its Dart code or use the widget tree, and preview the running app in a phone, tablet or desktop viewport. Use the built-in Agent with your own model provider or connect an AI client over MCP.

## Start

1. Install [Rust](https://rustup.rs/) and [Git](https://git-scm.com/downloads). On Windows, install the C++ build tools when Rust setup prompts you.
2. Run **rustup target add wasm32-unknown-unknown** once to enable the browser build.
3. On Windows, double-click **Start-Freeflow.cmd**. On macOS or Linux, run **cargo run --locked -p freeflow-build** in this folder.
4. Select **Try the sample** to run Daymark, or select **New project** or **Import project**.

The workbench opens at [http://127.0.0.1:4747](http://127.0.0.1:4747). Freeflow runs a native Rust server and a Rust browser interface compiled to WebAssembly. Apps are standard Flutter projects in Dart. Use **Settings → Install managed SDK** if Flutter is missing, or point Freeflow at an existing [Flutter SDK](https://docs.flutter.dev/install).

### Flutter updates

Freeflow checks the official stable release feed at startup and periodically while it is running. **Install managed SDK** downloads the matching archive, verifies its SHA-256 checksum and activates it. Future managed SDK updates are automatic by default. Existing SDKs on PATH can be used immediately. Freeflow reports available stable updates for those SDKs and keeps its managed installation separate. **Use cached Dart packages offline** is useful when dependencies are already in the pub cache.

## Build an app

- **Design** shows the real Flutter web app and the widget tree. Use the cursor button to select a widget directly in a compatible Flutter debug preview. Edit supported properties, add a child, wrap a widget, or move it within a literal children list. Changes are saved to Dart source.
- **Code** opens project files with Dart highlighting, tabs, formatting and per-file save history. Use **Suggest** or **Ctrl/Cmd + Space** for completions, **F12** for a definition in the project, **Type** for symbol information and **Problems** for live diagnostics in the current Dart file. The Dart SDK checks unsaved code. **Split** shows the preview and editor together. Use **Ctrl/Cmd + S** to save and **Ctrl/Cmd + P** to open files by name.
- **Plan** records your goal, screens and navigation in the project's **FREEFLOW_PLAN.md**. Switch between the screen planner and Markdown. **Plan with Agent** and **Build from plan** prepare prompts for review in the Agent panel.
- **App theme** edits the seed color, scaffold background and font family in a supported Material theme. Use the code editor or Agent for custom theme expressions.
- **Run** starts a Flutter preview. Source edits trigger a refresh. If the Flutter debug server cannot start, Freeflow compiles and serves the web app on a local port. The console shows package, compiler and build output. Use **Refresh Flutter dependencies**, **Analyze** and **Test** from the bottom toolbar.

The preview device controls resize the web viewport. Use **Run on device** in the bottom bar to launch on a connected phone, running simulator or desktop target detected by Flutter. **Run settings** accepts a Flutter flavor and Dart defines. The device panel shows logs and offers hot reload and hot restart where supported. Launching on iOS requires a Mac with Xcode and signing set up.

## Connect your AI

Open **Agent → Agent settings** and enter your OpenAI-compatible Chat Completions base URL and model name. For a local Ollama server the default base URL is **http://127.0.0.1:11434/v1**. Use a model that supports tool calls for project access and vision for attached images. Enter an API key when your provider requires one. Freeflow stores it in your local data folder.

The Agent streams replies and receives the active file and selected widget as context. **Ask** reads project files. **Edit files** can apply Dart and configuration edits. Use **Restore checkpoint** to undo a set of Agent edits while their file revisions still match.

Open a project and select **AI Connect**. It generates project-scoped setup commands for **Codex**, **Claude Code** and **Claude Desktop**. Your client can edit project files, work with the visual widget tree, and run previews, analysis and builds. The MCP server uses standard STDIO for other local MCP clients as well.

Use **cargo run --locked -p freeflow-build -- --no-open** to run Freeflow without opening a browser. The **AI Connect** dialog generates commands for the native **freeflow mcp** executable. Start Freeflow first and pass the same **--home** to both processes if you use a custom data folder. Existing MCP clients need the updated command from **AI Connect**. Use Codex for ChatGPT-based local development. The [ChatGPT web app's MCP integration](https://help.openai.com/en/articles/12584461) connects to remote servers.

Freeflow has no subscription of its own. Your AI client and backend providers retain their own usage limits.

## Source control

Use **Source control** to initialize Git, connect a remote, inspect and stage changes, commit, switch branches, fetch, pull and push. Use **Clone repository** to open a Flutter repository over HTTPS or SSH. Git uses the credentials and commit identity configured on your computer.

If a pull finds a conflict, open its diff to compare the two versions. Accept either version or edit the file in the code editor and mark it resolved. Commit the merge when all conflicts are resolved.

## Connect a backend

| Provider | What to enter | What Freeflow adds |
| --- | --- | --- |
| Firebase | Web app settings from the Firebase console. Register Android and iOS app IDs for builds on those platforms. | Firebase initialization, Firebase Auth and Firestore packages |
| Supabase | Project URL and publishable or legacy anon key | Supabase Flutter client initialization |

Freeflow templates call **FreeflowBackends.initialize()**. For an imported app, the Backends dialog shows how to add that call before **runApp()**. Add sign-in screens and data queries in Dart. Configure Firebase rules and Supabase row-level security in the respective backend project.

Freeflow passes public client configuration with **--dart-define-from-file**. Find the config path under **Backends → Build outside Freeflow**. Source exports leave this file and Android signing credentials in their local folders. See [Firebase for Flutter](https://firebase.google.com/docs/flutter/setup) and [Supabase for Flutter](https://supabase.com/docs/guides/getting-started/quickstarts/flutter).

## Export and submit

| Target | Output | What you need |
| --- | --- | --- |
| Web | ZIP containing the compiled website | A static host. Set the base path for subfolder hosting. |
| Google Play | Release AAB | Android SDK and JDK, plus an upload key for submission |
| Android device | Debug APK | Android SDK and JDK |
| App Store | Signed IPA | macOS, Xcode, an Apple team and signing configuration |
| Flutter source | ZIP containing the project | A Flutter SDK to continue on another machine |

- **Web**. Open **Export → Web → Publish website**. Save your Netlify team slug and [personal access token](https://docs.netlify.com/api-and-cli-guides/api-guides/get-started-with-api/#personal-access-tokens). Freeflow builds the app locally and creates a site in your team. **Update deployment** replaces the build at the same URL. **Unpublish** disables the site, and **Publish** enables it again after the new build is ready. Use **Refresh status** to check an interrupted upload. The hosting token is saved under the local Freeflow data folder.
- **Google Play**. For the default Kotlin Gradle template, **Configure signing** accepts your upload keystore. Other Gradle layouts can use [Flutter's Android signing instructions](https://docs.flutter.dev/deployment/android#sign-the-app). Upload the first AAB in Play Console. After that, build a signed AAB and use **Upload release build** with your package name, target track and [service account JSON](https://developers.google.com/android-publisher/getting_started). The form defaults to a draft on the internal track.
- **App Store**. Create the app in App Store Connect and set its team and bundle identifier in Xcode. Build the signed IPA on macOS, then use **Upload release build** with your [App Store Connect team key](https://developer.apple.com/help/app-store-connect/get-started/app-store-connect-api/). Apple processes the build before you can distribute it in TestFlight or submit it for review. On Windows, export the Flutter source to continue on a Mac.

Store upload keys are used for the selected upload. Freeflow saves a local receipt after the store accepts the build.

## Development

Run **cargo test --locked** to check the workbench and **cargo run --locked -p freeflow -- --help** for launch options. Use **cargo run --locked -p freeflow-build -- --build-only** to compile the browser assets into **target/freeflow-web**. Freeflow listens on loopback and serves Flutter previews from a separate local origin. Project metadata, file history, the managed SDK and generated builds live under **~/.freeflow** by default. Use **--home** for another location, **--port** to change the workbench port, and **--no-open** for a headless session. Import Flutter projects you trust to run on this computer.

## Current scope

Freeflow 0.1 covers local Flutter development, visual edits for supported Dart constructors, project AI, Git source control, backend setup, exports, Netlify publishing and store build uploads. Hosted collaboration, store review workflows and visual editing of arbitrary dynamic Dart UI are outside this release.
