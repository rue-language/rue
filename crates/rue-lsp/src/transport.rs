//! LSP base-protocol framing: `Content-Length` headers around JSON bodies.

use std::io::{self, BufRead, Write};

use serde_json::Value;

/// Bodies larger than this are rejected rather than buffered. Editors send
/// whole documents on every change under full synchronization, so the bound
/// sits well above the compiler's own per-source limit.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub enum Incoming {
    /// The client closed the stream.
    Eof,
    /// A well-framed body that is not JSON. The server answers it with a
    /// parse error and keeps reading.
    Malformed(String),
    Message(Value),
}

/// Read one framed message.
pub fn read_message(input: &mut impl BufRead) -> io::Result<Incoming> {
    let mut content_length = None;
    let mut saw_header = false;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return if saw_header {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream ended inside a message header",
                ))
            } else {
                Ok(Incoming::Eof)
            };
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if saw_header {
                break;
            }
            // Tolerate blank lines between messages.
            continue;
        }
        saw_header = true;
        let Some((name, value)) = line.split_once(':') else {
            return Err(invalid(format!("malformed header line `{line}`")));
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            let length = value
                .trim()
                .parse::<usize>()
                .map_err(|_| invalid(format!("bad Content-Length `{}`", value.trim())))?;
            content_length = Some(length);
        }
    }
    let length = content_length.ok_or_else(|| invalid("message without Content-Length".into()))?;
    if length > MAX_MESSAGE_BYTES {
        return Err(invalid(format!(
            "message of {length} bytes exceeds the {MAX_MESSAGE_BYTES}-byte limit"
        )));
    }
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    Ok(match serde_json::from_slice(&body) {
        Ok(value) => Incoming::Message(value),
        Err(error) => Incoming::Malformed(error.to_string()),
    })
}

/// Write one framed message and flush it.
pub fn write_message(output: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(io::Error::other)?;
    write!(output, "Content-Length: {}\r\n\r\n", body.len())?;
    output.write_all(&body)?;
    output.flush()
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn framed(body: &str) -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    #[test]
    fn messages_round_trip_through_framing() {
        let message = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        let mut bytes = Vec::new();
        write_message(&mut bytes, &message).unwrap();
        bytes.extend(framed("{\"a\":\"ü\"}"));
        let mut input = bytes.as_slice();
        match read_message(&mut input).unwrap() {
            Incoming::Message(value) => assert_eq!(value, message),
            other => panic!("unexpected {other:?}"),
        }
        match read_message(&mut input).unwrap() {
            Incoming::Message(value) => assert_eq!(value, json!({"a": "ü"})),
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(read_message(&mut input).unwrap(), Incoming::Eof));
    }

    #[test]
    fn a_non_json_body_is_reported_without_losing_the_stream() {
        let mut bytes = framed("{nope");
        bytes.extend(framed("null"));
        let mut input = bytes.as_slice();
        assert!(matches!(
            read_message(&mut input).unwrap(),
            Incoming::Malformed(_)
        ));
        assert!(matches!(
            read_message(&mut input).unwrap(),
            Incoming::Message(Value::Null)
        ));
    }

    #[test]
    fn a_header_without_length_is_an_error() {
        let mut input: &[u8] = b"Content-Type: x\r\n\r\n{}";
        assert!(read_message(&mut input).is_err());
    }
}
