use base64::{decode, encode};
use bytes::{BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use std::char;
#[cfg(feature = "server")]
use std::collections::VecDeque;
use std::convert::TryFrom;
use std::convert::TryInto;
use std::ops::Index;
#[cfg(feature = "server")]
use std::str::from_utf8;

use crate::{Error, Result, Sid};

const SEPARATOR: char = '\x1e';

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum PacketType {
    Open,
    Close,
    Ping,
    Pong,
    Message,
    MessageBinary,
    Upgrade,
    Noop,
}

impl From<PacketType> for String {
    fn from(packet: PacketType) -> Self {
        match packet {
            PacketType::MessageBinary => "b".to_owned(),
            _ => (u8::from(packet)).to_string(),
        }
    }
}

impl From<PacketType> for u8 {
    fn from(ptype: PacketType) -> Self {
        match ptype {
            PacketType::Open => 0,
            PacketType::Close => 1,
            PacketType::Ping => 2,
            PacketType::Pong => 3,
            PacketType::Message => 4,
            PacketType::MessageBinary => 4,
            PacketType::Upgrade => 5,
            PacketType::Noop => 6,
        }
    }
}

impl TryFrom<u8> for PacketType {
    type Error = Error;
    /// Converts a byte into the corresponding `PacketType`.
    fn try_from(b: u8) -> Result<PacketType> {
        match b {
            0 | b'0' => Ok(PacketType::Open),
            1 | b'1' => Ok(PacketType::Close),
            2 | b'2' => Ok(PacketType::Ping),
            3 | b'3' => Ok(PacketType::Pong),
            4 | b'4' => Ok(PacketType::Message),
            5 | b'5' => Ok(PacketType::Upgrade),
            6 | b'6' => Ok(PacketType::Noop),
            _ => Err(Error::InvalidPacketType(b)),
        }
    }
}

/// A `Packet` sent via the `engine.io` protocol.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Packet {
    pub ptype: PacketType,
    pub data: Bytes,
}

/// Data which gets exchanged in a handshake as defined by the server.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HandshakePacket {
    pub sid: Sid,
    pub upgrades: Vec<String>,
    pub ping_interval: u64,
    pub ping_timeout: u64,
    pub max_payload: usize,
}

impl TryFrom<Packet> for HandshakePacket {
    type Error = Error;
    fn try_from(packet: Packet) -> Result<HandshakePacket> {
        Ok(serde_json::from_slice(packet.data[..].as_ref())?)
    }
}

impl Packet {
    pub fn new<T: Into<Bytes>>(ptype: PacketType, data: T) -> Self {
        Packet {
            ptype,
            data: data.into(),
        }
    }

    pub fn noop() -> Self {
        Packet {
            ptype: PacketType::Noop,
            data: Bytes::new(),
        }
    }
}

impl TryFrom<Bytes> for Packet {
    type Error = Error;
    /// Decodes a single `Packet` from an `u8` byte stream.
    fn try_from(
        bytes: Bytes,
    ) -> std::result::Result<Self, <Self as std::convert::TryFrom<Bytes>>::Error> {
        if bytes.is_empty() {
            return Err(Error::IncompletePacket());
        }

        let is_base64 = *bytes.first().ok_or(Error::IncompletePacket())? == b'b';

        // only 'messages' packets could be encoded
        let ptype = if is_base64 {
            PacketType::MessageBinary
        } else {
            (*bytes.first().ok_or(Error::IncompletePacket())? as u8).try_into()?
        };

        if bytes.len() == 1 && ptype == PacketType::Message {
            return Err(Error::IncompletePacket());
        }

        let data: Bytes = bytes.slice(1..);

        Ok(Packet {
            ptype,
            data: if is_base64 {
                Bytes::from(decode(data.as_ref())?)
            } else {
                data
            },
        })
    }
}

impl From<Packet> for Bytes {
    /// Encodes a `Packet` into an `u8` byte stream.
    fn from(packet: Packet) -> Self {
        let mut result = BytesMut::with_capacity(packet.data.len() + 1);
        result.put(String::from(packet.ptype).as_bytes());
        if packet.ptype == PacketType::MessageBinary {
            result.extend(encode(packet.data).into_bytes());
        } else {
            result.put(packet.data);
        }
        result.freeze()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Payload(Vec<Packet>);

impl Payload {
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl TryFrom<Bytes> for Payload {
    type Error = Error;
    /// Decodes a `payload` which in the `engine.io` context means a chain of normal
    /// packets separated by a certain SEPARATOR, in this case the delimiter `\x30`.
    fn try_from(payload: Bytes) -> Result<Self> {
        let mut vec = Vec::new();
        let mut last_index = 0;

        for i in 0..payload.len() {
            if *payload.get(i).unwrap() as char == SEPARATOR {
                vec.push(Packet::try_from(payload.slice(last_index..i))?);
                last_index = i + 1;
            }
        }
        // push the last packet as well
        vec.push(Packet::try_from(payload.slice(last_index..payload.len()))?);

        Ok(Payload(vec))
    }
}

impl TryFrom<Payload> for Bytes {
    type Error = Error;
    /// Encodes a payload. Payload in the `engine.io` context means a chain of
    /// normal `packets` separated by a SEPARATOR, in this case the delimiter
    /// `\x30`.
    fn try_from(packets: Payload) -> Result<Self> {
        let mut buf = BytesMut::new();
        for packet in packets {
            // at the moment no base64 encoding is used
            buf.extend(Bytes::from(packet.clone()));
            buf.put_u8(SEPARATOR as u8);
        }

        // remove the last separator
        let _ = buf.split_off(buf.len() - 1);
        Ok(buf.freeze())
    }
}

#[derive(Clone, Debug)]
pub struct IntoIter {
    iter: std::vec::IntoIter<Packet>,
}

impl Iterator for IntoIter {
    type Item = Packet;
    fn next(&mut self) -> std::option::Option<<Self as std::iter::Iterator>::Item> {
        self.iter.next()
    }
}

impl IntoIterator for Payload {
    type Item = Packet;
    type IntoIter = IntoIter;
    fn into_iter(self) -> <Self as std::iter::IntoIterator>::IntoIter {
        IntoIter {
            iter: self.0.into_iter(),
        }
    }
}

impl Index<usize> for Payload {
    type Output = Packet;
    fn index(&self, index: usize) -> &Packet {
        &self.0[index]
    }
}

#[cfg(feature = "server")]
pub(crate) fn build_polling_payload(mut byte_vec: VecDeque<Bytes>) -> Option<String> {
    let mut payload = String::new();
    while let Some(bytes) = byte_vec.pop_front() {
        if *bytes.first()? == b'b' {
            payload.push_str(&encode(bytes));
        } else if let Ok(s) = from_utf8(&bytes) {
            payload.push_str(s);
        }

        if !byte_vec.is_empty() {
            payload.push(SEPARATOR);
        }
    }
    if payload.is_empty() {
        None
    } else {
        Some(payload)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn test_packet_error() {
        let err = Packet::try_from(BytesMut::with_capacity(10).freeze());
        assert!(err.is_err())
    }

    #[test]
    fn test_is_reflexive() {
        let data = Bytes::from_static(b"1Hello World");
        let packet = Packet::try_from(data).unwrap();

        assert_eq!(packet.ptype, PacketType::Close);
        assert_eq!(packet.data, Bytes::from_static(b"Hello World"));

        let data = Bytes::from_static(b"1Hello World");
        assert_eq!(Bytes::from(packet), data);
    }

    #[test]
    fn test_binary_packet() {
        // SGVsbG8= is the encoded string for 'Hello'
        let data = Bytes::from_static(b"bSGVsbG8=");
        let packet = Packet::try_from(data.clone()).unwrap();

        assert_eq!(packet.ptype, PacketType::MessageBinary);
        assert_eq!(packet.data, Bytes::from_static(b"Hello"));

        assert_eq!(Bytes::from(packet), data);
    }

    #[test]
    fn test_decode_payload() -> Result<()> {
        let data = Bytes::from_static(b"1Hello\x1e1HelloWorld");
        let packets = Payload::try_from(data)?;

        assert_eq!(packets[0].ptype, PacketType::Close);
        assert_eq!(packets[0].data, Bytes::from_static(b"Hello"));
        assert_eq!(packets[1].ptype, PacketType::Close);
        assert_eq!(packets[1].data, Bytes::from_static(b"HelloWorld"));

        let data = "1Hello\x1e1HelloWorld".to_owned().into_bytes();
        assert_eq!(Bytes::try_from(packets).unwrap(), data);

        Ok(())
    }

    #[test]
    fn test_binary_payload() {
        let data = Bytes::from_static(b"bSGVsbG8=\x1ebSGVsbG9Xb3JsZA==\x1ebSGVsbG8=");
        let packets = Payload::try_from(data.clone()).unwrap();

        assert!(packets.len() == 3);
        assert_eq!(packets[0].ptype, PacketType::MessageBinary);
        assert_eq!(packets[0].data, Bytes::from_static(b"Hello"));
        assert_eq!(packets[1].ptype, PacketType::MessageBinary);
        assert_eq!(packets[1].data, Bytes::from_static(b"HelloWorld"));
        assert_eq!(packets[2].ptype, PacketType::MessageBinary);
        assert_eq!(packets[2].data, Bytes::from_static(b"Hello"));

        assert_eq!(Bytes::try_from(packets).unwrap(), data);
    }

    #[test]
    fn test_packet_type_conversion_and_incompl_packet() {
        let sut = Packet::try_from(Bytes::from_static(b"4"));
        assert!(sut.is_err());
        let _sut = sut.unwrap_err();
        assert!(matches!(Error::IncompletePacket, _sut));

        let sut = PacketType::try_from(b'0');
        assert!(sut.is_ok());
        assert_eq!(sut.unwrap(), PacketType::Open);

        let sut = PacketType::try_from(b'1');
        assert!(sut.is_ok());
        assert_eq!(sut.unwrap(), PacketType::Close);

        let sut = PacketType::try_from(b'2');
        assert!(sut.is_ok());
        assert_eq!(sut.unwrap(), PacketType::Ping);

        let sut = PacketType::try_from(b'3');
        assert!(sut.is_ok());
        assert_eq!(sut.unwrap(), PacketType::Pong);

        let sut = PacketType::try_from(b'4');
        assert!(sut.is_ok());
        assert_eq!(sut.unwrap(), PacketType::Message);

        let sut = PacketType::try_from(b'5');
        assert!(sut.is_ok());
        assert_eq!(sut.unwrap(), PacketType::Upgrade);

        let sut = PacketType::try_from(b'6');
        assert!(sut.is_ok());
        assert_eq!(sut.unwrap(), PacketType::Noop);

        let sut = PacketType::try_from(42);
        assert!(sut.is_err());
        assert!(matches!(sut.unwrap_err(), Error::InvalidPacketType(42)));
    }

    #[test]
    fn test_handshake_packet() {
        assert!(
            HandshakePacket::try_from(Packet::new(PacketType::Message, Bytes::from("test")))
                .is_err()
        );
        let packet = HandshakePacket {
            ping_interval: 10000,
            ping_timeout: 1000,
            max_payload: 1000,
            sid: Arc::new("Test".to_owned()),
            upgrades: vec!["websocket".to_owned(), "test".to_owned()],
        };
        let encoded: String = serde_json::to_string(&packet).unwrap();

        assert_eq!(
            packet,
            HandshakePacket::try_from(Packet::new(PacketType::Message, Bytes::from(encoded)))
                .unwrap()
        );
    }

    #[test]
    fn test_build_polling_payload() {
        let byte_vec = VecDeque::new();
        let payload = build_polling_payload(byte_vec);
        assert!(payload.is_none());

        let data = Bytes::from_static(b"Hello\x1eHelloWorld\x1eYkhlbGxv");

        let mut byte_vec = VecDeque::new();
        byte_vec.push_back(Bytes::from_static(b"Hello"));
        byte_vec.push_back(Bytes::from_static(b"HelloWorld"));
        byte_vec.push_back(Bytes::from_static(b"bHello"));
        let payload = build_polling_payload(byte_vec);

        assert!(payload.is_some());
        let payload = payload.unwrap();
        assert_eq!(payload, data);
    }
}
