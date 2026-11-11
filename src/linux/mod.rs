// The Linux side, for X11 desktops and Xvfb: AT-SPI for reading and pressing controls, XTest for
// points and keys, and the window manager's hints for the window list.
pub mod atspi;
pub mod service;
pub mod x;
