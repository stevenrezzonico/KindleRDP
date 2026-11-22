//! The app around the session: server list, editor, saved passwords, rotation.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;

use crate::config::{self, Config};
use crate::cred;
use crate::device::{self, Fb, TouchScreen};
use crate::gesture::Touch;
use crate::rdp::{self, Outcome};
use crate::rects::{Rect, Rotation, Wave};
use crate::servers::{self, Server};
use crate::ui::{self, Areas, Canvas, Form, FormHit, ListHit, SettingsForm, SettingsHit};

/// The panel and touch screen, plus what the app last drew on it.
pub struct Ui {
    pub fb: Fb,
    pub touch: TouchScreen,
    shown: Option<Canvas>,
    /// Waveform for the next whole-screen draw: a flash after the Kindle UI or a rotation,
    /// quick black/white otherwise.
    whole: Wave,
}

impl Ui {
    fn canvas(&self) -> Canvas {
        let ((w, h), zoom) = (self.fb.size(), self.fb.zoom());
        Canvas::new(w * zoom, h * zoom)
    }

    /// Put `canvas` on the panel, refreshing only the part that differs from what is shown.
    fn show(&mut self, canvas: Canvas) {
        let changed = match &self.shown {
            Some(old) => canvas.diff(old).map(|r| (r, Wave::Mono)),
            None => Some((Rect { x: 0, y: 0, w: canvas.w, h: canvas.h }, std::mem::replace(&mut self.whole, Wave::Mono))),
        };
        if let Some((r, wave)) = changed {
            let on_panel = self.fb.blit_gray(&canvas, r);
            self.fb.refresh(on_panel, wave);
        }
        self.shown = Some(canvas);
    }

    /// Something else (the session) drew on the panel: redraw everything next time.
    pub fn forget(&mut self) {
        self.shown = None;
    }

    /// Wait for a finger to land; returns where, in canvas coordinates.
    fn tap(&mut self) -> anyhow::Result<(i32, i32)> {
        let mut touches = Vec::new();
        let zoom = self.fb.zoom();
        loop {
            device::poll([-1, self.touch.fd()], None)?;
            self.touch.read(self.fb.rotation, &mut touches)?;
            let landed = touches.drain(..).find_map(|t| match t {
                Touch::Down { x, y, .. } => Some((x * zoom, y * zoom)),
                _ => None,
            });
            if let Some(point) = landed {
                return Ok(point);
            }
        }
    }

    fn choose<T: Copy>(&mut self, areas: &Areas<T>) -> anyhow::Result<T> {
        loop {
            let (x, y) = self.tap()?;
            if let Some(choice) = ui::hit(areas, x, y) {
                return Ok(choice);
            }
        }
    }

    /// Show a message with buttons; returns the index of the one tapped.
    pub fn dialog(&mut self, text: &str, buttons: &[&str]) -> anyhow::Result<usize> {
        let mut canvas = self.canvas();
        let areas = ui::dialog_screen(&mut canvas, text, buttons);
        self.show(canvas);
        if areas.is_empty() { Ok(0) } else { self.choose(&areas) }
    }

    fn rotate(&mut self) {
        self.fb.rotation = self.fb.rotation.toggled();
        let name = if self.fb.rotation == Rotation::Portrait { "portrait" } else { "landscape" };
        if let Err(e) = device::write_private(&device::state("rotation"), name.as_bytes()) {
            eprintln!("rotation not saved: {e:#}");
        }
        self.forget();
        self.whole = Wave::Full;
    }
}

pub fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(e) => {
            device::message(&format!("KindleRDP: {e:#}"));
            std::thread::sleep(Duration::from_secs(5)); // time to read it before the Kindle UI returns
            1
        }
    }
}

struct App {
    ui: Ui,
    settings: Config,
    servers: Vec<Server>,
    servers_path: PathBuf,
    config_path: PathBuf,
    serial: Vec<u8>,
}

fn run() -> anyhow::Result<()> {
    let dir = std::env::current_exe()?.parent().context("exe has no directory")?.to_owned();
    let config_path = dir.join("config.ini");
    let config_text = match std::fs::read_to_string(&config_path) {
        Ok(text) => text,
        Err(_) => {
            // First start: put the documented defaults where the user can find and edit them.
            if let Err(e) = std::fs::write(&config_path, config::DEFAULT) {
                eprintln!("config.ini not created: {e}");
            }
            config::DEFAULT.to_string()
        }
    };
    let settings = config::parse(&config_text).map_err(|e| anyhow::anyhow!("config.ini {e}"))?;
    device::set_fbink(settings.fbink.as_deref());

    let saved = std::fs::read_to_string(device::state("rotation")).unwrap_or_default();
    let rotation = if saved.trim() == "portrait" { Rotation::Portrait } else { Rotation::Landscape };
    let firmware = std::fs::read_to_string("/etc/prettyversion.txt").unwrap_or_default();
    eprintln!("KindleRDP {} on {}", env!("CARGO_PKG_VERSION"), firmware.trim());
    let fb = Fb::open(settings.gamma, rotation, settings.refresh_fbink)?;
    let (fb_w, fb_h) = fb.fb_size();
    let touch = TouchScreen::open(fb_w, fb_h, settings.touch, settings.touch_device.as_deref())?;

    let servers_path = dir.join("servers.ini");
    // No servers.ini yet: an empty list.
    let servers = servers::parse(&std::fs::read_to_string(&servers_path).unwrap_or_default()).map_err(anyhow::Error::msg)?;
    App { ui: Ui { fb, touch, shown: None, whole: Wave::Full }, settings, servers, servers_path, config_path, serial: device::serial() }.list()
}

impl App {
    fn cred_path(server: &Server) -> PathBuf {
        device::state(&format!("cred-{}", &cred::sha256_hex(server.account().as_bytes())[..16]))
    }

    fn password(&self, server: &Server) -> Option<String> {
        cred::open(&self.serial, &server.account(), &std::fs::read(Self::cred_path(server)).ok()?)
    }

    fn save_password(&self, server: &Server, password: &str) -> anyhow::Result<()> {
        let blob = cred::seal(&self.serial, &server.account(), device::random_nonce()?, password);
        device::write_private(&Self::cred_path(server), &blob)
    }

    fn save_servers(&self) -> anyhow::Result<()> {
        // Written beside the list and renamed over it: a write cut short never leaves half a file.
        let tmp = self.servers_path.with_extension("ini.tmp");
        std::fs::write(&tmp, servers::serialize(&self.servers)).context("write servers.ini")?;
        std::fs::rename(&tmp, &self.servers_path).context("write servers.ini")
    }

    /// Remove the saved password of a server that is gone from the list, unless another
    /// server uses the same account and so the same file.
    fn forget_password(&self, gone: &Server) {
        if !self.servers.iter().any(|s| s.account() == gone.account()) {
            let _ = std::fs::remove_file(Self::cred_path(gone));
        }
    }

    fn list(&mut self) -> anyhow::Result<()> {
        let mut page = 0;
        loop {
            let mut canvas = self.ui.canvas();
            let rows = ui::list_rows(&canvas);
            if page * rows >= self.servers.len() {
                page = 0;
            }
            let areas = ui::list_screen(&mut canvas, &self.servers, page);
            self.ui.show(canvas);
            match self.ui.choose(&areas)? {
                ListHit::Exit => return Ok(()),
                ListHit::Rotate => self.ui.rotate(),
                ListHit::Settings => self.edit_settings()?,
                ListHit::NextPage => page += 1,
                ListHit::Add => self.edit(None)?,
                ListHit::Edit(index) => self.edit(Some(index))?,
                ListHit::Connect(index) => self.connect(index)?,
            }
        }
    }

    fn edit_settings(&mut self) -> anyhow::Result<()> {
        let mut form = SettingsForm::new(self.settings.scale, self.settings.gamma);
        loop {
            let mut canvas = self.ui.canvas();
            let areas = ui::settings_screen(&mut canvas, &form);
            self.ui.show(canvas);
            match self.ui.choose(&areas)? {
                SettingsHit::Step(row, up) => form.step(row, up),
                SettingsHit::Cancel => return Ok(()),
                SettingsHit::Save => {
                    let mut text = std::fs::read_to_string(&self.config_path).unwrap_or_default();
                    for (key, value) in [("scale", form.scale.to_string()), ("gamma", format!("{:.1}", form.gamma()))] {
                        text = config::set(&text, key, &value);
                    }
                    std::fs::write(&self.config_path, text).context("write config.ini")?;
                    self.settings.scale = form.scale;
                    self.settings.gamma = form.gamma();
                    self.ui.fb.set_gamma(form.gamma());
                    return Ok(());
                }
            }
        }
    }

    fn edit(&mut self, index: Option<usize>) -> anyhow::Result<()> {
        let server = index.map(|i| &self.servers[i]);
        let mut form = Form::new(server, server.is_some_and(|s| self.password(s).is_some()));
        loop {
            let mut canvas = self.ui.canvas();
            let areas = ui::form_screen(&mut canvas, &form, index.is_some());
            self.ui.show(canvas);
            match self.ui.choose(&areas)? {
                FormHit::Field(field) => form.active = field,
                FormHit::Key(key) => form.press(key),
                FormHit::Cancel => return Ok(()),
                FormHit::Save => match self.save(&form, index) {
                    Ok(()) => return Ok(()),
                    Err(problem) => form.note = format!("{problem:#}"),
                },
                FormHit::Delete => {
                    let index = index.expect("Delete is only offered for an existing server");
                    let question = format!("Delete {}?", self.servers[index].name);
                    if self.ui.dialog(&question, &["Delete", "Keep"])? == 0 {
                        let removed = self.servers.remove(index);
                        self.forget_password(&removed);
                        return self.save_servers();
                    }
                    self.ui.forget();
                }
            }
        }
    }

    fn save(&mut self, form: &Form, index: Option<usize>) -> anyhow::Result<()> {
        let server = servers::from_fields(&form.fields).map_err(anyhow::Error::msg)?;
        let password = &form.fields[5];
        let old = index.map(|i| self.servers[i].clone());
        let same_account = old.as_ref().is_some_and(|o| o.account() == server.account());
        if password.is_empty() && !(same_account && self.password(&server).is_some()) {
            anyhow::bail!("Enter the password");
        }
        if !password.is_empty() {
            self.save_password(&server, password)?;
        }
        match index {
            Some(i) => self.servers[i] = server,
            None => self.servers.push(server),
        }
        if let Some(old) = old {
            self.forget_password(&old);
        }
        self.save_servers()
    }

    fn connect(&mut self, index: usize) -> anyhow::Result<()> {
        loop {
            let server = self.servers[index].clone();
            let Some(password) = self.password(&server) else {
                self.ui.dialog(&format!("No saved password for {}. Tap Edit and enter it.", server.name), &["OK"])?;
                return Ok(());
            };
            self.ui.dialog(&format!("Connecting to {}...", server.name), &[])?;
            let outcome = rdp::run(&mut self.ui, server.scale.unwrap_or(self.settings.scale), &server, password);
            self.ui.forget();
            match outcome {
                Ok(Outcome::Disconnected) => return Ok(()),
                Ok(Outcome::Rotate) => self.ui.rotate(),
                Ok(Outcome::Scale(scale)) => {
                    self.servers[index].scale = Some(scale);
                    self.save_servers()?;
                }
                Err(e) => {
                    eprintln!("{}: {e:#}", server.name);
                    if let Some(changed) = e.downcast_ref::<rdp::CertChanged>() {
                        let question = format!(
                            "The certificate of {} changed. Windows renews it about twice a year; it also changes when the PC is reinstalled. Trust the new one?",
                            server.name
                        );
                        if self.ui.dialog(&question, &["Trust", "Cancel"])? == 0 {
                            rdp::save_pin(&changed.host, &changed.fingerprint)?;
                            continue;
                        }
                        return Ok(());
                    }
                    self.ui.dialog(&format!("{}: {e:#}", server.name), &["OK"])?;
                    return Ok(());
                }
            }
        }
    }
}
