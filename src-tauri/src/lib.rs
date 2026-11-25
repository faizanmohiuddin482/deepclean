mod cleaner;
mod inventory;

use cleaner::{CleanResult, ScanReport};
use inventory::Inventory;

/// Read-only scan. Never deletes anything.
#[tauri::command]
fn scan() -> Result<ScanReport, String> {
    cleaner::scan()
}

/// Move the selected paths to the Trash (recoverable), after re-validating each.
#[tauri::command]
fn clean(paths: Vec<String>) -> Result<CleanResult, String> {
    cleaner::clean(paths)
}

/// Reveal an item in Finder (read-only).
#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    cleaner::reveal(path)
}

/// Preview an item with Quick Look (read-only).
#[tauri::command]
fn quicklook(path: String) -> Result<(), String> {
    cleaner::quicklook(path)
}

/// Read-only inventory of installed tools, runtimes, apps and packages.
#[tauri::command]
fn inventory() -> Result<Inventory, String> {
    inventory::inventory()
}

/// Copy a suggested command to the clipboard.
#[tauri::command]
fn copy_text(text: String) -> Result<(), String> {
    inventory::copy_text(text)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            scan, clean, reveal, quicklook, inventory, copy_text
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
