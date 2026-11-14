// Host builds exist only to run the unit tests: nothing there calls the shared code.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

mod config;
mod cred;
mod gesture;
mod rects;
mod servers;
mod ui;

#[cfg(target_os = "linux")]
mod app;
#[cfg(target_os = "linux")]
mod device;
#[cfg(target_os = "linux")]
mod rdp;

#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(app::main());
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("kindlerdp runs on the Kindle only. Build with: .\\package.ps1");
}
