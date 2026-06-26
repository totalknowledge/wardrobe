use super::diagnostics::{is_drawer_data_file, relative_path_string, split_structural_path};
use super::{BackupArchive, BackupArchiveFile, RestoreReport, WardrobeEngine};
use std::fs;
use std::io::{Error, ErrorKind, Result};
use std::path::{Component, Path, PathBuf};

const BACKUP_ARCHIVE_FORMAT: &str = "wardrobe-cli-backup-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackupScope {
    Wardrobe,
    Bay,
    Drawer,
}

impl BackupScope {
    fn from_segment_count(segment_count: usize, label: &str) -> Result<Self> {
        match segment_count {
            1 => Ok(Self::Wardrobe),
            2 => Ok(Self::Bay),
            3 => Ok(Self::Drawer),
            _ => Err(Error::new(
                ErrorKind::InvalidInput,
                format!("{label} must identify a wardrobe, bay, or drawer"),
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Wardrobe => "wardrobe",
            Self::Bay => "bay",
            Self::Drawer => "drawer",
        }
    }

    fn expected_segments(self) -> usize {
        match self {
            Self::Wardrobe => 1,
            Self::Bay => 2,
            Self::Drawer => 3,
        }
    }
}

#[derive(Debug)]
struct StructuralBackupTarget {
    scope: BackupScope,
    segments: Vec<String>,
    logical_path: String,
    storage_path: PathBuf,
}

pub(super) fn backup_archive(engine: &WardrobeEngine, source_path: &str) -> Result<BackupArchive> {
    let target =
        structural_backup_target(&engine.root_directory, source_path, "backup source path")?;
    let files = collect_backup_archive_files(&target)?;
    Ok(BackupArchive {
        format: BACKUP_ARCHIVE_FORMAT.to_string(),
        source_path: target.logical_path,
        scope: target.scope.as_str().to_string(),
        files,
    })
}

pub(super) fn restore_archive(
    engine: &WardrobeEngine,
    destination_path: &str,
    archive: BackupArchive,
) -> Result<RestoreReport> {
    validate_backup_archive_format(&archive)?;
    let target = structural_backup_target(
        &engine.root_directory,
        destination_path,
        "restore destination path",
    )?;
    validate_archive_scope(&archive, &target)?;
    let decoded_files = decoded_restore_files(&archive, &target)?;
    let byte_count = decoded_files
        .iter()
        .map(|(_, bytes)| bytes.len())
        .sum::<usize>();

    clear_restore_target(&engine.root_directory, &target)?;
    for (relative_path, bytes) in &decoded_files {
        let destination = target.storage_path.join(relative_path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(destination, bytes)?;
    }
    register_restored_catalog(engine, &target)?;

    Ok(RestoreReport {
        destination_path: target.logical_path,
        scope: target.scope.as_str().to_string(),
        file_count: decoded_files.len(),
        byte_count,
    })
}

fn register_restored_catalog(
    engine: &WardrobeEngine,
    target: &StructuralBackupTarget,
) -> Result<()> {
    let Some(wardrobe) = target.segments.first() else {
        return Ok(());
    };

    engine.create_database(wardrobe)?;
    match target.scope {
        BackupScope::Wardrobe => {
            for bay in restored_bay_names(&target.storage_path)? {
                engine.create_schema(wardrobe, &bay)?;
                let bay_path = target.storage_path.join(&bay);
                for drawer in restored_drawer_names(&bay_path)? {
                    engine.create_drawer(wardrobe, &bay, &drawer)?;
                }
            }
        }
        BackupScope::Bay => {
            let Some(bay) = target.segments.get(1) else {
                return Ok(());
            };
            engine.create_schema(wardrobe, bay)?;
            for drawer in restored_drawer_names(&target.storage_path)? {
                engine.create_drawer(wardrobe, bay, &drawer)?;
            }
        }
        BackupScope::Drawer => {
            let (Some(bay), Some(drawer)) = (target.segments.get(1), target.segments.get(2)) else {
                return Ok(());
            };
            engine.create_schema(wardrobe, bay)?;
            engine.create_drawer(wardrobe, bay, drawer)?;
        }
    }

    Ok(())
}

fn structural_backup_target(
    root_directory: &Path,
    raw_path: &str,
    label: &str,
) -> Result<StructuralBackupTarget> {
    let segments = split_structural_path(raw_path, label)?;
    let scope = BackupScope::from_segment_count(segments.len(), label)?;
    let storage_path = match scope {
        BackupScope::Wardrobe | BackupScope::Bay => segments
            .iter()
            .fold(root_directory.to_path_buf(), |path, segment| {
                path.join(segment)
            }),
        BackupScope::Drawer => root_directory.join(&segments[0]).join(&segments[1]),
    };
    Ok(StructuralBackupTarget {
        scope,
        logical_path: segments.join("/"),
        segments,
        storage_path,
    })
}

fn collect_backup_archive_files(target: &StructuralBackupTarget) -> Result<Vec<BackupArchiveFile>> {
    match target.scope {
        BackupScope::Wardrobe | BackupScope::Bay => {
            if !target.storage_path.is_dir() {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    format!("backup source path does not exist: {}", target.logical_path),
                ));
            }
            let mut files = Vec::new();
            collect_directory_archive_files(
                &target.storage_path,
                &target.storage_path,
                &mut files,
            )?;
            if files.is_empty() {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    format!(
                        "backup source path contains no files: {}",
                        target.logical_path
                    ),
                ));
            }
            files.sort_by(|left, right| left.path.cmp(&right.path));
            Ok(files)
        }
        BackupScope::Drawer => {
            let Some(drawer) = target.segments.get(2) else {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "drawer backup requires a drawer path",
                ));
            };
            let mut files = Vec::new();
            for file_name in [
                format!("{drawer}.drw"),
                format!("{drawer}_index.drw"),
                format!("{drawer}_meta.drw"),
            ] {
                let path = target.storage_path.join(&file_name);
                if path.is_file() {
                    files.push(BackupArchiveFile {
                        path: file_name,
                        bytes_hex: encode_hex(&fs::read(path)?),
                    });
                }
            }
            if files.is_empty() {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    format!(
                        "drawer backup source contains no drawer files: {}",
                        target.logical_path
                    ),
                ));
            }
            Ok(files)
        }
    }
}

fn collect_directory_archive_files(
    base: &Path,
    current: &Path,
    files: &mut Vec<BackupArchiveFile>,
) -> Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_directory_archive_files(base, &path, files)?;
        } else if path.is_file() {
            let relative_path = path.strip_prefix(base).map_err(|error| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Failed to compute backup relative path: {error}"),
                )
            })?;
            files.push(BackupArchiveFile {
                path: relative_path_string(relative_path),
                bytes_hex: encode_hex(&fs::read(path)?),
            });
        }
    }
    Ok(())
}

fn validate_backup_archive_format(archive: &BackupArchive) -> Result<()> {
    if archive.format != BACKUP_ARCHIVE_FORMAT {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "Invalid backup archive format: expected {BACKUP_ARCHIVE_FORMAT}, found {}",
                archive.format
            ),
        ));
    }
    Ok(())
}

fn validate_archive_scope(archive: &BackupArchive, target: &StructuralBackupTarget) -> Result<()> {
    let archive_scope = match archive.scope.as_str() {
        "wardrobe" => BackupScope::Wardrobe,
        "bay" => BackupScope::Bay,
        "drawer" => BackupScope::Drawer,
        other => {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Invalid backup archive scope: {other}"),
            ));
        }
    };
    if archive_scope != target.scope {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "restore destination '{}' is a {}, but archive contains a {} backup",
                target.logical_path,
                target.scope.as_str(),
                archive.scope
            ),
        ));
    }
    let source_segments =
        split_structural_path(&archive.source_path, "backup archive source path")?;
    if source_segments.len() != target.scope.expected_segments() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "restore destination path does not match archive scope",
        ));
    }
    Ok(())
}

fn decoded_restore_files(
    archive: &BackupArchive,
    target: &StructuralBackupTarget,
) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut files = Vec::new();
    for file in &archive.files {
        let relative_path = restore_relative_path(archive, target, &file.path)?;
        let bytes = decode_hex(&file.bytes_hex)?;
        files.push((relative_path, bytes));
    }
    Ok(files)
}

fn restore_relative_path(
    archive: &BackupArchive,
    target: &StructuralBackupTarget,
    archive_path: &str,
) -> Result<PathBuf> {
    validate_archive_relative_path(archive_path)?;
    if target.scope != BackupScope::Drawer {
        return Ok(PathBuf::from(archive_path));
    }

    let source_segments =
        split_structural_path(&archive.source_path, "backup archive source path")?;
    let source_drawer = source_segments.last().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "drawer archive source path does not include a drawer",
        )
    })?;
    let destination_drawer = target.segments.get(2).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            "drawer restore requires a destination drawer path",
        )
    })?;
    let archive_file = Path::new(archive_path);
    if archive_file.components().count() != 1 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "drawer backup archive cannot contain nested file paths",
        ));
    }
    let Some(file_name) = archive_file.file_name().and_then(|file| file.to_str()) else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "drawer backup archive contains an invalid file path",
        ));
    };

    let mapped_name = if file_name == format!("{source_drawer}.drw") {
        format!("{destination_drawer}.drw")
    } else if file_name == format!("{source_drawer}_index.drw") {
        format!("{destination_drawer}_index.drw")
    } else if file_name == format!("{source_drawer}_meta.drw") {
        format!("{destination_drawer}_meta.drw")
    } else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("Unexpected drawer backup file: {file_name}"),
        ));
    };

    Ok(PathBuf::from(mapped_name))
}

fn clear_restore_target(root_directory: &Path, target: &StructuralBackupTarget) -> Result<()> {
    match target.scope {
        BackupScope::Wardrobe | BackupScope::Bay => {
            ensure_path_is_under_root(root_directory, &target.storage_path)?;
            if target.storage_path.exists() {
                fs::remove_dir_all(&target.storage_path)?;
            }
        }
        BackupScope::Drawer => {
            let Some(drawer) = target.segments.get(2) else {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "drawer restore requires a drawer path",
                ));
            };
            ensure_path_is_under_root(root_directory, &target.storage_path)?;
            for file_name in [
                format!("{drawer}.drw"),
                format!("{drawer}_index.drw"),
                format!("{drawer}_meta.drw"),
            ] {
                let path = target.storage_path.join(file_name);
                if path.exists() {
                    fs::remove_file(path)?;
                }
            }
        }
    }
    Ok(())
}

fn restored_bay_names(wardrobe_path: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    if !wardrobe_path.exists() {
        return Ok(names);
    }
    for entry in fs::read_dir(wardrobe_path)? {
        let entry = entry?;
        if entry.path().is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    Ok(names)
}

fn restored_drawer_names(bay_path: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    if !bay_path.exists() {
        return Ok(names);
    }
    for entry in fs::read_dir(bay_path)? {
        let entry = entry?;
        let path = entry.path();
        if !is_drawer_data_file(&path) {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            names.push(stem.to_string());
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn validate_archive_relative_path(path: &str) -> Result<()> {
    let relative_path = Path::new(path);
    if relative_path.is_absolute() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "backup archive file paths must be relative",
        ));
    }
    for component in relative_path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "backup archive file path escapes the restore target",
            ));
        }
    }
    Ok(())
}

fn ensure_path_is_under_root(root_directory: &Path, target: &Path) -> Result<()> {
    let root = absolute_lexical_path(root_directory);
    let target = absolute_lexical_path(target);
    if !target.starts_with(&root) {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "Refusing to restore outside the storage root: {}",
                target.display()
            ),
        ));
    }
    Ok(())
}

fn absolute_lexical_path(path: &Path) -> PathBuf {
    let mut absolute = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                absolute.pop();
            }
            other => absolute.push(other.as_os_str()),
        }
    }
    absolute
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex(raw: &str) -> Result<Vec<u8>> {
    let raw_bytes = raw.as_bytes();
    if raw_bytes.len() % 2 != 0 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Invalid backup archive hex payload length",
        ));
    }
    let mut bytes = Vec::with_capacity(raw_bytes.len() / 2);
    for pair in raw_bytes.chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_value(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            "Invalid backup archive hex payload",
        )),
    }
}
