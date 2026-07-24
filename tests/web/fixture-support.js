function tomlString(value) {
  return JSON.stringify(String(value));
}

export function buildLocalConfig({ store, keyFile, vault }) {
  return `backend = "local"
debug = false
subscription_id = ""
default_vault = ${tomlString(vault)}
default_resource_group = ""
default_location = ""
tenant_id = ""
output_json = false
no_color = true
cache_enabled = false
cache_ttl_secs = 0
clipboard_timeout = 0

[local]
store_path = ${tomlString(store)}
key_file = ${tomlString(keyFile)}
default_vault = ${tomlString(vault)}
`;
}

export function waitForUiUrl(child, {
  timeoutMs = 15_000,
  setTimer = setTimeout,
  clearTimer = clearTimeout,
} = {}) {
  return new Promise((resolve, reject) => {
    let output = '';
    let settled = false;

    const cleanup = () => {
      clearTimer(timer);
      child.stdout.off('data', capture);
      child.stderr.off('data', capture);
      child.off('error', failed);
      child.off('exit', exited);
    };
    const finish = (callback, value) => {
      if (settled) return;
      settled = true;
      cleanup();
      callback(value);
    };
    const capture = (chunk) => {
      output += chunk;
      const match = output.match(/xv ui listening at (http:\/\/127\.0\.0\.1:\d+\/\?token=[^\s]+)/);
      if (match) finish(resolve, match[1]);
    };
    const failed = (error) => finish(reject, new Error(`xv ui failed to start: ${error.message}\n${output}`));
    const exited = (code) => finish(reject, new Error(`xv ui exited with ${code}: ${output}`));
    const timer = setTimer(
      () => finish(reject, new Error(`xv ui startup timed out after ${timeoutMs}ms: ${output}`)),
      timeoutMs,
    );

    child.stdout.on('data', capture);
    child.stderr.on('data', capture);
    child.once('error', failed);
    child.once('exit', exited);
  });
}
