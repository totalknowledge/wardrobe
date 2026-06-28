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
        let payload_len = u32::try_from(self.payload.len()).map_err(|_| {
            Error::new(
                ErrorKind::InvalidInput,
                "Wardrobe protocol payload exceeds u32 length limit",
            )
        })?;

        let mut header = [0u8; HEADER_LENGTH];
        header[0..2].copy_from_slice(&PROTOCOL_MAGIC);
        header[2] = self.opcode.as_u8();
        header[3..7].copy_from_slice(&payload_len.to_be_bytes());

        let mut frame = Vec::with_capacity(HEADER_LENGTH + self.payload.len());
        frame.extend_from_slice(&header);
        frame.extend_from_slice(&self.payload);
        stream.write_all(&frame)?;
        stream.flush()
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
