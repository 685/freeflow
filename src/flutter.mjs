import { createHash } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import * as fs from 'node:fs/promises';
import path from 'node:path';
import { Readable, Transform } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { Problem, exists, inside, readJson, writeJson } from './files.mjs';
import { capture, executable, launch } from './processes.mjs';

const windows = process.platform === 'win32';
const platform = { win32: 'windows', darwin: 'macos', linux: 'linux' }[process.platform];
const mirror = 'https://storage.googleapis.com/flutter_infra_release/flutter/';
const launcher = windows ? 'flutter.bat' : 'flutter';
const stableVersion = /^\d+\.\d+\.\d+$/;
const timeoutMinutes = 35;
const quiet = { FLUTTER_SUPPRESS_ANALYTICS: 'true', DART_SUPPRESS_ANALYTICS: 'true', CI: 'true' };

async function findFlutter(value) {
  if (!value) return null;
  const file = path.resolve(value);
  const options = [file, path.join(file, 'bin', launcher)];
  for (const option of options) {
    try {
      if ((await fs.stat(option)).isFile()) return option;
    } catch (error) {
      if (error.code !== 'ENOENT' && error.code !== 'ENOTDIR') throw error;
    }
  }
  return null;
}

export class Flutter {
  #manifest;
  #expires = 0;
  #checking;

  constructor(home) {
    this.home = home;
    this.sdkHome = path.join(home, 'sdk');
    this.config = path.join(home, 'settings.json');
  }

  async settings() {
    return { flutterPath: '', offline: false, managedAutoUpdate: true, ...await readJson(this.config, {}) };
  }

  async saveSettings(input) {
    const flutterPath = input.flutterPath || '';
    if (typeof flutterPath !== 'string' || (flutterPath && !path.isAbsolute(flutterPath))) {
      throw new Problem(400, 'Enter an absolute Flutter SDK or executable path.');
    }
    if (flutterPath && !await findFlutter(flutterPath)) throw new Problem(400, 'Flutter was not found at that path.');
    await writeJson(this.config, {
      flutterPath,
      offline: input.offline === true,
      managedAutoUpdate: input.managedAutoUpdate !== false,
    });
    return this.settings();
  }

  async managed() {
    const selected = await readJson(path.join(this.sdkHome, 'active.json'), null);
    if (!selected || !stableVersion.test(selected.version)) return null;
    const file = await findFlutter(path.join(this.sdkHome, selected.version, 'flutter'));
    return file && { file, version: selected.version, mode: 'managed' };
  }

  async selected() {
    const settings = await this.settings();
    const override = process.env.FREEFLOW_FLUTTER || settings.flutterPath;
    if (override) {
      const file = await findFlutter(override);
      return file && { file, mode: 'external' };
    }
    const managed = await this.managed();
    if (managed) return managed;
    const file = executable(launcher);
    return file && { file, mode: 'external' };
  }

  async command(args, selected) {
    const sdk = selected ?? await this.selected();
    if (!sdk) throw new Problem(409, 'Install the stable Flutter SDK in Settings or add Flutter to PATH.');
    const temporary = path.join(this.home, 'tmp', 'flutter');
    await fs.mkdir(temporary, { recursive: true });
    const env = { ...quiet, ...(windows ? { TEMP: temporary, TMP: temporary } : { TMPDIR: temporary }) };
    const root = path.dirname(path.dirname(sdk.file));
    const standardRoot = path.basename(path.dirname(sdk.file)).toLowerCase() === 'bin' &&
      path.basename(sdk.file).toLowerCase() === launcher;
    if (process.env.FREEFLOW_FLUTTER_DIRECT === '1' && standardRoot) {
      const cache = path.join(root, 'bin', 'cache');
      const dart = path.join(cache, 'dart-sdk', 'bin', windows ? 'dart.exe' : 'dart');
      const snapshot = path.join(cache, 'flutter_tools.snapshot');
      if (await exists(dart) && await exists(snapshot)) {
        return { file: dart, args: [snapshot, '--no-version-check', '--suppress-analytics', ...args], env: { ...env, FLUTTER_ROOT: root } };
      }
    }
    return { file: sdk.file, args: ['--no-version-check', '--suppress-analytics', ...args], env };
  }

  async status() {
    const settings = await this.settings();
    const selected = await this.selected();
    let version = selected?.version;
    let channel = selected?.mode === 'managed' ? 'stable' : undefined;
    if (selected && !version && path.basename(selected.file).toLowerCase() === launcher &&
        path.basename(path.dirname(selected.file)).toLowerCase() === 'bin') {
      const cache = path.join(path.dirname(selected.file), 'cache', 'flutter.version.json');
      const info = await readJson(cache, {}).catch(() => ({}));
      version = info.frameworkVersion ?? info.flutterVersion;
      channel = info.channel;
    }
    return {
      found: !!selected, version, channel, mode: selected?.mode,
      latest: this.#manifest?.version, available: this.#manifest ? version !== this.#manifest.version : undefined,
      platform: process.platform, settings,
    };
  }

  async latest({ refresh = false } = {}) {
    if (!platform || !['x64', 'arm64'].includes(process.arch)) throw new Problem(422, 'Managed Flutter is unavailable for this platform.');
    if (!refresh && this.#manifest && Date.now() < this.#expires) return this.#manifest;
    if (this.#checking) return this.#checking;
    this.#checking = this.#fetchRelease().finally(() => { this.#checking = null; });
    return this.#checking;
  }

  async #fetchRelease() {
    const manifest = await fetch(mirror + 'releases/releases_' + platform + '.json', {
      signal: AbortSignal.timeout(8000), redirect: 'error',
    }).then(response => {
      if (!response.ok) throw new Error('Flutter release metadata returned HTTP ' + response.status);
      return response.json();
    }).catch(() => { throw new Problem(503, 'The official Flutter release server is unreachable. Installed SDKs can still be used.'); });
    const stableHash = manifest.current_release?.stable;
    const candidates = (manifest.releases || []).filter(item => item.hash === stableHash && item.channel === 'stable');
    const release = candidates.find(item => item.dart_sdk_arch === process.arch) ||
      (process.arch === 'x64' ? candidates.find(item => !item.dart_sdk_arch) : null);
    if (!release || !stableVersion.test(release.version) || !/^[a-f0-9]{64}$/i.test(release.sha256)) {
      throw new Problem(422, 'No verified stable Flutter archive was found for this architecture.');
    }
    const url = new URL(release.archive, String(manifest.base_url).replace(/\/$/, '') + '/');
    if (!url.href.startsWith(mirror + 'stable/' + platform + '/') || !/\.(?:zip|tar\.xz)$/.test(url.pathname)) {
      throw new Problem(422, 'Flutter returned an unexpected archive location.');
    }
    this.#manifest = { version: release.version, sha256: release.sha256.toLowerCase(), url: url.href };
    this.#expires = Date.now() + 6 * 60 * 60 * 1000;
    return this.#manifest;
  }

  async install(release, job) {
    const { version, sha256, url } = release;
    const sdkBase = path.join(this.sdkHome, version);
    if (!await findFlutter(path.join(sdkBase, 'flutter'))) {
      const tar = executable(windows ? 'tar.exe' : 'tar');
      if (!tar) throw new Problem(409, 'Install tar so Freeflow can extract the official Flutter archive.');
      await fs.mkdir(this.sdkHome, { recursive: true });
      const staging = await fs.mkdtemp(path.join(this.sdkHome, 'download-'));
      const base = await fs.realpath(this.sdkHome);
      const cleanup = await fs.realpath(staging);
      if (!inside(base, cleanup) || cleanup === base) throw new Error('Invalid Flutter staging directory.');
      try {
        const archive = path.join(staging, url.endsWith('.zip') ? 'flutter.zip' : 'flutter.tar.xz');
        const unpacked = path.join(staging, 'unpacked');
        await fs.mkdir(unpacked);
        job.log('Downloading Flutter ' + version + ' stable.');
        const signal = AbortSignal.any([job.signal, AbortSignal.timeout(timeoutMinutes * 60 * 1000)]);
        const response = await fetch(url, { signal, redirect: 'error' });
        if (!response.ok || !response.body) throw new Error('Flutter download returned HTTP ' + response.status);
        const expectedLength = Number(response.headers.get('content-length')) || 0;
        const hash = createHash('sha256');
        let lastProgress = 0;
        let bytes = 0;
        const meter = new Transform({
          transform(chunk, _, callback) {
            hash.update(chunk);
            bytes += chunk.length;
            const percent = expectedLength ? Math.floor(bytes / expectedLength * 100) : 0;
            if (percent >= lastProgress + 10) {
              lastProgress = percent;
              job.log('Download ' + percent + '%');
            }
            callback(null, chunk);
          },
        });
        await pipeline(Readable.fromWeb(response.body), meter, createWriteStream(archive), { signal });
        if (hash.digest('hex') !== sha256) throw new Error('Flutter archive failed SHA-256 verification.');
        job.log('Extracting the verified Flutter archive.');
        await job.exec({ file: tar, args: ['-xf', archive, '-C', unpacked] });
        if (!await findFlutter(path.join(unpacked, 'flutter'))) throw new Error('Flutter archive did not contain its SDK.');
        if (!inside(base, await fs.realpath(unpacked)) || !inside(base, path.join(base, version))) {
          throw new Error('Invalid Flutter installation path.');
        }
        await fs.rename(unpacked, sdkBase);
      } finally {
        await fs.rm(cleanup, { recursive: true, force: true });
      }
    }
    job.signal.throwIfAborted();
    await writeJson(path.join(this.sdkHome, 'active.json'), { version });
    job.log('Flutter ' + version + ' stable is ready.');
    return { version };
  }

  async diagnostics() {
    const command = await this.command(['doctor', '-v']);
    return capture(command.file, command.args, { env: command.env, timeout: 45000, input: '' });
  }

  async start(args, options = {}) {
    const { file, args: argv, env } = await this.command(args, options.sdk);
    return launch(file, argv, { ...options, env: { ...env, ...options.env } });
  }
}
