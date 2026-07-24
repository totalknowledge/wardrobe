use std::io::{Error, ErrorKind};

pub(super) fn write_varint(val: u64, buf: &mut Vec<u8>) {
    let mut temp = val;
    loop {
        let mut byte = (temp & 0x7F) as u8;
        temp >>= 7;
        if temp != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if temp == 0 {
            break;
        }
    }
}

pub(super) fn read_varint(bytes: &[u8], offset: &mut usize) -> std::io::Result<u64> {
    let mut result = 0u64;
    let mut shift = 0;
    loop {
        if *offset >= bytes.len() {
            return Err(Error::new(ErrorKind::UnexpectedEof, "EOF reading varint"));
        }
        let byte = bytes[*offset];
        *offset += 1;
        result |= ((byte & 0x7F) as u64) << shift;
        if (byte & 0x80) == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            return Err(Error::new(ErrorKind::InvalidData, "Varint overflow"));
        }
    }
    Ok(result)
}
