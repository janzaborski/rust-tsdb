use std::fs::{File, OpenOptions, read_dir};
use std::io::{self, BufReader, Read};
use std::path::Path;

use super::codec::{Codec, RecordHeader, record_crc};
use super::error::{Corruption, WalError};
use super::record::{RecordKind, WalRecord};
use super::segment::{Segment, SegmentInfo};

use super::MAX_RECORD_BYTES;

/// Sequential reader for WAL segments.
pub struct WalReader {
    segments: Vec<SegmentInfo>,
    cursor: usize,
    current: Option<BufReader<File>>,
    current_segment_id: u64,
    offset: u64,
}

/// Result of reading one WAL record.
pub enum ReadRecord {
    Record {
        segment_id: u64,
        offset: u64,
        kind: RecordKind,
        payload: Vec<u8>,
    },
    Eof,
    Truncated {
        segment_id: u64,
        offset: u64,
    },
}

impl WalReader {
    pub fn open(dir: &Path) -> Result<Self, WalError> {
        let entries = match read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    segments: Vec::new(),
                    cursor: 0,
                    current: None,
                    current_segment_id: 0,
                    offset: 0,
                });
            }
            Err(e) => return Err(e.into()),
        };

        let mut segments = Vec::new();

        for dir_entry_res in entries {
            let dir_entry = dir_entry_res?;
            let path = dir_entry.path();

            if let Some(id) = Segment::parse_id(&path) {
                segments.push(SegmentInfo { id, path });
            }
        }

        segments.sort_by_key(|segment| segment.id);

        Ok(Self {
            segments,
            cursor: 0,
            current: None,
            current_segment_id: 0,
            offset: 0,
        })
    }

    pub fn next_record(&mut self) -> Result<ReadRecord, WalError> {
        loop {
            if self.current.is_none() {
                if self.cursor >= self.segments.len() {
                    return Ok(ReadRecord::Eof);
                }

                self.open_current()?;
            }

            let reader = self.current.as_mut().expect("segment just opened");
            let record_offset = self.offset;

            let mut header_buf = [0u8; RecordHeader::ENCODED_LEN];

            let header_read = Self::read_full(reader, &mut header_buf)?;
            self.offset += header_read as u64;

            match header_read {
                0 => {
                    self.current = None;
                    continue;
                }

                n if n < RecordHeader::ENCODED_LEN => {
                    return Ok(ReadRecord::Truncated {
                        segment_id: self.current_segment_id,
                        offset: record_offset,
                    });
                }

                _ => {}
            }

            let header =
                RecordHeader::decode(header_buf).ok_or_else(|| WalError::CorruptRecord {
                    segment_id: self.current_segment_id,
                    offset: record_offset,
                    reason: Corruption::InvalidKind(header_buf[8]),
                })?;

            if header.len > MAX_RECORD_BYTES {
                return Err(WalError::CorruptRecord {
                    segment_id: self.current_segment_id,
                    offset: record_offset,
                    reason: Corruption::InvalidLength(header.len),
                });
            }

            let mut payload = vec![0u8; header.len as usize];

            let payload_read = Self::read_full(reader, &mut payload)?;
            self.offset += payload_read as u64;

            if payload_read < payload.len() {
                return Ok(ReadRecord::Truncated {
                    segment_id: self.current_segment_id,
                    offset: record_offset,
                });
            }

            if record_crc(header.kind, &payload) != header.crc {
                return Err(WalError::CorruptRecord {
                    segment_id: self.current_segment_id,
                    offset: record_offset,
                    reason: Corruption::ChecksumMismatch,
                });
            }

            return Ok(ReadRecord::Record {
                segment_id: self.current_segment_id,
                offset: record_offset,
                kind: header.kind,
                payload,
            });
        }
    }
    pub fn recover(&mut self) -> Result<Vec<WalRecord>, WalError> {
        let last_segment_id = self.segments.last().map(|s| s.id);
        let mut records = Vec::new();

        loop {
            match self.next_record()? {
                ReadRecord::Record {
                    segment_id,
                    offset,
                    kind,
                    payload,
                } => {
                    let record =
                        Codec::decode(kind, &payload).map_err(|err| WalError::CorruptRecord {
                            segment_id,
                            offset,
                            reason: Corruption::InvalidPayload(err),
                        })?;

                    records.push(record);
                }

                ReadRecord::Eof => return Ok(records),

                ReadRecord::Truncated { segment_id, offset } => {
                    if Some(segment_id) != last_segment_id {
                        return Err(WalError::TruncatedRecord { segment_id, offset });
                    }

                    self.current = None;
                    self.truncate_segment(segment_id, offset)?;

                    return Ok(records);
                }
            }
        }
    }
    fn open_current(&mut self) -> Result<(), WalError> {
        let info = &self.segments[self.cursor];

        self.current = Some(BufReader::new(File::open(&info.path)?));
        self.current_segment_id = info.id;
        self.offset = 0;
        self.cursor += 1;

        Ok(())
    }
    fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            match r.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(n)
    }
    fn truncate_segment(&self, segment_id: u64, offset: u64) -> Result<(), WalError> {
        let segment = self
            .segments
            .iter()
            .find(|s| s.id == segment_id)
            .expect("segment came from this reader");

        OpenOptions::new()
            .write(true)
            .open(&segment.path)?
            .set_len(offset)?;

        Ok(())
    }
    pub fn next_segment_id(&self) -> u64 {
        self.segments.last().map(|s| s.id + 1).unwrap_or(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs::File;
    use std::io::Write;

    use tempfile::tempdir;

    use crate::model::{Label, LabelSet, Sample, SeriesId};

    use super::super::codec::{Codec, RecordHeader};
    use super::super::record::{RecordKind, WalRecord};
    use super::super::segment::Segment;

    fn labels() -> LabelSet {
        LabelSet::from_labels([Label::new("__name__", "cpu"), Label::new("host", "a")])
    }

    #[test]
    fn recover_reads_valid_records() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(Segment::file_name(1));

        let mut codec = Codec::new();
        let labels = labels();

        let series_payload = codec.encode_series(SeriesId(1), &labels).to_vec();

        let samples_payload = codec
            .encode_samples(SeriesId(1), &[Sample::new(100, 1.0)])
            .to_vec();

        {
            let mut file = File::create(&path).unwrap();

            RecordHeader::for_record(RecordKind::Series, &series_payload)
                .write_to(&mut file)
                .unwrap();

            file.write_all(&series_payload).unwrap();

            RecordHeader::for_record(RecordKind::Samples, &samples_payload)
                .write_to(&mut file)
                .unwrap();

            file.write_all(&samples_payload).unwrap();
        }

        let mut reader = WalReader::open(dir.path()).unwrap();
        let records = reader.recover().unwrap();

        assert_eq!(records.len(), 2);

        assert!(matches!(records[0], WalRecord::Series(_)));
        assert!(matches!(records[1], WalRecord::Samples(_)));
    }

    #[test]
    fn recover_truncates_partial_final_record() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(Segment::file_name(1));

        let mut codec = Codec::new();

        let valid_payload = codec.encode_series(SeriesId(1), &labels()).to_vec();

        let partial_payload = codec
            .encode_samples(SeriesId(1), &[Sample::new(100, 1.0)])
            .to_vec();

        let valid_len = (RecordHeader::ENCODED_LEN + valid_payload.len()) as u64;

        {
            let mut file = File::create(&path).unwrap();

            RecordHeader::for_record(RecordKind::Series, &valid_payload)
                .write_to(&mut file)
                .unwrap();

            file.write_all(&valid_payload).unwrap();

            RecordHeader::for_record(RecordKind::Samples, &partial_payload)
                .write_to(&mut file)
                .unwrap();

            // Simulate a crash during payload write.
            file.write_all(&partial_payload[..1]).unwrap();
        }

        let mut reader = WalReader::open(dir.path()).unwrap();
        let records = reader.recover().unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), valid_len);
    }

    #[test]
    fn recover_rejects_truncated_non_final_segment() {
        let dir = tempdir().unwrap();

        let first = dir.path().join(Segment::file_name(1));
        let second = dir.path().join(Segment::file_name(2));

        let mut codec = Codec::new();

        let payload = codec
            .encode_samples(SeriesId(1), &[Sample::new(100, 1.0)])
            .to_vec();

        {
            let mut file = File::create(&first).unwrap();

            RecordHeader::for_record(RecordKind::Samples, &payload)
                .write_to(&mut file)
                .unwrap();

            // Segment 1 ends with an incomplete record.
            file.write_all(&payload[..1]).unwrap();
        }

        {
            let mut file = File::create(&second).unwrap();

            let series_payload = codec.encode_series(SeriesId(2), &labels()).to_vec();

            RecordHeader::for_record(RecordKind::Series, &series_payload)
                .write_to(&mut file)
                .unwrap();

            file.write_all(&series_payload).unwrap();
        }

        let mut reader = WalReader::open(dir.path()).unwrap();
        let err = reader.recover().unwrap_err();

        assert!(matches!(
            err,
            WalError::TruncatedRecord { segment_id: 1, .. }
        ));
    }
}
