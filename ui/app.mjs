import { createEditor, escapeHtml as h } from './editor.mjs';
import { icon, initializeIcons } from './icons.mjs';
import { createDialogs } from './dialogs.mjs';

const byId = id => document.getElementById(id);
const token = document.querySelector('meta[name="freeflow-token"]').content;
const state = {
  projects: [], templates: [], project: null, flutter: {},
  mode: 'design', explorer: 'widgets', file: 'lib/main.dart', lastDart: 'lib/main.dart',
  fileVersion: null, fileError: '', openTabs: [], tree: [], visual: null, widget: null,
  folders: new Set(['lib', 'assets']), collapsed: new Set(), drafts: new Map(),
  preview: null, device: 'phone', consoleJob: null, logJobs: [], consoleOpen: false,
};
const previewSizes = { phone: [390, 780], tablet: [768, 930], desktop: [1280, 760] };
const finished = new Set(['succeeded', 'failed', 'stopped']);
let fileLoad = 0;
let assetUrl;
let busyPoll = false;
let polls = 0;
let previewKey = '';
let treeKey = '';
let draggedWidget;
let lastLogs = '';

export async function api(route, { method = 'GET', data, raw = false } = {}) {
  const headers = { 'X-Freeflow-Token': token };
  if (data !== undefined) headers['Content-Type'] = 'application/json';
  const response = await fetch('/api' + route, {
    method, headers, body: data === undefined ? undefined : JSON.stringify(data), cache: 'no-store',
  });
  if (response.ok) return raw ? response : response.json();
  const result = await response.json().catch(() => ({}));
  const error = new Error(result.error || 'Freeflow returned HTTP ' + response.status + '.');
  error.status = response.status;
  throw error;
}

function projectApi(part, options, project = state.project) {
  if (!project) return Promise.reject(new Error('Open a project first.'));
  return api('/projects/' + project.id + '/' + part, options);
}

function toast(text, error = false) {
  const node = document.createElement('div');
  node.className = 'toast' + (error ? ' error' : '');
  node.innerHTML = icon(error ? 'warning' : 'check-circle') + '<span>' + h(text) + '</span>';
  byId('toasts').append(node);
  setTimeout(() => node.remove(), error ? 6500 : 3500);
}
const report = error => toast(error.message, true);
const guarded = fn => event => {
  try { Promise.resolve(fn(event)).catch(report); } catch (error) { report(error); }
};
const post = (route, data = {}) => api(route, { method: 'POST', data });
const projectPost = (route, data = {}) => projectApi(route, { method: 'POST', data });
const currentFile = () => state.mode === 'plan' ? 'FREEFLOW_PLAN.md' : state.file;
const draftKey = (file = currentFile()) => state.project?.id + '/' + file;
const basename = file => file.split('/').at(-1);
const query = file => '?path=' + encodeURIComponent(file);
const isImage = file => /\.(?:avif|gif|ico|jpe?g|png|webp)$/i.test(file);

const editor = createEditor(byId('editor'), value => {
  const record = state.fileVersion;
  if (!record) return;
  const key = draftKey();
  if (value === record.content) state.drafts.delete(key);
  else {
    const original = state.drafts.get(key);
    state.drafts.set(key, { content: value, revision: original ? original.revision : record.revision });
  }
  renderEditorChrome();
});

function renderApp() {
  const project = state.project;
  byId('active-project').textContent = project?.name || 'Projects';
  for (const id of ['backends', 'export', 'run', 'analyze', 'test']) byId(id).disabled = !project;
  byId('workspace-empty').hidden = !!project;
  byId('design-pane').hidden = !project || state.mode !== 'design';
  byId('editing-pane').hidden = !project || state.mode === 'design';
  for (const mode of document.querySelectorAll('[data-mode]')) {
    mode.classList.toggle('active', mode.dataset.mode === state.mode);
    mode.setAttribute('aria-pressed', String(mode.dataset.mode === state.mode));
  }
  const sdk = state.flutter;
  byId('sdk-status').innerHTML = icon('flutter', 15) + '<span>' + h(sdk.found ? 'Flutter ' + (sdk.version || 'SDK') : 'Install Flutter') + '</span>' +
    (sdk.available ? '<span class="update-dot" title="Latest stable is available">●</span>' : '');
  renderExplorer();
  renderInspector();
  renderEditorChrome();
  renderPreview(true);
}

async function refreshState() {
  const next = await api('/state');
  state.projects = next.projects;
  state.templates = next.templates;
  state.flutter = next.flutter;
  renderApp();
  return next;
}

async function refreshTree(project = state.project) {
  if (!project) return;
  const { entries } = await projectApi('tree', undefined, project);
  if (project !== state.project) return;
  const key = entries.map(entry => entry.path).join('\0');
  if (treeKey === key) return;
  state.tree = entries;
  treeKey = key;
  renderExplorer();
}

async function activateProject(project) {
  if (!project) return;
  localStorage.setItem('freeflow-project', project.id);
  state.project = project;
  state.mode = 'design';
  state.explorer = 'widgets';
  state.file = 'lib/main.dart';
  state.lastDart = state.file;
  state.tree = [];
  state.openTabs = [];
  state.visual = null;
  state.widget = null;
  state.preview = null;
  state.fileVersion = null;
  state.fileError = '';
  state.folders = new Set(['lib', 'assets']);
  state.collapsed.clear();
  state.logJobs = [];
  state.consoleJob = null;
  treeKey = '';
  previewKey = '';
  setConsole(false);
  renderApp();
  await refreshTree(project);
  if (project !== state.project) return;
  if (!state.tree.some(entry => entry.path === state.file)) {
    const first = state.tree.find(entry => entry.path.endsWith('.dart') && entry.path.startsWith('lib/'));
    if (first) state.file = state.lastDart = first.path;
  }
  await Promise.all([loadFile(currentFile()), updatePreview(project)]);
}

function nestedRows(entries, keyOf, parentOf) {
  const groups = new Map();
  for (const entry of entries) {
    const parent = parentOf(entry);
    if (!groups.has(parent)) groups.set(parent, []);
    groups.get(parent).push(entry);
  }
  return { roots: groups.get(null) || [], children: entry => groups.get(keyOf(entry)) || [] };
}

function widgetIcon(type) {
  if (type === 'Text' || type === 'TextField') return 'type';
  if (type === 'Image' || type.startsWith('Image.')) return 'image';
  if (['Row', 'Column', 'Stack', 'ListView', 'Wrap', 'Scaffold', 'MaterialApp'].includes(type.split('.')[0])) return 'layout';
  return 'box';
}

function renderExplorer() {
  const widgets = state.explorer === 'widgets';
  for (const tab of document.querySelectorAll('[data-explorer]')) {
    const active = tab.dataset.explorer === state.explorer;
    tab.classList.toggle('active', active);
    tab.setAttribute('aria-pressed', String(active));
  }
  byId('source-select-wrap').hidden = !state.project || !widgets;
  byId('file-actions').hidden = !state.project || widgets;
  const tree = byId('explorer-tree');
  if (!state.project) {
    tree.innerHTML = '<p class="tree-empty">Open a Flutter project to browse its widgets and files.</p>';
    return;
  }
  if (widgets) {
    const paths = state.tree.filter(entry => entry.type === 'file' && entry.path.endsWith('.dart')).map(entry => entry.path);
    const source = byId('source-select');
    source.innerHTML = paths.map(file => '<option value="' + h(file) + '">' + h(file) + '</option>').join('');
    source.value = paths.includes(state.file) ? state.file : state.lastDart;
    const nodes = state.visual?.nodes || [];
    if (!nodes.length) {
      tree.innerHTML = '<p class="tree-empty">Select a Dart file with Flutter widget constructors.</p>';
      return;
    }
    const groups = nestedRows(nodes, node => node.id, node => node.parent);
    function row(node, depth) {
      const items = groups.children(node);
      const collapsed = state.collapsed.has(node.id);
      const selected = state.widget === node.id;
      const label = node.label ? String(node.label).replace(/\s+/g, ' ') : '';
      return '<button type="button" class="tree-row' + (selected ? ' selected' : '') +
        '" data-widget="' + h(node.id) + '" style="--depth:' + Math.min(depth, 10) + '" draggable="' + node.movable +
        '" aria-pressed="' + selected + '" title="' + h(node.type + (label ? ' · ' + label : '') + ' · line ' + node.line) + '">' +
        '<span class="chevron"' + (items.length ? ' data-collapse="' + h(node.id) + '"' : '') + '>' +
        (items.length ? icon(collapsed ? 'chevron' : 'down') : '') + '</span><span class="tree-symbol">' + icon(widgetIcon(node.type)) +
        '</span><span class="tree-label ellipsis">' + h(node.type) + '</span>' +
        (label ? '<span class="tree-hint ellipsis">' + h(label) + '</span>' : '') + '</button>' +
        (collapsed ? '' : items.map(item => row(item, depth + 1)).join(''));
    }
    tree.innerHTML = groups.roots.map(node => row(node, 0)).join('');
    return;
  }
  const groups = nestedRows(state.tree, entry => entry.path, entry => entry.path.includes('/') ? entry.path.slice(0, entry.path.lastIndexOf('/')) : null);
  function row(entry, depth) {
    const directory = entry.type === 'directory';
    const expanded = state.folders.has(entry.path);
    const active = !directory && entry.path === currentFile();
    return '<button type="button" class="tree-row' + (active ? ' selected' : '') + '" data-' +
      (directory ? 'folder' : 'file') + '="' + h(entry.path) + '" style="--depth:' + Math.min(depth, 12) +
      '" title="' + h(entry.path) + '"><span class="chevron">' + (directory ? icon(expanded ? 'down' : 'chevron') : '') +
      '</span><span class="tree-symbol">' + icon(directory ? 'folder' : isImage(entry.path) ? 'image' : entry.path.endsWith('.dart') ? 'file-code' : 'file') +
      '</span><span class="tree-label ellipsis">' + h(basename(entry.path)) + '</span></button>' +
      (directory && expanded ? groups.children(entry).map(child => row(child, depth + 1)).join('') : '');
  }
  tree.innerHTML = groups.roots.map(entry => row(entry, 0)).join('');
}

function propertyInput(item) {
  const value = item.value ?? '';
  const data = ' data-property="' + h(item.name) + '" data-type="' + h(item.type) + '" aria-label="' + h(item.name) + '"';
  if (!item.editable) return '<input aria-label="' + h(item.name) + '" readonly value="' + h(item.expression || 'Code expression') + '" title="' + h(item.expression || 'Edit in Code') + '">';
  if (item.type === 'color') {
    return '<span class="color-input"><input type="color"' + data + ' value="' + h(value || '#ada1ff') + '">' +
      '<input type="text"' + data + ' value="' + h(value) + '" placeholder="' + h(item.expression || '#RRGGBB') + '"></span>';
  }
  if (item.type === 'select' || item.type === 'boolean') {
    const options = item.options || ['true', 'false'];
    return '<select' + data + '><option value="">Default</option>' + options.map(option => '<option value="' + h(option) + '"' +
      (String(value) === option ? ' selected' : '') + '>' + h(option) + '</option>').join('') + '</select>';
  }
  if (item.name === 'text') return '<textarea rows="2"' + data + '>' + h(value) + '</textarea>';
  const numeric = ['number', 'integer', 'spacing'].includes(item.type);
  return '<input type="' + (numeric ? 'number' : 'text') + '"' + data + (numeric ? ' step="' + (item.type === 'integer' ? '1' : 'any') + '" min="0"' : '') +
    ' value="' + h(value) + '" placeholder="Default">';
}

function renderInspector() {
  const panel = byId('inspector');
  const node = state.visual?.nodes.find(item => item.id === state.widget);
  if (!node || !state.project) {
    panel.innerHTML = '<div class="inspector-empty">' + icon('cursor') + '<strong>Select a widget</strong>' +
      '<p>Choose a widget in the tree to see its properties and layout controls.</p></div>';
    return;
  }
  const siblings = state.visual.nodes.filter(item => item.movable && item.parent === node.parent && item.listSlot === node.listSlot);
  const place = siblings.findIndex(item => item.id === node.id);
  const siblingButtons = node.movable ?
    '<button type="button" class="icon-button" data-move="up" aria-label="Move widget up" title="Move up"' + (place < 1 ? ' disabled' : '') + '>' + icon('up') + '</button>' +
    '<button type="button" class="icon-button" data-move="down" aria-label="Move widget down" title="Move down"' + (place >= siblings.length - 1 ? ' disabled' : '') + '>' + icon('down') + '</button>' : '';
  const properties = node.properties.length ? '<div class="inspect-group"><h3>Appearance &amp; layout</h3>' +
    node.properties.map(item => '<label class="property"><span class="property-name">' + h(item.name) + '</span>' + propertyInput(item) + '</label>').join('') + '</div>' : '';
  const add = node.slots.length ? '<div class="inspect-group"><h3>Add widget</h3>' +
    '<select id="insert-slot" aria-label="Child slot">' + node.slots.map(slot => '<option>' + h(slot) + '</option>').join('') + '</select>' +
    '<div class="add-row"><select id="insert-widget" aria-label="Widget to add">' + node.slotChoices[node.slots[0]].map(widget => '<option>' + h(widget) + '</option>').join('') +
    '</select><button type="button" data-widget-action="add" class="button outline small">' + icon('plus') + 'Add</button></div></div>' : '';
  panel.innerHTML = '<div class="inspect-title"><div class="inspect-widget">' + icon(widgetIcon(node.type), 18) + h(node.type) +
    '</div><div class="inspect-code">' + h(state.visual.path) + ' · ' + node.line + '</div><div class="inspect-actions">' +
    '<button type="button" data-widget-action="code" class="button soft small">' + icon('code') + 'Code</button>' +
    '<button type="button" data-widget-action="wrap" class="button soft small"' + (node.wrappable === false ? ' disabled' : '') + '>' + icon('wrap') + 'Wrap</button>' +
    siblingButtons + (node.removable ? '<button type="button" class="icon-button" data-widget-action="remove" aria-label="Remove widget" title="Remove widget">' + icon('trash') + '</button>' : '') +
    '</div></div>' + properties + add;
}

function renderEditorChrome() {
  const file = currentFile();
  const source = state.fileVersion;
  const draft = state.drafts.get(draftKey(file));
  const editing = state.mode !== 'design' && !!state.project;
  byId('editor-name').innerHTML = icon(state.mode === 'plan' ? 'plan' : isImage(file) ? 'image' : 'file-code') +
    '<span class="ellipsis" title="' + h(file) + '">' + h(state.mode === 'plan' ? 'App plan' : file) + (draft ? ' •' : '') + '</span>';
  byId('save').disabled = !source || !draft;
  byId('format').hidden = state.mode !== 'code' || !file.endsWith('.dart');
  byId('format').disabled = !source;
  byId('history').disabled = !source?.revision;
  const notice = byId('editor-notice');
  const conflicted = !!draft && draft.revision !== source?.revision;
  notice.hidden = !conflicted;
  if (conflicted) notice.innerHTML = '<span>File changed on disk. Your draft is still here.</span>' +
    '<button type="button" id="reload-source" class="button outline small">Use disk version</button>';
  byId('editor').hidden = !editing || !source;
  const asset = byId('asset-view');
  asset.hidden = !editing || !!source;
  if (!source && !isImage(file)) asset.innerHTML = '<p>' + h(state.fileError || 'Choose a source file.') + '</p>';
  const tabs = byId('editor-tabs');
  tabs.hidden = state.mode !== 'code' || !state.openTabs.length;
  if (!tabs.hidden) {
    tabs.innerHTML = state.openTabs.map(tab => '<button type="button" class="editor-tab' + (tab === file ? ' active' : '') +
      '" data-file="' + h(tab) + '" title="' + h(tab) + '">' + h(basename(tab)) + (state.drafts.has(draftKey(tab)) ? ' •' : '') + '</button>').join('');
  }
}

async function updateVisual(project, file, preferId = state.widget) {
  if (!file.endsWith('.dart')) return;
  const result = await projectApi('widgets' + query(file), undefined, project).catch(error => {
    if (error.status !== 404) throw error;
    return null;
  });
  if (state.project !== project || state.file !== file) return;
  const before = state.visual?.nodes.find(item => item.id === preferId);
  state.visual = result;
  const nodes = result?.nodes || [];
  state.widget = (nodes.find(item => item.id === preferId && (!before || before.type === item.type)) ||
    nodes.find(item => before && item.type === before.type && item.line === before.line) ||
    nodes.find(item => item.label?.startsWith('Make today')) || nodes[0])?.id || null;
  renderExplorer();
  renderInspector();
}

async function loadFile(file, { keep = false } = {}) {
  const project = state.project;
  if (!project) return;
  const request = ++fileLoad;
  const matches = () => state.project === project && request === fileLoad && currentFile() === file;
  const asset = byId('asset-view');
  if (!keep) {
    state.fileVersion = null;
    state.fileError = '';
    if (assetUrl) URL.revokeObjectURL(assetUrl);
    assetUrl = null;
    asset.replaceChildren();
    renderEditorChrome();
  }
  try {
    if (isImage(file)) {
      const response = await projectApi('asset' + query(file), { raw: true }, project);
      const blob = await response.blob();
      if (!matches()) return;
      assetUrl = URL.createObjectURL(blob);
      asset.innerHTML = '<img alt="' + h(basename(file)) + '" src="' + h(assetUrl) + '"><code>' + h(file) + '</code>' +
        '<p class="dialog-help">Declare this asset under flutter.assets in pubspec.yaml.</p>';
      return;
    }
    const plan = file === 'FREEFLOW_PLAN.md' && state.mode === 'plan';
    const source = await projectApi(plan ? 'plan' : 'file' + query(file), undefined, project);
    if (!matches()) return;
    state.fileVersion = source;
    const draft = state.drafts.get(draftKey(file));
    editor.set(draft?.content ?? source.content, file.endsWith('.dart') ? 'dart' : 'markdown', keep);
    renderEditorChrome();
    if (file.endsWith('.dart')) await updateVisual(project, file);
  } catch (error) {
    if (!matches()) return;
    state.fileError = error.message;
    if (isImage(file)) asset.innerHTML = '<p>' + h(error.message) + '</p>';
    renderEditorChrome();
  }
}

async function selectFile(file, mode = state.mode) {
  if (!state.project) return;
  state.mode = mode;
  if (mode !== 'plan') {
    state.file = file;
    if (file.endsWith('.dart')) state.lastDart = file;
  }
  if (mode === 'code') {
    state.explorer = 'files';
    if (!state.openTabs.includes(file)) state.openTabs = [...state.openTabs, file].slice(-9);
    for (let i = file.indexOf('/'); i >= 0; i = file.indexOf('/', i + 1)) state.folders.add(file.slice(0, i));
  }
  if (mode === 'design') state.explorer = 'widgets';
  renderApp();
  await loadFile(currentFile());
}

async function setMode(mode) {
  if (!state.project || mode === state.mode) return;
  const file = mode === 'design' ? state.lastDart : state.file;
  await selectFile(file, mode);
}

async function saveDraft() {
  const project = state.project;
  const file = currentFile();
  const key = draftKey(file);
  const draft = state.drafts.get(key);
  if (!draft || !project) return;
  const saved = await projectApi('file', { method: 'PUT', data: { path: file, ...draft } }, project);
  if (state.drafts.get(key) === draft) state.drafts.delete(key);
  else {
    const newer = state.drafts.get(key);
    if (newer) newer.revision = saved.revision;
  }
  if (project === state.project && file === currentFile()) {
    state.fileVersion = saved;
    renderEditorChrome();
    if (file.endsWith('.dart')) await updateVisual(project, file);
  }
  void refreshTree(project).catch(report);
  toast('Saved ' + basename(file));
}

async function visualEdit(command) {
  const visual = state.visual;
  const project = state.project;
  if (!visual || !project) return;
  if (state.drafts.has(draftKey(visual.path))) throw new Error('Save the code draft before editing its widgets.');
  const saved = await projectApi('widgets', { method: 'POST', data: { path: visual.path, revision: visual.revision, command } }, project);
  if (state.project !== project || state.file !== visual.path) return;
  state.fileVersion = saved;
  if (state.mode === 'code') editor.set(saved.content, 'dart', true);
  await updateVisual(project, visual.path, command.id);
}

function fitCanvas() {
  const canvas = byId('canvas');
  const [width, height] = previewSizes[state.device];
  const frame = byId('device-frame');
  const bezel = state.device === 'phone' ? 12 : state.device === 'tablet' ? 14 : 8;
  const scale = Math.max(.12, Math.min(1, (canvas.clientWidth - 46) / (width + bezel), (canvas.clientHeight - 63) / (height + bezel)));
  frame.className = 'device-frame ' + state.device;
  frame.style.width = width + bezel + 'px';
  frame.style.height = height + bezel + 'px';
  frame.style.transform = 'scale(' + scale + ')';
  byId('canvas-content').style.width = (width + bezel) * scale + 'px';
  byId('canvas-content').style.height = (height + bezel) * scale + 'px';
  byId('canvas-info').innerHTML = '<span>' + width + ' × ' + height + '</span><span>·</span><span>' + Math.round(scale * 100) + '%</span>' +
    (state.preview?.result?.reloading ? '<span class="busy-text">Refreshing</span>' : '');
}

function renderPreview(force = false) {
  const job = state.preview;
  const ready = job?.status === 'ready' && !!job.result?.url;
  const starting = ['queued', 'running'].includes(job?.status);
  const failed = job?.status === 'failed' || !!job?.result?.compileError;
  const key = [state.project?.id, job?.id, ready, starting, failed, job?.result?.epoch, job?.result?.reloading].join(':');
  if (!force && key === previewKey) return;
  previewKey = key;
  const canStop = ready || starting;
  byId('run').innerHTML = icon(canStop ? 'stop' : 'play', 15) + '<span>' + (canStop ? 'Stop' : 'Run') + '</span>';
  byId('reload').disabled = !ready || !!job.result.reloading;
  byId('open-preview').disabled = !ready;
  const status = failed ? ['failed', 'Build issue'] : starting ? ['working', 'Starting'] : ready ?
    [job.result.reloading ? 'working' : 'live', job.result.reloading ? 'Refreshing' : 'Preview live'] : ['', 'Idle'];
  byId('preview-status').innerHTML = '<span class="status-dot ' + status[0] + '"></span>' + status[1];
  const frame = byId('preview-frame');
  const empty = byId('preview-empty');
  frame.hidden = !ready;
  empty.hidden = ready;
  if (ready) {
    const url = new URL(job.result.url);
    url.searchParams.set('freeflow', job.id + '-' + job.result.epoch);
    if (frame.getAttribute('src') !== url.href) frame.src = url.href;
  } else {
    frame.removeAttribute('src');
    empty.innerHTML = (starting ? '<span class="spinner"></span>' : icon(failed ? 'warning' : 'preview', 38)) +
      '<strong>' + (starting ? 'Starting Flutter' : failed ? 'Preview stopped' : 'Run your app') + '</strong>' +
      '<p>' + (starting ? 'Resolving packages and compiling the web preview.' : failed ? 'Open the console to see the Flutter output.' : 'Launch a local preview to interact with your Flutter app.') + '</p>' +
      (starting ? '' : '<button type="button" class="button ' + (failed ? 'outline' : 'primary') + '" data-preview-action="' +
        (failed ? 'console' : 'start') + '">' + icon(failed ? 'terminal' : 'play') + (failed ? 'Open console' : 'Run preview') + '</button>');
  }
  fitCanvas();
}

async function updatePreview(project = state.project) {
  if (!project) return;
  const next = await projectApi('preview', undefined, project);
  if (project !== state.project) return;
  const changedFailure = next?.status === 'failed' && (next.id !== state.preview?.id || state.preview?.status !== 'failed');
  state.preview = next;
  renderPreview();
  if (changedFailure) {
    track(next, true);
    toast('Flutter preview could not start.', true);
  }
}

async function previewAction(action) {
  if (!state.project) return;
  if (action === 'start' && !state.flutter.found) {
    await dialogs.settings();
    return;
  }
  const project = state.project;
  const job = await projectPost('preview', { action });
  if (state.project === project) {
    state.preview = job;
    if (job && action !== 'stop') track(job, false);
    renderPreview();
  }
}

function setConsole(open) {
  state.consoleOpen = open;
  byId('console').hidden = !open;
  byId('console-toggle').setAttribute('aria-expanded', String(open));
  if (open) {
    byId('console-indicator').hidden = true;
    void refreshConsole().catch(() => {});
  }
}

function track(job, show = true) {
  if (!job) return;
  state.logJobs = [job, ...state.logJobs.filter(item => item.id !== job.id)].slice(0, 22);
  state.consoleJob = job.id;
  byId('console-indicator').hidden = state.consoleOpen;
  if (show) setConsole(true);
}

const jobLabels = {
  create: 'Create project', preview: 'Flutter preview', analyze: 'Analyze', test: 'Tests', format: 'Dart format',
  'flutter-sdk': 'Flutter SDK', 'backend-firebase': 'Firebase', 'backend-supabase': 'Supabase',
  'android-signing': 'Android signing', 'export-web': 'Web export', 'export-android': 'Google Play export',
  'export-apk': 'Android APK export', 'export-ios': 'App Store export', 'export-source': 'Flutter source export',
};
async function refreshConsole() {
  if (!state.consoleOpen) return;
  if (state.preview && !state.logJobs.some(job => job.id === state.preview.id)) {
    state.logJobs = [state.preview, ...state.logJobs].slice(0, 22);
    state.consoleJob ??= state.preview.id;
  }
  const choice = state.consoleJob;
  const current = choice ? await api('/jobs/' + choice).catch(() => null) : null;
  if (choice !== state.consoleJob || !state.consoleOpen) return;
  const select = byId('console-job');
  const options = state.logJobs.map(job => '<option value="' + h(job.id) + '">' + h(jobLabels[job.kind] || job.kind) + '</option>').join('');
  if (select.innerHTML !== options) select.innerHTML = options;
  select.value = choice || '';
  const output = byId('console-output');
  const lines = current ? (current.log.map(line => line.message).join('\n') || 'Waiting for command output…') : 'Run a preview, analysis or export to see Flutter output here.';
  if (lines !== lastLogs) {
    const follow = output.scrollHeight - output.scrollTop - output.clientHeight < 24;
    output.textContent = lines;
    if (follow) output.scrollTop = output.scrollHeight;
    lastLogs = lines;
  }
  byId('cancel-job').hidden = !current || current.kind === 'preview' || finished.has(current.status);
}

async function waitJob(job) {
  let current = job;
  while (!finished.has(current.status)) {
    await new Promise(resolve => setTimeout(resolve, 600));
    current = await api('/jobs/' + job.id);
  }
  if (current.status !== 'succeeded') {
    throw new Error(current.error || (current.status === 'stopped' ? 'The command was canceled.' : 'See the console for Flutter output.'));
  }
  return current.result;
}

async function newProject(data, preview = false) {
  if (!state.flutter.found) {
    await dialogs.settings();
    return null;
  }
  const job = await post('/projects/new', data);
  track(job, true);
  const result = await waitJob(job);
  await refreshState();
  await activateProject(result.project);
  if (preview) await previewAction('start');
  return result.project;
}

async function importProject(folder) {
  const project = await post('/projects/import', { folder });
  await refreshState();
  await activateProject(project);
  return project;
}

async function runCommand(action) {
  if (!state.project) return;
  if (!state.flutter.found) return dialogs.settings();
  if (action === 'format') await saveDraft();
  const project = state.project;
  const file = currentFile();
  const job = await projectPost('command', { action, path: file, revision: state.fileVersion?.revision });
  track(job);
  await waitJob(job);
  if (action === 'format' && state.project === project && currentFile() === file) await loadFile(file, { keep: true });
  toast(action === 'format' ? 'Formatted ' + basename(file) : action === 'test' ? 'Flutter tests passed' : 'Flutter analysis passed');
}

async function poll() {
  if (busyPoll || document.hidden) return;
  busyPoll = true;
  const project = state.project;
  try {
    if (project) {
      await updatePreview(project);
      if (state.consoleOpen) await refreshConsole();
      if (++polls % 4 === 0 && project === state.project) {
        await refreshTree(project);
        const file = currentFile();
        const current = state.fileVersion;
        if (current) {
          const next = await projectApi(state.mode === 'plan' ? 'plan' : 'file' + query(file), undefined, project).catch(() => null);
          if (next && current.revision !== next.revision && project === state.project && file === currentFile() && current === state.fileVersion) {
            await loadFile(file, { keep: true });
          }
        }
      }
    } else if (state.consoleOpen) await refreshConsole();
  } catch (error) {
    if (error.status === 401) byId('preview-status').textContent = 'Reload to reconnect';
  } finally { busyPoll = false; }
}

const dialogs = createDialogs({
  state, api, post, projectApi, projectPost, toast, refreshState, activateProject, newProject,
  importProject, track, waitJob, loadFile, selectFile, refreshTree, currentFile, saveDraft, visualEdit,
});

initializeIcons();
new ResizeObserver(fitCanvas).observe(byId('canvas'));
byId('project-switch').addEventListener('click', guarded(() => dialogs.projects()));
byId('new-project').addEventListener('click', guarded(() => dialogs.create()));
byId('import-project').addEventListener('click', guarded(() => dialogs.import()));
byId('try-sample').addEventListener('click', guarded(async () => {
  const button = byId('try-sample');
  button.disabled = true;
  try { await newProject({ name: 'Daymark', template: 'daymark', organization: 'dev.freeflow' }, true); }
  finally { button.disabled = false; }
}));
byId('ai').addEventListener('click', guarded(() => dialogs.ai()));
byId('backends').addEventListener('click', guarded(() => dialogs.backends()));
byId('export').addEventListener('click', guarded(() => dialogs.exports()));
byId('settings').addEventListener('click', guarded(() => dialogs.settings()));
byId('history').addEventListener('click', guarded(() => dialogs.history()));
byId('new-file').addEventListener('click', guarded(() => dialogs.newFile()));
byId('add-asset').addEventListener('click', () => byId('asset-input').click());
byId('asset-input').addEventListener('change', guarded(async event => {
  const file = event.target.files[0];
  event.target.value = '';
  if (!file) return;
  if (file.size > 10 * 1024 * 1024) throw new Error('Images must be 10 MiB or smaller.');
  const data = await new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result).split(',')[1]);
    reader.onerror = reject;
    reader.readAsDataURL(file);
  });
  const result = await projectPost('asset', { name: file.name, data });
  await refreshTree();
  await selectFile(result.path, 'code');
}));
byId('run').addEventListener('click', guarded(() => previewAction(['ready', 'running', 'queued'].includes(state.preview?.status) ? 'stop' : 'start')));
byId('reload').addEventListener('click', guarded(() => previewAction('reload')));
byId('open-preview').addEventListener('click', () => {
  if (state.preview?.result?.url) window.open(state.preview.result.url, '_blank', 'noopener,noreferrer');
});
byId('preview-empty').addEventListener('click', guarded(event => {
  const action = event.target.closest('[data-preview-action]')?.dataset.previewAction;
  if (action === 'console') setConsole(true);
  if (action === 'start') return previewAction(action);
}));
byId('save').addEventListener('click', guarded(saveDraft));
byId('format').addEventListener('click', guarded(() => runCommand('format')));
byId('analyze').addEventListener('click', guarded(() => runCommand('analyze')));
byId('test').addEventListener('click', guarded(() => runCommand('test')));
byId('editor-tabs').addEventListener('click', guarded(event => {
  const file = event.target.closest('[data-file]')?.dataset.file;
  if (file) return selectFile(file, 'code');
}));
byId('editor-notice').addEventListener('click', guarded(event => {
  if (event.target.id !== 'reload-source') return;
  state.drafts.delete(draftKey());
  return loadFile(currentFile(), { keep: true });
}));
byId('console-toggle').addEventListener('click', () => setConsole(!state.consoleOpen));
byId('console-close').addEventListener('click', () => setConsole(false));
byId('console-job').addEventListener('change', guarded(event => {
  state.consoleJob = event.target.value;
  return refreshConsole();
}));
byId('cancel-job').addEventListener('click', guarded(() => post('/jobs/' + state.consoleJob + '/stop')));
byId('mobile-explorer').addEventListener('click', () => document.querySelector('.workbench').classList.toggle('show-explorer'));
byId('mobile-inspector').addEventListener('click', event => {
  const open = document.querySelector('.workbench').classList.toggle('show-inspector');
  event.currentTarget.setAttribute('aria-pressed', String(open));
});

for (const node of document.querySelectorAll('[data-mode]')) node.addEventListener('click', guarded(() => setMode(node.dataset.mode)));
for (const node of document.querySelectorAll('[data-device]')) node.addEventListener('click', () => {
  state.device = node.dataset.device;
  for (const item of document.querySelectorAll('[data-device]')) {
    item.classList.toggle('active', item === node);
    item.setAttribute('aria-pressed', String(item === node));
  }
  fitCanvas();
});
for (const node of document.querySelectorAll('[data-explorer]')) node.addEventListener('click', () => {
  state.explorer = node.dataset.explorer;
  renderExplorer();
});
byId('source-select').addEventListener('change', guarded(event => selectFile(event.target.value, 'design')));
const treeElement = byId('explorer-tree');
treeElement.addEventListener('click', guarded(event => {
  const collapsed = event.target.closest('[data-collapse]')?.dataset.collapse;
  if (collapsed !== undefined) {
    if (state.collapsed.has(collapsed)) state.collapsed.delete(collapsed);
    else state.collapsed.add(collapsed);
    return renderExplorer();
  }
  const button = event.target.closest('button');
  if (!button) return;
  const { folder, file, widget } = button.dataset;
  if (folder) {
    if (state.folders.has(folder)) state.folders.delete(folder);
    else state.folders.add(folder);
    renderExplorer();
  } else if (file) return selectFile(file, 'code');
  else if (widget !== undefined) {
    state.widget = widget;
    renderExplorer();
    renderInspector();
  }
}));
treeElement.addEventListener('dragstart', event => {
  const button = event.target.closest('[data-widget]');
  const node = state.visual?.nodes.find(item => item.id === button?.dataset.widget);
  if (!node?.movable) return;
  draggedWidget = node;
  event.dataTransfer.effectAllowed = 'move';
  event.dataTransfer.setData('text/plain', node.id);
});
function dropTarget(event) {
  const button = event.target.closest('[data-widget]');
  const node = state.visual?.nodes.find(item => item.id === button?.dataset.widget);
  if (!draggedWidget || !node?.movable || node.parent !== draggedWidget.parent || node.listSlot !== draggedWidget.listSlot || node.id === draggedWidget.id) return null;
  const box = button.getBoundingClientRect();
  return { button, node, side: event.clientY < box.top + box.height / 2 ? 'before' : 'after' };
}
function clearDrop() {
  for (const row of treeElement.querySelectorAll('.drop-before,.drop-after')) row.classList.remove('drop-before', 'drop-after');
}
treeElement.addEventListener('dragover', event => {
  clearDrop();
  const target = dropTarget(event);
  if (!target) return;
  event.preventDefault();
  event.dataTransfer.dropEffect = 'move';
  target.button.classList.add('drop-' + target.side);
});
treeElement.addEventListener('dragend', () => { draggedWidget = null; clearDrop(); });
treeElement.addEventListener('drop', guarded(event => {
  const target = dropTarget(event);
  clearDrop();
  if (!target) return;
  event.preventDefault();
  return visualEdit({ id: draggedWidget.id, operation: 'move', target: target.node.id, side: target.side });
}));

byId('inspector').addEventListener('change', guarded(event => {
  const input = event.target;
  if (input.id === 'insert-slot') {
    const node = state.visual?.nodes.find(item => item.id === state.widget);
    byId('insert-widget').innerHTML = (node?.slotChoices[input.value] || []).map(widget => '<option>' + h(widget) + '</option>').join('');
    return;
  }
  const { property, type } = input.dataset;
  if (!property || (!input.value && type !== 'string')) return;
  const value = type === 'boolean' ? input.value === 'true' : ['number', 'integer', 'spacing'].includes(type) ? Number(input.value) : input.value;
  return visualEdit({ id: state.widget, operation: 'property', name: property, value });
}));
byId('inspector').addEventListener('click', guarded(async event => {
  const button = event.target.closest('button');
  const node = state.visual?.nodes.find(item => item.id === state.widget);
  if (!button || !node) return;
  if (button.dataset.move) {
    const siblings = state.visual.nodes.filter(item => item.movable && item.parent === node.parent && item.listSlot === node.listSlot);
    const up = button.dataset.move === 'up';
    const sibling = siblings[siblings.indexOf(node) + (up ? -1 : 1)];
    if (sibling) await visualEdit({ id: node.id, operation: 'move', target: sibling.id, side: up ? 'before' : 'after' });
  }
  if (button.dataset.widgetAction === 'code') {
    await selectFile(state.visual.path, 'code');
    editor.goTo(node.line, Number(node.id));
  }
  if (button.dataset.widgetAction === 'wrap') dialogs.wrap(node);
  if (button.dataset.widgetAction === 'remove') await visualEdit({ id: node.id, operation: 'remove' });
  if (button.dataset.widgetAction === 'add') await visualEdit({
    id: node.id, operation: 'add', slot: byId('insert-slot').value, widget: byId('insert-widget').value,
  });
}));

document.addEventListener('keydown', guarded(event => {
  if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
  if (event.key.toLowerCase() === 's' && state.mode !== 'design') {
    event.preventDefault();
    return saveDraft();
  }
  if (event.key.toLowerCase() === 'p' && state.project) {
    event.preventDefault();
    return dialogs.quickOpen();
  }
}));

try {
  await refreshState();
  const previous = localStorage.getItem('freeflow-project');
  const project = state.projects.find(item => item.id === previous) || state.projects[0];
  if (project) await activateProject(project);
} catch (error) { report(error); }
setInterval(poll, 850);
