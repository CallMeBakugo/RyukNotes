import assert from 'node:assert/strict';
import test from 'node:test';

const {
  applyCommittedTagStyle,
  isValidTagColor,
  lookupTagStyle,
  normalizeTagStyleKey,
  tagColor,
  tagCustomImagePath,
  tagIterationKey,
  tagStyleKeyEquals
} = await import(new URL('./tag-styles.ts', import.meta.url));

test('normalizes tag style keys by trimming', () => {
  assert.equal(normalizeTagStyleKey('  work  '), 'work');
});

test('accepts 3 and 6 digit hex colors only', () => {
  assert.equal(isValidTagColor('#e11'), true);
  assert.equal(isValidTagColor('#e11d48'), true);
  assert.equal(isValidTagColor('#E11D48'), true);
  assert.equal(isValidTagColor('red'), false);
  assert.equal(isValidTagColor('#gg0000'), false);
  assert.equal(isValidTagColor('#e11d48aa'), false);
});

test('looks up tag styles with a case-insensitive fallback', () => {
  const styles = { Work: { icon: 'builtin:briefcase', color: '#e11d48' } };
  assert.equal(lookupTagStyle('Work', styles)?.icon, 'builtin:briefcase');
  assert.equal(lookupTagStyle('work', styles)?.color, '#e11d48');
  assert.equal(tagColor('missing', styles), null);
  assert.equal(tagStyleKeyEquals('Work', 'work'), true);
});

test('treats non-builtin icons as custom image paths', () => {
  const styles = { daily: { icon: '.ryuknotes/attachments/tag.png' } };
  assert.equal(tagCustomImagePath('daily', styles), '.ryuknotes/attachments/tag.png');
  assert.equal(tagCustomImagePath('daily', { daily: { icon: 'builtin:calendar' } }), null);
});


test('resetting a differently cased tag removes the stored style', () => {
  const styles = { Work: { icon: 'builtin:briefcase', color: '#e11d48' } };
  assert.deepEqual(applyCommittedTagStyle(styles, 'work', null), {});
});

test('saving a style replaces a differently cased stored key', () => {
  const styles = { Work: { icon: 'builtin:briefcase' } };
  const next = applyCommittedTagStyle(styles, 'work', {
    icon: 'builtin:star',
    color: '#ec4899'
  });
  assert.deepEqual(next, {
    Work: { icon: 'builtin:star', color: '#ec4899' }
  });
  assert.equal(Object.hasOwn(next, 'work'), false);
});


test('duplicate tag names still get unique each keys', () => {
  const tags = ['work', 'work'];
  const keys = tags.map((tag, index) => tagIterationKey(tag, index));
  assert.deepEqual(keys, ['0:work', '1:work']);
  assert.equal(new Set(keys).size, keys.length);
});
