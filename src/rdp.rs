//! The RDP session: connect (TLS + NLA), then one poll loop over the socket and the touch screen.

use std::io::Write as _;
use std::net::TcpStream;
use std::os::fd::AsRawFd as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use ironrdp::connector::connection_activation::ConnectionActivationState;
use ironrdp::connector::{self, ConnectionResult, Credentials, Sequence as _};
use ironrdp::graphics::image_processing::PixelFormat;
use ironrdp::input::{Database, MouseButton, MousePosition, Operation, WheelRotations};
use ironrdp::pdu::gcc::KeyboardType;
use ironrdp::pdu::rdp::capability_sets::MajorPlatformType;
use ironrdp::pdu::rdp::client_info::{CompressionType, PerformanceFlags, TimezoneInfo};
use ironrdp::session::image::DecodedImage;
use ironrdp::session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use crate::config::{self, Config};
use crate::cred::{self, Pin};
use crate::device::{self, Fb, TouchScreen};
use crate::gesture::{Action, Gestures};
use crate::rects::{Rect, Scheduler};

type Framed = ironrdp_blocking::Framed<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>;

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

fn run() -> anyhow::Result<()> {
    let dir = std::env::current_exe()?.parent().context("exe has no directory")?.to_owned();
    let config_path = dir.join("config.ini");
    let config_text = std::fs::read_to_string(&config_path).with_context(|| format!("read {}", config_path.display()))?;
    let cfg = config::parse(&config_text).map_err(anyhow::Error::msg)?;
    device::set_fbink(cfg.fbink.as_deref());

    let account = format!("{}@{}:{}", cfg.user, cfg.host, cfg.port);
    let serial = device::serial();
    let cred_path = device::state("cred");
    let password = match &cfg.password {
        Some(p) => p.clone(),
        None => std::fs::read(&cred_path)
            .ok()
            .and_then(|blob| cred::open(&serial, &account, &blob))
            .context("no saved password for this host: add password = ... to config.ini")?,
    };

    let fb = Fb::open(&cfg.fast_waveform)?;
    let (fb_w, fb_h) = fb.fb_size();
    let mut touch = TouchScreen::open(fb_w, fb_h)?;
    let (width, height) = fb.session_size();

    device::message(&format!("Connecting to {}...", cfg.host));
    let (result, mut framed) = connect(&cfg, password.clone(), width, height)?;

    // Login worked: move the password off the USB-visible partition.
    if cfg.password.is_some() {
        let saved = device::random_nonce()
            .and_then(|nonce| device::write_private(&cred_path, &cred::seal(&serial, &account, nonce, &password)));
        match saved {
            Ok(()) => std::fs::write(&config_path, config::strip_password(&config_text)).context("remove password from config.ini")?,
            Err(e) => eprintln!("password not saved, left in config.ini: {e:#}"),
        }
    }

    if (result.desktop_size.width, result.desktop_size.height) != (width, height) {
        bail!("server refused {width}x{height}, offered {}x{}", result.desktop_size.width, result.desktop_size.height);
    }
    session(result, &mut framed, &fb, &mut touch, width, height)
}

fn connect(cfg: &Config, password: String, width: u16, height: u16) -> anyhow::Result<(ConnectionResult, Framed)> {
    use std::net::ToSocketAddrs as _;
    let rdp_config = connector::Config {
        credentials: Credentials::UsernamePassword { username: cfg.user.clone(), password },
        domain: cfg.domain.clone(),
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
        desktop_scale_factor: cfg.scale,
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
    let mut connector = connector::ClientConnector::new(rdp_config, client_addr);
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

fn tls_upgrade(tcp: TcpStream, cfg: &Config) -> anyhow::Result<(rustls::StreamOwned<rustls::ClientConnection, TcpStream>, Vec<u8>)> {
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

fn session(result: ConnectionResult, framed: &mut Framed, fb: &Fb, touch: &mut TouchScreen, width: u16, height: u16) -> anyhow::Result<()> {
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
    let mut scheduler = Scheduler::new(screen);
    let mut gestures = Gestures::new(screen.w);
    let mut input = Database::new();
    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u64;

    let socket = framed.get_inner().0.sock.as_raw_fd();
    framed.get_inner().0.sock.set_nonblocking(true)?;
    let (mut touches, mut actions) = (Vec::new(), Vec::new());

    loop {
        for (rect, wave) in scheduler.tick(now()) {
            fb.refresh(rect, wave);
        }
        let [net_ready, touch_ready] = device::poll([socket, touch.fd()], scheduler.wait(now()))?;

        if touch_ready {
            touch.read(&mut touches)?;
            for t in touches.drain(..) {
                gestures.feed(t, now(), &mut actions);
            }
            let mut ops = Vec::new();
            for action in actions.drain(..) {
                match action {
                    Action::Move(x, y) => ops.push(Operation::MouseMove(MousePosition {
                        x: x.clamp(0, screen.w - 1) as u16,
                        y: y.clamp(0, screen.h - 1) as u16,
                    })),
                    Action::Left(true) => ops.push(Operation::MouseButtonPressed(MouseButton::Left)),
                    Action::Left(false) => ops.push(Operation::MouseButtonReleased(MouseButton::Left)),
                    Action::Right(true) => ops.push(Operation::MouseButtonPressed(MouseButton::Right)),
                    Action::Right(false) => ops.push(Operation::MouseButtonReleased(MouseButton::Right)),
                    Action::Wheel(n) => ops.push(Operation::WheelRotations(WheelRotations { is_vertical: true, rotation_units: (n * 120) as i16 })),
                    Action::FullRefresh => scheduler.request_full(),
                    Action::Exit => {
                        for out in stage.graceful_shutdown()? {
                            if let ActiveStageOutput::ResponseFrame(frame) = out {
                                send(framed, &frame)?;
                            }
                        }
                        return Ok(());
                    }
                }
            }
            let events = input.apply(ops);
            for out in stage.process_fastpath_input(&mut image, &events)? {
                if let ActiveStageOutput::ResponseFrame(frame) = out {
                    send(framed, &frame)?;
                }
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
                        fb.blit(image.data(), usize::from(width), rect);
                        scheduler.damage(rect, now());
                    }
                    ActiveStageOutput::Terminate(reason) => {
                        eprintln!("server closed the session: {reason}");
                        return Ok(());
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
