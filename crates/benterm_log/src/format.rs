//! The on-disk format of a log.
//!
//! All integers are little-endian.
//!
//! ```text
//! header:  "BENTERMLOG"  version: u16  rows: u16  cols: u16
//! output:  1  length: u32  bytes: [u8; length]
//! resize:  2  rows: u16  cols: u16
//! ```
//!
//! The header is followed by any number of records, up to the end of the
//! data.

use std::fmt;
use std::io::{self, Read, Write};

use benterm_core::GridSize;

const MAGIC: &[u8; 10] = b"BENTERMLOG";
const VERSION: u16 = 1;
const OUTPUT: u8 = 1;
const RESIZE: u8 = 2;

/// One event of a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// Bytes the PTY delivered and the core was fed.
    Output(Vec<u8>),
    /// The grid was resized.
    Resize(GridSize),
}

pub struct LogWriter<W> {
    inner: W,
}

impl<W: Write> LogWriter<W> {
    /// Writes the header with the grid size at the start of the log.
    pub fn new(inner: W, initial_size: GridSize) -> io::Result<Self> {
        let mut header = Vec::with_capacity(16);
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&VERSION.to_le_bytes());
        push_size(&mut header, initial_size);
        let mut inner = inner;
        inner.write_all(&header)?;
        Ok(Self { inner })
    }

    /// Writes one record with a single `write_all`, so a reader never sees
    /// half of a record unless the writer died during the call.
    ///
    /// # Panics
    ///
    /// If an output record is longer than `u32::MAX` bytes.
    pub fn write_record(&mut self, record: &Record) -> io::Result<()> {
        let mut buf;
        match record {
            Record::Output(bytes) => {
                let len = u32::try_from(bytes.len()).expect("output record over 4 GiB");
                buf = Vec::with_capacity(5 + bytes.len());
                buf.push(OUTPUT);
                buf.extend_from_slice(&len.to_le_bytes());
                buf.extend_from_slice(bytes);
            }
            Record::Resize(size) => {
                buf = Vec::with_capacity(5);
                buf.push(RESIZE);
                push_size(&mut buf, *size);
            }
        }
        self.inner.write_all(&buf)
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

/// Reads a log from `R`. The records come out of the iterator in the order
/// they were written.
pub struct LogReader<R> {
    inner: R,
    initial_size: GridSize,
}

impl<R: Read> LogReader<R> {
    /// Reads the header.
    pub fn new(inner: R) -> Result<Self, ReadError> {
        let mut inner = inner;
        let mut magic = [0; 10];
        read_exact(&mut inner, &mut magic)?;
        if &magic != MAGIC {
            return Err(ReadError::NotALog);
        }
        let version = read_u16(&mut inner)?;
        if version != VERSION {
            return Err(ReadError::UnsupportedVersion(version));
        }
        let initial_size = read_size(&mut inner)?;
        Ok(Self {
            inner,
            initial_size,
        })
    }

    fn read_record(&mut self, tag: u8) -> Result<Record, ReadError> {
        match tag {
            OUTPUT => {
                let len = u64::from(read_u32(&mut self.inner)?);
                // The length is not trusted for allocation: a damaged log
                // could claim 4 GiB.
                let mut bytes = Vec::new();
                (&mut self.inner)
                    .take(len)
                    .read_to_end(&mut bytes)
                    .map_err(ReadError::Io)?;
                if bytes.len() as u64 != len {
                    return Err(ReadError::Truncated);
                }
                Ok(Record::Output(bytes))
            }
            RESIZE => Ok(Record::Resize(read_size(&mut self.inner)?)),
            tag => Err(ReadError::UnknownRecord(tag)),
        }
    }

    pub fn initial_size(&self) -> GridSize {
        self.initial_size
    }
}

impl<R: Read> Iterator for LogReader<R> {
    type Item = Result<Record, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut tag = [0; 1];
        loop {
            match self.inner.read(&mut tag) {
                Ok(0) => return None,
                Ok(_) => return Some(self.read_record(tag[0])),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Some(Err(ReadError::Io(error))),
            }
        }
    }
}

fn push_size(buf: &mut Vec<u8>, size: GridSize) {
    buf.extend_from_slice(&size.rows.to_le_bytes());
    buf.extend_from_slice(&size.cols.to_le_bytes());
}

fn read_exact(reader: &mut impl Read, buf: &mut [u8]) -> Result<(), ReadError> {
    reader.read_exact(buf).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            ReadError::Truncated
        } else {
            ReadError::Io(error)
        }
    })
}

fn read_u16(reader: &mut impl Read) -> Result<u16, ReadError> {
    let mut buf = [0; 2];
    read_exact(reader, &mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

fn read_u32(reader: &mut impl Read) -> Result<u32, ReadError> {
    let mut buf = [0; 4];
    read_exact(reader, &mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_size(reader: &mut impl Read) -> Result<GridSize, ReadError> {
    let rows = read_u16(reader)?;
    let cols = read_u16(reader)?;
    Ok(GridSize::new(rows, cols))
}

/// Why a log could not be read.
#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    /// The data does not start with the log's magic bytes.
    NotALog,
    UnsupportedVersion(u16),
    UnknownRecord(u8),
    /// The data ends in the middle of the header or a record.
    Truncated,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "cannot read the log: {error}"),
            Self::NotALog => f.write_str("not a benterm log"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported log version {version}")
            }
            Self::UnknownRecord(tag) => write!(f, "unknown record type {tag}"),
            Self::Truncated => f.write_str("the log ends in the middle of a record"),
        }
    }
}

impl std::error::Error for ReadError {}
