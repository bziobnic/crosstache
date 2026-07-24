#!/usr/bin/env bash
# Exercise an unsigned local macOS bundle without touching user configuration.
set -euo pipefail

is_clean_app_exit_status() {
  [[ "$1" -eq 0 || "$1" -eq 143 ]]
}

if [[ "${XV_PACKAGE_SMOKE_TEST_WAIT_STATUS:-0}" == 1 ]]; then
  is_clean_app_exit_status 0
  is_clean_app_exit_status 143
  ! is_clean_app_exit_status 137
  ! is_clean_app_exit_status 1
  exit 0
fi

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
temp_parent="${TMPDIR:-/tmp}"
temp_root="$(mktemp -d "${temp_parent%/}/xv-package-smoke.XXXXXX")"
temp_parent="$(cd -P -- "$(dirname -- "$temp_root")" && pwd -P)"
temp_root="$(cd -P -- "$temp_root" && pwd -P)"
home_root="$temp_root/home"
config_root="$temp_root/config"
data_root="$temp_root/data"
log_root="$temp_root/logs"
log_file="$log_root/package-smoke.log"
app_pid=""
completed=0

fail() {
  printf 'package smoke failed: %s\nlogs retained at: %s\n' "$1" "$temp_root" >&2
  exit 1
}

validated_temp_root() {
  local resolved
  resolved="$(cd -P -- "$temp_root" && pwd -P)" || return 1
  [[ "$resolved" == "$temp_parent"/* ]] || return 1
  [[ "${resolved##*/}" =~ ^xv-package-smoke\.[[:alnum:]]{6}$ ]] || return 1
  [[ "$resolved" == "$temp_root" ]]
}

remove_temp_root() {
  [[ "${XV_PACKAGE_SMOKE_FORCE_CLEANUP_FAILURE:-0}" != 1 ]] || return 1
  rm -rf -- "$temp_root"
}

cleanup_completed_root() {
  if ! validated_temp_root; then
    printf 'package smoke refused cleanup outside its mktemp root: %s\n' "$temp_root" >&2
    return 1
  fi
  if ! remove_temp_root; then
    printf 'package smoke cleanup failed; logs retained at: %s\n' "$temp_root" >&2
    return 1
  fi
  if [[ -e "$temp_root" ]]; then
    printf 'package smoke cleanup left its mktemp root in place: %s\n' "$temp_root" >&2
    return 1
  fi
}

if [[ "${XV_PACKAGE_SMOKE_TEST_CLEANUP_STATUS:-0}" == 1 ]]; then
  cleanup_status=0
  XV_PACKAGE_SMOKE_FORCE_CLEANUP_FAILURE=1 cleanup_completed_root || cleanup_status=$?
  cleanup_retained=0
  [[ -d "$temp_root" ]] && cleanup_retained=1
  rm -rf -- "$temp_root"
  [[ "$cleanup_status" -eq 1 && "$cleanup_retained" -eq 1 ]]
  exit 0
fi

terminate_app() {
  local attempt exit_status watchdog_pid forced_termination
  if ! kill -0 "$app_pid" 2>/dev/null; then
    if wait "$app_pid" 2>/dev/null; then
      exit_status=0
    else
      exit_status=$?
    fi
    app_pid=""
    is_clean_app_exit_status "$exit_status"
    return
  fi
  kill -TERM "$app_pid" 2>/dev/null || return 1
  forced_termination="$log_root/forced-termination"
  (
    for attempt in $(seq 1 50); do
      if ! kill -0 "$app_pid" 2>/dev/null; then
        exit 0
      fi
      sleep 0.1
    done
    if ! kill -0 "$app_pid" 2>/dev/null; then
      exit 0
    fi
    : > "$forced_termination"
    kill -KILL "$app_pid" 2>/dev/null || exit 1
  ) &
  watchdog_pid=$!
  if wait "$app_pid" 2>/dev/null; then
    exit_status=0
  else
    exit_status=$?
  fi
  app_pid=""
  if wait "$watchdog_pid" 2>/dev/null; then
    :
  else
    return 1
  fi
  [[ ! -f "$forced_termination" ]] || return 2
  is_clean_app_exit_status "$exit_status"
}

finish() {
  local status=$?
  if [[ -n "$app_pid" ]] && kill -0 "$app_pid" 2>/dev/null; then
    local termination=0
    if terminate_app; then
      :
    else
      termination=$?
    fi
    if [[ "$termination" -ne 0 ]]; then
      completed=0
      printf 'package smoke could not cleanly terminate packaged executable; logs retained at: %s\n' "$temp_root" >&2
      [[ "$status" -eq 0 ]] && status=1
    fi
  fi
  if [[ "$status" -eq 0 && "$completed" -eq 1 ]]; then
    if cleanup_completed_root; then
      printf 'package smoke passed\n'
    else
      status=1
    fi
  fi
  trap - EXIT
  exit "$status"
}
trap finish EXIT

mkdir -p -- "$home_root" "$config_root" "$data_root" "$log_root"
[[ -d "$home_root" && -d "$config_root" && -d "$data_root" ]] || fail "isolated roots were not created"
validated_temp_root || fail "mktemp root did not resolve beneath its exact parent"

cd -- "$repo_root/desktop/src-tauri"
case "${XV_PACKAGE_SMOKE_SKIP_BUILD:-0}" in
  0) cargo tauri build --bundles app --no-sign --ci ;;
  1) ;;
  *) fail 'XV_PACKAGE_SMOKE_SKIP_BUILD must be 0 or 1' ;;
esac

bundle_root="$repo_root/target/release/bundle/macos"
[[ -d "$bundle_root" ]] || fail "macOS bundle output was not created"
app_bundle="$bundle_root/Crosstache Vault.app"
app_executable="$app_bundle/Contents/MacOS/xv-desktop"
[[ -d "$app_bundle" ]] || fail "expected unsigned .app bundle was not found"
[[ -x "$app_executable" ]] || fail "expected bundle executable was not found"

env -i \
  PATH="$PATH" \
  TMPDIR="$temp_parent" \
  HOME="$home_root" \
  XDG_CONFIG_HOME="$config_root" \
  XDG_DATA_HOME="$data_root" \
  XV_NO_PARENT_CONFIG=1 \
  XV_DESKTOP_PACKAGE_SMOKE_ROOT="$temp_root" \
  "$app_executable" >"$log_file" 2>&1 &
app_pid=$!

wait_for_marker() {
  local marker="$1"
  local attempt
  for attempt in $(seq 1 300); do
    if grep -Fxq -- "$marker" "$log_file"; then
      return 0
    fi
    if ! kill -0 "$app_pid" 2>/dev/null; then
      return 1
    fi
    sleep 0.1
  done
  return 1
}

wait_for_marker 'XV_PACKAGE_SMOKE_STATE=setup-required' \
  || fail "packaged executable did not report Setup Required"
wait_for_marker 'XV_PACKAGE_SMOKE_STATE=ready' \
  || fail "packaged executable did not complete isolated Local setup/list"
[[ -f "$config_root/xv/xv.conf" ]] || fail "Local setup did not save the isolated config"
[[ -f "$temp_root/local-key.txt" ]] || fail "Local setup did not create its isolated key"
[[ -f "$temp_root/store/vaults/package-smoke/.vault.json" ]] \
  || fail "Local setup/list did not initialize the isolated vault"

termination=0
if terminate_app; then
  :
else
  termination=$?
fi
[[ "$termination" -eq 0 ]] || fail "packaged executable did not terminate cleanly after SIGTERM"
completed=1
