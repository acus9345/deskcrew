// The Linux side, for X11 desktops and Xvfb: AT-SPI for reading and pressing controls, XTest for
// points and keys, the window manager's hints for the window list, and drawn cursors.
pub mod atspi;
pub mod overlay;
pub mod service;
pub mod x;
