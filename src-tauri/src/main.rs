// Prevents additional console window on Windows in release, DO NOT REMOVE!!
// Prevents an extra console window in Windows release builds, do not remove!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Start the Tauri application defined in hitomi_downloader_lib
    hitomi_downloader_lib::run()
}
