//! A canned OpenAI-compatible endpoint, for tests that need to read what a run
//! actually put on the wire rather than ask a struct what it would have.
//!
//! Every request body is recorded, so a test can assert on what was sent as well
//! as on what the run did with the answer.

// Compiled into every test binary that pulls in `common`, most of which want
// none of this.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

/// Every `/chat/completions` body the endpoint was sent.
pub type Bodies = Arc<Mutex<Vec<String>>>;

/// One plain text answer, which ends the turn loop after a single request.
pub fn text_answer(text: &str) -> String {
    let content = serde_json::json!({"content": text});
    [
        format!(r#"data: {{"choices":[{{"delta":{content}}}]}}"#),
        r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#.to_string(),
        "data: [DONE]".to_string(),
    ]
    .join("\n\n")
        + "\n\n"
}

/// Answer `/chat/completions` with `reply(body)`, recording every body first.
///
/// `reply` takes the request body rather than a counter: afi probes the context
/// window on the side, and counting requests hands the probe the answer meant
/// for the first turn.
pub fn serve<R>(listener: TcpListener, bodies: &Bodies, reply: R) -> JoinHandle<()>
where
    R: Fn(&str) -> String + Send + 'static,
{
    let bodies = Arc::clone(bodies);
    thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            answer(stream, &bodies, &reply);
        }
    })
}

fn answer<R: Fn(&str) -> String>(mut stream: TcpStream, bodies: &Bodies, reply: &R) {
    let mut reader = BufReader::new(stream.try_clone().expect("the socket must clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let body = read_body(&mut reader);
    let response = if request_line.contains("/chat/completions") {
        bodies
            .lock()
            .expect("the lock must hold")
            .push(body.clone());
        let sse = reply(&body);
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{sse}",
            sse.len()
        )
    } else {
        // The context-window probe. 404 is a fine answer; afi falls back.
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
    };
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Read past the headers and whatever body they announce, so the client is not
/// answered before it has finished sending.
fn read_body(reader: &mut BufReader<TcpStream>) -> String {
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    let _ = reader.read_exact(&mut body);
    String::from_utf8_lossy(&body).into_owned()
}
