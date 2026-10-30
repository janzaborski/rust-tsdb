use crate::model::{LabelSet, Sample, SeriesId};

pub const MAX_RECORD_BYTES: u32 = 64 * 1024 * 1024;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    Write = 0,
}

impl RecordKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Write),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordHeader {
    pub len: u32,
    pub crc: u32,
    pub kind: u8,
}

impl RecordHeader {
    pub const ENCODED_LEN: usize = 4 + 4 + 1;

    pub fn for_payload(kind: RecordKind, payload: &[u8]) -> Self {
        let len = payload.len() as u32;
        Self {
            len,
            crc: record_crc(kind as u8, len, payload),
            kind: kind as u8,
        }
    }

    pub fn encode(&self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[0..4].copy_from_slice(&self.len.to_le_bytes());
        out[4..8].copy_from_slice(&self.crc.to_le_bytes());
        out[8] = self.kind;
        out
    }

    pub fn decode(bytes: &[u8; Self::ENCODED_LEN]) -> Self {
        Self {
            len: u32::from_le_bytes(bytes[0..4].try_into().expect("4 bytes")),
            crc: u32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes")),
            kind: bytes[8],
        }
    }
}

pub fn record_crc(kind: u8, len: u32, payload: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(&[kind]);
    h.update(&len.to_le_bytes());
    h.update(payload);
    h.finalize()
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesEntry {
    pub id: SeriesId,
    pub labels: LabelSet,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SamplesEntry {
    pub id: SeriesId,
    pub samples: Vec<Sample>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WalRecord {
    pub series: Vec<SeriesEntry>,
    pub samples: Vec<SamplesEntry>,
}
