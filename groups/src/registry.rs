//! Group registry — discover and manage known groups.

use crate::error::GroupError;
use crate::types::GroupInfo;
use serde::Deserialize;
use std::collections::HashMap;

pub const MAX_DIRECTORY_BYTES: usize = 1024 * 1024;

/// Registry of known groups and their endpoints.
#[derive(Debug)]
pub struct GroupRegistry {
    groups: HashMap<String, GroupInfo>,
}

#[derive(Deserialize)]
struct Directory {
    version: u32,
    groups: Vec<GroupInfo>,
}

impl GroupRegistry {
    pub fn new() -> Self {
        Self {
            groups: HashMap::new(),
        }
    }

    /// Register a new group.
    pub fn register(&mut self, info: GroupInfo) -> Result<(), GroupError> {
        if info.id.trim().is_empty()
            || info.id.len() > 128
            || info.id != info.id.trim()
            || info.id.chars().any(char::is_control)
            || info.name.trim().is_empty()
            || info.name.len() > 128
            || info.name.chars().any(char::is_control)
            || info.description.len() > 4096
        {
            return Err(GroupError::InvalidResponse(
                "invalid group identifier, name or description".into(),
            ));
        }
        crate::client::endpoint_url_checked(&info.endpoint_url)?;
        if self.groups.contains_key(&info.id) {
            return Err(GroupError::InvalidResponse(format!(
                "duplicate group identifier: {}",
                info.id
            )));
        }
        self.groups.insert(info.id.clone(), info);
        Ok(())
    }

    /// Look up a group by ID.
    pub fn get(&self, group_id: &str) -> Option<&GroupInfo> {
        self.groups.get(group_id)
    }

    /// Remove a group.
    pub fn unregister(&mut self, group_id: &str) -> Option<GroupInfo> {
        self.groups.remove(group_id)
    }

    /// List all registered groups.
    pub fn list(&self) -> Vec<&GroupInfo> {
        let mut entries: Vec<_> = self.groups.values().collect();
        entries.sort_by(|left, right| left.id.cmp(&right.id));
        entries
    }

    /// An untrusted listing, not a trust-policy update. Reject the entire listing
    /// if any entry is invalid; never silently replace a selected group's URL.
    pub fn from_directory(bytes: &[u8]) -> Result<Self, GroupError> {
        if bytes.len() > MAX_DIRECTORY_BYTES {
            return Err(GroupError::InvalidResponse(
                "group directory exceeds 1 MiB".into(),
            ));
        }
        let directory: Directory = serde_json::from_slice(bytes)
            .map_err(|error| GroupError::InvalidResponse(error.to_string()))?;
        if directory.version != 1 {
            return Err(GroupError::InvalidResponse(
                "unsupported group directory version".into(),
            ));
        }
        let mut registry = Self::new();
        for group in directory.groups {
            registry.register(group)?;
        }
        Ok(registry)
    }

    /// Export a deterministic public listing using the same limits as import.
    pub fn to_directory(&self) -> Result<Vec<u8>, GroupError> {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "groups": self.list(),
        }))
        .map_err(|error| GroupError::InvalidResponse(error.to_string()))?;
        if bytes.len() > MAX_DIRECTORY_BYTES {
            return Err(GroupError::InvalidResponse(
                "group directory exceeds 1 MiB".into(),
            ));
        }
        Ok(bytes)
    }
}

impl Default for GroupRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn group(id: &str) -> GroupInfo {
        GroupInfo {
            id: id.into(),
            name: "Local community".into(),
            description: "Community verification".into(),
            endpoint_url: "https://example.test/group".into(),
            member_count: 42,
        }
    }

    #[test]
    fn directory_is_versioned_sorted_and_all_or_nothing() {
        let bytes =
            serde_json::to_vec(&json!({"version":1,"groups":[group("z"),group("a")]})).unwrap();
        let registry = GroupRegistry::from_directory(&bytes).unwrap();
        assert_eq!(
            registry
                .list()
                .iter()
                .map(|group| group.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "z"]
        );
        for invalid in [
            json!({"version":2,"groups":[]}),
            json!({"groups":[]}),
            json!({"version":1,"groups":[group("a"),group("a")]}),
            json!({"version":1,"groups":[group("a"),group("")]}),
        ] {
            assert!(GroupRegistry::from_directory(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
        assert!(GroupRegistry::from_directory(&vec![b' '; 1024 * 1024 + 1]).is_err());
    }

    #[test]
    fn export_is_stable_and_never_exceeds_the_import_limit() {
        let mut registry = GroupRegistry::new();
        registry.register(group("z")).unwrap();
        registry.register(group("a")).unwrap();
        let bytes = registry.to_directory().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["groups"][0]["id"], "a");
        assert_eq!(value["groups"][1]["id"], "z");
        assert_eq!(
            GroupRegistry::from_directory(&bytes)
                .unwrap()
                .to_directory()
                .unwrap(),
            bytes
        );
        for index in 0..300 {
            let mut entry = group(&format!("group-{index}"));
            entry.description = "x".repeat(4096);
            registry.register(entry).unwrap();
        }
        assert!(registry.to_directory().is_err());
    }

    #[test]
    fn imports_cannot_silently_replace_a_selected_groups_endpoint() {
        let mut registry = GroupRegistry::new();
        registry.register(group("local")).unwrap();
        let mut replacement = group("local");
        replacement.endpoint_url = "https://different.example.test".into();
        assert!(registry.register(replacement).is_err());
        assert_eq!(
            registry.get("local").unwrap().endpoint_url,
            "https://example.test/group"
        );
        assert!(registry.unregister("local").is_some());
        assert!(registry.get("local").is_none());
    }

    #[test]
    fn invalid_endpoints_and_unbounded_display_fields_are_rejected() {
        for endpoint in [
            "file:///tmp/group",
            "https://user:password@example.test",
            "https://example.test?q=1",
            "https://example.test#fragment",
        ] {
            let mut info = group("local");
            info.endpoint_url = endpoint.into();
            assert!(GroupRegistry::new().register(info).is_err());
        }
        for info in [group(" "), group("a\nb"), group(&"x".repeat(129))] {
            assert!(GroupRegistry::new().register(info).is_err());
        }
        let mut info = group("local");
        info.name = " ".into();
        assert!(GroupRegistry::new().register(info).is_err());
        let mut info = group("local");
        info.description = "x".repeat(4097);
        assert!(GroupRegistry::new().register(info).is_err());
        assert!(GroupRegistry::new()
            .register(group("Helsinki yhteisö"))
            .is_ok());
    }
}
