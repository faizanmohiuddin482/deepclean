//! Read-only inventory of what's installed on the machine.
//!
//! macOS gives us an honest "last used" only for `.app` bundles (Spotlight's
//! `kMDItemLastUsedDate`). For CLI tools there is no such signal, so we never
//! fake one — we surface size, install intent (Homebrew leaf vs dependency),
//! orphaned dependencies, and old runtime versions instead. Nothing is removed;
//! each item carries the exact command the user can run themselves.

use crate::cleaner::path_size;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize)]
pub struct InvItem {
    pub name: String,
    pub detail: String,
    pub size: u64,
    pub size_known: bool,
    pub flags: Vec<String>,
    /// Exact command the user can run to remove it (empty = no command).
    pub remove_command: String,
    /// Filesystem path, for items we can reveal in Finder (empty otherwise).
    pub path: String,
}

#[derive(Serialize)]
pub struct InvSection {
    pub id: String,
    pub name: String,
    pub subtitle: String,
    pub available: bool,
    pub note: String,
    pub total_size: u64,
    pub items: Vec<InvItem>,
}

#[derive(Serialize)]
pub struct Inventory {
    pub sections: Vec<InvSection>,
}

// ---- helpers -----------------------------------------------------------

fn output_of(cmd: &str, args: &[&str]) -> Option<String> {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn read_first_line(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
        .filter(|s| !s.is_empty())
}

fn dir_children(path: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(path)
        .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default()
}

fn total(items: &[InvItem]) -> u64 {
    items.iter().map(|i| i.size).sum()
}

fn sort_by_size(items: &mut [InvItem]) {
    items.sort_by(|a, b| b.size.cmp(&a.size));
}

// ---- Homebrew ----------------------------------------------------------

fn brew_path() -> Option<String> {
    for p in ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"] {
        if Path::new(p).exists() {
            return Some(p.to_string());
        }
    }
    None
}

fn parse_autoremove(text: &str) -> HashSet<String> {
    // Lines after the "==> Would remove ..." header list formula names.
    text.lines()
        .filter(|l| !l.trim_start().starts_with("==>") && !l.trim().is_empty())
        .flat_map(|l| l.split_whitespace())
        .map(|s| s.to_string())
        .collect()
}

fn brew_section() -> InvSection {
    let Some(brew) = brew_path() else {
        return InvSection {
            id: "homebrew".into(),
            name: "Homebrew".into(),
            subtitle: "Formulae & casks".into(),
            available: false,
            note: "Homebrew isn't installed on this Mac.".into(),
            total_size: 0,
            items: Vec::new(),
        };
    };

    let leaves: HashSet<String> = output_of(&brew, &["leaves"])
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let all: Vec<String> = output_of(&brew, &["list", "--formula", "-1"])
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let orphans = parse_autoremove(&output_of(&brew, &["autoremove", "--dry-run"]).unwrap_or_default());
    let cellar = output_of(&brew, &["--cellar"])
        .unwrap_or_default()
        .trim()
        .to_string();

    let mut items: Vec<InvItem> = Vec::new();
    for name in &all {
        let size = if cellar.is_empty() {
            0
        } else {
            path_size(&PathBuf::from(&cellar).join(name))
        };
        let is_orphan = orphans.contains(name);
        let is_leaf = leaves.contains(name);
        let (detail, flags, cmd) = if is_orphan {
            (
                "orphaned dependency — nothing needs it".to_string(),
                vec!["orphan".to_string()],
                "brew autoremove".to_string(),
            )
        } else if is_leaf {
            (
                "you installed this".to_string(),
                vec!["leaf".to_string()],
                format!("brew uninstall {name}"),
            )
        } else {
            (
                "dependency of another formula".to_string(),
                vec!["dependency".to_string()],
                String::new(),
            )
        };
        items.push(InvItem {
            name: name.clone(),
            detail,
            size,
            size_known: !cellar.is_empty(),
            flags,
            remove_command: cmd,
            path: String::new(),
        });
    }

    // Casks (sizes live in /Applications and show up under the Apps lens).
    for name in output_of(&brew, &["list", "--cask", "-1"])
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        items.push(InvItem {
            name: name.clone(),
            detail: "cask (app installed via Homebrew)".to_string(),
            size: 0,
            size_known: false,
            flags: vec!["cask".to_string()],
            remove_command: format!("brew uninstall --cask {name}"),
            path: String::new(),
        });
    }

    // Orphans first, then leaves, then everything by size.
    items.sort_by(|a, b| {
        let rank = |i: &InvItem| {
            if i.flags.iter().any(|f| f == "orphan") {
                0
            } else if i.flags.iter().any(|f| f == "leaf") {
                1
            } else {
                2
            }
        };
        rank(a).cmp(&rank(b)).then(b.size.cmp(&a.size))
    });

    let orphan_n = items.iter().filter(|i| i.flags.iter().any(|f| f == "orphan")).count();
    let subtitle = if orphan_n > 0 {
        format!(
            "{} formulae · {} orphaned (run `brew autoremove`)",
            all.len(),
            orphan_n
        )
    } else {
        format!("{} formulae, {} you installed directly", all.len(), leaves.len())
    };

    InvSection {
        id: "homebrew".into(),
        name: "Homebrew".into(),
        subtitle,
        available: true,
        note: String::new(),
        total_size: total(&items),
        items,
    }
}

// ---- language runtimes -------------------------------------------------

fn runtime_items(
    base: &Path,
    lang: &str,
    current: &Option<String>,
    uninstall: &str,
) -> Vec<InvItem> {
    let mut items = Vec::new();
    if !base.exists() {
        return items;
    }
    for dir in dir_children(base) {
        if !dir.is_dir() {
            continue;
        }
        let ver = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let is_current = current
            .as_ref()
            .map(|c| c == &ver || ver.contains(c) || c.contains(&ver))
            .unwrap_or(false);
        let (detail, flags) = if is_current {
            (format!("{lang} {ver} · current"), vec!["current".to_string()])
        } else {
            (format!("{lang} {ver}"), vec!["old".to_string()])
        };
        items.push(InvItem {
            name: format!("{lang} {ver}"),
            detail,
            size: path_size(&dir),
            size_known: true,
            flags,
            remove_command: if is_current {
                String::new()
            } else {
                format!("{uninstall} {ver}")
            },
            path: dir.to_string_lossy().into_owned(),
        });
    }
    items
}

fn runtimes_section(home: &Path) -> InvSection {
    let mut items = Vec::new();
    items.extend(runtime_items(
        &home.join(".nvm/versions/node"),
        "Node",
        &read_first_line(&home.join(".nvm/alias/default")),
        "nvm uninstall",
    ));
    items.extend(runtime_items(
        &home.join(".pyenv/versions"),
        "Python",
        &read_first_line(&home.join(".pyenv/version")),
        "pyenv uninstall",
    ));
    items.extend(runtime_items(
        &home.join(".rbenv/versions"),
        "Ruby",
        &read_first_line(&home.join(".rbenv/version")),
        "rbenv uninstall",
    ));

    let old = items.iter().filter(|i| i.flags.iter().any(|f| f == "old")).count();
    sort_by_size(&mut items);

    InvSection {
        id: "runtimes".into(),
        name: "Language versions".into(),
        subtitle: format!("{} installed · {} not the current version", items.len(), old),
        available: !items.is_empty(),
        note: if items.is_empty() {
            "No nvm / pyenv / rbenv versions found.".into()
        } else {
            String::new()
        },
        total_size: total(&items),
        items,
    }
}

// ---- applications ------------------------------------------------------

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn today_days() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86400) as i64)
        .unwrap_or(0)
}

/// Days since the app was last opened; None if Spotlight has no record.
fn last_used_days(app: &Path) -> Option<i64> {
    let raw = output_of(
        "mdls",
        &["-name", "kMDItemLastUsedDate", "-raw", &app.to_string_lossy()],
    )?;
    let raw = raw.trim();
    if raw.is_empty() || raw == "(null)" || raw.len() < 10 {
        return None;
    }
    let y: i64 = raw[0..4].parse().ok()?;
    let m: i64 = raw[5..7].parse().ok()?;
    let d: i64 = raw[8..10].parse().ok()?;
    Some((today_days() - days_from_civil(y, m, d)).max(0))
}

fn describe_age(days: Option<i64>) -> (String, Vec<String>) {
    match days {
        None => ("never opened".to_string(), vec!["unused".to_string()]),
        Some(d) if d >= 365 => (
            format!("last used {:.1} yr ago", d as f64 / 365.0),
            vec!["unused".to_string()],
        ),
        Some(d) if d >= 180 => (
            format!("last used {} months ago", d / 30),
            vec!["unused".to_string()],
        ),
        Some(d) if d >= 30 => (format!("last used {} months ago", d / 30), vec![]),
        Some(d) => (format!("last used {} days ago", d), vec![]),
    }
}

fn apps_section(home: &Path) -> InvSection {
    let roots = [PathBuf::from("/Applications"), home.join("Applications")];
    let mut items = Vec::new();
    for root in &roots {
        for app in dir_children(root) {
            if app.extension().and_then(|e| e.to_str()) != Some("app") {
                continue;
            }
            let name = app
                .file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let days = last_used_days(&app);
            let (detail, flags) = describe_age(days);
            items.push(InvItem {
                name,
                detail,
                size: path_size(&app),
                size_known: true,
                flags,
                remove_command: String::new(),
                path: app.to_string_lossy().into_owned(),
            });
        }
    }

    // Stalest first (never-opened, then oldest), so unused apps surface.
    items.sort_by(|a, b| {
        let rank = |i: &InvItem| i.flags.iter().any(|f| f == "unused");
        rank(b).cmp(&rank(a)).then(b.size.cmp(&a.size))
    });

    let unused = items.iter().filter(|i| i.flags.iter().any(|f| f == "unused")).count();
    InvSection {
        id: "apps".into(),
        name: "Applications".into(),
        subtitle: format!("{} apps · {} not opened in 6+ months", items.len(), unused),
        available: !items.is_empty(),
        note: "Reveal in Finder to move an app to the Trash yourself.".into(),
        total_size: total(&items),
        items,
    }
}

// ---- global packages ---------------------------------------------------

fn npm_name_from_path(path: &str) -> String {
    match path.rfind("node_modules/") {
        Some(i) => path[i + "node_modules/".len()..].to_string(),
        None => path.rsplit('/').next().unwrap_or(path).to_string(),
    }
}

fn global_section(home: &Path) -> InvSection {
    let mut items = Vec::new();

    // npm -g
    if let Some(out) = output_of("npm", &["ls", "-g", "--depth=0", "--parseable"]) {
        for line in out.lines().filter(|l| l.contains("node_modules/")) {
            let path = line.trim();
            let name = npm_name_from_path(path);
            items.push(InvItem {
                name: name.clone(),
                detail: "npm global".to_string(),
                size: path_size(Path::new(path)),
                size_known: true,
                flags: vec!["npm".to_string()],
                remove_command: format!("npm -g uninstall {name}"),
                path: String::new(),
            });
        }
    }

    // pipx
    if let Some(out) = output_of("pipx", &["list", "--short"]) {
        for line in out.lines() {
            let name = line.split_whitespace().next().unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            items.push(InvItem {
                name: name.clone(),
                detail: "pipx".to_string(),
                size: path_size(&home.join(".local/pipx/venvs").join(&name)),
                size_known: true,
                flags: vec!["pipx".to_string()],
                remove_command: format!("pipx uninstall {name}"),
                path: String::new(),
            });
        }
    }

    // cargo install (skip rustup-managed symlinks)
    let cargo_bin = home.join(".cargo/bin");
    for bin in dir_children(&cargo_bin) {
        if std::fs::symlink_metadata(&bin)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(true)
        {
            continue;
        }
        let name = bin
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        items.push(InvItem {
            name: name.clone(),
            detail: "cargo".to_string(),
            size: path_size(&bin),
            size_known: true,
            flags: vec!["cargo".to_string()],
            remove_command: format!("cargo uninstall {name}"),
            path: bin.to_string_lossy().into_owned(),
        });
    }

    sort_by_size(&mut items);
    InvSection {
        id: "global".into(),
        name: "Global packages".into(),
        subtitle: format!("{} from npm, pipx and cargo · commands are best-effort", items.len()),
        available: !items.is_empty(),
        note: if items.is_empty() {
            "No global npm, pipx or cargo packages found.".into()
        } else {
            String::new()
        },
        total_size: total(&items),
        items,
    }
}

// ---- entry point -------------------------------------------------------

pub fn inventory() -> Result<Inventory, String> {
    let home = dirs::home_dir().ok_or("Could not determine your home directory")?;
    Ok(Inventory {
        sections: vec![
            brew_section(),
            runtimes_section(&home),
            apps_section(&home),
            global_section(&home),
        ],
    })
}

/// Copy text to the clipboard via `pbcopy` (used for suggested commands).
pub fn copy_text(text: String) -> Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(text.as_bytes())
        .map_err(|e| e.to_string())?;
    child.wait().map_err(|e| e.to_string())?;
    Ok(())
}
