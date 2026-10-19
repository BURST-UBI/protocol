use super::{invalid, CertifiedCheckpoint, ReplayError};
use std::borrow::Cow;

/// Ordered, authenticated selections. Implementations must restore historical
/// parent authority before returning a certificate; scratch integrity alone
/// cannot authorize one. A missing row inside `len` is an error.
pub trait SelectionSource {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn read_selection(&self, position: usize) -> Result<Cow<'_, CertifiedCheckpoint>, ReplayError>;

    fn read_all(&self) -> impl Iterator<Item = Result<Cow<'_, CertifiedCheckpoint>, ReplayError>> {
        (0..self.len()).map(|position| self.read_selection(position))
    }

    fn find_selection(
        &self,
        mut matches: impl FnMut(&CertifiedCheckpoint) -> bool,
    ) -> Result<Option<Cow<'_, CertifiedCheckpoint>>, ReplayError> {
        for certificate in self.read_all() {
            let certificate = certificate?;
            if matches(&certificate) {
                return Ok(Some(certificate));
            }
        }
        Ok(None)
    }
}

impl SelectionSource for [&CertifiedCheckpoint] {
    fn len(&self) -> usize {
        <[_]>::len(self)
    }

    fn read_selection(&self, position: usize) -> Result<Cow<'_, CertifiedCheckpoint>, ReplayError> {
        <[_]>::get(self, position)
            .map(|certificate| Cow::Borrowed(*certificate))
            .ok_or_else(|| invalid("missing authenticated selection"))
    }
}

impl SelectionSource for [CertifiedCheckpoint] {
    fn len(&self) -> usize {
        <[_]>::len(self)
    }

    fn read_selection(&self, position: usize) -> Result<Cow<'_, CertifiedCheckpoint>, ReplayError> {
        <[_]>::get(self, position)
            .map(Cow::Borrowed)
            .ok_or_else(|| invalid("missing authenticated selection"))
    }
}

impl<T> SelectionSource for Vec<T>
where
    [T]: SelectionSource,
{
    fn len(&self) -> usize {
        Vec::len(self)
    }

    fn read_selection(&self, position: usize) -> Result<Cow<'_, CertifiedCheckpoint>, ReplayError> {
        SelectionSource::read_selection(self.as_slice(), position)
    }
}

impl<const N: usize> SelectionSource for [&CertifiedCheckpoint; N] {
    fn len(&self) -> usize {
        N
    }

    fn read_selection(&self, position: usize) -> Result<Cow<'_, CertifiedCheckpoint>, ReplayError> {
        SelectionSource::read_selection(self.as_slice(), position)
    }
}
