#!/usr/bin/env node
// Sign CI-built bundles with an existing local identity. Never export its key.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { command, verifyApp } = require('./macos-signing.cjs');
const { createDmgMounts } = require('./dmg-mounts.cjs');
const { verifyTree } = require('./package-local-dmg.cjs');

const ARCHES = { aarch64: ['arm64'], x64: ['x86_64'], universal: ['arm64', 'x86_64'] };
const MACH_MAGICS = new Set(['feedface', 'cefaedfe', 'feedfacf', 'cffaedfe', 'cafebabe', 'bebafeca', 'cafebabf', 'bfbafeca']);

function sha256(file) {
  const hash = crypto.createHash('sha256');
  const buffer = Buffer.alloc(1024 * 1024);
  const fd = fs.openSync(file, 'r');
  try {
    let size;
    while ((size = fs.readSync(fd, buffer))) hash.update(buffer.subarray(0, size));
  } finally { fs.closeSync(fd); }
  return hash.digest('hex');
}

function validateOptions(options) {
  for (const name of ['inputDmg', 'outputDir', 'version', 'arch', 'identity', 'team', 'keyPath', 'keyId', 'issuer', 'bundleIdentifier']) {
    if (typeof options[name] !== 'string' || !options[name].trim()) throw new Error(`Missing ${name}`);
  }
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(options.version)) throw new Error('Expected a stable version');
  if (!ARCHES[options.arch]) throw new Error('Unsupported macOS architecture');
  if (!/^[A-Z0-9]{10}$/.test(options.team)
      || !options.identity.startsWith('Developer ID Application: ')
      || !options.identity.endsWith(`(${options.team})`)) throw new Error('Require a Developer ID Application identity matching the team');
  if (!/^[A-Z0-9]{10,}$/.test(options.keyId) || !/^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/i.test(options.issuer)) {
    throw new Error('Require an App Store Connect key ID and issuer UUID');
  }
  for (const name of ['inputDmg', 'keyPath']) {
    if (!fs.statSync(options[name]).isFile()) throw new Error(`Expected a file for ${name}`);
  }
  if (!options.inputDmg.endsWith('.dmg')) throw new Error('Expected a DMG input');
}

function onlyApp(directory) {
  const apps = fs.readdirSync(directory).filter((name) => name.endsWith('.app'));
  if (apps.length !== 1) throw new Error('Expected exactly one app bundle in the DMG');
  const app = path.join(directory, apps[0]);
  if (!fs.lstatSync(app).isDirectory()) throw new Error('The app bundle must be a directory, not a link');
  return app;
}

function inspectTree(app) {
  const root = fs.realpathSync(app);
  const files = [];
  const bundles = [];
  function visit(directory) {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const item = path.join(directory, entry.name);
      if (entry.isSymbolicLink()) {
        const resolved = fs.realpathSync(item);
        if (resolved !== root && !resolved.startsWith(`${root}${path.sep}`)) throw new Error(`App link escapes its bundle: ${path.relative(app, item)}`);
      } else if (entry.isDirectory()) {
        visit(item);
        if (/\.(app|framework|xpc|appex|bundle)$/.test(entry.name)) bundles.push(item);
      } else if (entry.isFile()) files.push(item);
      else throw new Error(`Unsupported app entry: ${path.relative(app, item)}`);
    }
  }
  visit(app);
  return { files, bundles };
}

function verifyBundleIdentity(app, options, run = command) {
  const plist = path.join(app, 'Contents', 'Info.plist');
  const get = (key) => run('/usr/libexec/PlistBuddy', ['-c', `Print :${key}`, plist]).trim();
  if (get('CFBundleShortVersionString') !== options.version) throw new Error('App version differs from the release');
  if (get('CFBundleIdentifier') !== options.bundleIdentifier) throw new Error('App identifier differs from the candidate');
  const executable = get('CFBundleExecutable');
  if (!executable || executable === '.' || executable === '..' || /[\/\\\r\n]/.test(executable)) throw new Error('Unsafe CFBundleExecutable');
  const binary = path.join(app, 'Contents', 'MacOS', executable);
  const actual = run('lipo', ['-archs', binary]).trim().split(/\s+/).sort();
  if (JSON.stringify(actual) !== JSON.stringify([...ARCHES[options.arch]].sort())) throw new Error(`App architecture differs from ${options.arch}`);
  inspectTree(app);
  return binary;
}

function isMachO(file, run) {
  const fd = fs.openSync(file, 'r');
  const header = Buffer.alloc(4);
  let size;
  try { size = fs.readSync(fd, header, 0, 4, 0); } finally { fs.closeSync(fd); }
  if (size !== 4 || !MACH_MAGICS.has(header.toString('hex'))) return false;
  // CAFEBABE also identifies Java bytecode; never mistake it for a fat binary.
  return /^Mach-O\b/.test(run('file', ['-b', file]).trim());
}

function signApp(app, options, work, run = command) {
  const { files, bundles } = inspectTree(app);
  const targets = [...files.filter((file) => isMachO(file, run)), ...bundles, app];
  for (let index = 0; index < targets.length; index += 1) {
    const target = targets[index];
    const args = ['--force', '--sign', options.identity, '--options', 'runtime', '--timestamp'];
    // Preserve entitlements actually present in the CI build, without copying
    // old ad-hoc requirements or adding relaxed signing permissions.
    let previous = '';
    try { previous = run('codesign', ['--display', '--entitlements', ':-', target]); } catch { /* Unsigned embedded code has no metadata to preserve. */ }
    const plist = previous.match(/(?:<\?xml[^>]*>\s*)?(?:<!DOCTYPE[^>]*>\s*)?<plist\b[\s\S]*?<\/plist>/)?.[0];
    if (plist) {
      const entitlementsFile = path.join(work, `entitlements-${index}.plist`);
      fs.writeFileSync(entitlementsFile, plist, { mode: 0o600 });
      const entitlements = JSON.parse(run('plutil', ['-convert', 'json', '-o', '-', entitlementsFile]));
      if (entitlements['com.apple.security.get-task-allow'] === true) throw new Error('Development get-task-allow entitlement is not allowed in a release');
      if (Object.keys(entitlements).length) args.push('--entitlements', entitlementsFile);
    }
    run('codesign', [...args, target]);
  }
  run('codesign', ['--verify', '--deep', '--strict', '--verbose=2', app]);
}

function submitNotarization(file, options, run = command, progress = () => {}) {
  progress({ phase: 'notarization-submit', arch: options.arch, message: `Submitting ${path.basename(file)} to Apple` });
  const submission = JSON.parse(run('xcrun', ['notarytool', 'submit', file,
    '--key', path.resolve(options.keyPath), '--key-id', options.keyId, '--issuer', options.issuer,
    '--wait', '--timeout', '30m', '--output-format', 'json'], { timeout: 31 * 60_000, stdoutOnly: true }));
  if (submission.status !== 'Accepted' || typeof submission.id !== 'string' || !submission.id.trim()) {
    throw new Error(`Apple notarization ${submission.status || 'unknown'}; submission ID: ${submission.id || 'unavailable'}`);
  }
  progress({ phase: 'notarization-accepted', arch: options.arch, submissionId: submission.id, message: `Apple accepted ${path.basename(file)}: ${submission.id}` });
  return { id: submission.id, status: submission.status };
}

function verifyDmg(dmg, team, run = command) {
  run('codesign', ['--verify', '--strict', dmg]);
  const info = run('codesign', ['--display', '--verbose=4', dmg]);
  if (!info.includes('Authority=Developer ID Application:') || !info.split(/\r?\n/).includes(`TeamIdentifier=${team}`)) {
    throw new Error('DMG requires the expected Developer ID team');
  }
  run('xcrun', ['stapler', 'validate', dmg]);
  run('spctl', ['--assess', '--type', 'open', '--context', 'context:primary-signature', '--verbose=2', dmg]);
}

async function signLocalDmg(options, deps = {}) {
  validateOptions(options);
  const run = deps.run || command;
  const progress = deps.onProgress || ((event) => console.log(`[${event.arch}] ${event.message}`));
  const mounts = deps.mounts || createDmgMounts();
  const outputDir = path.resolve(options.outputDir);
  fs.mkdirSync(outputDir, { recursive: true });
  const outputDmg = path.join(outputDir, path.basename(options.inputDmg));
  const outputArchive = options.arch === 'universal' ? null : path.join(outputDir, `Cockpit.Tools_${options.arch}.app.tar.gz`);
  for (const file of [outputDmg, outputArchive].filter(Boolean)) {
    if (fs.existsSync(file)) throw new Error(`Refusing to replace an existing output: ${file}`);
  }
  const identities = run('security', ['find-identity', '-v', '-p', 'codesigning']);
  if (!identities.includes(`"${options.identity}"`)) throw new Error('The requested Developer ID identity is not available in the local keychain');
  const work = fs.mkdtempSync(path.join(outputDir, '.macos-sign-'));
  fs.chmodSync(work, 0o700);
  const inputDigest = sha256(options.inputDmg);
  const removeHandlers = mounts.installExitHandlers();
  let completed = false;
  try {
    const sourceMount = path.join(work, 'source');
    fs.mkdirSync(sourceMount);
    const sourceRecord = mounts.attach(sourceMount, options.inputDmg, { readOnly: true });
    let app;
    try {
      const sourceApp = onlyApp(sourceMount);
      verifyBundleIdentity(sourceApp, options, run);
      run('codesign', ['--verify', '--deep', '--strict', sourceApp]);
      app = path.join(work, path.basename(sourceApp));
      run('ditto', [sourceApp, app]);
      verifyTree(sourceApp, app);
    } finally { mounts.detach(sourceRecord); }
    progress({ phase: 'sign-app', arch: options.arch, message: 'Signing app and embedded code with the local Developer ID' });
    verifyBundleIdentity(app, options, run);
    signApp(app, options, work, run);
    const zip = path.join(work, 'app-for-notarization.zip');
    run('ditto', ['-c', '-k', '--keepParent', app, zip]);
    const appSubmission = submitNotarization(zip, options, run, progress);
    run('xcrun', ['stapler', 'staple', app]);
    verifyApp(app, options.team, run);

    let archive = null;
    if (outputArchive) {
      archive = path.join(work, path.basename(outputArchive));
      run('tar', ['-czf', archive, '-C', work, path.basename(app)]);
      const extracted = path.join(work, 'verify-updater');
      fs.mkdirSync(extracted);
      run('tar', ['-xzf', archive, '-C', extracted]);
      const archivedApp = onlyApp(extracted);
      verifyTree(app, archivedApp);
      verifyBundleIdentity(archivedApp, options, run);
      verifyApp(archivedApp, options.team, run);
    }

    progress({ phase: 'package-dmg', arch: options.arch, message: 'Rebuilding the installer from the notarized app' });
    const dmg = path.join(work, path.basename(outputDmg));
    run(process.execPath, [path.join(__dirname, 'package-local-dmg.cjs'), '--app', app, '--output', dmg], { timeout: 5 * 60_000 });
    run('codesign', ['--force', '--sign', options.identity, '--timestamp', dmg]);
    const dmgSubmission = submitNotarization(dmg, options, run, progress);
    run('xcrun', ['stapler', 'staple', dmg]);
    verifyDmg(dmg, options.team, run);
    const verificationMount = path.join(work, 'verify-dmg');
    fs.mkdirSync(verificationMount);
    const verificationRecord = mounts.attach(verificationMount, dmg, { readOnly: true });
    try {
      const mountedApp = onlyApp(verificationMount);
      verifyTree(app, mountedApp);
      verifyBundleIdentity(mountedApp, options, run);
      verifyApp(mountedApp, options.team, run);
    } finally { mounts.detach(verificationRecord); }
    mounts.cleanup();
    if (sha256(options.inputDmg) !== inputDigest) throw new Error('The input DMG changed during signing');
    const receipt = {
      version: options.version, arch: options.arch, bundleIdentifier: options.bundleIdentifier,
      team: options.team, identity: options.identity, verifiedAt: new Date().toISOString(),
      input: { name: path.basename(options.inputDmg), sha256: inputDigest },
      appSubmission, dmgSubmission,
      outputs: [dmg, archive].filter(Boolean).map((file) => ({ name: path.basename(file), sha256: sha256(file) })),
    };
    fs.renameSync(dmg, outputDmg);
    if (archive) fs.renameSync(archive, outputArchive);
    completed = true;
    progress({ phase: 'complete', arch: options.arch, message: 'App, updater archive and installer passed Developer ID, Apple ticket and Gatekeeper checks' });
    return { dmg: outputDmg, archive: outputArchive, receipt };
  } catch (error) {
    error.message += `; signing work retained at ${work}`;
    throw error;
  } finally {
    // A failed detach must retain mounted image files, just like local packaging.
    mounts.cleanup();
    if (typeof removeHandlers === 'function') removeHandlers();
    if (completed) fs.rmSync(work, { recursive: true, force: true });
  }
}

function parseArgs(argv) {
  const result = {};
  for (let index = 0; index < argv.length; index += 2) {
    const name = argv[index];
    const value = argv[index + 1];
    if (!/^--[a-z-]+$/.test(name || '') || !value || value.startsWith('--')) throw new Error('Expected --name value arguments');
    result[name.slice(2).replace(/-([a-z])/g, (_, letter) => letter.toUpperCase())] = value;
  }
  return result;
}

if (require.main === module) {
  if (process.platform !== 'darwin') { console.error('Local Apple signing requires macOS'); process.exitCode = 1; }
  else signLocalDmg(parseArgs(process.argv.slice(2))).then((result) => console.log(JSON.stringify(result)))
    .catch((error) => { console.error(error.message); process.exitCode = 1; });
}

module.exports = { signLocalDmg, validateOptions, inspectTree, verifyBundleIdentity, signApp, submitNotarization, verifyDmg, sha256 };
