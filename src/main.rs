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
    // Host builds exist only to run the unit tests.
    let _ = (config::parse, config::strip_password, cred::seal, cred::open, cred::check_pin, cred::sha256_hex);
    let _ = (gesture::MtParser::default, gesture::Gestures::new, rects::Scheduler::new);
    let _ = (rects::Wave::Mono, gesture::TouchRouter::held, gesture::TouchRouter::take_hold);
    let _ = (servers::parse, servers::serialize, servers::from_fields, ui::list_screen, ui::form_screen, ui::dialog_screen, ui::settings_screen, config::set, config::DEFAULT);
    eprintln!("kindlerdp runs on the Kindle only. Build with: .\\package.ps1");
}
