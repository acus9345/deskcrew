// The Linux side, for X11/Xvfb and Wayland (AT-SPI controls): AT-SPI for reading and pressing
// controls, XTest for X11 points and keys, the window manager's hints for the X11 window list, and
// drawn cursors on X11.
pub mod atspi;
pub mod notifications;
pub mod overlay;
pub mod portal;
pub mod service;
pub mod x;
