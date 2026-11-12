import { createInterface } from 'node:readline';
import path from 'node:path';
import { Problem, readJson } from './files.mjs';

const string = description => ({ type: 'string', description });
const project = string('Project ID from list_projects. Omit when the MCP connection is scoped to one project.');
const relative = string('Project-relative path with forward slashes, e.g. lib/main.dart.');
const version = { type: ['string', 'null'], description: 'Revision returned by read_file. Use null only for a new file.' };
const entry = (name, description, properties = {}, required = [], readOnlyHint = false) => ({
  name, description, inputSchema: { type: 'object', properties, required, additionalProperties: false },
  annotations: { readOnlyHint, openWorldHint: ['start_preview', 'build', 'analyze', 'test'].includes(name) },
});

export const toolDefinitions = [
  entry('list_projects', 'List Flutter projects in the running local Freeflow workbench.', {}, [], true),
  entry('list_files', 'List visible Flutter project files. Build caches, symlinks and common credentials are skipped.', { projectId: project }, [], true),
  entry('read_file', 'Read UTF-8 source and obtain a revision for conflict-safe writes.', { projectId: project, path: relative }, ['path'], true),
  entry('write_file', 'Create or replace one project file. Read it first and pass its revision to avoid overwriting newer edits.', {
    projectId: project, path: relative, revision: version, content: string('The complete UTF-8 file content.'),
  }, ['path', 'revision', 'content']),
  entry('inspect_widgets', 'Read the supported visual widget tree and literal properties from a Dart file.', {
    projectId: project, path: relative,
  }, ['path'], true),
  entry('edit_widget', 'Edit the Dart source through a visual operation. Use IDs and a revision from inspect_widgets.', {
    projectId: project, path: relative, revision: version,
    command: {
      type: 'object', required: ['id', 'operation'], additionalProperties: false,
      properties: {
        id: string('Widget ID from inspect_widgets.'),
        operation: { enum: ['property', 'add', 'wrap', 'remove', 'move'] },
        name: string('Property name for a property edit.'), value: { type: ['string', 'number', 'boolean'] },
        slot: string('Available Dart child slot for add.'), widget: string('Palette widget for add or wrapper type for wrap.'),
        target: string('Sibling widget ID for move.'), side: { enum: ['before', 'after'] },
      },
    },
  }, ['path', 'revision', 'command']),
  entry('start_preview', 'Start the real local Flutter web preview and return its job. Check preview_status for the preview URL.', { projectId: project }),
  entry('preview_status', 'Get the latest local preview status, URL and Flutter logs.', { projectId: project }, [], true),
  entry('reload_preview', 'Refresh the running Flutter preview after source edits.', { projectId: project }),
  entry('analyze', 'Run Flutter static analysis. Check job_status when the job finishes.', { projectId: project }),
  entry('test', 'Run Flutter tests. Check job_status when the job finishes.', { projectId: project }),
  entry('build', 'Export a local Flutter build artifact. An iOS IPA requires macOS, Xcode and signing.', {
    projectId: project, target: { enum: ['web', 'android', 'apk', 'ios', 'source'] },
    baseHref: string('Optional web base path such as / or /my-app/.'), flavor: string('Optional Android or iOS Flutter flavor.'),
  }, ['target']),
  entry('job_status', 'Read status, output and compiler logs for a Freeflow command.', {
    projectId: project, jobId: string('Job ID returned by a tool.'), after: { type: 'integer', minimum: 0 },
  }, ['jobId'], true),
  entry('list_exports', 'List build artifacts available in the Freeflow Export dialog.', { projectId: project }, [], true),
];

export async function callTool(work, name, args, scope) {
  const { projects, jobs, readVisual, writeVisual } = work;
  if (!toolDefinitions.some(tool => tool.name === name)) throw new Problem(400, 'Unknown Freeflow tool.');
  if (!args || typeof args !== 'object' || Array.isArray(args)) throw new Problem(400, 'Tool arguments must be an object.');
  if (scope && args.projectId && args.projectId !== scope) throw new Problem(403, 'This MCP connection is scoped to a different project.');
  if (name === 'list_projects') {
    const records = scope ? [await projects.get(scope)] : await projects.list();
    return { projects: records.map(({ id, name, root, template }) => ({ id, name, root, template })) };
  }
  const id = scope || args.projectId;
  if (!id) throw new Problem(400, 'Choose a project from list_projects and pass its projectId.');
  const target = await projects.get(id);
  switch (name) {
    case 'list_files': return projects.tree(target);
    case 'read_file': return projects.file(target, args.path);
    case 'write_file': return projects.save(target, args.path, args.content, args.revision);
    case 'inspect_widgets': return readVisual(target, args.path);
    case 'edit_widget': return writeVisual(target, args);
    case 'start_preview': return jobs.startPreview(target);
    case 'preview_status': return { preview: jobs.preview(target) };
    case 'reload_preview': return jobs.reloadPreview(target);
    case 'analyze': return jobs.analyze(target);
    case 'test': return jobs.analyze(target, 'test');
    case 'build': return jobs.build(target, args.target, args);
    case 'list_exports': return { exports: await jobs.artifacts(target) };
    case 'job_status': {
      const job = jobs.get(args.jobId, args.after);
      if (job.projectId !== target.id) throw new Problem(404, 'Command not found in this project.');
      return job;
    }
    default: throw new Problem(400, 'Unknown Freeflow tool.');
  }
}

const protocols = new Set(['2024-11-05', '2025-03-26', '2025-06-18', '2025-11-25']);
const response = (id, result) => ({ jsonrpc: '2.0', id, result });
const failure = (id, code, message) => ({ jsonrpc: '2.0', id: id ?? null, error: { code, message } });

async function toolRequest(home, scope, params) {
  const session = await readJson(path.join(home, 'session.json'), null);
  if (!session || !/^http:\/\/127\.0\.0\.1:\d{1,5}$/.test(session.origin) || !/^[a-f0-9]{64}$/.test(session.token)) {
    throw new Error('Start Freeflow first. Use npm start -- --no-open to run it without opening a browser.');
  }
  const res = await fetch(session.origin + '/api/mcp', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Freeflow-Token': session.token },
    body: JSON.stringify({ name: params?.name, arguments: params?.arguments ?? {}, scope }),
    signal: AbortSignal.timeout(30000),
    redirect: 'error',
  }).catch(() => { throw new Error('The Freeflow workbench is not reachable. Start it before calling MCP tools.'); });
  const data = await res.json();
  if (!res.ok) throw new Error(data.error || 'Freeflow returned HTTP ' + res.status);
  return data;
}

export function serveMcp(home, scope, appVersion, input = process.stdin, output = process.stdout) {
  const lines = createInterface({ input, crlfDelay: Infinity, terminal: false });
  async function message(value) {
    if (!value || value.jsonrpc !== '2.0' || typeof value.method !== 'string') return failure(value?.id, -32600, 'Invalid JSON-RPC request.');
    if (!Object.hasOwn(value, 'id')) return undefined;
    const { id, method, params } = value;
    if (method === 'initialize') return response(id, {
      protocolVersion: protocols.has(params?.protocolVersion) ? params.protocolVersion : '2025-06-18',
      capabilities: { tools: { listChanged: false } },
      serverInfo: { name: 'freeflow', version: appVersion },
      instructions: 'Use list_projects, then read files with their revisions before making edits. The local Freeflow workbench runs Flutter and shows the preview.',
    });
    if (method === 'ping') return response(id, {});
    if (method === 'tools/list') return response(id, { tools: toolDefinitions });
    if (method === 'tools/call') {
      if (!toolDefinitions.some(tool => tool.name === params?.name)) return failure(id, -32602, 'Unknown Freeflow tool.');
      try {
        const result = await toolRequest(home, scope, params);
        return response(id, { content: [{ type: 'text', text: JSON.stringify(result, null, 2) }], structuredContent: result });
      } catch (error) {
        return response(id, { isError: true, content: [{ type: 'text', text: error.message }] });
      }
    }
    return failure(id, -32601, 'Method not found.');
  }
  lines.on('line', async line => {
    if (!line.trim()) return;
    let value;
    try { value = JSON.parse(line); } catch {
      output.write(JSON.stringify(failure(null, -32700, 'Malformed JSON.')) + '\n');
      return;
    }
    const result = Array.isArray(value) ? (await Promise.all(value.map(message))).filter(Boolean) : await message(value);
    if (result && (!Array.isArray(result) || result.length)) output.write(JSON.stringify(result) + '\n');
  });
  return lines;
}
