#![no_main]

use libfuzzer_sys::fuzz_target;
use burst_messages::WireMessage;

fuzz_target!(|data: &[u8]| {
    let _ = WireMessage::decode_framed(data);
    if let Ok(message) = WireMessage::decode(data) {
        let canonical = message.encode().expect("decoded message remains bounded");
        let again = WireMessage::decode(&canonical).unwrap();
        assert_eq!(again.encode().unwrap(), canonical);
    }
});
