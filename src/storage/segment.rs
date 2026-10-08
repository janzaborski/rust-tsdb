use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{LabelSet, Sample, SeriesId, TimeRange};
use crate::storage::{Index, MemTable, StorageError};

const MAGIC: &[u8; 8] = b"MiNItsDB";

#[derive(Serialize, Deserialize)]
struct Segment {
    series: Vec<DiskSeries>,
}

#[derive(Serialize, Deserialize)]
struct DiskSeries {
    labels: LabelSet,
    samples: Vec<(u64, u64)>,
}

struct SegmentMeta {
    path: PathBuf,
    ranges: HashMap<SeriesId, (u64, u64)>,
}

pub struct SegmentStore {
    dir: PathBuf,
    _lock: File,
    segments: Vec<SegmentMeta>,
    next_number: u64,
}

impl SegmentStore {
    pub fn open(
        dir: &Path,
        index: &mut Index,
    ) -> Result<(Self, HashMap<SeriesId, u64>), StorageError> {
        fs::create_dir_all(dir)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("LOCK"))?;
        lock.try_lock().map_err(std::io::Error::other)?;

        let mut files = Vec::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if let Some(raw_number) = name
                .strip_prefix("segment-")
                .and_then(|name| name.strip_suffix(".seg"))
            {
                let number = raw_number
                    .parse::<u64>()
                    .map_err(|_| StorageError::CorruptSegment(path.display().to_string()))?;
                if name != format!("segment-{number:020}.seg") {
                    return Err(StorageError::CorruptSegment(path.display().to_string()));
                }
                files.push((number, path));
            }
        }
        files.sort_by_key(|(number, _)| *number);

        let mut segments = Vec::new();
        let mut latest = HashMap::new();
        for (_, path) in &files {
            let segment = read_segment(path)?;
            let ranges = index_segment(&segment, index, &mut latest, path)?;
            segments.push(SegmentMeta {
                path: path.clone(),
                ranges,
            });
        }

        let next_number = files
            .last()
            .map_or(Some(0), |(number, _)| number.checked_add(1))
            .ok_or_else(|| StorageError::CorruptSegment("segment number overflow".into()))?;
        Ok((
            Self {
                dir: dir.to_path_buf(),
                _lock: lock,
                segments,
                next_number,
            },
            latest,
        ))
    }

    pub fn flush(&mut self, table: &MemTable, index: &Index) -> Result<(), StorageError> {
        if table.data.is_empty() {
            return Ok(());
        }
        let mut entries: Vec<_> = table.data.iter().collect();
        entries.sort_by_key(|(id, _)| id.0);
        let segment = Segment {
            series: entries
                .into_iter()
                .map(|(id, samples)| DiskSeries {
                    labels: index.labels_for(*id).expect("indexed series"),
                    samples: samples
                        .iter()
                        .map(|sample| (sample.timestamp, sample.value.to_bits()))
                        .collect(),
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&segment)?;
        let number = self.next_number;
        let base = format!("segment-{number:020}");
        let temporary = self.dir.join(format!("{base}.tmp"));
        let final_path = self.dir.join(format!("{base}.seg"));
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)?;
        file.write_all(MAGIC)?;
        file.write_all(&(bytes.len() as u64).to_le_bytes())?;
        file.write_all(&crc32fast::hash(&bytes).to_le_bytes())?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &final_path)?;
        File::open(&self.dir)?.sync_all()?;

        let ranges = segment
            .series
            .iter()
            .map(|series| {
                let id = index.id_for(&series.labels).expect("indexed series");
                let first = series.samples.first().expect("nonempty series").0;
                let last = series.samples.last().expect("nonempty series").0;
                (id, (first, last))
            })
            .collect();
        self.segments.push(SegmentMeta {
            path: final_path,
            ranges,
        });
        self.next_number = number
            .checked_add(1)
            .ok_or_else(|| StorageError::CorruptSegment("segment number overflow".into()))?;
        Ok(())
    }

    pub fn read(
        &self,
        ids: &[SeriesId],
        labels: &HashMap<LabelSet, SeriesId>,
        range: TimeRange,
    ) -> Result<HashMap<SeriesId, Vec<Sample>>, StorageError> {
        let requested: HashSet<_> = ids.iter().copied().collect();
        let mut result: HashMap<SeriesId, Vec<Sample>> = HashMap::new();
        for meta in &self.segments {
            if !meta.ranges.iter().any(|(id, (start, end))| {
                requested.contains(id) && *start <= range.end && *end >= range.start
            }) {
                continue;
            }
            for series in read_segment(&meta.path)?.series {
                if let Some(id) = labels.get(&series.labels)
                    && requested.contains(id)
                {
                    result.entry(*id).or_default().extend(
                        series
                            .samples
                            .into_iter()
                            .filter(|(timestamp, _)| {
                                *timestamp >= range.start && *timestamp <= range.end
                            })
                            .map(|(timestamp, bits)| Sample::new(timestamp, f64::from_bits(bits))),
                    );
                }
            }
        }
        Ok(result)
    }
}

fn read_segment(path: &Path) -> Result<Segment, StorageError> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    if bytes.len() < 20 || &bytes[..8] != MAGIC {
        return Err(StorageError::CorruptSegment(path.display().to_string()));
    }
    let len = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    let checksum = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
    let payload = &bytes[20..];
    if len != payload.len() as u64 || checksum != crc32fast::hash(payload) {
        return Err(StorageError::CorruptSegment(path.display().to_string()));
    }
    serde_json::from_slice(payload)
        .map_err(|_| StorageError::CorruptSegment(path.display().to_string()))
}

fn index_segment(
    segment: &Segment,
    index: &mut Index,
    latest: &mut HashMap<SeriesId, u64>,
    path: &Path,
) -> Result<HashMap<SeriesId, (u64, u64)>, StorageError> {
    let mut ranges = HashMap::new();
    for series in &segment.series {
        let Some(&(first, _)) = series.samples.first() else {
            return Err(StorageError::CorruptSegment(path.display().to_string()));
        };
        if series.samples.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return Err(StorageError::CorruptSegment(path.display().to_string()));
        }
        let id = index.encode(&series.labels);
        let last = series.samples.last().unwrap().0;
        if latest.get(&id).is_some_and(|previous| first <= *previous)
            || ranges.insert(id, (first, last)).is_some()
        {
            return Err(StorageError::CorruptSegment(path.display().to_string()));
        }
        latest.insert(id, last);
    }
    Ok(ranges)
}
