use super::error::WalError;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentInfo {
    pub id: u64,
    pub path: PathBuf,
}

pub fn file_name(id: u64) -> String {
    format!("wal-{id:08}.log")
}

pub fn parse_id(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    name.strip_prefix("wal-")?
        .strip_suffix(".log")?
        .parse()
        .ok()
}

pub fn list(dir: &Path) -> Result<Vec<SegmentInfo>, WalError> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };

    let mut out = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if let Some(id) = parse_id(&path) {
            out.push(SegmentInfo { id, path });
        }
    }

    out.sort_by_key(|s| s.id);

    Ok(out)
}

pub fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

pub struct ActiveSegment {
    pub info: SegmentInfo,
    file: Arc<File>,
    written: u64,
}

impl ActiveSegment {
    pub fn create(dir: &Path, id: u64) -> io::Result<Self> {
        let path = dir.join(file_name(id));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        sync_dir(dir)?;
        Ok(Self {
            info: SegmentInfo { id, path },
            file: Arc::new(file),
            written: 0,
        })
    }

    pub fn written(&self) -> u64 {
        self.written
    }

    pub fn file(&self) -> Arc<File> {
        Arc::clone(&self.file)
    }

    pub fn write(&mut self, buf: &[u8], writeback: bool) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }

        (&*self.file).write_all(buf)?;

        if writeback {
            start_writeback(&self.file, self.written, buf.len() as u64)?;
        }

        self.written += buf.len() as u64;

        Ok(())
    }

    pub fn sync(&self) -> io::Result<()> {
        self.file().sync_data()
    }
}

#[cfg(target_os = "linux")]
fn start_writeback(file: &File, offset: u64, len: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let rc = unsafe {
        libc::sync_file_range(
            file.as_raw_fd(),
            offset as libc::off64_t,
            len as libc::off64_t,
            libc::SYNC_FILE_RANGE_WRITE,
        )
    };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn start_writeback(_file: &File, _offset: u64, _len: u64) -> io::Result<()> {
    Ok(())
}
