//! Length-prefixed v3/v4 authority records. The 16 MiB limit applies to each
//! frame, not the combined header; callers bound the full archive.
use super::*;
use crate::confirmed_replay::snapshot::installed::authority_history::AuthorityHistory;

pub(super) const COMPACT_MAGIC: &[u8; 16] = b"BURST-SNAPSHOT\0\x04";
pub(super) const MAGIC: &[u8; 16] = b"BURST-SNAPSHOT\0\x03";
type Beacons = Vec<(burst_types::BlockHash, burst_vrf::RandomOutput)>;

pub(super) fn frame_size(value: &impl Serialize) -> Result<u64, ReplayError> {
    let length = bincode::serialized_size(value).map_err(invalid)?;
    if length == 0 || length > MAX_HEADER_BYTES {
        return Err(invalid("snapshot record exceeds frame limit"));
    }
    Ok(length)
}

pub(super) fn size<A: AuthoritySource + ?Sized>(
    authority: &A,
    certificate: &SnapshotCertificate,
    beacons: &[(burst_types::BlockHash, burst_vrf::RandomOutput)],
) -> Result<u64, ReplayError> {
    let mut length = 16u64
        .checked_add(frame_size(&(certificate, beacons))?)
        .ok_or_else(|| invalid("snapshot header length overflow"))?;
    let mut count = 0usize;
    for step in authority.steps() {
        let step = step?;
        let bytes = frame_size(&step)?;
        length = length
            .checked_add(8)
            .and_then(|length| length.checked_add(bytes))
            .ok_or_else(|| invalid("snapshot authority frame exceeds limits"))?;
        count = count
            .checked_add(1)
            .ok_or_else(|| invalid("snapshot authority count overflow"))?;
    }
    if count != authority.count()? {
        return Err(invalid("snapshot authority source length changed"));
    }
    Ok(length)
}

fn write_frame(value: &impl Serialize, writer: &mut impl Write) -> Result<(), ReplayError> {
    writer
        .write_all(&frame_size(value)?.to_be_bytes())
        .map_err(invalid)?;
    bincode::serialize_into(writer, value).map_err(invalid)
}

pub(super) fn write<A: AuthoritySource + ?Sized>(
    authority: &A,
    certificate: &SnapshotCertificate,
    beacons: &[(burst_types::BlockHash, burst_vrf::RandomOutput)],
    writer: &mut impl Write,
) -> Result<(), ReplayError> {
    let expected = authority.count()?;
    writer
        .write_all(&(expected as u64).to_be_bytes())
        .map_err(invalid)?;
    let mut count = 0usize;
    for step in authority.steps() {
        write_frame(&step?, writer)?;
        count = count
            .checked_add(1)
            .ok_or_else(|| invalid("snapshot authority count overflow"))?;
    }
    if count != expected {
        return Err(invalid("snapshot authority source length changed"));
    }
    write_frame(&(certificate, beacons), writer)
}

pub(super) fn compact_size<A: AuthoritySource + ?Sized>(
    base: &CheckpointAnchor,
    authority: &A,
    certificate: &SnapshotCertificate,
    beacons: &[(burst_types::BlockHash, burst_vrf::RandomOutput)],
) -> Result<u64, ReplayError> {
    let mut length = 16u64
        .checked_add(frame_size(&(certificate, beacons))?)
        .ok_or_else(|| invalid("snapshot header length overflow"))?;
    let mut count = 0usize;
    for entry in authority.entries(base) {
        let bytes = frame_size(&entry?)?;
        length = length
            .checked_add(8)
            .and_then(|n| n.checked_add(bytes))
            .ok_or_else(|| invalid("snapshot authority frame exceeds limits"))?;
        count = count
            .checked_add(1)
            .ok_or_else(|| invalid("snapshot authority count overflow"))?;
    }
    if count != authority.count()? {
        return Err(invalid("snapshot authority source length changed"));
    }
    Ok(length)
}

pub(super) fn write_compact<A: AuthoritySource + ?Sized>(
    base: &CheckpointAnchor,
    authority: &A,
    certificate: &SnapshotCertificate,
    beacons: &[(burst_types::BlockHash, burst_vrf::RandomOutput)],
    writer: &mut impl Write,
) -> Result<(), ReplayError> {
    let expected = authority.count()?;
    writer
        .write_all(&(expected as u64).to_be_bytes())
        .map_err(invalid)?;
    let mut count = 0usize;
    for entry in authority.entries(base) {
        write_frame(&entry?, writer)?;
        count = count
            .checked_add(1)
            .ok_or_else(|| invalid("snapshot authority count overflow"))?;
    }
    if count != expected {
        return Err(invalid("snapshot authority source length changed"));
    }
    write_frame(&(certificate, beacons), writer)
}

fn read_number(reader: &mut impl Read) -> Result<u64, ReplayError> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes).map_err(invalid)?;
    Ok(u64::from_be_bytes(bytes))
}

fn read_frame<T: serde::de::DeserializeOwned>(reader: &mut impl Read) -> Result<T, ReplayError> {
    let length = read_number(reader)?;
    if length == 0 || length > MAX_HEADER_BYTES {
        return Err(invalid("snapshot record exceeds frame limit"));
    }
    let mut frame = reader.take(length);
    let value = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(length)
        .deserialize_from(&mut frame)
        .map_err(invalid)?;
    if frame.limit() != 0 {
        return Err(invalid("trailing snapshot record data"));
    }
    Ok(value)
}

pub(super) fn read(
    base: &CheckpointAnchor,
    reader: &mut impl Read,
    length: u64,
    directory: &std::path::Path,
) -> Result<(Authority, SnapshotCertificate, Beacons), ReplayError> {
    read_format(base, reader, length, directory, false)
}

pub(super) fn read_compact(
    base: &CheckpointAnchor,
    reader: &mut impl Read,
    length: u64,
    directory: &std::path::Path,
) -> Result<(Authority, SnapshotCertificate, Beacons), ReplayError> {
    read_format(base, reader, length, directory, true)
}

fn read_format(
    base: &CheckpointAnchor,
    reader: &mut impl Read,
    length: u64,
    directory: &std::path::Path,
    compact: bool,
) -> Result<(Authority, SnapshotCertificate, Beacons), ReplayError> {
    if length == 0 {
        return Err(invalid("empty snapshot header"));
    }
    let mut header = std::io::BufReader::with_capacity(HEADER_BUFFER_BYTES, reader.take(length));
    let count = read_number(&mut header)?;
    if count > length / 8 {
        return Err(invalid("snapshot authority count exceeds framing"));
    }
    let mut history = AuthorityHistory::new(base.clone(), directory).map_err(invalid)?;
    let mut parent = base.clone();
    for _ in 0..count {
        if compact {
            let entry: super::super::installed::authority_history::Entry = read_frame(&mut header)?;
            let certificate = entry.certificate().clone();
            let next = history.append_entry(entry).map_err(invalid)?;
            // Deltas cannot evade the legacy bound on a complete authority witness.
            frame_size(&(&certificate, next.roster().humans()))?;
            parent = next;
        } else {
            let step: SnapshotAuthorityStep = read_frame(&mut header)?;
            let next = step.authenticate(&parent).map_err(invalid)?;
            history.append(&step.certificate, &next).map_err(invalid)?;
            parent = next;
        }
    }
    let (certificate, beacons): (SnapshotCertificate, Beacons) = read_frame(&mut header)?;
    certificate.verify(&parent).map_err(invalid)?;
    validate_beacon_bounds(history.step_count().map_err(invalid)?, &beacons)?;
    if !header.buffer().is_empty() || header.get_ref().limit() != 0 {
        return Err(invalid("trailing snapshot header data"));
    }
    Ok((Authority::File(Box::new(history)), certificate, beacons))
}
