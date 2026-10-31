use crate::confirmed_replay::scratch_file::ScratchFile as File;
use super::*;
use std::{
    io::{BufReader, BufWriter, Read, Seek, SeekFrom},
    cmp::Reverse,
    collections::BinaryHeap,
};

const KEY_BYTES: usize = 1024 * 1024;
const BUFFER_BYTES: usize = 16 * 1024;
type Entry = (Vec<u8>, (u64, u64));

#[derive(Clone, Copy)]
struct Scratch<'a>(Option<&'a std::path::Path>);

impl Scratch<'_> {
    fn file(self) -> Result<File, ReplayError> {
        match self.0 {
            Some(directory) => File::new_in(directory),
            None => File::new(),
        }
        .map_err(error)
    }
}

fn error(error: impl ToString) -> ReplayError {
    invalid(error.to_string())
}

/// Sort large maps in disk runs, flushing after accumulated key bytes reach
/// 1 MiB. One key can exceed that threshold; descriptors and merge buffers add
/// memory overhead. Values and runs are written once per call, then merged by
/// key length and bytes. A caller preflight may invoke this whole pass again.
pub(super) fn encode<M: MapSource>(
    map: &M,
    output: &mut dyn Write,
    directory: Option<&std::path::Path>,
    encode_key: impl Fn(&M::Key, &mut Vec<u8>) -> Result<(), ReplayError>,
    encode_value: impl Fn(&M::Value, &mut dyn Write) -> Result<(), ReplayError>,
) -> Result<(), ReplayError> {
    encode_sorted(map, output, encode_value, KEY_BYTES, Scratch(directory), encode_key)
}

#[cfg(test)]
fn encode_with_budget<M: MapSource>(
    map: &M, output: &mut dyn Write,
    encode_value: impl Fn(&M::Value, &mut dyn Write) -> Result<(), ReplayError>,
    key_budget: usize, scratch: Scratch<'_>,
) -> Result<(), ReplayError> {
    encode_sorted(map, output, encode_value, key_budget, scratch,
        |key, output| encode_at_depth(key, output, 2))
}

fn encode_sorted<M: MapSource>(
    map: &M,
    mut output: &mut dyn Write,
    encode_value: impl Fn(&M::Value, &mut dyn Write) -> Result<(), ReplayError>,
    key_budget: usize,
    scratch: Scratch<'_>,
    encode_key: impl Fn(&M::Key, &mut Vec<u8>) -> Result<(), ReplayError>,
) -> Result<(), ReplayError> {
    if key_budget == 0 { return Err(invalid("zero snapshot key budget")); }
    let max_runs = (MAX_SNAPSHOT_BYTES as usize).div_ceil(key_budget);
    let mut files = Vec::new();
    let mut entries: Vec<Entry> = Vec::new();
    let mut data = BufWriter::with_capacity(BUFFER_BYTES, scratch.file()?);
    let mut key_bytes_used = 0usize;
    let mut offset = 0u64;
    let mut total = 0usize;
    for (key, value) in map.entries() {
        let mut key_bytes = Vec::new();
        encode_key(key, &mut key_bytes)?;
        total = total
            .checked_add(key_bytes.len())
            .filter(|bytes| *bytes <= MAX_SNAPSHOT_BYTES as usize)
            .ok_or_else(|| invalid("snapshot exceeds byte limit"))?;
        let mut writer = BoundedWriter {
            output: &mut data,
            written: 0,
            limit: MAX_SNAPSHOT_BYTES as usize - total,
        };
        encode_value(value, &mut writer)?;
        let length = writer.written;
        total += length;
        key_bytes_used += key_bytes.len();
        entries.push((key_bytes, (offset, length as u64)));
        offset += length as u64;
        if key_bytes_used >= key_budget {
            let run = sorted_run(std::mem::take(&mut entries), scratch)?;
            if files.len() == max_runs { return Err(invalid("snapshot sort run count exceeds byte bound")); }
            files.push(run);
            key_bytes_used = 0;
        }
    }
    if !entries.is_empty() {
        if files.len() == max_runs { return Err(invalid("snapshot sort run count exceeds byte bound")); }
        files.push(sorted_run(entries, scratch)?);
    }
    let mut data = data.into_inner().map_err(error)?;
    let mut runs = files.into_iter().map(Run::new)
        .collect::<Result<Vec<_>, _>>()?;
    // Validate all keys and locations before emitting bytes, including duplicates
    // across runs. Rewind the immutable runs instead of writing a final run.
    visit_locations(&mut runs, offset, |_, _, _| Ok(()))?;
    for run in &mut runs {
        run.reader.rewind().map_err(error)?;
        run.next()?;
    }
    let mut header = Vec::new();
    collection_header(0xa0, map.len(), &mut header)?;
    if total
        .checked_add(header.len())
        .is_none_or(|bytes| bytes > MAX_SNAPSHOT_BYTES as usize)
    {
        return Err(invalid("snapshot exceeds byte limit"));
    }
    output.write_all(&header).map_err(error)?;
    let mut copy_buffer = [0; BUFFER_BYTES];
    visit_locations(&mut runs, offset, |key, start, length| {
        output.write_all(key).map_err(error)?;
        data.seek(SeekFrom::Start(start)).map_err(error)?;
        copy_exact(&mut data, &mut output, length, &mut copy_buffer)
    })?;
    Ok(())
}

// One head per run gives O(n log runs) heap operations; key comparisons also
// depend on key length. Production permits at most 64 runs with 1 MiB total
// read buffers, excluding heap keys and other allocations.
fn visit_locations(
    runs: &mut [Run],
    data_bytes: u64,
    mut visit: impl FnMut(&[u8], u64, u64) -> Result<(), ReplayError>,
) -> Result<(), ReplayError> {
    let mut heads = BinaryHeap::new();
    for (index, run) in runs.iter_mut().enumerate() {
        if let Some(key) = run.key.take() { heads.push(Reverse((key.len(), key, index))); }
    }
    let mut previous: Option<Vec<u8>> = None;
    while let Some(Reverse((_, key, index))) = heads.pop() {
        if let Some(previous) = &previous {
            match previous.len().cmp(&key.len()).then_with(|| previous.cmp(&key)) {
                std::cmp::Ordering::Equal => return Err(invalid("duplicate snapshot map key")),
                std::cmp::Ordering::Greater => return Err(invalid("unordered snapshot sort run")),
                std::cmp::Ordering::Less => {}
            }
        }
        let run = &mut runs[index];
        if run.length != 16 {
            return Err(invalid("invalid snapshot value location"));
        }
        let mut location = [0; 16];
        run.reader.read_exact(&mut location).map_err(error)?;
        let start = u64::from_be_bytes(location[..8].try_into().map_err(error)?);
        let length = u64::from_be_bytes(location[8..].try_into().map_err(error)?);
        if start.checked_add(length).is_none_or(|end| end > data_bytes) {
            return Err(invalid("snapshot value location exceeds staged data"));
        }
        visit(&key, start, length)?;
        previous = Some(key);
        run.next()?;
        if let Some(key) = run.key.take() { heads.push(Reverse((key.len(), key, index))); }
    }
    Ok(())
}

fn sorted_run(
    mut entries: Vec<Entry>,
    scratch: Scratch<'_>,
) -> Result<File, ReplayError> {
    sort_entries(&mut entries)?;
    let mut output = BufWriter::with_capacity(BUFFER_BYTES, scratch.file()?);
    for (key, (offset, length)) in entries {
        write_header(&mut output, &key, 16)?;
        output.write_all(&offset.to_be_bytes()).map_err(error)?;
        output.write_all(&length.to_be_bytes()).map_err(error)?;
    }
    output.into_inner().map_err(error)
}

fn write_header(output: &mut impl Write, key: &[u8], length: u64) -> Result<(), ReplayError> {
    output
        .write_all(&(key.len() as u64).to_be_bytes())
        .map_err(error)?;
    output.write_all(&length.to_be_bytes()).map_err(error)?;
    output.write_all(key).map_err(error)
}

fn copy_exact(
    input: &mut impl Read,
    output: &mut impl Write,
    mut length: u64,
    buffer: &mut [u8; BUFFER_BYTES],
) -> Result<(), ReplayError> {
    while length != 0 {
        let capacity = length.min(buffer.len() as u64) as usize;
        let read = match input.read(&mut buffer[..capacity]) {
            Ok(0) => return Err(invalid("truncated snapshot sort run")),
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(error(err)),
        };
        output.write_all(&buffer[..read]).map_err(error)?;
        length -= read as u64;
    }
    Ok(())
}

struct Run {
    reader: BufReader<File>,
    key: Option<Vec<u8>>,
    length: u64,
}

impl Run {
    fn new(mut file: File) -> Result<Self, ReplayError> {
        file.rewind().map_err(error)?;
        let mut run = Self {
            reader: BufReader::with_capacity(BUFFER_BYTES, file),
            key: None,
            length: 0,
        };
        run.next()?;
        Ok(run)
    }

    fn next(&mut self) -> Result<(), ReplayError> {
        let mut header = [0; 16];
        if self.reader.read(&mut header[..1]).map_err(error)? == 0 {
            self.key = None;
            return Ok(());
        }
        self.reader.read_exact(&mut header[1..]).map_err(error)?;
        let key_bytes = u64::from_be_bytes(header[..8].try_into().map_err(error)?);
        self.length = u64::from_be_bytes(header[8..].try_into().map_err(error)?);
        if key_bytes == 0 || key_bytes > MAX_SNAPSHOT_BYTES || self.length > MAX_SNAPSHOT_BYTES {
            return Err(invalid("invalid snapshot sort run length"));
        }
        let mut key = vec![0; key_bytes as usize];
        self.reader.read_exact(&mut key).map_err(error)?;
        self.key = Some(key);
        Ok(())
    }

    #[cfg(test)]
    fn copy_row(&mut self, output: &mut impl Write, framed: bool,
        buffer: &mut [u8; BUFFER_BYTES]) -> Result<(), ReplayError> {
        let key = self
            .key
            .as_ref()
            .ok_or_else(|| invalid("snapshot sort run exhausted"))?;
        if framed {
            write_header(output, key, self.length)?;
        } else {
            output.write_all(key).map_err(error)?;
        }
        copy_exact(&mut self.reader, output, self.length, buffer)?;
        self.next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reusable_copy_buffer_preserves_boundaries_and_io_errors() {
        struct ShortReads<'a> { bytes: &'a [u8], interrupt: bool }
        impl Read for ShortReads<'_> {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                if self.interrupt {
                    self.interrupt = false;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                let size = output.len().min(7);
                self.bytes.read(&mut output[..size])
            }
        }
        let bytes: Vec<u8> = (0..BUFFER_BYTES * 2 + 17).map(|n| n as u8).collect();
        let mut input = ShortReads { bytes: &bytes, interrupt: true };
        let mut buffer = [0; BUFFER_BYTES];
        let mut output = Vec::new();
        for length in [0, 3, BUFFER_BYTES as u64 + 1] {
            copy_exact(&mut input, &mut output, length, &mut buffer).unwrap();
        }
        assert_eq!(output, bytes[..BUFFER_BYTES + 4]);
        assert_eq!(input.bytes, &bytes[BUFFER_BYTES + 4..]);
        let remaining = input.bytes.len() as u64;
        assert!(copy_exact(&mut input, &mut output, remaining + 1, &mut buffer)
            .unwrap_err().to_string().contains("truncated snapshot sort run"));
        assert_eq!(output, bytes);
        let mut short = [0; 2];
        assert!(copy_exact(&mut bytes.as_slice(), &mut short.as_mut_slice(), 3,
            &mut buffer).is_err());
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected read failure"))
            }
        }
        assert!(copy_exact(&mut Broken, &mut output, 1, &mut buffer)
            .unwrap_err().to_string().contains("injected read failure"));
    }

    #[test]
    fn configured_scratch_is_used_without_fallback_and_cleans_up() {
        let map: std::collections::HashMap<_, _> = (0..5000u32).map(|key| (key, key)).collect();
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing");
        let mut output = Vec::new();
        assert!(super::super::encode_record_in(
            &[("map", &Map(&map))],
            &mut output,
            Some(&missing)
        )
        .is_err());
        assert!(output.is_empty(), "scratch failure must fail preflight");
        let expected = super::super::encode(&map).unwrap();
        encode_with_budget(
            &map,
            &mut output,
            |value, mut output| encode_at_depth(value, &mut output, 2),
            64,
            Scratch(Some(directory.path())),
        )
        .unwrap();
        assert_eq!(output, expected);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        let mut short = [0; 1];
        assert!(encode_with_budget(
            &map,
            &mut short.as_mut_slice(),
            |value, mut output| encode_at_depth(value, &mut output, 2),
            64,
            Scratch(Some(directory.path())),
        )
        .is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn merge_passes_store_large_values_once_within_shared_budget() {
        use crate::confirmed_replay::scratch_file::ScratchLimit;
        let map: std::collections::HashMap<_, _> = (0..2048u32)
            .map(|key| (key, format!("{key:04}") + &"x".repeat(1020))).collect();
        let expected = super::super::encode(&map).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 3 << 20).unwrap();
        let mut actual = Vec::new();
        encode_with_budget(&map, &mut actual,
            |value, mut output| encode_at_depth(value, &mut output, 2),
            64, Scratch(Some(directory.path()))).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
        drop(limit);
        let limit = ScratchLimit::acquire(directory.path(), 1 << 20).unwrap();
        let mut rejected = Vec::new();
        assert!(encode_with_budget(&map, &mut rejected,
            |value, mut output| encode_at_depth(value, &mut output, 2),
            64, Scratch(Some(directory.path()))).is_err());
        assert!(rejected.is_empty());
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    #[test]
    fn spilled_maps_and_multiple_runs_preserve_canonical_bytes() {
        for count in [0, 1, 23, 24, 255, 256, 5000] {
            let map: std::collections::HashMap<_, _> = (0..count)
                .rev()
                .map(|key| (key, (u128::MAX, key)))
                .collect();
            let expected = super::super::encode(&map).unwrap();
            let mut actual = Vec::new();
            encode_with_budget(
                &map,
                &mut actual,
                |value, mut output| encode_at_depth(value, &mut output, 2),
                64,
                Scratch(None),
            )
            .unwrap();
            assert_eq!(actual, expected);
            let mut integrated = Vec::new();
            Map(&map).encode_field(&mut integrated).unwrap();
            assert_eq!(integrated, expected);
        }
    }

    #[test]
    fn spilled_maps_reject_cross_run_duplicates_and_value_failures_before_output() {
        #[derive(Eq, PartialEq, Hash)]
        struct Key(u8);
        impl Serialize for Key {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_u8(1)
            }
        }
        let map = std::collections::HashMap::from([(Key(1), 1u8), (Key(2), 2u8)]);
        let mut output = Vec::new();
        assert!(encode_with_budget(
            &map,
            &mut output,
            |value, mut output| encode_at_depth(value, &mut output, 2),
            1,
            Scratch(None)
        )
        .unwrap_err()
        .to_string()
        .contains("duplicate snapshot map key"));
        assert!(output.is_empty());
        assert!(encode_with_budget(
            &map,
            &mut output,
            |_, _| Err(invalid("injected value failure")),
            1,
            Scratch(None)
        )
        .is_err());
        assert!(output.is_empty());
    }

    #[test]
    fn final_runs_stream_within_budget_and_reject_duplicates_before_output() {
        use crate::confirmed_replay::scratch_file::ScratchLimit;
        // Four key runs and their value storage must fit within 18 KiB.
        let map: std::collections::BTreeMap<_, _> = (0..4u32)
            .map(|key| (format!("{key:04}{}", "x".repeat(4092)), key)).collect();
        struct Source(std::collections::BTreeMap<String, u32>);
        impl MapSource for Source {
            type Key = String;
            type Value = u32;
            fn len(&self) -> usize { self.0.len() }
            fn entries(&self) -> impl Iterator<Item = (&String, &u32)> { self.0.iter() }
        }
        let expected = super::super::encode(&map).unwrap();
        let map = Source(map);
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 18 * 1024).unwrap();
        let mut actual = Vec::new();
        encode_with_budget(&map, &mut actual,
            |value, mut output| encode_at_depth(value, &mut output, 2),
            4096, Scratch(Some(directory.path()))).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
        let mut short = vec![0; expected.len() - 1];
        assert!(encode_with_budget(&map, &mut short.as_mut_slice(),
            |value, mut output| encode_at_depth(value, &mut output, 2),
            4096, Scratch(Some(directory.path()))).is_err());
        assert_eq!(&short[..], &expected[..short.len()]);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);


        struct Duplicate;
        impl MapSource for Duplicate {
            type Key = u8;
            type Value = u8;
            fn len(&self) -> usize { 3 }
            fn entries(&self) -> impl Iterator<Item = (&u8, &u8)> {
                static ENTRIES: [(u8, u8); 3] = [(1, 0), (2, 0), (1, 0)];
                ENTRIES.iter().map(|(key, value)| (key, value))
            }
        }
        let mut rejected = Vec::new();
        assert!(encode_with_budget(&Duplicate, &mut rejected,
            |value, mut output| encode_at_depth(value, &mut output, 2),
            1, Scratch(Some(directory.path()))).unwrap_err().to_string()
            .contains("duplicate snapshot map key"));
        assert!(rejected.is_empty());
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    #[test]
    fn final_location_preflight_rejects_invalid_and_truncated_offsets() {
        for (length, location) in [
            (15, vec![0; 15]),
            (16, vec![0; 8]),
            (16, [u64::MAX.to_be_bytes(), 1u64.to_be_bytes()].concat()),
            (16, [0u64.to_be_bytes(), 2u64.to_be_bytes()].concat()),
        ] {
            let mut file = File::new().unwrap();
            write_header(&mut file, &[1], length).unwrap();
            file.write_all(&location).unwrap();
            let mut runs = [Run::new(file).unwrap()];
            let mut visited = false;
            assert!(visit_locations(&mut runs, 1, |_, _, _| {
                visited = true;
                Ok(())
            }).is_err());
            assert!(!visited);
        }
        let mut file = File::new().unwrap();
        for key in [2, 1] {
            write_header(&mut file, &[key], 16).unwrap();
            file.write_all(&[0; 16]).unwrap();
        }
        let mut runs = [Run::new(file).unwrap()];
        assert!(visit_locations(&mut runs, 0, |_, _, _| Ok(()))
            .unwrap_err().to_string().contains("unordered snapshot sort run"));
    }

    #[test]
    fn truncated_sort_runs_and_output_failures_propagate() {
        let mut file = File::new().unwrap();
        write_header(&mut file, &[1], 10).unwrap();
        let mut run = Run::new(file).unwrap();
        assert!(run
            .copy_row(&mut std::io::sink(), false, &mut [0; BUFFER_BYTES])
            .unwrap_err()
            .to_string()
            .contains("truncated snapshot sort run"));
        let map = std::collections::HashMap::from([(1u8, 2u8)]);
        let mut short = [0; 1];
        assert!(encode_with_budget(
            &map,
            &mut short.as_mut_slice(),
            |value, mut output| encode_at_depth(value, &mut output, 2),
            1,
            Scratch(None)
        )
        .is_err());
    }
}
