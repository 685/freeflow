import * as fs from 'node:fs/promises';
import path from 'node:path';
import { Problem, exists, readJson, writeJson } from './files.mjs';
import { backendMarker, backendStub } from './templates.mjs';

export const backendFile = 'lib/freeflow_backends.dart';
export const startupCode = "import 'freeflow_backends.dart';\n\nFuture<void> main() async {\n  WidgetsFlutterBinding.ensureInitialized();\n  await FreeflowBackends.initialize();\n  runApp(const MyApp());\n}";
const firebaseFields = {
  apiKey: 'FIREBASE_API_KEY', appId: 'FIREBASE_APP_ID', projectId: 'FIREBASE_PROJECT_ID',
  messagingSenderId: 'FIREBASE_SENDER_ID', authDomain: 'FIREBASE_AUTH_DOMAIN', storageBucket: 'FIREBASE_STORAGE_BUCKET',
  databaseURL: 'FIREBASE_DATABASE_URL', measurementId: 'FIREBASE_MEASUREMENT_ID',
  androidAppId: 'FIREBASE_ANDROID_APP_ID', androidApiKey: 'FIREBASE_ANDROID_API_KEY',
  iosAppId: 'FIREBASE_IOS_APP_ID', iosApiKey: 'FIREBASE_IOS_API_KEY', iosBundleId: 'FIREBASE_IOS_BUNDLE_ID',
  iosClientId: 'FIREBASE_IOS_CLIENT_ID',
};
const supabaseFields = { url: 'SUPABASE_URL', key: 'SUPABASE_PUBLISHABLE_KEY' };
const fields = { firebase: firebaseFields, supabase: supabaseFields };
const dependencies = {
  firebase: ['firebase_core', 'firebase_auth', 'cloud_firestore'],
  supabase: ['supabase_flutter'],
};

function clean(value, label, required = false) {
  const text = typeof value === 'string' ? value.trim() : '';
  if ((!text && required) || text.length > 3000 || /[\x00-\x1f]/.test(text)) throw new Problem(400, 'Enter a valid ' + label + '.');
  return text;
}

function supabaseConfig(input) {
  const value = clean(input.url, 'Supabase project URL', true);
  let url;
  try { url = new URL(value); } catch {}
  const local = ['localhost', '127.0.0.1', '[::1]'].includes(url?.hostname);
  if (!url || !(url.protocol === 'https:' || (local && url.protocol === 'http:')) ||
      url.username || url.password || url.search || url.hash || url.pathname !== '/') {
    throw new Problem(400, 'Use the project URL from Supabase Connect, such as https://example.supabase.co.');
  }
  const key = clean(input.key, 'Supabase publishable key', true);
  let legacyAnon = false;
  if (/^[\w-]+\.[\w-]+\.[\w-]+$/.test(key)) {
    try {
      legacyAnon = JSON.parse(Buffer.from(key.split('.')[1], 'base64url').toString()).role === 'anon';
    } catch {}
  }
  if (!/^sb_publishable_[a-zA-Z0-9_-]+$/.test(key) && !legacyAnon) {
    throw new Problem(400, 'Use a Supabase publishable key or legacy anon key.');
  }
  return { url: url.origin, key };
}

function firebaseConfig(input) {
  const result = {};
  const required = new Set(['apiKey', 'appId', 'projectId', 'messagingSenderId']);
  for (const name of Object.keys(firebaseFields)) result[name] = clean(input[name], 'Firebase ' + name, required.has(name));
  if (!/^[a-z][a-z0-9-]{2,62}$/.test(result.projectId) || !/^\d+$/.test(result.messagingSenderId) ||
      !/^1:\d+:web:[\da-z]+$/i.test(result.appId) || !/^[\w-]{20,}$/.test(result.apiKey)) {
    throw new Problem(400, 'Copy the web app configuration from Firebase Project settings.');
  }
  for (const target of ['android', 'ios']) {
    if (result[target + 'AppId'] && !new RegExp('^1:\\d+:' + target + ':[\\da-z]+$', 'i').test(result[target + 'AppId'])) {
      throw new Problem(400, 'Enter a Firebase ' + target + ' app ID for the matching registered app.');
    }
    if (result[target + 'ApiKey'] && !/^[\w-]{20,}$/.test(result[target + 'ApiKey'])) {
      throw new Problem(400, 'Enter a valid Firebase ' + target + ' API key.');
    }
  }
  return result;
}

export function validateBackend(provider, input) {
  if (!Object.hasOwn(fields, provider) || !input || typeof input !== 'object') throw new Problem(400, 'Choose a backend provider.');
  if (input.enabled === false) return null;
  return provider === 'firebase' ? firebaseConfig(input) : supabaseConfig(input);
}

const env = (name, explicitConst = true) => (explicitConst ? 'const ' : '') + "String.fromEnvironment('" + name + "')";

export function generateBackends(enabled) {
  if (!enabled.firebase && !enabled.supabase) return backendStub;
  const imports = [];
  const statements = [];
  const helpers = [];
  if (enabled.firebase) {
    imports.push("import 'package:firebase_core/firebase_core.dart';", "import 'package:flutter/foundation.dart';");
    statements.push('    await Firebase.initializeApp(options: _firebaseOptions());');
    helpers.push(
      '\n  static String? _optional(String value) => value.isEmpty ? null : value;\n',
      '\n  static FirebaseOptions _firebaseOptions() {',
      '    const webId = ' + env('FIREBASE_APP_ID', false) + ';',
      '    const webKey = ' + env('FIREBASE_API_KEY', false) + ';',
      '    var appId = webId;',
      '    var apiKey = webKey;',
      '    String? iosBundleId;',
      '    String? iosClientId;',
      '    if (!kIsWeb) {',
      '      switch (defaultTargetPlatform) {',
      '        case TargetPlatform.android:',
      '          appId = ' + env('FIREBASE_ANDROID_APP_ID') + ';',
      '          const androidKey = ' + env('FIREBASE_ANDROID_API_KEY', false) + ';',
      '          apiKey = androidKey.isEmpty ? webKey : androidKey;',
      '          break;',
      '        case TargetPlatform.iOS:',
      '          appId = ' + env('FIREBASE_IOS_APP_ID') + ';',
      '          const iosKey = ' + env('FIREBASE_IOS_API_KEY', false) + ';',
      '          apiKey = iosKey.isEmpty ? webKey : iosKey;',
      '          iosBundleId = _optional(' + env('FIREBASE_IOS_BUNDLE_ID') + ');',
      '          iosClientId = _optional(' + env('FIREBASE_IOS_CLIENT_ID') + ');',
      '          break;',
      '        default:',
      "          throw UnsupportedError('Freeflow Firebase supports web, Android and iOS.');",
      '      }',
      '    }',
      "    if (appId.isEmpty || apiKey.isEmpty) throw StateError('Add Firebase settings for this platform in Freeflow.');",
      '    return FirebaseOptions(',
      '      apiKey: apiKey,',
      '      appId: appId,',
      '      projectId: ' + env('FIREBASE_PROJECT_ID') + ',',
      '      messagingSenderId: ' + env('FIREBASE_SENDER_ID') + ',',
      '      authDomain: _optional(' + env('FIREBASE_AUTH_DOMAIN') + '),',
      '      storageBucket: _optional(' + env('FIREBASE_STORAGE_BUCKET') + '),',
      '      databaseURL: _optional(' + env('FIREBASE_DATABASE_URL') + '),',
      '      measurementId: kIsWeb ? _optional(' + env('FIREBASE_MEASUREMENT_ID') + ') : null,',
      '      iosBundleId: iosBundleId,',
      '      iosClientId: iosClientId,',
      '    );',
      '  }',
    );
  }
  if (enabled.supabase) {
    imports.push("import 'package:supabase_flutter/supabase_flutter.dart';");
    statements.push(
      '    const supabaseUrl = ' + env('SUPABASE_URL', false) + ';',
      '    const supabaseKey = ' + env('SUPABASE_PUBLISHABLE_KEY', false) + ';',
      "    if (supabaseUrl.isEmpty || supabaseKey.isEmpty) throw StateError('Add Supabase settings in Freeflow.');",
      '    await Supabase.initialize(url: supabaseUrl, publishableKey: supabaseKey);',
    );
  }
  return [
    backendMarker, ...imports, '', 'class FreeflowBackends {',
    '  static Future<void> initialize() async {', ...statements, '  }', ...helpers, '}', '',
  ].join('\n');
}

export class Backends {
  constructor(home, projects) {
    this.home = home;
    this.projects = projects;
  }

  configPath(project) {
    return path.join(this.home, 'backends', project.id + '.json');
  }

  async state(project) {
    const defines = await readJson(this.configPath(project), {});
    const output = {};
    for (const [provider, names] of Object.entries(fields)) {
      const config = Object.fromEntries(Object.entries(names).map(([name, key]) => [name, defines[key] || '']));
      output[provider] = { enabled: Object.values(config).some(Boolean), ...config };
    }
    const entry = await this.projects.file(project, 'lib/main.dart').catch(() => null);
    return { ...output, wired: !!entry?.content.includes('FreeflowBackends.initialize('), definesFile: Object.keys(defines).length ? this.configPath(project) : null };
  }

  async options(project) {
    const file = this.configPath(project);
    const config = await readJson(file, {});
    return Object.keys(config).length ? ['--dart-define-from-file=' + file] : [];
  }

  async configure(project, provider, input, pub, job) {
    const config = validateBackend(provider, input);
    const before = await this.projects.file(project, backendFile).catch(error => {
      if (error.status !== 404) throw error;
      return null;
    });
    if (before && !before.content.startsWith(backendMarker)) {
      throw new Problem(409, 'lib/freeflow_backends.dart is already in use. Rename it or connect the backend in your own code.');
    }
    const file = this.configPath(project);
    const hadFile = await exists(file);
    const previous = await readJson(file, {});
    const defines = { ...previous };
    for (const [name, key] of Object.entries(fields[provider])) {
      delete defines[key];
      if (config?.[name]) defines[key] = config[name];
    }
    const enabled = { firebase: !!defines.FIREBASE_APP_ID, supabase: !!defines.SUPABASE_URL };
    if (config) await pub(['pub', 'add', ...dependencies[provider]]);
    job.signal.throwIfAborted();
    await writeJson(file, defines);
    try {
      await this.projects.save(project, backendFile, generateBackends(enabled), before?.revision ?? null);
    } catch (error) {
      if (hadFile) await writeJson(file, previous);
      else await fs.rm(file, { force: true });
      throw error;
    }
    const result = await this.state(project);
    job.log(config ? provider + ' is configured.' : provider + ' is disconnected.');
    if (!result.wired && (enabled.firebase || enabled.supabase)) job.log('Call FreeflowBackends.initialize() before runApp in lib/main.dart.');
    return { provider, enabled: !!config, wired: result.wired };
  }
}
