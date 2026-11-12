import assert from 'node:assert/strict';
import { EventEmitter, once } from 'node:events';
import * as fs from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { PassThrough, Readable } from 'node:stream';
import { test } from 'node:test';
import { zipFolder } from '../src/archive.mjs';
import { generateBackends, validateBackend } from '../src/backends.mjs';
import { serveMcp } from '../src/mcp.mjs';
import { capture } from '../src/processes.mjs';
import { Projects } from '../src/projects.mjs';
import { startWorkbench } from '../src/server.mjs';
import { starter } from '../src/templates.mjs';
import { editVisual, outline } from '../src/visual.mjs';

async function fixture(t) {
  const parent = await fs.realpath(tmpdir());
  const home = await fs.mkdtemp(path.join(parent, 'freeflow-test-'));
  t.after(async () => {
    if (path.dirname(home) === parent && path.basename(home).startsWith('freeflow-test-')) {
      await fs.rm(home, { recursive: true, force: true });
    }
  });
  const root = path.join(home, 'app');
  await fs.mkdir(path.join(root, 'lib'), { recursive: true });
  await fs.writeFile(path.join(root, 'pubspec.yaml'), 'name: example\n\ndependencies:\n  flutter:\n    sdk: flutter\n');
  await fs.writeFile(path.join(root, 'lib', 'main.dart'), starter('daymark'));
  await fs.writeFile(path.join(root, '.env'), 'PRIVATE_VALUE=excluded');
  return { home, root };
}

test('visual edits preserve expressions and change the intended Dart constructor', () => {
  const before = starter('daymark');
  const title = outline(before).nodes.find(node => node.label === 'Make today\ncount.');
  assert.ok(title);
  const changed = editVisual(before, { id: title.id, operation: 'property', name: 'text', value: "Today is yours.\nIt's $10." });
  assert.equal(outline(changed).nodes.find(node => node.id === title.id).label, "Today is yours.\nIt's $10.");
  const interpolated = outline(before).nodes.find(node => node.properties.some(prop => prop.expression?.includes('_completed.length')));
  assert.throws(() => editVisual(before, { id: interpolated.id, operation: 'property', name: 'text', value: 'Other' }), /Dart expression/);

  const source = "Widget build(BuildContext context) => const Column(children: [Text('a'), Text('b')]);";
  const nodes = outline(source).nodes;
  const moved = editVisual(source, { id: nodes[1].id, operation: 'move', target: nodes[2].id, side: 'after' });
  assert.deepEqual(outline(moved).nodes.filter(node => node.label).map(node => node.label), ['b', 'a']);
  assert.match(editVisual(source, { id: nodes[0].id, operation: 'add', slot: 'children', widget: 'Divider' }), /Divider\(\)/);
});

test('project writes use revisions and source archives exclude credentials', async t => {
  const { home, root } = await fixture(t);
  const projects = new Projects(home);
  const project = await projects.importFolder(root);
  const first = await projects.file(project, 'lib/main.dart');
  const edit = await projects.save(project, first.path, first.content + '\n// Edit\n', first.revision);
  assert.notEqual(edit.revision, first.revision);
  await assert.rejects(projects.save(project, first.path, 'stale', first.revision), { status: 409 });
  await assert.rejects(projects.file(project, '../main.dart'), { status: 400 });
  await assert.rejects(projects.file(project, '.env'), { status: 400 });
  const [history] = await projects.history(project, first.path);
  assert.equal((await projects.historyVersion(project, first.path, history.id)).content, first.content);
  const output = path.join(home, 'app.zip');
  await zipFolder(root, output, { source: true });
  const bytes = await fs.readFile(output);
  assert.equal(bytes.readUInt32LE(0), 0x04034b50);
  assert.equal(bytes.readUInt32LE(bytes.length - 22), 0x06054b50);
  assert.ok(bytes.includes(Buffer.from('lib/main.dart')));
  assert.ok(!bytes.includes(Buffer.from('PRIVATE_VALUE')));
});

test('backends generate Dart initialization and reject privileged Supabase keys', () => {
  const config = validateBackend('supabase', { url: 'https://demo.supabase.co', key: 'sb_publishable_example' });
  assert.equal(config.url, 'https://demo.supabase.co');
  assert.throws(() => validateBackend('supabase', { url: config.url, key: 'sb_secret_example' }), { status: 400 });
  const source = generateBackends({ firebase: true, supabase: true });
  assert.match(source, /Firebase\.initializeApp/);
  assert.match(source, /Supabase\.initialize/);
  assert.match(source, /FIREBASE_IOS_APP_ID/);
  assert.ok(!source.includes(config.key));
});

test('local MCP handshake advertises project tools', async () => {
  const input = new PassThrough();
  const output = new PassThrough();
  const lines = serveMcp('unused', undefined, '0.1.0', input, output);
  const initialized = once(output, 'data');
  input.write('{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}\n');
  const [first] = await initialized;
  assert.equal(JSON.parse(first).result.serverInfo.name, 'freeflow');
  const listed = once(output, 'data');
  input.write('{"jsonrpc":"2.0","id":2,"method":"tools/list"}\n');
  const [second] = await listed;
  assert.ok(JSON.parse(second).result.tools.some(tool => tool.name === 'write_file'));
  lines.close();
  input.end();
  output.end();
});

function request(workbench, url, options = {}) {
  const input = options.data === undefined ? [] : [Buffer.from(JSON.stringify(options.data))];
  const req = Readable.from(input);
  req.method = options.data === undefined ? 'GET' : 'POST';
  req.url = url;
  req.headers = {
    host: new URL(workbench.origin).host,
    'x-freeflow-token': workbench.token,
    'content-type': 'application/json',
    ...options.headers,
  };
  return new Promise(resolve => {
    const res = new EventEmitter();
    let status;
    res.setHeader = () => {};
    res.writeHead = value => { status = value; res.headersSent = true; return res; };
    res.end = value => { resolve({ status, body: JSON.parse(value) }); return res; };
    res.destroy = () => resolve({ status });
    workbench.server.emit('request', req, res);
  });
}

test('workbench routes enforce a local token and MCP project scope', async t => {
  const { home, root } = await fixture(t);
  await fs.writeFile(path.join(home, 'settings.json'), '{"offline":true}');
  const workbench = await startWorkbench({ home, port: 0, appVersion: 'test', bin: 'freeflow.mjs' });
  try {
    assert.equal((await request(workbench, '/api/state', { headers: { 'x-freeflow-token': '' } })).status, 401);
    assert.equal((await request(workbench, '/api/state', { headers: { origin: 'https://example.invalid' } })).status, 403);
    const imported = await request(workbench, '/api/projects/import', { data: { folder: root } });
    assert.equal(imported.status, 201);
    const input = { name: 'read_file', scope: imported.body.id, arguments: { path: 'lib/main.dart' } };
    const file = await request(workbench, '/api/mcp', { data: input });
    assert.equal(file.status, 200);
    assert.match(file.body.content, /Daymark/);
    const wrong = await request(workbench, '/api/mcp', { data: { ...input, arguments: { ...input.arguments, projectId: '01234567' } } });
    assert.equal(wrong.status, 403);
  } finally {
    await workbench.close();
  }
});

test('Windows batch launcher handles a spaced path and argument', { skip: process.platform !== 'win32' }, async t => {
  const { home } = await fixture(t);
  const folder = path.join(home, 'folder & tools');
  await fs.mkdir(folder);
  const program = path.join(folder, 'check.cmd');
  await fs.writeFile(program, '@echo off\r\necho %~1\r\n');
  const result = await capture(program, ['an argument'], { input: '' });
  assert.equal(result.code, 0);
  assert.equal(result.output.trim(), 'an argument');
});
