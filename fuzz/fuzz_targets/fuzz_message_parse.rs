#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Try to decode arbitrary bytes as a framed protocol message.
    // Tests the length-prefix framing + bincode deserialization path.

    // Try decoding as a generic JSON value (tests the framing layer)
    let _ = burst_protocol::codec::decode_framed::<serde_json::Value>(data);

    // Try decoding without framing (raw bincode)
    let _ = burst_protocol::codec::decode::<serde_json::Value>(data);

    // Try decoding as MessageHeader
    let _ = burst_protocol::codec::decode::<burst_messages::MessageHeader>(data);
    let _ = burst_protocol::codec::decode_framed::<burst_messages::MessageHeader>(data);

    // Try decoding as KeepaliveMessage
    let _ = burst_protocol::codec::decode::<burst_messages::KeepaliveMessage>(data);

    // Try decoding as PublishMessage
    let _ = burst_protocol::codec::decode::<burst_messages::PublishMessage>(data);
});
