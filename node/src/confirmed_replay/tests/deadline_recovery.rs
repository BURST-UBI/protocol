use super::*;
use crate::checkpoint_driver::{CheckpointDriver, Message};
use crate::checkpoint_replay::SigningClock;
use burst_consensus::checkpoint_round::Step;

fn deliver(
    drivers: &mut [Option<CheckpointDriver>],
    active: &[usize],
    messages: Vec<Message>,
    clock: Option<SigningClock>,
) {
    let mut queue: std::collections::VecDeque<_> = messages.into();
    let mut count = 0;
    while let Some(message) = queue.pop_front() {
        count += 1;
        assert!(count < 1000, "agreement did not converge");
        for &index in active {
            queue.extend(
                drivers[index]
                    .as_mut()
                    .unwrap()
                    .receive(message.clone(), clock)
                    .unwrap(),
            );
        }
    }
}

#[test]
fn expired_minority_selection_recovers_in_a_new_round_after_restart() {
    let DriverFixture {
        fixture,
        dirs,
        mut stores,
        mut drivers,
        manifests,
        leader,
        cutoff,
    } = checkpoint_drivers();
    let on_time = Some(SigningClock {
        now: cutoff + 30,
        error_bound: 0,
    });
    let late = Some(SigningClock {
        now: cutoff + 3601,
        error_bound: 0,
    });
    let minority: Vec<_> = (0..6).map(|i| (leader + i) % 9).collect();
    let all: Vec<_> = (0..9).collect();
    let initial = drivers[leader].as_mut().unwrap().propose(on_time).unwrap();
    deliver(&mut drivers, &minority, initial, on_time);
    for &index in &minority {
        let driver = drivers[index].as_mut().unwrap();
        assert!(driver.timeout(Step::Prevote).is_err());
        let before = driver.state().unwrap();
        driver.refresh_candidate(late).unwrap();
        assert_eq!(driver.state().unwrap(), before);
        assert!(before.unwrap().valid_proof().is_none());
    }
    // The three late humans cannot sign the now-known beacon proposal. Their
    // nil votes let the other six finish the round, not manufacture a prepare.
    let mut messages = Vec::new();
    for index in all.iter().filter(|index| !minority.contains(index)) {
        messages.extend(
            drivers[*index]
                .as_mut()
                .unwrap()
                .timeout(Step::Propose)
                .unwrap(),
        );
    }
    for &index in &minority {
        messages.extend(
            drivers[index]
                .as_mut()
                .unwrap()
                .resume()
                .unwrap()
                .into_iter()
                .filter(|message| matches!(message, Message::Vote(_))),
        );
    }
    deliver(&mut drivers, &all, messages, late);
    let mut messages = Vec::new();
    for driver in drivers.iter_mut().flatten() {
        messages.extend(driver.timeout(Step::Prevote).unwrap());
    }
    deliver(&mut drivers, &all, messages, late);
    for driver in drivers.iter_mut().flatten() {
        driver.timeout(Step::Precommit).unwrap();
        assert_eq!(driver.round(), 1);
        assert!(driver.state().unwrap().unwrap().valid_proof().is_none());
    }
    let next_leader = all
        .iter()
        .copied()
        .find(|&index| drivers[index].as_ref().unwrap().is_proposer())
        .unwrap();
    drop(drivers[next_leader].take());
    drop(stores[next_leader].take());
    let reopened =
        std::sync::Arc::new(LmdbStore::open(dirs[next_leader].path(), 32, 32 << 20).unwrap());
    let replay = crate::checkpoint_store::CertifiedCheckpoint::replay_manifest(
        &reopened,
        &fixture.params,
        &manifests[next_leader],
    )
    .unwrap();
    let mut restored = CheckpointDriver::new(
        reopened,
        fixture.params,
        burst_crypto::keypair_from_seed(&[next_leader as u8; 32]).private,
        replay,
    )
    .unwrap();
    assert_eq!(restored.round(), 1);
    restored.refresh_candidate(None).unwrap();
    assert!(restored.propose(None).is_err());
    restored.refresh_candidate(late).unwrap();
    let messages = restored.propose(late).unwrap();
    let checkpoint = messages
        .iter()
        .find_map(|message| match message {
            Message::Proposal { proposal, .. } => Some(proposal.body.checkpoint.clone()),
            _ => None,
        })
        .unwrap();
    assert_ne!(checkpoint.hash(), manifests[next_leader].checkpoint.hash());
    assert_eq!(checkpoint.parent, manifests[next_leader].checkpoint.parent);
    assert_eq!(checkpoint.height, manifests[next_leader].checkpoint.height);
    assert!(checkpoint.cutoff > manifests[next_leader].checkpoint.cutoff);
    let before = restored.state().unwrap();
    restored
        .refresh_candidate(Some(SigningClock {
            now: cutoff + 7201,
            error_bound: 0,
        }))
        .unwrap();
    assert_eq!(
        restored.state().unwrap(),
        before,
        "never replace a signed current-round proposal"
    );
    drivers[next_leader] = Some(restored);
    deliver(&mut drivers, &all, messages, late);
    for driver in drivers.iter().flatten() {
        assert_eq!(
            driver
                .state()
                .unwrap()
                .unwrap()
                .decision()
                .unwrap()
                .checkpoint,
            checkpoint
        );
    }
}
