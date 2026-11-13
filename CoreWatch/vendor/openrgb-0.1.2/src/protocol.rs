use async_trait::async_trait;
use log::debug;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use OpenRGBError::*;

use crate::data::{OpenRGBReadable, OpenRGBWritable, PacketId};
use crate::OpenRGBError;

static MAGIC: [u8; 4] = *b"ORGB";

#[async_trait]
pub trait OpenRGBReadableStream: AsyncReadExt + Sized + Send + Sync + Unpin {
    async fn read_value<T: OpenRGBReadable>(&mut self, protocol: u32) -> Result<T, OpenRGBError> {
        T::read(self, protocol).await
    }

    async fn read_header(&mut self, protocol: u32, expected_device_id: u32, expected_packet_id: PacketId) -> Result<usize, OpenRGBError> {
        debug!("Reading {:?} packet...", expected_packet_id);

        // Notifications may arrive between a command and its response.
        for _ in 0..32 {
            let mut magic = [0u8; 4];
            self.read_exact(&mut magic).await?;
            if magic != MAGIC {
                return Err(ProtocolError(format!("expected OpenRGB header ORGB, received bytes {:02X?}; reconnect required", magic)));
            }
            let device_id = self.read_value::<u32>(protocol).await?;
            let packet_id = self.read_value::<u32>(protocol).await?;
            let data_len = self.read_value::<u32>(protocol).await? as usize;
            if data_len > 8 * 1024 * 1024 {
                return Err(ProtocolError(format!("OpenRGB packet exceeds 8 MiB limit: {}", data_len)));
            }
            if packet_id == PacketId::DeviceListUpdated as u32 && packet_id != expected_packet_id as u32 {
                let mut notification = vec![0; data_len];
                self.read_exact(&mut notification).await?;
                continue;
            }
            if device_id != expected_device_id || packet_id != expected_packet_id as u32 {
                return Err(ProtocolError(format!("expected device {} packet {:?}, received device {} packet {}; reconnect required", expected_device_id, expected_packet_id, device_id, packet_id)));
            }
            return Ok(data_len);
        }
        Err(ProtocolError("Too many OpenRGB notifications before response".into()))
    }

    async fn read_packet<O: OpenRGBReadable>(&mut self, protocol: u32, expected_device_id: u32, expected_packet_id: PacketId) -> Result<O, OpenRGBError> {
        let len = self.read_header(protocol, expected_device_id, expected_packet_id).await?;
        // Consume one complete wire frame before decoding. Older parsers may not
        // know extension fields; they must never leave these bytes on the socket.
        let mut payload = vec![0; len];
        self.read_exact(&mut payload).await?;
        let mut cursor = std::io::Cursor::new(payload);
        let value = cursor.read_value(protocol).await?;
        if cursor.position() != len as u64 {
            debug!("OpenRGB response has {} trailing extension byte(s)", len as u64 - cursor.position());
        }
        Ok(value)
    }

}

#[async_trait]
pub trait OpenRGBWritableStream: AsyncWriteExt + Sized + Send + Sync + Unpin {
    async fn write_value<T: OpenRGBWritable>(&mut self, value: T, protocol: u32) -> Result<(), OpenRGBError> {
        T::write(value, self, protocol).await
    }

    async fn write_header(&mut self, protocol: u32, device_id: u32, packet_id: PacketId, data_len: usize) -> Result<(), OpenRGBError> {
        debug!("Sending {:?} packet of {} bytes...", packet_id, data_len);
        self.write_all(&MAGIC).await?;
        self.write_value(device_id, protocol).await?;
        self.write_value(packet_id, protocol).await?;
        self.write_value(data_len, protocol).await?;
        Ok(())
    }

    async fn write_packet<I: OpenRGBWritable>(&mut self, protocol: u32, device_id: u32, packet_id: PacketId, data: I) -> Result<(), OpenRGBError> {
        let size = data.size(protocol);

        // in debug builds, use intermediate buffer to ease debugging with Wireshark (see #3)
        #[cfg(debug_assertions)] {
            let mut buf: Vec<u8> = Vec::with_capacity(4 /* magic */ + 4 /* device id */ + 4 /* packet id */ + 4 /* len */ + size /* payload size*/);
            buf.write_header(protocol, device_id, packet_id, size).await?;
            buf.write_value(data, protocol).await?;
            self.write_all(&buf).await?;
        }

        // in release builds, write directly
        #[cfg(not(debug_assertions))] {
            self.write_header(protocol, device_id, packet_id, size).await?;
            self.write_value(data, protocol).await?;
        }

        Ok(())
    }
}

#[async_trait]
pub trait OpenRGBStream: OpenRGBReadableStream + OpenRGBWritableStream {
    async fn request<I: OpenRGBWritable, O: OpenRGBReadable>(&mut self, protocol: u32, device_id: u32, packet_id: PacketId, data: I) -> Result<O, OpenRGBError> {
        self.write_packet(protocol, device_id, packet_id, data).await?;
        self.read_packet(protocol, device_id, packet_id).await
    }
}

impl OpenRGBReadableStream for TcpStream {}

impl OpenRGBWritableStream for TcpStream {}

impl OpenRGBStream for TcpStream {}

#[cfg(debug_assertions)]
impl OpenRGBWritableStream for Vec<u8> {}

impl OpenRGBReadableStream for std::io::Cursor<Vec<u8>> {}

#[cfg(test)]
mod framing_tests {
    use super::*;
    use tokio_test::io::Builder;

    fn frame(packet: u32, data: &[u8]) -> Vec<u8> {
        let mut out = b"ORGB".to_vec();
        for value in [0, packet, data.len() as u32] { out.extend_from_slice(&value.to_le_bytes()); }
        out.extend_from_slice(data); out
    }

    #[tokio::test]
    async fn framing_consumes_extensions_before_next_reply() {
        let mut bytes = frame(0, &[4,0,0,0, 0xAA,0xBB,0xCC]);
        bytes.extend(frame(0, &[7,0,0,0]));
        let mut stream = Builder::new().read(&bytes).build();
        assert_eq!(stream.read_packet::<u32>(3,0,PacketId::RequestControllerCount).await.unwrap(),4);
        assert_eq!(stream.read_packet::<u32>(3,0,PacketId::RequestControllerCount).await.unwrap(),7);
    }

    #[tokio::test]
    async fn framing_handles_notifications_and_fragmentation() {
        let mut bytes = frame(100, &[1,2,3]);
        bytes.extend(frame(0, &[4,0,0,0]));
        let mut builder = Builder::new();
        for chunk in bytes.chunks(3) { builder.read(chunk); }
        let mut stream=builder.build();
        assert_eq!(stream.read_packet::<u32>(3,0,PacketId::RequestControllerCount).await.unwrap(),4);
    }

    #[tokio::test]
    async fn framing_truncated_payload_cannot_consume_next_packet() {
        let mut bytes=frame(0,&[1,2]);
        bytes.extend(frame(0,&[8,0,0,0]));
        let mut stream=Builder::new().read(&bytes).build();
        assert!(stream.read_packet::<u32>(3,0,PacketId::RequestControllerCount).await.is_err());
        assert_eq!(stream.read_packet::<u32>(3,0,PacketId::RequestControllerCount).await.unwrap(),8);
    }

    #[tokio::test]
    async fn framing_reports_actual_invalid_header() {
        let mut stream=Builder::new().read(b"NOPE").build();
        let error=stream.read_packet::<u32>(3,0,PacketId::RequestControllerCount).await.unwrap_err().to_string();
        assert!(error.contains("4E, 4F, 50, 45"), "{error}");
    }
}
