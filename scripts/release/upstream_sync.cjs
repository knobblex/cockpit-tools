#!/usr/bin/env node
// This controller runs from the trusted workflow checkout, never from candidate code.
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const SHA = /^[a-f0-9]{40}$/;
const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const CANDIDATE = /^sync\/upstream-([1-9]\d*)-([a-f0-9]{40})-([a-f0-9]{40})$/;

function assertRepository(value) {
  if (typeof value !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_.-]*\/[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(value)) {
    throw new Error('Expected an owner/repository identifier');
  }
  return value;
}

function assertSha(value) {
  if (typeof value !== 'string' || !SHA.test(value)) throw new Error('Expected an immutable 40-character commit SHA');
  return value;
}

function releaseVersion(tag) {
  if (typeof tag !== 'string' || tag[0] !== 'v' || !VERSION.test(tag.slice(1))) {
    throw new Error('Only exact stable vMAJOR.MINOR.PATCH release tags are supported');
  }
  return tag.slice(1);
}

function validateRelease(release) {
  if (!release || release.draft !== false || release.prerelease !== false ||
      !Number.isSafeInteger(release.id) || release.id <= 0) {
    throw new Error('Expected a published, non-draft, non-prerelease release with a numeric ID');
  }
  releaseVersion(release.tag_name);
  return release;
}

function compareVersions(left, right) {
  if (!VERSION.test(left) || !VERSION.test(right)) throw new Error('package.json must use a stable numeric version');
  const a = left.split('.').map(BigInt);
  const b = right.split('.').map(BigInt);
  for (let i = 0; i < 3; i += 1) if (a[i] !== b[i]) return a[i] > b[i] ? 1 : -1;
  return 0;
}

function runGit(repoRoot, args) {
  return spawnSync('git', [
    '-c', 'core.hooksPath=/dev/null', '-c', 'commit.gpgsign=false', '-c', 'push.followTags=false',
    '-c', 'user.name=github-actions[bot]',
    '-c', 'user.email=41898282+github-actions[bot]@users.noreply.github.com', ...args,
  ], {
    cwd: repoRoot, encoding: 'utf8', timeout: 120_000, maxBuffer: 8 * 1024 * 1024,
    env: { ...process.env, GIT_TERMINAL_PROMPT: '0', GIT_NO_REPLACE_OBJECTS: '1' },
  });
}

function context(options, dependencies) {
  const repository = assertRepository(options.repository);
  const upstreamRepository = assertRepository(options.upstreamRepository);
  if (repository.toLowerCase() === upstreamRepository.toLowerCase()) throw new Error('Refusing to sync into the upstream repository');
  const branch = options.targetBranch;
  if (typeof branch !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._/-]*$/.test(branch) ||
      ['main', 'master'].includes(branch) || branch.startsWith('sync/')) {
    throw new Error('Expected a dedicated customization branch, not main/master or a sync branch');
  }
  const repoRoot = path.resolve(options.repoRoot || process.cwd());
  const run = dependencies.runGit || ((args) => runGit(repoRoot, args));
  const git = (args) => {
    const result = run(args);
    // Do not echo upstream-controlled diagnostics or potentially credential-bearing URLs.
    if (result.error || result.status !== 0) {
      const advice = args[0] === 'push'
        ? '. Check destination permissions, including permission to update workflow files; inspect the retained candidate before a manual retry.' : '';
      throw new Error(`git ${args[0]} failed (exit ${result.status ?? 'unavailable'})${advice}`);
    }
    return result.stdout.trim();
  };
  git(['check-ref-format', `refs/heads/${branch}`]);
  if (git(['rev-parse', '--is-shallow-repository']) !== 'false') throw new Error('A full-history checkout is required');
  const origin = git(['remote', 'get-url', 'origin']);
  const origins = dependencies.originUrl ? [dependencies.originUrl] : [
    `https://github.com/${repository}.git`, `https://github.com/${repository}`,
    `git@github.com:${repository}.git`, `ssh://git@github.com/${repository}.git`,
  ];
  if (!origins.includes(origin)) throw new Error('origin does not match the destination repository');
  const upstreamUrl = dependencies.upstreamUrl || `https://github.com/${upstreamRepository}.git`;
  const targetRef = `refs/heads/${branch}`;
  const tip = (ref) => {
    const result = git(['ls-remote', '--refs', 'origin', ref]);
    if (!result) return null;
    const fields = result.split(/\s+/);
    if (fields.length !== 2 || fields[1] !== ref) throw new Error('Unexpected remote ref response');
    return assertSha(fields[0]);
  };
  const fetchRef = (remote, ref, destination) => {
    git(['fetch', '--no-tags', remote, `+${ref}:${destination}`]);
    return assertSha(git(['rev-parse', `${destination}^{commit}`]));
  };
  const versionAt = (sha) => {
    const version = JSON.parse(git(['show', `${assertSha(sha)}:package.json`])).version;
    if (typeof version !== 'string' || !VERSION.test(version)) throw new Error('package.json must use a stable numeric version');
    return version;
  };
  const ancestor = (parent, child) => {
    const result = run(['merge-base', '--is-ancestor', assertSha(parent), assertSha(child)]);
    if (!result.error && result.status === 0) return true;
    if (!result.error && result.status === 1) return false;
    throw new Error('Could not verify commit ancestry');
  };
  const validateCandidate = (candidate, base, upstream, version) => {
    const parents = git(['show', '-s', '--format=%P', assertSha(candidate)]).split(' ');
    if (parents.length !== 2 || parents[0] !== base || parents[1] !== upstream || !ancestor(base, candidate)) {
      throw new Error('Candidate is not the exact two-parent merge of the captured customization and upstream commits');
    }
    if (versionAt(candidate) !== version) throw new Error('Candidate package version differs from the upstream release tag');
  };
  return { git, run, tip, fetchRef, versionAt, ancestor, validateCandidate, targetRef, upstreamUrl, upstreamRepository };
}

async function latestRelease(repository, token, fetchImpl = fetch) {
  const response = await fetchImpl(`https://api.github.com/repos/${assertRepository(repository)}/releases/latest`, {
    headers: { Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28',
      ...(token ? { Authorization: `Bearer ${token}` } : {}) },
    redirect: 'error', signal: AbortSignal.timeout(30_000),
  });
  if (response.status === 404) return null;
  if (!response.ok) throw new Error(`Upstream release lookup failed (HTTP ${response.status})`);
  return validateRelease(await response.json());
}

function candidateBranch(releaseId, base, upstream) {
  if (!Number.isSafeInteger(releaseId) || releaseId <= 0) throw new Error('Invalid release ID');
  return `sync/upstream-${releaseId}-${assertSha(base)}-${assertSha(upstream)}`;
}

async function prepare(options, dependencies = {}) {
  const ctx = context(options, dependencies);
  const release = await (dependencies.getLatestRelease || (() => latestRelease(ctx.upstreamRepository, options.token)))();
  if (release === null) return { changed: false, reason: 'No published stable upstream release.' };
  validateRelease(release);
  const version = releaseVersion(release.tag_name);
  const upstream = ctx.fetchRef(ctx.upstreamUrl, `refs/tags/${release.tag_name}`, 'refs/upstream-sync/upstream');
  const base = ctx.fetchRef('origin', ctx.targetRef, 'refs/upstream-sync/base');
  if (ctx.versionAt(upstream) !== version) throw new Error('Upstream tag differs from its package.json version');
  const identity = { base_sha: base, upstream_sha: upstream, release_tag: release.tag_name };
  if (ctx.ancestor(upstream, base)) return { changed: false, ...identity, reason: 'The customization branch already contains this release.' };
  if (compareVersions(version, ctx.versionAt(base)) <= 0) throw new Error('Refusing an upstream downgrade or a different commit for the same version');
  const branch = candidateBranch(release.id, base, upstream);
  const ref = `refs/heads/${branch}`;
  let existing = ctx.tip(ref);
  if (existing && !options.retryFailed) {
    return { changed: false, ...identity, candidate_branch: branch,
      reason: 'This release/base pair was already attempted. Inspect its run, then use Run workflow with retry=true to retry.' };
  }
  if (!existing) {
    // A durable attempt marker also covers conflicts, cancellations and interrupted builds.
    // An empty lease only creates an absent ref; it never replaces another attempt.
    ctx.git(['push', `--force-with-lease=${ref}:`, 'origin', `${base}:${ref}`]);
    existing = base;
  }
  let candidate;
  if (existing === base) {
    if (ctx.git(['status', '--porcelain']).length) throw new Error('Refusing to merge into a dirty controller checkout');
    ctx.git(['checkout', '--detach', base]);
    const merge = ctx.run(['merge', '--no-ff', '--no-edit', '-m', `Merge upstream ${release.tag_name} into ${options.targetBranch}`, upstream]);
    if (merge.error || merge.status !== 0) {
      // This checkout was clean and the merge belongs to this controller alone.
      ctx.run(['merge', '--abort']);
      throw new Error('Upstream merge failed. The customization branch is unchanged and the attempt marker is retained for manual retry.');
    }
    candidate = assertSha(ctx.git(['rev-parse', 'HEAD']));
    ctx.validateCandidate(candidate, base, upstream, version);
    // The candidate is proven to be a fast-forward of the marker before this CAS.
    ctx.git(['push', `--force-with-lease=${ref}:${base}`, 'origin', `${candidate}:${ref}`]);
  } else {
    candidate = ctx.fetchRef('origin', ref, 'refs/upstream-sync/candidate');
    if (candidate !== existing) throw new Error('Candidate changed while preparing a retry');
    ctx.validateCandidate(candidate, base, upstream, version);
  }
  return { changed: true, ...identity, candidate_sha: candidate, candidate_branch: branch,
    reason: 'Candidate prepared; the customization branch will advance only after all checks and builds succeed.' };
}

async function promote(options, dependencies = {}) {
  const ctx = context(options, dependencies);
  const base = assertSha(options.baseSha);
  const candidate = assertSha(options.candidateSha);
  const upstream = assertSha(options.upstreamSha);
  const version = releaseVersion(options.releaseTag);
  const parsed = typeof options.candidateBranch === 'string' && CANDIDATE.exec(options.candidateBranch);
  if (!parsed || parsed[2] !== base || parsed[3] !== upstream) throw new Error('Candidate branch identity differs from captured commits');
  const candidateRef = `refs/heads/${options.candidateBranch}`;
  if (ctx.fetchRef('origin', ctx.targetRef, 'refs/upstream-sync/base') !== base) {
    throw new Error('Customization branch changed during the build; refusing to overwrite newer work');
  }
  if (ctx.fetchRef('origin', candidateRef, 'refs/upstream-sync/candidate') !== candidate) {
    throw new Error('Candidate branch changed during the build');
  }
  if (ctx.fetchRef(ctx.upstreamUrl, `refs/tags/${options.releaseTag}`, 'refs/upstream-sync/upstream') !== upstream) {
    throw new Error('Upstream tag changed during the build');
  }
  if (ctx.versionAt(upstream) !== version || compareVersions(version, ctx.versionAt(base)) <= 0) {
    throw new Error('Release version no longer matches the captured upgrade');
  }
  ctx.validateCandidate(candidate, base, upstream, version);
  // The validated first-parent relationship makes this fast-forward only. The
  // exact lease adds an atomic compare-and-swap, including concurrent ref resets.
  ctx.git(['push', `--force-with-lease=${ctx.targetRef}:${base}`, 'origin', `${candidate}:${ctx.targetRef}`]);
  return { promoted: true, candidate_sha: candidate, release_tag: options.releaseTag,
    reason: 'All gates passed and the customization branch advanced to the tested candidate.' };
}

function writeResult(result, env = process.env) {
  if (env.GITHUB_OUTPUT) {
    fs.appendFileSync(env.GITHUB_OUTPUT, Object.entries(result).filter(([key]) => key !== 'reason')
      .map(([key, value]) => `${key}=${value}\n`).join(''));
  }
  const summary = `## Upstream sync\n\n${result.reason}\n\n` +
    ['release_tag', 'base_sha', 'upstream_sha', 'candidate_sha', 'candidate_branch']
      .filter((key) => result[key]).map((key) => `- ${key}: \`${result[key]}\`\n`).join('');
  if (env.GITHUB_STEP_SUMMARY) fs.appendFileSync(env.GITHUB_STEP_SUMMARY, summary);
  console.log(result.reason);
}

async function main(argv = process.argv.slice(2), env = process.env) {
  if (argv.length !== 1 || !['prepare', 'promote'].includes(argv[0])) throw new Error('Usage: upstream_sync.cjs prepare|promote');
  if (env.RETRY_FAILED && !['true', 'false'].includes(env.RETRY_FAILED)) throw new Error('RETRY_FAILED must be true or false');
  const options = {
    repoRoot: process.cwd(), repository: env.GITHUB_REPOSITORY, upstreamRepository: env.UPSTREAM_REPOSITORY,
    targetBranch: env.SYNC_BRANCH, token: env.GH_TOKEN, retryFailed: env.RETRY_FAILED === 'true',
    baseSha: env.BASE_SHA, candidateSha: env.CANDIDATE_SHA, candidateBranch: env.CANDIDATE_BRANCH,
    upstreamSha: env.UPSTREAM_SHA, releaseTag: env.RELEASE_TAG,
  };
  writeResult(await (argv[0] === 'prepare' ? prepare(options) : promote(options)), env);
}

if (require.main === module) main().catch((error) => {
  console.error(`Upstream sync stopped: ${error.message}`);
  if (process.env.GITHUB_STEP_SUMMARY) fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY,
    `## Upstream sync stopped\n\n${String(error.message).replace(/[<>`\r\n]/g, ' ')}\n\nThe customization branch was not intentionally rewritten. Inspect this run before retrying.\n`);
  process.exitCode = 1;
});

module.exports = { prepare, promote, latestRelease, validateRelease, releaseVersion, compareVersions, candidateBranch, runGit, writeResult, main };
