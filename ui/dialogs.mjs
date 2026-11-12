import { escapeHtml as h } from './editor.mjs';
import { icon } from './icons.mjs';

const firebaseConsole = 'https://console.firebase.google.com/';
const supabaseConsole = 'https://supabase.com/dashboard';
const playConsole = 'https://play.google.com/console/';
const appStoreConnect = 'https://appstoreconnect.apple.com/';
const kb = size => size >= 1048576 ? (size / 1048576).toFixed(1) + ' MiB' : Math.max(1, Math.round(size / 1024)) + ' KiB';
const when = date => new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(date));
const external = (url, label) => '<a href="' + h(url) + '" target="_blank" rel="noopener noreferrer">' + h(label) + '</a>';
const btn = (id, label, symbol, style = 'outline', disabled = false) =>
  '<button type="button" class="button ' + style + '" id="' + id + '"' + (disabled ? ' disabled' : '') + '>' +
  (symbol ? icon(symbol) : '') + h(label) + '</button>';
const submit = label => '<button type="submit" class="button primary">' + h(label) + '</button>';
const msg = '<div class="form-error" role="alert"></div>';
const callout = (text, warning = false) => '<div class="callout' + (warning ? ' warning' : '') + '">' +
  icon(warning ? 'warning' : 'info') + '<div>' + text + '</div></div>';
const field = (name, label, value = '', attrs = '', help = '') => '<label class="form-field"><span>' + h(label) + '</span>' +
  '<input name="' + name + '" value="' + h(value) + '" ' + attrs + '>' + (help ? '<small>' + h(help) + '</small>' : '') + '</label>';
const code = text => '<pre class="code-card" tabindex="0">' + h(text) + '</pre>';

export function createDialogs(context) {
  const {
    state, api, post, projectApi, projectPost, toast, refreshState, activateProject, newProject,
    importProject, track, waitJob, loadFile, selectFile, refreshTree, currentFile, visualEdit,
  } = context;
  const dialog = document.getElementById('modal');
  const title = document.getElementById('modal-title');
  const subtitle = document.getElementById('modal-subtitle');
  const body = document.getElementById('modal-body');
  let view = 0;
  const find = selector => body.querySelector(selector);
  const current = number => dialog.open && view === number;
  const close = () => dialog.close();

  function open(heading, description, html) {
    const id = ++view;
    title.textContent = heading;
    subtitle.textContent = description;
    subtitle.hidden = !description;
    body.innerHTML = html;
    body.scrollTop = 0;
    if (!dialog.open) dialog.showModal();
    find('[autofocus]')?.focus();
    return id;
  }
  document.getElementById('modal-close').addEventListener('click', close);
  dialog.addEventListener('click', event => {
    if (event.target !== dialog) return;
    const box = dialog.getBoundingClientRect();
    if (event.clientX < box.left || event.clientX > box.right || event.clientY < box.top || event.clientY > box.bottom) close();
  });

  function click(selector, handler) {
    find(selector)?.addEventListener('click', event => {
      try { Promise.resolve(handler(event)).catch(error => toast(error.message, true)); }
      catch (error) { toast(error.message, true); }
    });
  }

  function form(handler, selector = 'form') {
    const node = find(selector);
    node.addEventListener('submit', async event => {
      event.preventDefault();
      const version = view;
      const button = node.querySelector('[type="submit"]');
      const errorField = node.querySelector('.form-error');
      const text = button.textContent;
      button.disabled = true;
      button.textContent = 'Working…';
      if (errorField) errorField.textContent = '';
      try {
        await handler(Object.fromEntries(new FormData(node)), version);
      } catch (error) {
        if (current(version) && errorField) errorField.textContent = error.message;
        else toast(error.message, true);
      } finally {
        button.disabled = false;
        button.textContent = text;
      }
    });
  }

  async function copy(text, description) {
    try {
      await navigator.clipboard.writeText(text);
      toast(description + ' copied');
    } catch {
      throw new Error('Select and copy the text manually.');
    }
  }

  function projects() {
    const items = state.projects.map(item => '<div class="dialog-item"><div class="dialog-item-text"><strong>' + h(item.name) +
      '</strong><small class="ellipsis mono" title="' + h(item.root) + '">' + h(item.root) + '</small></div>' +
      '<button type="button" class="icon-button" data-copy-folder="' + item.id + '" aria-label="Copy project folder" title="Copy folder">' + icon('copy') + '</button>' +
      '<button type="button" class="button small ' + (item.id === state.project?.id ? 'soft' : 'outline') + '" data-open-project="' +
      item.id + '">' + (item.id === state.project?.id ? 'Active' : 'Open') + '</button></div>').join('');
    open('Projects', '', '<div class="dialog-list">' + (items || '<p class="dialog-help">Your Flutter projects will appear here.</p>') +
      '</div><div class="form-footer">' + btn('import-folder', 'Import folder', 'folder') + btn('create-project', 'New project', 'plus', 'primary') + '</div>');
    click('#create-project', create);
    click('#import-folder', importFolder);
    for (const item of state.projects) {
      click('[data-open-project="' + item.id + '"]', async () => { close(); await activateProject(item); });
      click('[data-copy-folder="' + item.id + '"]', () => copy(item.root, 'Project folder'));
    }
  }

  function create() {
    const options = state.templates.map((template, i) => '<label class="choice-card"><input type="radio" name="template" value="' + h(template.id) +
      '"' + (i === 0 ? ' checked' : '') + '><span><strong>' + h(template.name) + '</strong><small>' +
      h(template.description) + '</small></span></label>').join('');
    open('New Flutter project', 'Create a Flutter project on this computer.', '<form class="form">' +
      field('name', 'App name', '', 'required maxlength="70" placeholder="My app" autofocus') +
      '<div class="form-field"><span>Template</span><div class="choice-grid">' + options + '</div></div>' +
      field('organization', 'Organization ID', 'com.example', 'placeholder="com.example"',
        'Use your reverse-DNS domain for new Android and iOS app identifiers.') +
      msg + '<div class="form-footer">' + submit('Create project') + '</div></form>');
    form(async (input, id) => { if (await newProject(input) && current(id)) close(); });
  }

  function importFolder() {
    const placeholder = state.flutter.platform === 'win32' ? 'C:\\Projects\\my_app' : '/home/me/projects/my_app';
    open('Import Flutter project', 'Choose an existing local project with a pubspec.yaml file.', '<form class="form">' +
      field('folder', 'Project folder', '', 'required autofocus placeholder="' + h(placeholder) + '"') +
      msg + '<div class="form-footer">' + submit('Import project') + '</div></form>');
    form(async (input, id) => { await importProject(input.folder.trim()); if (current(id)) close(); });
  }

  async function ai() {
    const configuration = await api('/ai' + (state.project ? '?project=' + state.project.id : ''));
    const clients = [
      { id: 'codex', label: 'Codex', note: 'Run the command in your terminal, then start a new Codex session.' },
      { id: 'claude', label: 'Claude Code', note: 'Run the command in your terminal, then start a new Claude Code session.' },
      { id: 'desktop', label: 'Claude Desktop', note: 'Merge this entry into your Claude Desktop MCP configuration, then restart Claude Desktop.' },
    ];
    function display(selected) {
      const client = clients.find(item => item.id === selected);
      open('AI Connect', state.project ? 'MCP access to ' + state.project.name : 'MCP access to your Freeflow projects.',
        '<div class="dialog-tabs">' + clients.map(item => btn('client-' + item.id, item.label, '', item.id === selected ? 'active' : 'soft')).join('') +
        '</div><div class="form"><p class="dialog-help">' + h(client.note) + ' Keep Freeflow running while your AI client uses it.</p>' +
        code(configuration[selected]) + '<div class="form-row">' + btn('copy-config', selected === 'desktop' ? 'Copy JSON' : 'Copy command', 'copy') +
        btn('copy-prompt', 'Copy starter prompt', 'copy', 'soft') + '</div>' +
        '<p class="dialog-help">For ChatGPT, use Codex with this local MCP server. ' +
        external('https://help.openai.com/en/articles/12584461', 'ChatGPT’s web app uses remote MCP servers') + '.</p></div>');
      for (const item of clients) click('#client-' + item.id, () => display(item.id));
      click('#copy-config', () => copy(configuration[selected], selected === 'desktop' ? 'MCP configuration' : 'Command'));
      click('#copy-prompt', () => copy(configuration.prompt, 'Starter prompt'));
    }
    display('codex');
  }

  async function backends(selected = 'firebase') {
    const project = state.project;
    const config = await projectApi('backends');
    if (project !== state.project) return;
    const provider = config[selected];
    const firebase = selected === 'firebase';
    const name = firebase ? 'Firebase' : 'Supabase';
    const firebaseInput = '<p class="dialog-help">Register a web app in ' + external(firebaseConsole, 'Firebase Project settings') + ' and paste its app settings.</p>' +
      '<div class="form-grid">' +
      field('projectId', 'Project ID', provider.projectId, 'required') + field('appId', 'Web app ID', provider.appId, 'required') +
      field('apiKey', 'Web API key', provider.apiKey, 'required') + field('messagingSenderId', 'Sender ID', provider.messagingSenderId, 'required') +
      field('authDomain', 'Auth domain', provider.authDomain) + field('storageBucket', 'Storage bucket', provider.storageBucket) +
      '</div><details><summary>Android, iOS and optional fields</summary><div class="form-grid">' +
      field('androidAppId', 'Android app ID', provider.androidAppId) + field('androidApiKey', 'Android API key', provider.androidApiKey) +
      field('iosAppId', 'iOS app ID', provider.iosAppId) + field('iosApiKey', 'iOS API key', provider.iosApiKey) +
      field('iosBundleId', 'iOS bundle ID', provider.iosBundleId) + field('iosClientId', 'iOS OAuth client ID', provider.iosClientId) +
      field('databaseURL', 'Database URL', provider.databaseURL) + field('measurementId', 'Measurement ID', provider.measurementId) +
      '</div></details>';
    const supabaseInput = '<p class="dialog-help">Find your project URL and publishable key in ' + external(supabaseConsole, 'Supabase Connect') + '.</p>' +
      field('url', 'Project URL', provider.url, 'required type="url" placeholder="https://your-project.supabase.co"') +
      field('key', 'Publishable key', provider.key, 'required type="password" autocomplete="off" placeholder="sb_publishable_…"');
    const wiring = !config.wired ? '<details><summary>Initialize from main.dart</summary>' +
      '<p class="dialog-help">Add the import and initialization to your existing main function before runApp.</p>' + code(config.startupCode) + '</details>' : '';
    const configInfo = config.definesFile ? '<details><summary>Build outside Freeflow</summary>' +
      '<p class="dialog-help">Pass this Dart defines file with --dart-define-from-file when running or building from a terminal.</p>' +
      code(config.definesFile) + '<div class="form-row">' + btn('copy-defines', 'Copy path', 'copy', 'soft') + '</div></details>' : '';
    open('Backends', 'Connect your Flutter app to a project you own.',
      '<div class="dialog-tabs">' + btn('firebase', 'Firebase', 'database', firebase ? 'active' : 'soft') +
      btn('supabase', 'Supabase', 'database', firebase ? 'soft' : 'active') + '</div><form class="form">' +
      (provider.enabled ? callout(h(name) + ' is configured for this project.') : '') + (firebase ? firebaseInput : supabaseInput) +
      wiring + configInfo + msg + '<div class="form-footer">' + (provider.enabled ? btn('disconnect', 'Disconnect', 'x', 'danger') : '') +
      submit(provider.enabled ? 'Save configuration' : 'Connect ' + name) + '</div></form>');
    click('#firebase', () => backends('firebase'));
    click('#supabase', () => backends('supabase'));
    click('#copy-defines', () => copy(config.definesFile, 'Dart defines path'));
    const configure = async (input, version = view) => {
      if (state.project !== project) throw new Error('Open the project again.');
      const job = await projectPost('backends', { provider: selected, config: input });
      track(job);
      await waitJob(job);
      toast(input.enabled === false ? name + ' disconnected' : name + ' configured');
      if (current(version)) await backends(selected);
    };
    form(configure);
    click('#disconnect', () => configure({ enabled: false }));
  }

  const exportOptions = [
    { id: 'web', icon: 'globe', label: 'Web', description: 'Static hosting ZIP' },
    { id: 'android', icon: 'android', label: 'Google Play', description: 'Release app bundle' },
    { id: 'ios', icon: 'apple', label: 'App Store', description: 'Signed IPA' },
    { id: 'source', icon: 'zip', label: 'Flutter source', description: 'Portable project ZIP' },
    { id: 'apk', icon: 'phone', label: 'Android device', description: 'Debug APK' },
  ];

  async function exports(selected = 'web') {
    const project = state.project;
    const [items, signing] = await Promise.all([projectApi('artifacts'), projectApi('signing')]);
    if (project !== state.project) return;
    const iosBlocked = selected === 'ios' && state.flutter.platform !== 'darwin';
    const android = selected === 'android';
    const isNative = ['android', 'apk', 'ios'].includes(selected);
    const options = exportOptions.map(item => '<button class="choice-card' + (item.id === selected ? ' selected' : '') +
      '" type="button" id="target-' + item.id + '" aria-pressed="' + (item.id === selected) + '">' + icon(item.icon) + '<span><strong>' +
      h(item.label) + '</strong><small>' + h(item.description) + '</small></span></button>').join('');
    const previous = items.slice(0, 8).map(item => '<div class="dialog-item"><div class="dialog-item-text"><strong class="ellipsis">' +
      h(item.name) + '</strong><small>' + h(item.label + ' · ' + kb(item.size) + ' · ' + when(item.date) + (item.debugSigned ? ' · Debug key' : '')) +
      '</small></div><button type="button" class="icon-button" id="download-' + item.id + '" aria-label="Download ' +
      h(item.name) + '" title="Download">' + icon('download') + '</button></div>').join('');
    const signingNote = android ? (signing.debugFallback ? callout('This project uses a debug key. Configure an upload key for Play Store submission.', true) :
      signing.configured ? callout('Upload signing is configured' + (signing.appId ? ' for ' + h(signing.appId) : '') + '.') : '') : '';
    const iosNote = selected === 'ios' ? callout(iosBlocked
      ? 'Build an App Store IPA on macOS with Xcode. Export Flutter source here and open the project on your Mac.'
      : 'Select your team and provisioning profile in Xcode before building.', iosBlocked) : '';
    const sourceNote = selected === 'source' ? callout('Includes your Flutter source, assets and platform projects. Backend defines and signing keys stay in their local folders.') : '';
    open('Export app', project.name, '<form class="form"><div class="choice-grid">' + options + '</div>' +
      (selected === 'web' ? field('baseHref', 'Web base path', '/', '', 'Use / for a domain or /my-app/ for a subfolder.') : '') +
      (isNative && !iosBlocked ? '<details><summary>Build options</summary>' + field('flavor', 'Flutter flavor', '', 'placeholder="Optional"') + '</details>' : '') +
      signingNote + iosNote + sourceNote +
      (android ? '<div class="form-row">' + (signing.compatible ? btn('android-signing', 'Configure signing', 'key', 'soft') : '') +
        external(playConsole, 'Google Play Console') + '</div>' : '') +
      (selected === 'ios' ? '<p class="dialog-help">' + external(appStoreConnect, 'App Store Connect') + '</p>' : '') +
      msg + '<div class="form-footer">' + (iosBlocked ? btn('ios-source', 'Switch to source export', 'zip', 'outline') : submit('Build export')) +
      '</div></form>' + (previous ? '<hr class="dialog-separator"><div class="modal-heading">Recent exports</div><div class="dialog-list">' + previous + '</div>' : ''));
    for (const item of exportOptions) click('#target-' + item.id, () => exports(item.id));
    click('#ios-source', () => exports('source'));
    click('#android-signing', () => androidSigning(signing));
    for (const item of items.slice(0, 8)) click('#download-' + item.id, async () => {
      const { url } = await projectPost('download', { id: item.id });
      const link = document.createElement('a');
      link.href = url;
      link.download = item.name;
      document.body.append(link);
      link.click();
      link.remove();
    });
    if (!iosBlocked) form(async (input, version) => {
      const job = await projectPost('build', { target: selected, ...input });
      track(job);
      const result = await waitJob(job);
      toast('Exported ' + result.artifact.name);
      if (current(version)) await exports(selected);
    });
  }

  function androidSigning(signing) {
    open('Android upload signing', signing.appId || 'Use the upload key for this Google Play app.',
      '<form class="form">' + field('storeFile', 'Keystore path', '', 'required autofocus', 'Absolute path to a .jks, .keystore or .p12 file.') +
      field('keyAlias', 'Key alias', '', 'required placeholder="upload"') +
      '<div class="form-grid">' + field('storePassword', 'Keystore password', '', 'required type="password" autocomplete="new-password"') +
      field('keyPassword', 'Key password', '', 'required type="password" autocomplete="new-password"') + '</div>' +
      callout('Freeflow writes the passwords to the local android/key.properties file used by Gradle and adds it to .gitignore.') +
      '<p class="dialog-help">' + external('https://docs.flutter.dev/deployment/android#sign-the-app', 'Create an upload keystore') + '</p>' +
      msg + '<div class="form-footer">' + btn('back-exports', 'Back', '', 'soft') + submit('Save signing') + '</div></form>');
    click('#back-exports', () => exports('android'));
    form(async (input, version) => {
      const job = await projectPost('signing', input);
      track(job);
      await waitJob(job);
      toast('Android upload signing configured');
      if (current(version)) await exports('android');
    });
  }

  async function settings() {
    await refreshState();
    const sdk = state.flutter;
    const config = sdk.settings || {};
    const platform = sdk.platform === 'win32' ? 'C:\\src\\flutter' : '/opt/flutter';
    const status = sdk.found ? 'Flutter ' + (sdk.version || 'SDK detected') +
      (sdk.mode === 'managed' ? ' · Freeflow-managed' : ' · Existing installation') : 'Flutter SDK unavailable';
    open('Settings', 'Flutter SDK and local build settings.',
      '<form class="form">' + callout(h(status) + (sdk.latest ? '<br>Latest stable ' + h(sdk.latest) : '')) +
      '<div class="form-row">' + btn('sdk-check', 'Check latest stable', 'reload') +
      btn('sdk-install', sdk.mode === 'managed' ? 'Update managed SDK' : 'Install managed SDK', 'download') +
      btn('sdk-doctor', 'Flutter doctor', 'terminal', 'soft', !sdk.found) + '</div>' +
      '<p class="dialog-help" id="settings-progress" hidden></p><pre class="code-card" id="doctor-output" hidden></pre>' +
      field('flutterPath', 'Flutter SDK path override', config.flutterPath, 'placeholder="' + h(platform) + '"',
        'Leave blank to use the Freeflow-managed SDK or Flutter on PATH.') +
      '<label class="check-row"><input type="checkbox" name="managedAutoUpdate"' + (config.managedAutoUpdate !== false ? ' checked' : '') +
      '><span>Automatically update the Freeflow-managed SDK to the latest stable release</span></label>' +
      '<label class="check-row"><input type="checkbox" name="offline"' + (config.offline ? ' checked' : '') +
      '><span>Use cached Dart packages offline</span></label>' +
      msg + '<div class="form-footer">' + submit('Save settings') + '</div></form>');
    const id = view;
    click('#sdk-check', async event => {
      const button = event.currentTarget;
      button.disabled = true;
      try {
        const next = await post('/sdk/check');
        toast(next.latest ? 'Latest stable Flutter ' + next.latest : 'Flutter release checked');
        if (current(id)) await settings();
      } finally { button.disabled = false; }
    });
    click('#sdk-install', async event => {
      const button = event.currentTarget;
      button.disabled = true;
      try {
        const job = await post('/sdk/install');
        track(job);
        const progress = find('#settings-progress');
        progress.hidden = false;
        progress.textContent = 'Downloading and verifying the Flutter SDK. Progress appears in the console.';
        const result = await waitJob(job);
        if (config.flutterPath) await post('/settings', { ...config, flutterPath: '' });
        await refreshState();
        toast('Flutter ' + result.version + ' stable installed');
        if (current(id)) await settings();
      } finally { button.disabled = false; }
    });
    click('#sdk-doctor', async event => {
      const button = event.currentTarget;
      button.disabled = true;
      try {
        const result = await post('/sdk/doctor');
        if (current(id)) {
          const output = find('#doctor-output');
          output.hidden = false;
          output.textContent = result.output || 'Flutter doctor returned no output.';
        }
      } finally { button.disabled = false; }
    });
    form(async (input, version) => {
      await post('/settings', {
        flutterPath: input.flutterPath.trim(), managedAutoUpdate: !!input.managedAutoUpdate, offline: !!input.offline,
      });
      await refreshState();
      toast('Settings saved');
      if (current(version)) close();
    });
  }

  async function history() {
    const project = state.project;
    const file = currentFile();
    if (state.drafts.has(project.id + '/' + file)) throw new Error('Save the draft before restoring file history.');
    const query = '?path=' + encodeURIComponent(file);
    const entries = await projectApi('history' + query);
    const items = entries.map(entry => '<div class="dialog-item"><div class="dialog-item-text"><strong>' + h(when(entry.date)) +
      '</strong><small class="mono">' + h(entry.revision.slice(0, 10)) + '</small></div>' + btn('version-' + entry.id, 'View', 'history', 'soft') + '</div>').join('');
    open('File history', file, '<div class="dialog-list">' + (items || '<p class="dialog-help">Freeflow keeps a copy of the previous version when you save an edit.</p>') + '</div>');
    for (const entry of entries) click('#version-' + entry.id, async () => {
      const old = await projectApi('history-version' + query + '&id=' + encodeURIComponent(entry.id));
      open('File version', file + ' · ' + when(entry.date), code(old.content) + '<div class="form-footer">' +
        btn('version-back', 'Back', '', 'soft') + btn('version-restore', 'Restore version', 'history', 'primary') + '</div>');
      click('#version-back', history);
      click('#version-restore', async () => {
        if (project !== state.project || file !== currentFile()) throw new Error('Open the file again before restoring.');
        if (state.drafts.has(project.id + '/' + file)) throw new Error('Save the draft before restoring file history.');
        await projectApi('file', { method: 'PUT', data: { path: file, content: old.content, revision: state.fileVersion.revision } });
        await loadFile(file, { keep: true });
        close();
        toast('Restored ' + file.split('/').at(-1));
      });
    });
  }

  function wrap(node) {
    open('Wrap widget', node.type, '<form class="form"><label class="form-field">Wrapper<select name="widget" autofocus>' +
      state.visual.wrappers.map(item => '<option>' + h(item) + '</option>').join('') + '</select></label>' +
      msg + '<div class="form-footer">' + submit('Wrap widget') + '</div></form>');
    form(async (input, version) => {
      await visualEdit({ id: node.id, operation: 'wrap', widget: input.widget });
      if (current(version)) close();
    });
  }

  function newFile() {
    open('New project file', '', '<form class="form">' +
      field('path', 'Project-relative path', '', 'required autofocus placeholder="lib/screens/home.dart"') +
      msg + '<div class="form-footer">' + submit('Create file') + '</div></form>');
    form(async (input, version) => {
      const file = input.path.trim();
      await projectApi('file', { method: 'PUT', data: { path: file, content: '', revision: null } });
      await refreshTree();
      if (current(version)) close();
      await selectFile(file, 'code');
    });
  }

  function quickOpen() {
    open('Open file', '', '<div class="form">' + field('search', 'Search project files', '', 'autofocus placeholder="lib/main.dart"') +
      '<div class="dialog-list" id="quick-results"></div></div>');
    const search = find('[name="search"]');
    function render() {
      const input = search.value.toLowerCase().trim();
      const entries = state.tree.filter(item => item.type === 'file' && item.path.toLowerCase().includes(input)).slice(0, 16);
      find('#quick-results').innerHTML = entries.map((entry, i) => '<button type="button" class="button soft" id="quick-' + i + '">' +
        icon('file') + h(entry.path) + '</button>').join('') || '<p class="dialog-help">No matching files.</p>';
      for (const [i, entry] of entries.entries()) click('#quick-' + i, async () => { close(); await selectFile(entry.path, 'code'); });
    }
    search.addEventListener('input', render);
    search.addEventListener('keydown', event => {
      if (event.key === 'Enter') { event.preventDefault(); find('#quick-0')?.click(); }
    });
    render();
  }

  return { projects, create, import: importFolder, ai, backends, exports, settings, history, wrap, newFile, quickOpen };
}
