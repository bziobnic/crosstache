import test from 'node:test';
import assert from 'node:assert/strict';
import {
  buildTypedDraft,
  conversionSummary,
  groupSuggestions,
  typeCards,
} from './ui-model.js';

const login = {
  name: 'login',
  source: 'builtin',
  fields: [
    { name: 'username', kind: 'metadata', required: true, primary: false },
    { name: 'otp', kind: 'secret', required: false, primary: false },
    { name: 'password', kind: 'secret', required: true, primary: true },
  ],
};

test('type cards expose required protected and primary fields', () => {
  const card = typeCards([login])[0];
  assert.deepEqual(card.required, ['username', 'password']);
  assert.deepEqual(card.protected, ['otp', 'password']);
  assert.equal(card.primary, 'password');
  assert.equal(card.source, 'builtin');
});

test('type cards isolate caller data and retain display metadata', () => {
  const source = structuredClone(login);
  const cards = typeCards([source]);
  source.fields[0].name = 'changed-after-render';
  assert.equal(cards[0].fields[0].name, 'username');
  assert.equal(cards[0].label, 'login');
  assert.equal(cards[0].source, 'builtin');
});

test('typed drafts preserve custom tags and untouched protected fields', () => {
  const draft = buildTypedDraft(login, {
    tags: {
      'xv-type': 'login',
      'f.username': 'alice',
      owner: 'payments',
    },
    protected: { password: 'stored-password', otp: 'stored-otp' },
    enabled: false,
    not_before: '2031-04-03T00:00:00Z',
  });
  draft.fields.username.value = 'bob';
  assert.deepEqual(draft.customTags, { owner: 'payments' });
  assert.equal(draft.fields.password.value, 'stored-password');
  assert.equal(draft.fields.password.dirty, false);
  assert.equal(draft.fields.otp.value, 'stored-otp');
  assert.equal(draft.enabled, false);
  assert.equal(draft.notBefore, '2031-04-03T00:00:00Z');
});

test('group suggestions are unique sorted values excluding selected groups', () => {
  assert.deepEqual(groupSuggestions([
    { groups: ['ops', 'prod'] },
    { groups: 'dev, ops' },
    { groups: ['Prod', 'qa'] },
  ], ['ops']), ['dev', 'prod', 'qa']);
});

test('conversion summaries describe loss exposure and missing fields without values', () => {
  assert.deepEqual(conversionSummary({
    dropped: ['legacy'],
    exposed: ['password'],
    renamed: ['token -> api-key'],
    missing_required: ['username'],
    requires_confirmation: true,
    source_revision: 'opaque-revision',
    supplied_fields: { password: 'must-not-escape' },
  }), {
    dropped: ['legacy'],
    exposed: ['password'],
    renamed: ['token → api-key'],
    missing: ['username'],
    requiresConfirmation: true,
    sourceRevision: 'opaque-revision',
    description: 'Drops 1 field; exposes 1 protected field; renames 1 field; needs 1 required field.',
  });
});
