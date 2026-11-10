const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const test = require('node:test');
const { finalizeFork, validateDraft, validateProvenance, verifyChecksums, writeChecksums, allowedName, parseArgs,
  assertTag, assertNewerRelease, validateSignedResult, findReleaseByTag } = require('./finalize_fork_release.cjs');

const source = 'a'.repeat(40);
const controller = 'b'.repeat(40);
const draft = { id: 12345, tag_name: 'fork-v1.3.79', draft: true, prerelease: false, target_commitish: source,
  assets: ['Fork-build.json', 'SHA256SUMS.txt', 'SHA256SUMS.txt.sig'].map(name => ({ name, size: 123, state: 'uploaded' })) };
const provenance = { schema: 1, repository: 'knobblex/cockpit-tools', source, controller,
  runId: '1234', runAttempt: '1', version: '1.3.79' };
const ci = { id: 1234, run_attempt: 1, repository: { full_name: 'knobblex/cockpit-tools' }, head_sha: controller,
  head_repository: { full_name: 'knobblex/cockpit-tools' }, head_branch: 'main', event: 'workflow_dispatch',
  status: 'completed', conclusion: 'success', path: '.github/workflows/fork-publish.yml' };

test('authenticated draft lookup uses an exact tag across paginated releases', () => {
  const calls = [];
  const unrelated = Array.from({ length: 100 }, (_, index) => ({ id: index + 1, tag_name: `another-${index}` }));
  const found = findReleaseByTag(draft.tag_name, endpoint => {
    calls.push(endpoint);
    if (endpoint === 'releases?per_page=100&page=1') return unrelated;
    if (endpoint === 'releases?per_page=100&page=2') return [draft];
    throw new Error('HTTP 404: public tags endpoint does not expose this draft');
  });
  assert.equal(found.id, draft.id);
  assert.equal(calls.length, 2);
  assert.throws(() => findReleaseByTag(draft.tag_name, () => [{ ...draft, tag_name: `${draft.tag_name}-other` }]), /not found/);
  assert.throws(() => findReleaseByTag(draft.tag_name, () => [draft, { ...draft, id: draft.id + 1 }]), /Multiple releases/);
});

test('published releases, wrong sources and incomplete drafts cannot enter signing', () => {
  assert.doesNotThrow(() => validateDraft(draft, draft.tag_name, source));
  for (const changes of [{ draft: false }, { target_commitish: controller }, { prerelease: true }, { assets: [] },
    { id: 0 },
    { assets: [...draft.assets, { name: '../escape.dmg', size: 1, state: 'uploaded' }] },
    { assets: [...draft.assets, { name: 'extra.exe', size: 0, state: 'uploaded' }] },
    { assets: [...draft.assets, draft.assets[0]] }]) {
    assert.throws(() => validateDraft({ ...draft, ...changes }, draft.tag_name, source));
  }
  assert.throws(() => validateDraft(draft, draft.tag_name, source, draft.id + 1), /replaced/);
});

test('only the exact successful trusted build run and attempt can be released', () => {
  assert.doesNotThrow(() => validateProvenance(provenance, ci, source, '1.3.79'));
  for (const changes of [{ conclusion: 'failure' }, { status: 'in_progress' }, { head_sha: source },
    { run_attempt: 2 }, { id: 99 }, { path: '.github/workflows/build-matrix.yml' },
    { head_branch: 'attacker-branch' }, { event: 'pull_request' },
    { head_repository: { full_name: 'attacker/cockpit-tools' } },
    { repository: { full_name: 'attacker/cockpit-tools' } }]) {
    assert.throws(() => validateProvenance(provenance, { ...ci, ...changes }, source, '1.3.79'));
  }
  for (const changes of [{ source: controller }, { version: '1.3.78' }, { runId: '../../x' }, { runAttempt: '0' }]) {
    assert.throws(() => validateProvenance({ ...provenance, ...changes }, ci, source, '1.3.79'));
  }
  assert.doesNotThrow(() => validateProvenance(provenance, { ...ci, event: 'schedule', path: '.github/workflows/upstream-sync.yml' }, source, '1.3.79'));
});

test('the actual tag commit and strictly newer public version are required', () => {
  assert.doesNotThrow(() => assertTag(draft.tag_name, source, () => ({ object: { type: 'commit', sha: source } })));
  const annotation = 'c'.repeat(40);
  assert.doesNotThrow(() => assertTag(draft.tag_name, source, endpoint => ({ object: endpoint.startsWith('git/tags/')
    ? { type: 'commit', sha: source } : { type: 'tag', sha: annotation } })));
  assert.throws(() => assertTag(draft.tag_name, source, () => ({ object: { type: 'commit', sha: controller } })), /tag/);
  assert.throws(() => assertTag(draft.tag_name, source, () => ({ object: { type: 'tag', sha: annotation } })), /tag/);
  assert.doesNotThrow(() => assertNewerRelease('1.3.79', { tag_name: 'fork-v1.3.78', draft: false, prerelease: false }));
  for (const tag of ['fork-v1.3.79', 'fork-v1.3.80', 'fork-v1.4.0', 'v1.3.78']) {
    assert.throws(() => assertNewerRelease('1.3.79', { tag_name: tag, draft: false, prerelease: false }));
  }
});

test('checksums reject tampered, missing, extra and duplicated content', t => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'fork-checksums-'));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const asset = path.join(directory, 'Cockpit.Tools_1.3.79_aarch64.dmg');
  fs.writeFileSync(asset, 'original');
  fs.writeFileSync(path.join(directory, 'SHA256SUMS.txt.sig'), 'detached inventory signature');
  writeChecksums(directory);
  const sumsPath = path.join(directory, 'SHA256SUMS.txt');
  const sums = fs.readFileSync(sumsPath, 'utf8');
  assert.equal(verifyChecksums(directory).size, 1);
  assert.ok(!sums.includes('SHA256SUMS.txt.sig'));
  fs.writeFileSync(asset, 'changed');
  assert.throws(() => verifyChecksums(directory), /mismatch/);
  fs.writeFileSync(asset, 'original');
  fs.writeFileSync(sumsPath, sums + sums);
  assert.throws(() => verifyChecksums(directory), /duplicate/);
  fs.writeFileSync(sumsPath, sums);
  fs.writeFileSync(path.join(directory, 'latest.json'), '{}');
  assert.throws(() => verifyChecksums(directory), /every asset/);
  fs.unlinkSync(path.join(directory, 'latest.json'));
  fs.unlinkSync(asset);
  assert.throws(() => verifyChecksums(directory), /every asset/);
});

test('asset inventory forbids paths, shell metacharacters and unrelated payloads', () => {
  for (const name of ['../test.dmg', 'path/test.dmg', 'bad;touch.dmg', 'secret.key', 'script.js', 'latest-unknown.json']) {
    assert.equal(allowedName(name), false);
  }
  assert.equal(allowedName('Apple-notarization.json'), true);
  assert.equal(allowedName('latest-darwin-aarch64-app.json'), true);
});

test('publication is opt-in and incomplete or unknown options fail', () => {
  assert.equal(parseArgs(['--tag', 'fork-v1.3.79']).publish, undefined);
  assert.equal(parseArgs(['--tag', 'fork-v1.3.79', '--publish']).publish, true);
  assert.throws(() => parseArgs(['--source-sha']));
  assert.throws(() => parseArgs(['--password', 'secret']));
});

test('the fork publisher leaves Apple signing to a separate task and keeps the release as a draft', () => {
  const workflow = fs.readFileSync(path.join(__dirname, '../../.github/workflows/fork-publish.yml'), 'utf8');
  assert.match(workflow, /Fork-build\.json/);
  assert.match(workflow, /Await GitHub Apple signing and notarization/);
  assert.doesNotMatch(workflow, /--draft=false|secrets\.APPLE_|secrets: inherit/);
});

const hash = file => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const fakeSign = file => fs.writeFileSync(`${file}.sig`, Buffer.from(`signed:${hash(file)}`).toString('base64'));

function finalizerFixture(t, overrides = {}) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'fork-finalizer-'));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const remote = path.join(directory, 'remote-draft');
  fs.mkdirSync(remote);
  const names = [
    'Cockpit.Tools_aarch64.app.tar.gz', 'Cockpit.Tools_x64.app.tar.gz',
    'Cockpit.Tools_1.3.79_x64_en-US.msi', 'Cockpit.Tools_1.3.79_x64-setup.exe',
    'Cockpit.Tools_1.3.79_amd64.AppImage', 'Cockpit.Tools_1.3.79_amd64.deb', 'Cockpit.Tools-1.3.79-1.x86_64.rpm',
    'Cockpit.Tools_1.3.79_aarch64.AppImage', 'Cockpit.Tools_1.3.79_arm64.deb', 'Cockpit.Tools-1.3.79-1.aarch64.rpm',
  ];
  for (const name of names) { const file = path.join(remote, name); fs.writeFileSync(file, `built ${name}`); fakeSign(file); }
  for (const arch of ['aarch64', 'x64', 'universal']) fs.writeFileSync(path.join(remote, `Cockpit.Tools_1.3.79_${arch}.dmg`), `built ${arch} dmg`);
  fs.writeFileSync(path.join(remote, 'Fork-build.json'), JSON.stringify(provenance));
  writeChecksums(remote);
  fakeSign(path.join(remote, 'SHA256SUMS.txt'));
  const config = { version: '1.3.79', identifier: 'com.example.cockpit', plugins: { updater: {
    pubkey: Buffer.from('test public key').toString('base64'),
    endpoints: ['https://github.com/knobblex/cockpit-tools/releases/latest/download/latest-{{target}}.json',
      'https://github.com/knobblex/cockpit-tools/releases/latest/download/latest.json'],
  } } };
  const options = { tag: draft.tag_name, sourceSha: source, workDir: path.join(directory, 'work'),
    identity: 'Developer ID Application: Example (ABCDEFGHIJ)', team: 'ABCDEFGHIJ',
    keyPath: path.join(directory, 'test.p8'), keyId: '0123456789', issuer: '00000000-0000-0000-0000-000000000001',
    updaterKey: path.join(directory, 'updater.key'), publish: true };
  let uploaded = false;
  let published = false;
  let publicVerified = false;
  const signedArchs = [];
  const commandCalls = [];
  const apiCalls = [];
  const release = () => ({ ...draft, assets: fs.readdirSync(remote).map(name => ({ name,
    state: 'uploaded', size: fs.statSync(path.join(remote, name)).size,
    digest: `sha256:${hash(path.join(remote, name))}` })) });
  const state = () => ({ uploaded, published, publicVerified, signedArchs, remote });
  const api = endpoint => {
    apiCalls.push(endpoint);
    const result = overrides.api?.(endpoint, state());
    if (result !== undefined) return result;
    if (endpoint.startsWith('releases/tags/')) throw new Error('HTTP 404: draft is not exposed by the public tag endpoint');
    if (endpoint === 'releases?per_page=100&page=1') return [release()];
    if (endpoint === `releases/${draft.id}`) return release();
    if (endpoint === 'releases/latest') return { tag_name: 'fork-v1.3.78', draft: false, prerelease: false };
    if (endpoint.startsWith('git/ref/')) return { object: { type: 'commit', sha: source } };
    if (endpoint === 'actions/runs/1234') return ci;
    throw new Error(`Unexpected mock API ${endpoint}`);
  };
  const run = (exe, args) => {
    commandCalls.push([exe, args]);
    const intercepted = overrides.run?.(exe, args, state());
    if (intercepted !== undefined) return intercepted;
    if (exe === 'git') return JSON.stringify(args[1].endsWith(':package.json') ? { version: '1.3.79' } : config);
    if (exe === 'minisign') {
      const file = args[args.indexOf('-Vm') + 1];
      const signature = fs.readFileSync(args[args.indexOf('-x') + 1], 'utf8');
      if (signature !== `signed:${hash(file)}`) throw new Error('Signature mismatch');
      return '';
    }
    if (exe === 'gh' && args[1] === 'download') {
      const destination = args[args.indexOf('--dir') + 1];
      fs.cpSync(remote, destination, { recursive: true });
      overrides.afterDownload?.(destination);
      return '';
    }
    if (exe === 'gh' && args[0] === 'api') {
      assert.match(args[5], /^body=@/);
      const expected = ['api', '--method', 'PATCH', `repos/knobblex/cockpit-tools/releases/${draft.id}`,
        '-F', args[5], '-f', `tag_name=${draft.tag_name}`];
      if (!options.uploadDraft) expected.push('-F', 'draft=false', '-F', 'prerelease=false', '-f', 'make_latest=true');
      assert.deepEqual(args, expected, 'metadata updates must preserve the release tag explicitly');
      const body = fs.readFileSync(args[5].slice('body=@'.length), 'utf8');
      assert.ok(body.includes(source));
      assert.equal(uploaded, true);
      if (options.uploadDraft) return JSON.stringify({ ...release(), body });
      published = true;
      return '';
    }
    if (exe === process.execPath && args[1] === 'signer') {
      assert.ok(path.extname(args.at(-1)), 'Tauri signer requires a file extension, including for the key probe');
      fakeSign(args.at(-1));
      return '';
    }
    if (exe === process.execPath && args[0].endsWith('build_merged_latest_json.cjs')) {
      fs.writeFileSync(args[args.indexOf('--output') + 1], JSON.stringify({ version: '1.3.79', platforms: {} }));
      return '';
    }
    if (exe === process.execPath && args[0].endsWith('verify_published_updater_manifests.cjs')) {
      assert.equal(published, true);
      assert.ok(args.includes('--legacy'));
      publicVerified = true;
      return '';
    }
    throw new Error(`Unexpected mock command ${exe}`);
  };
  const signMac = async signOptions => {
    overrides.beforeSign?.(signOptions, state());
    signedArchs.push(signOptions.arch);
    fs.mkdirSync(signOptions.outputDir);
    const dmg = path.join(signOptions.outputDir, path.basename(signOptions.inputDmg));
    fs.writeFileSync(dmg, `signed and notarized ${signOptions.arch}`);
    const archive = signOptions.arch === 'universal' ? null : path.join(signOptions.outputDir, `Cockpit.Tools_${signOptions.arch}.app.tar.gz`);
    if (archive) fs.writeFileSync(archive, `signed updater ${signOptions.arch}`);
    const signed = { dmg, archive, receipt: {
      version: signOptions.version, arch: signOptions.arch, identity: signOptions.identity, team: signOptions.team,
      bundleIdentifier: signOptions.bundleIdentifier,
      input: { name: path.basename(signOptions.inputDmg), sha256: hash(signOptions.inputDmg) },
      appSubmission: { status: 'Accepted', id: `app-${signOptions.arch}` },
      dmgSubmission: { status: 'Accepted', id: `dmg-${signOptions.arch}` },
      outputs: [dmg, archive].filter(Boolean).map(file => ({ name: path.basename(file), sha256: hash(file) })),
    } };
    return overrides.signedResult?.(signed, signOptions) || signed;
  };
  const uploadReleaseAssets = async (_tag, files) => {
    uploaded = true;
    for (const file of files) fs.copyFileSync(file, path.join(remote, path.basename(file)));
    overrides.afterUpload?.(remote);
  };
  return { options, state, commandCalls, apiCalls, remote,
    deps: { platform: 'darwin', run, api, signMac, uploadReleaseAssets } };
}

test('the complete finalizer verifies all three Mac outputs and uploads before public verification', async t => {
  const f = finalizerFixture(t);
  const result = await finalizeFork(f.options, f.deps);
  assert.equal(result.published, true);
  assert.deepEqual(f.state().signedArchs, ['aarch64', 'x64', 'universal']);
  assert.equal(f.state().uploaded, true);
  assert.equal(f.state().publicVerified, true);
  assert.equal(f.apiCalls.filter(endpoint => endpoint === `releases/${draft.id}`).length, 3);
  assert.ok(!f.apiCalls.some(endpoint => endpoint.startsWith('releases/tags/')));
  assert.ok(!f.commandCalls.some(([exe, args]) => exe === 'gh' && args[0] === 'release' && args[1] === 'edit'));
  assert.equal(f.commandCalls.filter(([exe, args]) => exe === 'gh' && args[0] === 'api' && args[2] === 'PATCH').length, 1);
  const evidence = JSON.parse(fs.readFileSync(path.join(f.remote, 'Apple-notarization.json')));
  assert.equal(evidence.builds.length, 3);
  verifyChecksums(f.remote);
});

test('signed draft notes preserve the original tag and asset inventory without publication', async t => {
  const f = finalizerFixture(t);
  f.options.publish = false;
  f.options.uploadDraft = true;
  const result = await finalizeFork(f.options, f.deps);
  assert.equal(result.uploaded, true);
  assert.equal(result.published, false);
  assert.equal(f.state().publicVerified, false);
  assert.deepEqual(f.state().signedArchs, ['aarch64', 'x64', 'universal']);
  verifyChecksums(f.remote);
});

test('pinned draft rechecks reject a different tag, replacement ID or already-public release', async t => {
  for (const changes of [{ tag_name: 'fork-v1.3.80' }, { id: draft.id + 1 }, { draft: false }]) {
    const f = finalizerFixture(t, { api(endpoint) {
      if (endpoint === `releases/${draft.id}`) return { ...draft, ...changes };
    } });
    await assert.rejects(finalizeFork(f.options, f.deps), /unpublished draft|replaced/);
    assert.equal(f.state().uploaded, false);
    assert.equal(f.state().published, false);
  }
});

test('an unsigned or altered inventory fails before any private key use or Apple submission', async t => {
  const f = finalizerFixture(t, { afterDownload(directory) {
    fs.appendFileSync(path.join(directory, 'SHA256SUMS.txt'), '\n');
  } });
  await assert.rejects(finalizeFork(f.options, f.deps), /Signature mismatch/);
  assert.deepEqual(f.state().signedArchs, []);
  assert.equal(f.state().uploaded, false);
  assert.ok(!f.commandCalls.some(([, args]) => args[1] === 'signer'));
});

test('wrong-architecture paths or forged Apple receipts stop before publication', async t => {
  for (const change of [
    (signed) => { signed.receipt.arch = 'another-arch'; return signed; },
    (signed) => { signed.receipt.dmgSubmission.status = 'In Progress'; return signed; },
    (signed) => { signed.receipt.outputs[0].sha256 = '0'.repeat(64); return signed; },
    (signed) => { const outside = path.join(path.dirname(path.dirname(signed.dmg)), path.basename(signed.dmg)); fs.copyFileSync(signed.dmg, outside); signed.dmg = outside; return signed; },
  ]) {
    const f = finalizerFixture(t, { signedResult: change });
    await assert.rejects(finalizeFork(f.options, f.deps), /receipt|submission|architecture/);
    assert.equal(f.state().uploaded, false);
    assert.equal(f.state().published, false);
  }
});

test('source changes and corrupt remote uploads never expose the draft', async t => {
  const changedSource = finalizerFixture(t, { api(endpoint, state) {
    if (endpoint === 'git/ref/heads/codex/local-customizations' && state.signedArchs.length === 3) return { object: { sha: controller } };
  } });
  await assert.rejects(finalizeFork(changedSource.options, changedSource.deps), /branch changed/);
  assert.equal(changedSource.state().published, false);
  assert.equal(changedSource.state().uploaded, false);
  const corrupt = finalizerFixture(t, { afterUpload(directory) {
    const file = path.join(directory, 'Cockpit.Tools_1.3.79_aarch64.dmg');
    const bytes = fs.readFileSync(file); bytes[0] ^= 1; fs.writeFileSync(file, bytes);
  } });
  await assert.rejects(finalizeFork(corrupt.options, corrupt.deps), /digest mismatch/);
  assert.equal(corrupt.state().published, false);
});
