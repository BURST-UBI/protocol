//! Node-reported origin identities. These are not independent light-client proofs
//! or contribution weights, and cannot determine an unknown-origin proportion.

use crate::{
    signed_client::{parse_address, parse_hash},
    NodeClient, WalletError,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, time::Duration};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PaymentOrigins {
    pub payment: String,
    /// Every source/return block, the root and all burns are node-confirmed.
    pub confirmed: bool,
    pub provenance: OriginReport,
}

#[derive(Deserialize)]
struct PaymentSource {
    block_type: String,
    origin: String,
    link: String,
    confirmed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OriginReport {
    pub origin: String,
    pub complete: bool,
    pub confirmed: bool,
    pub origins: Vec<BurnOrigin>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BurnOrigin {
    pub hash: String,
    pub originator: String,
    pub timestamp: u64,
    pub confirmed: bool,
    pub revoked: bool,
}

impl NodeClient {
    /// Inspect a Burn, Send or returned payment without accepting or signing it.
    /// This does not establish that the payment is pending for a particular wallet.
    pub async fn payment_origins(&self, payment: &str) -> Result<PaymentOrigins, WalletError> {
        crate::runtime::timeout(
            Duration::from_secs(30),
            self.resolve_payment_origins(payment, 64),
        )
        .await
        .map_err(|_| WalletError::Node("payment origin inspection timed out".into()))?
    }

    async fn resolve_payment_origins(
        &self,
        payment: &str,
        max_steps: usize,
    ) -> Result<PaymentOrigins, WalletError> {
        let payment = parse_hash(payment)?.to_string();
        let mut source = payment.clone();
        let mut visited = HashSet::new();
        let mut confirmed = true;
        for _ in 0..max_steps {
            if !visited.insert(source.clone()) {
                return Err(WalletError::Node("cyclic returned-payment history".into()));
            }
            let block: PaymentSource = serde_json::from_value(
                self.rpc_call("block_info", serde_json::json!({"hash":source}))
                    .await?,
            )
            .map_err(|error| WalletError::Node(format!("invalid payment source: {error}")))?;
            confirmed &= block.confirmed;
            let origin = match block.block_type.as_str() {
                "Burn" => source,
                "Send" => parse_hash(&block.origin)?.to_string(),
                "RejectReceive" => {
                    source = parse_hash(&block.link)?.to_string();
                    continue;
                }
                _ => {
                    return Err(WalletError::Node(
                        "expected a burn, transfer or returned payment".into(),
                    ))
                }
            };
            let provenance = self.trst_origins(&origin).await?;
            if block.block_type == "Burn"
                && (provenance.origins.len() != 1 || provenance.origins[0].hash != origin)
            {
                return Err(WalletError::Node(
                    "burn payment has inconsistent origins".into(),
                ));
            }
            confirmed &=
                provenance.confirmed && provenance.origins.iter().all(|burn| burn.confirmed);
            return Ok(PaymentOrigins {
                payment,
                confirmed,
                provenance,
            });
        }
        Err(WalletError::Node(
            "returned-payment inspection limit exceeded".into(),
        ))
    }

    pub async fn trst_origins(&self, origin: &str) -> Result<OriginReport, WalletError> {
        let origin = parse_hash(origin)?.to_string();
        let report: OriginReport = serde_json::from_value(
            self.rpc_call("trst_origins", serde_json::json!({"origin":origin}))
                .await?,
        )
        .map_err(|error| WalletError::Node(format!("invalid origin report: {error}")))?;
        if report.origin != origin
            || !report.complete
            || report.origins.is_empty()
            || report.origins.len() > 8192
        {
            return Err(WalletError::Node(
                "origin report is incomplete or does not match the requested origin".into(),
            ));
        }
        for burn in &report.origins {
            if parse_hash(&burn.hash)?.to_string() != burn.hash {
                return Err(WalletError::Node("non-canonical burn origin hash".into()));
            }
            parse_address(&burn.originator)?;
        }
        if report
            .origins
            .windows(2)
            .any(|pair| pair[0].hash >= pair[1].hash)
        {
            return Err(WalletError::Node(
                "origin report contains duplicate or unsorted burns".into(),
            ));
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    fn report() -> Value {
        let keys = burst_crypto::keypair_from_seed(&[0; 32]);
        json!({"origin":"03".repeat(32),"complete":true,"confirmed":true,"origins":[
            {"hash":"01".repeat(32),"originator":burst_crypto::derive_address(&keys.public).as_str(),"timestamp":100,"confirmed":true,"revoked":false},
            {"hash":"02".repeat(32),"originator":burst_crypto::derive_address(&keys.public).as_str(),"timestamp":200,"confirmed":true,"revoked":true}
        ]})
    }

    async fn fixture(replies: Vec<(Value, Value)>) -> (NodeClient, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for (expected, response) in replies {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = BufReader::new(socket);
                let mut content_length = None;
                let mut bytes = 0;
                loop {
                    let mut line = String::new();
                    let length = socket.read_line(&mut line).await.unwrap();
                    assert!(length > 0);
                    bytes += length;
                    assert!(bytes < 8192);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(length) = line.to_ascii_lowercase().strip_prefix("content-length:")
                    {
                        content_length = Some(length.trim().parse::<usize>().unwrap());
                    }
                }
                let length = content_length.unwrap();
                assert!(length < 1024);
                let mut body = vec![0; length];
                socket.read_exact(&mut body).await.unwrap();
                assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), expected);
                let body = json!({"result":response}).to_string();
                socket
                    .get_mut()
                    .write_all(
                        format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        (NodeClient::new(url).unwrap(), server)
    }

    async fn request(response: Value) -> Result<OriginReport, WalletError> {
        let (client, server) = fixture(vec![(
            json!({"action":"trst_origins","origin":"03".repeat(32)}),
            response,
        )])
        .await;
        let result = client.trst_origins(&"03".repeat(32)).await;
        server.await.unwrap();
        result
    }

    fn source(hash: u8, kind: &str, link: u8, confirmed: bool) -> (Value, Value) {
        (
            json!({"action":"block_info","hash":format!("{hash:02x}").repeat(32)}),
            json!({"block_type":kind,"origin":"03".repeat(32),"link":format!("{link:02x}").repeat(32),"confirmed":confirmed}),
        )
    }

    fn origin_reply() -> (Value, Value) {
        (
            json!({"action":"trst_origins","origin":"03".repeat(32)}),
            report(),
        )
    }

    #[tokio::test]
    async fn payments_resolve_burns_transfers_and_repeated_returns_to_original_creators() {
        let mut burn = origin_reply();
        burn.1["origins"] = json!([burn.1["origins"][0].clone()]);
        burn.1["origins"][0]["hash"] = "03".repeat(32).into();
        burn.1["origins"][0]["revoked"] = true.into();
        for (hash, replies) in [
            (3, vec![source(3, "Burn", 0, true), burn]),
            (4, vec![source(4, "Send", 0, true), origin_reply()]),
            (
                6,
                vec![
                    source(6, "RejectReceive", 5, true),
                    source(5, "RejectReceive", 4, true),
                    source(4, "Send", 0, true),
                    origin_reply(),
                ],
            ),
        ] {
            let (client, server) = fixture(replies).await;
            let payment = format!("{hash:02x}").repeat(32);
            let result = client.payment_origins(&payment).await.unwrap();
            server.await.unwrap();
            assert_eq!(result.payment, payment);
            assert!(result.confirmed);
            assert_eq!(result.provenance.origin, "03".repeat(32));
            assert_eq!(result.provenance.origins[0].timestamp, 100);
            assert!(result.provenance.origins.last().unwrap().revoked);
        }
    }

    #[tokio::test]
    async fn unconfirmed_return_root_or_burn_keeps_payment_report_unconfirmed() {
        for unconfirmed in 0..3 {
            let mut origins = origin_reply();
            if unconfirmed == 1 {
                origins.1["confirmed"] = false.into();
            }
            if unconfirmed == 2 {
                origins.1["origins"][0]["confirmed"] = false.into();
            }
            let (client, server) =
                fixture(vec![source(4, "Send", 0, unconfirmed != 0), origins]).await;
            assert!(
                !client
                    .payment_origins(&"04".repeat(32))
                    .await
                    .unwrap()
                    .confirmed
            );
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn invalid_payment_types_cycles_and_excessive_return_depth_fail_closed() {
        for replies in [
            vec![source(4, "Receive", 0, true)],
            vec![source(4, "RejectReceive", 4, true)],
            vec![
                source(4, "RejectReceive", 5, true),
                source(5, "RejectReceive", 6, true),
            ],
        ] {
            let (client, server) = fixture(replies).await;
            assert!(client
                .resolve_payment_origins(&"04".repeat(32), 2)
                .await
                .is_err());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn a_burn_cannot_claim_a_different_or_merged_origin() {
        let (client, server) = fixture(vec![source(3, "Burn", 0, true), origin_reply()]).await;
        assert!(client.payment_origins(&"03".repeat(32)).await.is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_or_missing_payment_source_fields_are_not_guessed() {
        for field in ["block_type", "link", "confirmed", "origin"] {
            let mut reply = source(4, "Send", 0, true);
            reply.1.as_object_mut().unwrap().remove(field);
            let (client, server) = fixture(vec![reply]).await;
            assert!(client.payment_origins(&"04".repeat(32)).await.is_err());
            server.await.unwrap();
        }
        for (kind, field) in [("Send", "origin"), ("RejectReceive", "link")] {
            let mut reply = source(4, kind, 0, true);
            reply.1[field] = "not a hash".into();
            let (client, server) = fixture(vec![reply]).await;
            assert!(client.payment_origins(&"04".repeat(32)).await.is_err());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn identities_preserve_each_burn_and_revocation_without_inventing_weights() {
        let result = request(report()).await.unwrap();
        assert_eq!(result.origins.len(), 2);
        assert_eq!(result.origins[0].timestamp, 100);
        assert!(!result.origins[0].revoked);
        assert!(result.origins[1].revoked);
        assert!(result.confirmed);
    }

    #[tokio::test]
    async fn incomplete_empty_or_wrong_root_reports_cannot_be_used_as_complete_evidence() {
        let mut incomplete = report();
        incomplete["complete"] = false.into();
        let mut empty = report();
        empty["origins"] = json!([]);
        let mut wrong = report();
        wrong["origin"] = "04".repeat(32).into();
        for response in [incomplete, empty, wrong] {
            assert!(request(response).await.is_err());
        }
    }

    #[tokio::test]
    async fn malformed_duplicate_and_unsorted_origins_are_rejected() {
        let mut duplicate = report();
        duplicate["origins"][1] = duplicate["origins"][0].clone();
        let mut unsorted = report();
        unsorted["origins"].as_array_mut().unwrap().reverse();
        let mut address = report();
        address["origins"][0]["originator"] = "brst_invalid".into();
        let mut hash = report();
        hash["origins"][0]["hash"] = "bad".into();
        let mut missing = report();
        missing["origins"][0]
            .as_object_mut()
            .unwrap()
            .remove("revoked");
        for response in [duplicate, unsorted, address, hash, missing] {
            assert!(request(response).await.is_err());
        }
    }

    #[tokio::test]
    async fn unconfirmed_history_is_not_relabeled_confirmed() {
        let mut response = report();
        response["confirmed"] = false.into();
        response["origins"][0]["confirmed"] = false.into();
        let result = request(response).await.unwrap();
        assert!(!result.confirmed);
        assert!(!result.origins[0].confirmed);
    }
}
