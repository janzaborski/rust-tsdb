use super::codec;
use super::error::{Corruption, WalError};
use super::record::{MAX_RECORD_BYTES, RecordHeader, RecordKind, record_crc};
use super::replay::{ReplaySink, ResolveStats, Resolver};
use super::segment;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Read};
use std::path::Path;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    pub segments: usize,
    pub last_segment: Option<u64>,
    pub records: u64,
    pub truncated: Option<(u64, u64, u64)>,
    pub series: ResolveStats,
}

pub(crate) fn replay(dir: &Path, sink: &mut impl ReplaySink) -> Result<RecoveryReport, WalError> {
    let segments = segment::list(dir)?;
    let mut report = RecoveryReport {
        segments: segments.len(),
        last_segment: segments.last().map(|s| s.id),
        ..Default::default()
    };
    let mut resolver = Resolver::default();

    for seg in &segments {
        let mut r = BufReader::with_capacity(1 << 20, File::open(&seg.path)?);
        let mut offset = 0u64;

        loop {
            let corrupt = |reason| WalError::Corrupt {
                segment_id: seg.id,
                offset,
                reason,
            };

            match read_record(&mut r)? {
                Ok(None) => break,
                Ok(Some((kind, payload))) => {
                    let rec = codec::decode(kind, &payload).map_err(|e| corrupt(e.into()))?;
                    offset += (RecordHeader::ENCODED_LEN + payload.len()) as u64;
                    report.records += 1;
                    resolver.apply(rec, sink);
                }
                Err(_) if Some(seg.id) == report.last_segment => {
                    drop(r);
                    let dropped = truncate(&seg.path, offset)?;
                    report.truncated = Some((seg.id, offset, dropped));
                    break;
                }
                Err(reason) => return Err(corrupt(reason)),
            }
        }
    }

    report.series = resolver.finish();

    Ok(report)
}

pub(crate) type ReadResult = Result<Option<(RecordKind, Vec<u8>)>, Corruption>;

pub(crate) fn read_record(r: &mut impl Read) -> io::Result<ReadResult> {
    let mut hdr = [0u8; RecordHeader::ENCODED_LEN];

    match read_full(r, &mut hdr)? {
        0 => return Ok(Ok(None)),
        n if n < hdr.len() => return Ok(Err(Corruption::Truncated)),
        _ => {}
    }

    let header = RecordHeader::decode(&hdr);
    let Some(kind) = RecordKind::from_u8(header.kind) else {
        return Ok(Err(Corruption::InvalidKind(header.kind)));
    };

    if header.len > MAX_RECORD_BYTES {
        return Ok(Err(Corruption::InvalidLength(header.len)));
    }

    let mut payload = vec![0u8; header.len as usize];
    if read_full(r, &mut payload)? < payload.len() {
        return Ok(Err(Corruption::Truncated));
    }

    if record_crc(header.kind, header.len, &payload) != header.crc {
        return Ok(Err(Corruption::ChecksumMismatch));
    }

    Ok(Ok(Some((kind, payload))))
}

fn truncate(path: &Path, offset: u64) -> io::Result<u64> {
    let file = OpenOptions::new().write(true).open(path)?;
    let len = file.metadata()?.len();
    file.set_len(offset)?;
    file.sync_all()?;
    Ok(len.saturating_sub(offset))
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}
