use std::fs::{self, File, OpenOptions};
use std::io::{self, Error, ErrorKind, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn to_io_error(error: impl std::fmt::Display) -> io::Error {
    Error::other(error.to_string())
}

pub(crate) fn chunk_ranges(total: usize, chunk_size: usize) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < total {
        let end = start.saturating_add(chunk_size).min(total);
        ranges.push((start, end));
        start = end;
    }
    ranges
}

pub(crate) fn repeated_shuffled_indices(total: usize, count: usize, seed: u64) -> Vec<usize> {
    if total == 0 || count == 0 {
        return Vec::new();
    }
    let mut order = (0..total).collect::<Vec<_>>();
    deterministic_shuffle(&mut order, seed);
    (0..count).map(|index| order[index % order.len()]).collect()
}

pub(crate) fn deterministic_range_bounds(
    count: usize,
    min_value: i64,
    max_value: i64,
    seed: u64,
) -> Vec<(i64, i64)> {
    if count == 0 || min_value > max_value {
        return Vec::new();
    }
    let span = (max_value - min_value + 1) as u64;
    let mut state = seed ^ ((count as u64) << 32);
    (0..count)
        .map(|_| {
            let low = min_value + (next_shuffle_u64(&mut state) % span) as i64;
            let width = (next_shuffle_u64(&mut state) % span) as i64;
            let high = (low + width).min(max_value);
            (low.min(high), high)
        })
        .collect()
}

pub(crate) fn shuffled_take(mut values: Vec<usize>, count: usize, seed: u64) -> Vec<usize> {
    deterministic_shuffle(&mut values, seed);
    values.truncate(count.min(values.len()));
    values
}

pub(crate) fn deterministic_shuffle(values: &mut [usize], seed: u64) {
    if values.len() <= 1 {
        return;
    }
    let mut state = seed ^ ((values.len() as u64) << 32);
    for index in (1..values.len()).rev() {
        let swap_with = (next_shuffle_u64(&mut state) as usize) % (index + 1);
        values.swap(index, swap_with);
    }
}

pub(crate) fn next_shuffle_u64(state: &mut u64) -> u64 {
    let mut value = *state;
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    *state = value;
    value
}

pub(crate) fn file_size_or_zero(path: impl AsRef<Path>) -> io::Result<u64> {
    match fs::metadata(path.as_ref()) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

pub(crate) fn sync_file_if_exists(path: &Path) -> io::Result<()> {
    let mut file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .or_else(|_| File::open(path))
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    match file.flush() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::PermissionDenied => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn fsync_tree(path: &Path) -> io::Result<()> {
    if path.is_file() {
        sync_file_if_exists(path)?;
        return Ok(());
    }
    if !path.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        if child.is_dir() {
            fsync_tree(&child)?;
        } else {
            sync_file_if_exists(&child)?;
        }
    }
    Ok(())
}

pub(crate) fn directory_size(path: impl AsRef<Path>) -> io::Result<u64> {
    let path = path.as_ref();
    if path.is_file() {
        return file_size_or_zero(path);
    }
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        if child.is_dir() {
            total = total.saturating_add(directory_size(child)?);
        } else {
            total = total.saturating_add(file_size_or_zero(child)?);
        }
    }
    Ok(total)
}

pub(crate) fn optional_count(value: Option<usize>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unavailable".to_string())
}

pub(crate) fn unix_timestamp_micros() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
}
