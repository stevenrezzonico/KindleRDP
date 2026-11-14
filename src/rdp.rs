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
use crate::config::Config;
use crate::cred::{self, Pin};
use crate::device;
use crate::gesture::{Action, Gestures, Phase, Routed, TouchRouter};
use crate::rects::{Rect, Scheduler};
use crate::servers::Server;

type Framed = ironrdp_blocking::Framed<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>;

/// Touch contacts Windows may get at once.
const MAX_CONTACTS: u16 = 5;
/// How often resting fingers are restated to Windows.
const KEEPALIVE_MS: u64 = 100;

/// Why a session ended without an error.
pub enum Outcome {
    Disconnected,
    /// The user asked to rotate: reconnect at the new size.
    Rotate,
}

pub fn run(ui: &mut Ui, settings: &Config, server: &Server, password: String) -> anyhow::Result<Outcome> {
    let (w, h) = ui.fb.size();
    let (width, height) = (w as u16, h as u16);
    let (result, mut framed) = connect(settings, server, password, width, height)?;
    if (result.desktop_size.width, result.desktop_size.height) != (width, height) {
        bail!("server refused {width}x{height}, offered {}x{}", result.desktop_size.width, result.desktop_size.height);
    }
    session(result, &mut framed, ui, width, height, Scheduler::new(Rect { x: 0, y: 0, w, h }))
}

fn connect(settings: &Config, cfg: &Server, password: String, width: u16, height: u16) -> anyhow::Result<(ConnectionResult, Framed)> {
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
        desktop_scale_factor: settings.scale,
        hardware_id: None,
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        alternate_shell: String::new(),
        work_dir: String::new(),
    };

    let addr = (cfg.host.as_str(), cfg.port).to_socket_addrs().context("resolve host")?.next().context("host has no address")?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).context("TCP connect")?;
    tcp.set_nodelay(true)?;
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
    let pins_path = device::state("pins");
    let known = std::fs::read_to_string(&pins_path).unwrap_or_default();
    let host = format!("{}:{}", cfg.host, cfg.port);
    let fingerprint = cred::sha256_hex(cert);
    match cred::check_pin(&known, &host, &fingerprint) {
        Pin::Match => {}
        Pin::New => device::write_private(&pins_path, format!("{known}{host} {fingerprint}\n").as_bytes()).context("save certificate pin")?,
        Pin::Mismatch => bail!(
            "certificate of {host} changed. If you reinstalled that PC, delete its line in {} and retry",
            pins_path.display()
        ),
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

fn session(result: ConnectionResult, framed: &mut Framed, ui: &mut Ui, width: u16, height: u16, mut scheduler: Scheduler) -> anyhow::Result<Outcome> {
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

    loop {
        for (rect, wave) in scheduler.tick(now()) {
            ui.fb.refresh(rect, wave);
        }
        let touch_channel = touch_channel_ready(&mut stage);
        let keepalive = (touch_channel && router.held().next().is_some()).then(|| last_contact_frame + KEEPALIVE_MS);
        let wake = [keepalive, router.hold_deadline()].into_iter().flatten().map(|at| at.saturating_sub(now()));
        let timeout = scheduler.wait(now()).into_iter().chain(wake).min();
        let [net_ready, touch_ready] = device::poll([socket, ui.touch.fd()], timeout)?;
        let now_us = start.elapsed().as_micros() as u64;

        if touch_ready {
            ui.touch.read(ui.fb.rotation, &mut touches)?;
            for touch in touches.drain(..) {
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

        // Two fingers resting open the local menu. Take everything back from Windows first.
        let menu = router.take_hold(now());
        if menu {
            actions.clear();
            contacts.cancel_all(now_us);
            mouse = Gestures::new();
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

        let mut ops = Vec::new();
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
            let choice = ui.dialog("KindleRDP", &["Keyboard", "Disconnect", "Rotate screen", "Back to Windows"])?;
            if choice == 1 || choice == 2 {
                send_outputs(framed, stage.graceful_shutdown()?)?;
                return Ok(if choice == 1 { Outcome::Disconnected } else { Outcome::Rotate });
            }
            // The menu overwrote the session picture: put it back, with a cleaning flash.
            ui.forget();
            ui.fb.blit(image.data(), usize::from(width), screen);
            scheduler.request_full();
            if choice == 0 {
                // Win+Ctrl+O toggles Windows' On-Screen Keyboard. It needs no Windows setting.
                let keys = [Scancode::from_u8(true, 0x5B), Scancode::from_u8(false, 0x1D), Scancode::from_u8(false, 0x18)];
                let chord = keys.map(Operation::KeyPressed).into_iter().chain(keys.into_iter().rev().map(Operation::KeyReleased));
                let events = input.apply(chord);
                send_outputs(framed, stage.process_fastpath_input(&mut image, &events)?)?;
            }
        }

        // Drain the socket: TLS may hold several decrypted frames after one readable event.
        while net_ready {
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
                        ui.fb.blit(image.data(), usize::from(width), rect);
                        scheduler.damage(rect, now());
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
