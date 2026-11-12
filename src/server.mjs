import { randomBytes, timingSafeEqual } from 'node:crypto';
import { createReadStream } from 'node:fs';
import * as fs from 'node:fs/promises';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Backends, startupCode } from './backends.mjs';
import { Problem, imageType, projectFile, readJson, writeJson } from './files.mjs';
import { Flutter } from './flutter.mjs';
import { Jobs } from './jobs.mjs';
import { callTool } from './mcp.mjs';
import { Projects, Serial } from './projects.mjs';
import { signingStatus } from './signing.mjs';
import { templates } from './templates.mjs';
import { editVisual, outline } from './visual.mjs';

const webDir = fileURLToPath(new URL('../ui/', import.meta.url));
const webFiles = new Set(['styles.css', 'app.mjs', 'icons.mjs', 'editor.mjs', 'dialogs.mjs']);
const mime = {
  '.css': 'text/css; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8', '.html': 'text/html; charset=utf-8',
  '.avif': 'image/avif', '.gif': 'image/gif', '.ico': 'image/x-icon', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg',
  '.png': 'image/png', '.webp': 'image/webp', '.zip': 'application/zip',
};

function standardHeaders(res) {
  res.setHeader('X-Content-Type-Options', 'nosniff');
  res.setHeader('X-Frame-Options', 'DENY');
  res.setHeader('Referrer-Policy', 'no-referrer');
  res.setHeader('Cache-Control', 'no-store');
  res.setHeader('Content-Security-Policy', "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' blob: data:; frame-src http://127.0.0.1:*; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'");
}

function json(res, data, status = 200) {
  res.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8' });
  res.end(JSON.stringify(data));
}

async function body(req) {
  if (req.headers['content-type']?.split(';')[0] !== 'application/json') throw new Problem(415, 'Use application/json.');
  const limit = 15 * 1024 * 1024;
  if (Number(req.headers['content-length']) > limit) throw new Problem(413, 'This request is too large.');
  const chunks = [];
  let bytes = 0;
  for await (const chunk of req) {
    bytes += chunk.length;
    if (bytes > limit) throw new Problem(413, 'This request is too large.');
    chunks.push(chunk);
  }
  let value;
  try { value = JSON.parse(Buffer.concat(chunks).toString('utf8')); } catch { throw new Problem(400, 'Send a JSON object.'); }
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Problem(400, 'Send a JSON object.');
  return value;
}

async function sendFile(res, file, name) {
  const stat = await fs.stat(file);
  if (!stat.isFile()) throw new Problem(404, 'File not found.');
  const headers = { 'Content-Type': mime[path.extname(file).toLowerCase()] || 'application/octet-stream', 'Content-Length': stat.size };
  if (name) headers['Content-Disposition'] = 'attachment; filename="' + name + '"';
  res.writeHead(200, headers);
  const stream = createReadStream(file);
  stream.on('error', () => res.destroy());
  res.once('close', () => stream.destroy());
  stream.pipe(res);
}

function authorized(value, token) {
  return typeof value === 'string' && /^[a-f0-9]{64}$/.test(value) &&
    timingSafeEqual(Buffer.from(value, 'hex'), Buffer.from(token, 'hex'));
}

function mcpConfiguration(home, project, bin) {
  const mcpName = project ? 'freeflow-' + project.id : 'freeflow';
  const args = [bin, 'mcp', '--home', home];
  if (project) args.push('--project', project.id);
  const shellQuote = process.platform === 'win32'
    ? value => "'" + value.replaceAll("'", "''") + "'"
    : value => "'" + value.replaceAll("'", "'\\''") + "'";
  const command = [shellQuote(process.execPath), ...args.map(shellQuote)].join(' ');
  return {
    codex: 'codex mcp add ' + mcpName + ' -- ' + command,
    claude: 'claude mcp add --transport stdio --scope user ' + mcpName + ' -- ' + command,
    desktop: JSON.stringify({ mcpServers: { [mcpName]: { command: process.execPath, args } } }, null, 2),
    prompt: 'Use the Freeflow MCP tools to inspect the Flutter project' +
      (project ? ' "' + project.name + '" (' + project.id + ')' : '') +
      '. Read source revisions before edits, run analysis, and refresh the local preview after changes.',
  };
}

export async function startWorkbench({ home, port, appVersion, bin }) {
  await fs.mkdir(home, { recursive: true, mode: 0o700 });
  const projects = new Projects(home);
  const flutter = new Flutter(home);
  const backends = new Backends(home, projects);
  const jobs = new Jobs(home, projects, flutter, backends);
  const settingsSerial = new Serial();
  const downloads = new Map();
  const token = randomBytes(32).toString('hex');
  const sessionPath = path.join(home, 'session.json');
  const html = (await fs.readFile(path.join(webDir, 'index.html'), 'utf8')).replace('FREEFLOW_SESSION_TOKEN', token);
  let origin;

  async function readVisual(project, relative) {
    if (typeof relative !== 'string' || !relative.endsWith('.dart')) throw new Problem(400, 'Select a Dart file.');
    const source = await projects.file(project, relative);
    return { path: source.path, revision: source.revision, ...outline(source.content) };
  }

  function writeVisual(project, input) {
    const { path: relative, revision, command } = input;
    if (typeof relative !== 'string' || !relative.endsWith('.dart')) throw new Problem(400, 'Select a Dart file.');
    return projects.update(project, relative, revision, prior => {
      if (!prior) throw new Problem(404, 'Dart file not found.');
      return editVisual(prior.content, command);
    });
  }

  async function projectRoute(req, res, url, target, part) {
    const method = req.method;
    const file = url.searchParams.get('path');
    if (method === 'GET') {
      switch (part) {
        case 'tree': return json(res, await projects.tree(target));
        case 'file': return json(res, await projects.file(target, file));
        case 'plan': return json(res, await projects.plan(target));
        case 'widgets': return json(res, await readVisual(target, file));
        case 'history': return json(res, await projects.history(target, file));
        case 'history-version': return json(res, await projects.historyVersion(target, file, url.searchParams.get('id')));
        case 'preview': return json(res, jobs.preview(target));
        case 'jobs': return json(res, jobs.list(target.id));
        case 'artifacts': return json(res, await jobs.artifacts(target));
        case 'backends': return json(res, { ...await backends.state(target), startupCode });
        case 'signing': return json(res, await signingStatus(target, projects));
        case 'asset': {
          if (!imageType.test(file || '')) throw new Problem(415, 'Select a supported image file.');
          const targetPath = await projectFile(target.root, file);
          if ((await fs.stat(targetPath)).size > 20 * 1024 * 1024) throw new Problem(413, 'Asset previews are limited to 20 MiB.');
          return sendFile(res, targetPath);
        }
      }
    }
    if (method === 'PUT' && part === 'file') {
      const input = await body(req);
      return json(res, await projects.save(target, input.path, input.content, input.revision));
    }
    if (method === 'POST') {
      const input = await body(req);
      switch (part) {
        case 'widgets': return json(res, await writeVisual(target, input));
        case 'preview': {
          if (input.action === 'start') return json(res, jobs.startPreview(target), 202);
          if (input.action === 'reload') return json(res, jobs.reloadPreview(target), 202);
          if (input.action === 'stop') return json(res, await jobs.stopPreview(target));
          throw new Problem(400, 'Choose a preview action.');
        }
        case 'command': {
          if (['analyze', 'test'].includes(input.action)) return json(res, jobs.analyze(target, input.action), 202);
          if (input.action === 'format') return json(res, await jobs.format(target, input.path, input.revision), 202);
          throw new Problem(400, 'Choose a project command.');
        }
        case 'build': return json(res, jobs.build(target, input.target, input), 202);
        case 'backends': return json(res, jobs.integrate(target, input.provider, input.config), 202);
        case 'signing': return json(res, jobs.signAndroid(target, input), 202);
        case 'asset': return json(res, await projects.asset(target, input.name, input.data), 201);
        case 'forget': {
          await jobs.stopPreview(target);
          if (jobs.busy(target.id)) throw new Problem(409, 'Wait for the project command to finish.');
          await projects.forget(target.id);
          return json(res, { removed: true });
        }
        case 'download': {
          const artifact = await jobs.artifact(target, input.id);
          const key = randomBytes(24).toString('hex');
          const now = Date.now();
          for (const [id, download] of downloads) if (download.expires <= now) downloads.delete(id);
          if (downloads.size >= 100) downloads.delete(downloads.keys().next().value);
          downloads.set(key, { ...artifact, expires: now + 2 * 60 * 1000 });
          return json(res, { url: '/download/' + key });
        }
      }
    }
    throw new Problem(404, 'Freeflow route not found.');
  }

  const server = http.createServer(async (req, res) => {
    standardHeaders(res);
    try {
      const address = new URL(origin);
      if (req.headers.host !== address.host || (req.headers.origin && req.headers.origin !== origin) ||
          req.headers['sec-fetch-site'] === 'cross-site' || !req.url.startsWith('/') || req.url.startsWith('//')) {
        throw new Problem(403, 'Use the local Freeflow address.');
      }
      const url = new URL(req.url, origin);
      const route = url.pathname;
      const method = req.method;
      if (route === '/' && method === 'GET') {
        res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
        return res.end(html);
      }
      if (method === 'GET' && webFiles.has(route.slice(1))) return sendFile(res, path.join(webDir, route.slice(1)));
      if (method === 'GET' && route.startsWith('/download/')) {
        const key = route.slice(10);
        const download = downloads.get(key);
        downloads.delete(key);
        if (!download || download.expires <= Date.now()) throw new Problem(404, 'This download link expired. Open Export to download it again.');
        return sendFile(res, download.file, download.name);
      }
      if (!route.startsWith('/api/')) throw new Problem(404, 'Page not found.');
      if (!authorized(req.headers['x-freeflow-token'], token)) throw new Problem(401, 'Reload the workbench to reconnect.');
      if (method === 'GET' && route === '/api/state') return json(res, {
        appVersion, projects: await projects.list(), templates, flutter: await flutter.status(),
      });
      if (method === 'POST' && route === '/api/projects/new') return json(res, await jobs.scaffold(await body(req)), 202);
      if (method === 'POST' && route === '/api/projects/import') return json(res, await projects.importFolder((await body(req)).folder), 201);
      if (method === 'POST' && route === '/api/mcp') {
        const input = await body(req);
        return json(res, await callTool({ projects, jobs, readVisual, writeVisual }, input.name, input.arguments, input.scope));
      }
      if (method === 'GET' && route === '/api/ai') {
        const id = url.searchParams.get('project');
        return json(res, mcpConfiguration(home, id ? await projects.get(id) : null, bin));
      }
      if (route === '/api/settings' && method === 'POST') {
        const input = await body(req);
        const result = await settingsSerial.run('settings', () => flutter.saveSettings(input));
        void jobs.autoUpdate().catch(() => {});
        return json(res, result);
      }
      if (route === '/api/sdk/check' && method === 'POST') {
        await flutter.latest({ refresh: true });
        return json(res, await flutter.status());
      }
      if (route === '/api/sdk/install' && method === 'POST') return json(res, jobs.sdkInstall(), 202);
      if (route === '/api/sdk/job' && method === 'GET') return json(res, jobs.sdkStatus());
      if (route === '/api/sdk/doctor' && method === 'POST') return json(res, await flutter.diagnostics());
      const job = route.match(/^\/api\/jobs\/([0-9a-f-]+)(\/stop)?$/);
      if (job) {
        if (method === 'GET' && !job[2]) return json(res, jobs.get(job[1], Number(url.searchParams.get('after')) || 0));
        if (method === 'POST' && job[2]) return json(res, jobs.cancel(job[1]));
      }
      const match = route.match(/^\/api\/projects\/([0-9a-f]{8})\/([a-z-]+)$/);
      if (match) return await projectRoute(req, res, url, await projects.get(match[1]), match[2]);
      throw new Problem(404, 'Freeflow route not found.');
    } catch (error) {
      if (res.headersSent) return res.destroy();
      const status = error.status || 500;
      if (status >= 500 && !error.status) process.stderr.write('[Freeflow] ' + error.message + '\n');
      return json(res, { error: error.status ? error.message : 'Freeflow could not complete the request. ' + error.message }, status);
    }
  });
  server.requestTimeout = 60000;
  server.headersTimeout = 12000;
  const listening = new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', () => {
      origin = 'http://127.0.0.1:' + server.address().port;
      resolve();
    });
  });
  await listening;
  try {
    await writeJson(sessionPath, { origin, token, pid: process.pid });
  } catch (error) {
    server.close();
    throw error;
  }
  void jobs.autoUpdate().catch(() => {});
  const timer = setInterval(() => void jobs.autoUpdate().catch(() => {}), 6 * 60 * 60 * 1000);
  timer.unref();
  return {
    origin, token, jobs, server,
    async close() {
      clearInterval(timer);
      await jobs.close();
      await new Promise(resolve => server.close(resolve));
      const current = await readJson(sessionPath, null);
      if (current?.token === token) await fs.rm(sessionPath, { force: true });
    },
  };
}
