const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');
const { prepare, promote, latestRelease, validateRelease, compareVersions, candidateBranch, runGit, writeResult, main } = require('./upstream_sync.cjs');
const { VERSION_FILES, versionFile, nextForkVersion } = require('./fork-version.cjs');

function git(cwd, ...args) {
  const result = runGit(cwd, args);
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}

function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cockpit-upstream-sync-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const upstream = path.join(root, 'upstream');
  const remote = path.join(root, 'fork.git');
  const custom = path.join(root, 'custom');
  const checkout = path.join(root, 'controller');
  fs.mkdirSync(upstream);
  git(upstream, 'init', '-b', 'main');
  const write = (directory, file, value) => {
    fs.mkdirSync(path.dirname(path.join(directory, file)), { recursive: true });
    fs.writeFileSync(path.join(directory, file), value);
  };
  const commit = (directory, message) => {
    git(directory, 'add', '.'); git(directory, 'commit', '-m', message);
    return git(directory, 'rev-parse', 'HEAD');
  };
  write(upstream, 'package.json', JSON.stringify({ name: 'cockpit-tools', version: '1.0.0', description: 'original' }, null, 2) + '\n');
  write(upstream, 'package-lock.json', JSON.stringify({ name: 'cockpit-tools', version: '1.0.0', lockfileVersion: 3,
    packages: { '': { name: 'cockpit-tools', version: '1.0.0' }, 'node_modules/fixture': { version: '9.0.0' } } }, null, 2) + '\n');
  write(upstream, 'src-tauri/tauri.conf.json', JSON.stringify({ version: '1.0.0', identifier: 'example.cockpit' }, null, 2) + '\n');
  write(upstream, 'src-tauri/Cargo.toml', '[package]\nname = "cockpit-tools"\nversion = "1.0.0"\n\n[dependencies]\nfixture = "9.0.0"\n');
  write(upstream, 'Cargo.lock', 'version = 4\n\n[[package]]\nname = "cockpit-tools"\nversion = "1.0.0"\ndependencies = ["fixture"]\n\n[[package]]\nname = "fixture"\nversion = "9.0.0"\n');
  write(upstream, 'app.txt', 'old application\n');
  write(upstream, 'shared.txt', 'original\n');
  const old = commit(upstream, 'initial upstream');
  git(upstream, 'tag', 'v1.0.0');
  git(root, 'init', '--bare', remote);
  git(upstream, 'push', remote, 'main');
  git(remote, 'symbolic-ref', 'HEAD', 'refs/heads/main');
  git(root, 'clone', remote, custom);
  const targetBranch = 'codex/local-customizations';
  git(custom, 'checkout', '-b', targetBranch);
  write(custom, 'custom.txt', 'keep my customization\n');
  write(custom, 'shared.txt', 'custom behavior\n');
  const base = commit(custom, 'local customizations');
  git(custom, 'push', 'origin', targetBranch);
  git(root, 'clone', remote, checkout);
  const release = { id: 42, tag_name: 'v1.1.0', draft: false, prerelease: false };
  const options = { repoRoot: checkout, repository: 'owner/fork', upstreamRepository: 'upstream/project', targetBranch };
  const dependencies = { originUrl: remote, upstreamUrl: upstream, getLatestRelease: async () => release };
  const setVersions = (directory, version, basis = null) => {
    for (const file of VERSION_FILES) write(directory, file,
      versionFile(file, fs.readFileSync(path.join(directory, file), 'utf8'), version, basis).text);
  };
  const publish = (changes = {}) => {
    setVersions(upstream, release.tag_name.slice(1));
    write(upstream, 'app.txt', 'new application\n');
    for (const [file, value] of Object.entries(changes)) write(upstream, file, value);
    const sha = commit(upstream, 'new stable release');
    git(upstream, 'tag', release.tag_name);
    return sha;
  };
  const targetTip = () => git(remote, 'rev-parse', `refs/heads/${targetBranch}`);
  const promotion = (result) => ({ ...options, baseSha: result.base_sha, candidateSha: result.candidate_sha,
    candidateBranch: result.candidate_branch, upstreamSha: result.upstream_sha, releaseTag: result.release_tag });
  return { root, upstream, remote, custom, checkout, base, old, release, options, dependencies, publish, targetTip, promotion, commit, write, setVersions };
}

test('already included stable releases are a no-op without candidate refs', async (t) => {
  const f = fixture(t);
  f.release.tag_name = 'v1.0.0';
  const result = await prepare(f.options, f.dependencies);
  assert.equal(result.changed, false);
  assert.match(result.reason, /already contains/);
  assert.equal(f.targetTip(), f.base);
  assert.equal(git(f.remote, 'for-each-ref', '--format=%(refname)', 'refs/heads/sync/'), '');
});

test('new release preserves custom code, builds an immutable merge, and promotes only that merge', async (t) => {
  const f = fixture(t);
  const upstream = f.publish();
  const result = await prepare(f.options, f.dependencies);
  assert.equal(result.changed, true);
  assert.equal(result.upstream_sha, upstream);
  assert.equal(result.base_sha, f.base);
  assert.equal(git(f.remote, 'show', `${result.candidate_sha}:custom.txt`), 'keep my customization');
  assert.equal(git(f.remote, 'show', `${result.candidate_sha}:shared.txt`), 'custom behavior');
  assert.equal(git(f.remote, 'show', `${result.candidate_sha}:app.txt`), 'new application');
  assert.equal(f.targetTip(), f.base, 'prepare must not advance the customization branch');
  const promoted = await promote(f.promotion(result), f.dependencies);
  assert.equal(promoted.promoted, true);
  assert.equal(f.targetTip(), result.candidate_sha);
  assert.equal((await prepare(f.options, f.dependencies)).changed, false);
  assert.equal(git(f.remote, 'tag'), '', 'no upstream or release tag is pushed to the fork');
});

test('annotated release tags are peeled to immutable commits and are never implicitly pushed', async (t) => {
  const f = fixture(t);
  const upstream = f.publish();
  git(f.upstream, 'tag', '-d', f.release.tag_name);
  git(f.upstream, 'tag', '-a', '-m', 'annotated stable release', f.release.tag_name, upstream);
  assert.notEqual(git(f.upstream, 'rev-parse', `refs/tags/${f.release.tag_name}`), upstream);
  git(f.checkout, 'config', 'push.followTags', 'true');
  git(f.checkout, 'fetch', f.upstream, `refs/tags/${f.release.tag_name}:refs/tags/${f.release.tag_name}`);
  const result = await prepare(f.options, f.dependencies);
  assert.equal(result.upstream_sha, upstream);
  await promote(f.promotion(result), f.dependencies);
  assert.equal(git(f.remote, 'tag'), '', 'local Git followTags configuration cannot leak a release tag');
});

test('conflicts retain an attempt marker, leave custom untouched, and stop hourly repeats', async (t) => {
  const f = fixture(t);
  const upstream = f.publish({ 'shared.txt': 'upstream behavior\n' });
  await assert.rejects(prepare(f.options, f.dependencies), /merge failed/);
  const branch = candidateBranch(f.release.id, f.base, upstream);
  assert.equal(git(f.remote, 'rev-parse', `refs/heads/${branch}`), f.base);
  assert.equal(f.targetTip(), f.base);
  assert.equal(git(f.checkout, 'status', '--porcelain'), '');
  const skipped = await prepare(f.options, f.dependencies);
  assert.equal(skipped.changed, false);
  assert.match(skipped.reason, /already attempted/);
  await assert.rejects(prepare({ ...f.options, retryFailed: true }, f.dependencies), /merge failed/);
  assert.equal(f.targetTip(), f.base);
});

test('manual retry recovers an interrupted marker and reuses a previously built candidate exactly', async (t) => {
  const f = fixture(t);
  const upstream = f.publish();
  const branch = candidateBranch(f.release.id, f.base, upstream);
  git(f.custom, 'push', 'origin', `${f.base}:refs/heads/${branch}`);
  assert.equal((await prepare(f.options, f.dependencies)).changed, false);
  const result = await prepare({ ...f.options, retryFailed: true }, f.dependencies);
  assert.equal(result.changed, true);
  assert.equal((await prepare(f.options, f.dependencies)).changed, false);
  const retry = await prepare({ ...f.options, retryFailed: true }, f.dependencies);
  assert.equal(retry.candidate_sha, result.candidate_sha);
  assert.equal(f.targetTip(), f.base);
});

test('promotion refuses a customization branch advanced during the build', async (t) => {
  const f = fixture(t); f.publish();
  const result = await prepare(f.options, f.dependencies);
  f.write(f.custom, 'later.txt', 'new user work');
  const later = f.commit(f.custom, 'user keeps working');
  git(f.custom, 'push', 'origin', f.options.targetBranch);
  await assert.rejects(promote(f.promotion(result), f.dependencies), /Customization branch changed/);
  assert.equal(f.targetTip(), later);
});

test('promotion refuses candidate tampering and an upstream tag moved during the build', async (t) => {
  const f = fixture(t); f.publish();
  const result = await prepare(f.options, f.dependencies);
  const ref = `refs/heads/${result.candidate_branch}`;
  git(f.remote, 'update-ref', ref, f.base);
  await assert.rejects(promote(f.promotion(result), f.dependencies), /Candidate branch changed/);
  git(f.remote, 'update-ref', ref, result.candidate_sha);
  git(f.upstream, 'tag', '-f', f.release.tag_name, f.old);
  await assert.rejects(promote(f.promotion(result), f.dependencies), /Upstream tag changed/);
  assert.equal(f.targetTip(), f.base);
});

test('the exact promotion lease prevents a race after the base check, including a rewind', async (t) => {
  const f = fixture(t); f.publish();
  const result = await prepare(f.options, f.dependencies);
  let raced = false;
  const run = (args) => {
    if (args[0] === 'push' && args.at(-1).endsWith(`:refs/heads/${f.options.targetBranch}`)) {
      raced = true;
      git(f.remote, 'update-ref', `refs/heads/${f.options.targetBranch}`, f.old);
    }
    return runGit(f.checkout, args);
  };
  await assert.rejects(promote(f.promotion(result), { ...f.dependencies, runGit: run }), /git push failed/);
  assert.equal(raced, true);
  assert.equal(f.targetTip(), f.old, 'a concurrent user reset must not be overwritten');
});

test('a rejected candidate push retains its base marker and never reports a buildable candidate', async (t) => {
  const f = fixture(t); const upstream = f.publish();
  const branch = candidateBranch(f.release.id, f.base, upstream);
  const output = path.join(f.root, 'candidate-output');
  const run = (args) => args[0] === 'push' && !args.at(-1).startsWith(`${f.base}:`)
    ? { status: 1, stdout: '', stderr: 'workflow permission rejected' } : runGit(f.checkout, args);
  await assert.rejects(async () => {
    writeResult(await prepare(f.options, { ...f.dependencies, runGit: run }), { GITHUB_OUTPUT: output });
  }, /permission to update workflow files/);
  assert.equal(fs.existsSync(output), false);
  assert.equal(git(f.remote, 'rev-parse', `refs/heads/${branch}`), f.base);
  assert.equal(f.targetTip(), f.base);
  assert.equal((await prepare(f.options, f.dependencies)).changed, false);
});

test('a rejected promotion push preserves both refs and never reports success', async (t) => {
  const f = fixture(t); f.publish();
  const result = await prepare(f.options, f.dependencies);
  const output = path.join(f.root, 'promotion-output');
  const run = (args) => args[0] === 'push'
    ? { status: 1, stdout: '', stderr: 'permission rejected' } : runGit(f.checkout, args);
  await assert.rejects(async () => {
    writeResult(await promote(f.promotion(result), { ...f.dependencies, runGit: run }), { GITHUB_OUTPUT: output });
  }, /inspect the retained candidate before a manual retry/);
  assert.equal(fs.existsSync(output), false);
  assert.equal(f.targetTip(), f.base);
  assert.equal(git(f.remote, 'rev-parse', `refs/heads/${result.candidate_branch}`), result.candidate_sha);
});

test('promotion rejects forged parent identity before any protected push', async (t) => {
  const f = fixture(t); f.publish();
  const result = await prepare(f.options, f.dependencies);
  git(f.checkout, 'checkout', '--detach', result.candidate_sha);
  f.write(f.checkout, 'untested.txt', 'not in the tested merge');
  const forged = f.commit(f.checkout, 'untested child');
  git(f.checkout, 'push', 'origin', `${forged}:refs/heads/${result.candidate_branch}`);
  await assert.rejects(promote({ ...f.promotion(result), candidateSha: forged }, f.dependencies), /exact two-parent merge/);
  assert.equal(f.targetTip(), f.base);
});

test('same-version replacement, downgrade, and mismatched package versions fail before any attempt', async (t) => {
  for (const [tag, packageVersion, expected] of [
    ['v1.0.0', '1.0.0', /same version/], ['v0.9.0', '0.9.0', /downgrade/], ['v1.1.0', '1.2.0', /tag differs/],
  ]) {
    const f = fixture(t); f.release.tag_name = tag;
    if (tag === 'v1.0.0') git(f.upstream, 'tag', '-d', tag);
    f.publish({ 'package.json': JSON.stringify({ version: packageVersion }) });
    await assert.rejects(prepare(f.options, f.dependencies), expected);
    assert.equal(f.targetTip(), f.base);
    assert.equal(git(f.remote, 'for-each-ref', '--format=%(refname)', 'refs/heads/sync/'), '');
  }
});

test('a newer upstream release at or below the fork version still produces a strictly newer fork', async (t) => {
  for (const [current, expected] of [['1.1.0', '1.1.1'], ['1.4.9', '1.4.10'], ['1.0.9', '1.1.1']]) {
    const f = fixture(t);
    f.setVersions(f.custom, current, { version: '1.0.0', commit: f.old });
    const local = JSON.parse(fs.readFileSync(path.join(f.custom, 'package.json')));
    local.scripts = { postinstall: 'touch MUST_NOT_RUN' };
    f.write(f.custom, 'package.json', JSON.stringify(local, null, 2) + '\n');
    const base = f.commit(f.custom, 'independent fork release');
    git(f.custom, 'push', 'origin', f.options.targetBranch);
    const lock = JSON.parse(fs.readFileSync(path.join(f.upstream, 'package-lock.json')));
    lock.version = lock.packages[''].version = '1.1.0';
    lock.packages['node_modules/fixture'].version = '9.1.0';
    const upstream = f.publish({
      'package-lock.json': JSON.stringify(lock, null, 2) + '\n',
      'src-tauri/Cargo.toml': '[package]\nname = "cockpit-tools"\nversion = "1.1.0"\n\n[dependencies]\nfixture = "9.1.0"\n',
    });
    const result = await prepare(f.options, f.dependencies);
    assert.equal(result.candidate_version, expected);
    assert.equal(result.base_sha, base);
    assert.equal(git(f.remote, 'show', '-s', '--format=%P', result.candidate_sha), `${base} ${upstream}`);
    for (const file of VERSION_FILES) {
      assert.ok(versionFile(file, git(f.remote, 'show', `${result.candidate_sha}:${file}`), expected).versions
        .every((version) => version === expected), file);
    }
    const candidatePackage = JSON.parse(git(f.remote, 'show', `${result.candidate_sha}:package.json`));
    assert.deepEqual(candidatePackage.forkUpstream, { version: '1.1.0', commit: upstream });
    assert.deepEqual(candidatePackage.scripts, local.scripts);
    assert.equal(fs.existsSync(path.join(f.checkout, 'MUST_NOT_RUN')), false, 'candidate package scripts must never run');
    assert.equal(JSON.parse(git(f.remote, 'show', `${result.candidate_sha}:package-lock.json`)).packages['node_modules/fixture'].version, '9.1.0');
    assert.match(git(f.remote, 'show', `${result.candidate_sha}:src-tauri/Cargo.toml`), /fixture = "9\.1\.0"/);
    assert.equal(git(f.remote, 'show', `${result.candidate_sha}:shared.txt`), 'custom behavior');
    await promote(f.promotion(result), f.dependencies);
    assert.equal(f.targetTip(), result.candidate_sha);
    assert.equal((await prepare(f.options, f.dependencies)).changed, false);
  }
});

test('business conflicts inside a version file are never replaced by version normalization', async (t) => {
  const f = fixture(t);
  f.setVersions(f.custom, '1.4.9', { version: '1.0.0', commit: f.old });
  const local = JSON.parse(fs.readFileSync(path.join(f.custom, 'package.json')));
  local.description = 'local behavior';
  f.write(f.custom, 'package.json', JSON.stringify(local, null, 2) + '\n');
  const base = f.commit(f.custom, 'fork version and behavior');
  git(f.custom, 'push', 'origin', f.options.targetBranch);
  const incoming = JSON.parse(fs.readFileSync(path.join(f.upstream, 'package.json')));
  incoming.version = '1.1.0'; incoming.description = 'upstream behavior';
  f.publish({ 'package.json': JSON.stringify(incoming, null, 2) + '\n' });
  await assert.rejects(prepare(f.options, f.dependencies), /Non-version conflict in package.json/);
  assert.equal(f.targetTip(), base);
  assert.equal(git(f.checkout, 'status', '--porcelain'), '');
});

test('independent versions require a verifiable upstream baseline before creating an attempt', async (t) => {
  for (const basis of [null, { version: '1.0.1', commit: 'placeholder' }]) {
    const f = fixture(t);
    f.setVersions(f.custom, '1.4.9', basis ? { ...basis, commit: f.old } : null);
    const base = f.commit(f.custom, 'fork version without a valid baseline');
    git(f.custom, 'push', 'origin', f.options.targetBranch);
    f.publish();
    await assert.rejects(prepare(f.options, f.dependencies), /forkUpstream/);
    assert.equal(f.targetTip(), base);
    assert.equal(git(f.remote, 'for-each-ref', '--format=%(refname)', 'refs/heads/sync/'), '');
  }
});

test('the recorded upstream commit prevents a later upstream history rewrite', async (t) => {
  const f = fixture(t); f.publish();
  const first = await prepare(f.options, f.dependencies);
  await promote(f.promotion(first), f.dependencies);
  git(f.upstream, 'checkout', '--detach', f.old);
  f.release.tag_name = 'v1.2.0'; f.release.id += 1;
  f.publish();
  await assert.rejects(prepare(f.options, f.dependencies), /no longer contains the recorded baseline/);
  assert.equal(f.targetTip(), first.candidate_sha);
});

test('promotion validates all generated version carriers even when the merge parents are unchanged', async (t) => {
  const f = fixture(t); f.publish();
  const result = await prepare(f.options, f.dependencies);
  const lock = JSON.parse(fs.readFileSync(path.join(f.checkout, 'package-lock.json')));
  lock.packages[''].version = '1.0.0';
  f.write(f.checkout, 'package-lock.json', JSON.stringify(lock, null, 2) + '\n');
  git(f.checkout, 'add', 'package-lock.json');
  git(f.checkout, 'commit', '--amend', '--no-edit');
  const modified = git(f.checkout, 'rev-parse', 'HEAD');
  git(f.remote, 'fetch', f.checkout, modified);
  git(f.remote, 'update-ref', `refs/heads/${result.candidate_branch}`, modified);
  await assert.rejects(promote({ ...f.promotion(result), candidateSha: modified }, f.dependencies), /version mismatch in package-lock.json/);
  assert.equal(f.targetTip(), f.base);
});

test('version generation refuses candidate symlinks without writing outside the checkout', async (t) => {
  const f = fixture(t);
  const outside = path.join(f.root, 'outside.toml');
  fs.writeFileSync(outside, 'must stay unchanged\n');
  f.publish();
  const manifest = path.join(f.upstream, 'src-tauri/Cargo.toml');
  fs.unlinkSync(manifest);
  fs.symlinkSync(outside, manifest);
  f.commit(f.upstream, 'replace manifest with symlink');
  git(f.upstream, 'tag', '-f', f.release.tag_name);
  await assert.rejects(prepare(f.options, f.dependencies), /regular repository file/);
  assert.equal(fs.readFileSync(outside, 'utf8'), 'must stay unchanged\n');
  assert.equal(f.targetTip(), f.base);
  assert.equal(git(f.checkout, 'status', '--porcelain'), '');
});

test('a failed merge commit restores generated version files and retains only the attempt marker', async (t) => {
  const f = fixture(t); const upstream = f.publish();
  const run = (args) => args[0] === 'commit'
    ? { status: 1, stdout: '', stderr: 'simulated commit failure' } : runGit(f.checkout, args);
  await assert.rejects(prepare(f.options, { ...f.dependencies, runGit: run }), /git commit failed/);
  assert.equal(git(f.checkout, 'status', '--porcelain'), '');
  assert.equal(JSON.parse(fs.readFileSync(path.join(f.checkout, 'package.json'))).version, '1.0.0');
  assert.equal(git(f.remote, 'rev-parse', `refs/heads/${candidateBranch(f.release.id, f.base, upstream)}`), f.base);
  assert.equal(f.targetTip(), f.base);
});

test('inputs reject shell syntax, malformed releases, and unsupported refs', async (t) => {
  const f = fixture(t); f.publish();
  for (const tag of ['v1.2.3\nchanged=true', 'v1.2.3;touch BAD', '--upload-pack=bad', 'v1.2.3-rc.1', 'v01.2.3']) {
    assert.throws(() => validateRelease({ ...f.release, tag_name: tag }), /stable/);
  }
  for (const patch of [{ draft: true }, { prerelease: true }, { id: '42' }, { id: NaN }]) {
    assert.throws(() => validateRelease({ ...f.release, ...patch }), /published/);
  }
  for (const targetBranch of ['main', 'sync/a', '--delete', 'codex/a\nINJECTED', 'codex/../main']) {
    await assert.rejects(prepare({ ...f.options, targetBranch }, f.dependencies));
  }
  await assert.rejects(prepare({ ...f.options, repository: 'upstream/project' }, f.dependencies), /upstream repository/);
  await assert.rejects(prepare(f.options, { ...f.dependencies, originUrl: 'another repository' }), /origin/);
  assert.equal(f.targetTip(), f.base);
});

test('release lookup treats only 404 as no release and never treats API errors as no-op', async () => {
  const response = (status, body) => async () => ({ status, ok: status === 200, json: async () => body });
  assert.equal(await latestRelease('upstream/project', '', response(404)), null);
  await assert.rejects(latestRelease('upstream/project', '', response(403)), /HTTP 403/);
  await assert.rejects(latestRelease('upstream/project', '', response(500)), /HTTP 500/);
  await assert.rejects(latestRelease('upstream/project', '', response(200, { draft: false })), /published/);
});

test('missing stable releases do not mutate refs, and output writes report only completed results', async (t) => {
  const f = fixture(t);
  const result = await prepare(f.options, { ...f.dependencies, getLatestRelease: async () => null });
  assert.equal(result.changed, false);
  const output = path.join(f.root, 'output'); const summary = path.join(f.root, 'summary');
  writeResult(result, { GITHUB_OUTPUT: output, GITHUB_STEP_SUMMARY: summary });
  assert.equal(fs.readFileSync(output, 'utf8'), 'changed=false\n');
  assert.match(fs.readFileSync(summary, 'utf8'), /No published stable/);
  assert.equal(f.targetTip(), f.base);
  await assert.rejects(main(['unknown'], {}), /Usage/);
});

test('version comparison is numeric and handles large components without precision loss', () => {
  assert.equal(compareVersions('1.10.0', '1.9.9'), 1);
  assert.equal(compareVersions('1.0.0', '1.0.0'), 0);
  assert.equal(compareVersions('1.0.0', '2.0.0'), -1);
  assert.equal(compareVersions('9007199254740993.0.0', '9007199254740992.0.0'), 1);
  assert.equal(nextForkVersion('1.3.78', '1.3.78'), '1.3.781');
  assert.equal(nextForkVersion('1.3.781', '1.3.79'), '1.3.791');
  assert.equal(nextForkVersion('1.3.791', '1.4.0'), '1.4.1');
});
