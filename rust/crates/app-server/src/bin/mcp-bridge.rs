use std::env;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const TOOLS: &[(&str, &str, &str)] = &[
    (
        "list_shows",
        "List tracked TV shows in Drama Warden.",
        r#"{"type":"object","properties":{},"additionalProperties":false}"#,
    ),
    (
        "get_show",
        "Read one tracked show and its linked metadata identities.",
        r#"{"type":"object","properties":{"showId":{"type":"integer","minimum":1}},"required":["showId"],"additionalProperties":false}"#,
    ),
    (
        "search_metadata",
        "Search metadata providers. This does not link a candidate.",
        r#"{"type":"object","properties":{"query":{"type":"string","minLength":1,"maxLength":200},"mediaType":{"type":"string","enum":["tv","movie"]},"limit":{"type":"integer","minimum":1,"maximum":50}},"required":["query"],"additionalProperties":false}"#,
    ),
    (
        "search_resources",
        "Search resource providers and preview candidates without queueing or saving media.",
        r#"{"type":"object","properties":{"showId":{"type":"integer","minimum":1},"storageId":{"type":"integer","minimum":1},"query":{"type":"string","minLength":1,"maxLength":200},"limit":{"type":"integer","minimum":1,"maximum":20}},"required":["showId","storageId"],"additionalProperties":false}"#,
    ),
    (
        "preview_followup",
        "Preview a share. This does not save media.",
        r#"{"type":"object","properties":{"showId":{"type":"integer","minimum":1},"storageId":{"type":"integer","minimum":1},"shareUrl":{"type":"string","minLength":1,"maxLength":4096},"shareTitle":{"type":"string","maxLength":500}},"required":["showId","storageId","shareUrl"],"additionalProperties":false}"#,
    ),
    (
        "queue_followup",
        "Queue confirmed missing files and reserve episode write claims; does not save media.",
        r#"{"type":"object","properties":{"showId":{"type":"integer","minimum":1},"storageId":{"type":"integer","minimum":1},"shareUrl":{"type":"string","minLength":1,"maxLength":4096},"shareTitle":{"type":"string","maxLength":500}},"required":["showId","storageId","shareUrl"],"additionalProperties":false}"#,
    ),
    (
        "list_events",
        "List follow-up events.",
        r#"{"type":"object","properties":{"status":{"type":"string","enum":["pending","processing","processed","skipped","failed"]},"limit":{"type":"integer","minimum":1,"maximum":1000}},"additionalProperties":false}"#,
    ),
    (
        "retry_event",
        "Retry an eligible failed event. Uncertain writes remain blocked.",
        r#"{"type":"object","properties":{"eventId":{"type":"integer","minimum":1}},"required":["eventId"],"additionalProperties":false}"#,
    ),
    (
        "process_event",
        "Process a pending follow-up event. This writes selected media.",
        r#"{"type":"object","properties":{"eventId":{"type":"integer","minimum":1}},"required":["eventId"],"additionalProperties":false}"#,
    ),
    (
        "automation_status",
        "Read periodic scheduler settings and latest scan summary.",
        r#"{"type":"object","properties":{},"additionalProperties":false}"#,
    ),
    (
        "scan_automation",
        "Run one-shot candidate preview; does not queue events or save media.",
        r#"{"type":"object","properties":{},"additionalProperties":false}"#,
    ),
];

fn error_response(id: Value, code: &str, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":message,"data":{"code":code}}})
}

fn api_request(name: &str, args: &Value) -> Result<(String, String, Value), &'static str> {
    let object = args.as_object().ok_or("invalid_params")?;
    let int = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_i64)
            .filter(|n| *n > 0)
            .ok_or("invalid_params")
    };
    let string = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or("invalid_params")
    };
    let (method, path, body) = match name {
        "list_shows" => ("GET", "/api/shows".to_owned(), None),
        "get_show" => ("GET", format!("/api/shows/{}", int("showId")?), None),
        "search_metadata" => {
            let query = string("query")?;
            let mut params = vec![("q", query.to_owned())];
            if let Some(v) = object.get("mediaType").and_then(Value::as_str) {
                params.push(("mediaType", v.to_owned()));
            }
            if let Some(v) = object.get("limit").and_then(Value::as_i64) {
                params.push(("limit", v.to_string()));
            }
            let encoded = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(params)
                .finish();
            ("GET", format!("/api/metadata/search?{encoded}"), None)
        }
        "search_resources" => (
            "POST",
            format!("/api/shows/{}/resources/search", int("showId")?),
            Some(without(args, "showId")),
        ),
        "preview_followup" => (
            "POST",
            format!("/api/shows/{}/followup/preview", int("showId")?),
            Some(without(args, "showId")),
        ),
        "queue_followup" => (
            "POST",
            format!("/api/shows/{}/followup/queue", int("showId")?),
            Some(without(args, "showId")),
        ),
        "list_events" => {
            let mut params = Vec::new();
            if let Some(v) = object.get("status").and_then(Value::as_str) {
                params.push(("status", v.to_owned()));
            }
            if let Some(v) = object.get("limit").and_then(Value::as_i64) {
                params.push(("limit", v.to_string()));
            }
            let query = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(params)
                .finish();
            (
                "GET",
                if query.is_empty() {
                    "/api/events".into()
                } else {
                    format!("/api/events?{query}")
                },
                None,
            )
        }
        "retry_event" => (
            "POST",
            format!("/api/events/{}/retry", int("eventId")?),
            Some(json!({})),
        ),
        "process_event" => (
            "POST",
            format!("/api/events/{}/process", int("eventId")?),
            Some(json!({})),
        ),
        "automation_status" => ("GET", "/api/automation/status".into(), None),
        "scan_automation" => ("POST", "/api/automation/scan".into(), Some(json!({}))),
        _ => return Err("unknown_tool"),
    };
    Ok((method.into(), path, body.unwrap_or(Value::Null)))
}

fn without(value: &Value, key: &str) -> Value {
    let mut output = value.clone();
    if let Some(map) = output.as_object_mut() {
        map.remove(key);
    }
    output
}

async fn handle(
    message: Value,
    client: &reqwest::Client,
    base: &url::Url,
    token: &str,
) -> Option<Value> {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Some(error_response(
            id,
            "invalid_request",
            "Invalid JSON-RPC request",
        ));
    };
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    match method {
        "notifications/initialized" | "notifications/cancelled" => None,
        "ping" => Some(json!({"jsonrpc":"2.0","id":id,"result":{}})),
        "initialize" => Some(
            json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"drama-warden","version":"0.1.0"}}}),
        ),
        "tools/list" => {
            let tools: Vec<Value> = TOOLS.iter().map(|(name, description, schema)| json!({"name":name,"description":description,"inputSchema":serde_json::from_str::<Value>(schema).unwrap()})).collect();
            Some(json!({"jsonrpc":"2.0","id":id,"result":{"tools":tools}}))
        }
        "tools/call" => {
            let tool = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let route = match api_request(tool, &args) {
                Ok(route) => route,
                Err(code) => {
                    return Some(
                        json!({"jsonrpc":"2.0","id":id,"result":{"isError":true,"content":[{"type":"text","text":json!({"error":"Invalid tool arguments","code":code}).to_string()}]}}),
                    );
                }
            };
            let (verb, path, body) = route;
            let url = match base.join(&path) {
                Ok(url) => url,
                Err(_) => return Some(error_response(id, "invalid_url", "Invalid API URL")),
            };
            let mut request = client
                .request(verb.parse().unwrap(), url)
                .bearer_auth(token);
            if !body.is_null() {
                request = request.json(&body);
            }
            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    let data = response.json::<Value>().await.unwrap_or_else(|_| json!({}));
                    if status.is_success() {
                        Some(
                            json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":data.to_string()}]}}),
                        )
                    } else {
                        let code = data
                            .get("code")
                            .and_then(Value::as_str)
                            .unwrap_or("MCP_REQUEST_FAILED");
                        let result = json!({"isError":true,"content":[{"type":"text","text":json!({"error":"Drama Warden API request failed","code":code}).to_string()}]});
                        Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
                    }
                }
                Err(_) => Some(error_response(
                    id,
                    "MCP_REQUEST_FAILED",
                    "Drama Warden API request failed",
                )),
            }
        }
        _ => Some(
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
        ),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !matches!(
        env::var("ENABLE_MCP")
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Ok("true" | "1")
    ) {
        return Err("MCP bridge is disabled; set ENABLE_MCP=true".into());
    }
    let token = env::var("API_ACCESS_TOKEN")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or("API_ACCESS_TOKEN is required")?;
    let base = url::Url::parse(
        &env::var("DRAMA_WARDEN_API_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into()),
    )?;
    if !["http", "https"].contains(&base.scheme())
        || base.username() != ""
        || base.password().is_some()
    {
        return Err(
            "DRAMA_WARDEN_API_URL must be an HTTP(S) URL without embedded credentials".into(),
        );
    }
    let client = reqwest::Client::new();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.len() > 1_048_576 {
            return Err("MCP message exceeds 1 MiB".into());
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(message, &client, &base, &token).await,
            Err(_) => Some(
                json!({"jsonrpc":"2.0","id":Value::Null,"error":{"code":-32700,"message":"Parse error"}}),
            ),
        };
        if let Some(response) = response {
            stdout.write_all(response.to_string().as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_followup_tools_to_the_shared_api_routes() {
        let input = json!({"showId":7,"storageId":3,"shareUrl":"https://share.invalid/x","shareTitle":"Example"});
        assert_eq!(
            api_request("preview_followup", &input).unwrap(),
            (
                "POST".into(),
                "/api/shows/7/followup/preview".into(),
                json!({"storageId":3,"shareUrl":"https://share.invalid/x","shareTitle":"Example"}),
            )
        );
        assert_eq!(
            api_request("queue_followup", &input).unwrap().1,
            "/api/shows/7/followup/queue"
        );
        assert_eq!(
            api_request(
                "search_resources",
                &json!({"showId":7,"storageId":3,"query":"Example"})
            )
            .unwrap()
            .2,
            json!({"storageId":3,"query":"Example"})
        );
    }

    #[test]
    fn rejects_invalid_ids_and_unknown_tools_before_http_requests() {
        assert_eq!(
            api_request("get_show", &json!({"showId":0})),
            Err("invalid_params")
        );
        assert_eq!(
            api_request("process_event", &json!({"eventId":"4"})),
            Err("invalid_params")
        );
        assert_eq!(
            api_request("delete_everything", &json!({})),
            Err("unknown_tool")
        );
    }

    #[test]
    fn lists_only_the_documented_mcp_tools() {
        let names: Vec<_> = TOOLS.iter().map(|tool| tool.0).collect();
        assert_eq!(names.len(), 11);
        assert!(!names.iter().any(|name| name.contains("delete")));
    }
}
