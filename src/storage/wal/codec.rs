use crate::model::{Label, LabelSet, Sample, SeriesId};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use crc32c::{crc32c, crc32c_append};
use std::io::{self, Write};
use thiserror::Error;

use super::record::{RecordKind, SamplesRecord, SeriesRecord, WalRecord};

use super::SAMPLE_ENCODED_LEN;

/// Header preceding every WAL record.
pub struct RecordHeader {
    pub len: u32,
    pub crc: u32,
    pub kind: RecordKind,
}

impl RecordHeader {
    pub const ENCODED_LEN: usize = size_of::<u32>() + size_of::<u32>() + 1;

    pub fn for_record(kind: RecordKind, payload: &[u8]) -> Self {
        Self {
            len: payload.len() as u32,
            crc: record_crc(kind, payload),
            kind,
        }
    }

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_u32::<LittleEndian>(self.len)?;
        w.write_u32::<LittleEndian>(self.crc)?;
        w.write_u8(self.kind as u8)?;

        Ok(())
    }

    pub fn decode(bytes: [u8; Self::ENCODED_LEN]) -> Option<Self> {
        Some(Self {
            len: (&bytes[0..4]).read_u32::<LittleEndian>().ok()?,
            crc: (&bytes[4..8]).read_u32::<LittleEndian>().ok()?,
            kind: RecordKind::from_repr(bytes[8])?,
        })
    }
}

pub fn record_crc(kind: RecordKind, payload: &[u8]) -> u32 {
    crc32c_append(
        crc32c_append(crc32c(&[kind as u8]), &payload.len().to_le_bytes()),
        payload,
    )
}

#[derive(Error, Debug)]
pub enum CodecError {
    #[error("Unexpected end of input")]
    UnexpectedEof,
    #[error("Invalid utf-8 in label")]
    BadUtf8,
    #[error("Trailing bytes after record")]
    TrailingBytes,
}

/// Encoder and decoder for WAL record payloads.
pub struct Codec {
    scratch: Vec<u8>,
}

impl Codec {
    pub fn new() -> Self {
        Self {
            scratch: Vec::new(),
        }
    }
    pub fn encode_series(&mut self, id: SeriesId, labels: &LabelSet) -> &[u8] {
        self.scratch.clear();
        self.scratch.extend_from_slice(&id.0.to_le_bytes());
        Self::encode_labelset(labels, &mut self.scratch);
        &self.scratch
    }
    pub fn encode_samples(&mut self, id: SeriesId, samples: &[Sample]) -> &[u8] {
        self.scratch.clear();
        self.scratch.extend_from_slice(&id.0.to_le_bytes());
        self.scratch
            .extend_from_slice(&(samples.len() as u32).to_le_bytes());
        for sample in samples {
            Self::encode_sample(*sample, &mut self.scratch);
        }
        &self.scratch
    }
    fn encode_labelset(labels: &LabelSet, out: &mut Vec<u8>) {
        out.extend_from_slice(&(labels.len() as u32).to_le_bytes());
        for label in labels {
            Self::encode_bytes(label.name.as_bytes(), out);
            Self::encode_bytes(label.value.as_bytes(), out);
        }
    }
    fn encode_sample(sample: Sample, out: &mut Vec<u8>) {
        out.extend_from_slice(&sample.timestamp.to_le_bytes());
        out.extend_from_slice(&sample.value.to_le_bytes());
    }
    fn encode_bytes(bytes: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    }

    pub fn decode(kind: RecordKind, bytes: &[u8]) -> Result<WalRecord, CodecError> {
        match kind {
            RecordKind::Series => Ok(WalRecord::Series(Self::decode_series(bytes)?)),
            RecordKind::Samples => Ok(WalRecord::Samples(Self::decode_samples(bytes)?)),
        }
    }
    fn decode_series(bytes: &[u8]) -> Result<SeriesRecord, CodecError> {
        let mut reader = Reader::new(bytes);
        let id = reader.read_u64()?;
        let label_set = Self::decode_labelset(&mut reader)?;

        if !reader.is_empty() {
            return Err(CodecError::TrailingBytes);
        }

        Ok(SeriesRecord {
            id: SeriesId(id),
            labels: label_set,
        })
    }
    fn decode_samples(bytes: &[u8]) -> Result<SamplesRecord, CodecError> {
        let mut reader = Reader::new(bytes);
        let id = reader.read_u64()?;
        let sample_count = reader.read_u32()?;
        if sample_count as usize > reader.remaining() / SAMPLE_ENCODED_LEN {
            return Err(CodecError::UnexpectedEof);
        }
        let mut samples: Vec<Sample> = Vec::with_capacity(sample_count as usize);
        for _ in 0..sample_count {
            samples.push(Self::decode_sample(&mut reader)?);
        }

        if !reader.is_empty() {
            return Err(CodecError::TrailingBytes);
        }

        Ok(SamplesRecord {
            id: SeriesId(id),
            samples,
        })
    }
    fn decode_labelset(r: &mut Reader) -> Result<LabelSet, CodecError> {
        let label_count = r.read_u32()?;
        if label_count as usize > r.remaining() / (2 * size_of::<u32>()) {
            return Err(CodecError::UnexpectedEof);
        }
        let mut labels: Vec<Label> = Vec::with_capacity(label_count as usize);
        for _ in 0..label_count {
            let name = r.read_str()?;
            let value = r.read_str()?;
            labels.push(Label::new(name, value));
        }

        Ok(LabelSet::from_labels(labels))
    }
    fn decode_sample(r: &mut Reader) -> Result<Sample, CodecError> {
        Ok(Sample {
            timestamp: r.read_u64()?,
            value: r.read_f64()?,
        })
    }
}

impl Default for Codec {
    fn default() -> Self {
        Self::new()
    }
}
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        let end = self.pos.checked_add(n).ok_or(CodecError::UnexpectedEof)?;
        let slice = self
            .buf
            .get(self.pos..end)
            .ok_or(CodecError::UnexpectedEof)?;
        self.pos = end;
        Ok(slice)
    }

    fn read_u32(&mut self) -> Result<u32, CodecError> {
        let bytes = self.take(size_of::<u32>())?;
        Ok(u32::from_le_bytes(
            bytes.try_into().expect("take returned 4 bytes"),
        ))
    }

    fn read_u64(&mut self) -> Result<u64, CodecError> {
        let bytes = self.take(size_of::<u64>())?;
        Ok(u64::from_le_bytes(
            bytes.try_into().expect("take returned 8 bytes"),
        ))
    }

    fn read_f64(&mut self) -> Result<f64, CodecError> {
        let bytes = self.take(size_of::<f64>())?;
        Ok(f64::from_le_bytes(
            bytes.try_into().expect("take returned 8 bytes"),
        ))
    }

    fn read_bytes(&mut self) -> Result<&'a [u8], CodecError> {
        let len = self.read_u32()? as usize;
        self.take(len)
    }

    fn read_str(&mut self) -> Result<&'a str, CodecError> {
        let bytes = self.read_bytes()?;
        std::str::from_utf8(bytes).map_err(|_| CodecError::BadUtf8)
    }

    fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> LabelSet {
        LabelSet::from_labels([Label::new("__name__", "cpu"), Label::new("host", "a")])
    }

    #[test]
    fn series_roundtrip() {
        let mut codec = Codec::new();
        let labels = labels();

        let bytes = codec.encode_series(SeriesId(1), &labels).to_vec();
        let record = Codec::decode(RecordKind::Series, &bytes).unwrap();

        match record {
            WalRecord::Series(record) => {
                assert_eq!(record.id, SeriesId(1));
                assert_eq!(record.labels, labels);
            }
            _ => panic!("expected series record"),
        }
    }

    #[test]
    fn samples_roundtrip() {
        let mut codec = Codec::new();

        let samples = vec![Sample::new(100, 1.0), Sample::new(200, 2.0)];

        let bytes = codec.encode_samples(SeriesId(1), &samples).to_vec();

        let record = Codec::decode(RecordKind::Samples, &bytes).unwrap();

        match record {
            WalRecord::Samples(record) => {
                assert_eq!(record.id, SeriesId(1));
                assert_eq!(record.samples, samples);
            }
            _ => panic!("expected samples record"),
        }
    }

    #[test]
    fn header_roundtrip() {
        let payload = b"hello";
        let header = RecordHeader::for_record(RecordKind::Samples, payload);

        let mut bytes = Vec::new();
        header.write_to(&mut bytes).unwrap();

        let decoded = RecordHeader::decode(bytes.try_into().unwrap()).unwrap();

        assert_eq!(decoded.len, payload.len() as u32);
        assert_eq!(decoded.kind, RecordKind::Samples);
        assert_eq!(decoded.crc, header.crc);
    }

    #[test]
    fn decode_rejects_trailing_bytes() {
        let mut codec = Codec::new();
        let mut bytes = codec.encode_series(SeriesId(1), &labels()).to_vec();

        bytes.push(0);

        assert!(matches!(
            Codec::decode(RecordKind::Series, &bytes),
            Err(CodecError::TrailingBytes)
        ));
    }
}
