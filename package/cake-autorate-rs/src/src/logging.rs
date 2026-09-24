//! Bounded daemon logs: one active file, one previous file, no child processes.
//! A stable private advisory lock serializes the reserved sibling namespace.
//! Existing timestamp archives are retired incrementally, never by a glob rm.
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) const HARD_LOG_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 16 * 1024;
const COMPACTION_MARKER: &[u8] = b"LOG_RETENTION_TRUNCATED\n";
const LEGACY_CLEANUP_BATCH: usize = 32;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn sibling(path: &Path, suffix: &str) -> io::Result<PathBuf> {
    let mut name = path
        .file_name()
        .ok_or_else(|| invalid("log path has no filename"))?
        .to_os_string();
    name.push(suffix);
    Ok(path.with_file_name(name))
}

fn owned_regular(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_file()
        && metadata.nlink() == 1
        && metadata.uid() == unsafe { libc::geteuid() }
}

fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn attest_file(path: &Path, file: &File) -> io::Result<fs::Metadata> {
    let current = fs::symlink_metadata(path)?;
    let opened = file.metadata()?;
    if !owned_regular(&current) || !owned_regular(&opened) || !same_file(&current, &opened) {
        return Err(invalid(
            "log path identity, ownership or link count changed",
        ));
    }
    Ok(current)
}

fn open_file(path: &Path, create: bool) -> io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !owned_regular(&metadata) => {
            return Err(invalid("log path is not an owned single-link regular file"))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound && create => {}
        Err(error) => return Err(error),
        _ => {}
    }
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    attest_file(path, &file)?;
    Ok(file)
}

fn effective_limit(configured: u64) -> u64 {
    if configured == 0 {
        HARD_LOG_FILE_BYTES
    } else {
        configured.min(HARD_LOG_FILE_BYTES)
    }
}

fn bounded_record(line: &str, limit: u64) -> String {
    let maximum = (limit as usize).min(MAX_RECORD_BYTES).saturating_sub(1);
    if line.len() <= maximum {
        return line.to_string();
    }
    let marker = if maximum >= 12 { " [truncated]" } else { "" };
    let mut end = maximum.saturating_sub(marker.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{marker}", &line[..end])
}

/// Only numeric archives emitted by the old daemon, not arbitrary dotted files.
pub(crate) fn legacy_archive_name(name: &std::ffi::OsStr, active: &std::ffi::OsStr) -> bool {
    let (Some(name), Some(active)) = (name.to_str(), active.to_str()) else {
        return false;
    };
    let Some(suffix) = name.strip_prefix(active).and_then(|s| s.strip_prefix('.')) else {
        return false;
    };
    let digits = suffix.strip_suffix(".gz").unwrap_or(suffix);
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && digits.parse::<u64>().is_ok()
}

/// A crash may leave the one reserved compaction file. Do not remove a file
/// that does not carry our marker (or its interrupted initial prefix).
fn recover_compaction(path: &Path) -> io::Result<()> {
    let file = match open_file(path, false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let metadata = attest_file(path, &file)?;
    if metadata.len() > HARD_LOG_FILE_BYTES {
        return Err(invalid("unrecognized oversized log compaction file"));
    }
    let mut prefix = Vec::new();
    (&file)
        .take(COMPACTION_MARKER.len() as u64)
        .read_to_end(&mut prefix)?;
    if prefix.len() < COMPACTION_MARKER.len() {
        if !COMPACTION_MARKER.starts_with(&prefix) {
            return Err(invalid("foreign file occupies log compaction path"));
        }
    } else if prefix != COMPACTION_MARKER {
        return Err(invalid("foreign file occupies log compaction path"));
    }
    attest_file(path, &file)?;
    fs::remove_file(path)
}

fn compact_file(path: &Path, temporary: &Path, limit: u64) -> io::Result<File> {
    let mut source = open_file(path, false)?;
    let original = attest_file(path, &source)?;
    if original.len() <= limit {
        return Ok(source);
    }
    recover_compaction(temporary)?;
    let mut output = OpenOptions::new()
        .read(true)
        .append(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(temporary)?;
    let result = (|| {
        let marker_length = (limit as usize).min(COMPACTION_MARKER.len());
        output.write_all(&COMPACTION_MARKER[..marker_length])?;
        let tail_length = limit as usize - marker_length;
        let mut tail = vec![0u8; tail_length];
        source.seek(SeekFrom::End(-(tail_length as i64)))?;
        source.read_exact(&mut tail)?;
        // Keep complete records only: never manufacture a UTF-8 fragment at
        // the beginning of an old oversized log's retained tail.
        if let Some(newline) = tail.iter().position(|b| *b == b'\n') {
            output.write_all(&tail[newline + 1..])?;
        }
        output.flush()?;
        let current = attest_file(path, &source)?;
        if current.len() != original.len()
            || current.mtime() != original.mtime()
            || current.mtime_nsec() != original.mtime_nsec()
        {
            return Err(invalid("log changed while compacting its retained tail"));
        }
        attest_file(temporary, &output)?;
        fs::rename(temporary, path)?;
        attest_file(path, &output)?;
        Ok(())
    })();
    if result.is_err() && attest_file(temporary, &output).is_ok() {
        let _ = fs::remove_file(temporary);
    }
    result?;
    Ok(output)
}

pub(crate) struct LogFile {
    path: PathBuf,
    previous: PathBuf,
    temporary: PathBuf,
    _namespace_lock: File,
    legacy_entries: Option<fs::ReadDir>,
    file: BufWriter<File>,
    active_missing_after_rotation: bool,
    opened_at: Instant,
    bytes_written: u64,
    bytes_pending: u64,
    last_flush: Instant,
}

impl LogFile {
    #[cfg(test)]
    pub(crate) fn open(path: PathBuf) -> io::Result<Self> {
        Self::open_with_limit(path, HARD_LOG_FILE_BYTES)
    }

    pub(crate) fn open_with_limit(path: PathBuf, configured_limit: u64) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // OpenWrt's /var/log may be a platform symlink. Resolve it once,
        // then require a namespace other users cannot replace entries in.
        let parent = fs::canonicalize(path.parent().unwrap_or_else(|| Path::new(".")))?;
        let directory = fs::symlink_metadata(&parent)?;
        if !directory.is_dir()
            || directory.uid() != unsafe { libc::geteuid() }
            || directory.mode() & 0o022 != 0
        {
            return Err(invalid(
                "log directory is not an owned non-shared namespace",
            ));
        }
        let path = parent.join(
            path.file_name()
                .ok_or_else(|| invalid("log path has no filename"))?,
        );
        let lock = open_file(&sibling(&path, ".lock")?, true)?;
        if lock.metadata()?.len() != 0 {
            return Err(invalid("nonempty file occupies log lock path"));
        }
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        lock.set_permissions(fs::Permissions::from_mode(0o600))?;
        let previous = sibling(&path, ".old")?;
        let temporary = sibling(&path, ".tmp")?;
        recover_compaction(&temporary)?;
        drop(open_file(&path, true)?);
        let limit = effective_limit(configured_limit);
        let active = compact_file(&path, &temporary, limit)?;
        active.set_permissions(fs::Permissions::from_mode(0o600))?;
        match compact_file(&previous, &temporary, limit) {
            Ok(previous) => {
                previous.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let bytes_written = active.metadata()?.len();
        let legacy_entries = Some(fs::read_dir(
            path.parent().unwrap_or_else(|| Path::new(".")),
        )?);
        let mut log = Self {
            path,
            previous,
            temporary,
            _namespace_lock: lock,
            legacy_entries,
            file: BufWriter::new(active),
            active_missing_after_rotation: false,
            opened_at: Instant::now(),
            bytes_written,
            bytes_pending: 0,
            last_flush: Instant::now(),
        };
        log.cleanup_legacy(128)?;
        Ok(log)
    }

    fn cleanup_legacy(&mut self, count: usize) -> io::Result<()> {
        let Some(entries) = self.legacy_entries.as_mut() else {
            return Ok(());
        };
        for _ in 0..count {
            let Some(entry) = entries.next() else {
                self.legacy_entries = None;
                break;
            };
            let entry = entry?;
            if !legacy_archive_name(&entry.file_name(), self.path.file_name().unwrap()) {
                continue;
            }
            let metadata = match fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if owned_regular(&metadata) {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    pub(crate) fn write_line(
        &mut self,
        line: &str,
        max_age: Duration,
        max_size_bytes: u64,
        buffer_size_bytes: u64,
        buffer_timeout: Duration,
        _legacy_export_compress: bool,
    ) -> io::Result<()> {
        let limit = effective_limit(max_size_bytes);
        if self.active_missing_after_rotation {
            // A previous create failure must not append to the archive or
            // overwrite somebody else's replacement active file.
            attest_file(&self.previous, self.file.get_ref())?;
            self.install_new_active()?;
        }
        attest_file(&self.path, self.file.get_ref())?;
        self.bytes_written = self
            .file
            .get_ref()
            .metadata()?
            .len()
            .saturating_add(self.file.buffer().len() as u64);
        if self.bytes_written > limit {
            self.flush()?;
            self.file = BufWriter::new(compact_file(&self.path, &self.temporary, limit)?);
            self.bytes_written = self.file.get_ref().metadata()?.len();
        }
        match fs::symlink_metadata(&self.previous) {
            Ok(metadata) if metadata.len() > limit => {
                compact_file(&self.previous, &self.temporary, limit)?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.cleanup_legacy(LEGACY_CLEANUP_BATCH)?;
        let line = bounded_record(line, limit);
        let pending = line.len() as u64 + 1;
        let age_exceeded = max_age > Duration::ZERO && self.opened_at.elapsed() >= max_age;
        let size_exceeded = self.bytes_written.saturating_add(pending) > limit;
        if age_exceeded || size_exceeded {
            self.rotate()?;
        }
        writeln!(self.file, "{line}")?;
        self.bytes_written = self.bytes_written.saturating_add(pending);
        self.bytes_pending = self.bytes_pending.saturating_add(pending);
        let flush_by_size = buffer_size_bytes == 0 || self.bytes_pending >= buffer_size_bytes;
        let flush_by_time =
            buffer_timeout == Duration::ZERO || self.last_flush.elapsed() >= buffer_timeout;
        if flush_by_size || flush_by_time {
            self.flush()?;
        }
        Ok(())
    }

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.bytes_pending = 0;
        self.last_flush = Instant::now();
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.flush()?;
        attest_file(&self.path, self.file.get_ref())?;
        match fs::symlink_metadata(&self.previous) {
            Ok(metadata) if !owned_regular(&metadata) => {
                return Err(invalid("unsafe previous-log path was preserved"))
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::rename(&self.path, &self.previous)?;
        self.active_missing_after_rotation = true;
        self.install_new_active()
    }

    fn install_new_active(&mut self) -> io::Result<()> {
        let active = OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.path)?;
        attest_file(&self.path, &active)?;
        self.file = BufWriter::new(active);
        self.active_missing_after_rotation = false;
        self.opened_at = Instant::now();
        self.bytes_written = 0;
        self.bytes_pending = 0;
        self.last_flush = Instant::now();
        Ok(())
    }
}

/// stderr is forwarded by procd. Never fork logger from the controller loop.
pub(crate) fn should_emit_stderr(kind: &str, debug_to_syslog: bool, file_enabled: bool) -> bool {
    !file_enabled || matches!(kind, "SYSLOG" | "ERROR") || (kind == "DEBUG" && debug_to_syslog)
}

pub(crate) fn stderr_line(line: &str) -> String {
    let safe: String = line
        .chars()
        .take(1024)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    bounded_record(&safe, 1024)
}

#[derive(Default)]
pub(crate) struct LogFailureThrottle {
    last: Option<(io::ErrorKind, Instant)>,
}

impl LogFailureThrottle {
    pub(crate) fn should_report(&mut self, kind: io::ErrorKind, now: Instant) -> bool {
        if self.last.is_some_and(|(previous, at)| {
            previous == kind && now.duration_since(at) < Duration::from_secs(30)
        }) {
            return false;
        }
        self.last = Some((kind, now));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "cake-log-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn active(&self) -> PathBuf {
            self.0.join("cake-autorate.test.log")
        }
        fn old(&self) -> PathBuf {
            self.0.join("cake-autorate.test.log.old")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn rotation_retains_exactly_one_previous_generation() {
        let f = Fixture::new();
        let mut log = LogFile::open(f.active()).unwrap();
        for n in 0..300 {
            log.write_line(
                &format!("INFO; {n:03}; synthetic record"),
                Duration::ZERO,
                48,
                0,
                Duration::ZERO,
                false,
            )
            .unwrap();
        }
        assert!(
            f.old().is_file(),
            "rotation must use the fixed owned previous path"
        );
        assert_eq!(
            fs::read_dir(&f.0)
                .unwrap()
                .filter(|entry| !entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".lock"))
                .count(),
            2
        );
        assert!(fs::read_to_string(f.active()).unwrap().contains("299"));
        assert!(fs::read_to_string(f.old()).unwrap().contains("298"));
    }

    #[test]
    fn rotation_does_not_compress_even_if_export_compression_is_enabled() {
        let f = Fixture::new();
        let mut log = LogFile::open(f.active()).unwrap();
        log.write_line("INFO; first", Duration::ZERO, 32, 0, Duration::ZERO, true)
            .unwrap();
        log.write_line(
            "INFO; second record beyond the first generation",
            Duration::ZERO,
            32,
            0,
            Duration::ZERO,
            true,
        )
        .unwrap();
        assert!(f.old().is_file());
        assert!(!fs::read_dir(&f.0).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".gz")));
    }

    #[test]
    fn oversized_record_cannot_exceed_file_budget() {
        let f = Fixture::new();
        let mut log = LogFile::open(f.active()).unwrap();
        log.write_line(
            &"ж".repeat(200),
            Duration::ZERO,
            48,
            0,
            Duration::ZERO,
            false,
        )
        .unwrap();
        let data = fs::read(f.active()).unwrap();
        assert!(data.len() <= 48);
        assert!(String::from_utf8(data).is_ok());
    }

    #[test]
    fn log_open_refuses_a_symlink_without_changing_its_target() {
        let f = Fixture::new();
        let target = f.0.join("foreign");
        fs::write(&target, "keep").unwrap();
        symlink(&target, f.active()).unwrap();
        assert!(LogFile::open(f.active()).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "keep");
    }

    #[test]
    fn log_open_refuses_hardlinks_without_changing_the_other_name() {
        let f = Fixture::new();
        let target = f.0.join("foreign");
        fs::write(&target, "keep").unwrap();
        fs::hard_link(&target, f.active()).unwrap();
        assert!(LogFile::open(f.active()).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "keep");
    }

    #[test]
    fn legacy_cleanup_is_bounded_and_preserves_foreign_and_linked_files() {
        let f = Fixture::new();
        for n in 0..500 {
            fs::write(
                sibling(&f.active(), &format!(".{n}.gz")).unwrap(),
                b"legacy",
            )
            .unwrap();
        }
        let foreign = f.0.join("foreign");
        fs::write(&foreign, b"keep").unwrap();
        let link = sibling(&f.active(), ".1001").unwrap();
        symlink(&foreign, &link).unwrap();
        let hard = sibling(&f.active(), ".1002.gz").unwrap();
        fs::hard_link(&foreign, &hard).unwrap();
        let unrelated = sibling(&f.active(), ".operator-backup").unwrap();
        fs::write(&unrelated, b"keep backup").unwrap();
        let other = f.0.join("cake-autorate.other.log.100");
        fs::write(&other, b"keep other instance").unwrap();
        let mut log = LogFile::open(f.active()).unwrap();
        assert!(
            log.legacy_entries.is_some(),
            "startup must not scan an unbounded directory"
        );
        for _ in 0..40 {
            log.write_line(
                "INFO; cleanup",
                Duration::ZERO,
                64,
                0,
                Duration::ZERO,
                false,
            )
            .unwrap();
        }
        assert!(log.legacy_entries.is_none());
        assert_eq!(fs::read_to_string(&foreign).unwrap(), "keep");
        assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
        assert!(hard.exists());
        assert!(unrelated.exists());
        assert!(other.exists());
        let managed_archives = fs::read_dir(&f.0)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                legacy_archive_name(&entry.file_name(), f.active().file_name().unwrap())
                    && fs::symlink_metadata(entry.path())
                        .map(|m| owned_regular(&m))
                        .unwrap_or(false)
            })
            .count();
        assert_eq!(managed_archives, 0);
    }

    #[test]
    fn second_writer_cannot_take_the_same_log_namespace() {
        let f = Fixture::new();
        let first = LogFile::open(f.active()).unwrap();
        assert!(LogFile::open(f.active()).is_err());
        drop(first);
        assert!(LogFile::open(f.active()).is_ok());
    }

    #[test]
    fn independent_instances_do_not_share_their_log_lock() {
        let f = Fixture::new();
        let _first = LogFile::open(f.active()).unwrap();
        assert!(LogFile::open(f.0.join("cake-autorate.second.log")).is_ok());
    }

    #[test]
    fn unsafe_previous_archive_is_preserved_and_active_is_not_rotated() {
        let f = Fixture::new();
        let mut log = LogFile::open(f.active()).unwrap();
        log.write_line(
            "INFO; first complete record",
            Duration::ZERO,
            48,
            0,
            Duration::ZERO,
            false,
        )
        .unwrap();
        let foreign = f.0.join("foreign");
        fs::write(&foreign, b"keep").unwrap();
        symlink(&foreign, f.old()).unwrap();
        assert!(log
            .write_line(
                "INFO; second complete record",
                Duration::ZERO,
                48,
                0,
                Duration::ZERO,
                false
            )
            .is_err());
        assert_eq!(fs::read_to_string(&foreign).unwrap(), "keep");
        assert_eq!(
            fs::read_to_string(f.active()).unwrap(),
            "INFO; first complete record\n"
        );
        assert!(fs::symlink_metadata(f.old())
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn changed_active_path_is_not_overwritten_or_appended_to() {
        let f = Fixture::new();
        let mut log = LogFile::open(f.active()).unwrap();
        fs::rename(f.active(), f.0.join("displaced")).unwrap();
        fs::write(f.active(), b"foreign replacement").unwrap();
        assert!(log
            .write_line(
                "INFO; rejected",
                Duration::ZERO,
                64,
                0,
                Duration::ZERO,
                false
            )
            .is_err());
        assert_eq!(
            fs::read_to_string(f.active()).unwrap(),
            "foreign replacement"
        );
        assert_eq!(fs::read(f.0.join("displaced")).unwrap(), b"");
    }

    #[test]
    fn interrupted_rotation_recovers_a_missing_active_without_changing_previous() {
        let f = Fixture::new();
        fs::write(f.old(), b"INFO; retained before interrupted create\n").unwrap();
        let mut log = LogFile::open(f.active()).unwrap();
        log.write_line(
            "INFO; after restart",
            Duration::ZERO,
            64,
            0,
            Duration::ZERO,
            false,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(f.old()).unwrap(),
            "INFO; retained before interrupted create\n"
        );
        assert_eq!(
            fs::read_to_string(f.active()).unwrap(),
            "INFO; after restart\n"
        );
    }

    #[test]
    fn failed_active_creation_never_resumes_writing_into_previous_generation() {
        let f = Fixture::new();
        let mut log = LogFile::open(f.active()).unwrap();
        log.write_line(
            "INFO; previous",
            Duration::ZERO,
            64,
            0,
            Duration::ZERO,
            false,
        )
        .unwrap();
        log.flush().unwrap();
        fs::rename(f.active(), f.old()).unwrap();
        log.active_missing_after_rotation = true;
        fs::write(f.active(), b"foreign race").unwrap();
        assert!(log
            .write_line(
                "INFO; rejected",
                Duration::ZERO,
                64,
                0,
                Duration::ZERO,
                false
            )
            .is_err());
        assert_eq!(fs::read_to_string(f.old()).unwrap(), "INFO; previous\n");
        assert_eq!(fs::read_to_string(f.active()).unwrap(), "foreign race");
    }

    #[test]
    fn oversized_existing_active_and_previous_are_compacted_to_complete_tail_records() {
        let f = Fixture::new();
        let data = (0..100)
            .map(|n| format!("INFO; record {n:03}\n"))
            .collect::<String>();
        fs::write(f.active(), &data).unwrap();
        fs::write(f.old(), &data).unwrap();
        let _log = LogFile::open_with_limit(f.active(), 96).unwrap();
        for path in [f.active(), f.old()] {
            let data = fs::read(&path).unwrap();
            assert!(data.len() <= 96);
            let text = String::from_utf8(data).unwrap();
            assert!(text.starts_with("LOG_RETENTION_TRUNCATED\n"));
            assert!(text.contains("record 099"));
        }
        assert!(!sibling(&f.active(), ".tmp").unwrap().exists());
    }

    #[test]
    fn configured_zero_and_large_limits_still_have_a_hard_file_bound() {
        assert_eq!(effective_limit(0), HARD_LOG_FILE_BYTES);
        assert_eq!(effective_limit(u64::MAX), HARD_LOG_FILE_BYTES);
        assert_eq!(effective_limit(1024), 1024);
        let f = Fixture::new();
        let file = File::create(f.active()).unwrap();
        file.set_len(HARD_LOG_FILE_BYTES + 1024).unwrap();
        drop(file);
        let _log = LogFile::open(f.active()).unwrap();
        assert!(fs::metadata(f.active()).unwrap().len() <= HARD_LOG_FILE_BYTES);
    }

    #[test]
    fn unfinished_owned_compaction_is_recovered_but_foreign_content_is_preserved() {
        let f = Fixture::new();
        let temporary = sibling(&f.active(), ".tmp").unwrap();
        fs::write(&temporary, b"LOG_RETENTION_TRUNC").unwrap();
        let log = LogFile::open(f.active()).unwrap();
        assert!(!temporary.exists());
        drop(log);
        fs::write(&temporary, b"operator content").unwrap();
        assert!(LogFile::open(f.active()).is_err());
        assert_eq!(fs::read_to_string(temporary).unwrap(), "operator content");
    }

    #[test]
    fn nonempty_lock_file_is_preserved_without_permission_changes() {
        let f = Fixture::new();
        let path = sibling(&f.active(), ".lock").unwrap();
        fs::write(&path, b"operator content").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(LogFile::open(f.active()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "operator content");
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o640);
    }

    #[test]
    fn records_and_stderr_stay_bounded_and_preserve_utf8() {
        for cap in [1, 2, 12, 24, 48, 16384] {
            let record = bounded_record(&"ж".repeat(20000), cap);
            assert!(record.len() + 1 <= cap as usize);
            assert!(std::str::from_utf8(record.as_bytes()).is_ok());
        }
        let text = stderr_line(&format!("ERROR\n\x1b{}", "ж".repeat(10000)));
        assert!(text.len() < 1024);
        assert!(!text.chars().any(char::is_control));
    }

    #[test]
    fn important_messages_reach_stderr_with_file_logging_enabled() {
        assert!(should_emit_stderr("ERROR", false, true));
        assert!(should_emit_stderr("SYSLOG", false, true));
        assert!(!should_emit_stderr("DEBUG", false, true));
        assert!(should_emit_stderr("DEBUG", true, true));
        assert!(!should_emit_stderr("DATA", false, true));
        assert!(should_emit_stderr("INFO", false, false));
    }

    #[test]
    fn repeated_file_io_errors_are_reported_without_a_log_storm() {
        let mut throttle = LogFailureThrottle::default();
        let now = Instant::now();
        assert!(throttle.should_report(io::ErrorKind::PermissionDenied, now));
        assert!(!throttle.should_report(
            io::ErrorKind::PermissionDenied,
            now + Duration::from_secs(1)
        ));
        assert!(throttle.should_report(
            io::ErrorKind::PermissionDenied,
            now + Duration::from_secs(30)
        ));
        assert!(throttle.should_report(io::ErrorKind::InvalidData, now + Duration::from_secs(31)));
    }

    #[test]
    fn flush_failure_does_not_rename_active_or_replace_previous() {
        let f = Fixture::new();
        fs::write(f.active(), b"INFO; retained active\n").unwrap();
        fs::write(f.old(), b"INFO; retained previous\n").unwrap();
        let mut log = LogFile::open(f.active()).unwrap();
        // A read-only descriptor has the same attested file identity, but
        // forces the buffered flush to fail deterministically (no disk fill).
        log.file = BufWriter::new(File::open(f.active()).unwrap());
        log.file.write_all(b"INFO; pending\n").unwrap();
        assert!(log.rotate().is_err());
        assert_eq!(fs::read(f.active()).unwrap(), b"INFO; retained active\n");
        assert_eq!(fs::read(f.old()).unwrap(), b"INFO; retained previous\n");
    }

    #[test]
    fn shared_writable_log_directory_is_rejected_before_creating_reserved_files() {
        let f = Fixture::new();
        fs::set_permissions(&f.0, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(LogFile::open(f.active()).is_err());
        assert_eq!(fs::read_dir(&f.0).unwrap().count(), 0);
    }
}
