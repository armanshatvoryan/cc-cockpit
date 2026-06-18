// Tauri desktop entry. Delegates to the lib so the same `run()` works on mobile.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    cc_cockpit_d3_lib::run();
}
