# Freeflow

Freeflow is an MIT-licensed Flutter workbench that runs on your computer. Create or import a Flutter project, edit its Dart code or use the widget tree, and preview the running app in a phone, tablet or desktop viewport. Connect your own AI client over MCP.

## Start

1. Install [Node.js 22.15 or newer](https://nodejs.org/) and [Git](https://git-scm.com/downloads).
2. On Windows, double-click **Start-Freeflow.cmd**. On macOS or Linux, open a terminal in this folder and run **npm start**.
3. Select **Try the sample** to run Daymark, or select **New project** or **Import project**.

The workbench opens at [http://127.0.0.1:4747](http://127.0.0.1:4747). No npm packages are required. Use **Settings → Install managed SDK** if Flutter is missing, or point Freeflow at an existing [Flutter SDK](https://docs.flutter.dev/install).

### Flutter updates

Freeflow checks the official stable release feed at startup and periodically while it is running. **Install managed SDK** downloads the matching archive, verifies its SHA-256 checksum and activates it. Future managed SDK updates are automatic by default. Existing SDKs on PATH can be used immediately. Freeflow reports available stable updates for those SDKs and keeps its managed installation separate. **Use cached Dart packages offline** is useful when dependencies are already in the pub cache.

## Build an app

- **Design** shows the real Flutter web app and a widget tree for the selected Dart file. Edit supported properties, add a child, wrap a widget, or move it within a literal children list. Changes are saved to Dart source. Custom expressions, callbacks, and runtime-generated layouts are editable in **Code**.
- **Code** opens project files with Dart highlighting, tabs, formatting and per-file save history. Use **Ctrl/Cmd + S** to save and **Ctrl/Cmd + P** to open files by name. Add image assets in **Files** and declare them in the project's pubspec.
- **Plan** edits the project's portable **FREEFLOW_PLAN.md**. Use it to record screen, data and navigation requirements for your AI client.
- **Run** starts a Flutter preview. Source edits trigger a refresh. If the Flutter debug server cannot start, Freeflow compiles and serves the web app on a local port. The console shows package, compiler and build output. Use **Analyze** and **Test** to run Flutter's checks.

The device controls resize the web viewport. Use the Android or iOS tools to test native permissions, plugins and platform behavior.

## Connect your AI

Open a project and select **AI Connect**. It generates project-scoped setup commands for **Codex**, **Claude Code** and **Claude Desktop**. Your client can edit project files, work with the visual widget tree, and run previews, analysis and builds. The MCP server uses standard STDIO for other local MCP clients as well.

Use **npm start -- --no-open** to run Freeflow without opening a browser. The MCP client launches **node bin/freeflow.mjs mcp**. Start Freeflow first and pass the same **--home** to both processes if you use a custom data folder. Use Codex for ChatGPT-based local development. The [ChatGPT web app's MCP integration](https://help.openai.com/en/articles/12584461) connects to remote servers.

Freeflow has no subscription or AI API requirement of its own. Your AI client and backend providers retain their own usage limits.

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

For the default Kotlin Gradle template, **Export → Google Play → Configure signing** accepts your existing upload keystore. Other Gradle layouts can use [Flutter's Android signing instructions](https://docs.flutter.dev/deployment/android#sign-the-app). Upload the AAB in Google Play Console. For iOS, set the team and bundle identifier in Xcode first, then upload the IPA in App Store Connect. On Windows you can export the Flutter source and continue on a Mac. See [Flutter's iOS release guide](https://docs.flutter.dev/deployment/ios).

## Development

Run **npm test** to check the workbench and **node bin/freeflow.mjs --help** for launch options. Freeflow listens on loopback and serves Flutter previews from a separate local origin. Project metadata, file history, the managed SDK and generated builds live under **~/.freeflow** by default. Use **--home** for another location, **--port** to change the workbench port, and **--no-open** for a headless session. Import Flutter projects you trust to run on this computer.

To publish from an authenticated GitHub CLI, initialize and commit the source, then run **gh repo create freeflow --public --source . --remote origin --push** from the Freeflow folder.

## Current scope

Freeflow 0.1 covers local Flutter development, visual edits for supported Dart constructors, AI client connections, backend setup and local export builds. Hosted collaboration, managed web hosting, automated store submissions and a visual editor for arbitrary dynamic Dart UI are outside this release.
