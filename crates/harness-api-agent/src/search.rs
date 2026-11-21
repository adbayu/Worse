use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use grep_regex::RegexMatcherBuilder;
use grep_searcher::{
    BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkMatch,
};
use ignore::{WalkBuilder, WalkState};

pub const DEADLINE: Duration = Duration::from_secs(20);
pub const CONTEXT_MAX: usize = 3;
const FILE_MAX_BYTES: u64 = 2 * 1024 * 1024;
const PER_FILE_SHOWN: usize = 20;
const SHOWN_MAX: usize = 150;
const FILES_MAX: usize = 300;
const COUNT_MAX: usize = 5000;
const LINE_CHARS: usize = 240;
const OUTPUT_CHARS: usize = 12_000;

const SKIPPED_DIRS: [&str; 16] = [
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".git",
    ".hg",
    ".svn",
    ".next",
    ".nuxt",
    ".turbo",
    ".gradle",
    "vendor",
    "__pycache__",
    ".venv",
    "coverage",
];

pub struct Request {
    pub root: PathBuf,
    pub cwd: PathBuf,
    pub pattern: String,
    pub glob: Option<String>,
    pub insensitive: bool,
    pub files_only: bool,
    pub context: usize,
}

struct FileHits {
    shown: String,
    count: usize,
    listed: usize,
    lines: Vec<String>,
}

#[derive(Default)]
struct Shared {
    files: Mutex<Vec<FileHits>>,
    matches: AtomicUsize,
    shown: AtomicUsize,
    timed_out: AtomicBool,
}

struct Collect<'a> {
    shown: &'a str,
    shared: &'a Shared,
    stop: &'a (dyn Fn() -> bool + Sync),
    count: usize,
    listed: usize,
    last_listed: u64,
    context: u64,
    lines: Vec<String>,
    files_only: bool,
}

fn clip(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_end();
    if text.chars().count() <= LINE_CHARS {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(LINE_CHARS).collect();
    cut.push('…');
    cut
}

impl Collect<'_> {
    fn room(&self) -> bool {
        !self.files_only
            && self.listed < PER_FILE_SHOWN
            && self.shared.shown.load(Ordering::Relaxed) < SHOWN_MAX
    }

    fn keeps_context(&self, kind: &SinkContextKind, line: u64) -> bool {
        match kind {
            SinkContextKind::Before => self.room(),
            SinkContextKind::After => {
                !self.files_only && self.listed > 0 && line <= self.last_listed + self.context
            }
            SinkContextKind::Other => false,
        }
    }
}

impl Sink for Collect<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        if self.room() {
            let line = mat.line_number().unwrap_or(0);
            self.lines
                .push(format!("{}:{line}: {}", self.shown, clip(mat.bytes())));
            self.listed += 1;
            self.last_listed = line;
            self.shared.shown.fetch_add(1, Ordering::Relaxed);
        }
        self.count += 1;
        let total = self.shared.matches.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(total < COUNT_MAX && !(self.stop)())
    }

    fn context(&mut self, _: &Searcher, ctx: &SinkContext<'_>) -> Result<bool, Self::Error> {
        let line = ctx.line_number().unwrap_or(0);
        if self.keeps_context(ctx.kind(), line) {
            self.lines
                .push(format!("{}-{line}- {}", self.shown, clip(ctx.bytes())));
        }
        Ok(!(self.stop)())
    }

    fn context_break(&mut self, _: &Searcher) -> Result<bool, Self::Error> {
        if self.room() && self.listed > 0 {
            self.lines.push("--".to_owned());
        }
        Ok(true)
    }
}

fn walker(request: &Request) -> Result<WalkBuilder, String> {
    let mut walk = WalkBuilder::new(&request.root);
    walk.require_git(false)
        .max_filesize(Some(FILE_MAX_BYTES))
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_some_and(|t| t.is_dir())
                || !SKIPPED_DIRS
                    .iter()
                    .any(|name| entry.file_name().eq_ignore_ascii_case(name))
        });
    if let Some(glob) = &request.glob {
        let mut overrides = ignore::overrides::OverrideBuilder::new(&request.root);
        overrides
            .add(glob)
            .map_err(|e| format!("invalid glob: {e}"))?;
        walk.overrides(
            overrides
                .build()
                .map_err(|e| format!("invalid glob: {e}"))?,
        );
    }
    Ok(walk)
}

pub fn run(request: &Request, cancel: &AtomicBool, deadline: Duration) -> Result<String, String> {
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(request.insensitive)
        .build(&request.pattern)
        .map_err(|e| format!("invalid pattern: {e}"))?;
    let context = request.context.min(CONTEXT_MAX);
    let started = Instant::now();
    let shared = Shared::default();
    let stop = || {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        if started.elapsed() >= deadline {
            shared.timed_out.store(true, Ordering::Relaxed);
            return true;
        }
        shared.matches.load(Ordering::Relaxed) >= COUNT_MAX
    };
    let stop: &(dyn Fn() -> bool + Sync) = &stop;
    let shared_ref = &shared;
    walker(request)?.build_parallel().run(|| {
        let matcher = matcher.clone();
        let mut searcher = SearcherBuilder::new()
            .binary_detection(BinaryDetection::quit(0))
            .line_number(true)
            .before_context(context)
            .after_context(context)
            .build();
        Box::new(move |entry| {
            if stop() {
                return WalkState::Quit;
            }
            let Ok(entry) = entry else {
                return WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            let path = entry.path();
            let shown = path
                .strip_prefix(&request.cwd)
                .unwrap_or(path)
                .display()
                .to_string();
            let mut sink = Collect {
                shown: &shown,
                shared: shared_ref,
                stop,
                count: 0,
                listed: 0,
                last_listed: 0,
                context: context as u64,
                lines: Vec::new(),
                files_only: request.files_only,
            };
            let _ = searcher.search_path(&matcher, path, &mut sink);
            if sink.count > 0 {
                let hits = FileHits {
                    shown: shown.clone(),
                    count: sink.count,
                    listed: sink.listed,
                    lines: sink.lines,
                };
                if let Ok(mut files) = shared_ref.files.lock() {
                    files.push(hits);
                }
            }
            WalkState::Continue
        })
    });
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let timed_out = shared.timed_out.load(Ordering::Relaxed);
    let files = shared.files.into_inner().unwrap_or_default();
    Ok(render(files, request.files_only, timed_out, deadline))
}

fn render(
    mut files: Vec<FileHits>,
    files_only: bool,
    timed_out: bool,
    deadline: Duration,
) -> String {
    use std::fmt::Write as _;
    files.sort_by(|a, b| a.shown.cmp(&b.shown));
    let total: usize = files.iter().map(|f| f.count).sum();
    let mut out = String::new();
    if total == 0 {
        out.push_str("no matches");
    } else {
        let more = if total >= COUNT_MAX { "+" } else { "" };
        let _ = writeln!(out, "{total}{more} matches in {} files", files.len());
        if files_only {
            let mut by_count: Vec<&FileHits> = files.iter().collect();
            by_count.sort_by(|a, b| b.count.cmp(&a.count).then(a.shown.cmp(&b.shown)));
            for file in by_count.iter().take(FILES_MAX) {
                let _ = writeln!(out, "{} ({})", file.shown, file.count);
            }
            if files.len() > FILES_MAX {
                let _ = writeln!(out, "… {} more files", files.len() - FILES_MAX);
            }
        } else {
            let mut hidden_files = 0;
            for file in &files {
                if file.lines.is_empty() || out.len() >= OUTPUT_CHARS {
                    hidden_files += 1;
                    continue;
                }
                for line in &file.lines {
                    if out.len() + line.len() >= OUTPUT_CHARS {
                        break;
                    }
                    out.push_str(line);
                    out.push('\n');
                }
                if file.count > file.listed {
                    let _ = writeln!(
                        out,
                        "  … {} more in {}",
                        file.count - file.listed,
                        file.shown
                    );
                }
            }
            if hidden_files > 0 {
                let _ = writeln!(
                    out,
                    "… {hidden_files} more files with matches: use files_only, a narrower path or a glob"
                );
            }
        }
    }
    if timed_out {
        let _ = write!(
            out,
            "\n… stopped after {} s with partial results: narrow path or glob",
            deadline.as_secs()
        );
    }
    out.trim_end().to_owned()
}

#[must_use]
pub fn shell_search(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || matches!(c, '|' | ';' | '(' | ')' | '&'))
        .filter(|w| !w.is_empty())
        .collect();
    let has = |w: &str| words.contains(&w);
    let listing = words
        .iter()
        .any(|w| matches!(*w, "get-childitem" | "gci" | "dir" | "ls"));
    let recursive = words
        .iter()
        .any(|w| matches!(*w, "-recurse" | "-r" | "-rec" | "-s" | "/s"));
    if (has("select-string") || has("sls")) && listing && recursive {
        return true;
    }
    let grep = (has("grep") || has("egrep") || has("fgrep"))
        && words.iter().any(|w| {
            *w == "--recursive"
                || (w.starts_with('-')
                    && !w.starts_with("--")
                    && w[1..].chars().all(|c| c.is_ascii_alphabetic())
                    && w.contains(['r', 'R'].as_slice()))
        });
    let findstr = has("findstr") && has("/s");
    grep || findstr
}

#[cfg(test)]
mod tests;
