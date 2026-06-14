use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalVerification {
    pub path: String,
    pub entry_count: usize,
    pub last_sequence: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct WalJournal {
    path: PathBuf,
}

impl WalJournal {
    pub fn at_database_path(database_path: impl AsRef<Path>) -> Self {
        Self {
            path: database_path.as_ref().join(WAL_FILE_NAME),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, operation: WalOperation, scope: &str, payload: &[u8]) -> Result<WalEntry> {
        let sequence = self.next_sequence()?;
        let entry = WalEntry {
            sequence,
            operation,
            scope: scope.to_string(),
            payload: payload.to_vec(),
        };

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(&entry.to_bytes()?)?;
        file.flush()?;
        file.sync_all()?;
        Ok(entry)
    }

    pub fn read_entries(&self) -> Result<Vec<WalEntry>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let mut file = File::open(&self.path)?;
        let mut entries = Vec::new();
        loop {
            match WalEntry::read_from(&mut file) {
                Ok(Some(entry)) => entries.push(entry),
                Ok(None) => return Ok(entries),
                Err(error) => return Err(error),
            }
        }
    }

    pub fn verify(&self) -> Result<WalVerification> {
        let entries = self.read_entries()?;
        Ok(WalVerification {
            path: self.path.to_string_lossy().into_owned(),
            entry_count: entries.len(),
            last_sequence: entries.last().map(|entry| entry.sequence),
        })
    }

    fn next_sequence(&self) -> Result<u64> {
        Ok(self
            .read_entries()?
            .last()
            .map(|entry| entry.sequence + 1)
            .unwrap_or(1))
    }
}

impl WalEntry {
    fn to_bytes(&self) -> Result<Vec<u8>> {
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

    fn read_from(reader: &mut impl Read) -> Result<Option<Self>> {
        let mut magic = [0_u8; 4];
        match reader.read_exact(&mut magic) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error),
        }

        if magic != WAL_MAGIC {
            return Err(Error::new(ErrorKind::InvalidData, "WAL magic header is corrupt"));
        }

        let mut version = [0_u8; 1];
        reader.read_exact(&mut version)?;
        if version[0] != WAL_VERSION {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Unsupported WAL version: {}", version[0]),
            ));
        }

        let mut sequence = [0_u8; 8];
        reader.read_exact(&mut sequence)?;

        let mut operation = [0_u8; 1];
        reader.read_exact(&mut operation)?;

        let mut scope_len = [0_u8; 2];
        reader.read_exact(&mut scope_len)?;
        let scope_len = u16::from_be_bytes(scope_len) as usize;

        let mut payload_len = [0_u8; 4];
        reader.read_exact(&mut payload_len)?;
        let payload_len = u32::from_be_bytes(payload_len) as usize;

        let mut scope = vec![0_u8; scope_len];
        reader.read_exact(&mut scope)?;
        let scope = String::from_utf8(scope).map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("WAL scope is not valid UTF-8: {error}"),
            )
        })?;

        let mut payload = vec![0_u8; payload_len];
        reader.read_exact(&mut payload)?;

        Ok(Some(Self {
            sequence: u64::from_be_bytes(sequence),
            operation: WalOperation::from_code(operation[0])?,
            scope,
            payload,
        }))
    }
}
