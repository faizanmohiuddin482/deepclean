//! Deep-clean scanning, safety analysis, and safe removal.
//!
//! Safety model:
//! - `scan` is strictly read-only. It never deletes anything.
//! - Every location is limited to the fixed allow-list in `CATEGORIES`, all
//!   under the user's home directory.
//! - Categories that live inside ~/Library/Caches are NOT duplicated as their
//!   own entries (they already appear under "Application Caches"), so sizes are
//!   never double-counted.
//! - Before reporting, we probe running processes (via `lsof`) to flag items
//!   held open right now, which cleaning would interrupt.
//! - Each item carries a baseline risk: "safe" (rebuilt/re-downloaded for free)
//!   or "caution" (in use, or costly to re-download) — surfaced to the user.
//! - `clean` moves items to the Trash (recoverable) — never a permanent `rm` —
//!   and re-validates every path against the allow-list before touching it.

use rayon::prelude::*;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

#[derive(Clone, Copy, PartialEq)]
enum Risk {
    Safe,
    Caution,
}

#[derive(Serialize, Clone)]
pub struct PathEntry {
    pub path: String,
    pub label: String,
    pub size: u64,
    /// "safe" or "caution".
    pub risk: String,
    /// Running processes currently holding this item open (empty if none).
    pub in_use_by: Vec<String>,
    /// One-line explanation of the verdict / what happens if you clean it.
    pub advice: String,
}

#[derive(Serialize, Clone)]
pub struct CategoryResult {
    pub id: String,
    pub name: String,
    pub description: String,
    pub group: String,
    pub total_size: u64,
    pub item_count: usize,
    pub entries: Vec<PathEntry>,
    pub safe_to_clean: bool,
}

#[derive(Serialize, Clone)]
pub struct ScanReport {
    pub categories: Vec<CategoryResult>,
    pub total_size: u64,
    pub safe_size: u64,
    pub caution_size: u64,
    pub in_use_detection: bool,
}

#[derive(Serialize)]
pub struct CleanError {
    pub path: String,
    pub error: String,
}

#[derive(Serialize)]
pub struct CleanResult {
    pub trashed: usize,
    pub freed: u64,
    pub errors: Vec<CleanError>,
}

struct CategoryDef {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    group: &'static str,
    /// Home-relative roots. Multiple roots are merged into one category.
    roots: &'static [&'static str],
    enumerate_children: bool,
    safe_to_clean: bool,
    /// Baseline risk when the item is not currently in use.
    default_risk: Risk,
    /// Explanation shown when the item is not in use (educational microcopy).
    note: &'static str,
}

const CATEGORIES: &[CategoryDef] = &[
    // ---- System ----
    CategoryDef {
        id: "user-caches",
        name: "Application Caches",
        description: "Cached data your apps rebuild automatically.",
        group: "System",
        roots: &["Library/Caches"],
        enumerate_children: true,
        safe_to_clean: true,
        default_risk: Risk::Safe,
        note: "Rebuilt automatically the next time the app runs.",
    },
    CategoryDef {
        id: "logs",
        name: "Application Logs",
        description: "Old log files written by apps and the system.",
        group: "System",
        roots: &["Library/Logs"],
        enumerate_children: true,
        safe_to_clean: true,
        default_risk: Risk::Safe,
        note: "Old log output. Apps write fresh logs as needed.",
    },
    CategoryDef {
        id: "trash",
        name: "Trash",
        description: "Files already in your Trash.",
        group: "System",
        roots: &[".Trash"],
        enumerate_children: true,
        safe_to_clean: false,
        default_risk: Risk::Safe,
        note: "Already in the Trash — empty it from Finder to reclaim this.",
    },
    // ---- Developer ----
    CategoryDef {
        id: "xcode-derived",
        name: "Xcode Derived Data",
        description: "Build intermediates Xcode regenerates.",
        group: "Developer",
        roots: &["Library/Developer/Xcode/DerivedData"],
        enumerate_children: true,
        safe_to_clean: true,
        default_risk: Risk::Safe,
        note: "Xcode rebuilds this on your next build.",
    },
    CategoryDef {
        id: "npm-cache",
        name: "npm Cache",
        description: "npm's downloaded-package cache.",
        group: "Developer",
        roots: &[".npm/_cacache"],
        enumerate_children: false,
        safe_to_clean: true,
        default_risk: Risk::Safe,
        note: "npm re-downloads packages when you next install them.",
    },
    CategoryDef {
        id: "uv-cache",
        name: "uv Cache",
        description: "Astral uv's package & build cache.",
        group: "Developer",
        roots: &[".cache/uv"],
        enumerate_children: false,
        safe_to_clean: true,
        default_risk: Risk::Safe,
        note: "uv re-downloads on demand. Installed environments are untouched.",
    },
    // ---- AI tools ----
    CategoryDef {
        id: "hf-hub",
        name: "Hugging Face Models",
        description: "Downloaded model & dataset weights (~/.cache/huggingface).",
        group: "AI tools",
        roots: &[".cache/huggingface/hub"],
        enumerate_children: true,
        safe_to_clean: true,
        default_risk: Risk::Caution,
        note: "A downloaded model/dataset. You'll re-download it (often GBs) next time it's used.",
    },
    CategoryDef {
        id: "ollama",
        name: "Ollama Models",
        description: "Local LLM weights pulled by Ollama.",
        group: "AI tools",
        roots: &[".ollama/models"],
        enumerate_children: false,
        safe_to_clean: true,
        default_risk: Risk::Caution,
        note: "Local LLM weights. Removing means re-pulling them (can be many GB). Quit Ollama first.",
    },
    CategoryDef {
        id: "lm-studio",
        name: "LM Studio Models",
        description: "GGUF models downloaded by LM Studio.",
        group: "AI tools",
        roots: &[".lmstudio/models", ".cache/lm-studio/models"],
        enumerate_children: true,
        safe_to_clean: true,
        default_risk: Risk::Caution,
        note: "A downloaded model. Quit LM Studio first; you'll re-download it to use it again.",
    },
    CategoryDef {
        id: "ml-caches",
        name: "ML Model Caches",
        description: "PyTorch, MLX and Whisper download caches.",
        group: "AI tools",
        roots: &[".cache/torch", ".cache/mlx", ".cache/whisper"],
        enumerate_children: false,
        safe_to_clean: true,
        default_risk: Risk::Safe,
        note: "Model checkpoints that re-download on demand.",
    },
    CategoryDef {
        id: "claude-code",
        name: "Claude Code Logs",
        description: "Claude Code debug logs & shell snapshots.",
        group: "AI tools",
        roots: &[".claude/debug", ".claude/shell-snapshots"],
        enumerate_children: false,
        safe_to_clean: true,
        default_risk: Risk::Safe,
        note: "Debug logs and shell snapshots. Your projects and login stay untouched.",
    },
    CategoryDef {
        id: "cursor-cache",
        name: "Cursor Caches",
        description: "Editor caches from the Cursor app.",
        group: "AI tools",
        roots: &[
            "Library/Application Support/Cursor/Cache",
            "Library/Application Support/Cursor/CachedData",
            "Library/Application Support/Cursor/Code Cache",
            "Library/Application Support/Cursor/GPUCache",
            "Library/Application Support/Cursor/logs",
        ],
        enumerate_children: false,
        safe_to_clean: true,
        default_risk: Risk::Safe,
        note: "Rebuilt automatically. Your settings, projects and extensions stay untouched.",
    },
];

/// Snapshot of what's running: open files under $HOME (with the owning command)
/// and the set of all running command names.
struct SystemState {
    open_files: Vec<(String, PathBuf)>,
    probed: bool,
}

/// Ask `lsof` which files are currently open (unprivileged: sees the user's own
/// processes — exactly the ones that matter for cache/model safety).
fn probe_system(home: &Path) -> SystemState {
    let mut open_files = Vec::new();
    let output = Command::new("lsof").args(["-nPw", "-F", "pcn"]).output();

    let Ok(out) = output else {
        return SystemState {
            open_files,
            probed: false,
        };
    };

    let text = String::from_utf8_lossy(&out.stdout);
    let mut current_cmd = String::new();
    for line in text.lines() {
        let Some(tag) = line.chars().next() else {
            continue;
        };
        let rest = &line[1..];
        match tag {
            'c' => current_cmd = rest.to_string(),
            'n' => {
                if rest.starts_with('/') {
                    let p = PathBuf::from(rest);
                    if p.starts_with(home) {
                        open_files.push((current_cmd.clone(), p));
                    }
                }
            }
            _ => {}
        }
    }

    SystemState {
        open_files,
        probed: true,
    }
}

fn assess(path: &Path, def: &CategoryDef, sys: &SystemState) -> (String, Vec<String>, String) {
    let users: BTreeSet<String> = sys
        .open_files
        .iter()
        .filter(|(_, p)| p.starts_with(path))
        .map(|(cmd, _)| cmd.clone())
        .collect();

    if !users.is_empty() {
        let list: Vec<String> = users.into_iter().collect();
        let advice = format!(
            "In use by {}. Cleaning now may interrupt it or force it to rebuild this data.",
            list.join(", ")
        );
        return ("caution".to_string(), list, advice);
    }

    match def.default_risk {
        Risk::Safe => ("safe".to_string(), Vec::new(), def.note.to_string()),
        Risk::Caution => ("caution".to_string(), Vec::new(), def.note.to_string()),
    }
}

pub(crate) fn path_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

fn label_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

fn build_entry(path: &Path, label: String, def: &CategoryDef, sys: &SystemState) -> PathEntry {
    let (risk, in_use_by, advice) = assess(path, def, sys);
    PathEntry {
        path: path.to_string_lossy().into_owned(),
        label,
        size: path_size(path),
        risk,
        in_use_by,
        advice,
    }
}

fn scan_category(home: &Path, def: &CategoryDef, sys: &SystemState) -> Option<CategoryResult> {
    let mut candidates: Vec<(PathBuf, String)> = Vec::new();
    for root_rel in def.roots {
        let root = home.join(root_rel);
        if !root.exists() {
            continue;
        }
        if def.enumerate_children {
            if let Ok(rd) = std::fs::read_dir(&root) {
                for e in rd.filter_map(Result::ok) {
                    let p = e.path();
                    let label = label_of(&p);
                    candidates.push((p, label));
                }
            }
        } else {
            let label = if def.roots.len() > 1 {
                label_of(&root)
            } else {
                def.name.to_string()
            };
            candidates.push((root, label));
        }
    }
    if candidates.is_empty() {
        return None;
    }

    let mut entries: Vec<PathEntry> = candidates
        .par_iter()
        .map(|(p, label)| build_entry(p, label.clone(), def, sys))
        .filter(|e| e.size > 0)
        .collect();
    if entries.is_empty() {
        return None;
    }

    // Caution first, then largest first.
    entries.sort_by(|a, b| {
        let ac = a.risk == "caution";
        let bc = b.risk == "caution";
        bc.cmp(&ac).then(b.size.cmp(&a.size))
    });

    let total_size = entries.iter().map(|e| e.size).sum();
    Some(CategoryResult {
        id: def.id.to_string(),
        name: def.name.to_string(),
        description: def.description.to_string(),
        group: def.group.to_string(),
        total_size,
        item_count: entries.len(),
        entries,
        safe_to_clean: def.safe_to_clean,
    })
}

/// Read-only scan of every allow-listed category, with per-item safety analysis.
pub fn scan() -> Result<ScanReport, String> {
    let home = dirs::home_dir().ok_or("Could not determine your home directory")?;
    let sys = probe_system(&home);

    let categories: Vec<CategoryResult> = CATEGORIES
        .iter()
        .filter_map(|d| scan_category(&home, d, &sys))
        .collect();

    let mut total_size = 0u64;
    let mut safe_size = 0u64;
    let mut caution_size = 0u64;
    for cat in &categories {
        total_size += cat.total_size;
        if !cat.safe_to_clean {
            continue;
        }
        for e in &cat.entries {
            if e.risk == "caution" {
                caution_size += e.size;
            } else {
                safe_size += e.size;
            }
        }
    }

    Ok(ScanReport {
        categories,
        total_size,
        safe_size,
        caution_size,
        in_use_detection: sys.probed,
    })
}

/// Confirm a path is inside the user's home dir and free of `..` traversal.
fn ensure_under_home(path: &Path) -> Result<(), String> {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("path contains '..'".to_string());
    }
    let home = dirs::home_dir().ok_or("Could not determine your home directory")?;
    if path.starts_with(&home) {
        Ok(())
    } else {
        Err("path is outside your home directory".to_string())
    }
}

/// Reveal an item in Finder (read-only; changes nothing). Allowed anywhere in
/// the home dir or the Applications folders (so Inventory can reveal apps too).
pub fn reveal(path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("path contains '..'".to_string());
    }
    let revealable = ensure_under_home(&p).is_ok()
        || p.starts_with("/Applications")
        || p.starts_with("/System/Applications");
    if !revealable {
        return Err("path is not revealable".to_string());
    }
    Command::new("open")
        .arg("-R")
        .arg(&p)
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Preview an item with Quick Look (read-only; changes nothing).
pub fn quicklook(path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    ensure_under_home(&p)?;
    Command::new("qlmanage")
        .arg("-p")
        .arg(&p)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn validate(path: &Path, allowed_roots: &[PathBuf]) -> Result<(), String> {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("path contains '..'".to_string());
    }
    if !path.exists() {
        return Err("path no longer exists".to_string());
    }
    if allowed_roots.iter().any(|root| path.starts_with(root)) {
        Ok(())
    } else {
        Err("path is outside the allowed cleaning locations".to_string())
    }
}

/// Move the given paths to the Trash after re-validating each one.
pub fn clean(paths: Vec<String>) -> Result<CleanResult, String> {
    let home = dirs::home_dir().ok_or("Could not determine your home directory")?;
    let allowed_roots: Vec<PathBuf> = CATEGORIES
        .iter()
        .filter(|c| c.safe_to_clean)
        .flat_map(|c| c.roots.iter().map(|r| home.join(r)))
        .collect();

    let mut result = CleanResult {
        trashed: 0,
        freed: 0,
        errors: Vec::new(),
    };

    for p in paths {
        let path = PathBuf::from(&p);
        match validate(&path, &allowed_roots) {
            Ok(()) => {
                let size = path_size(&path);
                match trash::delete(&path) {
                    Ok(()) => {
                        result.trashed += 1;
                        result.freed += size;
                    }
                    Err(e) => result.errors.push(CleanError {
                        path: p,
                        error: e.to_string(),
                    }),
                }
            }
            Err(error) => result.errors.push(CleanError { path: p, error }),
        }
    }

    Ok(result)
}
