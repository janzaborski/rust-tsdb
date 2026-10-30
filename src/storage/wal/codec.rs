use super::error::CodecError;
use super::record::{RecordHeader, RecordKind, SamplesEntry, SeriesEntry, WalRecord};
use crate::model::{Label, LabelSet, Sample, SeriesId};

const SAMPLE_ENCODED_LEN: usize = size_of::<u64>() + size_of::<f64>();

pub trait LabelSetEncoding: Sized {
    fn encode(&self, out: &mut Vec<u8>);
    fn decode(r: &mut Reader) -> Result<Self, CodecError>;
}

impl LabelSetEncoding for LabelSet {
    fn encode(&self, out: &mut Vec<u8>) {
        put_u32(out, self.len() as u32);
        for (name, value) in self.into_iter() {
            put_bytes(out, name.as_bytes());
            put_bytes(out, value.as_bytes());
        }
    }

    fn decode(r: &mut Reader) -> Result<Self, CodecError> {
        let n = r.read_u32()? as usize;
        if n > r.remaining() / (size_of::<u32>() + size_of::<u32>()) {
            return Err(CodecError::UnexpectedEof);
        }

        let mut labels = Vec::with_capacity(n);
        for _ in 0..n {
            let name = r.read_str()?;
            let value = r.read_str()?;
            labels.push(Label::new(name, value));
        }

        Ok(LabelSet::from_labels(labels))
    }
}

pub struct RecordEncoder<'a> {
    out: &'a mut Vec<u8>,
    series: u32,
    samples: u32,
}

const COUNTS_LEN: usize = size_of::<u32>() + size_of::<u32>();

impl<'a> RecordEncoder<'a> {
    pub fn new(out: &'a mut Vec<u8>) -> Self {
        out.clear();
        out.extend_from_slice(&[0u8; RecordHeader::ENCODED_LEN + COUNTS_LEN]);
        Self {
            out,
            series: 0,
            samples: 0,
        }
    }

    pub fn series<L: LabelSetEncoding>(&mut self, id: SeriesId, labels: &L) {
        self.series += 1;
        put_u64(self.out, id.0);
        labels.encode(self.out);
    }

    pub fn samples(&mut self, id: SeriesId, samples: &[Sample]) {
        if samples.is_empty() {
            return;
        }

        self.samples += 1;
        put_u64(self.out, id.0);
        put_u32(self.out, samples.len() as u32);
        self.out.reserve(samples.len() * SAMPLE_ENCODED_LEN);
        for s in samples {
            put_u64(self.out, s.timestamp);
            put_f64(self.out, s.value);
        }
    }

    pub fn finish(self) -> (bool, usize) {
        let h = RecordHeader::ENCODED_LEN;
        self.out[h..h + 4].copy_from_slice(&self.series.to_le_bytes());
        self.out[h + 4..h + 8].copy_from_slice(&self.samples.to_le_bytes());
        let payload_len = self.out.len() - h;
        let header = RecordHeader::for_payload(RecordKind::Write, &self.out[h..]);
        self.out[..h].copy_from_slice(&header.encode());
        (self.series == 0 && self.samples == 0, payload_len)
    }
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_f64(out: &mut Vec<u8>, v: f64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_u32(out, b.len() as u32);
    out.extend_from_slice(b);
}

pub fn decode(kind: RecordKind, payload: &[u8]) -> Result<WalRecord, CodecError> {
    let RecordKind::Write = kind;

    let mut r = Reader::new(payload);

    let n_series = r.read_u32()? as usize;
    let n_samples = r.read_u32()? as usize;

    if n_series.saturating_add(n_samples) > r.remaining() / (8 + size_of::<u32>()) {
        return Err(CodecError::UnexpectedEof);
    }

    let mut series = Vec::with_capacity(n_series);
    for _ in 0..n_series {
        let id = SeriesId(r.read_u64()?);
        let labels = LabelSet::decode(&mut r)?;
        series.push(SeriesEntry { id, labels });
    }

    let mut samples = Vec::with_capacity(n_samples);
    for _ in 0..n_samples {
        let id = SeriesId(r.read_u64()?);
        let n = r.read_u32()? as usize;
        if n > r.remaining() / SAMPLE_ENCODED_LEN {
            return Err(CodecError::UnexpectedEof);
        }

        let mut s = Vec::with_capacity(n);
        for _ in 0..n {
            s.push(Sample {
                timestamp: r.read_u64()?,
                value: r.read_f64()?,
            })
        }
        samples.push(SamplesEntry { id, samples: s });
    }

    if !r.is_empty() {
        return Err(CodecError::TrailingBytes);
    }

    Ok(WalRecord { series, samples })
}

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        let end = self.pos.checked_add(n).ok_or(CodecError::UnexpectedEof)?;
        let s = self
            .buf
            .get(self.pos..end)
            .ok_or(CodecError::UnexpectedEof)?;
        self.pos = end;
        Ok(s)
    }

    fn read_u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_le_bytes(
            self.take(size_of::<u32>())?.try_into().expect("4 bytes"),
        ))
    }

    fn read_u64(&mut self) -> Result<u64, CodecError> {
        Ok(u64::from_le_bytes(
            self.take(size_of::<u64>())?.try_into().expect("8 bytes"),
        ))
    }

    fn read_f64(&mut self) -> Result<f64, CodecError> {
        Ok(f64::from_le_bytes(
            self.take(size_of::<f64>())?.try_into().expect("8 bytes"),
        ))
    }

    fn read_str(&mut self) -> Result<&'a str, CodecError> {
        let len = self.read_u32()? as usize;
        std::str::from_utf8(self.take(len)?).map_err(|_| CodecError::BadUtf8)
    }

    fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
}
