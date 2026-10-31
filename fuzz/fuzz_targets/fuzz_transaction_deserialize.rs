#![no_main]

use libfuzzer_sys::fuzz_target;

// Look for panics while decoding transactions and fixed-size protocol types.
fuzz_target!(|data: &[u8]| {

    let _ = bincode::deserialize::<burst_transactions::Transaction>(data);

    let _ = bincode::deserialize::<burst_types::BlockHash>(data);

    let _ = bincode::deserialize::<burst_types::TxHash>(data);

    let _ = bincode::deserialize::<burst_types::Timestamp>(data);

    let _ = bincode::deserialize::<burst_types::Signature>(data);
});
