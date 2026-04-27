# One-Command Setup Design Document

## 1. Overview

### 1.1 Goal
Reduce the adapter setup from a multi-step manual process (compile, copy config, edit TOML, export env vars, start) to a single command:

```bash
brew tap szj2ys/codex
brew install codex-responses-adapter
codex-responses-adapter setup
codex-responses-adapter
```

### 1.2 Scope
- GitHub Actions CI for cross-platform binary releases
- Homebrew Tap with auto-updating Formula
- Interactive `setup` subcommand for one-shot config generation
- Model name passthrough (no mapping conversion)

---

## 2. GitHub Actions CI Release

### 2.1 Trigger
Push a Git tag matching `v*`, e.g. `v0.1.0`.

### 2.2 Build Matrix
| Target | Platform | Artifact Name |
|--------|----------|---------------|
| `x86_64-apple-darwin` | macOS Intel | `codex-responses-adapt<SECRET_KEY> `aarch64-apple-darwin` | macOS Apple Silicon | `codex-responses-adapter-{version}-aarch64-apple-darwin.tar.gz` |
| `x86_64-unknown-linux-gnu` | Linux x86_64 | `codex-responses-adapter-{version}-x86_64-unknown-linux-gnu.tar.gz` |
| `aarch64-unknown-linux-gnu` | Linux ARM64 | `codex-responses-adapter-{version}-aarch64-unknown-linux-gnu.tar.gz` |

### 2.3 Release Artifacts
Each target produces:
- `codex-responses-adapter-{version}-{target}.tar.gz` containing the binary
- `sha256sum.txt` appended with that artifact's hash

### 2.4 Release Creation
GitHub Action creates a GitHub Release (or updates an existing draft) with all artifacts attached. A release note template is auto-generated from git log since last tag.

---

## 3. Homebrew Tap

### 3.1 Repository
`github.com/szj2ys/homebrew-codex` (or `homebrew-tap`), independent repo.

### 3.2 Formula
`Formula/codex-responses-adapter.rb` with platform-conditional `url`/`sha256` using `on_macos`/`on_linux` + `on_intel`/`on_arm` blocks.

### 3.3 Auto-Update
After the main repo CI finishes releasing, a secondary job:
1. Clones the tap repo
2. Updates Formula version, URLs, and sha256 values from the release
3. Commits and pushes directly to tap repo (no PR needed for simple version bumps)

### 3.4 User Installation
```bash
brew tap szj2ys/codex
brew install codex-responses-adapter
```

---

## 4. Setup Subcommand

### 4.1 CLI Addition
```bash
codex-responses-adapter setup    # interactive config generation
```

### 4.2 Interaction Flow
1. Detect existing `~/.codex-responses-adapter.toml`
   - If exists: prompt "Config already exists. Overwrite? [Y/n]"
   - If declined: exit with path hint
2. Prompt "Upstream URL: " (no default, required)
3. Prompt "API Key: " (hidden input, required)
4. Write minimal TOML to `~/.codex-responses-adapter.toml`
5. Print success + next step: `Run: codex-responses-adapter`

### 4.3 Generated Config
```toml
[server]
allow_downgrade = true
port = 3000

[providers.default]
base_url = "<user-input>"
api_key = "<user-input>"
provider_type = "custom"
```

No `models` section, no `default_route`, no `web_search`. These can be added manually for advanced use.

### 4.4 Implementation
- Use `std::io::{stdin, stdout, Write}` for prompts
- Use `rpassword` or similar for hidden input (or `termios` if avoiding new deps)
- Write via `std::fs::write` with `create_new` or overwrite after confirmation

---

## 5. Model Passthrough

### 5.1 Current Behavior
The adapter resolves model names via `model_routes` mapping. If a mapping exists, it replaces the Codex model name with the configured upstream model name. If no mapping exists, it falls through to `default_route` or passes the original name.

### 5.2 New Behavior
**Always passthrough.** The Codex model name is sent to the upstream provider unchanged.

This means:
- `models` config section is unnecessary for the 95% use case
- `default_route` is unnecessary when there is only one provider
- The adapter becomes a thin protocol translator, not a model router

### 5.3 Backward Compatibility
- Config file parsing continues to accept `models` and `default_route` fields (no breaking change)
- When `models` is present and non-empty, existing routing logic still applies
- When `models` is empty or absent, behavior is pure passthrough
- CLI `--model-map` and `--default-model` args remain functional

### 5.4 Code Change
In `src/handler.rs`, modify the route resolution logic:
- If `model_routes` is empty and `default_route` is None, construct a single route with `provider = "default"` and `model = codex_model.clone()`
- This eliminates the "no routes configured" error for minimal configs

---

## 6. Implementation Order

1. Add `setup` subcommand to CLI (`src/main.rs`)
2. Modify handler to support model passthrough when no routes configured
3. Add `rpassword` dependency (or use inline hidden input)
4. Write `.github/workflows/release.yml`
5. Create `szj2ys/homebrew-codex` tap repo with initial Formula
6. Add tap-update step to release workflow
7. Tag `v0.1.0` and verify end-to-end

---

## 7. Verification

### 7.1 Setup Subcommand
```bash
codex-responses-adapter setup
# Input URL and key
cat ~/.codex-responses-adapter.toml
# Should contain the minimal config
codex-responses-adapter
# curl http://127.0.0.1:3000/health → {"status":"ok"}
```

### 7.2 Model Passthrough
```bash
# With minimal config (no models), send a request with model "test-model"
# Verify upstream receives "test-model" unchanged in the chat request body
```

### 7.3 Homebrew Install
```bash
brew untap szj2ys/codex 2>/dev/null; brew tap szj2ys/codex
brew install codex-responses-adapter
which codex-responses-adapter
codex-responses-adapter --version
```
