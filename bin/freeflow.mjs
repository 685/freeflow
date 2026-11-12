#!/usr/bin/env node
import { spawn } from 'node:child_process';
import { parseArgs } from 'node:util';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { dataHome, readJson } from '../src/files.mjs';
import { serveMcp } from '../src/mcp.mjs';
import { startWorkbench } from '../src/server.mjs';

const bin = fileURLToPath(import.meta.url);
const { version } = await readJson(new URL('../package.json', import.meta.url));
const major = Number(process.versions.node.split('.')[0]);
const minor = Number(process.versions.node.split('.')[1]);
if (major < 22 || (major === 22 && minor < 15)) {
  process.stderr.write('Freeflow requires Node.js 22.15 or newer.\n');
  process.exit(1);
}

function openBrowser(url) {
  const windows = process.platform === 'win32';
  const program = windows ? 'rundll32.exe' : process.platform === 'darwin' ? 'open' : 'xdg-open';
  const args = windows ? ['url.dll,FileProtocolHandler', url] : [url];
  const browser = spawn(program, args, { detached: true, stdio: 'ignore', windowsHide: true });
  browser.on('error', () => process.stderr.write('Open ' + url + ' in your browser.\n'));
  browser.unref();
}

async function running(home) {
  const session = await readJson(path.join(home, 'session.json'), null).catch(() => null);
  if (!session || !/^http:\/\/127\.0\.0\.1:\d{1,5}$/.test(session.origin) || !/^[a-f0-9]{64}$/.test(session.token)) return null;
  const response = await fetch(session.origin + '/api/state', {
    headers: { 'X-Freeflow-Token': session.token }, signal: AbortSignal.timeout(800), redirect: 'error',
  }).catch(() => null);
  if (!response?.ok) return null;
  await response.body?.cancel();
  return session.origin;
}

async function main() {
  const { values, positionals } = parseArgs({
    allowPositionals: true,
    options: {
      port: { type: 'string', default: process.env.FREEFLOW_PORT || '4747' },
      home: { type: 'string' }, project: { type: 'string' },
      'no-open': { type: 'boolean' }, help: { type: 'boolean', short: 'h' },
    },
  });
  if (values.help) {
    process.stdout.write('Freeflow ' + version + '\n\n' +
      '  freeflow [--no-open] [--port 4747] [--home folder]\n' +
      '  freeflow mcp [--project id] [--home folder]\n');
    return;
  }
  const home = values.home ? path.resolve(values.home) : dataHome();
  if (positionals[0] === 'mcp' && positionals.length === 1) {
    if (values.project && !/^[0-9a-f]{8}$/.test(values.project)) throw new Error('Use a valid Freeflow project ID.');
    serveMcp(home, values.project, version);
    return;
  }
  if (positionals.length) throw new Error('Unknown Freeflow command. Use --help.');
  if (!/^\d{1,5}$/.test(values.port) || Number(values.port) > 65535) throw new Error('Enter a port from 0 to 65535.');
  const existing = await running(home);
  if (existing) {
    process.stdout.write('Freeflow is already running at ' + existing + '\n');
    if (!values['no-open']) openBrowser(existing);
    return;
  }
  const options = { home, port: Number(values.port), appVersion: version, bin };
  let workbench;
  try {
    workbench = await startWorkbench(options);
  } catch (error) {
    if (error.code !== 'EADDRINUSE' || process.argv.includes('--port') ||
        process.argv.some(arg => arg.startsWith('--port=')) || process.env.FREEFLOW_PORT) throw error;
    workbench = await startWorkbench({ ...options, port: 0 });
  }
  process.stdout.write('Freeflow ' + version + ' is running at ' + workbench.origin + '\n');
  if (!values['no-open']) openBrowser(workbench.origin);
  let closing = false;
  const close = async () => {
    if (closing) return;
    closing = true;
    await workbench.close();
  };
  process.on('SIGINT', close);
  process.on('SIGTERM', close);
}

main().catch(error => {
  process.stderr.write('Freeflow: ' + error.message + '\n');
  process.exitCode = 1;
});
