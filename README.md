# full-yolo 🤠

> Give it a task. Touch grass. Come back to shipped code.

**full-yolo** runs Claude Code in a headless loop, chewing through a `todo.md` one task at a time — planning, researching, architecting, implementing, and testing — until your backlog is gone or your credit card explodes.

---

## Quick start

### Binary

```bash
export ANTHROPIC_API_KEY=sk-ant-...

full-yolo -t "Build a REST API for a todo app in Rust" \
          -p KUKARAF/full_yolo        # GitHub repo that hosts the .prompt files
```

### Docker (batteries included: nix + uv + claude)

```bash
docker run -it --rm \
  -e ANTHROPIC_API_KEY=sk-ant-... \
  -v $(pwd):/workspace \
  ghcr.io/kukaraf/full-yolo:latest \
  -t "Build a REST API for a todo app in Rust" \
  -p KUKARAF/full_yolo
```

The container mounts your current directory as `/workspace`. Claude reads and writes files there. You watch in mild terror.

---

## How it works

1. **Plan** — no `todo.md`? Claude writes one, breaking your goal into steps
2. **Gate tasks run in order**: `RESEARCH:` → `ARCHITECT:` → implementation → `TEST:`
3. Special tasks only fire once everything above them is checked off ✅
4. Rinse, repeat, commit

### todo.md format

```markdown
- [ ] RESEARCH: Compare Rust HTTP frameworks
  - actix-web vs axum vs rocket
- [ ] ARCHITECT: Design the API layer
  - REST endpoints, auth strategy
- [ ] Implement /users CRUD
  - GET, POST, PATCH, DELETE
- [ ] TEST: Integration tests for /users
  - Happy path + error cases
```

| Prefix | What Claude does | Allowed tools |
|--------|-----------------|---------------|
| `RESEARCH:` | Web search + write a research doc | WebSearch, WebFetch, Read, Write |
| `ARCHITECT:` | Design doc + stubs | Read, Write, Edit, Glob |
| `GRAPHIC:` | Color palette (thecolorapi) + SVG/CSS via OpenRouter nanobanana | Bash, WebFetch, Read, Write |
| `TEST:` | Write + run tests | Bash, Read, Write, Edit |
| *(none)* | Implement it | Read, Write, Edit, Bash, Glob, Grep |

---

## Key flags

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--prompts owner/repo` | `-p` | — | GitHub repo hosting your `.prompt` files |
| `--task "…"` | `-t` | — | Initial goal (triggers plan phase) |
| `--claude path` | `-c` | `claude` | Path to the claude binary |
| `--model` | | `sonnet` | Any Claude model alias or ID |
| `--bare` | `-b` | off | Skip CLAUDE.md / hooks / MCP (good for CI) |
| `--max-budget-usd` | | — | Per-invocation spend cap |
| `--on-complete` | | `exit` | `exit` / `wait` / `replan` |
| `--version` | `-v` | — | `MAJOR.MINOR.shortsha` |

---

## Custom prompts

Fork this repo, edit `prompts/*.prompt`, update `prompts/patterns.json`, push. Done.

Add new task types by extending `patterns.json` — no recompile needed:

```json
{ "regex": "^DEPLOY:\\s*", "prompt": "deploy", "strip_prefix": true, "description": "Deploy tasks" }
```

Control which tools Claude can use per prompt via YAML frontmatter:

```yaml
---
allowedTools:
  - WebSearch
  - WebFetch
  - Write
---
You are a researcher...
```

---

## ⚠️ Warning

This tool will write code, run commands, make commits, and burn through API credits without stopping to ask if you're sure. Set `--max-budget-usd` if you enjoy sleeping. Use `--bare` in CI to avoid surprises from your local claude config.

---

## Version

`MAJOR.MINOR` is bumped manually in `Cargo.toml`. The short git SHA is injected at build time. GitHub Actions builds static binaries for Linux, macOS, and Windows on every push, and publishes a Docker image to GHCR.
