use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub protocol_version: u16,
    #[serde(default = "default_api_version")]
    pub api_version: u16,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub label: Option<String>,
    pub kind: String,
    pub version: String,
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub subscriptions: Vec<String>,
    #[serde(default)]
    pub config_schema: ConfigSchema,
    #[serde(default)]
    pub storage_contract: Option<Value>,
}

fn default_api_version() -> u16 {
    2
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSchema {
    #[serde(default = "default_config_schema_version")]
    pub version: u16,
    #[serde(default)]
    pub fields: Vec<ConfigField>,
}

impl Default for ConfigSchema {
    fn default() -> Self {
        Self {
            version: 1,
            fields: Vec::new(),
        }
    }
}

fn default_config_schema_version() -> u16 {
    1
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigField {
    pub key: String,
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub name: String,
    pub domain: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginRequest {
    pub request_id: String,
    pub method: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub correlation_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginResponse {
    pub request_id: String,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<PluginError>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginError {
    pub code: String,
    #[serde(default)]
    pub side_effect_may_have_completed: bool,
}

impl PluginManifest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err("unsupported plugin protocol version");
        }
        if self.api_version != 2 {
            return Err("unsupported plugin API version");
        }
        if !valid_plugin_id(&self.id)
            || self.name.trim().is_empty()
            || self.version.trim().is_empty()
        {
            return Err("plugin manifest identity fields must not be empty");
        }
        if ![
            "resource-provider",
            "metadata-provider",
            "storage-provider",
            "indexer",
            "notification-provider",
            "subtitle-provider",
            "workflow-provider",
        ]
        .contains(&self.kind.as_str())
        {
            return Err("plugin kind is invalid");
        }
        if self.capabilities.iter().any(|capability| {
            !valid_capability_name(&capability.name) || !valid_domain(&capability.domain)
        }) {
            return Err("plugin capabilities must declare a name and domain");
        }
        if self
            .subscriptions
            .iter()
            .any(|event| !valid_subscription(event))
        {
            return Err("plugin event subscriptions are invalid");
        }
        if self.config_schema.version != 1 {
            return Err("unsupported plugin configuration schema version");
        }
        let mut keys = std::collections::HashSet::new();
        if self.config_schema.fields.iter().any(|field| {
            !valid_identifier(&field.key)
                || !["text", "url", "password"].contains(&field.field_type.as_str())
                || !keys.insert(field.key.as_str())
        }) {
            return Err("plugin configuration fields are invalid");
        }
        let mut capabilities = std::collections::HashSet::new();
        if self
            .capabilities
            .iter()
            .any(|capability| !capabilities.insert(&capability.name))
        {
            return Err("plugin capabilities must be unique");
        }
        let declared: std::collections::HashSet<&str> = self
            .capabilities
            .iter()
            .map(|item| item.name.as_str())
            .collect();
        let required: &[&str] = match self.kind.as_str() {
            "metadata-provider" => &["search", "detail", "episodes"],
            "resource-provider" => &["search"],
            "subtitle-provider" => &["search", "download"],
            "workflow-provider" => &["execute"],
            _ => &[],
        };
        if required
            .iter()
            .any(|capability| !declared.contains(capability))
        {
            return Err("plugin does not implement its required capabilities");
        }
        if self.kind == "storage-provider" {
            let contract = self
                .storage_contract
                .as_ref()
                .and_then(Value::as_object)
                .ok_or("storage plugin contract is required")?;
            let core = [
                "list",
                "listSharedFiles",
                "save",
                "move",
                "mkdir",
                "rename",
                "renameMany",
                "remove",
                "clean",
                "status",
            ];
            let core_values = contract
                .get("coreCapabilities")
                .and_then(Value::as_array)
                .ok_or("storage plugin contract is invalid")?;
            let actual_core: std::collections::HashSet<&str> =
                core_values.iter().filter_map(Value::as_str).collect();
            let remove_values = contract
                .get("removeModes")
                .and_then(Value::as_array)
                .ok_or("storage plugin contract is invalid")?;
            let remove_modes: std::collections::HashSet<&str> =
                remove_values.iter().filter_map(Value::as_str).collect();
            if contract.get("version").and_then(Value::as_u64) != Some(2)
                || core.iter().any(|capability| {
                    !declared.contains(capability) || !actual_core.contains(capability)
                })
                || actual_core.len() != core.len()
                || remove_modes.is_empty()
                || remove_modes
                    .iter()
                    .any(|mode| !["recycle", "permanent"].contains(mode))
                || contract.len() != 3
            {
                return Err("storage plugin contract is invalid");
            }
        } else if self.storage_contract.is_some() {
            return Err("only storage plugins may declare a storage contract");
        }
        Ok(())
    }
}

fn valid_plugin_id(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && (2..=64).contains(&value.len())
}

fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_lowercase())
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
        && value.len() <= 64
}

fn valid_capability_name(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_lowercase())
        && chars.all(|character| character.is_ascii_alphanumeric())
        && value.len() <= 64
}

fn valid_domain(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_subscription(value: &str) -> bool {
    value == "*"
        || (value.contains('.')
            && value.len() <= 128
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'-'
            }))
}
