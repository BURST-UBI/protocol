use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PanelState {
    position: usize,
    checkpoint: BlockHash,
    pub(super) selected: Option<HashSet<WalletAddress>>,
    pub(super) votes: HashMap<WalletAddress, BlockHash>,
    pub(super) defaults: HashSet<WalletAddress>,
}

pub(super) struct Panel<'a> {
    pub(super) certificate: Certificate<'a>,
    state: PanelState,
}

#[derive(Clone)]
pub(super) enum Certificate<'a> {
    Borrowed(&'a CertifiedCheckpoint),
    Shared(std::sync::Arc<CertifiedCheckpoint>),
}

impl<'a> From<std::borrow::Cow<'a, CertifiedCheckpoint>> for Certificate<'a> {
    fn from(value: std::borrow::Cow<'a, CertifiedCheckpoint>) -> Self {
        match value {
            std::borrow::Cow::Borrowed(value) => Self::Borrowed(value),
            std::borrow::Cow::Owned(value) => Self::Shared(std::sync::Arc::new(value)),
        }
    }
}

impl AsRef<CertifiedCheckpoint> for Certificate<'_> {
    fn as_ref(&self) -> &CertifiedCheckpoint {
        match self {
            Self::Borrowed(value) => value,
            Self::Shared(value) => value,
        }
    }
}

impl std::ops::Deref for Certificate<'_> {
    type Target = CertifiedCheckpoint;
    fn deref(&self) -> &Self::Target { self.as_ref() }
}

impl std::ops::Deref for Panel<'_> {
    type Target = PanelState;
    fn deref(&self) -> &PanelState {
        &self.state
    }
}
impl std::ops::DerefMut for Panel<'_> {
    fn deref_mut(&mut self) -> &mut PanelState {
        &mut self.state
    }
}

impl Panel<'_> {
    pub(super) fn complete(&self) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|selected| selected.len() == self.votes.len() + self.defaults.len())
    }
    pub(super) fn vote_hashes(&self) -> Vec<BlockHash> {
        let mut hashes: Vec<_> = self.votes.values().copied().collect();
        hashes.sort_by_key(|hash| *hash.as_bytes());
        hashes
    }
}

enum Records {
    Memory(HashMap<BlockHash, PanelState>),
    Disk(crate::confirmed_replay::scratch_records::ScratchRecords<PanelState>),
}

pub(super) struct Panels<'a, S: SelectionSource + ?Sized> {
    certificates: &'a S,
    records: Records,
    // Cache the last certificate by source position. Each lookup still reads
    // the panel row and checks its checkpoint and context. The source must keep
    // positions stable; durable-history checks remain the caller's responsibility.
    last_certificate: std::sync::Mutex<Option<(usize, Certificate<'a>)>>,
}

impl<'a, S: SelectionSource + ?Sized> Panels<'a, S> {
    pub(super) fn new(
        certificates: &'a S,
        directory: Option<&std::path::Path>,
    ) -> Result<Self, ReplayError> {
        let records = match directory {
            Some(directory) => Records::Disk(
                crate::confirmed_replay::scratch_records::ScratchRecords::new(directory)
                    .map_err(|error| invalid(error.to_string()))?,
            ),
            None => Records::Memory(HashMap::new()),
        };
        Ok(Self {
            certificates,
            records,
            last_certificate: std::sync::Mutex::new(None),
        })
    }

    pub(super) fn get(&self, context: &BlockHash) -> Result<Option<Panel<'a>>, ReplayError> {
        let state = match &self.records {
            Records::Memory(records) => records.get(context).cloned(),
            Records::Disk(records) => records
                .get(context.as_bytes())
                .map_err(|error| invalid(error.to_string()))?,
        };
        let Some(state) = state else {
            return Ok(None);
        };
        let cached = self
            .last_certificate
            .lock()
            .map_err(|error| invalid(error.to_string()))?
            .as_ref()
            .filter(|(position, _)| *position == state.position)
            .map(|(_, certificate)| certificate.clone());
        let cache_miss = cached.is_none();
        let certificate = match cached {
            Some(certificate) => certificate,
            None => self.certificates.read_selection(state.position)?.into(),
        };
        if certificate.certificate().checkpoint.hash() != state.checkpoint
            || certificate
                .replayed()
                .request()
                .vote_context(&certificate.certificate().checkpoint)
                .map_err(|error| invalid(error.to_string()))?
                != *context
        {
            return Err(invalid("stored panel differs from authenticated selection"));
        }
        if cache_miss {
            *self
                .last_certificate
                .lock()
                .map_err(|error| invalid(error.to_string()))? =
                Some((state.position, certificate.clone()));
        }
        Ok(Some(Panel { certificate, state }))
    }

    pub(super) fn require(&self, context: &BlockHash) -> Result<Panel<'a>, ReplayError> {
        self.get(context)?
            .ok_or_else(|| invalid("missing certified panel"))
    }

    fn put(&mut self, context: BlockHash, state: PanelState) -> Result<bool, ReplayError> {
        match &mut self.records {
            Records::Memory(records) => Ok(records.insert(context, state).is_some()),
            Records::Disk(records) => records
                .put(*context.as_bytes(), &state)
                .map_err(|error| invalid(error.to_string())),
        }
    }

    pub(super) fn insert(
        &mut self,
        context: BlockHash,
        position: usize,
        checkpoint: BlockHash,
        selected: Option<HashSet<WalletAddress>>,
    ) -> Result<bool, ReplayError> {
        self.put(
            context,
            PanelState {
                position,
                checkpoint,
                selected,
                votes: HashMap::new(),
                defaults: HashSet::new(),
            },
        )
    }

    pub(super) fn save(&mut self, context: BlockHash, panel: Panel<'_>) -> Result<(), ReplayError> {
        if !self.put(context, panel.state)? {
            return Err(invalid("updated panel was absent"));
        }
        Ok(())
    }
}

#[cfg(test)]
pub(in crate::confirmed_replay) fn assert_scoped_reuse(certificate: &CertifiedCheckpoint) {
    use std::{borrow::Cow, cell::Cell};
    struct Counted<'a> {
        certificate: &'a CertifiedCheckpoint,
        reads: Cell<usize>,
        owned: bool,
    }
    impl SelectionSource for Counted<'_> {
        fn len(&self) -> usize {
            1
        }
        fn read_selection(
            &self,
            position: usize,
        ) -> Result<Cow<'_, CertifiedCheckpoint>, ReplayError> {
            self.reads.set(self.reads.get() + 1);
            if position != 0 {
                return Err(invalid("missing selection"));
            }
            Ok(if self.owned { Cow::Owned(self.certificate.clone()) } else { Cow::Borrowed(self.certificate) })
        }
    }
    let context = certificate
        .replayed()
        .request()
        .vote_context(&certificate.certificate().checkpoint)
        .unwrap();
    for (disk, owned) in [(false, false), (false, true), (true, false), (true, true)] {
        let directory = tempfile::tempdir().unwrap();
        let source = Counted {
            certificate,
            reads: Cell::new(0),
            owned,
        };
        let path = disk.then_some(directory.path());
        let mut panels = Panels::new(&source, path).unwrap();
        panels
            .insert(
                context,
                0,
                certificate.certificate().checkpoint.hash(),
                None,
            )
            .unwrap();
        let first = panels.require(&context).unwrap();
        for _ in 0..32 {
            let next = panels.require(&context).unwrap();
            assert!(std::ptr::eq(first.certificate.as_ref(), next.certificate.as_ref()),
                "cache hit cloned the authenticated certificate payload");
        }
        assert_eq!(
            source.reads.get(),
            1,
            "repeated panel access reauthenticates history"
        );
        let original = panels.require(&context).unwrap().state;
        let mut wrong = original.clone();
        wrong.checkpoint = BlockHash::new([255; 32]);
        panels.put(context, wrong).unwrap();
        assert!(
            panels.require(&context).is_err(),
            "cached certificate bypassed row identity"
        );
        let other_context = BlockHash::new([254; 32]);
        panels.put(other_context, original.clone()).unwrap();
        assert!(
            panels.require(&other_context).is_err(),
            "cached certificate bypassed context"
        );
        let mut wrong = original.clone();
        wrong.position = 1;
        panels.put(context, wrong).unwrap();
        assert!(
            panels.require(&context).is_err(),
            "cache ignored source position"
        );
        panels.put(context, original).unwrap();
        panels.require(&context).unwrap();
        if let Records::Disk(records) = &panels.records {
            records.truncate_for_test();
            assert!(
                panels.require(&context).is_err(),
                "cache hid corrupt panel storage"
            );
        }
        drop(panels);
        let before = source.reads.get();
        let mut next = Panels::new(&source, path).unwrap();
        next.insert(
            context,
            0,
            certificate.certificate().checkpoint.hash(),
            None,
        )
        .unwrap();
        next.require(&context).unwrap();
        assert_eq!(
            source.reads.get(),
            before + 1,
            "certificate leaked across replay scopes"
        );
    }
}
