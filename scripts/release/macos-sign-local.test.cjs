const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');
const { spawnSync } = require('node:child_process');
const { signLocalDmg, validateOptions, inspectTree, verifyBundleIdentity, signApp, submitNotarization, verifyUpdaterArchiveLayout, sha256 } = require('./macos-sign-local.cjs');

const identity = 'Developer ID Application: Example (ABCDEFGHIJ)';
const validInfo = `Authority=${identity}\nTeamIdentifier=ABCDEFGHIJ\nCodeDirectory flags=0x10000(runtime)\n`;

function temp(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'cockpit-local-sign-test-'));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  return directory;
}

function makeApp(root) {
  const app = path.join(root, 'Cockpit Tools.app');
  fs.mkdirSync(path.join(app, 'Contents', 'MacOS'), { recursive: true });
  fs.writeFileSync(path.join(app, 'Contents', 'Info.plist'), 'test plist');
  fs.writeFileSync(path.join(app, 'Contents', 'MacOS', 'cockpit-tools'), Buffer.from('cffaedfe00000000', 'hex'));
  return app;
}

function fixture(t, overrides = {}) {
  const directory = temp(t);
  const source = makeApp(path.join(directory, 'fixture'));
  const options = {
    inputDmg: path.join(directory, 'Cockpit.Tools_1.3.79_aarch64.dmg'),
    outputDir: path.join(directory, 'signed'), version: '1.3.79', arch: 'aarch64',
    identity, team: 'ABCDEFGHIJ', keyPath: path.join(directory, 'test-key.p8'),
    keyId: '0123456789', issuer: '00000000-0000-0000-0000-000000000001',
    bundleIdentifier: 'com.example.app',
  };
  fs.writeFileSync(options.inputDmg, 'original dmg');
  fs.writeFileSync(options.keyPath, 'test fixture, not a key');
  const calls = [];
  const progress = [];
  const snapshots = new Map();
  let currentDmgApp;
  let detached = 0;
  let removeHandlers = 0;
  let submissions = 0;
  const run = (exe, args, commandOptions) => {
    calls.push([exe, [...args], commandOptions]);
    const intercept = overrides.run?.(exe, args, commandOptions);
    if (intercept !== undefined) return intercept;
    if (exe === 'security') return `1) test "${identity}"\n1 valid identities found`;
    if (exe === '/usr/libexec/PlistBuddy') {
      return ({ 'Print :CFBundleShortVersionString': options.version,
        'Print :CFBundleIdentifier': options.bundleIdentifier,
        'Print :CFBundleExecutable': 'cockpit-tools' })[args[1]];
    }
    if (exe === 'lipo') return options.arch === 'universal' ? 'arm64 x86_64' : options.arch === 'x64' ? 'x86_64' : 'arm64';
    if (exe === 'file') return 'Mach-O 64-bit executable arm64';
    if (exe === 'codesign') {
      if (args.includes('--entitlements')) return '';
      if (args[0] === '--display') return validInfo;
      if (args[0] === '--force' && args.at(-1).endsWith('.app')) {
        fs.writeFileSync(path.join(args.at(-1), 'Contents', 'signature'), 'Developer ID signature');
      }
      return '';
    }
    if (exe === 'ditto') {
      if (args.includes('--keepParent')) fs.writeFileSync(args.at(-1), 'notary archive');
      else fs.cpSync(args[0], args[1], { recursive: true });
      return '';
    }
    if (exe === 'tar') {
      if (args[0] === '-czf') {
        const snapshot = path.join(directory, `tar-snapshot-${snapshots.size}`);
        fs.cpSync(path.join(args[3], args[4]), path.join(snapshot, args[4]), { recursive: true });
        snapshots.set(args[1], snapshot);
        const result = spawnSync('tar', args, { encoding: 'utf8', env: commandOptions?.env });
        if (result.error || result.status !== 0) throw new Error(result.stderr || result.error?.message || 'tar failed');
      } else {
        fs.cpSync(snapshots.get(args[1]), args[3], { recursive: true });
      }
      return '';
    }
    if (exe === process.execPath) {
      currentDmgApp = args[2];
      fs.writeFileSync(args[4], 'dmg from stapled app');
      fs.writeFileSync(`${args[4]}.sha256`, 'old pre-signing checksum');
      return '';
    }
    if (exe === 'xcrun') {
      if (args[0] === 'notarytool') {
        submissions += 1;
        return JSON.stringify({ status: overrides.notaryStatus?.(submissions) || 'Accepted', id: `submission-${submissions}` });
      }
      if (args[0] === 'stapler' && args[1] === 'staple' && args[2].endsWith('.app')) {
        fs.writeFileSync(path.join(args[2], 'Contents', 'ticket'), 'accepted apple ticket');
      }
      return '';
    }
    if (exe === 'spctl') return 'accepted';
    throw new Error(`Unexpected mock command: ${exe}`);
  };
  const mounts = {
    installExitHandlers() { return () => { removeHandlers += 1; }; },
    attach(mountPoint, image, attachOptions) {
      assert.equal(attachOptions.readOnly, true);
      const app = image === options.inputDmg ? source : currentDmgApp;
      fs.cpSync(app, path.join(mountPoint, path.basename(app)), { recursive: true });
      return { mountPoint };
    },
    detach() { detached += 1; }, cleanup() {},
  };
  return { options, source, calls, progress, directory, run, mounts,
    deps: { run, mounts, onProgress: (event) => progress.push(event) },
    counts: () => ({ detached, removeHandlers, submissions }) };
}

test('invalid or mismatched Developer ID and missing API metadata fail before signing', (t) => {
  const f = fixture(t);
  for (const [change, message] of [
    [{ identity: 'Apple Development: Example (ABCDEFGHIJ)' }, /Developer ID/],
    [{ team: 'WRONGTEAM1' }, /matching/],
    [{ issuer: '' }, /issuer/],
    [{ version: '1.3.79+test' }, /stable/],
    [{ arch: 'unknown' }, /architecture/],
  ]) assert.throws(() => validateOptions({ ...f.options, ...change }), message);
});

test('bundle identity, version and architecture must match the tested candidate', (t) => {
  const f = fixture(t);
  assert.throws(() => verifyBundleIdentity(f.source, { ...f.options, version: '1.3.80' }, f.run), /version/);
  assert.throws(() => verifyBundleIdentity(f.source, { ...f.options, bundleIdentifier: 'another.app' }, f.run), /identifier/);
  assert.throws(() => verifyBundleIdentity(f.source, { ...f.options, arch: 'x64' }, f.run), /architecture/);
});

test('links outside an app are rejected before any target can be signed', (t) => {
  const f = fixture(t);
  fs.symlinkSync(f.options.keyPath, path.join(f.source, 'Contents', 'outside'));
  assert.throws(() => inspectTree(f.source), /escapes/);
});

test('signing proceeds inside-out and preserves existing release entitlements', (t) => {
  const directory = temp(t);
  const app = makeApp(directory);
  const nested = makeApp(path.join(app, 'Contents', 'Helpers'));
  const calls = [];
  const entitlementXml = '<?xml version="1.0"?><plist version="1.0"><dict><key>com.apple.security.cs.allow-jit</key><true/></dict></plist>';
  signApp(app, { identity }, directory, (exe, args) => {
    calls.push([exe, args]);
    if (exe === 'file') return 'Mach-O 64-bit executable arm64';
    if (exe === 'plutil') return JSON.stringify({ 'com.apple.security.cs.allow-jit': true });
    if (exe === 'codesign' && args[0] === '--display') return entitlementXml;
    return '';
  });
  const signed = calls.filter(([exe, args]) => exe === 'codesign' && args[0] === '--force').map(([, args]) => args);
  assert.equal(signed.at(-1).at(-1), app);
  assert.ok(signed.findIndex((args) => args.at(-1) === nested) < signed.length - 1);
  assert.ok(signed.every((args) => args.includes('--entitlements') && args.includes('runtime') && args.includes('--timestamp')));
});

test('development debugging entitlement cannot be silently shipped', (t) => {
  const directory = temp(t);
  const app = makeApp(directory);
  let signed = false;
  assert.throws(() => signApp(app, { identity }, directory, (exe, args) => {
    if (exe === 'file') return 'Mach-O 64-bit executable arm64';
    if (exe === 'codesign' && args[0] === '--display') return '<plist><dict><key>com.apple.security.get-task-allow</key><true/></dict></plist>';
    if (exe === 'plutil') return JSON.stringify({ 'com.apple.security.get-task-allow': true });
    if (exe === 'codesign' && args[0] === '--force') signed = true;
    return '';
  }), /get-task-allow/);
  assert.equal(signed, false);
});

test('rejected or unfinished Apple submissions never become accepted evidence', () => {
  for (const status of ['Invalid', 'In Progress', 'Rejected']) {
    assert.throws(() => submitNotarization('/tmp/app.zip', { keyPath: '/tmp/key.p8' },
      () => JSON.stringify({ status, id: 'submission-1' })), /submission-1/);
  }
});

test('the complete pipeline signs and notarizes both app and DMG before exposing outputs', async (t) => {
  const f = fixture(t);
  const inputHash = sha256(f.options.inputDmg);
  const result = await signLocalDmg(f.options, f.deps);
  assert.ok(fs.existsSync(result.dmg));
  assert.ok(fs.existsSync(result.archive));
  assert.equal(sha256(f.options.inputDmg), inputHash);
  assert.deepEqual(f.counts(), { detached: 2, removeHandlers: 1, submissions: 2 });
  assert.equal(result.receipt.appSubmission.status, 'Accepted');
  assert.equal(result.receipt.dmgSubmission.status, 'Accepted');
  assert.equal(result.receipt.outputs.length, 2);
  assert.ok(result.receipt.outputs.every((item) => !item.name.includes('/') && /^[a-f0-9]{64}$/.test(item.sha256)));
  assert.ok(!JSON.stringify(result.receipt).includes(f.directory));
  assert.ok(!JSON.stringify(f.progress).includes(f.options.keyPath));
  assert.ok(!fs.existsSync(`${result.dmg}.sha256`), 'must not expose the checksum made before DMG signing');
  const appStaple = f.calls.findIndex(([exe, args]) => exe === 'xcrun' && args[1] === 'staple' && args[2].endsWith('.app'));
  const archive = f.calls.findIndex(([exe, args]) => exe === 'tar' && args[0] === '-czf');
  assert.equal(f.calls[archive][2].env.COPYFILE_DISABLE, '1');
  const packageDmg = f.calls.findIndex(([exe]) => exe === process.execPath);
  assert.ok(appStaple < archive && archive < packageDmg);
  const submitCalls = f.calls.filter(([exe, args]) => exe === 'xcrun' && args[1] === 'submit');
  assert.ok(submitCalls.every(([, args, options]) => args.includes('30m') && options.timeout === 31 * 60_000));
});

test('updater archive rejects the AppleDouble entry that Tauri cannot install', (t) => {
  const directory = temp(t);
  makeApp(directory);
  fs.writeFileSync(path.join(directory, '._Cockpit Tools.app'), 'macOS metadata');
  const archive = path.join(directory, 'broken.tar.gz');
  const result = spawnSync('tar', ['-czf', archive, '-C', directory, '._Cockpit Tools.app', 'Cockpit Tools.app'], {
    encoding: 'utf8', env: { ...process.env, COPYFILE_DISABLE: '1' },
  });
  assert.equal(result.status, 0, result.stderr);
  assert.throws(() => verifyUpdaterArchiveLayout(archive, 'Cockpit Tools.app'), /Invalid updater archive entry/);
});

test('a rejected app or DMG keeps the existing input and exposes no release asset', async (t) => {
  for (const rejectedSubmission of [1, 2]) {
    const f = fixture(t, { notaryStatus: (number) => number === rejectedSubmission ? 'Invalid' : 'Accepted' });
    const inputHash = sha256(f.options.inputDmg);
    await assert.rejects(signLocalDmg(f.options, f.deps), /Invalid.*signing work retained/);
    assert.equal(sha256(f.options.inputDmg), inputHash);
    assert.ok(fs.readdirSync(f.options.outputDir).every((name) => name.startsWith('.macos-sign-')));
    const staples = f.calls.filter(([exe, args]) => exe === 'xcrun' && args[1] === 'staple');
    assert.equal(staples.length, rejectedSubmission - 1);
  }
});

test('Gatekeeper or ticket failure cannot produce a signed release output', async (t) => {
  const f = fixture(t, { run(exe, args) {
    if (exe === 'spctl') throw new Error('Gatekeeper rejected the app');
  } });
  await assert.rejects(signLocalDmg(f.options, f.deps), /Gatekeeper/);
  assert.equal(f.counts().submissions, 1);
  assert.ok(fs.readdirSync(f.options.outputDir).every((name) => name.startsWith('.macos-sign-')));
});

test('universal installer is notarized without adding an unused updater target', async (t) => {
  const f = fixture(t);
  f.options.arch = 'universal';
  const result = await signLocalDmg(f.options, f.deps);
  assert.equal(result.archive, null);
  assert.equal(result.receipt.outputs.length, 1);
  assert.ok(!f.calls.some(([exe]) => exe === 'tar'));
});
