use super::*;
use crate::{LmdbEnvironment, REQUIRED_DATABASES};

fn leaf(account: u8) -> HeadNode {
    HeadNode::Leaf {
        account: [account; 32],
        height: 1,
        hash: [account.wrapping_add(1); 32],
    }
}

#[test]
fn shared_roots_abort_reopen_and_bounded_cleanup_preserve_pinned_readers() {
    let dir = tempfile::tempdir().unwrap();
    let env = LmdbEnvironment::open(dir.path(), REQUIRED_DATABASES, 8 << 20).unwrap();
    let mut batch = env.tx_begin_write().unwrap();
    let left = batch.intern_checkpoint_head(&leaf(0)).unwrap();
    let right = batch.intern_checkpoint_head(&leaf(128)).unwrap();
    let node = HeadNode::Branch {
        bit: 0,
        left,
        right,
    };
    let root = batch.intern_checkpoint_head(&node).unwrap();
    assert_eq!(batch.intern_checkpoint_head(&node).unwrap(), root);
    batch.retain_checkpoint_head(&root).unwrap();
    batch.retain_checkpoint_head(&root).unwrap();
    assert!(batch.reclaim_checkpoint_heads(1).unwrap().complete);
    batch.commit().unwrap();
    let pinned = env.tx_begin_read().unwrap();
    {
        let mut aborted = env.tx_begin_write().unwrap();
        aborted.release_checkpoint_head(&root).unwrap();
        aborted.release_checkpoint_head(&root).unwrap();
        assert_eq!(aborted.reclaim_checkpoint_heads(1).unwrap().nodes, 1);
    }
    assert_eq!(
        env.read_view(&pinned)
            .unwrap()
            .checkpoint_head_node(&root)
            .unwrap(),
        Some(node.clone())
    );
    let mut batch = env.tx_begin_write().unwrap();
    batch.release_checkpoint_head(&root).unwrap();
    assert_eq!(batch.reclaim_checkpoint_heads(1).unwrap().nodes, 0);
    batch.commit().unwrap();
    let mut batch = env.tx_begin_write().unwrap();
    batch.release_checkpoint_head(&root).unwrap();
    let page = batch.reclaim_checkpoint_heads(1).unwrap();
    assert_eq!(page.nodes, 1);
    assert!(!page.complete);
    batch.commit().unwrap();
    let fresh = env.tx_begin_write().unwrap();
    assert!(fresh
        .read_view()
        .checkpoint_head_node(&root)
        .unwrap()
        .is_none());
    assert_eq!(
        env.read_view(&pinned)
            .unwrap()
            .checkpoint_head_node(&root)
            .unwrap(),
        Some(node)
    );
    assert_eq!(
        env.read_view(&pinned)
            .unwrap()
            .checkpoint_head_node(&left)
            .unwrap(),
        Some(leaf(0))
    );
    drop(fresh);
    drop(pinned);
    drop(env);
    let env = LmdbEnvironment::open(dir.path(), REQUIRED_DATABASES, 8 << 20).unwrap();
    for expected_complete in [false, true] {
        let mut batch = env.tx_begin_write().unwrap();
        let page = batch.reclaim_checkpoint_heads(1).unwrap();
        assert_eq!(page.nodes, 1);
        assert_eq!(page.complete, expected_complete);
        batch.commit().unwrap();
    }
    let reader = env.tx_begin_read().unwrap();
    assert!(env
        .read_view(&reader)
        .unwrap()
        .checkpoint_head_node(&left)
        .unwrap()
        .is_none());
    assert!(env
        .read_view(&reader)
        .unwrap()
        .checkpoint_head_node(&right)
        .unwrap()
        .is_none());
}

#[test]
fn queued_node_can_be_reused_without_double_owning_children() {
    let dir = tempfile::tempdir().unwrap();
    let env = LmdbEnvironment::open(dir.path(), REQUIRED_DATABASES, 8 << 20).unwrap();
    let mut batch = env.tx_begin_write().unwrap();
    let left = batch.intern_checkpoint_head(&leaf(0)).unwrap();
    let right = batch.intern_checkpoint_head(&leaf(128)).unwrap();
    let branch = HeadNode::Branch {
        bit: 0,
        left,
        right,
    };
    let root = batch.intern_checkpoint_head(&branch).unwrap();
    batch.commit().unwrap(); // Unowned root is queued, while its edges still own children.
    let mut batch = env.tx_begin_write().unwrap();
    assert_eq!(batch.intern_checkpoint_head(&branch).unwrap(), root);
    batch.retain_checkpoint_head(&root).unwrap();
    assert_eq!(batch.reclaim_checkpoint_heads(8).unwrap().nodes, 0);
    batch.release_checkpoint_head(&root).unwrap();
    let page = batch.reclaim_checkpoint_heads(8).unwrap();
    assert_eq!(page.nodes, 3);
    assert!(page.complete);
    batch.commit().unwrap();
}

#[test]
fn malformed_nodes_counts_and_corruption_poison_the_writer() {
    let dir = tempfile::tempdir().unwrap();
    let env = LmdbEnvironment::open(dir.path(), REQUIRED_DATABASES, 8 << 20).unwrap();
    let mut batch = env.tx_begin_write().unwrap();
    let hash = batch.intern_checkpoint_head(&leaf(0)).unwrap();
    batch.retain_checkpoint_head(&hash).unwrap();
    batch.commit().unwrap();
    for corruption in [true, false] {
        let mut batch = env.tx_begin_write().unwrap();
        if corruption {
            batch
                .env
                .checkpoint_heads_db
                .put(&mut batch.txn, &key(0, &hash), &[0; 81])
                .unwrap();
            assert!(batch.intern_checkpoint_head(&leaf(0)).is_err());
        } else {
            batch.release_checkpoint_head(&hash).unwrap();
            assert!(batch.release_checkpoint_head(&hash).is_err());
        }
        assert!(batch.commit().is_err());
    }
    let mut batch = env.tx_begin_write().unwrap();
    assert!(batch
        .intern_checkpoint_head(&HeadNode::Branch {
            bit: 256,
            left: hash,
            right: [2; 32]
        })
        .is_err());
    assert!(batch.commit().is_err());
    let reader = env.tx_begin_read().unwrap();
    assert_eq!(
        env.read_view(&reader)
            .unwrap()
            .checkpoint_head_node(&hash)
            .unwrap(),
        Some(leaf(0))
    );
}

#[test]
fn checkpoint_head_branch_rejects_child_depth_without_publishing_edges() {
    let dir = tempfile::tempdir().unwrap();
    let env = LmdbEnvironment::open(dir.path(), REQUIRED_DATABASES, 8 << 20).unwrap();
    let mut batch = env.tx_begin_write().unwrap();
    let left = batch.intern_checkpoint_head(&leaf(0)).unwrap();
    let right = batch.intern_checkpoint_head(&leaf(128)).unwrap();
    let branch = HeadNode::Branch {
        bit: 1,
        left,
        right,
    };
    let root = batch.intern_checkpoint_head(&branch).unwrap();
    batch.retain_checkpoint_head(&root).unwrap();
    batch.commit().unwrap();
    let mut batch = env.tx_begin_write().unwrap();
    // The first child's count is incremented before the second child is rejected.
    assert!(batch
        .intern_checkpoint_head(&HeadNode::Branch {
            bit: 1,
            left,
            right: root,
        })
        .is_err());
    assert!(batch.commit().is_err());
    let mut batch = env.tx_begin_write().unwrap();
    batch.release_checkpoint_head(&root).unwrap();
    let reclaimed = batch.reclaim_checkpoint_heads(8).unwrap();
    assert_eq!(reclaimed.nodes, 3);
    assert!(reclaimed.complete);
    batch.commit().unwrap();
}

#[test]
fn checkpoint_head_staging_pins_survive_cleanup_reopen_and_aborted_transfer() {
    let dir = tempfile::tempdir().unwrap();
    let env = LmdbEnvironment::open(dir.path(), REQUIRED_DATABASES, 8 << 20).unwrap();
    let job = [11; 32];
    let mut batch = env.tx_begin_write().unwrap();
    let left = batch.intern_checkpoint_head(&leaf(0)).unwrap();
    let right = batch.intern_checkpoint_head(&leaf(128)).unwrap();
    batch
        .replace_checkpoint_head_staging(&job, &[left, right])
        .unwrap();
    batch.commit().unwrap();
    let mut batch = env.tx_begin_write().unwrap();
    assert_eq!(batch.reclaim_checkpoint_heads(256).unwrap().nodes, 0);
    batch.commit().unwrap();
    drop(env);
    let env = LmdbEnvironment::open(dir.path(), REQUIRED_DATABASES, 8 << 20).unwrap();
    let mut batch = env.tx_begin_write().unwrap();
    assert_eq!(
        batch.read_view().checkpoint_head_staging(&job).unwrap(),
        vec![left, right]
    );
    let parent = HeadNode::Branch {
        bit: 0,
        left,
        right,
    };
    let root = batch.intern_checkpoint_head(&parent).unwrap();
    batch
        .replace_checkpoint_head_staging(&job, &[root])
        .unwrap();
    drop(batch); // Abort the node, pin transfer and descriptor together.
    let mut batch = env.tx_begin_write().unwrap();
    assert_eq!(
        batch.read_view().checkpoint_head_staging(&job).unwrap(),
        vec![left, right]
    );
    let root = batch.intern_checkpoint_head(&parent).unwrap();
    batch
        .replace_checkpoint_head_staging(&job, &[root])
        .unwrap();
    batch.commit().unwrap();
    let mut batch = env.tx_begin_write().unwrap();
    assert_eq!(batch.reclaim_checkpoint_heads(256).unwrap().nodes, 0);
    batch.retain_checkpoint_head(&root).unwrap(); // Published record's owner.
    batch.replace_checkpoint_head_staging(&job, &[]).unwrap();
    assert_eq!(batch.reclaim_checkpoint_heads(256).unwrap().nodes, 0);
    batch.release_checkpoint_head(&root).unwrap();
    assert_eq!(batch.reclaim_checkpoint_heads(256).unwrap().nodes, 3);
    assert!(batch
        .read_view()
        .checkpoint_head_staging(&job)
        .unwrap()
        .is_empty());
    batch.commit().unwrap();
}

#[test]
fn checkpoint_head_staging_rejects_duplicate_missing_and_oversized_forests() {
    let dir = tempfile::tempdir().unwrap();
    let env = LmdbEnvironment::open(dir.path(), REQUIRED_DATABASES, 8 << 20).unwrap();
    for mode in 0..3 {
        let mut batch = env.tx_begin_write().unwrap();
        let root = batch.intern_checkpoint_head(&leaf(0)).unwrap();
        let roots = match mode {
            0 => vec![root, root],
            1 => vec![[9; 32]],
            _ => vec![root; 258],
        };
        assert!(batch
            .replace_checkpoint_head_staging(&[1; 32], &roots)
            .is_err());
        assert!(batch.commit().is_err());
    }
}
