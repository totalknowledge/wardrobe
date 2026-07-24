use super::*;

pub const WAL_FILE_NAME: &str = ".wal";
const WAL_MAGIC: [u8; 4] = [0x57, 0x44, 0x57, 0x4c];
const WAL_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WalOperation {
    Upsert,
    Delete,
    Maintenance,
    Define,
}

impl WalOperation {
    fn code(self) -> u8 {
        match self {
            Self::Upsert => 1,
            Self::Delete => 2,
            Self::Maintenance => 3,
            Self::Define => 4,
        }
    }

    fn from_code(code: u8) -> Result<Self> {
        match code {
            1 => Ok(Self::Upsert),
            2 => Ok(Self::Delete),
            3 => Ok(Self::Maintenance),
            4 => Ok(Self::Define),
            _ => Err(Error::new(
                ErrorKind::InvalidData,
                format!("Unknown WAL operation code: {code}"),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalEntry {
    pub sequence: u64,
    pub operation: WalOperation,
    pub scope: String,
    pub payload: Vec<u8>,
}

impl WalEntry {
    pub(super) fn to_bytes(&self) -> Result<Vec<u8>> {
        let scope = self.scope.as_bytes();
        if scope.len() > u16::MAX as usize {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "WAL scope is too large to encode",
            ));
        }
        if self.payload.len() > u32::MAX as usize {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "WAL payload is too large to encode",
            ));
        }

        let mut bytes = Vec::with_capacity(20 + scope.len() + self.payload.len());
        bytes.extend_from_slice(&WAL_MAGIC);
        bytes.push(WAL_VERSION);
        bytes.extend_from_slice(&self.sequence.to_be_bytes());
        bytes.push(self.operation.code());
        bytes.extend_from_slice(&(scope.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(scope);
        bytes.extend_from_slice(&self.payload);
        Ok(bytes)
    }

    pub(super) fn read_from(reader: &mut impl Read) -> Result<Option<Self>> {
        let mut magic = [0_u8; 4];
        if read_exact_or_none(reader, &mut magic)?.is_none() {
            return Ok(None);
        }

        if magic != WAL_MAGIC {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "WAL magic header is corrupt",
            ));
        }

        let mut version = [0_u8; 1];
        if read_exact_or_none(reader, &mut version)?.is_none() {
            return Ok(None);
        }
        if version[0] != WAL_VERSION {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Unsupported WAL version: {}", version[0]),
            ));
        }

        let mut sequence = [0_u8; 8];
        if read_exact_or_none(reader, &mut sequence)?.is_none() {
            return Ok(None);
        }

        let mut operation = [0_u8; 1];
        if read_exact_or_none(reader, &mut operation)?.is_none() {
            return Ok(None);
        }

        let mut scope_len = [0_u8; 2];
        if read_exact_or_none(reader, &mut scope_len)?.is_none() {
            return Ok(None);
        }
        let scope_len = u16::from_be_bytes(scope_len) as usize;

        let mut payload_len = [0_u8; 4];
        if read_exact_or_none(reader, &mut payload_len)?.is_none() {
            return Ok(None);
        }
        let payload_len = u32::from_be_bytes(payload_len) as usize;

        let mut scope = vec![0_u8; scope_len];
        if read_exact_or_none(reader, &mut scope)?.is_none() {
            return Ok(None);
        }
        let scope = String::from_utf8(scope).map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("WAL scope is not valid UTF-8: {error}"),
            )
        })?;

        let mut payload = vec![0_u8; payload_len];
        if read_exact_or_none(reader, &mut payload)?.is_none() {
            return Ok(None);
        }

        Ok(Some(Self {
            sequence: u64::from_be_bytes(sequence),
            operation: WalOperation::from_code(operation[0])?,
            scope,
            payload,
        }))
    }
}

pub(super) fn read_entries_from_path(path: &Path) -> Result<Vec<WalEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let mut file = File::open(path)?;
    let mut entries = Vec::new();
    loop {
        match WalEntry::read_from(&mut file) {
            Ok(Some(entry)) => entries.push(entry),
            Ok(None) => return Ok(entries),
            Err(error) => return Err(error),
        }
    }
}

fn read_exact_or_none(reader: &mut impl Read, buffer: &mut [u8]) -> Result<Option<()>> {
    match reader.read_exact(buffer) {
        Ok(()) => Ok(Some(())),
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(error),
    }
}
