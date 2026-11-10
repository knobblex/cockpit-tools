#!/usr/bin/env node
'use strict';
// Optional legacy controller for local Apple signing; the current default
// pipeline signs on GitHub Actions (see docs/release-process.md#upstream-sync).
//
// GitHub already runs upstream-sync.yml every hour, which detects a new
// upstream release, builds it on CI, promotes the customization branch and
// leaves a DRAFT release. This optional local path performs Apple signing,
// notarization and publication on a configured Mac. This controller closes
// that loop and never re-implements the CI logic:
//
//   1. read the newest upstream release and the fork's recorded baseline
//   2. if GitHub already produced a matching draft, finalize it
//   3. otherwise attach to a running sync run, or dispatch one as a fallback
//   4. wait for the build, then finalize (sign + notarize + publish)
//
// Every step is idempotent and guarded by a lock plus a state file, so running
// it every hour is safe even when a previous run is still waiting on a build.

const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { spawnSync } = require('node:child_process');
const { compareVersions } = require('./fork-version.cjs');

const ROOT = path.resolve(__dirname, '../..');
const REPO = 'knobblex/cockpit-tools';
const UPSTREAM = 'jlcodes99/cockpit-tools';
const BRANCH = 'codex/local-customizations';
const WORKFLOW = 'upstream-sync.yml';
const STATE_DIR = path.join(ROOT, '.tmp/hourly-fork-release');
const STATE_FILE = path.join(STATE_DIR, 'state.json');
const LOCK_FILE = path.join(STATE_DIR, 'lock.json');
const CONFIG_FILE = path.join(os.homedir(), '.config/cockpit-tools-fork/hourly-release.json');
const TAG_VERSION = /^fork-v((?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*))$/;

function log(message) { console.log(`[hourly ${new Date().toISOString()}] ${message}`); }

function parseArgs(argv) {
  const options = { maxWaitMinutes: 50 };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--dry-run') { options.dryRun = true; continue; }
    if (arg === '--status') { options.status = true; continue; }
    if (arg === '--init-config') { options.initConfig = true; continue; }
    if (arg === '--prune') { options.prune = true; continue; }
    if (arg === '--max-wait-minutes') {
      const value = Number(argv[++i]);
      if (!Number.isInteger(value) || value < 1 || value > 180) throw new Error('Expected --max-wait-minutes 1..180');
      options.maxWaitMinutes = value;
      continue;
    }
    throw new Error(`Unknown argument: ${arg}`);
  }
  return options;
}

function gh(args, options = {}) {
  const result = spawnSync('gh', args, {
    cwd: ROOT, encoding: 'utf8', timeout: options.timeout || 120_000, maxBuffer: 32 * 1024 * 1024,
    env: { ...process.env, GH_PROMPT_DISABLED: '1', GH_NO_UPDATE_NOTIFIER: '1', GH_PAGER: 'cat' },
  });
  if (result.error) throw new Error(`gh ${args[0]} failed: ${result.error.message}`);
  if (result.status !== 0) {
    throw new Error(`gh ${args.slice(0, 3).join(' ')} failed: ${(result.stderr || '').trim() || `exit ${result.status}`}`);
  }
  return (result.stdout || '').trim();
}

function ghJson(args) { return JSON.parse(gh(args)); }

function readState() {
  try {
    const state = JSON.parse(fs.readFileSync(STATE_FILE, 'utf8'));
    return state && typeof state === 'object' ? state : {};
  } catch (error) {
    if (error.code === 'ENOENT') return {};
    throw new Error(`Unreadable state file ${STATE_FILE}: ${error.message}`);
  }
}

function writeState(patch) {
  if (writeState.disabled) return patch;
  fs.mkdirSync(STATE_DIR, { recursive: true });
  const next = { ...readState(), ...patch, updatedAt: new Date().toISOString() };
  const temp = `${STATE_FILE}.tmp`;
  fs.writeFileSync(temp, `${JSON.stringify(next, null, 2)}\n`);
  fs.renameSync(temp, STATE_FILE);
  return next;
}

function processAlive(pid) {
  if (!Number.isSafeInteger(pid) || pid <= 0) return false;
  try { process.kill(pid, 0); return true; } catch (error) { return error.code === 'EPERM'; }
}

// A waiting build can outlive the hour, so the lock is stale only after 3h.
function acquireLock() {
  fs.mkdirSync(STATE_DIR, { recursive: true });
  let current = null;
  try { current = JSON.parse(fs.readFileSync(LOCK_FILE, 'utf8')); } catch (error) {
    if (error.code !== 'ENOENT') throw new Error(`Unreadable lock file ${LOCK_FILE}: ${error.message}`);
  }
  if (current) {
    const ageMinutes = (Date.now() - Date.parse(current.startedAt || 0)) / 60_000;
    if (processAlive(current.pid) && ageMinutes < 180) {
      log(`Another controller run is active (pid ${current.pid}, started ${current.startedAt}); exiting.`);
      return false;
    }
    log(`Clearing stale lock (pid ${current.pid}, started ${current.startedAt}).`);
  }
  fs.writeFileSync(LOCK_FILE, `${JSON.stringify({ pid: process.pid, startedAt: new Date().toISOString() }, null, 2)}\n`);
  return true;
}

function releaseLock() {
  try { fs.unlinkSync(LOCK_FILE); } catch (error) { if (error.code !== 'ENOENT') throw error; }
}

function upstreamLatest() {
  const release = ghJson(['api', `repos/${UPSTREAM}/releases/latest`]);
  if (!release || release.draft || release.prerelease) throw new Error('Upstream latest release is not a published stable release');
  const version = String(release.tag_name || '').replace(/^v/, '');
  compareVersions(version, version);
  return { version, tag: release.tag_name, publishedAt: release.published_at };
}

function forkBaseline() {
  const encoded = gh(['api', `repos/${REPO}/contents/package.json?ref=${BRANCH}`, '--jq', '.content']);
  const pkg = JSON.parse(Buffer.from(encoded, 'base64').toString('utf8'));
  const baseline = pkg.forkUpstream;
  if (!baseline || typeof baseline.version !== 'string') throw new Error(`package.json on ${BRANCH} is missing forkUpstream`);
  return { version: baseline.version, commit: baseline.commit, appVersion: pkg.version };
}

function forkReleases() {
  const releases = ghJson(['api', `repos/${REPO}/releases?per_page=100`]);
  if (!Array.isArray(releases)) throw new Error('Expected a release list from GitHub');
  return releases.filter(release => TAG_VERSION.test(release.tag_name || ''))
    .sort((left, right) => compareVersions(TAG_VERSION.exec(right.tag_name)[1], TAG_VERSION.exec(left.tag_name)[1]));
}

function branchTip() {
  const result = spawnSync('git', ['ls-remote', '--refs', 'fork', `refs/heads/${BRANCH}`], {
    cwd: ROOT, encoding: 'utf8', timeout: 120_000,
  });
  if (result.error || result.status !== 0) {
    throw new Error(`git ls-remote failed: ${(result.stderr || result.error?.message || '').trim()}`);
  }
  const fields = (result.stdout || '').trim().split(/\s+/);
  if (fields.length !== 2 || !/^[a-f0-9]{40}$/.test(fields[0])) throw new Error(`Could not resolve the tip of ${BRANCH}`);
  return fields[0];
}

function syncRuns(limit = 20) {
  const data = ghJson(['api', `repos/${REPO}/actions/workflows/${WORKFLOW}/runs?per_page=${limit}`]);
  return Array.isArray(data.workflow_runs) ? data.workflow_runs : [];
}

function runState(run) {
  return { id: run.id, status: run.status, conclusion: run.conclusion, event: run.event, createdAt: run.created_at, url: run.html_url };
}

function dispatchSync() {
  gh(['workflow', 'run', WORKFLOW, '--repo', REPO, '--ref', 'main']);
}

// Poll one run until it leaves the queue, bounded by the caller's budget.
function waitRun(runId, deadline) {
  for (;;) {
    const run = ghJson(['api', `repos/${REPO}/actions/runs/${runId}`]);
    if (run.status === 'completed') return runState(run);
    if (Date.now() > deadline) return null;
    const remaining = deadline - Date.now();
    spawnSync('sleep', [String(Math.max(1, Math.ceil(Math.min(60_000, remaining) / 1000)))], { timeout: 90_000 });
  }
}

function newestRunAfter(timestamp, event) {
  const since = Date.parse(timestamp || 0);
  return syncRuns().find(run => Date.parse(run.created_at) >= since
    && (event ? run.event === event : true)
    && ['queued', 'in_progress'].includes(run.status));
}

function loadConfig(options) {
  if (options.initConfig) {
    const template = {
      identity: 'Developer ID Application: <Certificate Common Name> (<TEAMID>)',
      team: '<TEAMID>',
      keyPath: path.join(os.homedir(), '.appstoreconnect/private_keys/AuthKey_<KEYID>.p8'),
      keyId: '<KEYID>',
      issuer: '<ISSUER-UUID>',
      updaterKey: path.join(os.homedir(), '.config/cockpit-tools-fork/signing.key'),
    };
    fs.mkdirSync(path.dirname(CONFIG_FILE), { recursive: true });
    if (fs.existsSync(CONFIG_FILE)) throw new Error(`Config already exists: ${CONFIG_FILE}`);
    fs.writeFileSync(CONFIG_FILE, `${JSON.stringify(template, null, 2)}\n`, { mode: 0o600 });
    log(`Wrote config template to ${CONFIG_FILE}`);
    return null;
  }
  if (!fs.existsSync(CONFIG_FILE)) {
    throw new Error(`Signing config missing: ${CONFIG_FILE} (create it with --init-config, then fill in the values)`);
  }
  const config = JSON.parse(fs.readFileSync(CONFIG_FILE, 'utf8'));
  for (const name of ['identity', 'team', 'keyPath', 'keyId', 'issuer', 'updaterKey']) {
    if (typeof config[name] !== 'string' || !config[name].trim()) throw new Error(`Config field ${name} is required in ${CONFIG_FILE}`);
  }
  for (const name of ['keyPath', 'updaterKey']) {
    if (!fs.existsSync(config[name])) throw new Error(`Config ${name} does not exist: ${config[name]}`);
  }
  return config;
}

function fetchSource(sourceSha) {
  gh(['api', `repos/${REPO}/commits/${sourceSha}`, '--jq', '.sha']);
  const result = spawnSync('git', ['fetch', '--no-tags', 'fork', `+${BRANCH}:refs/hourly/source`], {
    cwd: ROOT, encoding: 'utf8', timeout: 300_000,
  });
  if (result.error || result.status !== 0) {
    throw new Error(`git fetch failed: ${(result.stderr || result.error?.message || '').trim()}`);
  }
  const check = spawnSync('git', ['cat-file', '-e', `${sourceSha}^{commit}`], { cwd: ROOT, encoding: 'utf8' });
  if (check.status !== 0) throw new Error(`Source commit ${sourceSha} is not available in the local repository`);
}

// Each finalized attempt keeps ~1.8 GiB of downloaded and signed bundles. The
// artifacts stay recoverable from the published release, so only the receipts
// and notes are retained to keep the signing Mac from filling up.
function pruneWorkDir(workDir) {
  let entries;
  try { entries = fs.readdirSync(workDir, { withFileTypes: true }); } catch (error) {
    if (error.code === 'ENOENT') return;
    throw error;
  }
  for (const entry of entries) {
    if (!entry.isDirectory() || !entry.name.startsWith('attempt-')) continue;
    const attempt = path.join(workDir, entry.name);
    for (const item of fs.readdirSync(attempt)) {
      if (/\.(json|md)$/.test(item)) continue;
      fs.rmSync(path.join(attempt, item), { recursive: true, force: true });
    }
  }
}

function pruneOldWorkDirs(keepVersion) {
  const tmp = path.join(ROOT, '.tmp');
  let entries;
  try { entries = fs.readdirSync(tmp, { withFileTypes: true }); } catch (error) {
    if (error.code === 'ENOENT') return;
    throw error;
  }
  for (const entry of entries) {
    const match = /^fork-v(.+)-finalize$/.exec(entry.name);
    if (!entry.isDirectory() || !match || match[1] === keepVersion) continue;
    pruneWorkDir(path.join(tmp, entry.name));
  }
}

function finalize(draft, config, options) {
  const version = TAG_VERSION.exec(draft.tag_name)[1];
  const source = draft.target_commitish;
  const workDir = path.join(ROOT, `.tmp/fork-v${version}-finalize`);
  log(`Finalizing ${draft.tag_name} (release ${draft.id}) from ${source} into ${workDir}`);
  if (options.dryRun) { log('Dry run: skipping signing and publication.'); return; }
  fetchSource(source);
  const args = [
    path.join(__dirname, 'finalize_fork_release.cjs'),
    '--tag', draft.tag_name,
    '--source-sha', source,
    '--work-dir', workDir,
    '--identity', config.identity,
    '--team', config.team,
    '--key-path', config.keyPath,
    '--key-id', config.keyId,
    '--issuer', config.issuer,
    '--updater-key', config.updaterKey,
    '--publish',
  ];
  const result = spawnSync(process.execPath, args, { cwd: ROOT, stdio: 'inherit', env: process.env });
  if (result.error || result.status !== 0) {
    throw new Error(`finalize_fork_release exited ${result.status ?? result.error?.message ?? 'unknown'}`);
  }
  log(`Published https://github.com/${REPO}/releases/tag/${draft.tag_name}`);
  pruneWorkDir(workDir);
  pruneOldWorkDirs(version);
  log('Pruned downloaded and signed bundles; receipts and notes kept.');
}

function pickDraft(releases, publishedVersion, tip) {
  const draft = releases.find(release => release.draft);
  if (!draft) return null;
  const version = TAG_VERSION.exec(draft.tag_name)[1];
  if (publishedVersion && compareVersions(version, publishedVersion) <= 0) {
    log(`Ignoring stale draft ${draft.tag_name}: ${publishedVersion} is already published.`);
    return null;
  }
  if (draft.target_commitish !== tip) {
    log(`Ignoring draft ${draft.tag_name}: it targets ${String(draft.target_commitish).slice(0, 8)}, but ${BRANCH} is at ${tip.slice(0, 8)}.`);
    return null;
  }
  return draft;
}

function main() {
  const options = parseArgs(process.argv.slice(2));

  if (options.status) {
    const state = readState();
    log(`State: ${JSON.stringify(state, null, 2)}`);
    return;
  }

  if (options.prune) {
    pruneOldWorkDirs(null);
    log('Pruned downloaded and signed bundles; receipts and notes kept.');
    return;
  }

  const config = loadConfig(options);
  if (!config) return;
  // A dry run must never consume the dispatch cooldown or the waiting state.
  writeState.disabled = options.dryRun === true;

  if (!options.dryRun && !acquireLock()) return;
  const deadline = Date.now() + options.maxWaitMinutes * 60_000;
  try {
    const upstream = upstreamLatest();
    const baseline = forkBaseline();
    const releases = forkReleases();
    const published = releases.find(release => !release.draft && !release.prerelease) || null;
    const publishedVersion = published ? TAG_VERSION.exec(published.tag_name)[1] : null;
    const tip = branchTip();
    const state = readState();

    log(`Upstream ${upstream.version} (${upstream.tag}); fork baseline ${baseline.version}; fork app ${baseline.appVersion}; published ${publishedVersion || 'none'}; tip ${tip.slice(0, 8)}`);

    const draft = pickDraft(releases, publishedVersion, tip);
    if (draft) {
      finalize(draft, config, options);
      writeState({ lastResult: 'published', lastTag: draft.tag_name, lastError: null });
      return;
    }

    if (compareVersions(baseline.version, upstream.version) >= 0) {
      log('Fork already carries the newest upstream release; nothing to publish.');
      writeState({ lastResult: 'up-to-date', lastUpstream: upstream.version, lastError: null });
      return;
    }

    log(`New upstream release ${upstream.version} is not in the fork yet; the build has to come from GitHub Actions.`);

    const active = newestRunAfter(Date.now() - 4 * 3600_000);
    if (active) {
      log(`Attaching to running ${active.event} run ${active.id} (${active.status}).`);
    } else {
      // GitHub schedules upstream-sync at :17 every hour. Only step in when that
      // schedule clearly did not pick the release up, otherwise two dispatches
      // would race for the same build.
      const releaseAgeMinutes = (Date.now() - Date.parse(upstream.publishedAt || 0)) / 60_000;
      if (!(releaseAgeMinutes > 90)) {
        log(`Upstream ${upstream.version} is ${Math.round(releaseAgeMinutes)} min old and GitHub has not finished dispatching it yet; will retry next hour.`);
        writeState({ lastResult: 'awaiting-schedule', lastUpstream: upstream.version, lastError: null });
        return;
      }
      const dispatched = state.lastDispatch && state.lastDispatch.upstream === upstream.version ? state.lastDispatch : null;
      if (dispatched && Date.now() - Date.parse(dispatched.at) < 6 * 3600_000) {
        log(`Already dispatched ${upstream.version} at ${dispatched.at} without a draft; refusing to retry for 6h.`);
        writeState({ lastResult: 'dispatch-cooldown', lastError: null });
        return;
      }
      if (options.dryRun) {
        log(`Dry run: would dispatch ${WORKFLOW} for upstream ${upstream.version}.`);
        return;
      }
      log(`Dispatching ${WORKFLOW} for upstream ${upstream.version}.`);
      dispatchSync();
      writeState({ lastDispatch: { upstream: upstream.version, at: new Date().toISOString() } });
    }

    if (options.dryRun) { log('Dry run: stopping before the wait loop.'); return; }

    let runId = active ? active.id : null;
    if (!runId) {
      // The dispatch endpoint returns before the new run appears in the list.
      const since = Date.now() - 5_000;
      for (let attempt = 0; attempt < 20 && !runId; attempt += 1) {
        const created = syncRuns().find(run => Date.parse(run.created_at) >= since);
        if (created) runId = created.id;
        else spawnSync('sleep', ['3'], { timeout: 10_000 });
      }
    }
    if (!runId) throw new Error('Could not identify the upstream-sync run to wait for');
    log(`Waiting for run ${runId} to finish (budget ${options.maxWaitMinutes} min).`);
    const finished = waitRun(runId, deadline);

    if (!finished) {
      log('Build still running; the next hourly run will pick it up.');
      writeState({ lastResult: 'waiting-build', waitingRun: runId, lastError: null });
      return;
    }
    if (finished.conclusion !== 'success') {
      throw new Error(`upstream-sync run ${runId} concluded ${finished.conclusion}: ${finished.url}`);
    }

    const after = forkReleases();
    const publishedAfter = after.find(release => !release.draft && !release.prerelease) || null;
    const publishedAfterVersion = publishedAfter ? TAG_VERSION.exec(publishedAfter.tag_name)[1] : null;
    const nextDraft = pickDraft(after, publishedAfterVersion, branchTip());
    if (!nextDraft) {
      const branchBaseline = forkBaseline();
      if (compareVersions(branchBaseline.version, upstream.version) >= 0) {
        log('Sync completed without a draft; the branch already carries the release.');
        writeState({ lastResult: 'up-to-date', lastUpstream: upstream.version, lastError: null });
        return;
      }
      throw new Error('Sync run succeeded but produced no publishable draft');
    }
    finalize(nextDraft, config, options);
    writeState({ lastResult: 'published', lastTag: nextDraft.tag_name, lastError: null });
  } catch (error) {
    log(`ERROR ${error.message}`);
    writeState({ lastResult: 'error', lastError: error.message });
    process.exitCode = 1;
  } finally {
    if (!options.dryRun) releaseLock();
  }
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    console.error(`[hourly] ${error.message}`);
    process.exitCode = 1;
  }
}

module.exports = { parseArgs, pickDraft, runState, TAG_VERSION };
