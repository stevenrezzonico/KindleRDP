//! The RDP session: connect (TLS + NLA), then one poll loop over the socket and the touch screen.

use std::io::Write as _;
use std::net::TcpStream;
use std::os::fd::AsRawFd as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use ironrdp::connector::connection_activation::ConnectionActivationState;
use ironrdp::connector::{self, ConnectionResult, Credentials, Sequence as _};
use ironrdp::dvc::DrdynvcClient;
use ironrdp::graphics::image_processing::PixelFormat;
use ironrdp::input::{Database, MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use ironrdp::pdu::gcc::KeyboardType;
use ironrdp::pdu::rdp::capability_sets::MajorPlatformType;
use ironrdp::pdu::rdp::client_info::{CompressionType, PerformanceFlags, TimezoneInfo};
use ironrdp::session::image::DecodedImage;
use ironrdp::session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput};
use ironrdp_rdpei::RdpeiClient;
use ironrdp_rdpei::contact::{TouchFrameBuilder, TouchPhase};
use ironrdp_rdpei::pdu::{CsReadyFlags, RdpInputProtocolVersion};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use crate::app::Ui;
use crate::cred::{self, Pin};
use crate::device::{self, Fb};
use crate::gesture::{Action, Gestures, Phase, Routed, Touch, TouchRouter};
use crate::rects::{Rect, Scheduler, Wave};
use crate::servers::Server;
use crate::ui::{self, Areas, Canvas, Key, Keyboard};

type Framed = ironrdp_blocking::Framed<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>;

/// Touch contacts Windows may get at once.
const MAX_CONTACTS: u16 = 5;
/// How often resting fingers are restated to Windows.
const KEEPALIVE_MS: u64 = 100;
/// Longest stretch of reading the socket before the screen and touches get their turn.
const DRAIN_MS: u64 = 50;
/// A held Backspace, Delete or arrow key repeats after this long, then at this pace.
const REPEAT_START_MS: u64 = 500;
const REPEAT_MS: u64 = 100;

/// Why a session ended without an error.
pub enum Outcome {
    Disconnected,
    /// The user asked to rotate: reconnect at the new size.
    Rotate,
    /// The user chose another Windows scale for this server: save it and reconnect.
    Scale(u32),
}

pub fn run(ui: &mut Ui, scale: u32, server: &Server, password: String) -> anyhow::Result<Outcome> {
    let (w, h) = ui.fb.size();
    let (width, height) = (w as u16, h as u16);
    let (result, mut framed) = connect(scale, server, password, width, height)?;
    if (result.desktop_size.width, result.desktop_size.height) != (width, height) {
        bail!("server refused {width}x{height}, offered {}x{}", result.desktop_size.width, result.desktop_size.height);
    }
    session(result, &mut framed, ui, width, height, scale, Scheduler::new(Rect { x: 0, y: 0, w, h }))
}

fn connect(scale: u32, cfg: &Server, password: String, width: u16, height: u16) -> anyhow::Result<(ConnectionResult, Framed)> {
    use std::net::ToSocketAddrs as _;
    let rdp_config = connector::Config {
        credentials: Credentials::UsernamePassword { username: cfg.user.clone(), password },
        domain: Some(cfg.domain.clone()).filter(|d| !d.is_empty()),
        enable_tls: false,
        enable_credssp: true, // NLA: what Windows requires by default
        keyboard_type: KeyboardType::IbmEnhanced,
        keyboard_subtype: 0,
        keyboard_layout: 0,
        keyboard_functional_keys_count: 12,
        ime_file_name: String::new(),
        dig_product_id: String::new(),
        desktop_size: connector::DesktopSize { width, height },
        bitmap: None,
        client_build: 0,
        client_name: "kindlerdp".to_owned(),
        client_dir: "C:\\Windows\\System32\\mstscax.dll".to_owned(),
        platform: MajorPlatformType::UNIX,
        enable_server_pointer: false, // touch screen: no cursor to draw
        request_data: None,
        autologon: false,
        enable_audio_playback: false,
        compression_type: Some(CompressionType::Rdp61),
        pointer_software_rendering: false,
        multitransport_flags: None,
        // Everything that animates or adds gray noise costs e-ink refreshes.
        performance_flags: PerformanceFlags::DISABLE_WALLPAPER
            | PerformanceFlags::DISABLE_FULLWINDOWDRAG
            | PerformanceFlags::DISABLE_MENUANIMATIONS
            | PerformanceFlags::DISABLE_THEMING
            | PerformanceFlags::DISABLE_CURSOR_SHADOW,
        desktop_scale_factor: scale,
        hardware_id: None,
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        alternate_shell: String::new(),
        work_dir: String::new(),
    };

    let addr = (cfg.host.as_str(), cfg.port).to_socket_addrs().context("resolve host")?.next().context("host has no address")?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).context("TCP connect")?;
    tcp.set_nodelay(true)?;
    // Without these a server that goes silent during login hangs the app, and the Kindle with it.
    // The session reads without blocking, so they only bound the login and each send.
    tcp.set_read_timeout(Some(Duration::from_secs(20)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(20)))?;
    let client_addr = tcp.local_addr()?;

    let mut framed = ironrdp_blocking::Framed::new(tcp);
    // Real touch input: Windows then does scrolling, right-click, drag and its touch keyboard itself.
    let touch_channel = RdpeiClient::new(MAX_CONTACTS, CsReadyFlags::empty(), RdpInputProtocolVersion::V200);
    let mut connector = connector::ClientConnector::new(rdp_config, client_addr)
        .with_static_channel(DrdynvcClient::new().with_dynamic_channel(touch_channel));
    let should_upgrade = ironrdp_blocking::connect_begin(&mut framed, &mut connector).context("begin connection")?;
    let (tls, server_public_key) = tls_upgrade(framed.into_inner_no_leftover(), cfg).context("TLS")?;
    let upgraded = ironrdp_blocking::mark_as_upgraded(should_upgrade, &mut connector);
    let mut framed = ironrdp_blocking::Framed::new(tls);
    let result = ironrdp_blocking::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut NoKerberos,
        cfg.host.clone().into(),
        server_public_key,
        None,
    )
    .context("login")?;
    Ok((result, framed))
}

/// NTLM needs no extra network round trips; only Kerberos (domain KDC) would.
struct NoKerberos;

impl sspi::network_client::NetworkClient for NoKerberos {
    fn send(&self, _: &sspi::generator::NetworkRequest) -> sspi::Result<Vec<u8>> {
        Err(sspi::Error::new(sspi::ErrorKind::NoAuthenticatingAuthority, "Kerberos is not supported"))
    }
}

fn tls_upgrade(tcp: TcpStream, cfg: &Server) -> anyhow::Result<(rustls::StreamOwned<rustls::ClientConnection, TcpStream>, Vec<u8>)> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut tls_config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedOnly(provider)))
        .with_no_client_auth();
    tls_config.resumption = rustls::client::Resumption::disabled(); // CredSSP does not support it

    let name = ServerName::try_from(cfg.host.clone())?;
    let mut tls = rustls::StreamOwned::new(rustls::ClientConnection::new(Arc::new(tls_config), name)?, tcp);
    tls.flush()?; // drives the handshake so the certificate is available

    let cert = tls.conn.peer_certificates().and_then(|c| c.first()).context("server sent no certificate")?;

    // Windows RDP certificates are self-signed, so trust is by first use: the certificate seen
    // on the first connection is the only one accepted later. Checked before any credential is sent.
    let known = std::fs::read_to_string(device::state("pins")).unwrap_or_default();
    let host = format!("{}:{}", cfg.host, cfg.port);
    let fingerprint = cred::sha256_hex(cert);
    match cred::check_pin(&known, &host, &fingerprint) {
        Pin::Match => {}
        Pin::New => save_pin(&host, &fingerprint)?,
        Pin::Mismatch => return Err(CertChanged { host, fingerprint }.into()),
    }

    use x509_cert::der::Decode as _;
    let server_public_key = x509_cert::Certificate::from_der(cert)?
        .tbs_certificate
        .subject_public_key_info
        .subject_public_key
        .as_bytes()
        .context("bad server public key")?
        .to_owned();
    Ok((tls, server_public_key))
}

/// The server showed another certificate than the one remembered. Nothing was sent to it.
/// The app asks the user whether to trust the new one (`save_pin`).
#[derive(Debug)]
pub struct CertChanged {
    pub host: String,
    pub fingerprint: String,
}

impl std::fmt::Display for CertChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "certificate of {} changed", self.host)
    }
}

impl std::error::Error for CertChanged {}

/// Remember `fingerprint` as the only certificate accepted for `host` ("name:port").
pub fn save_pin(host: &str, fingerprint: &str) -> anyhow::Result<()> {
    let path = device::state("pins");
    let known = std::fs::read_to_string(&path).unwrap_or_default();
    device::write_private(&path, cred::set_pin(&known, host, fingerprint).as_bytes()).context("save certificate pin")
}

/// Accepts any certificate chain (pinning happens after the handshake) but still verifies the
/// handshake signature, so the server must hold the private key of the certificate it shows.
#[derive(Debug)]
struct PinnedOnly(Arc<CryptoProvider>);

impl ServerCertVerifier for PinnedOnly {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Blocking write on the otherwise non-blocking socket: frames are small and must go out whole.
fn send(framed: &mut Framed, frame: &[u8]) -> anyhow::Result<()> {
    framed.get_inner().0.sock.set_nonblocking(false)?;
    let sent = framed.write_all(frame);
    framed.get_inner().0.sock.set_nonblocking(true)?;
    sent.context("send")
}


/// IronRDP's own error types are not all `Send + Sync`: keep their text.
fn text<E: std::fmt::Display>(e: E) -> anyhow::Error {
    anyhow!("{e}")
}

fn send_outputs(framed: &mut Framed, outputs: Vec<ActiveStageOutput>) -> anyhow::Result<()> {
    for out in outputs {
        if let ActiveStageOutput::ResponseFrame(frame) = out {
            send(framed, &frame)?;
        }
    }
    Ok(())
}

/// True when Windows accepted the touch channel and wants input on it right now.
fn touch_channel_ready(stage: &mut ActiveStage) -> bool {
    stage
        .get_dvc::<RdpeiClient>()
        .and_then(|channel| channel.channel_processor_downcast_ref::<RdpeiClient>())
        .is_some_and(|client| client.ready() && !client.is_suspended())
}

/// The app's own keyboard, drawn over the top or bottom of the session picture. Windows does
/// not know about it: the picture under it is simply not shown.
#[derive(Default)]
struct Board {
    keys: Keyboard,
    at_top: bool,
    areas: Areas<Key>,
    /// What is on the panel, so that only what changes is drawn again.
    shown: Option<Canvas>,
    /// One bit per touch slot whose finger landed on the keyboard.
    fingers: u8,
    /// The finger on a key: its slot and, for a key that repeats, what it types and when next.
    held: Option<(usize, Option<((Key, u8), u64)>)>,
    /// `Fb::zoom`: the keys are laid out on a canvas this many times the size of the panel.
    zoom: i32,
}

impl Board {
    fn area(&self, screen: Rect) -> Rect {
        let h = (screen.h * 9 / 20 / 6).min(110 / self.zoom) * 6;
        Rect { y: if self.at_top { 0 } else { screen.h - h }, h, ..screen }
    }

    /// The part of the screen left to the session picture.
    fn view(board: &Option<Board>, screen: Rect) -> Rect {
        let Some(board) = board else { return screen };
        let area = board.area(screen);
        Rect { y: if board.at_top { area.h } else { 0 }, h: screen.h - area.h, ..screen }
    }

    /// Put the keyboard into the framebuffer; returns the part that changed, to refresh.
    fn paint(&mut self, fb: &Fb, screen: Rect) -> Option<Rect> {
        let z = self.zoom;
        let mut canvas = Canvas::new(screen.w * z, screen.h * z);
        let area = self.area(screen);
        let area = Rect { x: area.x * z, y: area.y * z, w: area.w * z, h: area.h * z };
        self.areas = ui::keyboard(&mut canvas, area, &self.keys, true);
        let changed = match &self.shown {
            Some(old) => canvas.diff(old),
            None => Some(area),
        };
        let on_panel = changed.map(|r| fb.blit_gray(&canvas, r));
        self.shown = Some(canvas);
        on_panel
    }

    /// Take a touch that belongs to the keyboard; false leaves it to Windows. A key acts when
    /// the finger lands, so typing does not wait for the screen.
    fn touch(&mut self, touch: Touch, screen: Rect, now: u64, typed: &mut Vec<(Key, u8)>) -> bool {
        match touch {
            Touch::Down { slot, x, y } if self.area(screen).contains(x, y) => {
                self.fingers |= 1 << slot;
                if let Some(key) = ui::hit(&self.areas, x * self.zoom, y * self.zoom) {
                    let out = self.keys.press(key);
                    self.keys.pressed = Some(key);
                    self.held = Some((slot, out.filter(|(key, _)| key.repeats()).map(|out| (out, now + REPEAT_START_MS))));
                    typed.extend(out);
                }
                true
            }
            Touch::Down { .. } => false,
            Touch::Move { slot, .. } => self.fingers & (1 << slot) != 0,
            Touch::Up { slot } => {
                let mine = self.fingers & (1 << slot) != 0;
                self.fingers &= !(1 << slot);
                if self.held.is_some_and(|(held, _)| held == slot) {
                    self.release();
                }
                mine
            }
        }
    }

    fn release(&mut self) {
        self.held = None;
        self.keys.pressed = None;
    }

    fn repeat_at(&self) -> Option<u64> {
        self.held?.1.map(|(_, at)| at)
    }

    fn repeat(&mut self, now: u64, typed: &mut Vec<(Key, u8)>) {
        if let Some((_, Some((out, at)))) = &mut self.held {
            if now >= *at {
                typed.push(*out);
                *at = now + REPEAT_MS;
            }
        }
    }
}

/// The key events for one typed key. A character goes as Unicode, right on any Windows
/// keyboard layout. With Ctrl, Alt or Win it must be a real key instead.
/// shortcut: that key is taken by US position, so Ctrl+Z and Ctrl+Y swap on a QWERTZ
/// layout (and A/Q, Z/W on AZERTY); add a layout table when that matters.
fn key_ops(key: Key, mut mods: u8, ops: &mut Vec<Operation>) {
    let code = Scancode::from_u8;
    let scancode = match key {
        Key::Char(ch) => match ui::us_scancode(ch.to_ascii_lowercase()).filter(|_| mods & (ui::CTRL | ui::ALT | ui::WIN) != 0) {
            Some(at) => {
                if ch.is_ascii_uppercase() {
                    mods |= ui::SHIFT;
                }
                code(false, at)
            }
            None => return ops.extend([Operation::UnicodeKeyPressed(ch), Operation::UnicodeKeyReleased(ch)]),
        },
        Key::Esc => code(false, 0x01),
        Key::Backspace => code(false, 0x0E),
        Key::Tab => code(false, 0x0F),
        Key::Enter => code(false, 0x1C),
        Key::Up => code(true, 0x48),
        Key::Left => code(true, 0x4B),
        Key::Right => code(true, 0x4D),
        Key::Down => code(true, 0x50),
        Key::Delete => code(true, 0x53),
        _ => return,
    };
    let all = [(ui::CTRL, code(false, 0x1D)), (ui::ALT, code(false, 0x38)), (ui::WIN, code(true, 0x5B)), (ui::SHIFT, code(false, 0x2A))];
    let held: Vec<Scancode> = all.into_iter().filter(|(bit, _)| mods & bit != 0).map(|(_, code)| code).collect();
    ops.extend(held.iter().copied().map(Operation::KeyPressed));
    ops.extend([Operation::KeyPressed(scancode), Operation::KeyReleased(scancode)]);
    ops.extend(held.into_iter().rev().map(Operation::KeyReleased));
}

fn session(
    result: ConnectionResult,
    framed: &mut Framed,
    ui: &mut Ui,
    width: u16,
    height: u16,
    scale: u32,
    mut scheduler: Scheduler,
) -> anyhow::Result<Outcome> {
    let activation = result.activation_factory;
    let mut stage: ActiveStage = ActiveStageBuilder {
        static_channels: result.static_channels,
        user_channel_id: result.user_channel_id,
        io_channel_id: result.io_channel_id,
        message_channel_id: result.message_channel_id,
        share_id: result.share_id,
        compression_type: result.compression_type,
        enable_server_pointer: result.enable_server_pointer,
        pointer_software_rendering: result.pointer_software_rendering,
    }
    .build();

    let mut image = DecodedImage::new(PixelFormat::RgbA32, width, height);
    let screen = Rect { x: 0, y: 0, w: i32::from(width), h: i32::from(height) };
    let mut contacts = TouchFrameBuilder::new(MAX_CONTACTS);
    let mut router = TouchRouter::default();
    let mut last_contact_frame = 0;
    let mut mouse = Gestures::new();
    let mut input = Database::new();
    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u64;

    let socket = framed.get_inner().0.sock.as_raw_fd();
    framed.get_inner().0.sock.set_nonblocking(true)?;
    let (mut touches, mut actions, mut routed) = (Vec::new(), Vec::new(), Vec::new());
    // The last pass stopped reading the socket early: more may be waiting, undetected by poll.
    let mut backlog = false;
    let mut board: Option<Board> = None;

    loop {
        for (rect, wave) in scheduler.tick(now()) {
            ui.fb.refresh(rect, wave);
        }
        let touch_channel = touch_channel_ready(&mut stage);
        let keepalive = (touch_channel && router.held().next().is_some()).then(|| last_contact_frame + KEEPALIVE_MS);
        let repeat = board.as_ref().and_then(Board::repeat_at);
        let wake = [keepalive, router.hold_deadline(), repeat].into_iter().flatten().map(|at| at.saturating_sub(now()));
        let timeout = if backlog { Some(0) } else { scheduler.wait(now()).into_iter().chain(wake).min() };
        let [net_polled, touch_ready] = device::poll([socket, ui.touch.fd()], timeout)?;
        let net_ready = net_polled || std::mem::take(&mut backlog);
        let now_us = start.elapsed().as_micros() as u64;

        let mut ops = Vec::new();
        let mut typed = Vec::new();
        let mut repaint = false;
        if touch_ready {
            ui.touch.read(ui.fb.rotation, &mut touches)?;
            for touch in touches.drain(..) {
                if board.as_mut().is_some_and(|b| b.touch(touch, screen, now(), &mut typed)) {
                    repaint = true;
                    continue;
                }
                router.feed(touch, now(), &mut routed);
                if !touch_channel {
                    mouse.feed(touch, now(), &mut actions);
                }
            }
            for route in routed.drain(..) {
                match route {
                    Routed::Contact { .. } if !touch_channel => {} // mouse emulation handles the fingers
                    Routed::Contact { slot, phase, x, y } => {
                        let phase = match phase {
                            Phase::Start => TouchPhase::Started,
                            Phase::Move => TouchPhase::Moved,
                            Phase::End => TouchPhase::Ended,
                            Phase::Cancel => TouchPhase::Canceled,
                        };
                        contacts.push(slot as u64, phase, x.clamp(0, screen.w - 1), y.clamp(0, screen.h - 1), None, now_us);
                    }
                    Routed::RightClick { x, y } => actions.extend([Action::Move(x, y), Action::Right(true), Action::Right(false)]),
                }
            }
        }

        if let Some(b) = &mut board {
            b.repeat(now(), &mut typed);
        }
        for (key, mods) in typed {
            // Hide and Move give the covered picture back to the session.
            let uncovered = match key {
                Key::Hide => board.take().map(|b| b.area(screen)),
                Key::Move => board.as_mut().map(|b| {
                    let old = b.area(screen);
                    b.at_top = !b.at_top;
                    b.shown = None;
                    old
                }),
                _ => {
                    key_ops(key, mods, &mut ops);
                    None
                }
            };
            if let Some(r) = uncovered {
                ui.fb.blit(image.data(), usize::from(width), r);
                scheduler.damage(r, now());
            }
        }
        if let Some(b) = board.as_mut().filter(|_| repaint) {
            if let Some(r) = b.paint(&ui.fb, screen) {
                ui.fb.refresh(r, Wave::Mono);
            }
        }

        // Two fingers resting open the local menu. Take everything back from Windows first.
        let menu = router.take_hold(now());
        if menu {
            actions.clear();
            contacts.cancel_all(now_us);
            mouse = Gestures::new();
            if let Some(b) = &mut board {
                b.release();
                b.fingers = 0;
            }
            let released = input.release_all();
            send_outputs(framed, stage.process_fastpath_input(&mut image, &released)?)?;
        }

        if !touch_channel {
            contacts.reset();
        } else if !contacts.has_pending_frames() && now() >= last_contact_frame + KEEPALIVE_MS {
            // The panel reports nothing while a finger rests, and Windows drops a contact that
            // gets no updates: restate resting fingers so press-and-hold (right click) completes.
            for (slot, x, y) in router.held() {
                contacts.push(slot as u64, TouchPhase::Moved, x.clamp(0, screen.w - 1), y.clamp(0, screen.h - 1), None, now_us);
            }
        }
        if let Some(event) = contacts.take_event(now_us) {
            let messages = {
                let channel = stage.get_dvc::<RdpeiClient>().context("touch channel closed")?;
                let id = channel.channel_id().context("touch channel closed")?;
                let client = channel.channel_processor_downcast_ref::<RdpeiClient>().context("touch channel closed")?;
                client.encode_touch_event(id, event).map_err(text)?
            };
            let frame = stage.encode_dvc_messages(messages)?;
            send(framed, &frame)?;
            last_contact_frame = now();
        }

        for action in actions.drain(..) {
            ops.push(match action {
                Action::Move(x, y) => Operation::MouseMove(MousePosition { x: x.clamp(0, screen.w - 1) as u16, y: y.clamp(0, screen.h - 1) as u16 }),
                Action::Left(true) => Operation::MouseButtonPressed(MouseButton::Left),
                Action::Left(false) => Operation::MouseButtonReleased(MouseButton::Left),
                Action::Right(true) => Operation::MouseButtonPressed(MouseButton::Right),
                Action::Right(false) => Operation::MouseButtonReleased(MouseButton::Right),
                Action::Wheel(n) => Operation::WheelRotations(WheelRotations { is_vertical: true, rotation_units: (n * 120) as i16 }),
            });
        }
        let events = input.apply(ops);
        send_outputs(framed, stage.process_fastpath_input(&mut image, &events)?)?;

        if menu {
            ui.forget();
            let choice = ui.dialog("KindleRDP", &["Keyboard", "Scale", "Disconnect", "Rotate screen", "Back to Windows"])?;
            let mut leave = match choice {
                2 => Some(Outcome::Disconnected),
                3 => Some(Outcome::Rotate),
                _ => None,
            };
            let mut wanted = scale / 25 * 25;
            while choice == 1 {
                let buttons = ["Smaller", "Larger", "Apply (reconnects)", "Cancel"];
                match ui.dialog(&format!("Windows scale for this server: {wanted} %"), &buttons)? {
                    0 => wanted = (wanted - 25).max(100),
                    1 => wanted = (wanted + 25).min(500),
                    2 => {
                        leave = (wanted != scale).then_some(Outcome::Scale(wanted));
                        break;
                    }
                    _ => break,
                }
            }
            if let Some(outcome) = leave {
                send_outputs(framed, stage.graceful_shutdown()?)?;
                return Ok(outcome);
            }
            if choice == 0 {
                board = if board.is_some() { None } else { Some(Board { zoom: ui.fb.zoom(), ..Board::default() }) };
            }
            // The menu overwrote the session picture: put it back, with a cleaning flash.
            ui.forget();
            ui.fb.blit(image.data(), usize::from(width), screen);
            if let Some(b) = &mut board {
                b.shown = None;
                b.paint(&ui.fb, screen);
            }
            scheduler.request_full();
        }

        // Drain the socket: TLS may hold several decrypted frames after one readable event.
        // Not for longer than DRAIN_MS: a server that sends faster than the Kindle decodes must
        // not keep the screen and the touches waiting. `backlog` brings the loop straight back.
        let drain_start = Instant::now();
        while net_ready {
            if drain_start.elapsed() >= Duration::from_millis(DRAIN_MS) {
                backlog = true;
                break;
            }
            let (action, payload) = match framed.read_pdu() {
                Ok(pdu) => pdu,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e).context("connection lost"),
            };
            for out in stage.process(&mut image, action, &payload)? {
                match out {
                    ActiveStageOutput::ResponseFrame(frame) => send(framed, &frame)?,
                    ActiveStageOutput::GraphicsUpdate(r) => {
                        let rect = Rect {
                            x: i32::from(r.left),
                            y: i32::from(r.top),
                            w: i32::from(r.right) - i32::from(r.left) + 1,
                            h: i32::from(r.bottom) - i32::from(r.top) + 1,
                        };
                        // Not under the keyboard.
                        if let Some(rect) = rect.intersect(Board::view(&board, screen)) {
                            ui.fb.blit(image.data(), usize::from(width), rect);
                            scheduler.damage(rect, now());
                        }
                    }
                    ActiveStageOutput::Terminate(reason) => {
                        eprintln!("server closed the session: {reason}");
                        return Ok(Outcome::Disconnected);
                    }
                    ActiveStageOutput::DeactivateAll => {
                        // Windows does this when it attaches us to an existing session.
                        framed.get_inner().0.sock.set_nonblocking(false)?;
                        let mut sequence = activation.create();
                        let mut buf = ironrdp::core::WriteBuf::new();
                        let share_id = loop {
                            buf.clear();
                            let written = match sequence.next_pdu_hint() {
                                Some(hint) => sequence.step(&framed.read_by_hint(hint).context("reactivation")?, &mut buf)?,
                                None => sequence.step_no_input(&mut buf)?,
                            };
                            if let Some(len) = written.size() {
                                framed.write_all(&buf[..len]).context("reactivation")?;
                            }
                            if let ConnectionActivationState::Finalized { desktop_size, share_id, .. } = sequence.connection_activation_state() {
                                if (desktop_size.width, desktop_size.height) != (width, height) {
                                    bail!("server changed the resolution to {}x{}", desktop_size.width, desktop_size.height);
                                }
                                break share_id;
                            }
                        };
                        framed.get_inner().0.sock.set_nonblocking(true)?;
                        stage.set_share_id(share_id);
                        scheduler.request_full();
                    }
                    _ => {}
                }
            }
        }
    }
}
