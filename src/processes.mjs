import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import path from 'node:path';

export function executable(name) {
  if (path.isAbsolute(name)) return existsSync(name) ? name : null;
  const extensions = process.platform === 'win32' && !path.extname(name) ? ['.exe', '.cmd', '.bat'] : [''];
  for (const item of (process.env.PATH || process.env.Path || '').split(path.delimiter)) {
    if (!item) continue;
    const folder = item.replace(/^"|"$/g, '');
    if (!path.isAbsolute(folder)) continue;
    for (const extension of extensions) {
      const candidate = path.join(folder, name + extension);
      if (existsSync(candidate)) return candidate;
    }
  }
  return null;
}

function windowsBatch(file, args, env) {
  const values = [file, ...args];
  if (values.some(value => /["%!\r\n\x00]/.test(value))) {
    throw new Error('The Windows Flutter batch launcher cannot use paths containing %, !, or quotes.');
  }
  const parameters = values.map((value, index) => {
    const name = 'FREEFLOW_CMD_ARG_' + index;
    env[name] = value;
    return '"%' + name + '%"';
  });
  return {
    file: process.env.ComSpec || 'C:\\Windows\\System32\\cmd.exe',
    args: ['/d', '/s', '/v:off', '/c', '"' + parameters.join(' ') + '"'],
    windowsVerbatimArguments: true,
  };
}

export function launch(file, args, { cwd, env = {}, onLine = () => {}, input } = {}) {
  const childEnv = { ...process.env, ...env };
  const command = process.platform === 'win32' && /\.(cmd|bat)$/i.test(file)
    ? windowsBatch(file, args, childEnv) : { file, args };
  const child = spawn(command.file, command.args, {
    cwd, env: childEnv, stdio: ['pipe', 'pipe', 'pipe'],
    windowsHide: true, windowsVerbatimArguments: command.windowsVerbatimArguments,
    detached: process.platform !== 'win32',
  });
  let settled = false;
  const collect = stream => {
    let pending = '';
    stream.setEncoding('utf8');
    stream.on('data', chunk => {
      pending += chunk;
      const lines = pending.split(/\r\n|\r|\n/);
      pending = lines.pop();
      for (const line of lines) if (line) onLine(line);
      if (pending.length > 50000) {
        onLine(pending.slice(0, 50000));
        pending = '';
      }
    });
    stream.on('end', () => {
      if (pending) onLine(pending);
    });
  };
  collect(child.stdout);
  collect(child.stderr);
  child.stdin.on('error', () => {});
  if (input !== undefined) child.stdin.end(input);
  const done = new Promise((resolve, reject) => {
    child.once('error', error => {
      settled = true;
      reject(error);
    });
    child.once('close', (code, signal) => {
      settled = true;
      resolve({ code, signal });
    });
  });
  const stop = () => {
    if (settled || !child.pid) return;
    if (process.platform === 'win32') {
      const killer = spawn('taskkill.exe', ['/pid', String(child.pid), '/t', '/f'], { windowsHide: true, stdio: 'ignore' });
      killer.on('error', () => child.kill());
    } else {
      try {
        process.kill(-child.pid, 'SIGTERM');
      } catch (error) {
        if (error.code !== 'ESRCH') child.kill();
      }
    }
  };
  return { child, done, stop };
}

export async function capture(file, args, options = {}) {
  const lines = [];
  const processHandle = launch(file, args, {
    ...options, onLine: line => {
      lines.push(line);
      options.onLine?.(line);
    },
  });
  const timeout = setTimeout(processHandle.stop, options.timeout ?? 15000);
  timeout.unref();
  try {
    const result = await processHandle.done;
    return { ...result, output: lines.join('\n') };
  } finally {
    clearTimeout(timeout);
  }
}
