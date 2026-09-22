// Desktop entry point. All setup lives in `run()` in lib.rs.

// Release builds on Windows open without an extra console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    bg_bucket_browser::run();
}
