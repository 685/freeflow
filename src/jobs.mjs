import { randomUUID } from 'node:crypto';
import * as fs from 'node:fs/promises';
import { createServer } from 'node:net';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { zipFolder } from './archive.mjs';
import { Problem, exists, inside, readJson, visibleName, walk, writeJson } from './files.mjs';
import { servePreview } from './preview-server.mjs';
import { launch } from './processes.mjs';
import { configureSigning, signingStatus } from './signing.mjs';

const terminal = new Set(['succeeded', 'failed', 'stopped']);
const ansi = /\x1b\[[0-?]*[ -/]*[@-~]/g;
const targetInfo = {
  web: { file: 'web.zip', label: 'Web', kind: 'ZIP' },
  android: { file: 'android.aab', label: 'Google Play', kind: 'AAB' },
  apk: { file: 'android-debug.apk', label: 'Android device', kind: 'APK' },
  ios: { file: 'ios.ipa', label: 'App Store', kind: 'IPA' },
  source: { file: 'source.zip', label: 'Flutter source', kind: 'ZIP' },
};
const targetIds = Object.keys(targetInfo);

class Job {
  #abort = new AbortController();
  #lines = [];
  #sequence = 0;

  constructor(kind, projectId) {
    this.id = randomUUID().slice(0, 12);
    this.kind = kind;
    this.projectId = projectId;
    this.status = 'queued';
    this.startedAt = new Date().toISOString();
    this.result = {};
  }

  get signal() { return this.#abort.signal; }
  get finished() { return terminal.has(this.status); }

  log = value => {
    const message = String(value).replace(ansi, '').slice(0, 6000);
    if (message) this.#lines.push({ id: ++this.#sequence, message });
    if (this.#lines.length > 300) this.#lines.shift();
  };

  view(after = 0) {
    return {
      id: this.id, kind: this.kind, projectId: this.projectId, status: this.status,
      startedAt: this.startedAt, endedAt: this.endedAt, error: this.error, result: this.result,
      log: this.#lines.filter(entry => entry.id > after),
    };
  }

  stop() { this.#abort.abort(); }

  async using(handle) {
    const stop = () => handle.stop();
    this.signal.addEventListener('abort', stop, { once: true });
    if (this.signal.aborted) stop();
    try {
      return await handle.done;
    } finally {
      this.signal.removeEventListener('abort', stop);
    }
  }

  async exec(command, cwd) {
    this.signal.throwIfAborted();
    const handle = launch(command.file, command.args, { cwd, env: command.env, onLine: this.log, input: '' });
    const result = await this.using(handle);
    this.signal.throwIfAborted();
    if (result.code !== 0) throw new Error('The command exited with ' + (result.code ?? result.signal ?? 'an error') + '. See the console for details.');
  }
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(error => error ? reject(error) : resolve(port));
    });
  });
}

async function stamps(root) {
  async function stamp(relative) {
    const stat = await fs.stat(path.join(root, relative), { bigint: true }).catch(error => {
      if (error.code !== 'ENOENT' && error.code !== 'ENOTDIR') throw error;
    });
    return relative + ':' + (stat ? String(stat.mtimeNs) + ':' + String(stat.size) : '');
  }
  const collect = async directory => {
    const start = path.join(root, directory);
    if (!await exists(start) || (await fs.lstat(start)).isSymbolicLink()) return [];
    const { entries } = await walk(start, { include: visibleName, maxEntries: 4500 });
    return Promise.all(entries.map(entry => stamp(directory + '/' + entry.path)));
  };
  const [lib, web, assets, manifest, lock] = await Promise.all([
    collect('lib'), collect('web'), collect('assets'), stamp('pubspec.yaml'), stamp('pubspec.lock'),
  ]);
  return { code: lib.join('\0'), project: [...web, ...assets, manifest, lock].join('\0') };
}

function buildFlags(target, input) {
  if (target === 'web') {
    const base = input.baseHref || '/';
    if (typeof base !== 'string' || !/^\/(?:[A-Za-z0-9._~-]+\/)*$/.test(base) || /(?:^|\/)\.{1,2}(?:\/|$)/.test(base)) {
      throw new Problem(400, 'Web base path must look like / or /my-app/.');
    }
    return ['web', '--release', '--no-web-resources-cdn', '--base-href', base];
  }
  const flavor = input.flavor || '';
  if (typeof flavor !== 'string' || (flavor && !/^[A-Za-z][A-Za-z0-9_-]{0,39}$/.test(flavor))) {
    throw new Problem(400, 'Enter a valid Flutter flavor name.');
  }
  const flavorArgs = flavor ? ['--flavor', flavor] : [];
  if (target === 'android') return ['appbundle', '--release', ...flavorArgs];
  if (target === 'apk') return ['apk', '--debug', ...flavorArgs];
  return ['ipa', '--release', '--export-method', 'app-store', ...flavorArgs];
}

async function findOutput(root, extension) {
  if (!await exists(root)) throw new Error('Flutter did not produce the requested ' + extension.toUpperCase() + ' output.');
  const { entries } = await walk(root, { maxEntries: 2500 });
  const matches = entries.filter(entry => entry.path.toLowerCase().endsWith('.' + extension));
  const files = await Promise.all(matches.map(async entry => {
    const file = path.join(root, entry.path);
    return { file, modified: (await fs.stat(file)).mtimeMs };
  }));
  files.sort((a, b) => b.modified - a.modified);
  if (!files.length) throw new Error('Flutter did not produce the requested ' + extension.toUpperCase() + ' output.');
  return files[0].file;
}

class Preview {
  #waiters = new Map();
  #requestId = 0;
  #appId;
  #handle;
  #loop;
  #last;
  #reload;
  #restarting = false;
  #closed = new AbortController();
  #announced = false;
  #startupError;
  #compiled;

  constructor(work, job, project) {
    this.work = work;
    this.job = job;
    this.project = project;
  }

  get signal() { return AbortSignal.any([this.job.signal, this.#closed.signal]); }

  async run() {
    const { work, job, project } = this;
    const sdk = await work.flutter.selected();
    await work.pub(job, project, ['pub', 'get'], sdk);
    this.#last = await stamps(project.root);
    job.result = { url: null, epoch: 0, reloading: false };
    if (!work.useCompiledPreview(project.id)) {
      try {
        await this.#runDev(sdk);
        return;
      } catch (error) {
        if (job.signal.aborted || job.status === 'ready') throw error;
        job.log(error.message);
        job.log('Trying the compiled Flutter preview.');
        work.useCompiledPreview(project.id, true);
        this.#closed = new AbortController();
        this.#loop = undefined;
      }
    }
    await this.#runCompiled(sdk);
  }

  async #runDev(sdk) {
    const { work, job, project } = this;
    const port = await freePort();
    const url = 'http://127.0.0.1:' + port + '/';
    job.log('Starting a Flutter web preview.');
    const args = [
      'run', '-d', 'web-server', '--web-hostname', '127.0.0.1',
      '--web-port', String(port), '--machine', '--no-pub', '--no-web-resources-cdn',
      ...await work.backends.options(project),
    ];
    job.signal.throwIfAborted();
    this.#handle = await work.flutter.start(args, { cwd: project.root, sdk, onLine: line => this.#receive(line) });
    const running = job.using(this.#handle);
    // The web-server device can be serving HTML before the browser attaches.
    // Probe the local port as well as consuming the Flutter daemon events.
    const ready = this.#waitForServer(url);
    try {
      const first = await Promise.race([
        ready.then(() => 'ready'),
        running.then(result => ({ result })),
      ]);
      if (first !== 'ready') throw new Error(this.#startupError || 'Flutter preview stopped during launch.');
      job.status = 'ready';
      job.result.url = url;
      job.log('Preview ready.');
      this.#loop = this.#watch();
      const result = await running;
      if (!job.signal.aborted) throw new Error('Flutter preview exited (' + (result.code ?? result.signal) + ').');
    } finally {
      this.#closed.abort();
      this.#handle.stop();
      for (const waiter of this.#waiters.values()) waiter.reject(new Error('Preview closed.'));
      this.#waiters.clear();
      await running.catch(() => {});
      await ready.catch(() => {});
      await this.#loop;
      job.result = { ...job.result, url: null, reloading: false };
    }
  }

  async #runCompiled(sdk) {
    const { job } = this;
    const parent = path.join(this.work.home, 'tmp');
    await fs.mkdir(parent, { recursive: true });
    const folder = await fs.mkdtemp(path.join(parent, 'preview-'));
    const base = await fs.realpath(parent);
    const real = await fs.realpath(folder);
    if (!inside(base, real) || real === base) throw new Error('Invalid preview build directory.');
    const build = { root: real, generation: 0, sdk, server: null };
    this.#compiled = build;
    try {
      await this.#compile();
      job.status = 'ready';
      job.result.url = build.server.url;
      job.log('Preview ready. Flutter recompiles when source files change.');
      await this.#watch();
    } finally {
      await this.#reload?.catch(() => {});
      await build.server?.close();
      this.#compiled = null;
      job.result = { ...job.result, url: null, reloading: false };
      await fs.rm(real, { recursive: true, force: true });
    }
  }

  async #compile() {
    const { work, job, project } = this;
    const build = this.#compiled;
    const generation = ++build.generation;
    const output = path.join(build.root, 'web-' + generation);
    try {
      const args = ['build', 'web', '--release', '--no-pub', '--no-web-resources-cdn', '--output', output,
        ...await work.backends.options(project)];
      await job.exec(await work.flutter.command(args, build.sdk), project.root);
      const bootstrap = path.join(output, 'flutter_bootstrap.js');
      // Preview generations share a local origin, so disable Flutter's worker
      // registration to keep old assets from masking a freshly compiled app.
      if (await exists(bootstrap)) {
        const source = await fs.readFile(bootstrap, 'utf8');
        const uncached = source.replace(/serviceWorkerSettings:\s*\{\s*serviceWorkerVersion:\s*["'][^"']+["']\s*,?\s*\}\s*,?/g, '');
        if (uncached !== source) await fs.writeFile(bootstrap, uncached);
      }
      if (build.server) await build.server.swap(output);
      else build.server = await servePreview(output);
      const previous = build.output;
      build.output = output;
      if (previous && inside(build.root, previous) && previous !== build.root) {
        await fs.rm(previous, { recursive: true, force: true });
      }
    } catch (error) {
      if (inside(build.root, output) && output !== build.root && output !== build.output) {
        await fs.rm(output, { recursive: true, force: true });
      }
      throw error;
    }
  }

  #receive(line) {
    let messages;
    if (line.startsWith('[')) {
      try { messages = JSON.parse(line); } catch {}
    }
    if (!Array.isArray(messages)) {
      this.job.log(line);
      return;
    }
    for (const message of messages) {
      const { event, params, result, id, error } = message;
      if (event === 'app.start' || event === 'app.started') this.#appId = params?.appId || this.#appId;
      if (event === 'app.webLaunchUrl' || event === 'app.started') this.#announced = true;
      if (event === 'app.stop' && params?.error) {
        this.#startupError = params.error;
        this.job.log(params.error);
      }
      if (event === 'app.log' || event === 'daemon.logMessage' || event === 'app.debugOutput') {
        this.job.log(params?.log || params?.message || '');
      }
      if (id !== undefined && this.#waiters.has(id)) {
        const waiter = this.#waiters.get(id);
        this.#waiters.delete(id);
        if (error) waiter.reject(new Error(error.message || String(error)));
        else waiter.resolve(result);
      }
    }
  }

  async #waitForServer(url) {
    const deadline = Date.now() + 90000;
    const signal = this.signal;
    while (Date.now() < deadline) {
      signal.throwIfAborted();
      if (this.#announced) return;
      const response = await fetch(url, { signal: AbortSignal.any([signal, AbortSignal.timeout(850)]), redirect: 'error' }).catch(() => null);
      if (response?.ok) {
        await response.body?.cancel();
        return;
      }
      await delay(450, undefined, { signal });
    }
    throw new Error('Flutter did not start a preview within 90 seconds. See the console.');
  }

  async #watch() {
    const signal = this.signal;
    while (!signal.aborted) {
      try {
        await delay(950, undefined, { signal });
        if (this.work.busy(this.project.id) || this.#restarting) continue;
        const next = await stamps(this.project.root);
        const projectChanged = next.project !== this.#last.project;
        const codeChanged = next.code !== this.#last.code;
        this.#last = next;
        if (projectChanged) {
          this.#restart('Project files changed.');
          return;
        }
        if (codeChanged) await this.reload();
      } catch (error) {
        if (!signal.aborted) this.job.log(error.message);
      }
    }
  }

  reload() {
    if (!this.#reload) this.#reload = this.#refresh().finally(() => { this.#reload = null; });
    return this.#reload;
  }

  async #refresh() {
    const job = this.job;
    if (job.status !== 'ready' || this.#restarting || this.signal.aborted) return;
    if (this.#compiled) {
      job.result = { ...job.result, reloading: true, compileError: null };
      try {
        await this.#compile();
        job.result.epoch++;
        job.log('Preview updated.');
      } catch (error) {
        if (!this.signal.aborted) {
          job.result.compileError = error.message;
          job.log(error.message);
        }
      } finally {
        job.result.reloading = false;
      }
      return;
    }
    if (!this.#appId) {
      this.#restart('Dart source changed.');
      return;
    }
    job.result = { ...job.result, reloading: true, compileError: null };
    job.log('Refreshing the preview.');
    const id = ++this.#requestId;
    try {
      const result = await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('Flutter restart timed out.')), 25000);
        this.#waiters.set(id, {
          resolve: value => { clearTimeout(timer); resolve(value); },
          reject: error => { clearTimeout(timer); reject(error); },
        });
        this.#handle.child.stdin.write(JSON.stringify([{
          id, method: 'app.restart', params: { appId: this.#appId, fullRestart: true, pause: false },
        }]) + '\n');
      });
      if (result?.code) {
        const message = result.message || 'Flutter could not compile this change.';
        if (/not supported|not available|no connected/i.test(message)) this.#restart(message);
        else {
          job.result.compileError = message;
          job.log(message);
        }
      } else {
        job.result.epoch++;
        job.log('Preview updated.');
      }
    } catch (error) {
      if (!this.signal.aborted) this.#restart(error.message);
    } finally {
      this.#waiters.delete(id);
      job.result.reloading = false;
    }
  }

  #restart(reason) {
    if (this.#restarting || this.signal.aborted) return;
    this.#restarting = true;
    this.job.log(reason + ' Restarting Flutter.');
    void this.work.restartPreview(this.project, this.job).catch(error => this.job.log(error.message));
  }
}

export class Jobs {
  #jobs = new Map();
  #previews = new Map();
  #sessions = new Map();
  #compiledProjects = new Set();
  #busy = new Map();
  #sdkJob;
  #closing = false;

  constructor(home, projects, flutter, backends) {
    this.home = home;
    this.projects = projects;
    this.flutter = flutter;
    this.backends = backends;
  }

  #start(kind, projectId, work, exclusive = false) {
    if (this.#closing) throw new Problem(503, 'Freeflow is shutting down.');
    if (exclusive && this.busy(projectId)) throw new Problem(409, 'This project already has a command running.');
    const job = new Job(kind, projectId);
    this.#jobs.set(job.id, job);
    if (exclusive) this.#busy.set(projectId, job);
    job.done = Promise.resolve().then(async () => {
      job.status = 'running';
      try {
        job.signal.throwIfAborted();
        const result = await work(job);
        if (result) job.result = result;
        job.status = job.signal.aborted ? 'stopped' : 'succeeded';
      } catch (error) {
        if (job.signal.aborted && (job.status === 'ready' || error.name === 'AbortError')) job.status = 'stopped';
        else {
          job.status = 'failed';
          job.error = error.message;
          job.log(error.message);
        }
      } finally {
        job.endedAt = new Date().toISOString();
        if (this.#busy.get(projectId) === job) this.#busy.delete(projectId);
        this.#prune();
      }
    });
    return job;
  }

  #prune() {
    if (this.#jobs.size < 120) return;
    for (const job of this.#jobs.values()) {
      if (job.finished && this.#previews.get(job.projectId) !== job && job !== this.#sdkJob) this.#jobs.delete(job.id);
      if (this.#jobs.size < 100) break;
    }
  }

  busy(id) { return this.#busy.get(id); }

  useCompiledPreview(id, value = false) {
    if (value) this.#compiledProjects.add(id);
    return this.#compiledProjects.has(id);
  }

  get(id, after) {
    const job = this.#jobs.get(id);
    if (!job) throw new Problem(404, 'Command not found.');
    return job.view(after);
  }

  list(projectId) {
    return [...this.#jobs.values()].filter(job => job.projectId === projectId).slice(-25).reverse().map(job => ({
      id: job.id, kind: job.kind, status: job.status, startedAt: job.startedAt, error: job.error,
    }));
  }

  cancel(id) {
    const job = this.#jobs.get(id);
    if (!job) throw new Problem(404, 'Command not found.');
    if (!job.finished) job.stop();
    return job.view();
  }

  async pub(job, project, args, sdk) {
    const settings = await this.flutter.settings();
    const command = await this.flutter.command(settings.offline ? [...args, '--offline'] : args, sdk);
    await job.exec(command, project.root);
  }

  async scaffold(input) {
    const project = await this.projects.reserve(input.name, input.template, input.organization);
    return this.#start('create', project.id, async job => {
      job.log('Creating ' + project.name + ' with Flutter.');
      const command = await this.flutter.command([
        'create', '--no-pub', '--platforms=android,ios,web', '--org', project.organization,
        '--project-name', project.slug, '.',
      ]);
      await job.exec(command, project.root);
      return { project: await this.projects.finishScaffold(project) };
    }, true).view();
  }

  startPreview(project) {
    const current = this.#previews.get(project.id);
    if (current && !current.finished && !current.signal.aborted) return current.view();
    if (this.busy(project.id)) throw new Problem(409, 'Wait for the project command to finish before starting preview.');
    let session;
    const job = this.#start('preview', project.id, async running => {
      session = new Preview(this, running, project);
      this.#sessions.set(project.id, session);
      try { await session.run(); } finally {
        if (this.#sessions.get(project.id) === session) this.#sessions.delete(project.id);
      }
    });
    this.#previews.set(project.id, job);
    return job.view();
  }

  preview(project) { return this.#previews.get(project.id)?.view() ?? null; }

  async stopPreview(project) {
    const job = this.#previews.get(project.id);
    if (job && !job.finished) {
      job.stop();
      await job.done;
    }
    return job?.view() ?? null;
  }

  reloadPreview(project) {
    const session = this.#sessions.get(project.id);
    if (!session || session.job.status !== 'ready') return this.startPreview(project);
    void session.reload();
    return session.job.view();
  }

  async restartPreview(project, job) {
    if (this.#previews.get(project.id) !== job || job.signal.aborted) return;
    job.stop();
    await job.done;
    if (!this.#closing && this.#previews.get(project.id) === job && !this.busy(project.id)) this.startPreview(project);
  }

  #commandWithPreviewPaused(kind, project, action) {
    const preview = this.#previews.get(project.id);
    const resume = preview && !preview.finished && !preview.signal.aborted;
    const job = this.#start(kind, project.id, async job => {
      if (resume) await this.stopPreview(project);
      job.signal.throwIfAborted();
      return action(job);
    }, true);
    if (resume) void job.done.then(() => {
      if (!this.#closing && this.#previews.get(project.id) === preview) {
        try { this.startPreview(project); } catch {}
      }
    });
    return job.view();
  }

  analyze(project, kind = 'analyze') {
    if (!['analyze', 'test'].includes(kind)) throw new Problem(400, 'Choose Analyze or Test.');
    return this.#commandWithPreviewPaused(kind, project, async job => {
      const sdk = await this.flutter.selected();
      await this.pub(job, project, ['pub', 'get'], sdk);
      const args = [kind, '--no-pub'];
      if (kind === 'test') args.push(...await this.backends.options(project));
      await job.exec(await this.flutter.command(args, sdk), project.root);
      return {};
    });
  }

  integrate(project, provider, input) {
    return this.#commandWithPreviewPaused('backend-' + provider, project,
      job => this.backends.configure(project, provider, input, args => this.pub(job, project, args), job));
  }

  signAndroid(project, input) {
    return this.#start('android-signing', project.id, job => {
      job.log('Configuring the Android release upload key.');
      return configureSigning(project, this.projects, input);
    }, true).view();
  }

  build(project, target, input = {}) {
    if (!targetIds.includes(target)) throw new Problem(400, 'Select a supported export target.');
    if (target === 'ios' && process.platform !== 'darwin') throw new Problem(422, 'Build App Store IPAs on macOS with Xcode. Use the Flutter source export to move the project.');
    const flags = target === 'source' ? [] : buildFlags(target, input);
    if (target === 'source') return this.#start('export-source', project.id, job => this.#makeArtifact(job, project, target), true).view();
    return this.#commandWithPreviewPaused('export-' + target, project, async job => {
      const sdk = await this.flutter.selected();
      const signing = target === 'android' && await signingStatus(project, this.projects);
      if (signing?.debugFallback) job.log('This project uses the default debug key. Configure an upload key before submitting to Google Play.');
      await this.pub(job, project, ['pub', 'get'], sdk);
      const definitions = await this.backends.options(project);
      const args = ['build', ...flags, '--no-pub', ...definitions];
      await job.exec(await this.flutter.command(args, sdk), project.root);
      return this.#makeArtifact(job, project, target, signing?.debugFallback === true);
    });
  }

  async #makeArtifact(job, project, target, debugSigned = false) {
    job.signal.throwIfAborted();
    const info = targetInfo[target];
    const folder = path.join(this.home, 'exports', project.id, job.id);
    await fs.mkdir(folder, { recursive: true });
    const name = (project.slug || project.name.toLowerCase().replace(/[^a-z0-9_-]+/g, '-').slice(0, 55) || 'flutter-app') + '-' + info.file;
    const file = path.join(folder, name);
    if (target === 'source' || target === 'web') {
      const root = target === 'source' ? project.root : path.join(project.root, 'build', 'web');
      await zipFolder(root, file, { source: target === 'source', signal: job.signal });
    } else {
      const location = {
        android: ['build', 'app', 'outputs', 'bundle'],
        apk: ['build', 'app', 'outputs', 'flutter-apk'],
        ios: ['build', 'ios', 'ipa'],
      }[target];
      const artifact = await findOutput(path.join(project.root, ...location), info.kind.toLowerCase());
      await fs.copyFile(artifact, file);
    }
    job.signal.throwIfAborted();
    const { size } = await fs.stat(file);
    const artifact = { id: job.id, name, target, label: info.label, size, date: new Date().toISOString(), debugSigned };
    await writeJson(path.join(folder, 'artifact.json'), artifact);
    job.log('Exported ' + name + '.');
    return { artifact };
  }

  async artifacts(project) {
    const directory = path.join(this.home, 'exports', project.id);
    if (!await exists(directory)) return [];
    const folders = (await fs.readdir(directory, { withFileTypes: true }))
      .filter(entry => entry.isDirectory() && /^[0-9a-f]{8}-[0-9a-f]{3}$/.test(entry.name));
    const records = await Promise.all(folders.map(entry => readJson(path.join(directory, entry.name, 'artifact.json'), null)));
    return records.filter(Boolean).sort((a, b) => b.date.localeCompare(a.date));
  }

  async artifact(project, id) {
    if (typeof id !== 'string' || !/^[0-9a-f]{8}-[0-9a-f]{3}$/.test(id)) throw new Problem(404, 'Export not found.');
    const directory = path.join(this.home, 'exports', project.id, id);
    const record = await readJson(path.join(directory, 'artifact.json'), null);
    if (!record || !/^[a-z0-9_-]+\.(?:zip|aab|apk|ipa)$/.test(record.name)) throw new Problem(404, 'Export not found.');
    const file = path.join(directory, record.name);
    if (!await exists(file) || !(await fs.lstat(file)).isFile()) throw new Problem(404, 'The export file is missing.');
    return { file, ...record };
  }

  async format(project, relative, expected) {
    if (typeof relative !== 'string' || !relative.endsWith('.dart')) throw new Problem(400, 'Open a Dart file to format it.');
    const source = await this.projects.file(project, relative);
    if (source.revision !== expected) throw new Problem(409, 'This file changed on disk. Reload it first.');
    const sdk = await this.flutter.selected();
    if (!sdk) throw new Problem(409, 'Install Flutter to format Dart files.');
    const dart = path.join(path.dirname(sdk.file), 'cache', 'dart-sdk', 'bin', process.platform === 'win32' ? 'dart.exe' : 'dart');
    if (!await exists(dart)) throw new Problem(409, 'Run Flutter once to initialize its Dart SDK.');
    return this.#start('format', project.id, async job => {
      const parent = path.join(this.home, 'tmp');
      await fs.mkdir(parent, { recursive: true });
      const folder = await fs.mkdtemp(path.join(parent, 'format-'));
      const real = await fs.realpath(folder);
      const base = await fs.realpath(parent);
      if (!inside(base, real) || real === base) throw new Error('Invalid formatter working directory.');
      try {
        const file = path.join(real, 'source.dart');
        await fs.writeFile(file, source.content);
        await job.exec({ file: dart, args: ['format', file] }, project.root);
        const updated = await this.projects.save(project, relative, await fs.readFile(file, 'utf8'), source.revision);
        return { path: relative, revision: updated.revision };
      } finally {
        await fs.rm(real, { recursive: true, force: true });
      }
    }, true).view();
  }

  sdkStatus() { return this.#sdkJob?.view() ?? null; }

  sdkInstall(automatic = false) {
    if (this.#sdkJob && !this.#sdkJob.finished) return this.#sdkJob.view();
    this.#sdkJob = this.#start('flutter-sdk', null, async job => {
      const release = await this.flutter.latest({ refresh: !automatic });
      return this.flutter.install(release, job);
    });
    return this.#sdkJob.view();
  }

  async autoUpdate() {
    const settings = await this.flutter.settings();
    if (settings.offline) return;
    const release = await this.flutter.latest().catch(() => null);
    const selected = await this.flutter.selected();
    if (release && selected?.mode === 'managed' && settings.managedAutoUpdate && selected.version !== release.version && !this.#closing) {
      this.sdkInstall(true);
    }
  }

  async close() {
    this.#closing = true;
    const jobs = [...this.#jobs.values()].filter(job => !job.finished);
    jobs.forEach(job => job.stop());
    await Promise.allSettled(jobs.map(job => job.done));
  }
}
