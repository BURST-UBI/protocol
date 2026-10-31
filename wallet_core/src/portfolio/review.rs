use burst_types::TxHash;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq)]
pub struct MergeSelection {
    pub id: String,
    pub amount: String,
    pub expiry_at: Option<u64>,
    pub status: String,
}

/// Compare 2–256 selected batches with the supplied current list. Reject duplicate
/// IDs, changed fields, mixed active/expired status and invalid amount sums.
/// The caller supplies the list; this does not authenticate ownership or freshness.
pub fn reviewed_merge_sources(
    batches: &[serde_json::Value],
    selected: &[MergeSelection],
) -> Result<Vec<TxHash>, String> {
    if !(2..=256).contains(&selected.len()) {
        return Err("Choose between 2 and 256 batches.".into());
    }
    let status = &selected[0].status;
    if !matches!(status.as_str(), "active" | "expired")
        || selected.iter().any(|batch| &batch.status != status)
    {
        return Err(
            "Combine active or expired batches separately. Revoked TRST cannot be combined.".into(),
        );
    }
    // Keep duplicate IDs distinguishable from a unique match without rescanning
    // the node's entire response for each selected batch.
    let mut by_id = std::collections::HashMap::new();
    for batch in batches {
        if let Some(id) = batch["id"].as_str() {
            by_id.entry(id).and_modify(|value| *value = None).or_insert(Some(batch));
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u128;
    selected
        .iter()
        .map(|selection| {
            let id = crate::signed_client::parse_hash(&selection.id)
                .map_err(|error| error.to_string())?
                .into_tx_hash();
            if !seen.insert(id) {
                return Err("A batch was selected more than once.".into());
            }
            let current = by_id.get(selection.id.as_str()).copied().flatten()
                .ok_or("Selected batches changed. Refresh and review again.")?;
            let current: MergeSelection = serde_json::from_value(current.clone())
                .map_err(|_| "The node returned an invalid batch.".to_string())?;
            if current != *selection {
                return Err(
                    "Selected amounts, status or expiry changed. Refresh and review again.".into(),
                );
            }
            let amount = selection
                .amount
                .parse::<u128>()
                .map_err(|_| "Invalid batch amount")?;
            if amount == 0 {
                return Err("An empty batch cannot be combined.".into());
            }
            total = total
                .checked_add(amount)
                .ok_or("Combined amount overflow")?;
            Ok(id)
        })
        .collect()
}

/// Sum active batches by origin and choose one that covers the whole transfer.
/// Without an explicit origin, choose the first sufficient origin in string order,
/// not expiry order. Different origins are never combined implicitly.
pub fn select_transfer_origin(
    batches: &[serde_json::Value],
    amount: u128,
    origin: Option<&str>,
) -> Result<TxHash, String> {
    if amount == 0 {
        return Err("Amount must be positive.".into());
    }
    let mut available = std::collections::BTreeMap::<String, u128>::new();
    for batch in batches.iter().filter(|batch| batch["status"] == "active") {
        let id = batch["origin"]
            .as_str()
            .ok_or("Missing origin")?
            .to_string();
        let value = batch["amount"]
            .as_str()
            .and_then(|value| value.parse::<u128>().ok())
            .ok_or("Invalid batch amount")?;
        let total = available.entry(id).or_default();
        *total = total.checked_add(value).ok_or("TRST amount overflow")?;
    }
    let chosen = match origin {
        Some(origin) if available.get(origin).copied().unwrap_or(0) >= amount => origin.to_string(),
        Some(_) => return Err("Selected origin has insufficient active TRST".into()),
        None => available.into_iter().find(|(_,value)|*value >= amount).map(|(origin,_)|origin)
            .ok_or("No single origin covers this amount. Choose a smaller amount or merge compatible batches first.")?,
    };
    crate::signed_client::parse_hash(&chosen)
        .map(|hash| hash.into_tx_hash())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merge_review_rejects_duplicate_changed_and_overflowing_batches() {
        let selected: Vec<MergeSelection> = (1..=2).map(|byte| MergeSelection {
            id: TxHash::new([byte; 32]).to_string(), amount: "1".into(),
            expiry_at: Some(100), status: "active".into(),
        }).collect();
        let batches: Vec<_> = selected.iter().map(|s| serde_json::to_value(s).unwrap()).collect();
        assert_eq!(reviewed_merge_sources(&batches, &selected).unwrap().len(), 2);
        let mut duplicate = batches.clone();
        duplicate.push(batches[0].clone());
        assert!(reviewed_merge_sources(&duplicate, &selected).is_err());
        let mut changed = batches.clone();
        changed[0]["amount"] = serde_json::json!("2");
        assert!(reviewed_merge_sources(&changed, &selected).is_err());
        assert!(reviewed_merge_sources(&batches[1..], &selected).is_err());
        assert!(reviewed_merge_sources(&batches, &[selected[0].clone(), selected[0].clone()]).is_err());
        let mut overflow = selected;
        overflow[0].amount = u128::MAX.to_string();
        let batches: Vec<_> = overflow.iter().map(|s| serde_json::to_value(s).unwrap()).collect();
        assert!(reviewed_merge_sources(&batches, &overflow).is_err());
    }

    #[test]
    fn offline_trst_selection_preserves_active_origin_and_rejects_implicit_merge() {
        let origin = TxHash::new([1; 32]);
        let other = TxHash::new([2; 32]);
        let mut batches = vec![
            serde_json::json!({"origin":origin.to_string(),"amount":"3","status":"active"}),
            serde_json::json!({"origin":origin.to_string(),"amount":"4","status":"active"}),
            serde_json::json!({"origin":other.to_string(),"amount":"6","status":"active"}),
        ];
        assert_eq!(select_transfer_origin(&batches, 7, None).unwrap(), origin);
        assert!(select_transfer_origin(&batches, 8, None).is_err());
        assert!(select_transfer_origin(&batches, 0, None).is_err());
        for status in ["expired", "revoked"] {
            batches[1]["status"] = serde_json::json!(status);
            assert!(select_transfer_origin(&batches, 7, Some(&origin.to_string())).is_err());
        }
        batches[1]["status"] = serde_json::json!("active");
        batches[1]["amount"] = serde_json::json!(u128::MAX.to_string());
        assert!(select_transfer_origin(&batches, 1, None).is_err());
    }
}
