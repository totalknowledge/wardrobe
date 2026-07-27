use std::io::{Error, ErrorKind, Read, Result, Write};

pub const PROTOCOL_MAGIC: [u8; 2] = [0x57, 0x44];
const HEADER_LENGTH: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolOpcode {
    Command,
    Result,
    Error,
}

impl ProtocolOpcode {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Command => 0x01,
            Self::Result => 0x02,
            Self::Error => 0x03,
        }
    }

    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(Self::Command),
            0x02 => Ok(Self::Result),
            0x03 => Ok(Self::Error),
            _ => Err(Error::new(
                ErrorKind::InvalidData,
                format!("Invalid Wardrobe protocol opcode: {value:#04x}"),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolFrame {
    pub opcode: ProtocolOpcode,
    pub payload: Vec<u8>,
}

impl ProtocolFrame {
    pub fn new(opcode: ProtocolOpcode, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            opcode,
            payload: payload.into(),
        }
    }

    pub fn write_to_stream<W: Write>(&self, stream: &mut W) -> Result<()> {
        Self::write_payload_to_stream(self.opcode, &self.payload, stream)
    }

    pub fn write_to_stream_unflushed<W: Write>(&self, stream: &mut W) -> Result<()> {
        Self::write_payload_to_stream_unflushed(self.opcode, &self.payload, stream)
    }

    pub fn write_payload_to_stream<W: Write>(
        opcode: ProtocolOpcode,
        payload: &[u8],
        stream: &mut W,
    ) -> Result<()> {
        Self::write_payload_to_stream_unflushed(opcode, payload, stream)?;
        stream.flush()
    }

    pub fn write_payload_to_stream_unflushed<W: Write>(
        opcode: ProtocolOpcode,
        payload: &[u8],
        stream: &mut W,
    ) -> Result<()> {
        let payload_len = u32::try_from(payload.len()).map_err(|_| {
            Error::new(
                ErrorKind::InvalidInput,
                "Wardrobe protocol payload exceeds u32 length limit",
            )
        })?;

        let mut header = [0u8; HEADER_LENGTH];
        header[0..2].copy_from_slice(&PROTOCOL_MAGIC);
        header[2] = opcode.as_u8();
        header[3..7].copy_from_slice(&payload_len.to_be_bytes());

        stream.write_all(&header)?;
        stream.write_all(payload)
    }

    pub fn read_from_stream<R: Read>(stream: &mut R) -> Result<Self> {
        let mut header = [0u8; HEADER_LENGTH];
        stream.read_exact(&mut header)?;

        if header[0..2] != PROTOCOL_MAGIC {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "Invalid Wardrobe protocol magic bytes",
            ));
        }

        let opcode = ProtocolOpcode::from_u8(header[2])?;
        let payload_len = u32::from_be_bytes([header[3], header[4], header[5], header[6]]) as usize;
        let mut payload = vec![0u8; payload_len];
        stream.read_exact(&mut payload)?;

        Ok(Self { opcode, payload })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protocol_opcode() {
        assert_eq!(ProtocolOpcode::Command.as_u8(), 0x01);
        assert_eq!(ProtocolOpcode::Result.as_u8(), 0x02);
        assert_eq!(ProtocolOpcode::Error.as_u8(), 0x03);

        assert_eq!(ProtocolOpcode::from_u8(0x01).unwrap(), ProtocolOpcode::Command);
        assert_eq!(ProtocolOpcode::from_u8(0x02).unwrap(), ProtocolOpcode::Result);
        assert_eq!(ProtocolOpcode::from_u8(0x03).unwrap(), ProtocolOpcode::Error);
        assert!(ProtocolOpcode::from_u8(0x99).is_err());
    }

    #[test]
    fn test_protocol_frame_roundtrip() {
        let frame = ProtocolFrame::new(ProtocolOpcode::Command, b"hello wardrobe".to_vec());
        let mut buf = Vec::new();
        frame.write_to_stream(&mut buf).unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let read = ProtocolFrame::read_from_stream(&mut cursor).unwrap();
        assert_eq!(read, frame);
    }

    #[test]
    fn test_protocol_frame_unflushed_roundtrip() {
        let frame = ProtocolFrame::new(ProtocolOpcode::Result, b"result payload".to_vec());
        let mut buf = Vec::new();
        frame.write_to_stream_unflushed(&mut buf).unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let read = ProtocolFrame::read_from_stream(&mut cursor).unwrap();
        assert_eq!(read, frame);
    }

    #[test]
    fn test_protocol_frame_invalid_magic() {
        let bad_header = vec![0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00];
        let mut cursor = std::io::Cursor::new(bad_header);
        assert!(ProtocolFrame::read_from_stream(&mut cursor).is_err());
    }
}
