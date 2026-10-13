use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tokenbubble_core::secret::Secret;
use tokenbubble_core::transport::{HttpTransport, Limits, Request, Transport, TransportError};

/// Serves one raw HTTP response and reports the received request head.
fn serve_once(response: Vec<u8>, delay: Duration) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/usage", listener.local_addr().unwrap());
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut head = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
            head.push_str(&line);
        }
        sender.send(head).unwrap();
        thread::sleep(delay);
        let _ = stream.write_all(&response);
    });
    (url, receiver)
}

fn request(url: String) -> Request {
    Request {
        url,
        bearer: Some(Secret::new("fixture-token")),
        headers: vec![("ChatGPT-Account-Id", "acct".to_owned())],
    }
}

fn limits(timeout_ms: u64, max_body_bytes: u64) -> Limits {
    Limits {
        timeout: Duration::from_millis(timeout_ms),
        max_body_bytes,
    }
}

#[test]
fn sends_headers_and_reads_status_body_and_retry_after() {
    let (url, received) = serve_once(
        b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 120\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
            .to_vec(),
        Duration::ZERO,
    );
    let response = HttpTransport::new(limits(5_000, 1024))
        .get(&request(url))
        .unwrap();

    assert_eq!(response.status, 429);
    assert_eq!(response.body, b"{}");
    assert_eq!(response.retry_after, Some(Duration::from_secs(120)));
    let head = received.recv().unwrap().to_ascii_lowercase();
    assert!(
        head.contains("authorization: bearer fixture-token"),
        "{head}"
    );
    assert!(head.contains("chatgpt-account-id: acct"), "{head}");
    assert!(head.contains("user-agent: tokenbubble/"), "{head}");
    assert!(!head.contains("cookie"), "{head}");
}

#[test]
fn refuses_redirects() {
    let (url, _received) = serve_once(
        b"HTTP/1.1 302 Found\r\nLocation: https://elsewhere.example/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec(),
        Duration::ZERO,
    );
    let error = HttpTransport::new(limits(5_000, 1024))
        .get(&request(url))
        .unwrap_err();
    assert_eq!(error, TransportError::Redirect { status: 302 });
}

#[test]
fn enforces_body_limit() {
    let body = "x".repeat(64);
    let raw = format!("HTTP/1.1 200 OK\r\nContent-Length: 64\r\nConnection: close\r\n\r\n{body}");
    let (url, _received) = serve_once(raw.into_bytes(), Duration::ZERO);
    let error = HttpTransport::new(limits(5_000, 16))
        .get(&request(url))
        .unwrap_err();
    assert_eq!(error, TransportError::BodyTooLarge { limit: 16 });
}

#[test]
fn enforces_deadline() {
    let (url, _received) = serve_once(
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
        Duration::from_secs(3),
    );
    let error = HttpTransport::new(limits(300, 1024))
        .get(&request(url))
        .unwrap_err();
    assert_eq!(error, TransportError::Timeout);
}
