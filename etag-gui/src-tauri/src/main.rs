// Tauri v2 entry point.
//
// All application logic lives in `lib.rs`; this file is kept minimal so that
// the desktop binary and the mobile library crate share one code path.

fn main() {
    etag_gui_lib::run();
}
