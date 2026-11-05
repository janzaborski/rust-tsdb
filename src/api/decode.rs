use crate::model::{LabelSet, Sample};
use thiserror::Error;

pub const MAX_COMPRESSED: usize = 16 << 20;
pub const MAX_DECOMPRESSED: usize = 32 << 20;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("snappy: {0}")]
    Snappy(String),
    #[error("decompressed body exceeds {MAX_DECOMPRESSED} bytes")]
    TooLarge,
    #[error("truncated message")]
    Truncated,
    #[error("malformed varint")]
    Varint,
    #[error("unexpected wire type {1} for field {0}")]
    WireType(u64, u8),
    #[error("invalid UTF-8 in label")]
    Utf8,
    #[error("empty label name")]
    EmptyLabelName,
    #[error("duplicate label name {0:?}")]
    DuplicateLabel(String),
    #[error("negative timestamp {0}")]
    NegativeTimestamp(i64),
}

type Result<T> = std::result::Result<T, DecodeError>;

pub fn decompress(body: &[u8]) -> Result<Vec<u8>> {
    let len = snap::raw::decompress_len(body).map_err(|e| DecodeError::Snappy(e.to_string()))?;

    if len > MAX_DECOMPRESSED {
        return Err(DecodeError::TooLarge);
    }

    let mut out = vec![0; len];
    let n = snap::raw::Decoder::new()
        .decompress(body, &mut out)
        .map_err(|e| DecodeError::Snappy(e.to_string()))?;

    out.truncate(n);

    Ok(out)
}

pub fn decode_write_request(buf: &[u8]) -> Result<Vec<(LabelSet, Vec<Sample>)>> {
    let mut out = Vec::new();
    let mut labels = Vec::new();
    let mut samples = Vec::new();
    let mut r = Pb::new(buf);
    while let Some((field, wt)) = r.key()? {
        match (field, wt) {
            (1, LEN) => {
                if let Some(series) = decode_series(r.bytes()?, &mut labels, &mut samples)? {
                    out.push(series);
                }
            }
            (1, _) => return Err(DecodeError::WireType(field, wt)),
            _ => r.skip(field, wt)?,
        }
    }
    Ok(out)
}

fn decode_series<'a>(
    buf: &'a [u8],
    labels: &mut Vec<(&'a str, &'a str)>,
    samples: &mut Vec<&'a [u8]>,
) -> Result<Option<(LabelSet, Vec<Sample>)>> {
    labels.clear();
    samples.clear();
    let mut r = Pb::new(buf);

    while let Some((field, wt)) = r.key()? {
        match (field, wt) {
            (1, LEN) => {
                let (name, value) = decode_label(r.bytes()?)?;
                if name.is_empty() {
                    return Err(DecodeError::EmptyLabelName);
                }
                if !value.is_empty() {
                    labels.push((name, value));
                }
            }
            (2, LEN) => samples.push(r.bytes()?),
            (1 | 2, _) => return Err(DecodeError::WireType(field, wt)),
            _ => r.skip(field, wt)?,
        }
    }
    if samples.is_empty() {
        return Ok(None);
    }

    if !labels.is_sorted_by(|a, b| a.0 <= b.0) {
        labels.sort_unstable_by(|a, b| a.0.cmp(b.0));
    }

    if let Some(w) = labels.windows(2).find(|w| w[0].0 == w[1].0) {
        return Err(DecodeError::DuplicateLabel(w[0].0.to_owned()));
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in samples.iter() {
        out.push(decode_sample(s)?);
    }

    Ok(Some((LabelSet::from_sorted_pairs(labels), out)))
}

fn decode_label(buf: &[u8]) -> Result<(&str, &str)> {
    let (mut name, mut value): (&[u8], &[u8]) = (b"", b"");
    let mut r = Pb::new(buf);

    while let Some((field, wt)) = r.key()? {
        match (field, wt) {
            (1, LEN) => name = r.bytes()?,
            (2, LEN) => value = r.bytes()?,
            (1 | 2, _) => return Err(DecodeError::WireType(field, wt)),
            _ => r.skip(field, wt)?,
        }
    }

    Ok((utf8(name)?, utf8(value)?))
}

fn decode_sample(buf: &[u8]) -> Result<Sample> {
    let (mut value, mut timestamp) = (0.0, 0i64);
    let mut r = Pb::new(buf);

    while let Some((field, wt)) = r.key()? {
        match (field, wt) {
            (1, I64) => value = f64::from_bits(r.fixed64()?),
            (2, VARINT) => timestamp = r.varint()? as i64,
            (1 | 2, _) => return Err(DecodeError::WireType(field, wt)),
            _ => r.skip(field, wt)?,
        }
    }

    if timestamp < 0 {
        return Err(DecodeError::NegativeTimestamp(timestamp));
    }

    Ok(Sample::new(timestamp as u64, value))
}

fn utf8(b: &[u8]) -> Result<&str> {
    std::str::from_utf8(b).map_err(|_| DecodeError::Utf8)
}

const VARINT: u8 = 0;
const I64: u8 = 1;
const LEN: u8 = 2;
const I32: u8 = 5;

struct Pb<'a> {
    buf: &'a [u8],
}

impl<'a> Pb<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    fn key(&mut self) -> Result<Option<(u64, u8)>> {
        if self.buf.is_empty() {
            return Ok(None);
        }

        let key = self.varint()?;
        Ok(Some((key >> 3, (key & 7) as u8)))
    }

    #[inline]
    fn varint(&mut self) -> Result<u64> {
        let mut value = 0u64;

        for (i, &b) in self.buf.iter().take(10).enumerate() {
            value |= u64::from(b & 0x7f) << (7 * i);

            if b < 0x80 {
                if i == 9 && b > 1 {
                    return Err(DecodeError::Varint);
                }

                self.buf = &self.buf[i + 1..];
                return Ok(value);
            }
        }

        Err(if self.buf.len() < 10 {
            DecodeError::Truncated
        } else {
            DecodeError::Varint
        })
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.buf.len() {
            return Err(DecodeError::Truncated);
        }

        let (head, rest) = self.buf.split_at(n);

        self.buf = rest;

        Ok(head)
    }

    fn fixed64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.varint()?;
        self.take(usize::try_from(n).map_err(|_| DecodeError::Truncated)?)
    }

    fn skip(&mut self, field: u64, wt: u8) -> Result<()> {
        match wt {
            VARINT => self.varint().map(drop),
            I64 => self.take(8).map(drop),
            LEN => self.bytes().map(drop),
            I32 => self.take(4).map(drop),
            _ => Err(DecodeError::WireType(field, wt)), // groups (3, 4) are not supported
        }
    }
}
