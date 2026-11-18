//! Positional file IO, so many chunks can be written into one file from
//! several threads without sharing a cursor.

use std::fs::File;
use std::io;

#[cfg(unix)]
pub(crate) fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
}

#[cfg(windows)]
pub(crate) fn write_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_write(buf, offset) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(n) => {
                buf = &buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Fills `buf` from `offset`. Returns how much was read: less than
/// `buf.len()` only at the end of the file.
#[cfg(unix)]
pub(crate) fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    let mut done = 0;
    while done < buf.len() {
        match file.read_at(&mut buf[done..], offset + done as u64) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(done)
}

#[cfg(windows)]
pub(crate) fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    let mut done = 0;
    while done < buf.len() {
        match file.seek_read(&mut buf[done..], offset + done as u64) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            // Windows reports reading past the end as an error.
            Err(e) if e.raw_os_error() == Some(38) => break,
            Err(e) => return Err(e),
        }
    }
    Ok(done)
}

/// Writes `bytes` to `path` so a crash leaves either the old file or the
/// new one: a sibling temp file, flushed, then renamed over.
pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f");
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        f.set_len(10).unwrap();
        write_at(&f, b"abc", 7).unwrap();
        write_at(&f, b"xy", 0).unwrap();
        let mut buf = [0u8; 4];
        assert_eq!(read_at(&f, &mut buf, 7).unwrap(), 3);
        assert_eq!(&buf[..3], b"abc");
        assert_eq!(read_at(&f, &mut buf, 0).unwrap(), 4);
        assert_eq!(&buf, b"xy\0\0");
    }
}
