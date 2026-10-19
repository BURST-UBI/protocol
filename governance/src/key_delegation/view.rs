//! Public discovery data. Encrypted grants contain no plaintext signing keys.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ScopeChoice {
    Proposal(String),
    Parameter(String),
}

impl ScopeChoice {
    pub fn resolve(&self) -> Result<KeyScope, String> {
        match self {
            Self::Proposal(value) => {
                let bytes = hex::decode(value).map_err(|_| "invalid proposal ID")?;
                let hash = TxHash::new(bytes.try_into().map_err(|_| "invalid proposal ID length")?);
                if hash.is_zero() {
                    return Err("zero proposal ID".into());
                }
                Ok(KeyScope::Proposal(hash))
            }
            Self::Parameter(value) => GovernableParam::from_name(value)
                .map(KeyScope::Parameter)
                .map_err(|e| e.to_string()),
        }
    }
}
impl From<&KeyScope> for ScopeChoice {
    fn from(scope: &KeyScope) -> Self {
        match scope {
            KeyScope::Proposal(hash) => Self::Proposal(hash.to_string()),
            KeyScope::Parameter(param) => Self::Parameter(param.name().into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    #[default]
    Outgoing,
    Incoming,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Query {
    pub account: String,
    #[serde(default)]
    pub direction: Direction,
    pub scope: Option<ScopeChoice>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub owner: String,
    pub scope: ScopeChoice,
    pub generation: String,
    pub delegate: Option<String>,
    pub grant_block: Option<String>,
    pub grant: Option<Grant>,
    pub confirmed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub enabled: bool,
    pub network: NetworkId,
    pub genesis: String,
    pub entries: Vec<Entry>,
    pub cursor: Option<String>,
    pub parameters: Vec<String>,
}

pub fn index_prefix(direction: Direction, account: &WalletAddress) -> Result<String, String> {
    burst_crypto::decode_address(account.as_str()).ok_or("invalid delegation account")?;
    let direction = match direction {
        Direction::Outgoing => "outgoing",
        Direction::Incoming => "incoming",
    };
    Ok(format!("{}{direction}/{account}/", scoped::PREFIX))
}

pub fn index_key(
    direction: Direction,
    account: &WalletAddress,
    scope_key: &str,
) -> Result<String, String> {
    let suffix = scope_key
        .strip_prefix(scoped::PREFIX)
        .ok_or("invalid scope key")?;
    if suffix.len() != 64 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid scope key".into());
    }
    Ok(format!("{}{suffix}", index_prefix(direction, account)?))
}
