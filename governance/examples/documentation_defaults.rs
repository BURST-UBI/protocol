use burst_governance::params::view::snapshot;
use burst_types::{ProtocolParams, BRN_UNIT, TRST_UNIT};

fn main() {
    let defaults = serde_json::json!({
        "live": snapshot(&ProtocolParams::burst_defaults()),
        "test": snapshot(&ProtocolParams::testnet_defaults()),
        "dev": snapshot(&ProtocolParams::devnet_defaults()),
        "units": {"brn": BRN_UNIT.to_string(), "trst": TRST_UNIT.to_string()},
    });
    println!("{}", serde_json::to_string_pretty(&defaults).unwrap());
}
