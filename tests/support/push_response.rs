//! Original smart-HTTP response fixture with a callback-to-server completion handshake.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

pub fn packet(payload: &[u8]) -> Vec<u8> {
    [format!("{:04x}", payload.len() + 4).as_bytes(), payload].concat()
}

/// The server sends `first`, then requires callback acknowledgement before sending `rest`.
/// The timeout is a deadlock watchdog, not the evidence that notification preceded EOF.
pub fn serve(
    sideband: bool,
    first: Vec<u8>,
    rest: Vec<u8>,
    handshake: bool,
) -> (String, Sender<()>, JoinHandle<()>) {
    let caps = if sideband { " side-band-64k" } else { "" };
    let advertisement = [
        packet(b"# service=git-receive-pack\n"),
        b"0000".to_vec(),
        packet(
            format!(
                "{} capabilities^{{}}\0report-status{caps}\n",
                "0".repeat(40)
            )
            .as_bytes(),
        ),
        b"0000".to_vec(),
    ]
    .concat();
    serve_response("git-receive-pack", advertisement, first, rest, handshake)
}

pub fn serve_fetch(
    id: girt::ObjectId,
    first: Vec<u8>,
    rest: Vec<u8>,
    handshake: bool,
) -> (String, Sender<()>, JoinHandle<()>) {
    let advertisement = [
        packet(b"# service=git-upload-pack\n"),
        b"0000".to_vec(),
        packet(format!("{id} refs/heads/main\0side-band-64k shallow\n").as_bytes()),
        b"0000".to_vec(),
    ]
    .concat();
    serve_response("git-upload-pack", advertisement, first, rest, handshake)
}

fn serve_response(
    service: &'static str,
    advertisement: Vec<u8>,
    first: Vec<u8>,
    rest: Vec<u8>,
    handshake: bool,
) -> (String, Sender<()>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/repo", listener.local_addr().unwrap());
    let (acknowledge, received) = channel();
    let server = std::thread::spawn(move || {
        let mut discovery = request(&listener);
        header(
            &mut discovery,
            service,
            "advertisement",
            advertisement.len(),
        );
        discovery.write_all(&advertisement).unwrap();
        drop(discovery);
        let mut push = request(&listener);
        header(&mut push, service, "result", first.len() + rest.len());
        push.write_all(&first).unwrap();
        push.flush().unwrap();
        if handshake {
            received
                .recv_timeout(Duration::from_secs(10))
                .expect("progress callback must run before response completion");
        }
        // Cancellation may close the socket after the acknowledged prefix.
        let _ = push.write_all(&rest);
    });
    (url, acknowledge, server)
}

fn request(listener: &TcpListener) -> TcpStream {
    let (mut stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
    }
    let header = String::from_utf8(header).unwrap();
    let length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    stream.read_exact(&mut vec![0; length]).unwrap();
    stream
}

fn header(stream: &mut TcpStream, service: &str, kind: &str, length: usize) {
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/x-{service}-{kind}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n").unwrap();
}
