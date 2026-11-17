#!/usr/bin/env node
'use strict';
// Publish a Fork draft that CI has already signed, notarized and verified.
//
// Unlike finalize_fork_release.cjs this needs no Apple tooling and no 871 MB
// download: GitHub stores a sha256 digest for every uploaded asset, so the
// uploaded set is re-verified against the signed SHA256SUMS.txt and the Apple
// receipts without fetching the installers again. Every check below is
// read-only until the final draft -> public transition.

const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { validateDraft, validateProvenance, findReleaseByTag, allowedName, assertTag, assertNewerRelease,
  assertSource, verifyUpdater } = require('./finalize_fork_release.cjs');
const { TARGET_SPECS } = require('./build_target_latest_json.cjs');

const REPO = 'knobblex/cockpit-tools';
const ROOT = path.resolve(__dirname, '../..');
const ARCHES = ['aarch64', 'x64', 'universal'];
const TARGETS = Object.keys(TARGET_SPECS);
const TAG_VERSION = /^fork-v((?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*))$/;
const CHECKSUM_FILES = ['SHA256SUMS.txt', 'SHA256SUMS.txt.sig'];
const META_PATTERNS = ['Apple-notarization.json', 'Fork-build.json', ...CHECKSUM_FILES,
  'latest.json', ...TARGETS.map(target => `latest-${target}.json`)];

function run(executable, args, options = {}) {
  const result = spawnSync(executable, args, {
    cwd: ROOT, encoding: 'utf8', timeout: options.timeout || 120_000, maxBuffer: 8 * 1024 * 1024,
    env: { ...process.env, GH_REPO: REPO }, ...options,
  });
  if (result.error || result.status !== 0) {
    throw new Error(`${executable} ${args[0]} failed: ${(result.stderr || result.error?.message || `exit ${result.status}`).trim()}`);
  }
  return (result.stdout || '').trim();
}

function ghJson(endpoint, commandRun = run) {
  const raw = commandRun('gh', ['api', `repos/${REPO}/${endpoint}`]);
  return JSON.parse(raw);
}

function parseArgs(argv) {
  const options = {};
  const names = ['tag', 'source-sha', 'work-dir', 'identity', 'team', 'notes-file',
    'expected-run-id', 'expected-run-attempt', 'expected-controller-sha'];
  for (let index = 0; index < argv.length; index += 1) {
    const name = argv[index].replace(/^--/, '');
    if (!argv[index].startsWith('--') || !names.includes(name) || !argv[index + 1] || argv[index + 1].startsWith('--')) {
      throw new Error(`Invalid argument: ${argv[index]}`);
    }
    options[name.replace(/-([a-z])/g, (_, letter) => letter.toUpperCase())] = argv[++index];
  }
  for (const key of ['expectedRunId', 'expectedRunAttempt']) {
    if (options[key] !== undefined && !/^[1-9]\d*$/.test(options[key])) throw new Error(`${key} must be a positive decimal integer`);
  }
  if (options.expectedControllerSha !== undefined && !/^[a-f0-9]{40}$/.test(options.expectedControllerSha)) {
    throw new Error('expectedControllerSha must be a full lowercase commit SHA');
  }
  return options;
}

// The signed checksum list is the provenance root for every other asset.
function parseChecksums(text) {
  const hashes = new Map();
  const lines = String(text).trim();
  if (!lines) throw new Error('The signed checksum list is empty');
  for (const line of lines.split(/\r?\n/)) {
    const match = /^([a-f0-9]{64}) {2}([A-Za-z0-9][A-Za-z0-9._-]*)$/.exec(line);
    if (!match || CHECKSUM_FILES.includes(match[2]) || hashes.has(match[2])) {
      throw new Error('Invalid or duplicate checksum entry');
    }
    hashes.set(match[2], match[1]);
  }
  if (hashes.size === 0) throw new Error('The signed checksum list is empty');
  return hashes;
}

function validateNotarization(document, expected) {
  const { version, source, team, identity, identifier, checksums } = expected;
  if (!document || document.schema !== 1 || document.version !== version || document.source !== source) {
    throw new Error('Apple notarization record does not match this release');
  }
  if (!Array.isArray(document.builds) || document.builds.length !== ARCHES.length) {
    throw new Error('Apple notarization record must cover exactly three architectures');
  }
  const seen = new Set();
  for (const build of document.builds) {
    const arch = build?.arch;
    if (!ARCHES.includes(arch) || seen.has(arch)) throw new Error('Apple notarization record has an unexpected architecture');
    seen.add(arch);
    if (build.version !== version || !build.team || !build.identity || !build.bundleIdentifier) {
      throw new Error(`Incomplete Apple receipt for ${arch}`);
    }
    if (team && build.team !== team) throw new Error(`Apple receipt for ${arch} was signed by an unexpected team`);
    if (identity && build.identity !== identity) throw new Error(`Apple receipt for ${arch} uses an unexpected identity`);
    if (identifier && build.bundleIdentifier !== identifier) throw new Error(`Apple receipt for ${arch} has an unexpected bundle identifier`);
    for (const submission of [build.appSubmission, build.dmgSubmission]) {
      if (submission?.status !== 'Accepted' || typeof submission.id !== 'string' || !submission.id.trim()) {
        throw new Error(`Missing accepted Apple submission for ${arch}`);
      }
    }
    if (!Array.isArray(build.outputs) || build.outputs.length === 0) throw new Error(`Apple receipt for ${arch} lists no signed outputs`);
    // Bind the file Apple notarized to the file uploaded to the release: the
    // receipt hash must equal the hash in the signed checksum list.
    for (const output of build.outputs) {
      if (typeof output?.name !== 'string' || !checksums.has(output.name)) {
        throw new Error(`Apple receipt output ${String(output?.name)} is missing from the signed checksum list`);
      }
      if (checksums.get(output.name) !== output.sha256) {
        throw new Error(`Published asset ${output.name} differs from the notarized file`);
      }
    }
    const dmgName = `Cockpit.Tools_${version}_${arch}.dmg`;
    if (!build.outputs.some(output => output.name === dmgName)) throw new Error(`Apple receipt for ${arch} does not cover ${dmgName}`);
  }
  const receiptTeams = new Set(document.builds.map(build => build.team));
  if (receiptTeams.size !== 1) throw new Error('Apple receipts disagree on the signing team');
}

function validateAssets(release, checksums) {
  const names = new Set();
  const digests = new Map();
  for (const asset of release.assets || []) {
    if (!allowedName(asset.name) || names.has(asset.name)) throw new Error(`Invalid or duplicate draft asset: ${asset.name}`);
    if (asset.state !== 'uploaded' || !(asset.size > 0)) throw new Error(`Incomplete draft asset: ${asset.name}`);
    names.add(asset.name);
    if (CHECKSUM_FILES.includes(asset.name)) continue;
    const expected = checksums.get(asset.name);
    if (!expected) throw new Error(`Asset ${asset.name} is missing from the signed checksum list`);
    if (typeof asset.digest !== 'string' || asset.digest !== `sha256:${expected}`) {
      throw new Error(`Asset ${asset.name} does not match the signed checksum list (GitHub digest ${asset.digest || 'missing'})`);
    }
    digests.set(asset.name, expected);
  }
  for (const name of ['Fork-build.json', 'Apple-notarization.json', ...CHECKSUM_FILES]) {
    if (!names.has(name)) throw new Error(`Draft is missing ${name}`);
  }
  if (digests.size !== checksums.size) throw new Error('The signed checksum list does not cover every draft asset exactly once');
  return digests;
}

function assetSnapshot(release) {
  return (release.assets || []).map(asset => [asset.name, asset.id ?? null, asset.size, asset.digest ?? null, asset.state])
    .sort((left, right) => left[0].localeCompare(right[0]));
}

async function publishSignedDraft(options, dependencies = {}) {
  const { tag, sourceSha: source } = options;
  const version = TAG_VERSION.exec(tag || '')?.[1];
  if (!version || !/^[a-f0-9]{40}$/.test(source || '')) throw new Error('A stable Fork tag and full source SHA are required');
  const commandRun = dependencies.run || run;
  const api = dependencies.api || (endpoint => ghJson(endpoint, commandRun));
  const workDir = path.resolve(options.workDir || path.join(ROOT, '.tmp/publish-signed-draft'));

  // Parse version and updater identity at the immutable source, never at HEAD.
  const pkg = JSON.parse(commandRun('git', ['show', `${source}:package.json`]));
  const config = JSON.parse(commandRun('git', ['show', `${source}:src-tauri/tauri.conf.json`]));
  if (pkg.version !== version || config.version !== version || !config.plugins?.updater?.pubkey) {
    throw new Error('Source package, Tauri configuration and release version must agree');
  }
  const expectedEndpoints = [
    `https://github.com/${REPO}/releases/latest/download/latest-{{target}}.json`,
    `https://github.com/${REPO}/releases/latest/download/latest.json`,
  ];
  if (JSON.stringify(config.plugins.updater.endpoints) !== JSON.stringify(expectedEndpoints)) {
    throw new Error('Unexpected updater endpoints');
  }

  const release = findReleaseByTag(tag, api);
  const releaseEndpoint = `releases/${release.id}`;
  const alreadyPublished = !release.draft && !release.prerelease;
  if (alreadyPublished) {
    if (release.target_commitish !== source) throw new Error('A published release for this tag points at another commit');
  } else {
    validateDraft(release, tag, source);
  }
  assertSource(source, api);
  assertTag(tag, source, api);
  if (!alreadyPublished) assertNewerRelease(version, api('releases/latest'));

  fs.mkdirSync(workDir, { recursive: true });
  const attempt = fs.mkdtempSync(path.join(workDir, 'attempt-'));
  const meta = path.join(attempt, 'meta');
  fs.mkdirSync(meta);
  console.log(`Downloading release metadata for ${tag} into ${attempt}`);
  commandRun('gh', ['release', 'download', tag, '--repo', REPO, '--dir', meta,
    ...META_PATTERNS.flatMap(pattern => ['--pattern', pattern])], { timeout: 10 * 60_000 });

  const checksums = parseChecksums(fs.readFileSync(path.join(meta, 'SHA256SUMS.txt'), 'utf8'));
  const publicKey = path.join(attempt, 'updater.pub');
  fs.writeFileSync(publicKey, Buffer.from(config.plugins.updater.pubkey, 'base64'));
  verifyUpdater(path.join(meta, 'SHA256SUMS.txt'), publicKey, attempt, commandRun);

  const provenance = JSON.parse(fs.readFileSync(path.join(meta, 'Fork-build.json'), 'utf8'));
  for (const [option, value] of Object.entries({ expectedRunId: provenance.runId,
    expectedRunAttempt: provenance.runAttempt, expectedControllerSha: provenance.controller })) {
    if (options[option] !== undefined && options[option] !== String(value)) {
      throw new Error(`Build provenance does not match ${option}`);
    }
  }
  validateProvenance(provenance, api(`actions/runs/${provenance.runId}`), source, version);
  const controllerConfig = JSON.parse(commandRun('git', ['show', `${provenance.controller}:src-tauri/tauri.conf.json`]));
  if (JSON.stringify(config.plugins.updater) !== JSON.stringify(controllerConfig.plugins?.updater)) {
    throw new Error('Draft updater identity differs from the trusted build controller');
  }

  validateNotarization(JSON.parse(fs.readFileSync(path.join(meta, 'Apple-notarization.json'), 'utf8')), {
    version, source, team: options.team, identity: options.identity, identifier: config.identifier, checksums,
  });
  validateAssets(release, checksums);
  console.log('Signed checksum list, Apple receipts and asset digests all agree.');

  if (alreadyPublished) {
    console.log(`${tag} is already published; verifying its manifests only.`);
    verifyPublishedActions(commandRun, version, tag);
    return { published: false, alreadyPublished: true, version, tag };
  }

  assertSource(source, api);
  assertTag(tag, source, api);
  assertNewerRelease(version, api('releases/latest'));
  const current = api(releaseEndpoint);
  validateDraft(current, tag, source, release.id);
  if (JSON.stringify(assetSnapshot(current)) !== JSON.stringify(assetSnapshot(release))) {
    throw new Error('Draft assets changed while they were being verified');
  }

  const patchArgs = ['api', '--method', 'PATCH', `repos/${REPO}/${releaseEndpoint}`];
  if (options.notesFile) patchArgs.push('-F', `body=@${path.resolve(options.notesFile)}`);
  patchArgs.push('-F', 'draft=false', '-F', 'prerelease=false', '-f', 'make_latest=true');
  const published = JSON.parse(commandRun('gh', patchArgs));
  if (published.draft !== false || published.prerelease !== false) throw new Error('Release did not become public');
  if (JSON.stringify(assetSnapshot(published)) !== JSON.stringify(assetSnapshot(release))) {
    throw new Error('Published assets differ from the verified draft');
  }
  console.log(`Published https://github.com/${REPO}/releases/tag/${tag}`);

  verifyPublishedActions(commandRun, version, tag);
  return { published: true, alreadyPublished: false, version, tag };
}

function verifyPublishedActions(commandRun, version, tag) {
  commandRun(process.execPath, [path.join(__dirname, 'verify_published_updater_manifests.cjs'),
    '--version', version, '--repo', REPO,
    '--release-base-url', `https://github.com/${REPO}/releases/download/${tag}`,
    '--targets', TARGETS.join(','), '--legacy'], { timeout: 10 * 60_000, stdio: 'inherit' });
}

if (require.main === module) {
  publishSignedDraft(parseArgs(process.argv.slice(2)))
    .catch(error => { console.error(error.message); process.exitCode = 1; });
}

module.exports = { publishSignedDraft, parseChecksums, validateNotarization, validateAssets, assetSnapshot, parseArgs, META_PATTERNS };
