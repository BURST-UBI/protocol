use std::io::{self, Write};

/// Compare an encoded snapshot without retaining a second encoded body.
pub(crate) fn matches(
    bytes: &[u8],
    encode: impl FnOnce(&mut dyn Write) -> Result<(), bincode::Error>,
) -> Result<bool, bincode::Error> {
    struct Compare<'a> {
        remaining: &'a [u8],
        equal: bool,
    }
    impl Write for Compare<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining.get(..bytes.len()) != Some(bytes) {
                self.equal = false;
            }
            self.remaining = self.remaining.get(bytes.len()..).unwrap_or_default();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Compare {
        remaining: bytes,
        equal: true,
    };
    encode(&mut writer)?;
    Ok(writer.equal && writer.remaining.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_checks_content_length_and_encoder_errors() {
        for chunks in [vec![b"abcdef".as_slice()], vec![b"ab", b"cd", b"ef"]] {
            for expected in [b"abcdef".as_slice(), b"abcdeg", b"abcde", b"abcdefg", b""] {
                assert_eq!(
                    matches(expected, |writer| {
                        for chunk in &chunks {
                            writer.write_all(chunk)?;
                        }
                        Ok(())
                    })
                    .unwrap(),
                    expected == b"abcdef"
                );
            }
        }
        assert!(matches(b"", |_| Ok(())).unwrap());
        assert!(!matches(b"a", |_| Ok(())).unwrap());
        assert!(matches(b"abc", |writer| {
            writer.write_all(b"a")?;
            Err(Box::new(bincode::ErrorKind::Custom(
                "injected encoder failure".into(),
            )))
        })
        .is_err());
    }
}
