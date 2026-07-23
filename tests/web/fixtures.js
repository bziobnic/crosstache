import { test as base, expect } from '@playwright/test';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { once } from 'node:events';
import { buildLocalConfig, waitForUiUrl } from './fixture-support.js';

const workspace = path.resolve(import.meta.dirname, '../..');

function run(command, args, options) {
  const child = spawn(command, args, options);
  return once(child, 'exit').then(([code]) => {
    if (code !== 0) throw new Error(`${command} ${args.join(' ')} exited with ${code}`);
  });
}

function startUi(binary, environment) {
  const child = spawn(binary, ['ui', '--no-open'], { cwd: workspace, env: environment });
  const url = waitForUiUrl(child);
  return { child, url };
}

export const test = base.extend({
  appUrl: async ({}, use) => {
    const home = await mkdtemp(path.join(tmpdir(), 'xv-playwright-'));
    const configHome = path.join(home, 'config');
    const dataHome = path.join(home, 'data');
    const store = path.join(home, 'store');
    const xvConfigHome = path.join(configHome, 'xv');
    await mkdir(xvConfigHome, { recursive: true });
    const config = buildLocalConfig({
      store,
      keyFile: path.join(home, 'key.txt'),
      vault: 'playwright',
    });
    await writeFile(path.join(xvConfigHome, 'xv.conf'), config);

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
      if (server.child.exitCode === null && server.child.signalCode === null) {
        server.child.kill('SIGTERM');
        await once(server.child, 'exit').catch(() => {});
      }
      await rm(home, { recursive: true, force: true });
    }
  },
});

export { expect };
