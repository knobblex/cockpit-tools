const assert = require('node:assert/strict');
const test = require('node:test');
const { parseArgs, pickDraft, TAG_VERSION } = require('./hourly_fork_release.cjs');

const TIP = 'a'.repeat(40);
const OTHER = 'b'.repeat(40);

function release(tag, extra = {}) {
  return { tag_name: tag, draft: true, prerelease: false, target_commitish: TIP, id: 1, ...extra };
}

test('only stable fork release tags are recognised', () => {
  assert.equal(TAG_VERSION.exec('fork-v1.3.783')[1], '1.3.783');
  assert.equal(TAG_VERSION.exec('fork-v1.4.0')[1], '1.4.0');
  for (const tag of ['v1.3.783', 'fork-v1.3', 'fork-v01.3.783', 'fork-v1.3.783-rc.1', 'fork-v1.3.783\n']) {
    assert.equal(TAG_VERSION.test(tag), false, tag);
  }
});

test('a draft that matches the current branch tip is selected', () => {
  const draft = release('fork-v1.3.791');
  assert.equal(pickDraft([draft], '1.3.783', TIP), draft);
});

test('drafts at or below the published version are ignored', () => {
  assert.equal(pickDraft([release('fork-v1.3.783')], '1.3.783', TIP), null);
  assert.equal(pickDraft([release('fork-v1.3.781')], '1.3.783', TIP), null);
});

test('a draft targeting another commit is ignored so stale builds cannot ship', () => {
  assert.equal(pickDraft([release('fork-v1.3.791', { target_commitish: OTHER })], '1.3.783', TIP), null);
});

test('published releases never masquerade as drafts', () => {
  assert.equal(pickDraft([release('fork-v1.3.791', { draft: false })], '1.3.783', TIP), null);
});

test('arguments default to a bounded wait and reject unknown input', () => {
  assert.deepEqual(parseArgs([]), { maxWaitMinutes: 50 });
  assert.equal(parseArgs(['--dry-run']).dryRun, true);
  assert.equal(parseArgs(['--max-wait-minutes', '20']).maxWaitMinutes, 20);
  assert.throws(() => parseArgs(['--max-wait-minutes', '0']));
  assert.throws(() => parseArgs(['--max-wait-minutes', '999']));
  assert.throws(() => parseArgs(['--force']));
});
