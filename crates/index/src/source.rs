//! Bounded reads from pinned, non-following source handles.
use std::{
    fs::File,
    io::{self, Read},
    path::{Component, Path},
};

use code_intel_core::MAX_FILE_BYTES;

pub(crate) fn open(root: &Path, relative: &str) -> io::Result<File> {
    let path = Path::new(relative);
    if relative.is_empty()
        || relative.chars().any(char::is_control)
        || relative.contains('\\')
        || !path.components().all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid repository-relative path",
        ));
    }
    #[cfg(unix)]
    let file = {
        let (parent, name) = unix::parent(root, path)?;
        unix::leaf(&parent, &name)?
    };
    #[cfg(not(unix))]
    let file = {
        let mut selected = root.to_path_buf();
        for component in path.components() {
            selected.push(component);
            if std::fs::symlink_metadata(&selected)?
                .file_type()
                .is_symlink()
            {
                return Err(io::Error::other("source passes through a symlink"));
            }
        }
        if !selected.canonicalize()?.starts_with(root) {
            return Err(io::Error::other("source escaped repository"));
        }
        File::open(selected)?
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("source is not a regular file"));
    }
    Ok(file)
}

pub(crate) fn read(file: File) -> io::Result<Vec<u8>> {
    if file.metadata()?.len() > MAX_FILE_BYTES {
        return Err(io::Error::other("source exceeds 1 MiB limit"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(io::Error::other("source exceeds 1 MiB limit"));
    }
    Ok(bytes)
}

#[cfg(unix)]
mod unix {
    use super::{Component, File, Path, io};
    use rustix::{
        fd::OwnedFd,
        fs::{self, Mode, OFlags},
    };
    use std::ffi::{OsStr, OsString};

    pub(super) fn parent(root: &Path, relative: &Path) -> io::Result<(OwnedFd, OsString)> {
        if !root.is_absolute() {
            return Err(io::Error::other("source root must be absolute"));
        }
        let mut directory = fs::open(
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let path = root.join(relative);
        for component in path
            .parent()
            .expect("validated source has a parent")
            .components()
        {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    directory = fs::openat(
                        &directory,
                        name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )?;
                }
                _ => return Err(io::Error::other("invalid source component")),
            }
        }
        Ok((
            directory,
            relative
                .file_name()
                .expect("validated file name")
                .to_owned(),
        ))
    }

    pub(super) fn leaf(parent: &OwnedFd, name: &OsStr) -> io::Result<File> {
        Ok(fs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?
        .into())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{fs as disk, os::unix::fs::symlink};

        #[test]
        fn fifo_replacement_is_nonblocking_and_rejected_before_reading() {
            let root = tempfile::tempdir().unwrap();
            let root_path = root.path().canonicalize().unwrap();
            disk::write(root.path().join("policy.md"), "original").unwrap();
            let (parent, name) = parent(&root_path, Path::new("policy.md")).unwrap();
            disk::remove_file(root.path().join("policy.md")).unwrap();
            fs::mkfifoat(&parent, &name, Mode::RUSR | Mode::WUSR).unwrap();
            let file = leaf(&parent, &name).unwrap();
            assert!(!file.metadata().unwrap().is_file());
            assert!(crate::source::open(&root_path, "policy.md").is_err());
        }

        #[test]
        fn renamed_parent_stays_pinned_and_leaf_symlinks_are_rejected() {
            let root = tempfile::tempdir().unwrap();
            let root_path = root.path().canonicalize().unwrap();
            let outside = tempfile::tempdir().unwrap();
            disk::create_dir(root.path().join("docs")).unwrap();
            disk::write(root.path().join("docs/evidence.md"), "inside").unwrap();
            disk::write(outside.path().join("evidence.md"), "outside").unwrap();
            let (parent, name) = parent(&root_path, Path::new("docs/evidence.md")).unwrap();
            disk::rename(root.path().join("docs"), root.path().join("held")).unwrap();
            symlink(outside.path(), root.path().join("docs")).unwrap();
            assert_eq!(
                crate::source::read(leaf(&parent, &name).unwrap()).unwrap(),
                b"inside"
            );
            assert!(crate::source::open(&root_path, "docs/evidence.md").is_err());
            disk::remove_file(root.path().join("held/evidence.md")).unwrap();
            symlink(
                outside.path().join("evidence.md"),
                root.path().join("held/evidence.md"),
            )
            .unwrap();
            assert!(leaf(&parent, &name).is_err());
        }
    }
}
