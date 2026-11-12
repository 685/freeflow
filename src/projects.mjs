import { randomUUID } from 'node:crypto';
import * as fs from 'node:fs/promises';
import path from 'node:path';
import {
  Problem, atomicWrite, exists, imageType, isText, maxText, projectFile, readJson,
  readSource, revision, visibleName, walk, writeJson,
} from './files.mjs';
import { backendStub, starter, templates } from './templates.mjs';

export class Serial {
  #tails = new Map();

  run(key, fn) {
    const task = (this.#tails.get(key) || Promise.resolve()).catch(() => {}).then(fn);
    this.#tails.set(key, task);
    return task.finally(() => {
      if (this.#tails.get(key) === task) this.#tails.delete(key);
    });
  }
}

function projectName(value) {
  const name = typeof value === 'string' ? value.trim() : '';
  if (!name || name.length > 70 || /[\x00-\x1f]/.test(name)) throw new Problem(400, 'Enter an app name of up to 70 characters.');
  return name;
}

function projectSlug(name) {
  const normalized = name.toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_+|_+$/g, '').slice(0, 44).replace(/_+$/g, '');
  return 'app_' + (normalized || 'starter');
}

const projectId = () => randomUUID().slice(0, 8);
const planStart = '# App plan\n\n## Goal\n\n## Screens\n\n- [ ] Home\n\n## Data and user flows\n';

export class Projects {
  #serial = new Serial();
  #db;

  constructor(home) {
    this.home = home;
    this.#db = path.join(home, 'projects.json');
  }

  list() {
    return readJson(this.#db, []);
  }

  async get(id) {
    const project = (await this.list()).find(item => item.id === id);
    if (!project) throw new Problem(404, 'Project not found.');
    if (!await exists(path.join(project.root, 'pubspec.yaml'))) {
      throw new Problem(410, 'The Flutter project folder has moved or is unavailable.');
    }
    return project;
  }

  async register(project) {
    return this.#serial.run('projects', async () => {
      const records = await this.list();
      const existing = records.find(item => item.root === project.root);
      if (existing) return existing;
      const record = { ...project, createdAt: new Date().toISOString() };
      await writeJson(this.#db, [record, ...records]);
      return record;
    });
  }

  async forget(id) {
    return this.#serial.run('projects', async () => {
      const records = await this.list();
      if (!records.some(project => project.id === id)) throw new Problem(404, 'Project not found.');
      await writeJson(this.#db, records.filter(project => project.id !== id));
    });
  }

  async importFolder(input) {
    if (typeof input !== 'string' || !path.isAbsolute(input)) throw new Problem(400, 'Enter the absolute path to a Flutter project.');
    const root = await fs.realpath(input.trim()).catch(() => {
      throw new Problem(404, 'That project folder could not be opened.');
    });
    let manifest;
    try {
      manifest = await fs.readFile(await projectFile(root, 'pubspec.yaml'), 'utf8');
    } catch {
      throw new Problem(400, 'Choose a Flutter project folder containing pubspec.yaml.');
    }
    if (!/^\s*sdk:\s*flutter\s*(?:#.*)?$/m.test(manifest) && !/flutter:\s*\{\s*sdk:\s*flutter\s*\}/.test(manifest)) {
      throw new Problem(400, 'pubspec.yaml must declare a Flutter SDK dependency.');
    }
    const displayName = manifest.match(/^name:\s*([a-zA-Z0-9_]+)/m)?.[1]?.replaceAll('_', ' ') || path.basename(root);
    return this.register({ id: projectId(), name: displayName, root, template: 'imported' });
  }

  async reserve(nameInput, templateId, organizationInput) {
    const name = projectName(nameInput);
    if (!templates.some(item => item.id === templateId)) throw new Problem(400, 'Select a starter template.');
    const organization = organizationInput || 'com.example';
    if (typeof organization !== 'string' || organization.length > 80 || !/^[a-z][a-z0-9]*(?:\.[a-z][a-z0-9]*)+$/.test(organization)) {
      throw new Problem(400, 'Use a reverse-DNS organization such as com.example.');
    }
    const id = projectId();
    const slug = projectSlug(name);
    const parent = path.join(this.home, 'projects');
    const root = path.join(parent, slug + '-' + id);
    await fs.mkdir(parent, { recursive: true });
    await fs.mkdir(root);
    return { id, name, root: await fs.realpath(root), slug, template: templateId, organization };
  }

  async finishScaffold(project) {
    await atomicWrite(path.join(project.root, 'lib', 'main.dart'), starter(project.template));
    await atomicWrite(path.join(project.root, 'lib', 'freeflow_backends.dart'), backendStub);
    await atomicWrite(path.join(project.root, 'FREEFLOW_PLAN.md'), '# ' + project.name + '\n' + planStart.slice(10));
    const defaultTest = path.join(project.root, 'test', 'widget_test.dart');
    if (await exists(defaultTest)) {
      await atomicWrite(defaultTest,
        "import 'package:flutter_test/flutter_test.dart';\n" +
        "import 'package:" + project.slug + "/main.dart';\n\n" +
        'void main() {\n' +
        "  testWidgets('starts the app', (tester) async {\n" +
        '    await tester.pumpWidget(const FreeflowApp());\n' +
        '    expect(find.byType(FreeflowApp), findsOneWidget);\n' +
        '  });\n' +
        '}\n');
    }
    return this.register(project);
  }

  tree(project) {
    return walk(project.root, { include: visibleName, directories: true });
  }

  async file(project, relative) {
    return readSource(await projectFile(project.root, relative), relative);
  }

  async plan(project) {
    try {
      return await this.file(project, 'FREEFLOW_PLAN.md');
    } catch (error) {
      if (error.status !== 404) throw error;
      return { path: 'FREEFLOW_PLAN.md', content: planStart, revision: null };
    }
  }

  save(project, relative, content, expected) {
    return this.update(project, relative, expected, () => content);
  }

  update(project, relative, expected, transform) {
    return this.#serial.run(project.root + '\0' + relative.toLowerCase(), async () => {
      const target = await projectFile(project.root, relative, true);
      const previous = await exists(target) ? await readSource(target, relative) : null;
      if (expected !== (previous?.revision ?? null)) {
        throw new Problem(409, 'This file changed on disk. Reload it and retry your edit.');
      }
      const content = await transform(previous);
      if (typeof content !== 'string' || Buffer.byteLength(content) > maxText || content.includes('\0')) {
        throw new Problem(413, 'Enter text up to 2 MiB.');
      }
      // Validate a new file's type before creating it.
      if (!previous && !isText(path.basename(relative))) throw new Problem(415, 'Choose a supported text file type.');
      if (previous?.content === content) return previous;
      if (previous) await this.#remember(project, relative, previous);
      const mode = previous ? (await fs.stat(target)).mode & 0o777 : 0o644;
      await atomicWrite(target, content, mode);
      return { path: relative, content, revision: revision(content) };
    });
  }

  #historyPath(project, relative) {
    return path.join(this.home, 'history', project.id, revision(relative.toLowerCase()));
  }

  async #remember(project, relative, previous) {
    const directory = this.#historyPath(project, relative);
    const id = Date.now() + '-' + randomUUID().slice(0, 6);
    await writeJson(path.join(directory, id + '.json'), {
      id, date: new Date().toISOString(), content: previous.content, revision: previous.revision,
    });
    const files = (await fs.readdir(directory)).filter(file => /^\d{13}-[0-9a-f]{6}\.json$/.test(file)).sort().reverse();
    await Promise.all(files.slice(30).map(file => fs.rm(path.join(directory, file), { force: true })));
  }

  async history(project, relative) {
    await projectFile(project.root, relative);
    const directory = this.#historyPath(project, relative);
    if (!await exists(directory)) return [];
    const entries = (await fs.readdir(directory)).filter(file => /^\d{13}-[0-9a-f]{6}\.json$/.test(file)).sort().reverse();
    return Promise.all(entries.map(async name => {
      const { id, date, revision: hash } = await readJson(path.join(directory, name));
      return { id, date, revision: hash };
    }));
  }

  async historyVersion(project, relative, id) {
    if (typeof id !== 'string' || !/^\d{13}-[0-9a-f]{6}$/.test(id)) throw new Problem(400, 'Invalid history entry.');
    await projectFile(project.root, relative);
    const result = await readJson(path.join(this.#historyPath(project, relative), id + '.json'), null);
    if (!result) throw new Problem(404, 'History entry not found.');
    return result;
  }

  async asset(project, name, encoded) {
    if (typeof name !== 'string' || !imageType.test(name) || !/^[\w.-]+$/.test(name) || name.startsWith('.')) {
      throw new Problem(400, 'Use an image filename containing letters, numbers, dashes or underscores.');
    }
    if (typeof encoded !== 'string' || !/^[A-Za-z0-9+/]*={0,2}$/.test(encoded)) throw new Problem(400, 'Invalid image.');
    const data = Buffer.from(encoded, 'base64');
    if (!data.length || data.length > 10 * 1024 * 1024) throw new Problem(413, 'Images must be between 1 byte and 10 MiB.');
    const relative = 'assets/' + name;
    const target = await projectFile(project.root, relative, true);
    await fs.mkdir(path.dirname(target), { recursive: true });
    try {
      await fs.writeFile(target, data, { flag: 'wx', mode: 0o644 });
    } catch (error) {
      if (error.code === 'EEXIST') throw new Problem(409, 'An asset with that name already exists.');
      throw error;
    }
    return { path: relative };
  }
}
