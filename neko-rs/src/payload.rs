//! Binary data channel protocol, ported from server/internal/webrtc/payload
//! (Go) and verified against client/core/src/input/datachannel.ts.
//!
//! Wire format: `[u8 opcode][u16 BE payload length][payload...]`; the length
//! covers the payload only (Go semantics — see encodePong in the old server).

use bytes::{Buf, BufMut, Bytes, BytesMut};

pub const OP_MOVE: u8 = 0x01;
pub const OP_SCROLL: u8 = 0x02;
pub const OP_KEY_DOWN: u8 = 0x03;
pub const OP_KEY_UP: u8 = 0x04;
pub const OP_BTN_DOWN: u8 = 0x05;
pub const OP_BTN_UP: u8 = 0x06;
pub const OP_PING: u8 = 0x07;
pub const OP_TOUCH_BEGIN: u8 = 0x08;
pub const OP_TOUCH_UPDATE: u8 = 0x09;
pub const OP_TOUCH_END: u8 = 0x0a;

// server -> client
pub const OP_CURSOR_POSITION: u8 = 0x01;
pub const OP_CURSOR_IMAGE: u8 = 0x02;
pub const OP_PONG: u8 = 0x03;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("message too short: {0}")]
    TooShort(usize),
    #[error("length mismatch: header {header} != actual {actual}")]
    LengthMismatch { header: usize, actual: usize },
    #[error("unknown opcode {0:#x}")]
    UnknownOp(u8),
    #[error("payload too large: {0}")]
    TooLarge(usize),
}

/// Messages the client sends over the data channel / control websocket.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    Move { x: u16, y: u16 },
    Scroll { delta_x: i16, delta_y: i16, control_key: bool },
    KeyDown { key: u32 },
    KeyUp { key: u32 },
    ButtonDown { key: u32 },
    ButtonUp { key: u32 },
    Ping { client_ts1: u32, client_ts2: u32 },
    TouchBegin { touch_id: u32, x: i32, y: i32, pressure: u8 },
    TouchUpdate { touch_id: u32, x: i32, y: i32, pressure: u8 },
    TouchEnd { touch_id: u32, x: i32, y: i32, pressure: u8 },
}

/// Messages the server sends back (cursor + pong).
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    CursorPosition { x: u16, y: u16 },
    CursorImage {
        width: u16,
        height: u16,
        x: u16,
        y: u16,
        png: Bytes,
    },
    Pong {
        client_ts1: u32,
        client_ts2: u32,
        server_ts1: u32,
        server_ts2: u32,
    },
}

/// The `length` field counts the WHOLE message including the 3-byte header
/// (Go semantics — see the embedded handler.go: OP_CURSOR_POSITION has
/// Length 7 with a 4-byte body, OP_PONG has Length 19 with a 16-byte body).
fn header(op: u8, body_len: usize, out: &mut BytesMut) {
    let total = 3 + body_len;
    debug_assert!(total <= u16::MAX as usize);
    out.put_u8(op);
    out.put_u16(total as u16);
}

/// Mirrors the Go legacy quirk: an old 4-byte scroll (x, y: u16) arrives
/// with length 4 and no control_key byte.
#[derive(Debug, Clone, PartialEq)]
pub enum ScrollPayload {
    Legacy { x: u16, y: u16 },
    Full { delta_x: i16, delta_y: i16, control_key: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decoded {
    Msg(ClientMessage),
    Scroll(ScrollPayload),
}

pub fn decode_client(data: &[u8]) -> Result<Decoded, Error> {
    if data.len() < 3 {
        return Err(Error::TooShort(data.len()));
    }
    let op = data[0];
    let len = u16::from_be_bytes([data[1], data[2]]) as usize;
    let body = &data[3..];
    if len != body.len() {
        return Err(Error::LengthMismatch {
            header: len,
            actual: body.len(),
        });
    }
    let mut b = body;

    let msg = match op {
        OP_MOVE => ClientMessage::Move {
            x: b.get_u16(),
            y: b.get_u16(),
        },
        OP_SCROLL => {
            let scroll = if len == 4 {
                ScrollPayload::Legacy {
                    x: b.get_u16(),
                    y: b.get_u16(),
                }
            } else {
                ScrollPayload::Full {
                    delta_x: b.get_i16(),
                    delta_y: b.get_i16(),
                    control_key: b.get_u8() != 0,
                }
            };
            return Ok(Decoded::Scroll(scroll));
        }
        OP_KEY_DOWN => ClientMessage::KeyDown { key: b.get_u32() },
        OP_KEY_UP => ClientMessage::KeyUp { key: b.get_u32() },
        OP_BTN_DOWN => ClientMessage::ButtonDown { key: b.get_u32() },
        OP_BTN_UP => ClientMessage::ButtonUp { key: b.get_u32() },
        OP_PING => ClientMessage::Ping {
            client_ts1: b.get_u32(),
            client_ts2: b.get_u32(),
        },
        OP_TOUCH_BEGIN | OP_TOUCH_UPDATE | OP_TOUCH_END => {
            let touch_id = b.get_u32();
            let x = b.get_i32();
            let y = b.get_i32();
            let pressure = b.get_u8();
            match op {
                OP_TOUCH_BEGIN => ClientMessage::TouchBegin { touch_id, x, y, pressure },
                OP_TOUCH_UPDATE => ClientMessage::TouchUpdate { touch_id, x, y, pressure },
                _ => ClientMessage::TouchEnd { touch_id, x, y, pressure },
            }
        }
        other => return Err(Error::UnknownOp(other)),
    };

    Ok(Decoded::Msg(msg))
}

impl ServerMessage {
    pub fn encode(&self) -> Bytes {
        let mut out = BytesMut::new();
        match *self {
            ServerMessage::CursorPosition { x, y } => {
                header(OP_CURSOR_POSITION, 4, &mut out);
                out.put_u16(x);
                out.put_u16(y);
            }
            ServerMessage::CursorImage {
                width,
                height,
                x,
                y,
                ref png,
            } => {
                header(OP_CURSOR_IMAGE, 8 + png.len(), &mut out);
                out.put_u16(width);
                out.put_u16(height);
                out.put_u16(x);
                out.put_u16(y);
                out.extend_from_slice(png);
            }
            ServerMessage::Pong {
                client_ts1,
                client_ts2,
                server_ts1,
                server_ts2,
            } => {
                header(OP_PONG, 16, &mut out);
                out.put_u32(client_ts1);
                out.put_u32(client_ts2);
                out.put_u32(server_ts1);
                out.put_u32(server_ts2);
            }
        }
        out.freeze()
    }
}

impl ClientMessage {
    /// length of the payload body (after the 3-byte header)
    pub fn body_len(&self) -> usize {
        match *self {
            ClientMessage::Move { .. } => 4,
            ClientMessage::Scroll { .. } => 5,
            ClientMessage::KeyDown { .. }
            | ClientMessage::KeyUp { .. }
            | ClientMessage::ButtonDown { .. }
            | ClientMessage::ButtonUp { .. } => 4,
            ClientMessage::Ping { .. } => 8,
            ClientMessage::TouchBegin { .. }
            | ClientMessage::TouchUpdate { .. }
            | ClientMessage::TouchEnd { .. } => 13,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(op: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![op, (body.len() >> 8) as u8, body.len() as u8];
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn cursor_position_roundtrip() {
        let bytes = ServerMessage::CursorPosition { x: 1920, y: 1080 }.encode();
        // field = total = 3 + 4; body follows
        assert_eq!(
            bytes.to_vec(),
            vec![OP_CURSOR_POSITION, 0, 7, 0x07, 0x80, 0x04, 0x38]
        );
    }

    #[test]
    fn pong_layout_matches_go() {
        // Go: header{event, Length:19} + ping(8) + server_ts(8) = 19 bytes total
        let bytes = ServerMessage::Pong {
            client_ts1: 1,
            client_ts2: 2,
            server_ts1: 3,
            server_ts2: 4,
        }
        .encode();
        assert_eq!(bytes.len(), 19);
        assert_eq!(bytes[2], 19);
    }

    #[test]
    fn decode_move() {
        let msg = decode_client(&frame(OP_MOVE, &[0x00, 0x0a, 0x00, 0x14])).unwrap();
        assert_eq!(msg, Decoded::Msg(ClientMessage::Move { x: 10, y: 20 }));
    }

    #[test]
    fn decode_legacy_scroll() {
        let scroll = decode_client(&frame(OP_SCROLL, &[0x00, 0x05, 0x00, 0x06])).unwrap();
        assert_eq!(scroll, Decoded::Scroll(ScrollPayload::Legacy { x: 5, y: 6 }));
    }

    #[test]
    fn decode_full_scroll() {
        let body = [0xff, 0xf6, 0x00, 0x19, 0x01]; // dx=-10, dy=25, ctrl
        let scroll = decode_client(&frame(OP_SCROLL, &body)).unwrap();
        assert_eq!(
            scroll,
            Decoded::Scroll(ScrollPayload::Full {
                delta_x: -10,
                delta_y: 25,
                control_key: true
            })
        );
    }

    #[test]
    fn length_mismatch_rejected() {
        assert!(matches!(
            decode_client(&[OP_MOVE, 0, 7, 1, 2, 3, 4]),
            Err(Error::LengthMismatch { .. })
        ));
    }
}
