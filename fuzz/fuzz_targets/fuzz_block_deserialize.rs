#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Attempt to deserialize arbitrary bytes as various BURST types.
    // The goal is to ensure deserialization never panics on malformed input.

    // Try deserializing as a Transaction enum
    let _ = bincode::deserialize::<burst_transactions::Transaction>(data);

    // Try deserializing as a BlockHash
    let _ = bincode::deserialize::<burst_types::BlockHash>(data);

    // Try deserializing as a TxHash
    let _ = bincode::deserialize::<burst_types::TxHash>(data);

    // Try deserializing as a Timestamp
    let _ = bincode::deserialize::<burst_types::Timestamp>(data);

    // Try deserializing as a Signature
    let _ = bincode::deserialize::<burst_types::Signature>(data);
});
