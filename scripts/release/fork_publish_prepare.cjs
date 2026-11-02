#!/usr/bin/env node
// Runs from the trusted default branch. Candidate files are parsed, never executed.
const fs = require('node:fs');
const { execFileSync } = require('node:child_process');
const { VERSION: versionPattern, compareVersions: compare, forkUpstream } = require('./fork-version.cjs');

const shaPattern = /^[a-f0-9]{40}$/;
const branch = 'codex/local-customizations';

function git(...args) {
  return execFileSync('git', args, { encoding: 'utf8', maxBuffer: 4 * 1024 * 1024 }).trim();
}

async function main() {
  if (process.env.GITHUB_REPOSITORY !== 'knobblex/cockpit-tools') throw new Error('Unexpected fork');
  const expected = process.env.EXPECTED_SOURCE || '';
  if (expected && !shaPattern.test(expected)) throw new Error('Expected an immutable source commit');
  git('fetch', '--no-tags', 'origin', `+refs/heads/${branch}:refs/fork-publish/source`);
  const source = git('rev-parse', 'refs/fork-publish/source^{commit}');
  if (!shaPattern.test(source) || (expected && source !== expected)) {
    throw new Error('Customization branch changed after the successful build');
  }
  const packageJson = JSON.parse(git('show', `${source}:package.json`));
  const version = packageJson.version;
  if (typeof version !== 'string' || !versionPattern.test(version)) throw new Error('Expected a stable package version');
  const upstream = forkUpstream(packageJson);
  if (upstream) {
    git('merge-base', '--is-ancestor', upstream.commit, source);
    if (JSON.parse(git('show', `${upstream.commit}:package.json`)).version !== upstream.version
        || compare(version, upstream.version) < 0) {
      throw new Error('Fork upstream baseline does not match the included source and release version');
    }
  }
  const candidateConfig = JSON.parse(git('show', `${source}:src-tauri/tauri.conf.json`));
  const controllerConfig = JSON.parse(fs.readFileSync('src-tauri/tauri.conf.json', 'utf8'));
  if (JSON.stringify(candidateConfig.plugins?.updater) !== JSON.stringify(controllerConfig.plugins?.updater)) {
    throw new Error('Candidate updater configuration differs from the trusted fork controller');
  }
  const response = await fetch(`https://api.github.com/repos/${process.env.GITHUB_REPOSITORY}/releases/latest`, {
    headers: { Accept: 'application/vnd.github+json', Authorization: `Bearer ${process.env.GH_TOKEN}` },
    redirect: 'error', signal: AbortSignal.timeout(30000),
  });
  let changed = true;
  if (response.status !== 404) {
    if (!response.ok) throw new Error(`Could not check the current fork release: HTTP ${response.status}`);
    const release = await response.json();
    const match = /^fork-v(\d+\.\d+\.\d+)$/.exec(release.tag_name || '');
    if (!match || !versionPattern.test(match[1]) || release.draft || release.prerelease) {
      throw new Error('Latest release is not a recognized stable fork release');
    }
    const order = compare(version, match[1]);
    if (order < 0) throw new Error('Refusing to publish an older version');
    changed = order > 0;
  }
  fs.appendFileSync(process.env.GITHUB_OUTPUT,
    `changed=${changed}\nsource=${source}\nversion=${version}\nupstream_version=${upstream?.version || version}\ntag=fork-v${version}\n`);
  fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY,
    `## Fork updater\n\n${changed ? 'Ready to build and publish' : 'This version is already published'}: \`${version}\` (\`${source}\`).\n`);
}

main().catch((error) => { console.error(error.message); process.exitCode = 1; });
