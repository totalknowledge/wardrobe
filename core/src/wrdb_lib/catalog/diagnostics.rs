use crate::engine::{
    CheckEntry, CheckReport, DrawerInspectionMetrics, StorageDiagnosis, WardrobeEngine,
};
use std::fs;
use std::io::{Error, ErrorKind, Result};
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
struct InspectTarget {
    data_dir: PathBuf,
    drawer_name: String,
    label: String,
}

struct DrawerFiles {
    data: PathBuf,
    index: PathBuf,
    meta: PathBuf,
}

#[derive(Default)]
struct StorageBreakdown {
    total_bytes: u64,
    data_bytes: u64,
    index_bytes: u64,
    metadata_bytes: u64,
    logical_wal_bytes: u64,
    transaction_wal_bytes: u64,
    other_bytes: u64,
}

enum StorageFileKind {
    Data,
    Index,
    Metadata,
    LogicalWal,
    TransactionWal,
    Other,
}

pub(crate) fn inspect_drawer(
    engine: &WardrobeEngine,
    drawer_name: &str,
) -> Result<DrawerInspectionMetrics> {
    let target = inspect_target(engine.root_directory(), drawer_name)?;
    let files = drawer_files(&target.data_dir, &target.drawer_name);
    let data_bytes = file_size_or_zero(&files.data)?;
    let index_bytes = file_size_or_zero(&files.index)?;
    let meta_bytes = file_size_or_zero(&files.meta)?;
    let total_bytes = data_bytes
        .saturating_add(index_bytes)
        .saturating_add(meta_bytes);
    let register_file_count = [&files.data, &files.index, &files.meta]
        .iter()
        .filter(|path| path.is_file())
        .count();
    let record_count = engine.count(target.label.as_str(), None::<crate::OperationOptions>)?;

    Ok(DrawerInspectionMetrics {
        path: target.label,
        data_bytes,
        index_bytes,
        meta_bytes,
        total_bytes,
        record_count,
        register_file_count,
        tombstone_fragmentation_percent: None,
    })
}

pub(crate) fn check_path(engine: &WardrobeEngine, raw_path: &str) -> Result<CheckReport> {
    let segments = split_structural_path(raw_path, "check path")?;
    let logical_path = segments.join("/");
    let mut entries = Vec::new();

    let kind = match segments.len() {
        1 => {
            let path = engine.root_directory().join(&segments[0]);
            entries.push(check_entry("directory", &path)?);
            "wardrobe"
        }
        2 => {
            let path = engine
                .root_directory()
                .join(&segments[0])
                .join(&segments[1]);
            entries.push(check_entry("directory", &path)?);
            "bay"
        }
        3 => {
            let files = drawer_files(engine.root_directory(), &logical_path);
            entries.push(check_entry("data", &files.data)?);
            entries.push(check_entry("index", &files.index)?);
            entries.push(check_entry("meta", &files.meta)?);
            "drawer"
        }
        _ => {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "check path must identify a wardrobe, bay, or drawer",
            ));
        }
    };

    Ok(CheckReport {
        path: logical_path,
        kind: kind.to_string(),
        entries,
    })
}

pub(crate) fn diagnose_storage(engine: &WardrobeEngine) -> Result<StorageDiagnosis> {
    let drawers = list_drawer_names(engine)?;
    let breakdown = storage_breakdown(engine.root_directory())?;
    Ok(StorageDiagnosis {
        storage_directory: engine.root_directory().display().to_string(),
        storage_bytes: breakdown.total_bytes,
        data_bytes: breakdown.data_bytes,
        index_bytes: breakdown.index_bytes,
        metadata_bytes: breakdown.metadata_bytes,
        logical_wal_bytes: breakdown.logical_wal_bytes,
        transaction_wal_bytes: breakdown.transaction_wal_bytes,
        other_bytes: breakdown.other_bytes,
        drawer_count: drawers.len(),
        status: if drawers.is_empty() {
            "empty".to_string()
        } else {
            "ok".to_string()
        },
        drawers,
    })
}

pub(crate) fn list_drawer_names(engine: &WardrobeEngine) -> Result<Vec<String>> {
    let mut drawers = Vec::new();
    collect_drawer_names(
        engine.root_directory(),
        engine.root_directory(),
        &mut drawers,
    )?;
    drawers.sort();
    drawers.dedup();
    Ok(drawers)
}

fn inspect_target(root_directory: &Path, raw_path: &str) -> Result<InspectTarget> {
    let mut segments = split_structural_path(raw_path, "inspect path")?;
    let drawer_name = segments
        .pop()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "inspect requires a drawer name"))?;
    let mut data_dir = root_directory.to_path_buf();
    for segment in &segments {
        data_dir.push(segment);
    }
    let label = if segments.is_empty() {
        drawer_name.clone()
    } else {
        format!("{}/{}", segments.join("/"), drawer_name)
    };

    Ok(InspectTarget {
        data_dir,
        drawer_name,
        label,
    })
}

pub(crate) fn split_structural_path(raw_path: &str, label: &str) -> Result<Vec<String>> {
    let mut segments = Vec::new();
    for segment in raw_path.split(|c| c == '/' || c == '\\') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Invalid {label} segment: {segment}"),
            ));
        }
        segments.push(segment.to_string());
    }
    if segments.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{label} cannot be empty"),
        ));
    }
    Ok(segments)
}

fn drawer_files(data_dir: &Path, drawer_name: &str) -> DrawerFiles {
    DrawerFiles {
        data: data_dir.join(format!("{drawer_name}.drw")),
        index: data_dir.join(format!("{drawer_name}_index.drw")),
        meta: data_dir.join(format!("{drawer_name}_meta.drw")),
    }
}

fn file_size_or_zero(path: &Path) -> Result<u64> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

fn storage_breakdown(path: &Path) -> Result<StorageBreakdown> {
    let mut breakdown = StorageBreakdown::default();
    collect_storage_breakdown(path, &mut breakdown)?;
    Ok(breakdown)
}

fn collect_storage_breakdown(path: &Path, breakdown: &mut StorageBreakdown) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child_path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_storage_breakdown(&child_path, breakdown)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }

        let bytes = metadata.len();
        breakdown.total_bytes = breakdown.total_bytes.saturating_add(bytes);
        match storage_file_kind(&child_path) {
            StorageFileKind::Data => {
                breakdown.data_bytes = breakdown.data_bytes.saturating_add(bytes)
            }
            StorageFileKind::Index => {
                breakdown.index_bytes = breakdown.index_bytes.saturating_add(bytes)
            }
            StorageFileKind::Metadata => {
                breakdown.metadata_bytes = breakdown.metadata_bytes.saturating_add(bytes)
            }
            StorageFileKind::LogicalWal => {
                breakdown.logical_wal_bytes = breakdown.logical_wal_bytes.saturating_add(bytes)
            }
            StorageFileKind::TransactionWal => {
                breakdown.transaction_wal_bytes =
                    breakdown.transaction_wal_bytes.saturating_add(bytes)
            }
            StorageFileKind::Other => {
                breakdown.other_bytes = breakdown.other_bytes.saturating_add(bytes)
            }
        }
    }

    Ok(())
}

fn storage_file_kind(path: &Path) -> StorageFileKind {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return StorageFileKind::Other;
    };

    if file_name == ".wal" {
        return StorageFileKind::LogicalWal;
    }
    if file_name == "wardrobe.wal" {
        return StorageFileKind::TransactionWal;
    }
    if file_name.ends_with("_index.drw") {
        return StorageFileKind::Index;
    }
    if file_name.ends_with("_meta.drw") || file_name.ends_with(".wal.meta") {
        return StorageFileKind::Metadata;
    }
    if path.extension().and_then(|extension| extension.to_str()) == Some("drw") {
        return StorageFileKind::Data;
    }
    StorageFileKind::Other
}

fn check_entry(label: &str, path: &Path) -> Result<CheckEntry> {
    let metadata = fs::metadata(path);
    let (exists, bytes) = match metadata {
        Ok(metadata) => (
            true,
            if metadata.is_file() {
                Some(metadata.len())
            } else {
                None
            },
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => (false, None),
        Err(error) => return Err(error),
    };
    Ok(CheckEntry {
        label: label.to_string(),
        path: path.display().to_string(),
        exists,
        bytes,
    })
}

fn collect_drawer_names(root: &Path, current: &Path, drawers: &mut Vec<String>) -> Result<()> {
    if !current.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_drawer_names(root, &path, drawers)?;
            continue;
        }
        if !is_drawer_data_file(&path) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let parent = path.parent().unwrap_or(current);
        let relative_parent = parent.strip_prefix(root).unwrap_or(parent);
        let name = if relative_parent.as_os_str().is_empty() {
            stem.to_string()
        } else {
            format!("{}/{}", relative_path_string(relative_parent), stem)
        };
        drawers.push(name);
    }
    Ok(())
}

pub(crate) fn is_drawer_data_file(path: &Path) -> bool {
    if path.extension().and_then(|extension| extension.to_str()) != Some("drw") {
        return false;
    }
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    !stem.starts_with('.') && !stem.ends_with("_index") && !stem.ends_with("_meta")
}

pub(crate) fn relative_path_string(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str().map(ToOwned::to_owned),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}
