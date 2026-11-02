// The macOS side: the same commands and replies as on Windows, built on the accessibility API,
// events posted to one app at a time, and AppKit windows for the cursors.
pub mod apps;
pub mod ax;
pub mod capture;
pub mod ffi;
pub mod input;
pub mod ocr;
pub mod service;
