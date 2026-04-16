use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use regex::Regex;
use serde::Deserialize;

// Bundled fallback prompts (compiled into the binary from ./prompts/)
const DEFAULT_PLAN_PROMPT: &str = include_str!("../prompts/plan.prompt");
const DEFAULT_ARCHITECT_PROMPT: &str = include_str!("../prompts/architect.prompt");
const DEFAULT_RESEARCH_PROMPT: &str = include_str!("../prompts/research.prompt");
const DEFAULT_TODO_PROMPT: &str = include_str!("../prompts/todo.prompt");
const DEFAULT_TEST_PROMPT: &str = include_str!("../prompts/test.prompt");

// Bundled fallback patterns (compiled in from ./prompts/patterns.json)
const DEFAULT_PATTERNS_JSON: &str = include_str!("../prompts/patterns.json");

// ─── CLI ─────────────────────────────────────────────────────────────────────

/// Autonomous Claude Code agent driven by todo.md.
///
/// Runs `claude --print` in a loop, processing tasks from todo.md one at a
/// time. When todo.md is absent the plan phase creates it.
///
/// Task prefixes in todo.md select which prompt file to use.
/// The mapping is loaded from `prompts/patterns.json` in your --prompts repo
/// (or compiled-in defaults). Add entries there to extend the behaviour
/// without recompiling.
///
/// Default patterns (regex → prompt file):
///
///   ^ARCHITECT:\s*  →  architect.prompt
///   ^RESEARCH:\s*   →  research.prompt
///   ^TEST:\s*       →  test.prompt
///   .*              →  todo.prompt   (catch-all)
#[derive(Parser, Debug)]
#[command(
    name = "full-yolo",
    // Version = MAJOR.MINOR (Cargo.toml) + .shortsha (build.rs / GIT_SHA env)
    version = env!("FULL_YOLO_VERSION"),
    disable_version_flag = true,
)]
struct Cli {
    /// Print version (MAJOR.MINOR.shortsha) and exit
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
    version: (),

    /// GitHub "owner/repo" that hosts .prompt files and patterns.json.
    /// Files are fetched from:
    ///   https://raw.githubusercontent.com/{repo}/main/prompts/{name}.prompt
    ///   https://raw.githubusercontent.com/{repo}/main/prompts/patterns.json
    #[arg(short = 'p', long = "prompts", env = "FULL_YOLO_PROMPTS")]
    prompts_repo: Option<String>,

    /// Initial task description injected into {{TASK}} in plan.prompt when
    /// todo.md does not exist.
    #[arg(short = 't', long, env = "FULL_YOLO_TASK")]
    task: Option<String>,

    /// Path to todo.md (relative to --work-dir)
    #[arg(long, env = "FULL_YOLO_TODO", default_value = "todo.md")]
    todo: PathBuf,

    /// Path or name of the claude binary
    #[arg(short = 'c', long = "claude", env = "FULL_YOLO_CLAUDE_BIN", default_value = "claude")]
    claude_bin: String,

    /// Claude model alias or full model ID
    #[arg(long, env = "FULL_YOLO_MODEL", default_value = "sonnet")]
    model: String,

    /// Maximum spend per claude invocation in USD (omit for no limit)
    #[arg(long, env = "FULL_YOLO_MAX_BUDGET")]
    max_budget: Option<f64>,

    /// Seconds to sleep between task iterations
    #[arg(long, env = "FULL_YOLO_SLEEP", default_value = "2")]
    sleep: u64,

    /// Behaviour when all todo items are checked off
    #[arg(long, env = "FULL_YOLO_ON_COMPLETE", default_value = "exit", value_enum)]
    on_complete: OnComplete,

    /// Working directory passed to claude (defaults to current directory)
    #[arg(long, env = "FULL_YOLO_WORK_DIR")]
    work_dir: Option<PathBuf>,

    /// Directory used to cache downloaded .prompt and patterns.json files
    #[arg(long, env = "FULL_YOLO_PROMPT_CACHE", default_value = ".prompts")]
    prompt_cache: PathBuf,

    /// Re-download prompts even if cached copies exist
    #[arg(long)]
    no_cache: bool,

    /// Permission mode passed to claude
    #[arg(long, env = "FULL_YOLO_PERMISSION_MODE", default_value = "bypass", value_enum)]
    permission_mode: PermissionMode,

    /// Pass --bare to claude (skips CLAUDE.md, hooks, skills, MCP auto-discovery).
    /// Recommended for CI / fully scripted runs to get consistent behaviour
    /// regardless of local claude configuration.
    #[arg(short = 'b', long, env = "FULL_YOLO_BARE")]
    bare: bool,
}

#[derive(Debug, Clone, ValueEnum)]
enum OnComplete {
    /// Exit with code 0
    Exit,
    /// Sleep and keep polling todo.md for new items
    Wait,
    /// Delete todo.md and run the plan phase again
    Replan,
}

#[derive(Debug, Clone, ValueEnum)]
enum PermissionMode {
    /// --dangerously-skip-permissions (fully autonomous, no prompts)
    Bypass,
    /// No extra permission flag (default interactive – blocks headless runs)
    Default,
    /// --permission-mode plan (read-only dry-run proposals)
    Plan,
}

// ─── Pattern config ───────────────────────────────────────────────────────────

/// One entry from prompts/patterns.json
#[derive(Debug, Deserialize)]
struct PatternEntry {
    /// Regular expression matched against the raw task text (after `- [ ] `)
    regex: String,
    /// Name of the .prompt file to use (without extension)
    prompt: String,
    /// Whether to strip the matched prefix from the task description
    #[serde(default = "bool_true")]
    strip_prefix: bool,
    /// Human-readable description (documentation only, unused at runtime)
    #[serde(default)]
    #[allow(dead_code)]
    description: String,
}

fn bool_true() -> bool {
    true
}

/// Compiled pattern set loaded from patterns.json
struct PatternSet(Vec<(Regex, PatternEntry)>);

impl PatternSet {
    fn load(json: &str) -> Result<Self> {
        let entries: Vec<PatternEntry> =
            serde_json::from_str(json).context("Parsing patterns.json")?;
        let compiled = entries
            .into_iter()
            .map(|e| {
                let re = Regex::new(&e.regex)
                    .with_context(|| format!("Invalid regex '{}' in patterns.json", e.regex))?;
                Ok((re, e))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(PatternSet(compiled))
    }

    /// Return `(prompt_name, cleaned_description)` for a raw task string.
    fn classify<'a>(&self, raw: &'a str) -> (&str, String) {
        for (re, entry) in &self.0 {
            if re.is_match(raw) {
                let desc = if entry.strip_prefix {
                    re.replace(raw, "").trim().to_string()
                } else {
                    raw.to_string()
                };
                return (&entry.prompt, desc);
            }
        }
        // Should never reach here if patterns.json has a catch-all, but be safe
        ("todo", raw.to_string())
    }
}

// ─── Task model ───────────────────────────────────────────────────────────────

#[derive(Debug)]
struct TodoItem {
    /// 0-based line index of the `- [ ] …` line in the file
    line_index: usize,
    /// Prompt name resolved by pattern matching (e.g. "architect", "todo")
    prompt_name: String,
    /// Human-readable label for logging (e.g. "ARCHITECT")
    label: String,
    /// Task description with prefix stripped (if strip_prefix=true)
    description: String,
    /// Indented lines below the task item
    sub_steps: Vec<String>,
}

// ─── Entry point ──────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    let work_dir = match &cli.work_dir {
        Some(p) => p
            .canonicalize()
            .with_context(|| format!("--work-dir '{}' does not exist", p.display()))?,
        None => std::env::current_dir().context("Failed to get current directory")?,
    };
    let todo_path = work_dir.join(&cli.todo);

    eprintln!("╔══════════════════════════════════════════╗");
    eprintln!("║       full-yolo  –  autonomous agent     ║");
    eprintln!("╚══════════════════════════════════════════╝");
    eprintln!("  work dir : {}", work_dir.display());
    eprintln!("  todo     : {}", todo_path.display());
    eprintln!("  model    : {}", cli.model);
    if let Some(r) = &cli.prompts_repo {
        eprintln!("  prompts  : https://github.com/{r}/tree/main/prompts");
    } else {
        eprintln!("  prompts  : compiled-in fallbacks (set -p owner/repo to override)");
    }
    eprintln!();

    loop {
        match run_iteration(&cli, &work_dir, &todo_path) {
            Ok(true) => match cli.on_complete {
                OnComplete::Exit => {
                    eprintln!(">>> All tasks complete. Exiting.");
                    break;
                }
                OnComplete::Wait => {
                    eprintln!(">>> All tasks complete. Waiting for new items…");
                    thread::sleep(Duration::from_secs(cli.sleep.saturating_mul(10)));
                }
                OnComplete::Replan => {
                    eprintln!(">>> All tasks complete. Re-planning…");
                    if todo_path.exists() {
                        fs::remove_file(&todo_path).context("Removing todo.md for replan")?;
                    }
                }
            },
            Ok(false) => {
                thread::sleep(Duration::from_secs(cli.sleep));
            }
            Err(e) => {
                eprintln!(">>> ERROR: {e:#}");
                eprintln!(">>> Retrying in {} seconds…", cli.sleep.saturating_mul(3));
                thread::sleep(Duration::from_secs(cli.sleep.saturating_mul(3)));
            }
        }
    }

    Ok(())
}

// ─── Iteration logic ──────────────────────────────────────────────────────────

/// Returns `Ok(true)` when all items are done, `Ok(false)` after processing one.
fn run_iteration(cli: &Cli, work_dir: &Path, todo_path: &Path) -> Result<bool> {
    // Load pattern config (fetched or compiled-in)
    let patterns_json = fetch_file(cli, "patterns.json", Some(DEFAULT_PATTERNS_JSON))?;
    let patterns = PatternSet::load(&patterns_json)?;

    if !todo_path.exists() {
        eprintln!(">>> [PLAN] todo.md not found – running plan phase");
        run_plan(cli, work_dir, todo_path, &patterns)?;
        return Ok(false);
    }

    let content = fs::read_to_string(todo_path)
        .with_context(|| format!("Reading {}", todo_path.display()))?;

    let items = parse_todo(&content, &patterns);

    if items.is_empty() {
        return Ok(true);
    }

    // Special items (non-catch-all prompts) may only run once every item
    // above them in the file is checked off.  General items are always
    // eligible.  If the selected candidate is blocked, log and fall back to
    // the first eligible item; if nothing is eligible we are effectively done.
    let item = match select_next_item(&items, &content) {
        Some(i) => i,
        None => return Ok(true),
    };

    eprintln!(">>> [{}] {}", item.label.to_uppercase(), item.description);
    for step in &item.sub_steps {
        eprintln!("       - {step}");
    }

    let raw = fetch_file(cli, &format!("{}.prompt", item.prompt_name), None)?;
    let (meta, template) = parse_frontmatter(&raw);
    let prompt = fill_prompt(template, &item.description, &item.sub_steps);

    run_claude(cli, work_dir, &prompt, &meta)
        .with_context(|| format!("claude failed on: {}", item.description))?;

    mark_done(todo_path, item.line_index)?;
    eprintln!(">>> ✓  {}", item.description);

    Ok(false)
}

fn run_plan(
    cli: &Cli,
    work_dir: &Path,
    todo_path: &Path,
    _patterns: &PatternSet,
) -> Result<()> {
    let task = cli
        .task
        .as_deref()
        .unwrap_or("Analyse the current directory and plan the project");

    let raw = fetch_file(cli, "plan.prompt", Some(DEFAULT_PLAN_PROMPT))?;
    let (meta, template) = parse_frontmatter(&raw);
    let prompt = fill_prompt(template, task, &[]);

    run_claude(cli, work_dir, &prompt, &meta)?;

    if !todo_path.exists() {
        bail!(
            "Plan phase finished but {} was not created.\n\
             Check the plan prompt and ensure claude has write permissions.",
            todo_path.display()
        );
    }
    Ok(())
}

// ─── Item selection ───────────────────────────────────────────────────────────

/// Choose the next item to process.
///
/// Rule: a **special** item (anything that isn't the catch-all "todo" prompt)
/// may only run when every `- [ ]` line *above* it in the file is already
/// ticked off.  This ensures RESEARCH / ARCHITECT / TEST gates are not
/// bypassed if the file is manually edited or if a previous claude run added
/// items out of order.
///
/// If a special item is blocked, it is skipped and we keep looking for an
/// eligible item.  Returns `None` only when nothing is eligible (all remaining
/// unchecked items are blocked special items – treat as "all done for now").
fn select_next_item<'a>(items: &'a [TodoItem], raw_content: &str) -> Option<&'a TodoItem> {
    let raw_lines: Vec<&str> = raw_content.lines().collect();

    for item in items {
        if item.prompt_name == "todo" {
            // Catch-all / general items are always eligible
            return Some(item);
        }

        // Special item: verify no unchecked lines exist above it in the file.
        let unchecked_above = raw_lines[..item.line_index]
            .iter()
            .any(|l| l.trim_start().starts_with("- [ ] "));

        if unchecked_above {
            eprintln!(
                ">>> [{}] '{}' is deferred – unchecked items still exist above it",
                item.label.to_uppercase(),
                item.description
            );
            // continue to the next candidate (which has a higher line_index,
            // so it is even further into the file – it will fail too if the
            // same unchecked items are above it).  In practice this loop will
            // exhaust all items and return None, signalling "wait".
            continue;
        }

        return Some(item);
    }

    None
}

// ─── todo.md parser ───────────────────────────────────────────────────────────

fn parse_todo(content: &str, patterns: &PatternSet) -> Vec<TodoItem> {
    let mut items = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        if let Some(raw) = trimmed.strip_prefix("- [ ] ") {
            let raw = raw.trim();
            let (prompt_name, description) = patterns.classify(raw);

            // Derive a human-readable label from the raw prefix (e.g. "ARCHITECT")
            let label = raw
                .split(':')
                .next()
                .filter(|s| s.chars().all(|c| c.is_ascii_uppercase()))
                .unwrap_or("TODO")
                .to_string();

            // Collect indented sub-steps below this item
            let mut sub_steps = Vec::new();
            let mut j = i + 1;

            while j < lines.len() {
                let next = lines[j];
                let next_trimmed = next.trim_start();

                if next_trimmed.is_empty() {
                    j += 1;
                    continue;
                }

                let next_indent = next.len() - next_trimmed.len();

                if next_indent <= indent
                    || next_trimmed.starts_with("- [ ] ")
                    || next_trimmed.starts_with("- [x] ")
                    || next_trimmed.starts_with("- [X] ")
                {
                    break;
                }

                let step = next_trimmed.trim_start_matches("- ").trim().to_string();
                if !step.is_empty() {
                    sub_steps.push(step);
                }
                j += 1;
            }

            items.push(TodoItem {
                line_index: i,
                prompt_name: prompt_name.to_string(),
                label,
                description,
                sub_steps,
            });
        }

        i += 1;
    }

    items
}

fn mark_done(todo_path: &Path, line_index: usize) -> Result<()> {
    let content = fs::read_to_string(todo_path)?;
    let mut lines: Vec<String> = content.lines().map(String::from).collect();

    let line = lines
        .get_mut(line_index)
        .context("line_index out of range when marking task done")?;
    *line = line.replacen("- [ ] ", "- [x] ", 1);

    let mut out = lines.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    fs::write(todo_path, out)?;
    Ok(())
}

// ─── Prompt management ────────────────────────────────────────────────────────

/// Metadata parsed from YAML frontmatter at the top of a .prompt file.
///
/// Format:
/// ```yaml
/// ---
/// allowedTools:
///   - Read
///   - Write
///   - Bash
/// ---
/// … prompt body …
/// ```
#[derive(Debug, Default)]
struct PromptMeta {
    /// Tool names passed to `--allowed-tools` when invoking claude.
    /// Empty means no restriction (all tools allowed).
    allowed_tools: Vec<String>,
}

/// Split a prompt file into its frontmatter metadata and body.
///
/// If the file does not start with `---\n` the entire content is returned as
/// the body with an empty `PromptMeta`.
fn parse_frontmatter(content: &str) -> (PromptMeta, &str) {
    let mut meta = PromptMeta::default();

    let Some(rest) = content.strip_prefix("---\n") else {
        return (meta, content);
    };

    // Find the closing `---`
    let Some(end) = rest.find("\n---\n") else {
        return (meta, content);
    };

    let frontmatter = &rest[..end];
    let body = &rest[end + 5..]; // skip "\n---\n"

    // Minimal YAML parser: look for `allowedTools:` then collect `  - Tool` lines
    let mut in_tools = false;
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if trimmed == "allowedTools:" || trimmed.starts_with("allowedTools:") {
            in_tools = true;
            // Handle inline single-value: `allowedTools: Read` (rare but handle it)
            if let Some(val) = trimmed.strip_prefix("allowedTools:") {
                let val = val.trim();
                if !val.is_empty() {
                    meta.allowed_tools.push(val.to_string());
                    in_tools = false; // inline value, not a block list
                }
            }
        } else if in_tools {
            if let Some(tool) = trimmed.strip_prefix("- ") {
                meta.allowed_tools.push(tool.trim().to_string());
            } else if !line.starts_with(' ') && !line.starts_with('\t') {
                // Indentation dropped — end of the list
                in_tools = false;
            }
        }
    }

    (meta, body)
}

fn fill_prompt(template: &str, task: &str, sub_steps: &[String]) -> String {
    let steps_block = if sub_steps.is_empty() {
        "(no sub-steps provided)".to_string()
    } else {
        sub_steps
            .iter()
            .map(|s| format!("- {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    template
        .replace("{{TASK}}", task)
        .replace("{{SUB_STEPS}}", &steps_block)
}

/// Fetch a file (prompt or patterns.json) in order:
///   1. Local cache (`.prompts/` dir)
///   2. GitHub raw URL (`--prompts owner/repo`)
///   3. `fallback` compiled-in string (if provided)
fn fetch_file(cli: &Cli, name: &str, fallback: Option<&str>) -> Result<String> {
    let cache_file = cli.prompt_cache.join(name);

    // 1. Cache hit
    if !cli.no_cache && cache_file.exists() {
        return fs::read_to_string(&cache_file)
            .with_context(|| format!("Reading cached file {}", cache_file.display()));
    }

    // 2. GitHub raw
    if let Some(repo) = &cli.prompts_repo {
        let url = format!(
            "https://raw.githubusercontent.com/{repo}/main/prompts/{name}"
        );
        eprintln!("    fetching: {url}");
        match ureq::get(&url).call() {
            Ok(resp) => {
                let text = resp.into_string().context("Reading HTTP response body")?;
                if let Some(parent) = cache_file.parent() {
                    fs::create_dir_all(parent).ok();
                }
                fs::write(&cache_file, &text).ok();
                return Ok(text);
            }
            Err(e) => {
                eprintln!("    warning: fetch failed ({e}), using compiled-in fallback");
            }
        }
    }

    // 3. Compiled-in fallback (for built-in files only)
    if let Some(f) = fallback {
        return Ok(f.to_string());
    }

    // 4. Try built-in map for .prompt files
    let built_in = match name {
        "plan.prompt" => Some(DEFAULT_PLAN_PROMPT),
        "architect.prompt" => Some(DEFAULT_ARCHITECT_PROMPT),
        "research.prompt" => Some(DEFAULT_RESEARCH_PROMPT),
        "todo.prompt" => Some(DEFAULT_TODO_PROMPT),
        "test.prompt" => Some(DEFAULT_TEST_PROMPT),
        "patterns.json" => Some(DEFAULT_PATTERNS_JSON),
        _ => None,
    };

    built_in.map(str::to_string).ok_or_else(|| {
        anyhow::anyhow!(
            "No source for '{name}': not cached, --prompts not set or fetch failed, \
             and no compiled-in fallback exists"
        )
    })
}

// ─── Claude runner ────────────────────────────────────────────────────────────

fn run_claude(cli: &Cli, work_dir: &Path, prompt: &str, meta: &PromptMeta) -> Result<()> {
    let mut cmd = Command::new(&cli.claude_bin);

    cmd.arg("--print")
        .arg("--model")
        .arg(&cli.model)
        .arg("--output-format")
        .arg("text");

    if cli.bare {
        cmd.arg("--bare");
    }

    match cli.permission_mode {
        PermissionMode::Bypass => {
            cmd.arg("--dangerously-skip-permissions");
        }
        PermissionMode::Default => {}
        PermissionMode::Plan => {
            cmd.arg("--permission-mode").arg("plan");
        }
    }

    // Per-prompt tool allowlist from YAML frontmatter
    if !meta.allowed_tools.is_empty() {
        cmd.arg("--allowed-tools").arg(meta.allowed_tools.join(","));
        eprintln!("    tools: {}", meta.allowed_tools.join(", "));
    }

    if let Some(budget) = cli.max_budget {
        cmd.arg("--max-budget-usd").arg(budget.to_string());
    }

    cmd.current_dir(work_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn '{}'", cli.claude_bin))?;

    {
        let mut stdin = child.stdin.take().context("Failed to open claude stdin")?;
        stdin
            .write_all(prompt.as_bytes())
            .context("Writing prompt to claude stdin")?;
        // Drop closes the pipe → claude sees EOF and starts processing
    }

    let status = child.wait().context("Waiting for claude to finish")?;

    if !status.success() {
        bail!(
            "claude exited {}",
            status
                .code()
                .map(|c| format!("with code {c}"))
                .unwrap_or_else(|| "due to signal".to_string())
        );
    }

    Ok(())
}
