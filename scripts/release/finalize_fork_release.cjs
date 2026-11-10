#!/usr/bin/env node
// Use Apple credentials on the signing Mac; upload only verified artifacts.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { spawnSync } = require('node:child_process');
const { buildTargetManifests, TARGET_SPECS } = require('./build_target_latest_json.cjs');
const { uploadReleaseAssets } = require('./upload_release_assets.cjs');
const { isAllowedReleaseAsset } = require('./stage_release_assets.cjs');

const REPO = 'knobblex/cockpit-tools';
const ROOT = path.resolve(__dirname, '../..');
const ARCHES = ['aarch64', 'x64', 'universal'];
const TARGETS = Object.keys(TARGET_SPECS);

function run(executable, args, options = {}) {
  const result = spawnSync(executable, args, {
    cwd: ROOT, encoding: 'utf8', timeout: 120_000, maxBuffer: 4 * 1024 * 1024,
    env: { ...process.env, GH_REPO: REPO }, ...options,
  });
  if (result.error || result.status !== 0) {
    // Never print arguments: callers may supply credential file paths.
    throw new Error(`${executable} failed: ${(result.stderr || result.error?.message || `exit ${result.status}`).trim()}`);
  }
  return (result.stdout || '').trim();
}

function ghJson(endpoint) { return JSON.parse(run('gh', ['api', `repos/${REPO}/${endpoint}`])); }
function findReleaseByTag(tag, api = ghJson) {
  // The public tag endpoint can return 404 for an authenticated draft. The
  // authenticated release list includes drafts; retain its immutable ID.
  let match;
  for (let page = 1; page <= 100; page += 1) {
    const releases = api(`releases?per_page=100&page=${page}`);
    if (!Array.isArray(releases)) throw new Error('Expected a release list from GitHub');
    for (const release of releases) {
      if (release.tag_name !== tag) continue;
      if (match) throw new Error(`Multiple releases match ${tag}; refusing an ambiguous draft`);
      match = release;
    }
    if (releases.length < 100) {
      if (!match) throw new Error(`Release ${tag} was not found in the authenticated release list`);
      return match;
    }
  }
  throw new Error('Release lookup exceeded its pagination limit');
}
function sha256(file) { return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex'); }
function filesIn(directory) {
  return fs.readdirSync(directory).sort().map(name => {
    const file = path.join(directory, name);
    if (!fs.lstatSync(file).isFile()) throw new Error(`Unexpected non-file asset: ${name}`);
    return { name, file, size: fs.statSync(file).size };
  });
}
function allowedName(name) {
  return typeof name === 'string' && /^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(name) && (
    ['windows', 'linux', 'macos'].some(platform => isAllowedReleaseAsset(platform, name))
    || ['SHA256SUMS.txt', 'SHA256SUMS.txt.sig', 'Fork-build.json', 'Apple-notarization.json', 'latest.json', ...TARGETS.map(target => `latest-${target}.json`)].includes(name)
  );
}

function validateDraft(release, tag, source, expectedId) {
  if (release.tag_name !== tag || !release.draft || release.prerelease || release.target_commitish !== source) {
    throw new Error('Expected an unpublished draft for exactly the requested source commit');
  }
  if (!Number.isSafeInteger(release.id) || release.id < 1 || (expectedId !== undefined && release.id !== expectedId)) {
    throw new Error('The release was replaced or has an invalid identity');
  }
  const names = new Set();
  for (const asset of release.assets || []) {
    if (!allowedName(asset.name) || names.has(asset.name) || asset.state !== 'uploaded' || !(asset.size > 0)) {
      throw new Error(`Invalid, incomplete or duplicate draft asset: ${asset.name}`);
    }
    names.add(asset.name);
  }
  for (const name of ['Fork-build.json', 'SHA256SUMS.txt', 'SHA256SUMS.txt.sig']) {
    if (!names.has(name)) throw new Error(`Draft is missing ${name}`);
  }
}

function verifyChecksums(directory) {
  const lines = fs.readFileSync(path.join(directory, 'SHA256SUMS.txt'), 'utf8').trim().split(/\r?\n/);
  const hashes = new Map();
  for (const line of lines) {
    const match = /^([a-f0-9]{64})  ([A-Za-z0-9][A-Za-z0-9._-]*)$/.exec(line);
    if (!match || !allowedName(match[2]) || ['SHA256SUMS.txt', 'SHA256SUMS.txt.sig'].includes(match[2]) || hashes.has(match[2])) {
      throw new Error('Invalid or duplicate checksum entry');
    }
    hashes.set(match[2], match[1]);
  }
  const files = filesIn(directory).filter(({ name }) => !['SHA256SUMS.txt', 'SHA256SUMS.txt.sig'].includes(name));
  if (hashes.size !== files.length) throw new Error('Checksums must cover every asset exactly once');
  for (const { name, file } of files) {
    if (hashes.get(name) !== sha256(file)) throw new Error(`Checksum mismatch: ${name}`);
  }
  return hashes;
}

function validateProvenance(provenance, ci, source, version) {
  if (provenance.schema !== 1 || provenance.repository !== REPO || provenance.source !== source
      || provenance.version !== version || !/^[a-f0-9]{40}$/.test(provenance.controller || '')
      || !/^[1-9]\d*$/.test(String(provenance.runId || '')) || !/^[1-9]\d*$/.test(String(provenance.runAttempt || ''))) {
    throw new Error('Build provenance does not match this release');
  }
  if (String(ci.id) !== String(provenance.runId) || String(ci.run_attempt) !== String(provenance.runAttempt)
      || ci.repository?.full_name !== REPO || ci.head_sha !== provenance.controller
      || ci.head_repository?.full_name !== REPO || ci.head_branch !== 'main'
      || !['workflow_dispatch', 'schedule'].includes(ci.event)
      || ci.status !== 'completed' || ci.conclusion !== 'success'
      || !['.github/workflows/fork-publish.yml', '.github/workflows/upstream-sync.yml'].includes(ci.path)) {
    throw new Error('Build run must be completed successfully by the recorded trusted controller');
  }
}

function writeChecksums(directory) {
  const lines = filesIn(directory).filter(({ name }) => !['SHA256SUMS.txt', 'SHA256SUMS.txt.sig'].includes(name))
    .map(({ name, file }) => `${sha256(file)}  ${name}`);
  fs.writeFileSync(path.join(directory, 'SHA256SUMS.txt'), `${lines.join('\n')}\n`);
  verifyChecksums(directory);
}

function verifyUpdater(file, publicKey, workDir, commandRun = run) {
  const signature = path.join(workDir, 'verify.minisig');
  fs.writeFileSync(signature, Buffer.from(fs.readFileSync(`${file}.sig`, 'utf8').trim(), 'base64'));
  commandRun('minisign', ['-Vm', file, '-p', publicKey, '-x', signature, '-q']);
}

function signUpdater(file, privateKey, publicKey, workDir, commandRun = run, tauriCli = path.join(ROOT, 'node_modules/@tauri-apps/cli/tauri.js')) {
  commandRun(process.execPath, [tauriCli, 'signer', 'sign', '--private-key-path', privateKey, file], {
    env: { ...process.env, GH_REPO: REPO, TAURI_PRIVATE_KEY_PASSWORD: '' },
  });
  verifyUpdater(file, publicKey, workDir, commandRun);
}

function matchUpdaterFiles(directory) {
  const files = filesIn(directory);
  return TARGETS.map(target => {
    const matches = files.filter(({ name }) => TARGET_SPECS[target].test(name));
    if (matches.length !== 1) throw new Error(`Expected exactly one updater asset for ${target}`);
    return matches[0];
  });
}

function assertSource(source, api = ghJson) {
  const current = api('git/ref/heads/codex/local-customizations');
  if (current.object?.sha !== source) throw new Error('Customization branch changed; refusing publication');
}

function assertTag(tag, source, api = ghJson) {
  let object = api(`git/ref/tags/${encodeURIComponent(tag)}`).object;
  for (let depth = 0; object?.type === 'tag' && depth < 4; depth += 1) {
    if (!/^[a-f0-9]{40}$/.test(object.sha || '')) throw new Error('Invalid annotated release tag');
    object = api(`git/tags/${object.sha}`).object;
  }
  if (object?.type !== 'commit' || object.sha !== source) throw new Error('Release tag does not point to the tested source commit');
}

function assertNewerRelease(version, latest) {
  if (!latest) return;
  const match = /^fork-v((0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*))$/.exec(latest.tag_name || '');
  if (!match || latest.draft || latest.prerelease) throw new Error('Latest release is not a recognized stable Fork version');
  const candidate = version.split('.').map(BigInt);
  const current = match[1].split('.').map(BigInt);
  for (let index = 0; index < 3; index += 1) {
    if (candidate[index] > current[index]) return;
    if (candidate[index] < current[index]) break;
  }
  throw new Error('A newer or equal Fork version has already been published');
}

function validateSignedResult(signed, expected) {
  const outputDir = fs.realpathSync(expected.outputDir);
  const expectedNames = [`Cockpit.Tools_${expected.version}_${expected.arch}.dmg`];
  if (expected.arch !== 'universal') expectedNames.push(`Cockpit.Tools_${expected.arch}.app.tar.gz`);
  const outputs = [signed.dmg, signed.archive].filter(Boolean);
  if (outputs.length !== expectedNames.length || (expected.arch === 'universal' && signed.archive !== null)) {
    throw new Error(`Incomplete signed outputs for ${expected.arch}`);
  }
  for (let index = 0; index < outputs.length; index += 1) {
    const file = outputs[index];
    if (typeof file !== 'string' || path.basename(file) !== expectedNames[index]
        || !fs.lstatSync(file).isFile() || fs.realpathSync(path.dirname(file)) !== outputDir
        || fs.statSync(file).size === 0) throw new Error(`Signed output escaped its architecture directory: ${expected.arch}`);
  }
  const receipt = signed.receipt;
  if (!receipt || receipt.version !== expected.version || receipt.arch !== expected.arch
      || receipt.team !== expected.team || receipt.identity !== expected.identity
      || receipt.bundleIdentifier !== expected.bundleIdentifier
      || receipt.input?.name !== path.basename(expected.inputDmg)
      || receipt.input?.sha256 !== sha256(expected.inputDmg)
      || !Array.isArray(receipt.outputs) || receipt.outputs.length !== outputs.length) {
    throw new Error(`Apple receipt does not match the source for ${expected.arch}`);
  }
  for (const submission of [receipt.appSubmission, receipt.dmgSubmission]) {
    if (submission?.status !== 'Accepted' || typeof submission.id !== 'string' || !submission.id.trim()) {
      throw new Error(`Missing accepted Apple submission for ${expected.arch}`);
    }
  }
  for (let index = 0; index < outputs.length; index += 1) {
    if (receipt.outputs[index]?.name !== expectedNames[index] || receipt.outputs[index]?.sha256 !== sha256(outputs[index])) {
      throw new Error(`Apple receipt output mismatch for ${expected.arch}`);
    }
  }
}

function validateRequestedOptions(options) {
  if (options.publish && options.uploadDraft) throw new Error('--publish and --upload-draft cannot be combined');
  for (const name of ['expectedRunId', 'expectedRunAttempt']) {
    if (options[name] !== undefined && (typeof options[name] !== 'string' || !/^[1-9]\d*$/.test(options[name]))) {
      throw new Error(`${name} must be a positive decimal integer`);
    }
  }
  if (options.expectedControllerSha !== undefined
      && (typeof options.expectedControllerSha !== 'string' || !/^[a-f0-9]{40}$/.test(options.expectedControllerSha))) {
    throw new Error('expectedControllerSha must be a full lowercase commit SHA');
  }
  if (options.tauriCli !== undefined && (typeof options.tauriCli !== 'string' || !options.tauriCli.trim())) {
    throw new Error('tauriCli must be a path to the Tauri CLI');
  }
}

async function finalizeFork(options, dependencies = {}) {
  validateRequestedOptions(options);
  if ((dependencies.platform || process.platform) !== 'darwin') throw new Error('Run the finalizer on the signing Mac');
  const commandRun = dependencies.run || run;
  const api = dependencies.api || ghJson;
  const { tag, sourceSha: source, workDir, identity, team, keyPath, keyId, issuer, updaterKey } = options;
  const tauriCli = options.tauriCli ? path.resolve(options.tauriCli) : path.join(ROOT, 'node_modules/@tauri-apps/cli/tauri.js');
  const version = /^fork-v((0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*))$/.exec(tag || '')?.[1];
  if (!version || !/^[a-f0-9]{40}$/.test(source || '')) throw new Error('A stable Fork tag and full source SHA are required');
  for (const [name, value] of Object.entries({ workDir, identity, team, keyPath, keyId, issuer, updaterKey })) {
    if (!value) throw new Error(`Missing ${name}`);
  }
  // Parse version/config at the immutable source; never execute its code from an archive.
  const pkg = JSON.parse(commandRun('git', ['show', `${source}:package.json`]));
  const config = JSON.parse(commandRun('git', ['show', `${source}:src-tauri/tauri.conf.json`]));
  if (pkg.version !== version || config.version !== version || !config.plugins?.updater?.pubkey) {
    throw new Error('Source package, Tauri configuration and release version must agree');
  }
  const expectedEndpoints = [
    `https://github.com/${REPO}/releases/latest/download/latest-{{target}}.json`,
    `https://github.com/${REPO}/releases/latest/download/latest.json`,
  ];
  if (JSON.stringify(config.plugins.updater.endpoints) !== JSON.stringify(expectedEndpoints)) throw new Error('Unexpected updater endpoints');
  const release = findReleaseByTag(tag, api);
  validateDraft(release, tag, source);
  const releaseEndpoint = `releases/${release.id}`;
  assertSource(source, api);
  assertTag(tag, source, api);
  assertNewerRelease(version, api('releases/latest'));
  fs.mkdirSync(path.resolve(workDir), { recursive: true });
  const attempt = fs.mkdtempSync(path.join(path.resolve(workDir), 'attempt-'));
  const downloaded = path.join(attempt, 'downloaded');
  const assets = path.join(attempt, 'release-assets');
  fs.mkdirSync(downloaded);
  fs.mkdirSync(assets);
  console.log(`Downloading verified draft ${tag} into ${attempt}`);
  commandRun('gh', ['release', 'download', tag, '--repo', REPO, '--dir', downloaded], { timeout: 20 * 60_000 });
  const publicKey = path.join(attempt, 'updater.pub');
  fs.writeFileSync(publicKey, Buffer.from(config.plugins.updater.pubkey, 'base64'));
  // The controller signs the complete checksum list, binding provenance and
  // otherwise unsigned DMGs to the same trusted release as updater archives.
  verifyUpdater(path.join(downloaded, 'SHA256SUMS.txt'), publicKey, attempt, commandRun);
  verifyChecksums(downloaded);
  const inventory = filesIn(downloaded);
  if (inventory.length !== release.assets.length || inventory.some(local => !release.assets.some(remote => remote.name === local.name && remote.size === local.size))) {
    throw new Error('Downloaded asset set differs from the inspected draft');
  }
  const original = new Map(filesIn(downloaded).map(({ name, file }) => [name, sha256(file)]));
  const provenance = JSON.parse(fs.readFileSync(path.join(downloaded, 'Fork-build.json'), 'utf8'));
  if (!/^[1-9]\d*$/.test(String(provenance.runId || ''))) throw new Error('Build provenance has an invalid run ID');
  for (const [option, value] of Object.entries({ expectedRunId: provenance.runId,
    expectedRunAttempt: provenance.runAttempt, expectedControllerSha: provenance.controller })) {
    if (options[option] !== undefined && options[option] !== String(value)) {
      throw new Error(`Build provenance does not match ${option}`);
    }
  }
  const ci = api(`actions/runs/${provenance.runId}`);
  validateProvenance(provenance, ci, source, version);
  const controllerConfig = JSON.parse(commandRun('git', ['show', `${provenance.controller}:src-tauri/tauri.conf.json`]));
  if (JSON.stringify(config.plugins.updater) !== JSON.stringify(controllerConfig.plugins?.updater)) {
    throw new Error('Candidate updater identity differs from the trusted build controller');
  }
  for (const { file } of matchUpdaterFiles(downloaded)) verifyUpdater(file, publicKey, attempt, commandRun);
  // Verify the local updater key before any Apple submission.
  const probe = path.join(attempt, 'updater-key-check.txt');
  fs.writeFileSync(probe, `${tag}\n${source}\n`);
  signUpdater(probe, path.resolve(updaterKey), publicKey, attempt, commandRun, tauriCli);
  for (const { name, file } of filesIn(downloaded)) fs.copyFileSync(file, path.join(assets, name));

  const signMac = dependencies.signMac || require('./macos-sign-local.cjs').signLocalDmg;
  const receipts = [];
  for (const arch of ARCHES) {
    const inputDmg = path.join(downloaded, `Cockpit.Tools_${version}_${arch}.dmg`);
    if (!fs.existsSync(inputDmg)) throw new Error(`Missing ${arch} DMG`);
    const signOptions = { inputDmg, outputDir: path.join(attempt, `signed-${arch}`),
      version, arch, identity, team, keyPath: path.resolve(keyPath), keyId, issuer,
      bundleIdentifier: config.identifier };
    const signed = await signMac(signOptions);
    validateSignedResult(signed, signOptions);
    fs.writeFileSync(path.join(attempt, `apple-receipt-${arch}.json`), `${JSON.stringify(signed.receipt, null, 2)}\n`);
    fs.copyFileSync(signed.dmg, path.join(assets, path.basename(signed.dmg)));
    if (signed.archive) {
      const archive = path.join(assets, path.basename(signed.archive));
      fs.copyFileSync(signed.archive, archive);
      signUpdater(archive, path.resolve(updaterKey), publicKey, attempt, commandRun, tauriCli);
    }
    receipts.push(signed.receipt);
  }
  fs.writeFileSync(path.join(assets, 'Apple-notarization.json'), `${JSON.stringify({ schema: 1, version, source, team, builds: receipts }, null, 2)}\n`);
  const notes = path.join(attempt, 'release-notes.md');
  const upstream = pkg.forkUpstream?.version || version;
  fs.writeFileSync(notes, options.notesFile ? fs.readFileSync(options.notesFile, 'utf8')
    : `基于上游 ${upstream}，保留本 Fork 定制功能。\n\nmacOS ARM、Intel 和 Universal 安装包均已完成 Developer ID 签名、Apple 公证及 Gatekeeper 校验。更新包使用本 Fork 的独立签名。\n\n源码：[${source.slice(0, 8)}](https://github.com/${REPO}/commit/${source})。附件包含 Apple 公证记录和 SHA256SUMS.txt。\n`);
  buildTargetManifests({ version, repo: REPO, tag, assetsDir: assets, outputDir: assets,
    notesFile: notes, publishedAt: new Date().toISOString(), targets: TARGETS });
  commandRun(process.execPath, [path.join(__dirname, 'build_merged_latest_json.cjs'), '--version', version, '--tag', tag,
    '--repo', REPO, '--assets-dir', assets, '--notes-file', notes, '--published-at', new Date().toISOString(),
    '--output', path.join(assets, 'latest.json')]);
  for (const { file } of matchUpdaterFiles(assets)) verifyUpdater(file, publicKey, attempt, commandRun);
  writeChecksums(assets);
  signUpdater(path.join(assets, 'SHA256SUMS.txt'), path.resolve(updaterKey), publicKey, attempt, commandRun, tauriCli);
  console.log(`All local signing and notarization checks passed: ${assets}`);
  if (!options.publish && !options.uploadDraft) return { assets, notes, published: false };

  assertSource(source, api);
  assertTag(tag, source, api);
  assertNewerRelease(version, api('releases/latest'));
  validateDraft(api(releaseEndpoint), tag, source, release.id);
  const changed = filesIn(assets).filter(({ name, file }) => original.get(name) !== sha256(file));
  if (changed.length) {
    await (dependencies.uploadReleaseAssets || uploadReleaseAssets)(tag, changed.map(({ file }) => file), {
      run: args => spawnSync('gh', args, { encoding: 'utf8', timeout: 10 * 60_000,
        maxBuffer: 1024 * 1024, env: { ...process.env, GH_REPO: REPO } }),
    });
  }
  const uploaded = api(releaseEndpoint);
  validateDraft(uploaded, tag, source, release.id);
  const expected = filesIn(assets);
  if (uploaded.assets.length !== expected.length || expected.some(local => !uploaded.assets.some(remote => remote.name === local.name && remote.size === local.size))) {
    throw new Error('Uploaded asset set is incomplete or has unexpected files');
  }
  if (uploaded.assets.every(asset => typeof asset.digest === 'string' && asset.digest.startsWith('sha256:'))) {
    for (const asset of uploaded.assets) {
      if (asset.digest !== `sha256:${sha256(path.join(assets, asset.name))}`) throw new Error(`Uploaded digest mismatch: ${asset.name}`);
    }
  } else {
    const verified = path.join(attempt, 'verified-upload');
    fs.mkdirSync(verified);
    commandRun('gh', ['release', 'download', tag, '--repo', REPO, '--dir', verified], { timeout: 20 * 60_000 });
    verifyUpdater(path.join(verified, 'SHA256SUMS.txt'), publicKey, attempt, commandRun);
    verifyChecksums(verified);
    for (const { name, file } of expected) {
      if (sha256(file) !== sha256(path.join(verified, name))) throw new Error(`Downloaded upload mismatch: ${name}`);
    }
  }
  assertSource(source, api);
  assertTag(tag, source, api);
  assertNewerRelease(version, api('releases/latest'));
  const finalDraft = api(releaseEndpoint);
  validateDraft(finalDraft, tag, source, release.id);
  const assetSnapshot = value => value.assets.map(asset => [asset.name, asset.id ?? null, asset.size,
    asset.digest ?? null, asset.state]).sort((a, b) => a[0].localeCompare(b[0]));
  if (JSON.stringify(assetSnapshot(finalDraft)) !== JSON.stringify(assetSnapshot(uploaded))) {
    throw new Error('Draft assets changed after upload verification');
  }
  if (options.uploadDraft) {
    const updatedDraft = JSON.parse(commandRun('gh', ['api', '--method', 'PATCH', `repos/${REPO}/${releaseEndpoint}`,
      '-F', `body=@${notes}`, '-f', `tag_name=${tag}`]));
    validateDraft(updatedDraft, tag, source, release.id);
    if (updatedDraft.body !== fs.readFileSync(notes, 'utf8')
        || JSON.stringify(assetSnapshot(updatedDraft)) !== JSON.stringify(assetSnapshot(uploaded))) {
      throw new Error('Draft changed while updating the verified release notes');
    }
    console.log(`Verified signed assets in draft https://github.com/${REPO}/releases/tag/${tag}`);
    return { assets, notes, published: false, uploaded: true };
  }
  commandRun('gh', ['api', '--method', 'PATCH', `repos/${REPO}/${releaseEndpoint}`,
    '-F', `body=@${notes}`, '-f', `tag_name=${tag}`, '-F', 'draft=false', '-F', 'prerelease=false', '-f', 'make_latest=true']);
  console.log(`Published https://github.com/${REPO}/releases/tag/${tag}`);
  commandRun(process.execPath, [path.join(__dirname, 'verify_published_updater_manifests.cjs'), '--version', version,
    '--repo', REPO, '--release-base-url', `https://github.com/${REPO}/releases/download/${tag}`,
    '--targets', TARGETS.join(','), '--legacy'], { timeout: 10 * 60_000, stdio: 'inherit' });
  return { assets, notes, published: true };
}

function parseArgs(argv) {
  const options = {};
  const names = ['tag', 'source-sha', 'work-dir', 'identity', 'team', 'key-path', 'key-id', 'issuer', 'updater-key', 'notes-file',
    'expected-run-id', 'expected-run-attempt', 'expected-controller-sha', 'tauri-cli'];
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--publish') { options.publish = true; continue; }
    if (argv[i] === '--upload-draft') { options.uploadDraft = true; continue; }
    const name = argv[i].replace(/^--/, '');
    if (!names.includes(name) || !argv[i + 1] || argv[i + 1].startsWith('--')) throw new Error(`Invalid argument: ${argv[i]}`);
    options[name.replace(/-([a-z])/g, (_, letter) => letter.toUpperCase())] = argv[++i];
  }
  validateRequestedOptions(options);
  return options;
}

if (require.main === module) {
  finalizeFork(parseArgs(process.argv.slice(2))).catch(error => { console.error(error.message); process.exitCode = 1; });
}
module.exports = { finalizeFork, validateDraft, validateProvenance, verifyChecksums, writeChecksums, allowedName, parseArgs,
  assertTag, assertNewerRelease, validateSignedResult, findReleaseByTag };
