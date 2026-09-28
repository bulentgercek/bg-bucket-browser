//! The archive behind "Download as ZIP": stored (uncompressed) members with
//! ZIP64 records throughout, written straight into a file that a later run can
//! pick up again.
//!
//! The `zip` crate reads such archives well and is still what reads one back
//! for a resume. Its writer is not used: to add to an archive it has to reopen
//! it, and a reopened archive loses the ZIP64 records of its large members, so
//! it stops opening once it passes 4 GiB. This writer never reopens. It knows
//! where each member starts, so a resume cuts after the last whole member and
//! goes on, and a member that fails is cut away before the directory is written.

use std::collections::HashSet;
use std::io::{self, Read, Seek, SeekFrom, Write};

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const ZIP64_END_SIG: u32 = 0x0606_4b50;
const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;
const END_SIG: u32 = 0x0605_4b50;

/// Version 4.5, the first with ZIP64.
const VERSION: u16 = 45;
/// Made on Unix, so the permissions below mean what they say.
const MADE_BY: u16 = (3 << 8) | VERSION;
/// Names are UTF-8.
const FLAGS: u16 = 1 << 11;
/// 1980-01-01 00:00, the format's first moment; the `zip` crate wrote the same.
const DOS_TIME: u16 = 0;
const DOS_DATE: u16 = (1 << 5) | 1;
/// A regular file, readable by all and writable by its owner.
const EXTERNAL_ATTRS: u32 = 0o100644 << 16;

const ZIP64_ID: u16 = 0x0001;
/// The object's ETag, kept in the directory entry so a resume takes a member
/// only while its object is unchanged. Extractors skip fields they do not know.
const ETAG_ID: u16 = 0x4742;
/// The longest ETag a member may carry; a real one is a few dozen bytes.
const MAX_ETAG: usize = 1024;

/// The fixed tail this writer ends an archive with: the ZIP64 end record (56
/// bytes), its locator (20) and the classic end record without a comment (22).
const TAIL: u64 = 98;

/// Where the archive goes: a file, or in tests a store that can hold gigabytes
/// of zeros without the disk.
pub trait Sink: Read + Write + Seek {
    fn set_len(&mut self, len: u64) -> io::Result<()>;
}

impl Sink for std::fs::File {
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        std::fs::File::set_len(self, len)
    }
}

/// One whole member.
struct Entry {
    name: String,
    etag: String,
    crc: u32,
    size: u64,
    /// Where its local header starts.
    offset: u64,
}

/// The member being written.
struct Open {
    entry: Entry,
    crc: crc32fast::Hasher,
}

pub struct ZipOut<S: Sink> {
    /// Taken by `finish`.
    sink: Option<S>,
    entries: Vec<Entry>,
    /// The names in `entries`, so a name is checked in constant time.
    names: HashSet<String>,
    /// The end of the last whole member: where the next member, or the
    /// directory, goes.
    end: u64,
    open: Option<Open>,
}

impl<S: Sink> ZipOut<S> {
    /// Starts an empty archive at the beginning of `sink`.
    pub fn create(sink: S) -> Self {
        ZipOut { sink: Some(sink), entries: Vec::new(), names: HashSet::new(), end: 0, open: None }
    }

    /// Picks up an archive this writer left, keeping the members `keep` takes
    /// by name, size and ETag. `None` when it cannot be continued: it does not
    /// open, it holds a member `keep` refuses, or another program wrote it.
    pub fn resume(mut sink: S, keep: impl Fn(&str, u64, &str) -> bool) -> Option<Self> {
        // A file that does not end the way this writer ends an archive is not
        // searched at all: on a large leftover without a directory, the search
        // for an end record could read the whole file.
        ends_like_ours(&mut sink)?;
        let mut entries = Vec::new();
        let mut end = 0;
        {
            let mut archive = zip::ZipArchive::new(&mut sink).ok()?;
            for i in 0..archive.len() {
                let file = archive.by_index_raw(i).ok()?;
                if file.compression() != zip::CompressionMethod::Stored || file.compressed_size() != file.size() {
                    return None;
                }
                let etag = etag_field(file.extra_data()?)?;
                if etag.len() > MAX_ETAG || !keep(file.name(), file.size(), &etag) {
                    return None;
                }
                end = end.max(file.data_start() + file.size());
                entries.push(Entry {
                    name: file.name().to_string(),
                    etag,
                    crc: file.crc32(),
                    size: file.size(),
                    offset: file.header_start(),
                });
            }
        }
        let names: HashSet<String> = entries.iter().map(|e| e.name.clone()).collect();
        Some(ZipOut { sink: Some(sink), entries, names, end, open: None })
    }

    /// The whole members, by name.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// Opens the next member. A name already in the archive is refused, as
    /// the `zip` crate refused it.
    pub fn start(&mut self, name: &str, etag: &str) -> io::Result<()> {
        if self.open.is_some() {
            return Err(io::Error::other("zip: a member is still open"));
        }
        if self.names.contains(name) {
            return Err(io::Error::other(format!("zip: {name} is already in the archive")));
        }
        if etag.len() > MAX_ETAG {
            return Err(io::Error::other(format!("zip: the ETag of {name} is too long")));
        }
        let name_len = u16::try_from(name.len()).map_err(|_| io::Error::other("zip: the name is too long"))?;
        let mut h = Vec::with_capacity(30 + name.len() + 20);
        put32(&mut h, LOCAL_SIG);
        put16(&mut h, VERSION);
        put16(&mut h, FLAGS);
        put16(&mut h, 0); // stored
        put16(&mut h, DOS_TIME);
        put16(&mut h, DOS_DATE);
        put32(&mut h, 0); // the checksum, filled in by `end_member`
        put32(&mut h, u32::MAX); // the sizes are in the ZIP64 field
        put32(&mut h, u32::MAX);
        put16(&mut h, name_len);
        put16(&mut h, 20);
        h.extend_from_slice(name.as_bytes());
        put16(&mut h, ZIP64_ID);
        put16(&mut h, 16);
        put64(&mut h, 0); // uncompressed and compressed size, filled in by `end_member`
        put64(&mut h, 0);
        let sink = self.sink.as_mut().ok_or_else(finished)?;
        sink.seek(SeekFrom::Start(self.end))?;
        sink.write_all(&h)?;
        let entry = Entry { name: name.to_string(), etag: etag.to_string(), crc: 0, size: 0, offset: self.end };
        self.open = Some(Open { entry, crc: crc32fast::Hasher::new() });
        Ok(())
    }

    /// Adds data to the open member.
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let open = self.open.as_mut().ok_or_else(|| io::Error::other("zip: no member is open"))?;
        self.sink.as_mut().ok_or_else(finished)?.write_all(data)?;
        open.crc.update(data);
        open.entry.size += data.len() as u64;
        Ok(())
    }

    /// Closes the open member: its header gets the checksum and the size, and
    /// it joins the archive.
    pub fn end_member(&mut self) -> io::Result<()> {
        let Open { mut entry, crc } = self.open.take().ok_or_else(|| io::Error::other("zip: no member is open"))?;
        entry.crc = crc.finalize();
        let sink = self.sink.as_mut().ok_or_else(finished)?;
        let data_end = sink.stream_position()?;
        sink.seek(SeekFrom::Start(entry.offset + 14))?;
        sink.write_all(&entry.crc.to_le_bytes())?;
        sink.seek(SeekFrom::Start(entry.offset + 30 + entry.name.len() as u64 + 4))?;
        let mut sizes = Vec::with_capacity(16);
        put64(&mut sizes, entry.size);
        put64(&mut sizes, entry.size);
        sink.write_all(&sizes)?;
        sink.seek(SeekFrom::Start(data_end))?;
        // Only now is the member part of the archive.
        self.end = data_end;
        self.names.insert(entry.name.clone());
        self.entries.push(entry);
        Ok(())
    }

    /// Writes the directory after the whole members and trims the file there.
    /// A member still open is cut away.
    pub fn finish(mut self) -> io::Result<S> {
        self.write_directory()?;
        self.sink.take().ok_or_else(finished)
    }

    fn write_directory(&mut self) -> io::Result<()> {
        self.open = None;
        let sink = self.sink.as_mut().ok_or_else(finished)?;
        let count = self.entries.len() as u64;
        let dir_start = self.end;
        let mut d = Vec::new();
        for e in &self.entries {
            let name_len = u16::try_from(e.name.len()).map_err(|_| io::Error::other("zip: the name is too long"))?;
            let etag_len = u16::try_from(e.etag.len()).map_err(|_| io::Error::other("zip: the ETag is too long"))?;
            let extra_len = u16::try_from(4 + 24 + 4 + e.etag.len())
                .map_err(|_| io::Error::other("zip: the ETag is too long"))?;
            put32(&mut d, CENTRAL_SIG);
            put16(&mut d, MADE_BY);
            put16(&mut d, VERSION);
            put16(&mut d, FLAGS);
            put16(&mut d, 0); // stored
            put16(&mut d, DOS_TIME);
            put16(&mut d, DOS_DATE);
            put32(&mut d, e.crc);
            put32(&mut d, u32::MAX); // sizes and offset are in the ZIP64 field
            put32(&mut d, u32::MAX);
            put16(&mut d, name_len);
            put16(&mut d, extra_len);
            put16(&mut d, 0); // no comment
            put16(&mut d, 0); // disk
            put16(&mut d, 0); // internal attributes
            put32(&mut d, EXTERNAL_ATTRS);
            put32(&mut d, u32::MAX);
            d.extend_from_slice(e.name.as_bytes());
            put16(&mut d, ZIP64_ID);
            put16(&mut d, 24);
            put64(&mut d, e.size);
            put64(&mut d, e.size);
            put64(&mut d, e.offset);
            put16(&mut d, ETAG_ID);
            put16(&mut d, etag_len);
            d.extend_from_slice(e.etag.as_bytes());
        }
        let dir_size = d.len() as u64;
        let zip64_end = dir_start + dir_size;
        put32(&mut d, ZIP64_END_SIG);
        put64(&mut d, 44); // the size of the rest of this record
        put16(&mut d, MADE_BY);
        put16(&mut d, VERSION);
        put32(&mut d, 0); // this disk
        put32(&mut d, 0); // the disk the directory starts on
        put64(&mut d, count);
        put64(&mut d, count);
        put64(&mut d, dir_size);
        put64(&mut d, dir_start);
        put32(&mut d, ZIP64_LOCATOR_SIG);
        put32(&mut d, 0);
        put64(&mut d, zip64_end);
        put32(&mut d, 1); // disks in all
        put32(&mut d, END_SIG);
        put16(&mut d, 0);
        put16(&mut d, 0);
        put16(&mut d, count.min(0xFFFF) as u16);
        put16(&mut d, count.min(0xFFFF) as u16);
        put32(&mut d, dir_size.min(u32::MAX as u64) as u32);
        put32(&mut d, dir_start.min(u32::MAX as u64) as u32);
        put16(&mut d, 0); // no comment
        // Whatever lies beyond the last whole member, a cut member or an old
        // directory, goes first, so the room it held is free for the directory.
        if sink.seek(SeekFrom::End(0))? > dir_start {
            sink.set_len(dir_start)?;
        }
        sink.seek(SeekFrom::Start(dir_start))?;
        sink.write_all(&d)?;
        sink.flush()
    }
}

impl<S: Sink> Drop for ZipOut<S> {
    // Leaving without `finish`, on an error or a cancel in the middle of a
    // member, still leaves a whole archive of the members written so far, for
    // a later run to resume.
    fn drop(&mut self) {
        if self.sink.is_some() {
            let _ = self.write_directory();
        }
    }
}

/// Whether the file ends as this writer ends an archive: `TAIL` bytes of end
/// records right after the directory they point to, and that directory still
/// there. A resumed run overwrites the old directory from its start, so after
/// a crash the old tail can be whole while the directory is not.
fn ends_like_ours<S: Sink>(sink: &mut S) -> Option<()> {
    let len = sink.seek(SeekFrom::End(0)).ok()?;
    let tail_start = len.checked_sub(TAIL)?;
    sink.seek(SeekFrom::Start(tail_start)).ok()?;
    let mut t = [0u8; TAIL as usize];
    sink.read_exact(&mut t).ok()?;
    let u16_at = |i: usize| u16::from_le_bytes([t[i], t[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes(t[i..i + 4].try_into().unwrap_or_default());
    let u64_at = |i: usize| u64::from_le_bytes(t[i..i + 8].try_into().unwrap_or_default());
    let (dir_size, dir_start) = (u64_at(40), u64_at(48));
    let ours = u32_at(0) == ZIP64_END_SIG
        && u32_at(56) == ZIP64_LOCATOR_SIG
        && u64_at(64) == tail_start
        && u32_at(76) == END_SIG
        && u16_at(96) == 0
        && dir_start.checked_add(dir_size) == Some(tail_start);
    if !ours {
        return None;
    }
    if u64_at(32) > 0 {
        sink.seek(SeekFrom::Start(dir_start)).ok()?;
        let mut sig = [0u8; 4];
        sink.read_exact(&mut sig).ok()?;
        (u32::from_le_bytes(sig) == CENTRAL_SIG).then_some(())
    } else {
        Some(())
    }
}

fn finished() -> io::Error {
    io::Error::other("zip: the archive is already finished")
}

/// The ETag this writer stored in a directory entry's extra data.
fn etag_field(mut extra: &[u8]) -> Option<String> {
    while extra.len() >= 4 {
        let id = u16::from_le_bytes([extra[0], extra[1]]);
        let len = u16::from_le_bytes([extra[2], extra[3]]) as usize;
        let body = extra.get(4..4 + len)?;
        if id == ETAG_ID {
            return String::from_utf8(body.to_vec()).ok();
        }
        extra = &extra[4 + len..];
    }
    None
}

fn put16(v: &mut Vec<u8>, n: u16) {
    v.extend_from_slice(&n.to_le_bytes());
}

fn put32(v: &mut Vec<u8>, n: u32) {
    v.extend_from_slice(&n.to_le_bytes());
}

fn put64(v: &mut Vec<u8>, n: u64) {
    v.extend_from_slice(&n.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::{Sink, ZipOut};
    use std::collections::HashMap;
    use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};

    impl Sink for Cursor<Vec<u8>> {
        fn set_len(&mut self, len: u64) -> io::Result<()> {
            self.get_mut().truncate(len as usize);
            Ok(())
        }
    }

    /// Every entry of an archive, in order, with its content; the reader
    /// checks each checksum on the way.
    fn entries<R: Read + Seek>(r: R) -> Vec<(String, Vec<u8>)> {
        let mut ar = zip::ZipArchive::new(r).expect("the archive opens");
        (0..ar.len())
            .map(|i| {
                let mut e = ar.by_index(i).unwrap();
                let mut buf = Vec::new();
                e.read_to_end(&mut buf).unwrap();
                (e.name().to_string(), buf)
            })
            .collect()
    }

    /// Every entry read front to back from the local headers, as a streaming
    /// reader does: it takes the sizes and the checksum from each local header,
    /// which the directory-based reader never looks at.
    fn streamed(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut r = Cursor::new(bytes);
        let mut out = Vec::new();
        while let Some(mut e) = zip::read::read_zipfile_from_stream(&mut r).expect("a local header reads") {
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).expect("the local checksum holds");
            out.push((e.name().to_string(), buf));
        }
        out
    }

    fn put(out: &mut ZipOut<Cursor<Vec<u8>>>, name: &str, data: &[u8]) {
        out.start(name, &format!("\"etag-{name}\"")).unwrap();
        out.write(data).unwrap();
        out.end_member().unwrap();
    }

    #[test]
    fn members_read_back_whole() {
        let mut out = ZipOut::create(Cursor::new(Vec::new()));
        put(&mut out, "a.txt", b"hello");
        put(&mut out, "empty", b"");
        put(&mut out, "dossier/éàñ.bin", &[7u8; 70_000]);
        let file = out.finish().unwrap();
        let got = entries(file.clone());
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], ("a.txt".into(), b"hello".to_vec()));
        assert_eq!(got[1], ("empty".into(), Vec::new()));
        assert_eq!(got[2], ("dossier/éàñ.bin".into(), vec![7u8; 70_000]));
        assert_eq!(streamed(file.get_ref()), got, "the local headers say the same");
    }

    #[test]
    fn a_long_etag_is_refused_before_anything_is_written() {
        let mut out = ZipOut::create(Cursor::new(Vec::new()));
        put(&mut out, "a.txt", b"hello");
        assert!(out.start("b.bin", &"x".repeat(2000)).is_err());
        assert_eq!(entries(out.finish().unwrap()), vec![("a.txt".to_string(), b"hello".to_vec())]);
    }

    // More members than the classic end record can count: the count lives in
    // the ZIP64 record.
    #[test]
    fn more_than_65535_members_are_all_there() {
        let mut out = ZipOut::create(Cursor::new(Vec::new()));
        for i in 0..70_000 {
            out.start(&format!("{i}"), "\"e\"").unwrap();
            out.end_member().unwrap();
        }
        let ar = zip::ZipArchive::new(out.finish().unwrap()).expect("the archive opens");
        assert_eq!(ar.len(), 70_000);
    }

    // A crash while a resumed run writes over the old directory leaves the old
    // tail whole; the directory it points to is gone, and the file is not searched.
    #[test]
    fn a_crash_over_the_old_directory_is_not_resumed() {
        let mut out = ZipOut::create(Cursor::new(Vec::new()));
        put(&mut out, "a.txt", b"hello");
        put(&mut out, "b.txt", b"world");
        let file = out.finish().unwrap();
        let mut out = ZipOut::resume(file, |_, _, _| true).expect("resumable");
        out.start("c.bin", "\"c\"").unwrap();
        out.write(&[3u8; 10]).unwrap();
        // A crash: nothing after this point runs, `Drop` included.
        let bytes = out.sink.take().unwrap().into_inner();
        std::mem::forget(out);
        assert!(super::ends_like_ours(&mut Cursor::new(bytes.clone())).is_none(), "the tail check sees it");
        assert!(ZipOut::resume(Cursor::new(bytes), |_, _, _| true).is_none());
    }

    // A leftover that does not end as this writer ends an archive is not searched.
    #[test]
    fn a_file_without_our_ending_is_not_resumed() {
        let junk: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        assert!(ZipOut::resume(Cursor::new(junk), |_, _, _| true).is_none());
        let mut out = ZipOut::create(Cursor::new(Vec::new()));
        put(&mut out, "a.txt", b"hello");
        let mut bytes = out.finish().unwrap().into_inner();
        bytes.extend_from_slice(b"trailing");
        assert!(ZipOut::resume(Cursor::new(bytes), |_, _, _| true).is_none(), "bytes after the end record");
    }

    // The same as the in-memory tests, on a real file: the cut is a `set_len`.
    #[test]
    fn a_cut_member_leaves_a_file_that_resumes() {
        let path = std::env::temp_dir().join(format!("bgzipw-{}.zip.part", std::process::id()));
        let mut out = ZipOut::create(std::fs::File::create(&path).unwrap());
        out.start("a.txt", "\"a\"").unwrap();
        out.write(b"hello").unwrap();
        out.end_member().unwrap();
        out.start("b.bin", "\"b\"").unwrap();
        out.write(&[2u8; 300_000]).unwrap();
        drop(out);
        assert!(std::fs::metadata(&path).unwrap().len() < 1000, "the cut member is gone from the file");

        let f = std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
        let mut out = ZipOut::resume(f, |_, _, _| true).expect("resumable");
        out.start("b.bin", "\"b\"").unwrap();
        out.write(&[2u8; 300_000]).unwrap();
        out.end_member().unwrap();
        drop(out.finish().unwrap());
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let got = entries(Cursor::new(bytes.clone()));
        assert_eq!(got.len(), 2);
        assert_eq!(got[1], ("b.bin".to_string(), vec![2u8; 300_000]));
        assert_eq!(streamed(&bytes), got, "the local headers say the same");
    }

    // A failed member is cut away and the rest stays readable; the file ends
    // at the directory, with nothing of the cut member left behind.
    #[test]
    fn a_member_that_fails_is_cut_away() {
        let mut out = ZipOut::create(Cursor::new(Vec::new()));
        put(&mut out, "a.txt", b"hello");
        let whole = {
            let mut probe = ZipOut::create(Cursor::new(Vec::new()));
            put(&mut probe, "a.txt", b"hello");
            probe.finish().unwrap().into_inner().len()
        };
        out.start("half.bin", "\"x\"").unwrap();
        out.write(&[1u8; 300_000]).unwrap();
        let file = out.finish().unwrap();
        assert_eq!(file.get_ref().len(), whole, "the archive is as if the member never started");
        assert_eq!(entries(file), vec![("a.txt".to_string(), b"hello".to_vec())]);
    }

    // Leaving without `finish` (an error, a cancel) still leaves a whole archive.
    #[test]
    fn dropping_the_writer_leaves_a_resumable_archive() {
        let shared = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>, u64);
        impl Read for Shared {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let v = self.0.borrow();
                let at = (self.1 as usize).min(v.len());
                let n = buf.len().min(v.len() - at);
                buf[..n].copy_from_slice(&v[at..at + n]);
                self.1 += n as u64;
                Ok(n)
            }
        }
        impl Write for Shared {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                let mut v = self.0.borrow_mut();
                let at = self.1 as usize;
                if v.len() < at + buf.len() {
                    v.resize(at + buf.len(), 0);
                }
                v[at..at + buf.len()].copy_from_slice(buf);
                self.1 += buf.len() as u64;
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl Seek for Shared {
            fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
                let len = self.0.borrow().len() as i64;
                self.1 = match to {
                    SeekFrom::Start(n) => n as i64,
                    SeekFrom::End(d) => len + d,
                    SeekFrom::Current(d) => self.1 as i64 + d,
                } as u64;
                Ok(self.1)
            }
        }
        impl Sink for Shared {
            fn set_len(&mut self, len: u64) -> io::Result<()> {
                self.0.borrow_mut().truncate(len as usize);
                Ok(())
            }
        }

        let mut out = ZipOut::create(Shared(shared.clone(), 0));
        out.start("a.txt", "\"a\"").unwrap();
        out.write(b"hello").unwrap();
        out.end_member().unwrap();
        out.start("b.bin", "\"b\"").unwrap();
        out.write(&[2u8; 5000]).unwrap();
        drop(out);

        let bytes = shared.borrow().clone();
        assert_eq!(entries(Cursor::new(bytes.clone())), vec![("a.txt".to_string(), b"hello".to_vec())]);
        let out = ZipOut::resume(Cursor::new(bytes), |_, _, _| true).expect("resumable");
        assert_eq!(out.names().collect::<Vec<_>>(), ["a.txt"]);
    }

    #[test]
    fn a_resume_goes_on_after_the_last_whole_member() {
        let mut out = ZipOut::create(Cursor::new(Vec::new()));
        put(&mut out, "a.txt", b"hello");
        put(&mut out, "b.txt", b"world");
        out.start("c.bin", "\"etag-c.bin\"").unwrap();
        out.write(&[3u8; 10_000]).unwrap();
        let file = out.finish().unwrap(); // c.bin failed halfway

        let keep_all = |_: &str, _: u64, _: &str| true;
        let mut out = ZipOut::resume(file, keep_all).expect("resumable");
        let done: Vec<&str> = out.names().collect();
        assert_eq!(done, ["a.txt", "b.txt"]);
        put(&mut out, "c.bin", &[3u8; 10_000]);
        let file = out.finish().unwrap();
        assert_eq!(streamed(file.get_ref()).len(), 3, "the local headers read front to back");
        let got = entries(file);
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a.txt", "b.txt", "c.bin"], "every member once");
        assert_eq!(got[2].1, vec![3u8; 10_000]);
    }

    #[test]
    fn a_name_goes_in_once() {
        let mut out = ZipOut::create(Cursor::new(Vec::new()));
        put(&mut out, "a.txt", b"hello");
        assert!(out.start("a.txt", "\"x\"").is_err());
    }

    #[test]
    fn an_archive_with_a_changed_member_is_not_resumed() {
        let mut out = ZipOut::create(Cursor::new(Vec::new()));
        put(&mut out, "a.txt", b"hello");
        let file = out.finish().unwrap();
        let wanted: HashMap<&str, (u64, &str)> = HashMap::from([("a.txt", (5, "\"a newer etag\""))]);
        let keep = |name: &str, size: u64, etag: &str| wanted.get(name) == Some(&(size, etag));
        assert!(ZipOut::resume(file, keep).is_none(), "the object changed, so the archive starts over");
    }

    #[test]
    fn an_archive_written_by_another_program_is_not_resumed() {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        w.start_file("a.txt", opts).unwrap();
        w.write_all(b"hello").unwrap();
        let file = w.finish().unwrap();
        assert!(ZipOut::resume(file, |_, _, _| true).is_none(), "no ETags, so nothing can be checked");
    }

    /// A store that keeps only the pages holding something other than zeros,
    /// so an archive past 4 GiB fits in a test.
    #[derive(Default)]
    struct Sparse {
        pages: HashMap<u64, Vec<u8>>,
        len: u64,
        pos: u64,
    }

    const PAGE: u64 = 1 << 16;
    /// Compared against with `==`, which is a `memcmp` even in a debug build.
    static ZEROS: [u8; PAGE as usize] = [0; PAGE as usize];

    impl Write for Sparse {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut done = 0;
            while done < buf.len() {
                let at = self.pos + done as u64;
                let (page, off) = (at / PAGE, (at % PAGE) as usize);
                let n = (PAGE as usize - off).min(buf.len() - done);
                let chunk = &buf[done..done + n];
                if let Some(p) = self.pages.get_mut(&page) {
                    p[off..off + n].copy_from_slice(chunk);
                } else if chunk != &ZEROS[..n] {
                    let mut p = vec![0u8; PAGE as usize];
                    p[off..off + n].copy_from_slice(chunk);
                    self.pages.insert(page, p);
                }
                done += n;
            }
            self.pos += buf.len() as u64;
            self.len = self.len.max(self.pos);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Read for Sparse {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos >= self.len {
                return Ok(0);
            }
            let (page, off) = (self.pos / PAGE, (self.pos % PAGE) as usize);
            let n = (PAGE as usize - off).min(buf.len()).min((self.len - self.pos) as usize);
            match self.pages.get(&page) {
                Some(p) => buf[..n].copy_from_slice(&p[off..off + n]),
                None => buf[..n].fill(0),
            }
            self.pos += n as u64;
            Ok(n)
        }
    }

    impl Seek for Sparse {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.pos = match to {
                SeekFrom::Start(n) => n,
                SeekFrom::End(d) => (self.len as i64 + d) as u64,
                SeekFrom::Current(d) => (self.pos as i64 + d) as u64,
            };
            Ok(self.pos)
        }
    }

    impl Sink for Sparse {
        fn set_len(&mut self, len: u64) -> io::Result<()> {
            self.len = len;
            self.pages.retain(|&p, _| p * PAGE < len);
            if let Some(p) = self.pages.get_mut(&(len / PAGE)) {
                p[(len % PAGE) as usize..].fill(0);
            }
            Ok(())
        }
    }

    // The archive stays whole past 4 GiB, through a resume too: the ZIP64
    // records of a large member survive.
    #[test]
    fn a_member_past_4_gib_keeps_its_size_through_a_resume() {
        let big: u64 = (4 << 30) + (100 << 20);
        let zeros = vec![0u8; 1 << 20];
        let mut out = ZipOut::create(Sparse::default());
        out.start("big.bin", "\"b\"").unwrap();
        let mut left = big;
        while left > 0 {
            let n = left.min(zeros.len() as u64) as usize;
            out.write(&zeros[..n]).unwrap();
            left -= n as u64;
        }
        out.end_member().unwrap();
        out.start("small.txt", "\"s\"").unwrap();
        out.write(b"hello").unwrap();
        out.end_member().unwrap();
        let file = out.finish().unwrap();

        let mut out = ZipOut::resume(file, |_, _, _| true).expect("resumable past 4 GiB");
        out.start("more.txt", "\"m\"").unwrap();
        out.write(b"x").unwrap();
        out.end_member().unwrap();
        let mut file = out.finish().unwrap();

        file.seek(SeekFrom::Start(0)).unwrap();
        let mut ar = zip::ZipArchive::new(&mut file).expect("the archive opens past 4 GiB");
        assert_eq!(ar.len(), 3);
        assert_eq!(ar.by_name("big.bin").unwrap().size(), big);
        let mut small = String::new();
        ar.by_name("small.txt").unwrap().read_to_string(&mut small).unwrap();
        assert_eq!(small, "hello");
        let mut more = String::new();
        ar.by_name("more.txt").unwrap().read_to_string(&mut more).unwrap();
        assert_eq!(more, "x");
    }
}
