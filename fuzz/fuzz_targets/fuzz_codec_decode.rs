#![no_main]

use libfuzzer_sys::fuzz_target;

// Fuzz the codec encode/decode roundtrip with arbitrary payloads.
fuzz_target!(|data: &[u8]| {
    // Test that encoding then decoding arbitrary strings roundtrips correctly
    // (when it succeeds), and that decoding arbitrary bytes never panics.

    // 1. Try to decode raw arbitrary bytes as a String
    let _ = burst_protocol::codec::decode::<String>(data);

    // 2. Try to decode raw bytes as a Vec<u8>
    let _ = burst_protocol::codec::decode::<Vec<u8>>(data);

    // 3. Try to decode framed bytes as a u64
    let _ = burst_protocol::codec::decode_framed::<u64>(data);

    // 4. If data is long enough, try treating first 4 bytes as length prefix
    if data.len() >= 4 {
        let _ = burst_protocol::codec::decode_framed::<Vec<u8>>(data);
    }

    // 5. Encode a simple value derived from input, then verify roundtrip
    if data.len() >= 8 {
        let val = u64::from_le_bytes([
            data[0], data[1], data[2], data[3],
            data[4], data[5], data[6], data[7],
        ]);
        if let Ok(encoded) = burst_protocol::codec::encode(&val) {
            let decoded = burst_protocol::codec::decode_framed::<u64>(&encoded);
            assert!(decoded.is_ok(), "roundtrip must succeed for u64");
            let (decoded_val, consumed) = decoded.unwrap();
            assert_eq!(decoded_val, val);
            assert_eq!(consumed, encoded.len());
        }
    }
});
