import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { fileURLToPath } from 'node:url';
import * as model from './ui-model.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

function loadProtectedRenderer() {
  const appPath = path.join(__dirname, 'secrets.js');
  const appSource = fs.readFileSync(appPath, 'utf8');
  const start = appSource.indexOf('function setRevealLabel');
  const end = appSource.indexOf('// Same rule as the TUI', start);
  assert.notEqual(start, -1, 'protected renderer start is present');
  assert.notEqual(end, -1, 'protected renderer end is present');

  const context = {
    XvUiModel: model,
    icon: (name) => ({ name }),
  };
  vm.runInNewContext(
    `'use strict';\n${appSource.slice(start, end)}\nglobalThis.renderProtectedControl = renderProtectedControl;`,
    context,
    { filename: appPath },
  );
  return context.renderProtectedControl;
}

// The asset test suite has no browser DOM dependency. These small controls
// preserve the relevant Web IDL distinction: textarea.type is read-only,
// while input.type is writable.
class MainSecretTextarea {
  constructor() {
    this.readOnly = false;
    this.value = '';
  }

  get type() { return 'textarea'; }
}

class RecordFieldInput {
  constructor() {
    this.readOnly = false;
    this.value = '';
    this._type = 'text';
  }

  get type() { return this._type; }
  set type(value) { this._type = value; }
}

class RevealButton {
  constructor(id) {
    this.id = id;
    this.textContent = '';
    this.children = [];
  }

  replaceChildren(...children) { this.children = children; }
}

test('protected renderer supports the main textarea and record-field input', () => {
  const renderProtectedControl = loadProtectedRenderer();
  const state = model.createProtectedState('stored secret', true);

  const textarea = new MainSecretTextarea();
  const mainButton = new RevealButton('reveal');
  assert.doesNotThrow(() => renderProtectedControl(textarea, mainButton, state));
  assert.equal(textarea.type, 'textarea');
  assert.equal(textarea.readOnly, true);
  assert.equal(textarea.value, model.PROTECTED_MASK);
  assert.equal(mainButton.children[1], 'Reveal');

  const input = new RecordFieldInput();
  const fieldButton = new RevealButton('field-reveal');
  assert.doesNotThrow(() => renderProtectedControl(input, fieldButton, state));
  assert.equal(input.type, 'text');
  assert.equal(input.readOnly, true);
  assert.equal(input.value, model.PROTECTED_MASK);
  assert.equal(fieldButton.textContent, 'Reveal');
});
