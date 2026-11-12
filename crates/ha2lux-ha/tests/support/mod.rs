//! Fake Home Assistant server: HTTP, HTTPS (self-signed certificate) and WebSocket,
//! on real local sockets.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// No `Content-Length`: the body is streamed, then the connection is closed.
    pub streamed: bool,
    pub hang: bool,
}

impl Reply {
    pub fn json(body: &str) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.as_bytes().to_vec(),
            streamed: false,
            hang: false,
        }
    }

    pub fn state(state: &str) -> Self {
        Self::json(&format!(
            r#"{{"entity_id":"sensor.lux","state":"{state}","attributes":{{"device_class":"illuminance"}},"last_updated":"2026-11-11T20:00:00+00:00"}}"#
        ))
    }

    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            streamed: false,
            hang: false,
        }
    }

    pub fn hang() -> Self {
        Self {
            hang: true,
            ..Self::status(200)
        }
    }
}

pub struct Server {
    pub addr: SocketAddr,
    pub requests: Arc<Mutex<Vec<Recorded>>>,
}

impl Server {
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn count(&self) -> usize {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub fn last(&self) -> Recorded {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last()
            .cloned()
            .expect("aucune requête reçue")
    }
}

async fn read_request(stream: &mut (impl AsyncReadExt + Unpin)) -> Option<Recorded> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buffer.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..n]);
        if buffer.len() > 64 * 1024 {
            return None;
        }
    }
    let text = String::from_utf8_lossy(&buffer).into_owned();
    let mut lines = text.split("\r\n");
    let mut head = lines.next()?.split(' ');
    let (method, path) = (head.next()?.to_owned(), head.next()?.to_owned());
    let headers = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    Some(Recorded {
        method,
        path,
        headers,
    })
}

async fn write_reply(stream: &mut (impl AsyncWriteExt + Unpin), reply: &Reply) {
    if reply.hang {
        tokio::time::sleep(Duration::from_secs(3_600)).await;
        return;
    }
    let mut head = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", reply.status);
    for (k, v) in &reply.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if !reply.streamed {
        head.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
    }
    head.push_str("\r\n");
    // The client may have given up: write errors do not matter here.
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(&reply.body).await;
    let _ = stream.shutdown().await;
}

/// HTTP server: `handler(request, rank)` returns the response.
pub async fn http_server(
    handler: impl Fn(&Recorded, usize) -> Reply + Send + Sync + 'static,
) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests: Arc<Mutex<Vec<Recorded>>> = Arc::default();
    let (log, handler) = (Arc::clone(&requests), Arc::new(handler));
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let (log, handler) = (Arc::clone(&log), Arc::clone(&handler));
            tokio::spawn(async move {
                let Some(request) = read_request(&mut stream).await else {
                    return;
                };
                let index = {
                    let mut log = log.lock().unwrap_or_else(PoisonError::into_inner);
                    log.push(request.clone());
                    log.len() - 1
                };
                let reply = handler(&request, index);
                write_reply(&mut stream, &reply).await;
            });
        }
    });
    Server { addr, requests }
}

/// HTTPS server with a **self-signed** certificate (unknown to the system: must be rejected).
pub async fn https_server_with_self_signed_cert() -> Server {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio_rustls::TlsAcceptor;
    use tokio_rustls::rustls::ServerConfig;
    use tokio_rustls::rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

    let certified =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])
            .unwrap();
    let cert = certified.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certified.signing_key.serialize_der(),
    ));
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests: Arc<Mutex<Vec<Recorded>>> = Arc::default();
    let served = Arc::new(AtomicUsize::new(0));
    let log = Arc::clone(&requests);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (acceptor, log, served) = (acceptor.clone(), Arc::clone(&log), Arc::clone(&served));
            tokio::spawn(async move {
                // A handshake rejected by the client (unknown certificate) fails here.
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    return;
                };
                if let Some(request) = read_request(&mut tls).await {
                    log.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(request);
                    served.fetch_add(1, Ordering::SeqCst);
                    write_reply(&mut tls, &Reply::state("42")).await;
                }
            });
        }
    });
    Server { addr, requests }
}
