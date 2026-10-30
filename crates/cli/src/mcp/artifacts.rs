//! Immutable, root-scoped transport storage. No analysis or caller paths here.
use std::{fmt, path::Path};

use crate::manifest_artifact::{Verified, valid_id};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

pub const MAX_ENTRIES: usize = 32;
pub const MAX_STORAGE: usize = 64_000_000;
pub const MAX_CHUNK: usize = 16_000;

fn encoded(value: &Value, budget: usize) -> Result<String> {
    let text = format!("{value}\n");
    let result = json!({"content":[{"type":"text","text":text}],"isError":false});
    if result.to_string().len() + 128 > budget {
        return Err(Error::new(
            "response_too_large",
            "complete transport response exceeds max_bytes; increase the response budget",
        ));
    }
    Ok(text)
}

pub fn descriptor(artifact: &Verified<'_>, budget: usize) -> Result<String> {
    encoded(
        &json!({
            "kind":"verification_manifest_descriptor","is_manifest":false,
            "protocol":"atlas-manifest-bytes","transport_version":1,
            "schema_version":1,"manifest_id":artifact.manifest_id,
            "artifact_id":artifact.envelope_digest,"envelope_digest":artifact.envelope_digest,
            "digest_algorithm":"blake3-256","total_bytes":artifact.bytes.len(),
            "snapshots":{"base":artifact.value["snapshots"]["base"],"head":artifact.value["snapshots"]["head"]},
            "availability":"retained",
            "retrieval":{"tool":"atlas_impact","encoding":"base64","offset_unit":"decoded_bytes",
                "max_chunk_bytes":MAX_CHUNK,"advance_by":"next_offset",
                "arguments":{"artifact":{"id":artifact.envelope_digest,"offset":0,"length":MAX_CHUNK}}}
        }),
        budget,
    )
}

pub fn chunk(
    bytes: &[u8],
    id: &str,
    offset: usize,
    length: usize,
    budget: usize,
) -> Result<String> {
    if offset >= bytes.len() || !(1..=MAX_CHUNK).contains(&length) {
        return Err(Error::new(
            "invalid_arguments",
            "offset must be below total_bytes and length must be 1..16000 decoded bytes",
        ));
    }
    let response = |count: usize| {
        encoded(
            &json!({
                "kind":"verification_manifest_chunk","is_manifest":false,
                "protocol":"atlas-manifest-bytes","transport_version":1,
                "artifact_id":id,"envelope_digest":id,"digest_algorithm":"blake3-256",
                "total_bytes":bytes.len(),"offset":offset,"next_offset":offset+count,
                "decoded_bytes":count,"encoding":"base64","data":STANDARD.encode(&bytes[offset..offset+count]),
                "final":offset+count==bytes.len()
            }),
            budget,
        )
    };
    let mut low = 1;
    let mut high = length.min(bytes.len() - offset);
    let mut best = response(1)?;
    while low <= high {
        let mid = low + (high - low) / 2;
        if let Ok(text) = response(mid) {
            best = text;
            low = mid + 1;
        } else {
            high = mid - 1;
        }
    }
    Ok(best)
}

#[derive(Debug, Clone, Copy)]
pub struct Error {
    pub code: &'static str,
    message: &'static str,
}

impl Error {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

fn expired() -> Error {
    Error::new(
        "artifact_expired",
        "artifact is unavailable in this root; prepare a new manifest explicitly",
    )
}
fn busy() -> Error {
    Error::new(
        "artifact_cache_busy",
        "artifact cache is busy; retry the request",
    )
}
fn rejected() -> Error {
    Error::new(
        "state_rejected",
        "artifact cache path or entry was rejected",
    )
}
fn corrupt() -> Error {
    Error::new(
        "artifact_corrupt",
        "cached artifact failed integrity validation; discard incomplete retrieval",
    )
}

#[cfg(unix)]
mod disk {
    use super::{
        MAX_ENTRIES, MAX_STORAGE, Path, Result, Verified, busy, corrupt, expired, rejected,
        valid_id,
    };
    use crate::manifest_artifact::{digest, verify};
    use code_intel_retrieval::manifest::MAX_BYTES;
    use rustix::{
        fd::OwnedFd,
        fs::{self, AtFlags, Dir, FileType, FlockOperation, Mode, OFlags},
        io::Errno,
    };
    use std::{
        fs::File,
        io::{Read, Write},
        os::unix::ffi::OsStrExt,
        path::Component,
    };

    pub struct Cache {
        dir: OwnedFd,
        _lock: OwnedFd,
        entries: usize,
        storage: usize,
    }

    #[derive(Clone)]
    struct Entry {
        name: String,
        sequence: u64,
        id: String,
        bytes: usize,
    }

    fn directory(parent: &OwnedFd, name: &std::ffi::OsStr, create: bool) -> Result<OwnedFd> {
        if create {
            match fs::mkdirat(parent, name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(_) => return Err(rejected()),
            }
        }
        fs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| {
            if !create && e == Errno::NOENT {
                expired()
            } else {
                rejected()
            }
        })
    }

    fn regular(fd: &OwnedFd) -> Result<fs::Stat> {
        let stat = fs::fstat(fd).map_err(|_| rejected())?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
            return Err(rejected());
        }
        Ok(stat)
    }

    impl Cache {
        pub fn open(root: &Path, create: bool) -> Result<Self> {
            // Open every absolute-path component without following symlinks.
            let mut dir = fs::open(
                "/",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| rejected())?;
            for component in root.components() {
                match component {
                    Component::RootDir => {}
                    Component::Normal(name) => dir = directory(&dir, name, false)?,
                    _ => return Err(rejected()),
                }
            }
            for name in [".code-intel", "impact", "artifacts-v1"] {
                dir = directory(&dir, name.as_ref(), create)?;
            }
            let flags = OFlags::RDWR
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC
                | OFlags::NONBLOCK
                | if create {
                    OFlags::CREATE
                } else {
                    OFlags::empty()
                };
            // macOS can fail an O_CREAT open with ENOENT while another process
            // creates the same file; that is contention, not a missing artifact.
            let mut attempts = 0;
            let lock = loop {
                match fs::openat(&dir, ".lock", flags, Mode::RUSR | Mode::WUSR) {
                    Ok(lock) => break lock,
                    Err(Errno::NOENT) if create && attempts < 3 => attempts += 1,
                    Err(Errno::NOENT) if create => return Err(busy()),
                    Err(Errno::NOENT) => return Err(expired()),
                    Err(_) => return Err(rejected()),
                }
            };
            regular(&lock)?;
            fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|_| busy())?;
            let cache = Self {
                dir,
                _lock: lock,
                entries: MAX_ENTRIES,
                storage: MAX_STORAGE,
            };
            let binding = digest(root.as_os_str().as_bytes());
            match cache.read_file("root", 71) {
                Ok(bytes) if bytes == binding.as_bytes() => {}
                Err(e) if e.code == "artifact_expired" && create => {
                    if !cache.list()?.is_empty() {
                        return Err(rejected());
                    }
                    cache.install("root", binding.as_bytes())?;
                }
                _ => return Err(rejected()),
            }
            Ok(cache)
        }

        fn read_file(&self, name: &str, maximum: usize) -> Result<Vec<u8>> {
            let fd = fs::openat(
                &self.dir,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| {
                if e == Errno::NOENT {
                    expired()
                } else {
                    rejected()
                }
            })?;
            let stat = regular(&fd)?;
            let size = usize::try_from(stat.st_size).map_err(|_| corrupt())?;
            if size == 0 || size > maximum {
                return Err(corrupt());
            }
            let mut bytes = Vec::with_capacity(size);
            File::from(fd)
                .take(u64::try_from(maximum + 1).unwrap())
                .read_to_end(&mut bytes)
                .map_err(|_| corrupt())?;
            if bytes.len() != size {
                return Err(corrupt());
            }
            Ok(bytes)
        }

        fn list(&self) -> Result<Vec<Entry>> {
            let mut entries = Vec::new();
            let mut count = 0;
            for item in Dir::read_from(&self.dir).map_err(|_| rejected())? {
                let item = item.map_err(|_| rejected())?;
                let name = item.file_name().to_str().map_err(|_| rejected())?;
                if matches!(name, "." | "..") {
                    continue;
                }
                count += 1;
                if count > MAX_ENTRIES + 8 {
                    return Err(rejected());
                }
                let stat = fs::statat(&self.dir, name, AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(|_| rejected())?;
                if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
                    || stat.st_nlink != 1
                {
                    return Err(rejected());
                }
                if matches!(name, ".lock" | "root" | ".install") {
                    continue;
                }
                let (sequence, hex) = name
                    .strip_suffix(".json")
                    .and_then(|s| s.split_once('-'))
                    .ok_or_else(rejected)?;
                let id = format!("blake3:{hex}");
                if sequence.len() != 16
                    || !sequence.bytes().all(|b| b.is_ascii_hexdigit())
                    || !valid_id(&id)
                {
                    return Err(rejected());
                }
                let sequence = u64::from_str_radix(sequence, 16).map_err(|_| rejected())?;
                if sequence == 0 {
                    return Err(rejected());
                }
                let bytes = usize::try_from(stat.st_size).map_err(|_| corrupt())?;
                if bytes == 0 || bytes > MAX_BYTES {
                    return Err(corrupt());
                }
                entries.push(Entry {
                    name: name.to_owned(),
                    sequence,
                    id,
                    bytes,
                });
            }
            entries.sort_by(|a, b| (a.sequence, &a.id).cmp(&(b.sequence, &b.id)));
            Ok(entries)
        }

        fn install(&self, name: &str, bytes: &[u8]) -> Result<()> {
            // Only our single regular temporary file is ever cleaned up.
            match fs::statat(&self.dir, ".install", AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat)
                    if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
                        && stat.st_nlink == 1 =>
                {
                    fs::unlinkat(&self.dir, ".install", AtFlags::empty())
                        .map_err(|_| rejected())?;
                }
                Err(Errno::NOENT) => {}
                _ => return Err(rejected()),
            }
            let fd = fs::openat(
                &self.dir,
                ".install",
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|_| rejected())?;
            let mut file = File::from(fd);
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| rejected())?;
            fs::renameat(&self.dir, ".install", &self.dir, name).map_err(|_| rejected())?;
            fs::fsync(&self.dir).map_err(|_| rejected())?;
            Ok(())
        }

        pub fn retain(&self, artifact: &Verified<'_>) -> Result<()> {
            let entries = self.list()?;
            if let Some(entry) = entries.iter().find(|e| e.id == artifact.envelope_digest) {
                let retained = self.read(&entry.id)?;
                if retained != artifact.bytes {
                    return Err(corrupt());
                }
                return Ok(());
            }
            let next = entries
                .last()
                .map_or(Some(1), |e| e.sequence.checked_add(1))
                .ok_or_else(rejected)?;
            let mut size = entries.iter().map(|e| e.bytes).sum::<usize>() + artifact.bytes.len();
            let mut count = entries.len() + 1;
            for entry in entries {
                if count <= self.entries && size <= self.storage {
                    break;
                }
                fs::unlinkat(&self.dir, entry.name.as_str(), AtFlags::empty())
                    .map_err(|_| rejected())?;
                count -= 1;
                size -= entry.bytes;
            }
            if count > self.entries || size > self.storage {
                return Err(rejected());
            }
            self.install(
                &format!("{next:016x}-{}.json", &artifact.envelope_digest[7..]),
                artifact.bytes,
            )
        }

        pub fn read(&self, id: &str) -> Result<Vec<u8>> {
            if !valid_id(id) {
                return Err(rejected());
            }
            let entries = self.list()?;
            let entry = entries.iter().find(|e| e.id == id).ok_or_else(expired)?;
            let bytes = self.read_file(&entry.name, MAX_BYTES)?;
            let artifact = verify(&bytes).map_err(|_| corrupt())?;
            if artifact.envelope_digest != id {
                return Err(corrupt());
            }
            Ok(bytes)
        }
    }

    #[cfg(test)]
    mod unit {
        use super::{Cache, digest, verify};
        use serde_json::json;

        fn bytes(depth: usize) -> Vec<u8> {
            let mut payload = json!({"schema_version":1,"analysis":{"depth":depth},
                "snapshots":{"base":{},"head":{}},"changes":{},"affected_code":{},
                "verification":{},"coverage":{},"completeness":{"output_truncated":false,"output_omissions":{}}});
            let id = digest(&serde_json::to_vec(&payload).unwrap());
            payload["manifest_id"] = json!(id);
            format!("{payload}\n").into_bytes()
        }

        #[test]
        fn byte_limit_evicts_fifo_and_abandoned_install_is_recovered() {
            let dir = tempfile::TempDir::new().unwrap();
            // Servers open canonical roots; temp paths can contain links (macOS /var).
            let root = dir.path().canonicalize().unwrap();
            let a = bytes(1);
            let b = bytes(2);
            let c = bytes(3);
            let mut cache = Cache::open(&root, true).unwrap();
            cache.storage = a.len() * 2;
            cache.retain(&verify(&a).unwrap()).unwrap();
            cache.retain(&verify(&b).unwrap()).unwrap();
            std::fs::write(
                root.join(".code-intel/impact/artifacts-v1/.install"),
                "incomplete",
            )
            .unwrap();
            cache.retain(&verify(&c).unwrap()).unwrap();
            assert_eq!(
                cache.read(&digest(&a)).unwrap_err().code,
                "artifact_expired"
            );
            assert_eq!(cache.read(&digest(&b)).unwrap(), b);
            assert_eq!(cache.read(&digest(&c)).unwrap(), c);
            assert!(cache.list().unwrap().iter().map(|e| e.bytes).sum::<usize>() <= cache.storage);
            assert!(
                !root
                    .join(".code-intel/impact/artifacts-v1/.install")
                    .exists()
            );
        }

        #[test]
        fn readers_are_serialized_and_temporary_symlink_is_rejected() {
            let dir = tempfile::TempDir::new().unwrap();
            // Servers open canonical roots; temp paths can contain links (macOS /var).
            let root = dir.path().canonicalize().unwrap();
            let cache = Cache::open(&root, true).unwrap();
            assert_eq!(
                Cache::open(&root, true).err().unwrap().code,
                "artifact_cache_busy"
            );
            let outside = tempfile::NamedTempFile::new().unwrap();
            std::os::unix::fs::symlink(
                outside.path(),
                root.join(".code-intel/impact/artifacts-v1/.install"),
            )
            .unwrap();
            assert_eq!(
                cache.retain(&verify(&bytes(1)).unwrap()).unwrap_err().code,
                "state_rejected"
            );
            assert_eq!(outside.as_file().metadata().unwrap().len(), 0);
        }
    }
}

#[cfg(unix)]
pub use disk::Cache;

#[cfg(not(unix))]
pub struct Cache;
#[cfg(not(unix))]
impl Cache {
    pub fn open(_: &Path, _: bool) -> Result<Self> {
        Err(Error::new(
            "artifact_transport_unavailable",
            "artifact retention needs Unix no-follow filesystem operations; use CLI manifest output",
        ))
    }
    pub fn retain(&self, _: &Verified<'_>) -> Result<()> {
        unreachable!()
    }
    pub fn read(&self, _: &str) -> Result<Vec<u8>> {
        unreachable!()
    }
}
