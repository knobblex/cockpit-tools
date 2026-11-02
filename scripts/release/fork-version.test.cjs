const assert = require('node:assert/strict');
const test = require('node:test');
const { forkUpstream, versionFile, nextForkVersion } = require('./fork-version.cjs');

test('fork versions append 1 to the upstream patch and keep subsequent releases increasing', () => {
  assert.equal(nextForkVersion('1.3.78', '1.3.78'), '1.3.781');
  assert.equal(nextForkVersion('1.3.781', '1.3.79'), '1.3.791');
  assert.equal(nextForkVersion('1.3.791', '1.4.0'), '1.4.1');
  assert.equal(nextForkVersion('1.3.781', '1.3.78'), '1.3.782');
  assert.equal(nextForkVersion('1.3.782', '1.3.78'), '1.3.783');
  assert.equal(nextForkVersion('1.3.799', '1.3.79'), '1.3.800');
  assert.equal(nextForkVersion('0.0.0', '0.0.0'), '0.0.1');
  assert.equal(nextForkVersion('1.3.1', '1.3.9007199254740993'), '1.3.90071992547409931');
});

test('the upstream identity permits only an exact stable version and immutable commit', () => {
  const identity = { version: '1.3.78', commit: 'a'.repeat(40) };
  assert.deepEqual(forkUpstream({ forkUpstream: identity }), identity);
  assert.equal(forkUpstream({}), null);
  for (const value of [null, [], {}, { ...identity, commit: 'main' }, { ...identity, version: '1.3.78+fork.1' },
    { ...identity, extra: true }, { ...identity, version: '1.3.78\noutput=injected' }]) {
    assert.throws(() => forkUpstream({ forkUpstream: value }));
  }
});

test('Cargo version edits affect only the local application and preserve dependency values and comments', () => {
  const manifest = '# release\n[package]\nname = "cockpit-tools"\nversion = "1.3.78" # application\n\n[dependencies]\nthing = { version = "1.3.78" }\n';
  const result = versionFile('src-tauri/Cargo.toml', manifest, '1.3.79');
  assert.deepEqual(result.versions, ['1.3.78']);
  assert.equal(result.text, manifest.replace('version = "1.3.78" # application', 'version = "1.3.79" # application'));
  const lock = 'version = 4\n\n[[package]]\nname = "cockpit-tools"\nversion = "1.3.78"\n\n[[package]]\nname = "dependency"\nversion = "1.3.78"\nsource = "registry+https://example.test"\n';
  assert.equal(versionFile('Cargo.lock', lock, '1.3.79').text, lock.replace('name = "cockpit-tools"\nversion = "1.3.78"', 'name = "cockpit-tools"\nversion = "1.3.79"'));
  assert.throws(() => versionFile('Cargo.lock', lock + '\n[[package]]\nname = "cockpit-tools"\nversion = "1.0.0"\n', '1.3.79'), /one local/);
  assert.throws(() => versionFile('src-tauri/Cargo.toml', '[package]\nname = "other"\nversion = "1.3.78"\n', '1.3.79'), /one local/);
});

test('JSON version normalization preserves every non-version field and only removes the fork baseline for a merge', () => {
  const original = { name: 'cockpit-tools', version: '1.3.78', forkUpstream: { version: '1.3.77', commit: 'a'.repeat(40) },
    scripts: { install: 'do-not-execute' }, dependencies: { example: '1.3.78' } };
  const normalized = JSON.parse(versionFile('package.json', JSON.stringify(original), '0.0.0', null).text);
  const { forkUpstream: _basis, ...expected } = original;
  assert.deepEqual(normalized, { ...expected, version: '0.0.0' });
  const identity = { version: '1.3.78', commit: 'b'.repeat(40) };
  const updated = JSON.parse(versionFile('package.json', JSON.stringify(original), '1.3.79', identity).text);
  assert.deepEqual(updated, { ...original, version: '1.3.79', forkUpstream: identity });
});
