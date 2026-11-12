import * as fs from 'node:fs/promises';
import path from 'node:path';
import { Problem, atomicWrite, exists } from './files.mjs';

const gradlePath = 'android/app/build.gradle.kts';
const debugFallback = 'signingConfig = signingConfigs.getByName("debug")';
const releaseSigning = 'signingConfig = signingConfigs.getByName("release")';
const keyHeader = '# Freeflow Android release signing\n';
const gradleHeader = 'val freeflowKeystore = Properties().apply {\n' +
  '    val propertiesFile = rootProject.file("key.properties")\n' +
  '    if (propertiesFile.isFile) propertiesFile.inputStream().use { load(it) }\n' +
  '}\n\n';
const signingConfig = '    signingConfigs {\n' +
  '        create("release") {\n' +
  '            storeFile = freeflowKeystore.getProperty("storeFile")?.let { file(it) }\n' +
  '            storePassword = freeflowKeystore.getProperty("storePassword")\n' +
  '            keyAlias = freeflowKeystore.getProperty("keyAlias")\n' +
  '            keyPassword = freeflowKeystore.getProperty("keyPassword")\n' +
  '        }\n' +
  '    }\n\n';

const javaValue = value => value.replace(/[\\\x00-\x20\x7f-\uffff]/g, char =>
  '\\u' + char.charCodeAt(0).toString(16).padStart(4, '0'));

export async function signingStatus(project, projects) {
  const gradle = await projects.file(project, gradlePath).catch(() => null);
  const managed = !!gradle?.content.includes('val freeflowKeystore =');
  const configured = managed && !!gradle?.content.includes(releaseSigning) &&
    await exists(path.join(project.root, 'android', 'key.properties'));
  return {
    managed, configured,
    compatible: managed || !!gradle?.content.includes(debugFallback),
    appId: gradle?.content.match(/applicationId\s*=\s*"([^"]+)"/)?.[1],
    debugFallback: !!gradle?.content.includes(debugFallback),
  };
}

export async function configureSigning(project, projects, input) {
  const { storeFile, keyAlias, storePassword, keyPassword } = input;
  if (typeof storeFile !== 'string' || !path.isAbsolute(storeFile) || !/\.(?:jks|keystore|p12)$/i.test(storeFile)) {
    throw new Problem(400, 'Enter the absolute path to your upload keystore.');
  }
  const stat = await fs.stat(storeFile).catch(() => null);
  if (!stat?.isFile()) throw new Problem(400, 'The upload keystore was not found.');
  const properties = { storeFile, keyAlias, storePassword, keyPassword };
  if (Object.values(properties).some(value => typeof value !== 'string' || !value || value.length > 1000 || /[\x00-\x1f]/.test(value))) {
    throw new Problem(400, 'Enter the upload key alias and both passwords.');
  }
  const gradle = await projects.file(project, gradlePath).catch(() => {
    throw new Problem(422, 'Configure signing in this project’s Android Gradle file.');
  });
  const keyFile = path.join(project.root, 'android', 'key.properties');
  const keyStat = await fs.lstat(keyFile).catch(error => { if (error.code !== 'ENOENT') throw error; });
  if (keyStat && (!keyStat.isFile() || !(await fs.readFile(keyFile, 'utf8')).startsWith(keyHeader))) {
    throw new Problem(409, 'This project already has its own Android signing settings.');
  }
  const managed = gradle.content.includes(gradleHeader) && gradle.content.includes(releaseSigning);
  if (!managed && (!gradle.content.includes(debugFallback) || !/^android \{$/m.test(gradle.content) ||
      !/^    buildTypes \{$/m.test(gradle.content) || /^\s*signingConfigs\s*\{/m.test(gradle.content))) {
    throw new Problem(422, 'This Gradle layout needs manual Android signing configuration.');
  }
  const gitignore = await projects.file(project, '.gitignore').catch(error => {
    if (error.status !== 404) throw error;
    return null;
  });
  const ignore = gitignore?.content ?? '';
  if (!ignore.split(/\r?\n/).includes('/android/key.properties')) {
    await projects.save(project, '.gitignore', ignore.trimEnd() + '\n\n/android/key.properties\n*.jks\n*.keystore\n*.p12\n', gitignore?.revision ?? null);
  }
  if (!managed) {
    const imports = /^\s*import java\.util\.Properties\s*$/m.test(gradle.content) ? '' : 'import java.util.Properties\n\n';
    const patched = imports + gradle.content.replace(/^android \{/m, gradleHeader + 'android {')
      .replace(/^    buildTypes \{/m, signingConfig + '    buildTypes {')
      .replace(debugFallback, releaseSigning);
    await projects.save(project, gradlePath, patched, gradle.revision);
  }
  const lines = Object.entries(properties).map(([key, value]) => key + '=' + javaValue(value));
  await atomicWrite(keyFile, keyHeader + lines.join('\n') + '\n');
  return signingStatus(project, projects);
}
