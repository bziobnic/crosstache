import { test as base, expect } from '@playwright/test';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { once } from 'node:events';

const workspace = path.resolve(import.meta.dirname, '../..');

function run(command, args, options) {
  const child = spawn(command, args, options);
  return once(child, 'exit').then(([code]) => {
    if (code !== 0) throw new Error(`${command} ${args.join(' ')} exited with ${code}`);
  });
}

function startUi(binary, environment) {
  const child = spawn(binary, ['ui', '--no-open'], { cwd: workspace, env: environment });
  const url = new Promise((resolve, reject) => {
    let output = '';
    const capture = (chunk) => {
      output += chunk;
      const match = output.match(/xv ui listening at (http:\/\/127\.0\.0\.1:\d+\/\?token=[^\s]+)/);
      if (match) resolve(match[1]);
    };
    child.stdout.on('data', capture);
    child.stderr.on('data', capture);
    child.once('error', reject);
    child.once('exit', (code) => reject(new Error(`xv ui exited with ${code}: ${output}`)));
  });
  return { child, url };
}

export const test = base.extend({
  appUrl: async ({}, use) => {
    const home = await mkdtemp(path.join(tmpdir(), 'xv-playwright-'));
    const configHome = path.join(home, 'config');
    const dataHome = path.join(home, 'data');
    const store = path.join(home, 'store');
    await mkdir(configHome, { recursive: true });
    const config = `backend = "local"\n\n[local]\nstore_path = "${store}"\nkey_file = "${path.join(home, 'key.txt')}"\ndefault_vault = "playwright"\n`;
    await writeFile(path.join(configHome, 'xv.conf'), config);

    const target = path.join(workspace, 'target', 'debug', 'xv');
    await run('cargo', ['build', '--features', 'ui'], { cwd: workspace, stdio: 'inherit' });
    const environment = {
      PATH: process.env.PATH,
      HOME: home,
      XDG_CONFIG_HOME: configHome,
      XDG_DATA_HOME: dataHome,
      XV_BACKEND: 'local',
      XV_NO_PARENT_CONFIG: '1',
      DEFAULT_VAULT: 'playwright',
      NO_COLOR: '1',
      FORCE_COLOR: '0',
    };
    const server = startUi(target, environment);
    try {
      await use(await server.url);
    } finally {
      server.child.kill('SIGTERM');
      await once(server.child, 'exit').catch(() => {});
      await rm(home, { recursive: true, force: true });
    }
  },
});

export { expect };
