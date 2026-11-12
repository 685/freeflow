import { createHash, randomUUID } from 'node:crypto';
import { constants } from 'node:fs';
import * as fs from 'node:fs/promises';
import { homedir } from 'node:os';
import path from 'node:path';

export class Problem extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

export const dataHome = () => path.resolve(process.env.FREEFLOW_HOME || path.join(homedir(), '.freeflow'));
export const revision = value => createHash('sha256').update(value).digest('hex');
export const maxText = 2 * 1024 * 1024;

export async function exists(file) {
  try {
    await fs.access(file, constants.F_OK);
    return true;
  } catch (error) {
    if (error.code === 'ENOENT' || error.code === 'ENOTDIR') return false;
    throw error;
  }
}

export async function readJson(file, fallback) {
  try {
    return JSON.parse((await fs.readFile(file, 'utf8')).replace(/^\uFEFF/, ''));
  } catch (error) {
    if (error.code === 'ENOENT') return fallback;
    throw error;
  }
}

export async function atomicWrite(file, content, mode = 0o600) {
  await fs.mkdir(path.dirname(file), { recursive: true });
  const temp = path.join(path.dirname(file), '.freeflow-tmp-' + randomUUID());
  try {
    await fs.writeFile(temp, content, { flag: 'wx', mode });
    await fs.rename(temp, file);
  } finally {
    await fs.rm(temp, { force: true });
  }
}

export const writeJson = (file, value) => atomicWrite(file, JSON.stringify(value, null, 2) + '\n');

const privateNames = new Set([
  '.git', '.dart_tool', '.freeflow', '.idea', '.gradle', '.symlinks', '.cxx',
  'build', 'node_modules', 'pods', 'key.properties', 'local.properties',
  '.flutter-plugins', '.flutter-plugins-dependencies', '.netrc', '.npmrc', '.ssh',
  '.firebase', '.fvm', 'deriveddata', 'ephemeral', '.settings', '.vscode',
]);
const secretName = /^(?:\.env(?:\.|$)|service[-_]?account|firebase-adminsdk|appstoreconnect|authkey_|credentials\.)|\.(?:p8|p12|jks|keystore|pem|mobileprovision|key|log|apk|aab|ipa)$/i;
const allowedDot = new Set(['.gitignore', '.metadata', '.gitattributes', '.github', '.env.example']);
const textType = /\.(?:dart|ya?ml|json|md|txt|xml|plist|html|css|scss|js|mjs|ts|swift|java|kt|kts|gradle|rb|sh|properties|xcconfig|pbxproj|entitlements|sql|svg|lock|toml|iml|arb|webmanifest|gitignore|gitattributes)$/i;
export const imageType = /\.(?:avif|gif|ico|jpe?g|png|webp)$/i;
export const isText = name => textType.test(name) || ['Podfile', 'Gemfile', '.metadata', 'LICENSE', 'README'].includes(name);

export function visibleName(name) {
  const lower = name.toLowerCase();
  if (privateNames.has(lower)) return false;
  if (allowedDot.has(lower)) return true;
  return !name.startsWith('.') && !secretName.test(name);
}

export function relativeParts(value) {
  if (typeof value !== 'string' || value.length > 700 || !value) {
    throw new Problem(400, 'Enter a project-relative file path.');
  }
  const parts = value.split('/');
  const invalid = part => !part || !visibleName(part) || /[\\:*?"<>|\x00-\x1f]/.test(part) ||
    /[. ]$/.test(part) || /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(part);
  if (parts.some(invalid)) throw new Problem(400, 'That project path is unavailable.');
  return parts;
}

export function inside(root, candidate) {
  const relative = path.relative(root, candidate);
  return relative === '' || (!path.isAbsolute(relative) && relative !== '..' && !relative.startsWith('..' + path.sep));
}

// Use the canonical root and reject links at every component, including links
// inside imported projects. A Flutter build can still execute trusted project code.
export async function projectFile(root, value, create = false) {
  const parts = relativeParts(value);
  let target = root;
  for (let i = 0; i < parts.length; i++) {
    target = path.join(target, parts[i]);
    let stat;
    try {
      stat = await fs.lstat(target);
    } catch (error) {
      if (create && error.code === 'ENOENT') {
        return path.join(target, ...parts.slice(i + 1));
      }
      if (error.code === 'ENOENT' || error.code === 'ENOTDIR') throw new Problem(404, 'Project file not found.');
      throw error;
    }
    if (stat.isSymbolicLink()) throw new Problem(403, 'Linked files cannot be edited through Freeflow.');
    if (i < parts.length - 1 && !stat.isDirectory()) throw new Problem(400, 'A path component is not a folder.');
    if (i === parts.length - 1 && !stat.isFile()) throw new Problem(400, 'Select a project file.');
  }
  return target;
}

export async function walk(root, options = {}) {
  const maxEntries = options.maxEntries ?? 5000;
  const entries = [];
  let truncated = false;
  async function visit(parent, relative = '', depth = 0) {
    if (depth > 28) {
      truncated = true;
      return;
    }
    const children = await fs.readdir(parent, { withFileTypes: true });
    children.sort((a, b) => Number(b.isDirectory()) - Number(a.isDirectory()) || a.name.localeCompare(b.name));
    for (const entry of children) {
      if (entries.length >= maxEntries) {
        truncated = true;
        return;
      }
      if (entry.isSymbolicLink() || (options.include && !options.include(entry.name))) continue;
      const name = relative + entry.name;
      const fullPath = path.join(parent, entry.name);
      if (entry.isDirectory()) {
        if (options.directories) entries.push({ path: name, type: 'directory' });
        await visit(fullPath, name + '/', depth + 1);
      } else if (entry.isFile()) {
        entries.push({ path: name, type: 'file' });
      }
    }
  }
  await visit(root);
  return { entries, truncated };
}

export async function readSource(file, relativePath) {
  if (!isText(path.basename(relativePath))) throw new Problem(415, 'This file is not editable as text.');
  const stat = await fs.stat(file);
  if (stat.size > maxText) throw new Problem(413, 'Text files are limited to 2 MiB in the editor.');
  const content = await fs.readFile(file, 'utf8');
  if (content.includes('\0')) throw new Problem(415, 'This file contains binary data.');
  return { path: relativePath, content, revision: revision(content) };
}
