//! Group registry — discover and manage known groups.

use crate::error::GroupError;
use crate::types::GroupInfo;
use std::collections::HashMap;

/// Registry of known groups and their endpoints.
pub struct GroupRegistry {
    groups: HashMap<String, GroupInfo>,
}

impl GroupRegistry {
    pub fn new() -> Self {
        Self {
            groups: HashMap::new(),
        }
    }

    /// Register a new group.
    pub fn register(&mut self, info: GroupInfo) -> Result<(), GroupError> {
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
        self.groups.values().collect()
    }
}

impl Default for GroupRegistry {
    fn default() -> Self {
        Self::new()
    }
}
