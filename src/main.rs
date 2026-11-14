mod config;
mod cred;
mod gesture;
mod rects;

#[cfg(target_os = "linux")]
mod device;
#[cfg(target_os = "linux")]
mod rdp;

#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(rdp::main());
}

#[cfg(not(target_os = "linux"))]
fn main() {
    // Host builds exist only to run the unit tests.
    let _ = (config::parse, config::strip_password, cred::seal, cred::open, cred::check_pin, cred::sha256_hex);
    let _ = (gesture::MtParser::default, gesture::Gestures::new, rects::Scheduler::new);
    eprintln!("kindlerdp runs on the Kindle only. Build with: .\\package.ps1");
}
