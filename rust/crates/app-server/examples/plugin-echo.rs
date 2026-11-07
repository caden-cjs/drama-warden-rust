use std::io::{self, BufRead, Write};

use drama_warden_plugin_protocol::{
    Capability, ConfigField, ConfigSchema, PROTOCOL_VERSION, PluginManifest, PluginRequest,
    PluginResponse,
};
use serde_json::{Value, json};

fn manifest() -> PluginManifest {
    PluginManifest {
        protocol_version: PROTOCOL_VERSION,
        api_version: 2,
        id: "example-echo".to_owned(),
        name: "Example echo plugin".to_owned(),
        label: Some("Echo example".to_owned()),
        kind: "indexer".to_owned(),
        version: "0.1.0".to_owned(),
        capabilities: vec![Capability {
            name: "echo".to_owned(),
            domain: "indexer".to_owned(),
            description: "Returns a small synthetic response".to_owned(),
        }],
        subscriptions: Vec::new(),
        config_schema: ConfigSchema {
            version: 1,
            fields: vec![ConfigField {
                key: "label".to_owned(),
                field_type: "text".to_owned(),
                required: false,
            }],
        },
        storage_contract: None,
    }
}

fn handle(request: PluginRequest) -> PluginResponse {
    let result = match request.method.as_str() {
        "manifest" => serde_json::to_value(manifest()).ok(),
        "initialize" | "reset" => Some(json!({ "ok": true })),
        "monitor" => Some(json!({ "ok": true, "configured": true })),
        "invoke" => Some(json!({
            "capability": request.input.get("capability").cloned().unwrap_or(Value::Null),
            "echo": request.input.get("input").cloned().unwrap_or(Value::Null),
        })),
        _ => {
            return PluginResponse {
                request_id: request.request_id,
                result: None,
                error: Some(drama_warden_plugin_protocol::PluginError {
                    code: "METHOD_UNSUPPORTED".to_owned(),
                    side_effect_may_have_completed: false,
                }),
            };
        }
    };
    PluginResponse {
        request_id: request.request_id,
        result,
        error: None,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line?;
        let response = match serde_json::from_str::<PluginRequest>(&line) {
            Ok(request) => handle(request),
            Err(_) => continue,
        };
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}
