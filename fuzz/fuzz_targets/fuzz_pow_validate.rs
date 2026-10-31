#![no_main]

use libfuzzer_sys::fuzz_target;

// Look for validation panics; inputs encode a root, nonce and optional threshold.
fuzz_target!(|data: &[u8]| {
    if data.len() >= 40 {
        let mut hash_bytes = [0u8; 32];
        hash_bytes.copy_from_slice(&data[..32]);
        let nonce = u64::from_le_bytes([
            data[32], data[33], data[34], data[35],
            data[36], data[37], data[38], data[39],
        ]);
        let block_hash = burst_types::BlockHash::new(hash_bytes);

        let difficulty = if data.len() >= 48 {
            u64::from_le_bytes([
                data[40], data[41], data[42], data[43],
                data[44], data[45], data[46], data[47],
            ])
        } else {
            0x00000000_ffffffff
        };

        let _ = burst_work::validate_work(&block_hash, nonce, difficulty);
    }
});
