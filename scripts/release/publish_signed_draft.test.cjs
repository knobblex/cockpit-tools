const assert = require('node:assert/strict');
const test = require('node:test');
const { parseChecksums, validateNotarization, validateAssets, assetSnapshot } = require('./publish_signed_draft.cjs');

const version = '1.4.1';
const source = 'a'.repeat(40);
const identifier = 'com.jlcodes.cockpit-tools';
const team = 'TEAM123';
const identity = 'Developer ID Application: Fork (TEAM123)';

// The signed list covers every uploaded file except the two checksum files.
const checksumEntries = [
  ['Cockpit.Tools_1.4.1_aarch64.dmg', '1'],
  ['Cockpit.Tools_1.4.1_x64.dmg', '2'],
  ['Cockpit.Tools_1.4.1_universal.dmg', '3'],
  ['Cockpit.Tools_aarch64.app.tar.gz', '4'],
  ['Cockpit.Tools_x64.app.tar.gz', '5'],
  ['latest.json', '6'],
  ['latest-darwin-aarch64-app.json', '7'],
  ['latest-darwin-x86_64-app.json', '8'],
  ['latest-windows-x86_64-msi.json', '9'],
  ['latest-windows-x86_64-nsis.json', 'a'],
  ['latest-linux-x86_64-appimage.json', 'b'],
  ['latest-linux-x86_64-deb.json', 'c'],
  ['latest-linux-x86_64-rpm.json', 'd'],
  ['latest-linux-aarch64-appimage.json', 'e'],
  ['latest-linux-aarch64-deb.json', 'f'],
  ['latest-linux-aarch64-rpm.json', '0'],
  ['Fork-build.json', 'b1'],
  ['Apple-notarization.json', 'b2'],
].map(([name, digit]) => [name, digit.repeat(64)]);

const checksums = new Map(checksumEntries);

function buildFor(arch) {
  const outputs = [{ name: `Cockpit.Tools_${version}_${arch}.dmg`, sha256: checksums.get(`Cockpit.Tools_${version}_${arch}.dmg`) }];
  if (arch !== 'universal') outputs.push({ name: `Cockpit.Tools_${arch}.app.tar.gz`, sha256: checksums.get(`Cockpit.Tools_${arch}.app.tar.gz`) });
  return { arch, version, team, identity, bundleIdentifier: identifier, outputs,
    appSubmission: { id: `app-${arch}`, status: 'Accepted' }, dmgSubmission: { id: `dmg-${arch}`, status: 'Accepted' } };
}

function receipt(overrides = {}) {
  return { schema: 1, version, source, team, builds: ['aarch64', 'x64', 'universal'].map(buildFor), ...overrides };
}

function uploadedAssets() {
  const assets = [...checksums].map(([name, hash]) => ({ name, size: 10, state: 'uploaded', digest: `sha256:${hash}` }));
  assets.push({ name: 'SHA256SUMS.txt', size: 10, state: 'uploaded', digest: `sha256:${'1'.repeat(64)}` });
  assets.push({ name: 'SHA256SUMS.txt.sig', size: 10, state: 'uploaded', digest: `sha256:${'1'.repeat(64)}` });
  return assets;
}

const expected = { version, source, team, identity, identifier, checksums };

test('checksum lists reject duplicates, malformed lines and self-reference', () => {
  assert.equal(parseChecksums(`${'1'.repeat(64)}  latest.json\n`).size, 1);
  assert.throws(() => parseChecksums(`${'1'.repeat(64)}  latest.json\n${'2'.repeat(64)}  latest.json\n`), /duplicate/);
  assert.throws(() => parseChecksums(`${'1'.repeat(64)}  SHA256SUMS.txt\n`), /Invalid or duplicate/);
  assert.throws(() => parseChecksums(`${'1'.repeat(64)} latest.json\n`), /Invalid or duplicate/);
  assert.throws(() => parseChecksums('   \n'), /empty/);
});

test('a complete notarization record binds every signed output to the checksum list', () => {
  assert.doesNotThrow(() => validateNotarization(receipt(), expected));
  const pending = receipt();
  pending.builds[1].dmgSubmission = { id: 'dmg-x64', status: 'In Progress' };
  assert.throws(() => validateNotarization(pending, expected), /Missing accepted Apple submission for x64/);
  const rehashed = receipt();
  rehashed.builds[0].outputs[0].sha256 = 'f'.repeat(64);
  assert.throws(() => validateNotarization(rehashed, expected), /differs from the notarized file/);
});

test('notarization records from another release, team or architecture set are rejected', () => {
  assert.throws(() => validateNotarization(receipt({ source: 'b'.repeat(40) }), expected), /does not match this release/);
  assert.throws(() => validateNotarization(receipt({ schema: 2 }), expected), /does not match this release/);
  assert.throws(() => validateNotarization(receipt({ builds: [] }), expected), /exactly three architectures/);
  const duplicated = receipt();
  duplicated.builds[2] = buildFor('aarch64');
  assert.throws(() => validateNotarization(duplicated, expected), /unexpected architecture/);
  const otherTeam = receipt();
  otherTeam.builds[2].team = 'OTHERTEAM';
  assert.throws(() => validateNotarization(otherTeam, expected), /unexpected team/);
  const otherBundle = receipt();
  otherBundle.builds[0].bundleIdentifier = 'com.example.other';
  assert.throws(() => validateNotarization(otherBundle, expected), /unexpected bundle identifier/);
});

test('asset validation requires a digest match for every checksum entry', () => {
  assert.equal(validateAssets({ assets: uploadedAssets() }, checksums).size, checksums.size);

  const tampered = { assets: uploadedAssets() };
  tampered.assets[0].digest = `sha256:${'f'.repeat(64)}`;
  assert.throws(() => validateAssets(tampered, checksums), /does not match the signed checksum list/);

  const missingDigest = { assets: uploadedAssets() };
  delete missingDigest.assets[0].digest;
  assert.throws(() => validateAssets(missingDigest, checksums), /GitHub digest missing/);

  const unsigned = { assets: uploadedAssets().filter(asset => asset.name !== 'Apple-notarization.json') };
  assert.throws(() => validateAssets(unsigned, checksums), /missing Apple-notarization.json/);
});

test('unknown, duplicate or incomplete draft assets are rejected', () => {
  const extra = { assets: uploadedAssets() };
  extra.assets.push({ name: 'Cockpit.Tools_evil.dmg', size: 10, state: 'uploaded', digest: `sha256:${'1'.repeat(64)}` });
  assert.throws(() => validateAssets(extra, checksums), /missing from the signed checksum list|Invalid or duplicate/);

  const duplicated = { assets: uploadedAssets() };
  duplicated.assets.push({ ...duplicated.assets[0] });
  assert.throws(() => validateAssets(duplicated, checksums), /Invalid or duplicate draft asset/);

  const pending = { assets: uploadedAssets() };
  pending.assets[0].state = 'starter';
  assert.throws(() => validateAssets(pending, checksums), /Incomplete draft asset/);
});

test('a checksum list that omits an uploaded asset cannot be published', () => {
  const trimmed = new Map(checksums);
  trimmed.delete('Cockpit.Tools_x64.app.tar.gz');
  assert.throws(() => validateAssets({ assets: uploadedAssets() }, trimmed), /missing from the signed checksum list/);
});

test('a checksum list with an entry that was never uploaded cannot be published', () => {
  const padded = new Map(checksums);
  padded.set('Cockpit.Tools_ghost.dmg', '9'.repeat(64));
  assert.throws(() => validateAssets({ assets: uploadedAssets() }, padded), /does not cover every draft asset exactly once/);
});

test('asset snapshots detect a release mutated after verification', () => {
  const make = digest => ({ assets: [{ name: 'a.dmg', id: 1, size: 5, digest, state: 'uploaded' }] });
  assert.deepEqual(assetSnapshot(make('sha256:aa')), assetSnapshot(make('sha256:aa')));
  assert.notDeepEqual(assetSnapshot(make('sha256:aa')), assetSnapshot(make('sha256:bb')));
  assert.notDeepEqual(assetSnapshot(make('sha256:aa')), assetSnapshot(make('sha256:cc')));
});
