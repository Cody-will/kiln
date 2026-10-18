//! TLS for Kiln programs: a small C ABI over [rustls].
//!
//! rustls is used as a pure state machine. The Kiln side (`std::net::tls`)
//! owns the TCP socket and moves ciphertext between it and the connection
//! with [`kn_tls_read_tls`] / [`kn_tls_write_tls`], so all socket waits go
//! through Kiln's scheduler and timeouts. No threads or sockets live here.
//!
//! Return conventions: counts are `>= 0`; `-1` means "would block" (needs
//! more input), `-2` means an error whose message [`kn_tls_error`] gives.

#![allow(clippy::missing_safety_doc)]

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{
    ClientConfig, ClientConnection, Connection, RootCertStore, ServerConfig, ServerConnection,
};
use std::io::{ErrorKind, Read, Write};
use std::sync::Arc;

/// A client or server TLS connection, plus its last error.
pub struct Conn {
    c: Connection,
    err: String,
}

/// A server configuration (certificate chain and key).
pub struct Server {
    cfg: Arc<ServerConfig>,
}

/// A client configuration (trusted roots).
pub struct Client {
    cfg: Arc<ClientConfig>,
}

unsafe fn bytes<'a>(p: *const u8, n: usize) -> &'a [u8] {
    if n == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(p, n)
    }
}

unsafe fn bytes_mut<'a>(p: *mut u8, n: usize) -> &'a mut [u8] {
    if n == 0 {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(p, n)
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Writes `msg` into `out` (truncated), returning its full length.
unsafe fn put(msg: &str, out: *mut u8, cap: usize) -> usize {
    let b = msg.as_bytes();
    let n = b.len().min(cap);
    bytes_mut(out, cap)[..n].copy_from_slice(&b[..n]);
    b.len()
}

/// A client config trusting the Mozilla roots (webpki-roots) plus any PEM
/// certificates in `extra` (may be empty). Null on error, with the message
/// in `err`.
#[no_mangle]
pub unsafe extern "C" fn kn_tls_client_config(
    extra: *const u8,
    extra_len: usize,
    insecure: i32,
    err: *mut u8,
    err_cap: usize,
) -> *mut Client {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let pem = bytes(extra, extra_len);
    if !pem.is_empty() {
        let mut n = 0;
        for c in CertificateDer::pem_slice_iter(pem) {
            match c {
                Ok(c) => {
                    if let Err(e) = roots.add(c) {
                        put(&format!("bad root certificate: {e}"), err, err_cap);
                        return std::ptr::null_mut();
                    }
                    n += 1;
                }
                Err(e) => {
                    put(&format!("bad PEM: {e:?}"), err, err_cap);
                    return std::ptr::null_mut();
                }
            }
        }
        if n == 0 {
            put("no certificates found in PEM", err, err_cap);
            return std::ptr::null_mut();
        }
    }
    let builder = match ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
    {
        Ok(b) => b,
        Err(e) => {
            put(&e.to_string(), err, err_cap);
            return std::ptr::null_mut();
        }
    };
    let mut cfg = builder.with_root_certificates(roots).with_no_client_auth();
    if insecure != 0 {
        cfg.dangerous()
            .set_certificate_verifier(Arc::new(NoVerify(provider())));
    }
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Box::into_raw(Box::new(Client { cfg: Arc::new(cfg) }))
}

/// Frees a client config.
#[no_mangle]
pub unsafe extern "C" fn kn_tls_client_config_free(c: *mut Client) {
    if !c.is_null() {
        drop(Box::from_raw(c));
    }
}

/// A server config from a PEM certificate chain and a PEM private key
/// (PKCS#8, PKCS#1, or SEC1). Null on error, with the message in `err`.
#[no_mangle]
pub unsafe extern "C" fn kn_tls_server_config(
    cert: *const u8,
    cert_len: usize,
    key: *const u8,
    key_len: usize,
    err: *mut u8,
    err_cap: usize,
) -> *mut Server {
    let certs: Result<Vec<CertificateDer<'static>>, _> =
        CertificateDer::pem_slice_iter(bytes(cert, cert_len)).collect();
    let certs = match certs {
        Ok(c) if !c.is_empty() => c,
        Ok(_) => {
            put("no certificates found in the certificate PEM", err, err_cap);
            return std::ptr::null_mut();
        }
        Err(e) => {
            put(&format!("bad certificate PEM: {e:?}"), err, err_cap);
            return std::ptr::null_mut();
        }
    };
    let key = match PrivateKeyDer::from_pem_slice(bytes(key, key_len)) {
        Ok(k) => k,
        Err(e) => {
            put(&format!("bad private key PEM: {e:?}"), err, err_cap);
            return std::ptr::null_mut();
        }
    };
    let builder = match ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
    {
        Ok(b) => b,
        Err(e) => {
            put(&e.to_string(), err, err_cap);
            return std::ptr::null_mut();
        }
    };
    match builder.with_no_client_auth().with_single_cert(certs, key) {
        Ok(mut cfg) => {
            cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
            Box::into_raw(Box::new(Server { cfg: Arc::new(cfg) }))
        }
        Err(e) => {
            put(&format!("certificate and key: {e}"), err, err_cap);
            std::ptr::null_mut()
        }
    }
}

/// Frees a server config.
#[no_mangle]
pub unsafe extern "C" fn kn_tls_server_config_free(s: *mut Server) {
    if !s.is_null() {
        drop(Box::from_raw(s));
    }
}

/// A client connection to `host` (checked against the certificate).
#[no_mangle]
pub unsafe extern "C" fn kn_tls_client(
    cfg: *const Client,
    host: *const u8,
    host_len: usize,
    err: *mut u8,
    err_cap: usize,
) -> *mut Conn {
    let host = String::from_utf8_lossy(bytes(host, host_len)).into_owned();
    let name = match ServerName::try_from(host.clone()) {
        Ok(n) => n,
        Err(_) => {
            put(&format!("invalid server name {host:?}"), err, err_cap);
            return std::ptr::null_mut();
        }
    };
    match ClientConnection::new((*cfg).cfg.clone(), name) {
        Ok(c) => Box::into_raw(Box::new(Conn {
            c: c.into(),
            err: String::new(),
        })),
        Err(e) => {
            put(&e.to_string(), err, err_cap);
            std::ptr::null_mut()
        }
    }
}

/// A server connection.
#[no_mangle]
pub unsafe extern "C" fn kn_tls_server(cfg: *const Server) -> *mut Conn {
    match ServerConnection::new((*cfg).cfg.clone()) {
        Ok(c) => Box::into_raw(Box::new(Conn {
            c: c.into(),
            err: String::new(),
        })),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Frees a connection.
#[no_mangle]
pub unsafe extern "C" fn kn_tls_free(c: *mut Conn) {
    if !c.is_null() {
        drop(Box::from_raw(c));
    }
}

/// Feeds `n` bytes of ciphertext from the peer; returns how many were
/// taken, -1 if no more is wanted until plaintext is read, or -2 on a
/// protocol error.
#[no_mangle]
pub unsafe extern "C" fn kn_tls_read_tls(c: *mut Conn, buf: *const u8, n: usize) -> i64 {
    let c = &mut *c;
    if !c.c.wants_read() {
        // Decrypted data is waiting; the caller keeps the rest for later.
        return -1;
    }
    let mut src = bytes(buf, n);
    let taken = match c.c.read_tls(&mut src) {
        Ok(k) => k,
        Err(e) => {
            c.err = e.to_string();
            return -2;
        }
    };
    if let Err(e) = c.c.process_new_packets() {
        c.err = e.to_string();
        // Queue the alert so it can still be sent.
        return -2;
    }
    taken as i64
}

/// Moves ciphertext to send into `buf`; returns the count (0: none).
#[no_mangle]
pub unsafe extern "C" fn kn_tls_write_tls(c: *mut Conn, buf: *mut u8, cap: usize) -> i64 {
    let c = &mut *c;
    let mut dst = bytes_mut(buf, cap);
    match c.c.write_tls(&mut dst) {
        Ok(k) => k as i64,
        Err(e) => {
            c.err = e.to_string();
            -2
        }
    }
}

/// Whether ciphertext is waiting to be sent.
#[no_mangle]
pub unsafe extern "C" fn kn_tls_wants_write(c: *const Conn) -> i32 {
    (*c).c.wants_write() as i32
}

/// Whether the handshake is still in progress.
#[no_mangle]
pub unsafe extern "C" fn kn_tls_handshaking(c: *const Conn) -> i32 {
    (*c).c.is_handshaking() as i32
}

/// Reads decrypted data: the count, 0 at a clean close (close_notify),
/// -1 if more ciphertext is needed, -2 on error.
#[no_mangle]
pub unsafe extern "C" fn kn_tls_read(c: *mut Conn, buf: *mut u8, cap: usize) -> i64 {
    let c = &mut *c;
    let mut r = c.c.reader().read(bytes_mut(buf, cap));
    if matches!(&r, Err(e) if e.kind() == ErrorKind::WouldBlock) {
        // Records may be buffered but not yet decrypted (rustls stops
        // when its plaintext buffer fills); process them before asking
        // the socket for more.
        if let Err(e) = c.c.process_new_packets() {
            c.err = e.to_string();
            return -2;
        }
        r = c.c.reader().read(bytes_mut(buf, cap));
    }
    match r {
        Ok(k) => k as i64,
        Err(e) if e.kind() == ErrorKind::WouldBlock => -1,
        Err(e) => {
            c.err = e.to_string();
            -2
        }
    }
}

/// Queues `n` bytes of plaintext for sending; returns the count taken.
#[no_mangle]
pub unsafe extern "C" fn kn_tls_write(c: *mut Conn, buf: *const u8, n: usize) -> i64 {
    let c = &mut *c;
    match c.c.writer().write(bytes(buf, n)) {
        Ok(k) => k as i64,
        Err(e) => {
            c.err = e.to_string();
            -2
        }
    }
}

/// Queues a close_notify alert.
#[no_mangle]
pub unsafe extern "C" fn kn_tls_close(c: *mut Conn) {
    (*c).c.send_close_notify();
}

/// The last error message, copied into `out`; returns its full length.
#[no_mangle]
pub unsafe extern "C" fn kn_tls_error(c: *const Conn, out: *mut u8, cap: usize) -> usize {
    put(&(*c).err, out, cap)
}

/// The negotiated protocol version: 12 or 13 (0 before the handshake).
#[no_mangle]
pub unsafe extern "C" fn kn_tls_version(c: *const Conn) -> i32 {
    match (*c).c.protocol_version() {
        Some(rustls::ProtocolVersion::TLSv1_2) => 12,
        Some(rustls::ProtocolVersion::TLSv1_3) => 13,
        _ => 0,
    }
}

/// Parses an ALPN list: protocol names separated by commas (`h2,http/1.1`).
unsafe fn alpn_list(p: *const u8, n: usize) -> Vec<Vec<u8>> {
    bytes(p, n)
        .split(|b| *b == b',')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_vec())
        .collect()
}

/// Sets the protocols a client offers with ALPN, most preferred first.
#[no_mangle]
pub unsafe extern "C" fn kn_tls_client_config_alpn(c: *mut Client, list: *const u8, n: usize) {
    let mut cfg = (*(*c).cfg).clone();
    cfg.alpn_protocols = alpn_list(list, n);
    (*c).cfg = Arc::new(cfg);
}

/// Sets the protocols a server accepts with ALPN, most preferred first.
#[no_mangle]
pub unsafe extern "C" fn kn_tls_server_config_alpn(s: *mut Server, list: *const u8, n: usize) {
    let mut cfg = (*(*s).cfg).clone();
    cfg.alpn_protocols = alpn_list(list, n);
    (*s).cfg = Arc::new(cfg);
}

/// The protocol agreed with ALPN, written to `out`; its length (0 if none).
#[no_mangle]
pub unsafe extern "C" fn kn_tls_alpn(c: *const Conn, out: *mut u8, cap: usize) -> usize {
    match (*c).c.alpn_protocol() {
        Some(p) => put(&String::from_utf8_lossy(p), out, cap),
        None => 0,
    }
}

/// Accepts any server certificate (for tests against self-signed servers).
#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
