//! Local stdio MCP adapter for Rue's existing machine-readable authorities.
//!
//! The server deliberately does not embed a compiler pipeline, project loader,
//! diagnostic formatter, error registry, or specification parser. Compile
//! requests execute the real `rue --error-format json` driver; specification
//! queries execute `rue-spec --machine-index`; and explanations project the
//! compiler-owned `rue-error` records directly.

use rue_error::{ErrorCode, VERSION, error_code_explanation, error_code_metadata};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

const PROTOCOL_VERSION: &str = "2026-07-28";
const SERVER_NAME: &str = "rue-mcp";
const PROTOCOL_VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CLIENT_CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
const CLIENT_INFO_KEY: &str = "io.modelcontextprotocol/clientInfo";
const SERVER_INFO_KEY: &str = "io.modelcontextprotocol/serverInfo";
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
const SERVER_BUSY: i64 = -32000;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_PRODUCER_STREAM_BYTES: usize = 8 * 1024 * 1024;
const MAX_IN_FLIGHT: usize = 8;
const MAX_CAPTURE_READERS: usize = MAX_IN_FLIGHT * 2;
const CACHE_TTL_MS: u64 = 3_600_000;

static ACTIVE_CAPTURE_READERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

struct RequestControl {
    state: Mutex<RequestState>,
}

struct RequestState {
    child: Option<Child>,
    phase: RequestPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestPhase {
    Open,
    Cancelled,
    ResponseClaimed,
}

type Requests = Arc<Mutex<HashMap<String, Arc<RequestControl>>>>;
type Output = Arc<Mutex<Box<dyn Write + Send>>>;

enum IncomingMessage {
    Eof,
    TooLarge,
    Bytes(Vec<u8>),
}

fn main() {
    let output: Output = Arc::new(Mutex::new(Box::new(io::stdout())));
    let requests: Requests = Arc::new(Mutex::new(HashMap::new()));
    let shutting_down = Arc::new(AtomicBool::new(false));
    let mut workers = Vec::new();
    let mut input = io::stdin().lock();

    loop {
        reap_finished(&mut workers);
        let bytes = match read_message(&mut input) {
            Ok(IncomingMessage::Eof) => break,
            Ok(IncomingMessage::TooLarge) => {
                write_response(
                    &output,
                    rpc_error(
                        Value::Null,
                        INVALID_REQUEST,
                        format!("MCP message exceeds {MAX_MESSAGE_BYTES} bytes"),
                    ),
                );
                continue;
            }
            Ok(IncomingMessage::Bytes(bytes)) => bytes,
            Err(error) => {
                write_response(
                    &output,
                    rpc_error(Value::Null, INTERNAL_ERROR, error.to_string()),
                );
                break;
            }
        };
        let message = match serde_json::from_slice::<Value>(&bytes) {
            Ok(request) => request,
            Err(error) => {
                write_response(&output, rpc_error(Value::Null, -32700, error.to_string()));
                continue;
            }
        };
        let Some(object) = message.as_object() else {
            write_response(
                &output,
                rpc_error(Value::Null, INVALID_REQUEST, "request must be an object"),
            );
            continue;
        };
        // JSON-RPC notifications never receive responses, including when
        // malformed. Only a fully valid cancellation may mutate request state.
        if object.get("id").is_none() && object.contains_key("method") {
            if object.get("method").and_then(Value::as_str) == Some("notifications/cancelled") {
                let _ = cancel_request(&message, &requests);
            }
            continue;
        }
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || !object.get("method").is_some_and(Value::is_string)
        {
            write_response(
                &output,
                rpc_error(
                    object.get("id").cloned().unwrap_or(Value::Null),
                    INVALID_REQUEST,
                    "malformed JSON-RPC request or notification",
                ),
            );
            continue;
        }

        let id = object.get("id").cloned().unwrap_or(Value::Null);
        if !valid_id(&id) {
            write_response(
                &output,
                rpc_error(
                    Value::Null,
                    INVALID_REQUEST,
                    "id must be a string or number",
                ),
            );
            continue;
        }
        let key = id_key(&id);
        let control = Arc::new(RequestControl {
            state: Mutex::new(RequestState {
                child: None,
                phase: RequestPhase::Open,
            }),
        });
        {
            let mut active = requests.lock().expect("request registry poisoned");
            if active.contains_key(&key) {
                write_response(
                    &output,
                    rpc_error(id, INVALID_REQUEST, "duplicate in-flight request id"),
                );
                continue;
            }
            if active.len() >= MAX_IN_FLIGHT {
                write_response(
                    &output,
                    rpc_error(id, SERVER_BUSY, "too many in-flight requests"),
                );
                continue;
            }
            active.insert(key.clone(), Arc::clone(&control));
        }

        let output = Arc::clone(&output);
        let requests = Arc::clone(&requests);
        let shutting_down = Arc::clone(&shutting_down);
        workers.push(thread::spawn(move || {
            dispatch(message, output, control, shutting_down);
            requests
                .lock()
                .expect("request registry poisoned")
                .remove(&key);
        }));
    }

    shutting_down.store(true, Ordering::Relaxed);
    for control in requests.lock().expect("request registry poisoned").values() {
        cancel_control(control);
    }
    for worker in workers {
        let _ = worker.join();
    }
}

fn read_message(reader: &mut impl BufRead) -> io::Result<IncomingMessage> {
    let mut bytes = Vec::new();
    let mut too_large = false;
    let mut saw_input = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if saw_input {
                Ok(if too_large {
                    IncomingMessage::TooLarge
                } else {
                    IncomingMessage::Bytes(bytes)
                })
            } else {
                Ok(IncomingMessage::Eof)
            };
        }
        saw_input = true;
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if !too_large {
            if bytes.len().saturating_add(consumed) > MAX_MESSAGE_BYTES {
                too_large = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(&available[..consumed]);
            }
        }
        let found_newline = available.get(consumed.wrapping_sub(1)) == Some(&b'\n');
        reader.consume(consumed);
        if found_newline {
            return Ok(if too_large {
                IncomingMessage::TooLarge
            } else {
                IncomingMessage::Bytes(bytes)
            });
        }
    }
}

fn reap_finished(workers: &mut Vec<thread::JoinHandle<()>>) {
    let mut index = 0;
    while index < workers.len() {
        if workers[index].is_finished() {
            let worker = workers.swap_remove(index);
            let _ = worker.join();
        } else {
            index += 1;
        }
    }
}

fn dispatch(
    request: Value,
    output: Output,
    control: Arc<RequestControl>,
    shutting_down: Arc<AtomicBool>,
) {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let response = match validate_request(&request) {
        Err(response) => response,
        Ok(method) => match method {
            "server/discover" => rpc_result(id, discover_result()),
            "tools/list" => rpc_result(id, tools_result()),
            "tools/call" => {
                let result = call_tool(&request, id.clone(), &control, &shutting_down);
                // A cancelled request is no longer awaited by its sender. MCP
                // 2026-07-28 forbids any later response for that request.
                if result.is_null() {
                    return;
                }
                if result.get("jsonrpc").is_some() {
                    result
                } else {
                    rpc_result(id, result)
                }
            }
            _ => rpc_error(id, METHOD_NOT_FOUND, "method not found"),
        },
    };
    if claim_response_for_dispatch(&control) {
        write_response(&output, response);
    }
}

fn validate_request(request: &Value) -> Result<&str, Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(object) = request.as_object() else {
        return Err(rpc_error(id, INVALID_REQUEST, "request must be an object"));
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(rpc_error(id, INVALID_REQUEST, "jsonrpc must be 2.0"));
    }
    if !valid_id(&id) {
        return Err(rpc_error(
            Value::Null,
            INVALID_REQUEST,
            "id must be a string or number",
        ));
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Err(rpc_error(id, INVALID_REQUEST, "method must be a string"));
    };
    let Some(params) = object.get("params").and_then(Value::as_object) else {
        return Err(rpc_error(id, INVALID_PARAMS, "params must be an object"));
    };
    let Some(meta) = params.get("_meta").and_then(Value::as_object) else {
        return Err(rpc_error(id, INVALID_PARAMS, "params._meta is required"));
    };
    let Some(requested_version) = meta.get(PROTOCOL_VERSION_KEY).and_then(Value::as_str) else {
        return Err(rpc_error(
            id,
            INVALID_PARAMS,
            format!("params._meta.{PROTOCOL_VERSION_KEY} must be a string"),
        ));
    };
    if requested_version != PROTOCOL_VERSION {
        return Err(rpc_error_data(
            id,
            UNSUPPORTED_PROTOCOL_VERSION,
            "unsupported MCP protocol version",
            json!({
            "supported": [PROTOCOL_VERSION],
            "requested": requested_version,
            }),
        ));
    }
    if let Err(error) = validate_request_metadata(meta) {
        return Err(rpc_error(id, INVALID_PARAMS, error));
    }
    if let Err(error) = validate_method_params(method, params) {
        return Err(rpc_error(id, INVALID_PARAMS, error));
    }
    Ok(method)
}

fn validate_request_metadata(meta: &Map<String, Value>) -> Result<(), String> {
    let capabilities = meta
        .get(CLIENT_CAPABILITIES_KEY)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("params._meta.{CLIENT_CAPABILITIES_KEY} must be an object"))?;
    for name in [
        "elicitation",
        "experimental",
        "extensions",
        "roots",
        "sampling",
    ] {
        if capabilities
            .get(name)
            .is_some_and(|value| !value.is_object())
        {
            return Err(format!("client capability {name} must be an object"));
        }
    }
    for (capability, members) in [
        ("elicitation", &["form", "url"][..]),
        ("sampling", &["context", "tools"][..]),
    ] {
        if let Some(object) = capabilities.get(capability).and_then(Value::as_object) {
            for member in members {
                if object.get(*member).is_some_and(|value| !value.is_object()) {
                    return Err(format!(
                        "client capability {capability}.{member} must be an object"
                    ));
                }
            }
        }
    }
    for capability in ["experimental", "extensions"] {
        if let Some(object) = capabilities.get(capability).and_then(Value::as_object) {
            if object.values().any(|value| !value.is_object()) {
                return Err(format!(
                    "client capability {capability} values must be objects"
                ));
            }
        }
    }
    if let Some(client_info) = meta.get(CLIENT_INFO_KEY) {
        let Some(client_info) = client_info.as_object() else {
            return Err(format!("params._meta.{CLIENT_INFO_KEY} must be an object"));
        };
        if !client_info.get("name").is_some_and(Value::is_string)
            || !client_info.get("version").is_some_and(Value::is_string)
        {
            return Err(format!(
                "params._meta.{CLIENT_INFO_KEY} requires string name and version"
            ));
        }
        for field in ["description", "title", "websiteUrl"] {
            if client_info
                .get(field)
                .is_some_and(|value| !value.is_string())
            {
                return Err(format!(
                    "params._meta.{CLIENT_INFO_KEY}.{field} must be a string"
                ));
            }
        }
        if let Some(icons) = client_info.get("icons") {
            if !valid_icons(icons) {
                return Err(format!(
                    "params._meta.{CLIENT_INFO_KEY}.icons entries are invalid"
                ));
            }
        }
    }
    if meta
        .get("progressToken")
        .is_some_and(|token| !valid_id(token))
    {
        return Err("params._meta.progressToken must be a string or number".to_string());
    }
    if let Some(level) = meta.get("io.modelcontextprotocol/logLevel") {
        let valid = level.as_str().is_some_and(|level| {
            [
                "alert",
                "critical",
                "debug",
                "emergency",
                "error",
                "info",
                "notice",
                "warning",
            ]
            .contains(&level)
        });
        if !valid {
            return Err("params._meta.io.modelcontextprotocol/logLevel is invalid".to_string());
        }
    }
    Ok(())
}

fn validate_method_params(method: &str, params: &Map<String, Value>) -> Result<(), String> {
    match method {
        "tools/list" => {
            if params
                .get("cursor")
                .is_some_and(|cursor| !cursor.is_string())
            {
                return Err("params.cursor must be a string".to_string());
            }
        }
        "tools/call" => {
            if !params.get("name").is_some_and(Value::is_string) {
                return Err("params.name must be a string".to_string());
            }
            if params
                .get("arguments")
                .is_some_and(|arguments| !arguments.is_object())
            {
                return Err("params.arguments must be an object".to_string());
            }
            if params
                .get("requestState")
                .is_some_and(|state| !state.is_string())
            {
                return Err("params.requestState must be a string".to_string());
            }
            if let Some(responses) = params.get("inputResponses") {
                let Some(responses) = responses.as_object() else {
                    return Err("params.inputResponses must be an object".to_string());
                };
                if responses
                    .values()
                    .any(|response| !valid_input_response(response))
                {
                    return Err(
                        "params.inputResponses values must be valid MCP input results".to_string(),
                    );
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn valid_input_response(response: &Value) -> bool {
    let Some(response) = response.as_object() else {
        return false;
    };
    let create_message = response.get("content").is_some_and(valid_sampling_content)
        && response.get("model").is_some_and(Value::is_string)
        && matches!(
            response.get("role").and_then(Value::as_str),
            Some("assistant" | "user")
        )
        && response
            .get("stopReason")
            .is_none_or(|value| value.is_string())
        && response.get("_meta").is_none_or(Value::is_object);
    let roots = response.get("roots").is_some_and(|roots| {
        roots.as_array().is_some_and(|roots| {
            roots.iter().all(|root| {
                root.as_object().is_some_and(|root| {
                    root.get("uri").is_some_and(valid_root_uri)
                        && root.get("name").is_none_or(Value::is_string)
                        && root.get("_meta").is_none_or(Value::is_object)
                })
            })
        })
    });
    let elicitation = matches!(
        response.get("action").and_then(Value::as_str),
        Some("accept" | "cancel" | "decline")
    ) && response.get("content").is_none_or(|content| {
        content.as_object().is_some_and(|content| {
            content.values().all(|value| {
                value.is_string()
                    || value.is_number()
                    || value.is_boolean()
                    || value
                        .as_array()
                        .is_some_and(|items| items.iter().all(Value::is_string))
            })
        })
    });
    create_message || roots || elicitation
}

fn valid_sampling_content(content: &Value) -> bool {
    if let Some(items) = content.as_array() {
        return items.iter().all(valid_sampling_content_block);
    }
    valid_sampling_content_block(content)
}

fn valid_sampling_content_block(content: &Value) -> bool {
    let Some(content) = content.as_object() else {
        return false;
    };
    if content.get("_meta").is_some_and(|value| !value.is_object()) {
        return false;
    }
    match content.get("type").and_then(Value::as_str) {
        Some("text") => {
            content.get("text").is_some_and(Value::is_string)
                && content.get("annotations").is_none_or(valid_annotations)
        }
        Some("image" | "audio") => {
            content.get("data").is_some_and(Value::is_string)
                && content.get("mimeType").is_some_and(Value::is_string)
                && content.get("annotations").is_none_or(valid_annotations)
        }
        Some("tool_use") => {
            content.get("id").is_some_and(Value::is_string)
                && content.get("name").is_some_and(Value::is_string)
                && content.get("input").is_some_and(Value::is_object)
        }
        Some("tool_result") => {
            content.get("toolUseId").is_some_and(Value::is_string)
                && content.get("isError").is_none_or(Value::is_boolean)
                && content.get("content").is_some_and(|items| {
                    items
                        .as_array()
                        .is_some_and(|items| items.iter().all(valid_tool_result_content))
                })
        }
        _ => false,
    }
}

fn valid_tool_result_content(content: &Value) -> bool {
    if valid_basic_content(content) {
        return true;
    }
    let Some(content) = content.as_object() else {
        return false;
    };
    if content.get("_meta").is_some_and(|value| !value.is_object())
        || content
            .get("annotations")
            .is_some_and(|value| !valid_annotations(value))
    {
        return false;
    }
    match content.get("type").and_then(Value::as_str) {
        Some("resource_link") => {
            content.get("name").is_some_and(Value::is_string)
                && content.get("uri").is_some_and(valid_uri)
                && content.get("title").is_none_or(Value::is_string)
                && content.get("description").is_none_or(Value::is_string)
                && content.get("mimeType").is_none_or(Value::is_string)
                && content.get("icons").is_none_or(valid_icons)
                && content.get("size").is_none_or(Value::is_number)
        }
        Some("resource") => content.get("resource").is_some_and(valid_resource_contents),
        _ => false,
    }
}

fn valid_basic_content(content: &Value) -> bool {
    let Some(content) = content.as_object() else {
        return false;
    };
    if content.get("_meta").is_some_and(|value| !value.is_object())
        || content
            .get("annotations")
            .is_some_and(|value| !valid_annotations(value))
    {
        return false;
    }
    match content.get("type").and_then(Value::as_str) {
        Some("text") => content.get("text").is_some_and(Value::is_string),
        Some("image" | "audio") => {
            content.get("data").is_some_and(Value::is_string)
                && content.get("mimeType").is_some_and(Value::is_string)
        }
        _ => false,
    }
}

fn valid_resource_contents(resource: &Value) -> bool {
    resource.as_object().is_some_and(|resource| {
        resource.get("uri").is_some_and(valid_uri)
            && resource.get("mimeType").is_none_or(Value::is_string)
            && resource.get("_meta").is_none_or(Value::is_object)
            && (resource.get("text").is_some_and(Value::is_string)
                || resource.get("blob").is_some_and(Value::is_string))
    })
}

fn valid_icons(icons: &Value) -> bool {
    icons.as_array().is_some_and(|icons| {
        icons.iter().all(|icon| {
            icon.as_object().is_some_and(|icon| {
                icon.get("src").is_some_and(valid_uri)
                    && icon.get("mimeType").is_none_or(Value::is_string)
                    && icon.get("sizes").is_none_or(|sizes| {
                        sizes
                            .as_array()
                            .is_some_and(|sizes| sizes.iter().all(Value::is_string))
                    })
                    && matches!(
                        icon.get("theme").map(Value::as_str),
                        None | Some(Some("dark" | "light"))
                    )
            })
        })
    })
}

fn valid_uri(uri: &Value) -> bool {
    uri.as_str().is_some_and(valid_uri_str)
}

fn valid_uri_str(uri: &str) -> bool {
    let Some((scheme, remainder)) = uri.split_once(':') else {
        return false;
    };
    let _ = remainder;
    matches!(scheme.as_bytes().first(), Some(b'a'..=b'z' | b'A'..=b'Z'))
        && scheme
            .bytes()
            .skip(1)
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
        && !uri.bytes().any(|byte| byte.is_ascii_whitespace())
}

fn valid_root_uri(uri: &Value) -> bool {
    uri.as_str()
        .is_some_and(|uri| uri.starts_with("file://") && valid_uri_str(uri))
}

fn valid_annotations(annotations: &Value) -> bool {
    annotations.as_object().is_some_and(|annotations| {
        annotations.get("audience").is_none_or(|audience| {
            audience.as_array().is_some_and(|audience| {
                audience
                    .iter()
                    .all(|role| matches!(role.as_str(), Some("assistant" | "user")))
            })
        }) && annotations.get("lastModified").is_none_or(Value::is_string)
            && annotations.get("priority").is_none_or(|priority| {
                priority
                    .as_f64()
                    .is_some_and(|priority| (0.0..=1.0).contains(&priority))
            })
    })
}

fn discover_result() -> Value {
    json!({
        "resultType": "complete",
        "supportedVersions": [PROTOCOL_VERSION],
        "capabilities": {"tools": {}},
        "instructions": "Compile or check one Rue root module and query canonical Rue error/specification metadata. Imports must be reached from the root; use sourceManifest to bound accepted reads.",
        "ttlMs": CACHE_TTL_MS,
        "cacheScope": "public",
    })
}

fn tools_result() -> Value {
    json!({"ttlMs": CACHE_TTL_MS, "cacheScope": "public", "tools": [
        tool("compile", "Compile a single Rue root module with the canonical filesystem driver.", compile_schema(true)),
        tool("check", "Check a single Rue root module without retaining an executable.", compile_schema(false)),
        tool("explain-error", "Return the compiler-owned explanation for an E-code.", json!({
            "type": "object", "properties": {"code": {"type": "string", "pattern": "^E[0-9]{4}$"}}, "required": ["code"], "additionalProperties": false
        })),
        tool("error-metadata", "Return the compiler-owned public error-code inventory.", json!({"type": "object", "properties": {}, "additionalProperties": false})),
        tool("spec", "Query schema-v1 compiler/specification metadata produced by rue-spec.", json!({
            "type": "object",
            "properties": {
                "specId": {"type": "string", "minLength": 1, "description": "Optional exact specification rule ID."},
                "errorCode": {"type": "string", "pattern": "^E[0-9]{4}$", "description": "Optional exact compiler error code."}
            },
            "additionalProperties": false
        })),
    ]})
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
        "annotations": {"readOnlyHint": name != "compile", "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}
    })
}

fn compile_schema(with_output: bool) -> Value {
    let mut properties = Map::from_iter([
        (
            "root".to_string(),
            json!({"type": "string", "minLength": 1, "description": "Path to the single root .rue source."}),
        ),
        (
            "sourceManifest".to_string(),
            json!({"type": "string", "minLength": 1, "description": "Optional line-oriented manifest bounding source reads."}),
        ),
        (
            "target".to_string(),
            json!({"type": "string", "minLength": 1}),
        ),
        (
            "optimization".to_string(),
            json!({"type": "integer", "minimum": 0, "maximum": 3}),
        ),
        (
            "preview".to_string(),
            json!({"type": "array", "items": {"type": "string", "minLength": 1}, "uniqueItems": true}),
        ),
    ]);
    if with_output {
        properties.insert(
            "output".to_string(),
            json!({"type": "string", "minLength": 1, "description": "New executable destination path; existing paths are never replaced."}),
        );
    }
    let mut required = vec![Value::String("root".to_string())];
    if with_output {
        required.push(Value::String("output".to_string()));
    }
    json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
}

fn call_tool(
    request: &Value,
    id: Value,
    control: &Arc<RequestControl>,
    shutting_down: &AtomicBool,
) -> Value {
    let Some(params) = request.get("params").and_then(Value::as_object) else {
        return rpc_error(id, INVALID_PARAMS, "params must be an object");
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return rpc_error(id, INVALID_PARAMS, "params.name must be a string");
    };
    let arguments = match params.get("arguments") {
        None => Map::new(),
        Some(Value::Object(arguments)) => arguments.clone(),
        Some(_) => return rpc_error(id, INVALID_PARAMS, "params.arguments must be an object"),
    };
    match name {
        "compile" => compile_tool(&id, arguments, true, control, shutting_down),
        "check" => compile_tool(&id, arguments, false, control, shutting_down),
        "explain-error" => explain_tool(&id, arguments),
        "error-metadata" => error_metadata_tool(&id, arguments),
        "spec" => spec_index_tool(&id, arguments, control, shutting_down),
        _ => rpc_error(id, INVALID_PARAMS, format!("unknown tool: {name}")),
    }
}

fn compile_tool(
    id: &Value,
    arguments: Map<String, Value>,
    compile: bool,
    control: &Arc<RequestControl>,
    shutting_down: &AtomicBool,
) -> Value {
    let allowed = if compile {
        &[
            "root",
            "output",
            "sourceManifest",
            "target",
            "optimization",
            "preview",
        ][..]
    } else {
        &[
            "root",
            "sourceManifest",
            "target",
            "optimization",
            "preview",
        ][..]
    };
    if let Err(error) = reject_unknown(&arguments, allowed) {
        return rpc_error(id.clone(), INVALID_PARAMS, error);
    }
    let root = match required_string(&arguments, "root") {
        Ok(value) => value,
        Err(error) => return rpc_error(id.clone(), INVALID_PARAMS, error),
    };
    if option_like_path(root) {
        return rpc_error(
            id.clone(),
            INVALID_PARAMS,
            "root must not be an option-like relative path",
        );
    }
    let requested_output = if compile {
        let output = match required_string(&arguments, "output") {
            Ok(value) => value,
            Err(error) => return rpc_error(id.clone(), INVALID_PARAMS, error),
        };
        if option_like_path(output) {
            return rpc_error(
                id.clone(),
                INVALID_PARAMS,
                "output must not be an option-like relative path",
            );
        }
        Some(PathBuf::from(output))
    } else {
        None
    };
    let temporary = match tempfile::Builder::new().prefix("rue-mcp-build-").tempdir() {
        Ok(directory) => directory,
        Err(error) => return tool_error(format!("failed to create build directory: {error}")),
    };
    let producer_output = temporary.path().join("program");
    let binary = match producer_env("RUE_BINARY") {
        Ok(binary) => binary,
        Err(error) => return tool_error(error),
    };
    let mut command = Command::new(binary);
    command
        .arg("--error-format")
        .arg("json")
        .arg("--linker")
        .arg("internal")
        .arg("-o")
        .arg(&producer_output);
    let manifest = match optional_string(&arguments, "sourceManifest") {
        Ok(value) => value,
        Err(error) => return rpc_error(id.clone(), INVALID_PARAMS, error),
    };
    if let Some(manifest) = manifest {
        if option_like_path(manifest) {
            return rpc_error(
                id.clone(),
                INVALID_PARAMS,
                "sourceManifest must not be an option-like relative path",
            );
        }
        command.arg("--source-manifest").arg(manifest);
    }
    let target = match optional_string(&arguments, "target") {
        Ok(value) => value,
        Err(error) => return rpc_error(id.clone(), INVALID_PARAMS, error),
    };
    if let Some(target) = target {
        if target.starts_with('-') {
            return rpc_error(id.clone(), INVALID_PARAMS, "target must not be option-like");
        }
        command.arg("--target").arg(target);
    }
    if let Some(level) = arguments.get("optimization").and_then(Value::as_u64) {
        if level > 3 {
            return rpc_error(
                id.clone(),
                INVALID_PARAMS,
                "optimization must be an integer from 0 through 3",
            );
        }
        command.arg(format!("-O{level}"));
    } else if arguments.contains_key("optimization") {
        return rpc_error(
            id.clone(),
            INVALID_PARAMS,
            "optimization must be an integer from 0 through 3",
        );
    }
    if let Some(previews) = arguments.get("preview") {
        let Some(previews) = previews.as_array() else {
            return rpc_error(
                id.clone(),
                INVALID_PARAMS,
                "preview must be an array of unique non-empty strings",
            );
        };
        let mut unique = HashSet::new();
        for preview in previews {
            let Some(preview) = preview
                .as_str()
                .filter(|preview| !preview.is_empty() && !preview.starts_with('-'))
            else {
                return rpc_error(
                    id.clone(),
                    INVALID_PARAMS,
                    "preview must be an array of unique non-empty strings",
                );
            };
            if !unique.insert(preview) {
                return rpc_error(
                    id.clone(),
                    INVALID_PARAMS,
                    "preview must be an array of unique non-empty strings",
                );
            }
            command.arg("--preview").arg(preview);
        }
    }
    command.arg(root);
    let output = match run_command(command, control, shutting_down) {
        Ok(output) => output,
        Err(CommandError::Cancelled) => return Value::Null,
        Err(CommandError::Message(error)) => return tool_error(error),
        Err(CommandError::OutputLimit) => {
            return tool_error(format!(
                "canonical producer exceeded the {MAX_PRODUCER_STREAM_BYTES}-byte stream limit"
            ));
        }
    };
    let producer_succeeded = output.status.success();
    if producer_succeeded && !producer_output.is_file() {
        return tool_error("compiler reported success without creating its output artifact");
    }
    let result = match compiler_result(
        output,
        requested_output
            .as_deref()
            .map(|path| path.to_string_lossy().into_owned()),
    ) {
        Ok(result) => result,
        Err(CommandError::Message(error)) => return tool_error(error),
        Err(CommandError::Cancelled | CommandError::OutputLimit) => {
            return tool_error("compiler result became unavailable");
        }
    };
    if producer_succeeded {
        if let Some(requested_output) = requested_output.as_deref() {
            let staged = match stage_output(&producer_output, requested_output) {
                Ok(staged) => staged,
                Err(error) => return tool_error(error),
            };
            match commit_publication(control, || persist_output(staged, requested_output)) {
                Ok(()) => {}
                Err(CommandError::Cancelled) => return Value::Null,
                Err(CommandError::Message(error)) => return tool_error(error),
                Err(CommandError::OutputLimit) => {
                    return tool_error("publication failed with an invalid internal state");
                }
            }
        }
    }
    tool_success(result)
}

fn compiler_result(
    output: std::process::Output,
    output_path: Option<String>,
) -> Result<Value, CommandError> {
    let stderr = String::from_utf8(output.stderr)
        .map_err(|_| CommandError::Message("compiler stderr was not UTF-8".to_string()))?;
    let mut diagnostics = Vec::new();
    for line in stderr.lines().filter(|line| !line.is_empty()) {
        let batch: Value = serde_json::from_str(line).map_err(|_| {
            CommandError::Message("compiler violated its --error-format json contract".to_string())
        })?;
        let Some(batch) = batch.as_array() else {
            return Err(CommandError::Message(
                "compiler diagnostic batch was not an array".to_string(),
            ));
        };
        diagnostics.extend(batch.iter().cloned());
    }
    Ok(json!({
        "success": output.status.success(),
        "exitCode": output.status.code(),
        "output": output_path,
        "diagnosticSchema": "docs/process/diagnostics.md",
        "diagnostics": diagnostics,
    }))
}

fn explain_tool(id: &Value, arguments: Map<String, Value>) -> Value {
    if let Err(error) = reject_unknown(&arguments, &["code"]) {
        return rpc_error(id.clone(), INVALID_PARAMS, error);
    }
    let code = match required_string(&arguments, "code")
        .and_then(|code| code.parse::<ErrorCode>().map_err(|error| error.to_string()))
    {
        Ok(code) => code,
        Err(error) => return rpc_error(id.clone(), INVALID_PARAMS, error),
    };
    let Some(explanation) = error_code_explanation(code) else {
        return tool_error(format!(
            "no compiler-owned explanation is available for {code}"
        ));
    };
    tool_success(json!({
        "code": explanation.metadata.code.to_string(),
        "name": explanation.metadata.name,
        "title": explanation.metadata.title,
        "sourcePath": explanation.metadata.source_path,
        "explanation": explanation.explanation,
        "likelyCause": explanation.likely_cause,
        "examples": explanation.examples.iter().map(|example| json!({"title": example.title, "source": example.source})).collect::<Vec<_>>(),
        "references": explanation.references.iter().map(|reference| json!({"title": reference.title, "path": reference.path, "rule": reference.rule})).collect::<Vec<_>>(),
    }))
}

fn error_metadata_tool(id: &Value, arguments: Map<String, Value>) -> Value {
    if let Err(error) = reject_unknown(&arguments, &[]) {
        return rpc_error(id.clone(), INVALID_PARAMS, error);
    }
    tool_success(json!({
        "errors": error_code_metadata().iter().map(|entry| json!({
            "code": entry.code.to_string(), "name": entry.name, "title": entry.title, "sourcePath": entry.source_path
        })).collect::<Vec<_>>()
    }))
}

fn spec_index_tool(
    id: &Value,
    arguments: Map<String, Value>,
    control: &Arc<RequestControl>,
    shutting_down: &AtomicBool,
) -> Value {
    if let Err(error) = reject_unknown(&arguments, &["specId", "errorCode"]) {
        return rpc_error(id.clone(), INVALID_PARAMS, error);
    }
    let spec_id = match optional_string(&arguments, "specId") {
        Ok(value) => value,
        Err(error) => return rpc_error(id.clone(), INVALID_PARAMS, error),
    };
    let error_code = match optional_string(&arguments, "errorCode") {
        Ok(value) => value,
        Err(error) => return rpc_error(id.clone(), INVALID_PARAMS, error),
    };
    if let Some(code) = error_code {
        if !valid_error_code_spelling(code) {
            return rpc_error(
                id.clone(),
                INVALID_PARAMS,
                "errorCode must match E followed by four decimal digits",
            );
        }
    }
    let binary = match producer_env("RUE_SPEC_BINARY") {
        Ok(binary) => binary,
        Err(error) => return tool_error(error),
    };
    let mut command = Command::new(binary);
    command.arg("--machine-index");
    let output = match run_command(command, control, shutting_down) {
        Ok(output) => output,
        Err(CommandError::Cancelled) => return Value::Null,
        Err(CommandError::Message(error)) => return tool_error(error),
        Err(CommandError::OutputLimit) => {
            return tool_error(format!(
                "canonical producer exceeded the {MAX_PRODUCER_STREAM_BYTES}-byte stream limit"
            ));
        }
    };
    if !output.status.success() {
        return tool_error("canonical machine-index producer failed");
    }
    let index: Value = match serde_json::from_slice(&output.stdout) {
        Ok(index) => index,
        Err(_) => return tool_error("canonical machine-index producer returned invalid JSON"),
    };
    let Some(index_object) = index.as_object() else {
        return tool_error("canonical machine-index producer returned a non-object");
    };
    if index_object.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return tool_error("unsupported canonical machine-index schema version");
    }
    let errors = filtered_array(index_object, "errors", |entry| {
        error_code.is_none_or(|code| entry.get("code").and_then(Value::as_str) == Some(code))
    });
    let rules = filtered_array(index_object, "spec_rules", |entry| {
        spec_id.is_none_or(|id| entry.get("id").and_then(Value::as_str) == Some(id))
    });
    let relationships = filtered_array(index_object, "error_spec_relationships", |entry| {
        error_code.is_none_or(|code| entry.get("error_code").and_then(Value::as_str) == Some(code))
            && spec_id.is_none_or(|id| entry.get("spec_id").and_then(Value::as_str) == Some(id))
    });
    tool_success(
        json!({"schema_version": 1, "errors": errors, "spec_rules": rules, "error_spec_relationships": relationships}),
    )
}

fn filtered_array<F: Fn(&Value) -> bool>(
    object: &Map<String, Value>,
    key: &str,
    predicate: F,
) -> Vec<Value> {
    object
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| predicate(entry))
        .cloned()
        .collect()
}

#[derive(Debug)]
enum CommandError {
    Cancelled,
    Message(String),
    OutputLimit,
}

fn run_command(
    mut command: Command,
    control: &Arc<RequestControl>,
    shutting_down: &AtomicBool,
) -> Result<std::process::Output, CommandError> {
    if shutting_down.load(Ordering::Relaxed) {
        cancel_control(control);
    }
    if is_cancelled(control) {
        return Err(CommandError::Cancelled);
    }
    if ACTIVE_CAPTURE_READERS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
            (active + 2 <= MAX_CAPTURE_READERS).then_some(active + 2)
        })
        .is_err()
    {
        return Err(CommandError::Message(
            "producer capture-reader limit is exhausted".to_string(),
        ));
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    configure_process_group(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            ACTIVE_CAPTURE_READERS.fetch_sub(2, Ordering::AcqRel);
            return Err(CommandError::Message(format!(
                "failed to start canonical producer: {error}"
            )));
        }
    };
    let stdout = child.stdout.take().expect("piped producer stdout missing");
    let stderr = child.stderr.take().expect("piped producer stderr missing");
    let (limit_sender, limit_receiver) = mpsc::channel();
    let stdout_receiver = capture_stream(stdout, limit_sender.clone());
    let stderr_receiver = capture_stream(stderr, limit_sender);
    {
        let mut state = control.state.lock().expect("request state poisoned");
        state.child = Some(child);
        if shutting_down.load(Ordering::Relaxed) {
            state.phase = RequestPhase::Cancelled;
        }
        if state.phase == RequestPhase::Cancelled {
            terminate_running_child(&mut state);
        }
    }
    let mut output_limit = false;
    let status_result = loop {
        if shutting_down.load(Ordering::Relaxed) {
            cancel_control(control);
        }
        if limit_receiver.try_recv().is_ok() {
            output_limit = true;
            terminate_control(control);
        }
        let status =
            match poll_and_reap_child(&mut control.state.lock().expect("request state poisoned")) {
                Ok(status) => status,
                Err(error) => {
                    break Err(CommandError::Message(format!(
                        "failed to wait for canonical producer: {error}"
                    )));
                }
            };
        if let Some(status) = status {
            break Ok(status);
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = receive_capture(stdout_receiver)?;
    let stderr = receive_capture(stderr_receiver)?;
    output_limit |= stdout.exceeded || stderr.exceeded;
    if is_cancelled(control) {
        Err(CommandError::Cancelled)
    } else if output_limit {
        Err(CommandError::OutputLimit)
    } else {
        let status = status_result?;
        Ok(std::process::Output {
            status,
            stdout: stdout.bytes,
            stderr: stderr.bytes,
        })
    }
}

struct Capture {
    bytes: Vec<u8>,
    exceeded: bool,
}

fn capture_stream(
    mut stream: impl Read + Send + 'static,
    limit_sender: mpsc::Sender<()>,
) -> mpsc::Receiver<io::Result<Capture>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        struct ReaderPermit;
        impl Drop for ReaderPermit {
            fn drop(&mut self) {
                ACTIVE_CAPTURE_READERS.fetch_sub(1, Ordering::AcqRel);
            }
        }
        let _permit = ReaderPermit;
        // Allocate the complete retention budget once so geometric Vec growth
        // cannot overshoot it. The only additional reader storage is `chunk`.
        let mut bytes = Vec::with_capacity(MAX_PRODUCER_STREAM_BYTES);
        let mut chunk = [0_u8; 8192];
        let mut exceeded = false;
        let result = loop {
            match stream.read(&mut chunk) {
                Ok(0) => break Ok(Capture { bytes, exceeded }),
                Ok(count) => {
                    let remaining = MAX_PRODUCER_STREAM_BYTES.saturating_sub(bytes.len());
                    bytes.extend_from_slice(&chunk[..count.min(remaining)]);
                    if count > remaining && !exceeded {
                        exceeded = true;
                        let _ = limit_sender.send(());
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => break Err(error),
            }
        };
        let _ = sender.send(result);
    });
    receiver
}

fn receive_capture(receiver: mpsc::Receiver<io::Result<Capture>>) -> Result<Capture, CommandError> {
    receiver
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| {
            CommandError::Message(
                "producer output remained open after direct-child cleanup".to_string(),
            )
        })?
        .map_err(|error| CommandError::Message(format!("failed to read producer output: {error}")))
}

fn cancel_request(request: &Value, requests: &Requests) -> Result<(), &'static str> {
    let Some(object) = request.as_object() else {
        return Err("cancellation notification must be an object");
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || object.get("id").is_some()
        || object.get("method").and_then(Value::as_str) != Some("notifications/cancelled")
    {
        return Err("malformed cancellation notification");
    }
    let Some(params) = object.get("params").and_then(Value::as_object) else {
        return Err("cancellation params must be an object");
    };
    let Some(request_id) = params.get("requestId").filter(|id| valid_id(id)) else {
        return Err("cancellation requestId must be a string or number");
    };
    if params
        .get("reason")
        .is_some_and(|reason| !reason.is_string())
        || params.get("_meta").is_some_and(|meta| !meta.is_object())
    {
        return Err("cancellation reason and _meta have invalid types");
    }
    if let Some(control) = requests
        .lock()
        .expect("request registry poisoned")
        .get(&id_key(request_id))
        .cloned()
    {
        cancel_control(&control);
    }
    Ok(())
}

fn cancel_control(control: &RequestControl) {
    let mut state = control.state.lock().expect("request state poisoned");
    if state.phase == RequestPhase::Open {
        state.phase = RequestPhase::Cancelled;
        terminate_running_child(&mut state);
    }
}

fn terminate_control(control: &RequestControl) {
    terminate_running_child(&mut control.state.lock().expect("request state poisoned"));
}

fn terminate_running_child(state: &mut RequestState) {
    if let Some(child) = state.child.as_mut() {
        terminate_child(child);
    }
}

fn is_cancelled(control: &RequestControl) -> bool {
    control.state.lock().expect("request state poisoned").phase == RequestPhase::Cancelled
}

fn claim_response_for_dispatch(control: &RequestControl) -> bool {
    let mut state = control.state.lock().expect("request state poisoned");
    match state.phase {
        RequestPhase::Open => {
            state.phase = RequestPhase::ResponseClaimed;
            true
        }
        RequestPhase::ResponseClaimed => true,
        RequestPhase::Cancelled => false,
    }
}

fn commit_publication<T>(
    control: &RequestControl,
    publish: impl FnOnce() -> Result<T, String>,
) -> Result<T, CommandError> {
    let mut state = control.state.lock().expect("request state poisoned");
    if state.phase != RequestPhase::Open {
        return Err(CommandError::Cancelled);
    }
    // Staging has already completed. Keep the cancellation/side-effect
    // boundary to the single atomic no-clobber persistence operation.
    let result = publish().map_err(CommandError::Message);
    state.phase = RequestPhase::ResponseClaimed;
    result
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn poll_and_reap_child(state: &mut RequestState) -> io::Result<Option<ExitStatus>> {
    let Some(child) = state.child.as_mut() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "producer child is not running",
        ));
    };
    let pid = child.id().min(i32::MAX as u32) as libc::pid_t;
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    loop {
        // SAFETY: `info` is writable siginfo_t storage and `pid` names this
        // still-unreaped child. WNOWAIT retains the zombie, reserving its PID
        // while the process group it led is terminated.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            terminate_child(child);
            let _ = child.wait();
            state.child = None;
            return Err(error);
        }
    }
    // SAFETY: waitid initialized the zeroed structure on success. A zero pid
    // with WNOHANG means the child has not exited.
    let info = unsafe { info.assume_init() };
    if unsafe { info.si_pid() } == 0 {
        return Ok(None);
    }
    terminate_child(child);
    let status = child.wait();
    state.child = None;
    status.map(Some)
}

#[cfg(not(unix))]
fn poll_and_reap_child(state: &mut RequestState) -> io::Result<Option<ExitStatus>> {
    let Some(child) = state.child.as_mut() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "producer child is not running",
        ));
    };
    match child.try_wait() {
        Ok(Some(status)) => {
            // try_wait reaped the direct child. Clear it while holding the
            // cancellation state lock, before its PID can be reused.
            state.child = None;
            Ok(Some(status))
        }
        Ok(None) => Ok(None),
        Err(error) => {
            terminate_child(child);
            let _ = child.wait();
            state.child = None;
            Err(error)
        }
    }
}

#[cfg(unix)]
fn terminate_child(child: &mut Child) {
    let process_group = child.id().min(i32::MAX as u32) as i32;
    // SAFETY: a negative, positive child pid addresses only the process group
    // created for this producer. SIGKILL requires no shared memory contract.
    unsafe {
        libc::kill(-process_group, libc::SIGKILL);
    }
    let _ = child.kill();
}

#[cfg(not(unix))]
fn terminate_child(child: &mut Child) {
    // Rust's portable API terminates the direct producer. Rue MCP does not
    // claim descendant-tree cleanup on non-Unix hosts.
    let _ = child.kill();
}

fn producer_env(name: &str) -> Result<String, String> {
    env::var(name)
        .map_err(|_| format!("{name} is not set; run //crates/rue-mcp:server through Buck"))
}

fn valid_id(id: &Value) -> bool {
    id.is_string() || id.is_number()
}

fn option_like_path(value: &str) -> bool {
    value.starts_with('-') && Path::new(value).is_relative()
}

fn valid_error_code_spelling(code: &str) -> bool {
    let bytes = code.as_bytes();
    bytes.len() == 5 && bytes[0] == b'E' && bytes[1..].iter().all(u8::is_ascii_digit)
}

fn stage_output(source: &Path, destination: &Path) -> Result<tempfile::NamedTempFile, String> {
    stage_output_with(source, destination, || {})
}

fn stage_output_with(
    source: &Path,
    destination: &Path,
    staging_started: impl FnOnce(),
) -> Result<tempfile::NamedTempFile, String> {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut source_file =
        File::open(source).map_err(|error| format!("failed to open compiler output: {error}"))?;
    let mut staged = tempfile::Builder::new()
        .prefix(".rue-mcp-publish-")
        .tempfile_in(parent)
        .map_err(|error| format!("failed to stage output in its destination directory: {error}"))?;
    staging_started();
    io::copy(&mut source_file, staged.as_file_mut())
        .map_err(|error| format!("failed to stage compiler output: {error}"))?;
    let permissions = fs::metadata(source)
        .map_err(|error| format!("failed to inspect compiler output: {error}"))?
        .permissions();
    fs::set_permissions(staged.path(), permissions)
        .map_err(|error| format!("failed to preserve compiler output permissions: {error}"))?;
    staged
        .as_file_mut()
        .sync_all()
        .map_err(|error| format!("failed to flush staged compiler output: {error}"))?;
    Ok(staged)
}

fn persist_output(staged: tempfile::NamedTempFile, destination: &Path) -> Result<(), String> {
    staged.persist_noclobber(destination).map_err(|error| {
        format!(
            "refusing to replace or reuse output destination {}: {}",
            destination.display(),
            error.error
        )
    })?;
    Ok(())
}

fn reject_unknown(arguments: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    if let Some(key) = arguments
        .keys()
        .find(|key| !allowed.contains(&key.as_str()))
    {
        Err(format!("unknown argument: {key}"))
    } else {
        Ok(())
    }
}

fn required_string<'a>(arguments: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{key} must be a non-empty string"))
}

fn optional_string<'a>(
    arguments: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, String> {
    match arguments.get(key) {
        None => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value)),
        Some(_) => Err(format!("{key} must be a non-empty string")),
    }
}

fn tool_success(structured: Value) -> Value {
    tool_result(structured, false)
}

fn tool_error(message: impl Into<String>) -> Value {
    tool_result(json!({"error": message.into()}), true)
}

fn tool_result(structured: Value, is_error: bool) -> Value {
    let text = serde_json::to_string(&structured).expect("JSON value always serializes");
    json!({"content": [{"type": "text", "text": text}], "structuredContent": structured, "isError": is_error})
}

fn rpc_result(id: Value, result: Value) -> Value {
    let mut result = result;
    if let Value::Object(object) = &mut result {
        object.insert(
            "resultType".to_string(),
            Value::String("complete".to_string()),
        );
        object.insert("_meta".to_string(), server_meta());
    }
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    let mut response = Map::from_iter([
        ("jsonrpc".to_string(), Value::String("2.0".to_string())),
        (
            "error".to_string(),
            json!({"code": code, "message": message.into()}),
        ),
    ]);
    // Result._meta is the only 2026-07-28 serverInfo carrier. The final
    // JSONRPCErrorResponse schema has no metadata field.
    if valid_id(&id) {
        response.insert("id".to_string(), id);
    }
    Value::Object(response)
}

fn rpc_error_data(id: Value, code: i64, message: impl Into<String>, data: Value) -> Value {
    let mut response = rpc_error(id, code, message);
    response["error"]["data"] = data;
    response
}

fn server_meta() -> Value {
    json!({SERVER_INFO_KEY: {"name": SERVER_NAME, "version": VERSION}})
}

fn id_key(id: &Value) -> String {
    serde_json::to_string(id).unwrap_or_else(|_| "null".to_string())
}

fn write_response(output: &Output, response: Value) {
    let mut output = output.lock().expect("stdout lock poisoned");
    let _ = serde_json::to_writer(&mut *output, &response);
    let _ = output.write_all(b"\n");
    let _ = output.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, params: Value) -> Value {
        let mut params = params.as_object().cloned().unwrap_or_default();
        params.insert(
            "_meta".to_string(),
            json!({
                PROTOCOL_VERSION_KEY: PROTOCOL_VERSION,
                CLIENT_CAPABILITIES_KEY: {},
            }),
        );
        json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params})
    }

    fn control() -> RequestControl {
        RequestControl {
            state: Mutex::new(RequestState {
                child: None,
                phase: RequestPhase::Open,
            }),
        }
    }

    #[test]
    fn current_protocol_discovery_and_tools_are_deterministic() {
        let discover = request("server/discover", json!({}));
        assert_eq!(validate_request(&discover).unwrap(), "server/discover");
        assert_eq!(
            discover_result()["supportedVersions"],
            json!([PROTOCOL_VERSION])
        );
        let tools = tools_result()["tools"].as_array().unwrap().clone();
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "compile",
                "check",
                "explain-error",
                "error-metadata",
                "spec"
            ]
        );
    }

    #[test]
    fn malformed_and_old_protocol_requests_are_rejected_without_panicking() {
        let response = validate_request(&json!([])).unwrap_err();
        assert_eq!(response["error"]["code"], INVALID_REQUEST);
        assert!(response.get("_meta").is_none());
        assert!(response.get("id").is_none());
        let mut old = request("tools/list", json!({}));
        old["params"]["_meta"][PROTOCOL_VERSION_KEY] = json!("2025-11-25");
        let response = validate_request(&old).unwrap_err();
        assert_eq!(response["error"]["code"], UNSUPPORTED_PROTOCOL_VERSION);
    }

    #[test]
    fn cancellation_and_response_claim_have_one_winner() {
        let cancelled = control();
        cancel_control(&cancelled);
        assert!(!claim_response_for_dispatch(&cancelled));

        let responding = control();
        assert!(claim_response_for_dispatch(&responding));
        cancel_control(&responding);
        assert_eq!(
            responding.state.lock().unwrap().phase,
            RequestPhase::ResponseClaimed
        );
        assert!(claim_response_for_dispatch(&responding));
    }

    #[test]
    fn cancellation_during_staging_prevents_publication() {
        use std::sync::Barrier;

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("producer-output");
        let destination = directory.path().join("published-output");
        fs::write(&source, b"program").unwrap();
        let request_control = Arc::new(control());
        let staging_started = Arc::new(Barrier::new(2));
        let staging_released = Arc::new(Barrier::new(2));
        let worker = {
            let control = Arc::clone(&request_control);
            let staging_started = Arc::clone(&staging_started);
            let staging_released = Arc::clone(&staging_released);
            let source = source.clone();
            let destination = destination.clone();
            thread::spawn(move || {
                let staged = stage_output_with(&source, &destination, || {
                    staging_started.wait();
                    staging_released.wait();
                })
                .unwrap();
                commit_publication(&control, || persist_output(staged, &destination))
            })
        };
        staging_started.wait();
        cancel_control(&request_control);
        staging_released.wait();
        assert!(matches!(
            worker.join().unwrap(),
            Err(CommandError::Cancelled)
        ));
        assert!(!destination.exists());
    }

    #[test]
    fn final_persist_and_cancellation_have_one_ordered_winner() {
        use std::sync::Barrier;

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("producer-output");
        let destination = directory.path().join("published-output");
        fs::write(&source, b"program").unwrap();
        let staged = stage_output(&source, &destination).unwrap();
        let request_control = Arc::new(control());
        let commit_locked = Arc::new(Barrier::new(2));
        let commit_released = Arc::new(Barrier::new(2));
        let worker = {
            let control = Arc::clone(&request_control);
            let destination = destination.clone();
            let commit_locked = Arc::clone(&commit_locked);
            let commit_released = Arc::clone(&commit_released);
            thread::spawn(move || {
                commit_publication(&control, || {
                    commit_locked.wait();
                    commit_released.wait();
                    persist_output(staged, &destination)
                })
            })
        };
        commit_locked.wait();
        let cancellation_started = Arc::new(Barrier::new(2));
        let canceller = {
            let control = Arc::clone(&request_control);
            let cancellation_started = Arc::clone(&cancellation_started);
            thread::spawn(move || {
                cancellation_started.wait();
                cancel_control(&control);
            })
        };
        cancellation_started.wait();
        commit_released.wait();
        assert!(worker.join().unwrap().is_ok());
        canceller.join().unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"program");
        assert!(claim_response_for_dispatch(&request_control));

        let occupied = directory.path().join("occupied");
        fs::write(&occupied, b"existing").unwrap();
        let staged = stage_output(&source, &occupied).unwrap();
        let failed = control();
        assert!(matches!(
            commit_publication(&failed, || persist_output(staged, &occupied)),
            Err(CommandError::Message(_))
        ));
        assert!(claim_response_for_dispatch(&failed));
        assert_eq!(fs::read(&occupied).unwrap(), b"existing");
    }

    #[test]
    fn current_request_metadata_and_method_shapes_are_validated() {
        let mut valid = request("tools/list", json!({"cursor": "next", "extension": true}));
        valid["params"]["_meta"][CLIENT_INFO_KEY] =
            json!({"name": "test", "version": "1", "extension": true});
        valid["params"]["_meta"][CLIENT_CAPABILITIES_KEY] =
            json!({"roots": {}, "custom.example/capability": true});
        valid["params"]["_meta"]["progressToken"] = json!(3.5);
        assert_eq!(validate_request(&valid).unwrap(), "tools/list");

        let mut valid_call = request(
            "tools/call",
            json!({
                "name": "error-metadata",
                "inputResponses": {
                    "roots": {"roots": [{"uri": "file:///workspace", "extension": true}]},
                    "elicitation": {"action": "accept", "content": {"answer": "yes", "score": 1.5}},
                    "sampling": {
                        "content": {
                            "type": "tool_result",
                            "toolUseId": "call-1",
                            "content": [
                                {
                                    "type": "resource_link",
                                    "name": "source",
                                    "title": "Source",
                                    "uri": "file:///workspace/main.rue",
                                    "description": "root module",
                                    "mimeType": "text/plain",
                                    "size": 12.5,
                                    "icons": [{"src": "data:image/png;base64,AA==", "theme": "light"}],
                                    "annotations": {"priority": 0.5},
                                    "_meta": {"example/key": true}
                                },
                                {
                                    "type": "resource",
                                    "resource": {
                                        "uri": "file:///workspace/main.rue",
                                        "mimeType": "text/plain",
                                        "text": "fn main() {}",
                                        "_meta": {"example/key": true}
                                    }
                                }
                            ]
                        },
                        "model": "m",
                        "role": "assistant"
                    }
                }
            }),
        );
        valid_call["params"]["_meta"][CLIENT_INFO_KEY] = json!({
            "name": "test",
            "version": "1",
            "icons": [{"src": "data:image/png;base64,AA==", "sizes": ["16x16"], "theme": "dark", "extension": true}]
        });
        assert_eq!(validate_request(&valid_call).unwrap(), "tools/call");

        valid["params"]["cursor"] = json!(3);
        assert_eq!(
            validate_request(&valid).unwrap_err()["error"]["code"],
            INVALID_PARAMS
        );
        let mut invalid_capability = request("server/discover", json!({}));
        invalid_capability["params"]["_meta"][CLIENT_CAPABILITIES_KEY] =
            json!({"sampling": {"tools": true}});
        assert_eq!(
            validate_request(&invalid_capability).unwrap_err()["error"]["code"],
            INVALID_PARAMS
        );
        let mut invalid_response = request(
            "tools/call",
            json!({"name": "error-metadata", "inputResponses": {"bad": {}}}),
        );
        assert_eq!(
            validate_request(&invalid_response).unwrap_err()["error"]["code"],
            INVALID_PARAMS
        );
        for bad in [
            json!({"roots": [{"uri": "https://example.com"}]}),
            json!({"roots": [{"uri": "file:///workspace", "_meta": 7}]}),
            json!({
                "content": {
                    "type": "tool_result",
                    "toolUseId": "call-1",
                    "content": [{"type": "resource", "resource": {"uri": "file:///x", "text": "x", "_meta": 7}}]
                },
                "model": "m",
                "role": "assistant"
            }),
            json!({
                "content": {
                    "type": "tool_result",
                    "toolUseId": "call-1",
                    "content": [{"type": "resource_link", "name": "x", "uri": "file:///x", "icons": [{"src": 7}]}]
                },
                "model": "m",
                "role": "assistant"
            }),
        ] {
            let invalid = request(
                "tools/call",
                json!({"name": "error-metadata", "inputResponses": {"bad": bad}}),
            );
            assert_eq!(
                validate_request(&invalid).unwrap_err()["error"]["code"],
                INVALID_PARAMS
            );
        }
        invalid_response["params"]
            .as_object_mut()
            .unwrap()
            .remove("inputResponses");
        invalid_response["params"]["_meta"][CLIENT_INFO_KEY] =
            json!({"name": "test", "version": "1", "icons": [{}]});
        assert_eq!(
            validate_request(&invalid_response).unwrap_err()["error"]["code"],
            INVALID_PARAMS
        );
    }

    #[test]
    fn tool_results_have_structured_and_text_compatibility_views() {
        let result = explain_tool(
            &json!(7),
            Map::from_iter([("code".to_string(), json!("E0201"))]),
        );
        assert_eq!(result["isError"], false);
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, result["structuredContent"]);
        assert_eq!(text["code"], "E0201");
    }

    #[test]
    fn compiler_json_batches_are_flattened_without_changing_diagnostics() {
        let expected = json!({"code":"E0201","helps":[],"message":"undefined variable","notes":[],"severity":"error","spans":[],"suggestions":[]});
        let output = std::process::Output {
            status: success_status(),
            stdout: Vec::new(),
            stderr: format!("[{}]\n", serde_json::to_string(&expected).unwrap()).into_bytes(),
        };
        let result = compiler_result(output, None).unwrap();
        assert_eq!(result["diagnostics"], json!([expected]));
    }

    #[cfg(unix)]
    fn success_status() -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(0)
    }

    #[cfg(windows)]
    fn success_status() -> std::process::ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(0)
    }

    #[test]
    fn source_guards_pin_canonical_authorities() {
        let source = include_str!("main.rs");
        assert!(source.contains("error_code_metadata()"));
        assert!(source.contains("error_code_explanation(code)"));
        assert!(source.contains(".arg(\"--error-format\")"));
        assert!(source.contains(".arg(\"--linker\")"));
        assert!(source.contains("command.arg(\"--source-manifest\")"));
        assert!(source.contains("command.arg(\"--machine-index\")"));
        assert!(!source.contains(concat!("FilesystemCompilerHost", "::open")));
    }
}
