use std::{fmt::Debug, pin::Pin, sync::Arc, task::ready};

use crate::{
    error::Result,
    packet::{Packet, PacketType},
    Error, Event, Payload,
};
use async_stream::try_stream;
use bytes::Bytes;
use engineio::{
    Client as EngineClient, Event as EngineEvent, Packet as EnginePacket,
    PacketType as EnginePacketType, StreamGenerator,
};
use futures_util::{FutureExt, Stream, StreamExt};
use tokio::sync::Mutex;

#[derive(Clone)]
pub(crate) struct Socket {
    engine_client: Arc<EngineClient>,
    generator: Arc<Mutex<StreamGenerator<Packet, Error>>>,
    is_server: bool,
}

impl Socket {
    /// Creates an instance of `Socket`.
    pub(super) fn client_end(engine_client: EngineClient) -> Self {
        Socket {
            engine_client: Arc::new(engine_client.clone()),
            generator: Arc::new(Mutex::new(StreamGenerator::new(Self::stream(
                engine_client,
            )))),
            is_server: false,
        }
    }

    pub(super) fn server_end(engine_client: EngineClient) -> Self {
        Socket {
            engine_client: Arc::new(engine_client.clone()),
            generator: Arc::new(Mutex::new(StreamGenerator::new(Self::stream(
                engine_client,
            )))),
            is_server: true,
        }
    }

    /// Connects to the server. This includes a connection of the underlying
    /// engine.io client and afterwards an opening socket.io request.
    pub async fn connect(&self) -> Result<()> {
        if !self.is_server {
            self.engine_client.connect().await?;
        }

        Ok(())
    }

    /// Disconnects from the server by sending a socket.io `Disconnect` packet. This results
    /// in the underlying engine.io transport to get closed as well.
    pub async fn disconnect(&self) -> Result<()> {
        if self.is_engineio_connected() {
            self.engine_client.disconnect().await?;
        }

        Ok(())
    }

    /// Sends a `socket.io` packet to the server using the `engine.io` client.
    pub async fn send(&self, packet: Packet) -> Result<()> {
        if !self.is_engineio_connected() {
            return Err(Error::IllegalActionBeforeOpen());
        }

        // the packet, encoded as an engine.io message packet
        let engine_packet = EnginePacket::new(EnginePacketType::Message, Bytes::from(&packet));
        self.engine_client.emit(engine_packet).await?;

        if let Some(attachments) = packet.attachments {
            for attachment in attachments {
                let engine_packet = EnginePacket::new(EnginePacketType::MessageBinary, attachment);
                self.engine_client.emit(engine_packet).await?;
            }
        }

        Ok(())
    }

    pub async fn ack(&self, nsp: &str, id: usize, data: Payload) -> Result<()> {
        let socket_packet = self.build_packet_for_payload(data, None, nsp, Some(id), true)?;

        self.send(socket_packet).await
    }

    /// Emits to certain event with given data. The data needs to be JSON,
    /// otherwise this returns an `InvalidJson` error.
    pub async fn emit(&self, nsp: &str, event: Event, data: Payload) -> Result<()> {
        let socket_packet = self.build_packet_for_payload(data, Some(event), nsp, None, false)?;

        self.send(socket_packet).await
    }

    pub(crate) async fn handshake(&self, nsp: &str, data: String) -> Result<()> {
        let socket_packet = Packet::new(
            PacketType::Connect,
            nsp.to_owned(),
            Some(data),
            None,
            0,
            None,
        );
        self.send(socket_packet).await
    }

    /// Returns a packet for a payload, could be used for bot binary and non binary
    /// events and acks. Convenance method.
    #[inline]
    pub(crate) fn build_packet_for_payload<'a>(
        &'a self,
        payload: Payload,
        event: Option<Event>,
        nsp: &'a str,
        id: Option<usize>,
        is_ack: bool,
    ) -> Result<Packet> {
        match payload {
            Payload::Binary(bin_data) => Ok(Packet::new(
                if id.is_some() && is_ack {
                    PacketType::BinaryAck
                } else {
                    PacketType::BinaryEvent
                },
                nsp.to_owned(),
                event.map(|event| serde_json::Value::String(event.into()).to_string()),
                id,
                1,
                Some(vec![bin_data]),
            )),
            Payload::String(str_data) => {
                serde_json::from_str::<serde_json::Value>(&str_data)?;

                let payload = match event {
                    None => format!("[{}]", str_data),
                    Some(event) => format!("[\"{}\",{}]", String::from(event), str_data),
                };

                Ok(Packet::new(
                    if id.is_some() && is_ack {
                        PacketType::Ack
                    } else {
                        PacketType::Event
                    },
                    nsp.to_owned(),
                    Some(payload),
                    id,
                    0,
                    None,
                ))
            }
        }
    }

    fn stream(client: EngineClient) -> Pin<Box<impl Stream<Item = Result<Packet>> + Send>> {
        Box::pin(try_stream! {
            for await received_data in client.clone() {
                let packet = received_data?;
                if packet.ptype == EnginePacketType::Message || packet.ptype == EnginePacketType::MessageBinary {
                    let packet = Self::handle_engineio_packet(packet, client.clone()).await?;
                    yield packet;
                }
            }
        })
    }

    /// Handles new incoming engineio packets
    async fn handle_engineio_packet(
        packet: EnginePacket,
        mut client: EngineClient,
    ) -> Result<Packet> {
        let mut socket_packet = Packet::try_from(&packet.data)?;

        // Only handle attachments if there are any
        if socket_packet.attachment_count > 0 {
            let mut attachments_left = socket_packet.attachment_count;
            let mut attachments = Vec::new();
            while attachments_left > 0 {
                // TODO: This is not nice! Find a different way to peek the next element while mapping the stream
                let next = client.next().await.unwrap();
                match next {
                    Err(err) => return Err(err.into()),
                    Ok(packet) => match packet.ptype {
                        EnginePacketType::MessageBinary | EnginePacketType::Message => {
                            attachments.push(packet.data);
                            attachments_left -= 1;
                        }
                        _ => {
                            return Err(Error::InvalidAttachmentPacketType(packet.ptype.into()));
                        }
                    },
                    _ => continue,
                }
            }
            socket_packet.attachments = Some(attachments);
        }

        Ok(socket_packet)
    }

    fn is_engineio_connected(&self) -> bool {
        self.engine_client.is_connected()
    }
}

impl Stream for Socket {
    type Item = Result<Packet>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let mut lock = ready!(Box::pin(self.generator.lock()).poll_unpin(cx));
        lock.poll_next_unpin(cx)
    }
}

impl Debug for Socket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Socket")
            .field("engine_client", &self.engine_client)
            .field("is_server", &self.is_server)
            .field("connected", &self.is_engineio_connected())
            .finish()
    }
}
