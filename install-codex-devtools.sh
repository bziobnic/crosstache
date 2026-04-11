#!/usr/bin/env bash
# install-codex-devtools.sh
#
# Idempotent macOS installer for OpenAI Codex CLI and a practical local
# development toolchain. This script is intentionally conservative:
#   - it preserves existing ~/.codex/config.toml content
#   - it does not force API-key auth over ChatGPT login
#   - it uses `codex mcp add` for MCP servers instead of rewriting config
#   - it is safe to re-run

set -euo pipefail

log()  { printf '\033[1;34m[codex-setup]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[codex-setup]\033[0m %s\n' "$*" >&2; }
err()  { printf '\033[1;31m[codex-setup]\033[0m %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Usage: ./install-codex-devtools.sh [options]

Installs and configures a useful Codex CLI development setup on macOS.

Options:
  --with-api-key          If OPENAI_API_KEY is set or found in .env, write it to
                          ~/.codex/auth.env and source it from ~/.zshrc.
                          Default: prefer `codex login`.
  --skip-brew            Do not install Homebrew packages.
  --skip-mcp             Do not add MCP servers.
  --force-model          Set model to CODEX_MODEL even if config already has one.
  --model <name>         Model to use when config has none. Default: gpt-5.4.
  --dry-run              Print commands/changes without applying them.
  -h, --help             Show this help.

Environment:
  CODEX_HOME             Codex config directory. Default: ~/.codex.
  CODEX_MODEL            Default model if --model is not passed. Default: gpt-5.4.
  OPENAI_API_KEY         Used only with --with-api-key.
  GITHUB_PERSONAL_ACCESS_TOKEN or GITHUB_TOKEN
                          If set, adds the GitHub MCP server with that token.
EOF
}

DRY_RUN=0
SKIP_BREW=0
SKIP_MCP=0
WITH_API_KEY=0
FORCE_MODEL=0
CODEX_MODEL="${CODEX_MODEL:-gpt-5.4}"

while (($#)); do
  case "$1" in
    --with-api-key) WITH_API_KEY=1 ;;
    --skip-brew) SKIP_BREW=1 ;;
    --skip-mcp) SKIP_MCP=1 ;;
    --force-model) FORCE_MODEL=1 ;;
    --model)
      [[ $# -ge 2 ]] || err "--model requires an argument"
      CODEX_MODEL="$2"
      shift
      ;;
    --dry-run) DRY_RUN=1 ;;
    -h|--help) usage; exit 0 ;;
    *) err "Unknown option: $1" ;;
  esac
  shift
done

run() {
  if [[ "$DRY_RUN" == 1 ]]; then
    printf '[dry-run] %q' "$1"
    shift
    printf ' %q' "$@"
    printf '\n'
  else
    "$@"
  fi
}

backup() {
  local file=$1
  [[ -f "$file" ]] || return 0

  local ts dest
  ts=$(date +%Y%m%d-%H%M%S)
  dest="$file.bak.$ts"

  if [[ "$DRY_RUN" == 1 ]]; then
    log "Would back up $file -> $dest"
  else
    command cp -f "$file" "$dest"
    log "Backed up $file -> $dest"
  fi
}

append_once() {
  local file=$1
  local marker=$2
  local content=$3

  if [[ -f "$file" ]] && grep -Fq "$marker" "$file"; then
    log "$file already contains $marker"
    return 0
  fi

  if [[ "$DRY_RUN" == 1 ]]; then
    log "Would append $marker to $file"
  else
    printf '\n%s\n' "$content" >> "$file"
    log "Appended $marker to $file"
  fi
}

[[ "$(uname -s)" == "Darwin" ]] || err "This installer is macOS-only."

CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
CONFIG="$CODEX_HOME/config.toml"

if [[ "$SKIP_BREW" == 0 ]]; then
  if ! command -v brew >/dev/null 2>&1; then
    log "Installing Homebrew..."
    if [[ "$DRY_RUN" == 1 ]]; then
      log "Would download and run the official Homebrew installer"
    else
      /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
      if [[ -x /opt/homebrew/bin/brew ]]; then
        eval "$(/opt/homebrew/bin/brew shellenv)"
      elif [[ -x /usr/local/bin/brew ]]; then
        eval "$(/usr/local/bin/brew shellenv)"
      fi
    fi
  else
    log "Homebrew already installed."
  fi

  if command -v brew >/dev/null 2>&1; then
    BREW_FORMULAE=(
      ripgrep       # rg: fast search
      fd            # fast file finder
      jq            # JSON
      yq            # YAML/XML/TOML-ish workflows
      gh            # GitHub CLI
      fzf           # fuzzy finder
      bat           # readable cat
      git-delta     # readable git diff
      uv            # Python tools and uvx MCP servers
      node          # npx MCP servers
      eza           # readable ls
      tree          # directory overview
      git           # current git
      httpie        # friendly HTTP client
      coreutils     # gdate, gsed, etc.
      shellcheck    # shell linting
      shfmt         # shell formatting
      hyperfine     # quick benchmarking
      just          # command runner used by many repos
    )

    log "Installing/verifying Homebrew formulae..."
    for pkg in "${BREW_FORMULAE[@]}"; do
      if brew list --formula "$pkg" >/dev/null 2>&1; then
        log "  ok   $pkg"
      else
        log "  add  $pkg"
        run brew install "$pkg"
      fi
    done
  else
    warn "Homebrew is not available yet; skipping formula installation in this run."
  fi
fi

if command -v codex >/dev/null 2>&1; then
  log "Codex CLI already installed: $(codex --version 2>/dev/null || printf 'unknown version')"
else
  log "Installing Codex CLI..."
  if [[ "$DRY_RUN" == 1 ]]; then
    log "Would install Codex CLI via Homebrew cask, falling back to npm if needed."
  elif command -v brew >/dev/null 2>&1 && brew install --cask codex; then
      log "Installed Codex CLI via Homebrew cask."
  else
    warn "Homebrew cask install failed; falling back to npm."
    command -v npm >/dev/null 2>&1 || err "npm is required for fallback install."
    npm install -g @openai/codex
  fi
fi

if [[ "$DRY_RUN" == 0 ]]; then
  mkdir -p "$CODEX_HOME"
  touch "$CONFIG"
  chmod 600 "$CONFIG" 2>/dev/null || true
else
  log "Would ensure $CONFIG exists"
fi

backup "$CONFIG"

log "Merging Codex defaults into $CONFIG without removing existing settings..."
if [[ "$DRY_RUN" == 0 ]]; then
  python3 - "$CONFIG" "$CODEX_MODEL" "$FORCE_MODEL" <<'PY'
from pathlib import Path
import re
import sys

path = Path(sys.argv[1]).expanduser()
model = sys.argv[2]
force_model = sys.argv[3] == "1"
text = path.read_text() if path.exists() else ""
lines = text.splitlines()

def has_key(table, key):
    current = None
    for line in lines:
        match = re.match(r"\s*\[([^\]]+)\]\s*$", line)
        if match:
            current = match.group(1).strip()
            continue
        if current == table and re.match(rf"\s*{re.escape(key)}\s*=", line):
            return True
    return False

def set_top_key(key, value, force=False):
    first_table = next((i for i, line in enumerate(lines) if re.match(r"\s*\[", line)), len(lines))
    for i, line in enumerate(lines[:first_table]):
        if re.match(rf"\s*{re.escape(key)}\s*=", line):
            if force:
                lines[i] = f'{key} = {value}'
            return
    insert_at = 0
    while insert_at < len(lines) and (lines[insert_at].strip() == "" or lines[insert_at].lstrip().startswith("#")):
        insert_at += 1
    lines.insert(insert_at, f'{key} = {value}')

def ensure_table_key(table, key, value):
    if has_key(table, key):
        return

    header = f"[{table}]"
    for i, line in enumerate(lines):
        if line.strip() == header:
            insert_at = i + 1
            while insert_at < len(lines) and not re.match(r"\s*\[", lines[insert_at]):
                insert_at += 1
            lines.insert(insert_at, f'{key} = {value}')
            return

    if lines and lines[-1].strip():
        lines.append("")
    lines.extend([header, f'{key} = {value}'])

def has_table(table):
    return any(line.strip() == f"[{table}]" for line in lines)

set_top_key("model", f'"{model}"', force_model)
set_top_key("approval_policy", '"on-request"')
set_top_key("sandbox_mode", '"workspace-write"')
ensure_table_key("tools", "web_search", '{ context_size = "medium" }')
ensure_table_key("tools", "view_image", "true")

if not has_table("history"):
    if lines and lines[-1].strip():
        lines.append("")
    lines.extend(["[history]", 'persistence = "save-all"'])
elif not has_key("history", "persistence"):
    ensure_table_key("history", "persistence", '"save-all"')

path.write_text("\n".join(lines).rstrip() + "\n")
PY
else
  log "Would set missing Codex defaults: model=$CODEX_MODEL, approval_policy=on-request, sandbox_mode=workspace-write, web_search, view_image, history persistence"
fi

if [[ "$WITH_API_KEY" == 1 ]]; then
  resolve_api_key() {
    if [[ -n "${OPENAI_API_KEY:-}" ]]; then
      printf '%s' "$OPENAI_API_KEY"
      return 0
    fi

    local f key
    for f in "$PWD/.env" "$HOME/.env"; do
      [[ -f "$f" ]] || continue
      key=$(
        grep -E '^[[:space:]]*OPENAI_API_KEY[[:space:]]*=' "$f" 2>/dev/null \
          | head -1 \
          | sed -E 's/^[[:space:]]*OPENAI_API_KEY[[:space:]]*=[[:space:]]*//; s/^"//; s/"$//; s/^'\''//; s/'\''$//'
      )
      if [[ -n "$key" ]]; then
        printf '%s' "$key"
        return 0
      fi
    done
    return 1
  }

  AUTH_ENV="$CODEX_HOME/auth.env"
  if API_KEY=$(resolve_api_key); then
    backup "$AUTH_ENV"
    if [[ "$DRY_RUN" == 0 ]]; then
      (
        umask 077
        cat > "$AUTH_ENV" <<EOF
# Optional API-key auth for Codex CLI.
# Prefer `codex login` for normal interactive local use.
export OPENAI_API_KEY="$API_KEY"
EOF
      )
      chmod 600 "$AUTH_ENV"
    fi
    append_once "$HOME/.zshrc" "Codex CLI optional API-key auth" \
'# Codex CLI optional API-key auth
[ -f "$HOME/.codex/auth.env" ] && source "$HOME/.codex/auth.env"'
  else
    warn "--with-api-key was passed, but OPENAI_API_KEY was not found."
  fi
else
  log "Leaving authentication alone. Run 'codex login' if Codex is not already signed in."
fi

if [[ "$SKIP_MCP" == 0 ]]; then
  command -v codex >/dev/null 2>&1 || err "codex is required before MCP setup."

  mcp_add_if_missing() {
    local name=$1
    shift
    if codex mcp get "$name" >/dev/null 2>&1; then
      log "MCP server already configured: $name"
      return 0
    fi

    log "Adding MCP server: $name"
    run codex mcp add "$name" "$@"
  }

  # Official OpenAI docs. This is the highest-signal MCP for current Codex/API
  # behavior and avoids guessing from stale local knowledge.
  mcp_add_if_missing openaiDeveloperDocs --url https://developers.openai.com/mcp

  # Current library/framework docs, useful across most coding repos.
  mcp_add_if_missing context7 -- npx -y @upstash/context7-mcp

  # Browser automation/devtools for frontend debugging and screenshot checks.
  mcp_add_if_missing playwright -- npx -y @playwright/mcp@latest

  # GitHub API MCP is useful when a token is available. Otherwise `gh` remains
  # the safer default because it can use the user's existing GitHub auth.
  if [[ -n "${GITHUB_PERSONAL_ACCESS_TOKEN:-}" ]]; then
    mcp_add_if_missing github --env GITHUB_PERSONAL_ACCESS_TOKEN="$GITHUB_PERSONAL_ACCESS_TOKEN" -- npx -y @modelcontextprotocol/server-github
  elif [[ -n "${GITHUB_TOKEN:-}" ]]; then
    mcp_add_if_missing github --env GITHUB_PERSONAL_ACCESS_TOKEN="$GITHUB_TOKEN" -- npx -y @modelcontextprotocol/server-github
  else
    warn "Skipping GitHub MCP because no GitHub token is set. Use 'gh auth login' for CLI GitHub work."
  fi
fi

if command -v fzf >/dev/null 2>&1 && command -v brew >/dev/null 2>&1; then
  FZF_INSTALL="$(brew --prefix)/opt/fzf/install"
  if [[ -x "$FZF_INSTALL" ]]; then
    log "Installing fzf shell integration without editing shell rc files..."
    if [[ "$DRY_RUN" == 1 ]]; then
      log "Would run fzf shell integration installer"
    else
      "$FZF_INSTALL" --key-bindings --completion --no-update-rc --no-bash --no-fish >/dev/null 2>&1 || true
    fi
  fi
fi

log "Done."
cat <<EOF

Configured:
  Codex CLI:      $(command -v codex >/dev/null 2>&1 && codex --version 2>/dev/null || printf 'not found')
  Codex home:     $CODEX_HOME
  Config:         $CONFIG
  Default model:  $CODEX_MODEL (only changed if missing, unless --force-model was used)

Recommended next checks:
  codex mcp list
  codex

Notes:
  - Existing Codex projects, plugins, MCP servers, and auth files were preserved.
  - Run 'codex login' for normal local use unless you intentionally use --with-api-key.
  - Run 'gh auth login' for GitHub CLI workflows.
EOF
