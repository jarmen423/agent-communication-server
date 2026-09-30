//! Browser → hub commands. Each WS text frame is a JSON command the bridge
//! publishes onto the NATS bus on the browser's behalf.

use anyhow::{Context, Result};
use tracing::warn;
use uuid::Uuid;

use crate::protocol::{Envelope, MessageKind};
use crate::HubClient;

/// Browser command → NATS publish.
/// Supported:
/// - `{"type":"send_message","to":"agent","message":"...","provider":"grok"?,"model":"..."}` → ensure worker then task
/// - `{"type":"stop_agent","identity":"..."}` → stop supervised worker + status closed
/// - `{"type":"resume_agent","identity":"..."}` → status ready on agents.<id>
/// - `{"type":"ensure_worker","identity":"...","provider":"...","model":"..."}` → spawn only
/// - `{"type":"list_models","provider":"...","refresh":false}` → live model list for any provider
/// - `{"type":"list_providers"}` → model-source catalog for all providers
///
/// `sender` is the bridge's configured identity (`--ws-identity`), stamped
/// as `meta.from` on the task envelope.
pub(super) async fn handle_client_command(
    client: &HubClient,
    text: &str,
    sender: &str,
) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(text).context("invalid JSON command")?;
    let cmd = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match cmd {
        "send_message" => {
            let to = v
                .get("to")
                .and_then(|t| t.as_str())
                .context("send_message requires to")?;
            let message = v
                .get("message")
                .and_then(|t| t.as_str())
                .context("send_message requires message")?;
            let provider = v
                .get("provider")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string());
            let model = v
                .get("model")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string());
            let ensure = v
                .get("ensure_worker")
                .and_then(|t| t.as_bool())
                .unwrap_or(true);

            let mut ensure_status = serde_json::Value::Null;
            if ensure {
                if let Some(ref prov) = provider {
                    let mut ensure_req = serde_json::json!({
                        "identity": to,
                        "provider": prov,
                    });
                    if let Some(ref m) = model {
                        ensure_req["model"] = serde_json::Value::String(m.clone());
                    }
                    match client
                        .request_json(
                            "hub.worker.ensure",
                            ensure_req,
                            std::time::Duration::from_secs(45),
                        )
                        .await
                    {
                        Ok(resp) => {
                            ensure_status = resp.clone();
                            if resp.get("ok") == Some(&serde_json::Value::Bool(false)) {
                                return Ok(format!(
                                    r#"{{"type":"error","message":"worker ensure failed","detail":{}}}"#,
                                    resp
                                ));
                            }
                        }
                        Err(e) => {
                            // Supervisor down — still deliver the task (manual workers may exist)
                            warn!("worker ensure failed (continuing to publish task): {e}");
                            ensure_status = serde_json::json!({
                                "ok": false,
                                "error": e.to_string(),
                                "continued": true,
                            });
                        }
                    }
                }
            }

            let task_short = &Uuid::new_v4().to_string()[..8];
            let task_channel = format!("task.{task_short}");
            let payload = serde_json::json!({
                "prompt": message,
                "source": "visualizer",
                "provider": provider,
            });
            let env = Envelope::new(sender, &task_channel, MessageKind::Message, payload).to(to);
            client.send(&env).await?;

            Ok(format!(
                r#"{{"type":"ack","action":"send_message","to":{},"task_channel":{},"ensure":{}}}"#,
                serde_json::to_string(to).unwrap(),
                serde_json::to_string(&task_channel).unwrap(),
                ensure_status
            ))
        }
        "ensure_worker" => {
            let identity = v
                .get("identity")
                .and_then(|t| t.as_str())
                .context("ensure_worker requires identity")?;
            let provider = v
                .get("provider")
                .and_then(|t| t.as_str())
                .context("ensure_worker requires provider")?;
            let model = v
                .get("model")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string());
            let mut ensure_req = serde_json::json!({ "identity": identity, "provider": provider });
            if let Some(ref m) = model {
                ensure_req["model"] = serde_json::Value::String(m.clone());
            }
            match client
                .request_json(
                    "hub.worker.ensure",
                    ensure_req,
                    std::time::Duration::from_secs(45),
                )
                .await
            {
                Ok(resp) => Ok(format!(
                    r#"{{"type":"ack","action":"ensure_worker","detail":{}}}"#,
                    resp
                )),
                Err(e) => Ok(format!(
                    r#"{{"type":"error","message":"ensure_worker failed: {}"}}"#,
                    e.to_string().replace('"', "'")
                )),
            }
        }
        "stop_agent" => {
            let identity = v
                .get("identity")
                .and_then(|t| t.as_str())
                .context("stop_agent requires identity")?;
            let _ = client
                .request_json(
                    "hub.worker.stop",
                    serde_json::json!({ "identity": identity }),
                    std::time::Duration::from_secs(10),
                )
                .await;
            let channel = format!("agents.{identity}");
            client.send_status(&channel, "closed").await?;
            let _ = client
                .send_message(
                    &channel,
                    serde_json::json!({
                        "message": format!("visualizer stop requested for {identity}"),
                        "action": "stop",
                        "source": "visualizer",
                    }),
                )
                .await;
            Ok(format!(
                r#"{{"type":"ack","action":"stop_agent","identity":{}}}"#,
                serde_json::to_string(identity).unwrap()
            ))
        }
        "resume_agent" => {
            let identity = v
                .get("identity")
                .and_then(|t| t.as_str())
                .context("resume_agent requires identity")?;
            let channel = format!("agents.{identity}");
            client.send_status(&channel, "ready").await?;
            let _ = client
                .send_message(
                    &channel,
                    serde_json::json!({
                        "message": format!("visualizer resume requested for {identity}"),
                        "action": "resume",
                        "source": "visualizer",
                    }),
                )
                .await;
            Ok(format!(
                r#"{{"type":"ack","action":"resume_agent","identity":{}}}"#,
                serde_json::to_string(identity).unwrap()
            ))
        }
        "list_models" => {
            let provider = v
                .get("provider")
                .and_then(|t| t.as_str())
                .context("list_models requires provider")?;
            let refresh = v.get("refresh").and_then(|t| t.as_bool()).unwrap_or(false);
            match client
                .request_json(
                    "hub.worker.models",
                    serde_json::json!({ "provider": provider, "refresh": refresh }),
                    std::time::Duration::from_secs(45),
                )
                .await
            {
                Ok(resp) => Ok(format!(
                    r#"{{"type":"models","provider":{},"detail":{}}}"#,
                    serde_json::to_string(provider).unwrap(),
                    resp
                )),
                Err(e) => Ok(format!(
                    r#"{{"type":"error","message":"list_models failed: {}"}}"#,
                    e.to_string().replace('"', "'")
                )),
            }
        }
        "list_providers" => {
            match client
                .request_json(
                    "hub.worker.providers",
                    serde_json::json!({}),
                    std::time::Duration::from_secs(10),
                )
                .await
            {
                Ok(resp) => Ok(format!(r#"{{"type":"providers","detail":{}}}"#, resp)),
                Err(e) => Ok(format!(
                    r#"{{"type":"error","message":"list_providers failed: {}"}}"#,
                    e.to_string().replace('"', "'")
                )),
            }
        }
        other => Ok(format!(
            r#"{{"type":"error","message":"unknown command: {}"}}"#,
            other.replace('"', "'")
        )),
    }
}
