use super::*;
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

mod prepare;
mod resume;

pub(in crate::confirmed_replay::tests) use prepare::prepared;

#[test]
fn signed_electorate_grows_across_snapshot_epochs_and_reopens() {
    grow(257, 128, 257, None, false, None);
}

#[test]
#[ignore = "manual signed electorate preparation; use capped validation host"]
fn signed_incremental_electorate_resource_probe() {
    let humans = std::env::var("BURST_ELECTORATE_HUMANS")
        .expect("set BURST_ELECTORATE_HUMANS")
        .parse::<u32>()
        .unwrap();
    let batch_size = std::env::var("BURST_ELECTORATE_BATCH")
        .expect("set BURST_ELECTORATE_BATCH")
        .parse::<u32>()
        .unwrap();
    assert!((257..=100_000).contains(&humans));
    assert!((128..=4096).contains(&batch_size));
    let until = std::env::var("BURST_ELECTORATE_UNTIL")
        .map(|value| value.parse::<u32>().unwrap())
        .unwrap_or(humans);
    let baseline = std::env::var_os("BURST_ELECTORATE_BASELINE").map(PathBuf::from);
    let params = match std::env::var("BURST_ELECTORATE_GOVERNANCE").as_deref() {
        Ok("1") => Some(crate::verification_test_support::governance_fixture_params(
            humans,
        )),
        Err(std::env::VarError::NotPresent) => None,
        _ => panic!("BURST_ELECTORATE_GOVERNANCE must be 1 or absent"),
    };
    if let Some(mode) = std::env::var_os("BURST_ELECTORATE_FINISH_SUFFIX") {
        assert_eq!(mode, "1");
        assert!(std::env::var_os("BURST_ELECTORATE_FINISH_ACCEPTED").is_none());
        assert!(params.is_none(), "suffix recovery uses saved parameters");
        resume::finish_suffix(
            baseline.as_deref().expect("set recovery baseline"),
            humans,
            until,
        );
        return;
    }
    match std::env::var("BURST_ELECTORATE_FINISH_ACCEPTED").as_deref() {
        Ok("1") => {
            assert!(params.is_none(), "accepted recovery uses saved parameters");
            resume::finish_accepted(
                baseline.as_deref().expect("set recovery baseline"),
                humans,
                until,
            );
        }
        Err(std::env::VarError::NotPresent) => {
            grow(humans, batch_size, until, baseline.as_deref(), true, params);
        }
        _ => panic!("BURST_ELECTORATE_FINISH_ACCEPTED must be 1 or absent"),
    }
}

fn configured_map_size() -> usize {
    let mut config = crate::NodeConfig::default();
    match std::env::var("BURST_ELECTORATE_MAP_BYTES") {
        Ok(value) => {
            config.database_map_size_bytes =
                value.parse().expect("invalid BURST_ELECTORATE_MAP_BYTES")
        }
        Err(std::env::VarError::NotPresent) => {}
        Err(error) => panic!("invalid BURST_ELECTORATE_MAP_BYTES: {error}"),
    }
    config
        .database_map_size()
        .expect("invalid configured electorate map size")
}

fn grow(
    humans: u32,
    batch_size: u32,
    until: u32,
    baseline: Option<&Path>,
    retain: bool,
    fresh_params: Option<ProtocolParams>,
) -> Option<PathBuf> {
    assert!((3..=humans).contains(&until));
    assert!(batch_size > 0);
    assert!(
        baseline.is_none() || fresh_params.is_none(),
        "resume uses immutable saved parameters"
    );
    let started = Instant::now();
    let report = |stage: &str, population: u32, store: &LmdbStore| {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let peak = status.lines().find(|line| line.starts_with("VmHWM:"));
        eprintln!(
            "electorate humans={population}/{humans} stage={stage} elapsed_ms={} map_bytes={} data_bytes={} peak={}",
            started.elapsed().as_millis(),
            store.env().info().map_size,
            std::fs::metadata(store.env().path().join("data.mdb")).unwrap().len(),
            peak.unwrap_or("unavailable")
        );
    };
    let map_size = configured_map_size();
    let (directory, mut store, mut fixture, mut population, mut epoch, initial_cutoff) =
        if let Some(baseline) = baseline {
            let directory = tempfile::tempdir().unwrap();
            std::fs::copy(baseline.join("data.mdb"), directory.path().join("data.mdb")).unwrap();
            let store = LmdbStore::open(directory.path(), 32, map_size).unwrap();
            let (fixture, population, epoch, now) =
                resume::restore(&store, baseline, humans).unwrap();
            assert!(
                population < until,
                "resume must extend the saved population"
            );
            (directory, store, fixture, population, epoch, now - 1)
        } else {
            let (directory, store, fixture, epoch, cutoff) = prepared(
                humans,
                fresh_params.unwrap_or_else(|| Fixture::new().params),
                map_size,
            );
            (directory, store, fixture, 3, epoch, cutoff)
        };
    let path = directory.path().to_path_buf();
    let _cleanup = if retain {
        eprintln!("electorate working={}", directory.keep().display());
        None
    } else {
        Some(directory)
    };
    report(
        if baseline.is_none() {
            "prepared-committees"
        } else {
            "authenticated-resume"
        },
        population,
        &store,
    );
    resume::save(
        &path,
        &store,
        &fixture.params,
        epoch,
        population,
        initial_cutoff + 1,
        humans,
    );
    while population < until {
        let recovered = installed::load_base(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap()
            .recover_stored_successors(&store, |base, plan, _| {
                crate::checkpoint_sync::witness::read_plan(&store, base, plan)
            })
            .unwrap();
        let base = &recovered.continuation;
        let end = (population + batch_size).min(until);
        let first_block = fixture.blocks.len();
        let cutoff = fixture.extend_electorate(&store, population..end, epoch, |index| {
            address((index % 3) as u8)
        });
        // VerificationVote/Open use serial admission in the node. A single writer for
        // the whole electorate suffix also pins pages retired by the last
        // installation until thousands of unrelated blocks have been stored.
        for block in &fixture.blocks[first_block..] {
            cemented::append(&store, block, true);
        }
        report("signed-suffix", end, &store);
        let tail = cemented::finish(base, &store, &store);
        let candidate = base
            .prepare_next_cemented_candidate(
                &store,
                &tail,
                &recovered.selections,
                SigningClock {
                    now: cutoff + 1,
                    error_bound: 0,
                },
            )
            .unwrap()
            .unwrap();
        assert!(matches!(&candidate, CompactReplayCandidate::Roster(_)));
        let manifest = candidate.input().manifest();
        let certificate = certified_votes::certify(manifest.checkpoint(), &[0, 1, 2]);
        report("discovered-roster", end, &store);
        crate::checkpoint_store::compact::accept_candidate(
            &store,
            base.anchor(),
            candidate.input(),
            &certificate,
        )
        .unwrap();
        epoch = certificate.checkpoint.hash();
        install(&fixture, &store, epoch, &[0, 1, 2]);
        report("installed-snapshot", end, &store);
        drop(store);
        store = LmdbStore::open(&path, 32, map_size).unwrap();
        let reopened = installed::load_base(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap();
        assert_eq!(reopened.anchor().hash(), epoch);
        resume::check_membership(reopened.anchor().roster(), end).unwrap();
        resume::save(
            &path,
            &store,
            &fixture.params,
            epoch,
            end,
            reopened.anchor().cutoff() + 1,
            humans,
        );
        report("verified-reopen", end, &store);
        population = end;
    }
    store.force_sync().unwrap();
    drop(store);
    if retain {
        eprintln!(
            "electorate retained={} population={population} target={humans}",
            path.display()
        );
        Some(path)
    } else {
        None
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "explicit capped-host snapshot-backed signed governance query check"]
async fn signed_snapshot_electorate_feeds_governance_queries() {
    let baseline = grow(
        257,
        128,
        257,
        None,
        true,
        Some(crate::verification_test_support::governance_fixture_params(
            257,
        )),
    )
    .unwrap();
    let source = burst_crypto::blake2b_256(&std::fs::read(baseline.join("data.mdb")).unwrap());
    let metadata = std::fs::read(baseline.join("resource-probe.bin")).unwrap();
    crate::certified_network_tests::governance_queries_from_snapshot(&baseline).await;
    assert_eq!(
        source,
        burst_crypto::blake2b_256(&std::fs::read(baseline.join("data.mdb")).unwrap())
    );
    assert_eq!(
        metadata,
        std::fs::read(baseline.join("resource-probe.bin")).unwrap()
    );
    std::fs::remove_dir_all(baseline).unwrap();
}
