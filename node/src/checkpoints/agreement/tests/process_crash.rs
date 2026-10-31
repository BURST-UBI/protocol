use super::*;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

const CHILD: &str = "checkpoints::agreement::tests::process_crash::crash_child";
const MARKER: &str = "checkpoint-crash-test/atomic-marker";

fn ready() -> ! {
    println!("BURST_CHECKPOINT_CRASH_READY");
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::park();
    }
}

#[test]
#[ignore = "subprocess helper for the abrupt-termination test"]
fn crash_child() {
    let directory = std::env::var("BURST_CHECKPOINT_CRASH_DIRECTORY").unwrap();
    let stage = std::env::var("BURST_CHECKPOINT_CRASH_STAGE").unwrap();
    let anchor = anchor();
    let keys = keys();
    let key = &keys[0];
    let store = open(std::path::Path::new(&directory));
    let proposal = proposal(&anchor, 10, 0);
    let prevote = vote(
        apply(
            &store,
            &anchor,
            &key.private,
            Command::Proposal(&proposal),
            fixture_valid,
        )
        .unwrap(),
    );
    if stage == "prevote" {
        ready();
    }
    let quorum = Quorum {
        signatures: keys[..3]
            .iter()
            .map(|key| sign(key, &prevote.ballot.signing_payload()))
            .collect(),
        ballot: prevote.ballot,
    };
    let commit = vote(
        super::super::transition(
            &store,
            &anchor,
            &key.private,
            Command::Prepared(&proposal, &quorum),
            None,
            fixture_valid,
            |batch, _| {
                batch.put_meta(MARKER, &[1])?;
                if stage == "before_commit" {
                    ready();
                }
                Ok(())
            },
        )
        .unwrap(),
    );
    if stage == "precommit" {
        ready();
    }
    assert_eq!(stage, "decision");
    let certificate = CheckpointCertificate {
        checkpoint: proposal.body.checkpoint,
        round: 0,
        signatures: keys[..3]
            .iter()
            .map(|key| sign(key, &commit.ballot.signing_payload()))
            .collect(),
    };
    apply(
        &store,
        &anchor,
        &key.private,
        Command::Decide(certificate),
        fixture_valid,
    )
    .unwrap();
    ready();
}

#[tokio::test]
async fn abrupt_exit_keeps_votes_locks_and_decisions_but_not_uncommitted_writes() {
    for stage in ["prevote", "before_commit", "precommit", "decision"] {
        let directory = tempfile::tempdir().unwrap();
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD, "--ignored", "--nocapture"])
            .env("BURST_CHECKPOINT_CRASH_DIRECTORY", directory.path())
            .env("BURST_CHECKPOINT_CRASH_STAGE", stage)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let line = output
                    .next_line()
                    .await
                    .unwrap()
                    .expect("crash child exited before its checkpoint boundary");
                if line.contains("BURST_CHECKPOINT_CRASH_READY") {
                    break;
                }
            }
        })
        .await
        .expect("crash child did not reach its checkpoint boundary");
        child.start_kill().unwrap();
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.signal(), Some(9), "child was not killed abruptly");

        let store = open(directory.path());
        let anchor = anchor();
        let key = keys().remove(0);
        let representative = derive_address(&key.public);
        let before = read_state(&store, &anchor, &representative)
            .unwrap()
            .unwrap();
        let value = checkpoint(&anchor, 10).hash();
        assert_eq!(before.prevote().unwrap().value, Some(value));
        let prepared = matches!(stage, "precommit" | "decision");
        assert_eq!(before.precommit().is_some(), prepared);
        if prepared {
            assert_eq!(before.precommit().unwrap().value, Some(value));
            assert_eq!(store.meta_store().get_meta(MARKER).unwrap(), vec![1]);
        } else {
            assert!(matches!(
                store.meta_store().get_meta(MARKER),
                Err(StoreError::NotFound(_))
            ));
        }
        assert_eq!(before.decision().is_some(), stage == "decision");
        let resumed = apply(&store, &anchor, &key.private, Command::Resume, |_| {
            panic!("resume should retransmit the retained choice")
        })
        .unwrap();
        if let Some(certificate) = before.decision() {
            anchor.verify_certificate(certificate).unwrap();
            assert_eq!(resumed, vec![Outgoing::Decision(certificate.clone())]);
        } else {
            assert_eq!(resumed.len(), if prepared { 2 } else { 1 });
            for output in &resumed {
                let Outgoing::Vote(vote) = output else {
                    panic!("expected resumed ballot")
                };
                vote.verify(&anchor).unwrap();
                assert_eq!(vote.ballot.value, Some(value));
            }
        }
        assert!(apply(
            &store,
            &anchor,
            &key.private,
            Command::Proposal(&proposal(&anchor, 11, 0)),
            |_| Ok(()),
        )
        .is_err());
        assert_eq!(
            read_state(&store, &anchor, &representative).unwrap(),
            Some(before)
        );
        eprintln!("recovered {stage} after SIGKILL without a conflicting vote");
    }
}
