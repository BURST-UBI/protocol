use super::*;

struct Segments<'a>(&'a burst_brn::RateHistory);
impl Field for Segments<'_> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_at(
        &self,
        mut output: &mut dyn Write,
        _: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        check_depth(depth)?;
        let mut output = BoundedWriter {
            output: &mut output,
            written: 0,
            limit: MAX_SNAPSHOT_BYTES as usize,
        };
        collection_header(0x80, self.0.segments().len(), &mut output)?;
        for segment in self.0.segments().iter() {
            encode_at_depth(segment, &mut output, depth + 1)?;
        }
        Ok(())
    }
}

/// Encode stored segments in their existing order, including their end times.
/// Canonical encoding does not validate the accrual history.
pub(in crate::confirmed_replay::snapshot) struct RateHistory<'a>(pub &'a burst_brn::RateHistory);
impl Field for RateHistory<'_> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_in(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
    ) -> Result<(), ReplayError> {
        self.encode_field_at(output, directory, 1)
    }
    fn encode_field_at(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        self.encode(output, directory, depth, false)
    }
}
impl RateHistory<'_> {
    fn encode(
        &self,
        mut output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
        preflight: bool,
    ) -> Result<(), ReplayError> {
        check_depth(depth)?;
        write_bounded(self.0, &mut std::io::sink(), MAX_SNAPSHOT_BYTES as usize)?;
        record_at_depth(
            &[("segments", &Segments(self.0))],
            &mut output,
            directory,
            depth,
            preflight,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_brn::{RateHistory as History, RateSegment};
    use burst_types::Timestamp;

    #[test]
    fn streamed_rate_history_preserves_legacy_segments_and_depth() {
        for count in [0, 1, 4097] {
            // Encoding retains raw stored order and ends even when accrual indexing rejects them.
            let segments: Vec<_> = (0..count)
                .map(|index| RateSegment {
                    rate: if index % 2 == 0 {
                        u128::MAX
                    } else {
                        index as u128
                    },
                    start: Timestamp::new((count - index) as u64),
                    end: if index % 3 == 0 {
                        None
                    } else {
                        Some(Timestamp::new(0))
                    },
                })
                .collect();
            let state = History::from_segments(segments.clone());
            #[derive(Serialize)]
            struct Stored<'a> {
                segments: &'a [RateSegment],
            }
            let oracle = Stored {
                segments: &segments,
            };
            let mut bytes = Vec::new();
            RateHistory(&state)
                .encode(&mut bytes, None, 0, true)
                .unwrap();
            assert_eq!(bytes, encode(&oracle).unwrap());
            assert_eq!(bytes, encode(&state).unwrap());
            #[derive(Serialize)]
            struct Outer<'a> {
                brn_rates: &'a History,
            }
            let mut nested = Vec::new();
            encode_record_in(&[("brn_rates", &RateHistory(&state))], &mut nested, None).unwrap();
            assert_eq!(nested, encode(&Outer { brn_rates: &state }).unwrap());
            for depth in 60..=65 {
                let mut old_bytes = Vec::new();
                let old = encode_at_depth(&state, &mut old_bytes, depth);
                let mut new_bytes = Vec::new();
                let new = RateHistory(&state).encode(&mut new_bytes, None, depth, true);
                assert_eq!(old.is_ok(), new.is_ok(), "count {count},depth {depth}");
                if old.is_ok() {
                    assert_eq!(old_bytes, new_bytes);
                } else {
                    assert!(new_bytes.is_empty());
                }
            }
            let decoded: History = ciborium::from_reader(bytes.as_slice()).unwrap();
            assert_eq!(
                decoded.segments().iter().collect::<Vec<_>>(),
                segments.iter().collect::<Vec<_>>()
            );
        }
    }
}
