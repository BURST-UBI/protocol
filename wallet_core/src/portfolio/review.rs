use burst_types::TxHash;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq)]
pub struct MergeSelection {
    pub id: String,
    pub amount: String,
    pub expiry_at: Option<u64>,
    pub status: String,
}

pub fn reviewed_merge_sources(batches: &[serde_json::Value], selected: &[MergeSelection]) -> Result<Vec<TxHash>, String> {
    if !(2..=256).contains(&selected.len()) {
        return Err("Choose between 2 and 256 batches.".into());
    }
    let status = &selected[0].status;
    if !matches!(status.as_str(), "active" | "expired") || selected.iter().any(|batch| &batch.status != status) {
        return Err("Combine active or expired batches separately. Revoked TRST cannot be combined.".into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u128;
    selected.iter().map(|selection| {
        let id = crate::signed_client::parse_hash(&selection.id)
            .map_err(|error| error.to_string())?.into_tx_hash();
        if !seen.insert(id) { return Err("A batch was selected more than once.".into()); }
        let matches = batches.iter().filter(|batch| batch["id"].as_str() == Some(selection.id.as_str())).collect::<Vec<_>>();
        if matches.len() != 1 { return Err("Selected batches changed. Refresh and review again.".into()); }
        let current: MergeSelection = serde_json::from_value(matches[0].clone())
            .map_err(|_| "The node returned an invalid batch.".to_string())?;
        if current != *selection { return Err("Selected amounts, status or expiry changed. Refresh and review again.".into()); }
        let amount = selection.amount.parse::<u128>().map_err(|_| "Invalid batch amount")?;
        if amount == 0 { return Err("An empty batch cannot be combined.".into()); }
        total = total.checked_add(amount).ok_or("Combined amount overflow")?;
        Ok(id)
    }).collect()
}

pub fn select_transfer_origin(batches: &[serde_json::Value], amount: u128, origin: Option<&str>) -> Result<TxHash, String> {
    if amount == 0 { return Err("Amount must be positive.".into()); }
    let mut available = std::collections::BTreeMap::<String, u128>::new();
    for batch in batches.iter().filter(|batch| batch["status"] == "active") {
        let id = batch["origin"].as_str().ok_or("Missing origin")?.to_string();
        let value = batch["amount"].as_str().and_then(|value| value.parse::<u128>().ok()).ok_or("Invalid batch amount")?;
        let total = available.entry(id).or_default();
        *total = total.checked_add(value).ok_or("TRST amount overflow")?;
    }
    let chosen = match origin {
        Some(origin) if available.get(origin).copied().unwrap_or(0) >= amount => origin.to_string(),
        Some(_) => return Err("Selected origin has insufficient active TRST".into()),
        None => available.into_iter().find(|(_,value)|*value >= amount).map(|(origin,_)|origin)
            .ok_or("No single origin covers this amount. Choose a smaller amount or merge compatible batches first.")?,
    };
    crate::signed_client::parse_hash(&chosen).map(|hash| hash.into_tx_hash()).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offline_trst_selection_preserves_active_origin_and_rejects_implicit_merge() {
        let origin=TxHash::new([1;32]);let other=TxHash::new([2;32]);
        let mut batches=vec![serde_json::json!({"origin":origin.to_string(),"amount":"3","status":"active"}),
            serde_json::json!({"origin":origin.to_string(),"amount":"4","status":"active"}),
            serde_json::json!({"origin":other.to_string(),"amount":"6","status":"active"})];
        assert_eq!(select_transfer_origin(&batches,7,None).unwrap(),origin);
        assert!(select_transfer_origin(&batches,8,None).is_err());
        assert!(select_transfer_origin(&batches,0,None).is_err());
        for status in ["expired","revoked"] {
            batches[1]["status"]=serde_json::json!(status);
            assert!(select_transfer_origin(&batches,7,Some(&origin.to_string())).is_err());
        }
        batches[1]["status"]=serde_json::json!("active");
        batches[1]["amount"]=serde_json::json!(u128::MAX.to_string());
        assert!(select_transfer_origin(&batches,1,None).is_err());
    }
}
