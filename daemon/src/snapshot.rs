use anyhow::{ensure, Context};
use burst_node::{
    checkpoint_replay::ReplayedCheckpoint,
    confirmed_replay::snapshot::{
        archive::AuthenticatedSnapshot,
        export::{SnapshotAttestation, SnapshotExport},
    },
    NodeConfig,
};
use std::path::PathBuf;

#[derive(clap::Subcommand)]
pub enum Action {
    /// Replay a certified epoch and durably attest its state with the local representative key.
    Attest {
        #[arg(long, value_parser = parse_epoch)]
        epoch: burst_types::BlockHash,
        #[arg(long)]
        output: PathBuf,
    },
    /// Replay the epoch, verify supplied attestation files and write a human-quorum archive.
    Export {
        #[arg(long, value_parser = parse_epoch)]
        epoch: burst_types::BlockHash,
        #[arg(long, required = true)]
        attestation: Vec<PathBuf>,
        /// Also export the original genesis parameters for independently trusted distribution.
        #[arg(long)]
        genesis_params_output: Option<PathBuf>,
        #[arg(long)]
        output: PathBuf,
    },
    /// Verify genesis-to-epoch authority, human quorum and the entire state body.
    /// Uses temporary scratch files without opening the node ledger or starting networking.
    /// Does not install state or prune history.
    Verify {
        #[arg(long)]
        input: PathBuf,
        /// Refuse snapshots older than this independently obtained checkpoint height.
        #[arg(long, default_value_t = 1)]
        minimum_height: u64,
        /// Require this independently obtained checkpoint hash as the snapshot epoch.
        #[arg(long, value_parser = parse_epoch)]
        expected_epoch: Option<burst_types::BlockHash>,
        /// Independently trusted original genesis parameters in JSON (not current governed rules).
        #[arg(long)]
        genesis_params: Option<PathBuf>,
    },
}

fn parse_epoch(value: &str) -> Result<burst_types::BlockHash, String> {
    value.parse().map_err(|error: burst_types::transaction_hash::ParseHashError| error.to_string())
}

pub fn run(config: &NodeConfig, action: Action) -> anyhow::Result<()> {
    match action {
        Action::Attest { epoch, output } => {
            let attestation = SnapshotExport::attest_config(config, epoch)?;
            write_new(&output, |file| {
                attestation.write(file)?;
                Ok(())
            })?;
            println!("Attested snapshot epoch {epoch}: {}", output.display());
            Ok(())
        }
        Action::Export {
            epoch,
            attestation,
            genesis_params_output,
            output,
        } => {
            // Bound operator-supplied signature files before allocating their
            // decoded contents. The archive header has its own independent cap.
            ensure!(attestation.len() <= 32768, "too many attestation files");
            let attestations = attestation
                .iter()
                .map(|path| {
                    ensure!(
                        std::fs::metadata(path)?.is_file(),
                        "attestation must be a regular file"
                    );
                    let mut file = std::fs::File::open(path)?;
                    SnapshotAttestation::read(&mut file).map_err(Into::into)
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let (export, _store) = SnapshotExport::from_config(config, epoch)?;
            write_new(&output, |file| {
                export.write(&attestations, file)?;
                Ok(())
            })?;
            if let Some(path) = genesis_params_output {
                write_new(&path, |file| {
                    serde_json::to_writer_pretty(file, export.genesis_params())?;
                    Ok(())
                })
                .context("archive was written, but genesis parameter export failed")?;
            }
            println!(
                "Exported authenticated snapshot epoch {epoch}: {}",
                output.display()
            );
            Ok(())
        }
        Action::Verify {
            input,
            minimum_height,
            expected_epoch,
            genesis_params,
        } => {
            let params = match genesis_params {
                Some(path) => read_genesis_params(&path)?,
                None => config.params.clone(),
            };
            let anchor = ReplayedCheckpoint::genesis_anchor(config.network, &params)?;
            ensure!(
                std::fs::metadata(&input)?.is_file(),
                "snapshot input must be a regular file"
            );
            let file = std::fs::File::open(&input)
                .with_context(|| format!("cannot open snapshot {}", input.display()))?;
            ensure!(
                file.metadata()?.is_file(),
                "snapshot input must be a regular file"
            );
            let directory =
                std::env::current_dir().context("cannot locate verification scratch directory")?;
            let snapshot = AuthenticatedSnapshot::read_in(
                &anchor,
                &mut std::io::BufReader::new(file),
                &directory,
            )
            .context("snapshot authentication failed")?;
            let tip = snapshot.anchor();
            ensure!(
                tip.height() >= minimum_height,
                "snapshot is older than the required height"
            );
            ensure!(
                expected_epoch.is_none_or(|epoch| epoch == tip.hash()),
                "snapshot does not match the required epoch"
            );
            println!(
                "Verified {} snapshot at height {}",
                config.network.as_str(),
                tip.height()
            );
            println!("Epoch: {}", tip.hash());
            println!("State hash: {}", snapshot.certificate().manifest.state_hash);
            println!("No ledger was changed. Authentication does not establish that this is the latest checkpoint.");
            Ok(())
        }
    }
}

fn read_genesis_params(path: &std::path::Path) -> anyhow::Result<burst_types::ProtocolParams> {
    use std::io::Read;
    ensure!(
        std::fs::metadata(path)?.is_file(),
        "genesis parameters must be a regular file"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(65537)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 65536,
        "genesis parameter file exceeds byte limit"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

fn write_new(
    path: &std::path::Path,
    write: impl FnOnce(&mut std::fs::File) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    write(temporary.as_file_mut())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)
        .with_context(|| {
            format!(
                "cannot publish snapshot file {}; existing files are never replaced",
                path.display()
            )
        })?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_complete_or_absent_and_never_overwrites_existing_files() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("snapshot");
        assert!(write_new(&target, |file| {
            file.write_all(b"partial")?;
            anyhow::bail!("interrupted export")
        })
        .is_err());
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        write_new(&target, |file| {
            file.write_all(b"complete")?;
            Ok(())
        })
        .unwrap();
        assert!(write_new(&target, |file| {
            file.write_all(b"replacement")?;
            Ok(())
        })
        .is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"complete");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn epoch_argument_is_an_exact_hash() {
        assert_eq!(
            parse_epoch(&"aB".repeat(32)).unwrap(),
            burst_types::BlockHash::new([0xab; 32])
        );
        for value in [
            "a".repeat(63),
            "a".repeat(65),
            "g".repeat(64),
            "é".repeat(32),
        ] {
            assert!(parse_epoch(&value).is_err());
        }
    }

    #[test]
    fn invalid_archive_does_not_create_or_change_a_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("invalid.snapshot");
        std::fs::write(&input, b"not a snapshot").unwrap();
        let config = NodeConfig {
            data_dir: dir.path().join("must-not-exist"),
            ..NodeConfig::default()
        };
        assert!(run(
            &config,
            Action::Verify {
                input: input.clone(),
                minimum_height: 1,
                expected_epoch: None,
                genesis_params: None
            }
        )
        .is_err());
        assert!(!config.data_dir.exists());
        assert_eq!(std::fs::read(input).unwrap(), b"not a snapshot");
    }
}
