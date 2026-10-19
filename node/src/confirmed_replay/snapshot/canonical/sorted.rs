use super::*;
use std::{
    fs::File,
    io::{BufReader, BufWriter, Read, Seek, SeekFrom},
};

const KEY_BYTES: usize = 1024 * 1024;
const BUFFER_BYTES: usize = 16 * 1024;
type Entry = (Vec<u8>, (u64, u64));

#[derive(Clone, Copy)]
struct Scratch<'a>(Option<&'a std::path::Path>);

impl Scratch<'_> {
    fn file(self) -> Result<File, ReplayError> {
        match self.0 {
            Some(directory) => tempfile::tempfile_in(directory),
            None => tempfile::tempfile(),
        }
        .map_err(error)
    }
}

fn error(error: impl ToString) -> ReplayError {
    invalid(error.to_string())
}

/// Large maps sort bounded key batches in temporary files. Binary carry merging keeps one
/// temporary run per level rather than one open file per batch.
pub(super) fn encode<K: Serialize + Eq + std::hash::Hash, V: Serialize>(
    map: &std::collections::HashMap<K, V>,
    output: &mut dyn Write,
    directory: Option<&std::path::Path>,
    encode_value: impl Fn(&V, &mut dyn Write) -> Result<(), ReplayError>,
) -> Result<(), ReplayError> {
    encode_with_budget(map, output, encode_value, KEY_BYTES, Scratch(directory))
}

fn encode_with_budget<K: Serialize + Eq + std::hash::Hash, V: Serialize>(
    map: &std::collections::HashMap<K, V>,
    mut output: &mut dyn Write,
    encode_value: impl Fn(&V, &mut dyn Write) -> Result<(), ReplayError>,
    key_budget: usize,
    scratch: Scratch<'_>,
) -> Result<(), ReplayError> {
    let mut levels: Vec<Option<File>> = Vec::new();
    let mut entries: Vec<Entry> = Vec::new();
    let mut data = BufWriter::with_capacity(BUFFER_BYTES, scratch.file()?);
    let mut keys = 0usize;
    let mut offset = 0u64;
    let mut total = 0usize;
    for (key, value) in map {
        let mut key_bytes = Vec::new();
        encode_at_depth(key, &mut key_bytes, 2)?;
        total = total
            .checked_add(key_bytes.len())
            .filter(|bytes| *bytes <= MAX_SNAPSHOT_BYTES as usize)
            .ok_or_else(|| invalid("snapshot exceeds byte limit"))?;
        let mut writer = Bounded {
            output: &mut data,
            written: 0,
            limit: MAX_SNAPSHOT_BYTES as usize - total,
        };
        encode_value(value, &mut writer)?;
        let length = writer.written;
        total += length;
        keys += key_bytes.len();
        entries.push((key_bytes, (offset, length as u64)));
        offset += length as u64;
        if keys >= key_budget {
            let run = sorted_run(std::mem::take(&mut entries), data, scratch)?;
            push_run(&mut levels, run, scratch)?;
            data = BufWriter::with_capacity(BUFFER_BYTES, scratch.file()?);
            keys = 0;
            offset = 0;
        }
    }
    if !entries.is_empty() {
        push_run(&mut levels, sorted_run(entries, data, scratch)?, scratch)?;
    }
    let mut combined = None;
    for file in levels.into_iter().flatten() {
        combined = Some(match combined {
            None => file,
            Some(previous) => merge(previous, file, scratch)?,
        });
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
    if let Some(file) = combined {
        let mut run = Run::new(file)?;
        while run.key.is_some() {
            run.copy_row(&mut output, false)?;
        }
    }
    Ok(())
}

fn sorted_run(
    mut entries: Vec<Entry>,
    data: BufWriter<File>,
    scratch: Scratch<'_>,
) -> Result<File, ReplayError> {
    sort_entries(&mut entries)?;
    let mut data = data.into_inner().map_err(error)?;
    let mut output = BufWriter::with_capacity(BUFFER_BYTES, scratch.file()?);
    for (key, (offset, length)) in entries {
        write_header(&mut output, &key, length)?;
        data.seek(SeekFrom::Start(offset)).map_err(error)?;
        copy_exact(&mut data, &mut output, length)?;
    }
    output.into_inner().map_err(error)
}

fn push_run(
    levels: &mut Vec<Option<File>>,
    mut file: File,
    scratch: Scratch<'_>,
) -> Result<(), ReplayError> {
    for slot in levels.iter_mut() {
        match slot.take() {
            Some(previous) => file = merge(previous, file, scratch)?,
            None => {
                *slot = Some(file);
                return Ok(());
            }
        }
    }
    levels.push(Some(file));
    Ok(())
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
    length: u64,
) -> Result<(), ReplayError> {
    let copied = std::io::copy(&mut input.take(length), output).map_err(error)?;
    if copied != length {
        return Err(invalid("truncated snapshot sort run"));
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

    fn copy_row(&mut self, output: &mut impl Write, framed: bool) -> Result<(), ReplayError> {
        let key = self
            .key
            .as_ref()
            .ok_or_else(|| invalid("snapshot sort run exhausted"))?;
        if framed {
            write_header(output, key, self.length)?;
        } else {
            output.write_all(key).map_err(error)?;
        }
        copy_exact(&mut self.reader, output, self.length)?;
        self.next()
    }
}

fn merge(first: File, second: File, scratch: Scratch<'_>) -> Result<File, ReplayError> {
    let mut first = Run::new(first)?;
    let mut second = Run::new(second)?;
    let mut output = BufWriter::with_capacity(BUFFER_BYTES, scratch.file()?);
    loop {
        let select_first = match (&first.key, &second.key) {
            (Some(a), Some(b)) => match a.len().cmp(&b.len()).then_with(|| a.cmp(b)) {
                std::cmp::Ordering::Less => true,
                std::cmp::Ordering::Greater => false,
                std::cmp::Ordering::Equal => return Err(invalid("duplicate snapshot map key")),
            },
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if select_first {
            first.copy_row(&mut output, true)?;
        } else {
            second.copy_row(&mut output, true)?;
        }
    }
    output.into_inner().map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn spilled_maps_and_multi_level_runs_preserve_canonical_bytes() {
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
    fn truncated_sort_runs_and_output_failures_propagate() {
        let mut file = tempfile::tempfile().unwrap();
        write_header(&mut file, &[1], 10).unwrap();
        let mut run = Run::new(file).unwrap();
        assert!(run
            .copy_row(&mut std::io::sink(), false)
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
