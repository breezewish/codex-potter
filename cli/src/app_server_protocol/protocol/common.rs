use serde::Deserialize;
use serde::Serialize;

use crate::app_server_protocol::JSONRPCRequest;
use crate::app_server_protocol::RequestId;

use super::v1;
use super::v2;

/// Request from the client to the server.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "method", rename_all = "camelCase")]
pub enum ClientRequest {
    Initialize {
        #[serde(rename = "id")]
        request_id: RequestId,
        params: v1::InitializeParams,
    },

    #[serde(rename = "thread/start")]
    ThreadStart {
        #[serde(rename = "id")]
        request_id: RequestId,
        params: v2::ThreadStartParams,
    },

    #[serde(rename = "thread/rollback")]
    ThreadRollback {
        #[serde(rename = "id")]
        request_id: RequestId,
        params: v2::ThreadRollbackParams,
    },

    #[serde(rename = "turn/start")]
    TurnStart {
        #[serde(rename = "id")]
        request_id: RequestId,
        params: v2::TurnStartParams,
    },
}

/// Notification from the client to the server.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "method", rename_all = "camelCase")]
pub enum ClientNotification {
    Initialized,
}

/// Request initiated from the server and sent to the client.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "method", rename_all = "camelCase")]
pub enum ServerRequest {
    #[serde(rename = "item/commandExecution/requestApproval")]
    CommandExecution {
        #[serde(rename = "id")]
        request_id: RequestId,
        #[serde(default)]
        params: Option<serde_json::Value>,
    },

    #[serde(rename = "item/fileChange/requestApproval")]
    FileChange {
        #[serde(rename = "id")]
        request_id: RequestId,
        #[serde(default)]
        params: Option<serde_json::Value>,
    },

    ApplyPatch {
        #[serde(rename = "id")]
        request_id: RequestId,
        #[serde(default)]
        params: Option<serde_json::Value>,
    },

    ExecCommand {
        #[serde(rename = "id")]
        request_id: RequestId,
        #[serde(default)]
        params: Option<serde_json::Value>,
    },
}

impl TryFrom<JSONRPCRequest> for ServerRequest {
    type Error = serde_json::Error;

    fn try_from(value: JSONRPCRequest) -> Result<Self, Self::Error> {
        serde_json::from_value(serde_json::to_value(value)?)
    }
}

#[cfg(test)]
mod tests {
    use super::v1::ClientInfo;
    use super::v2::ThreadRollbackParams;
    use super::v2::ThreadStartParams;
    use super::v2::TurnStartParams;
    use super::*;

    #[test]
    fn serialize_initialized_notification_has_no_params_field() {
        let notification = ClientNotification::Initialized;
        let value = serde_json::to_value(&notification).expect("serialize notification");
        assert_eq!(value["method"], "initialized");
        assert!(
            value.get("params").is_none(),
            "Initialized should not include a params field"
        );
    }

    #[test]
    fn serialize_thread_start_includes_null_option_fields() {
        let request = ClientRequest::ThreadStart {
            request_id: RequestId::Integer(1),
            params: ThreadStartParams {
                model: None,
                model_provider: None,
                cwd: None,
                approval_policy: Some(crate::app_server_protocol::AskForApproval::Never),
                sandbox: None,
                config: None,
                base_instructions: None,
                developer_instructions: None,
                experimental_raw_events: false,
            },
        };

        let value = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(value["method"], "thread/start");
        assert_eq!(value["id"], 1);

        let params = value["params"].as_object().expect("params object");
        for key in [
            "model",
            "modelProvider",
            "cwd",
            "approvalPolicy",
            "sandbox",
            "config",
            "baseInstructions",
            "developerInstructions",
        ] {
            assert!(
                params.contains_key(key),
                "thread/start params must contain key {key}"
            );
        }
        assert_eq!(value["params"]["approvalPolicy"], "never");
        assert_eq!(value["params"]["experimentalRawEvents"], false);
    }

    #[test]
    fn serialize_turn_start_includes_output_schema_key() {
        let request = ClientRequest::TurnStart {
            request_id: RequestId::Integer(2),
            params: TurnStartParams {
                thread_id: "thread-1".to_string(),
                input: Vec::new(),
                cwd: None,
                approval_policy: None,
                sandbox_policy: None,
                model: None,
                effort: None,
                summary: None,
                output_schema: None,
                collaboration_mode: None,
            },
        };

        let value = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(value["method"], "turn/start");
        assert_eq!(value["id"], 2);

        let params = value["params"].as_object().expect("params object");
        for key in [
            "threadId",
            "input",
            "cwd",
            "approvalPolicy",
            "sandboxPolicy",
            "model",
            "effort",
            "summary",
            "outputSchema",
            "collaborationMode",
        ] {
            assert!(
                params.contains_key(key),
                "turn/start params must contain key {key}"
            );
        }
    }

    #[test]
    fn serialize_thread_rollback_includes_num_turns() {
        let request = ClientRequest::ThreadRollback {
            request_id: RequestId::Integer(3),
            params: ThreadRollbackParams {
                thread_id: "thread-1".to_string(),
                num_turns: 1,
            },
        };

        let value = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(value["method"], "thread/rollback");
        assert_eq!(value["id"], 3);

        let params = value["params"].as_object().expect("params object");
        for key in ["threadId", "numTurns"] {
            assert!(
                params.contains_key(key),
                "thread/rollback params must contain key {key}"
            );
        }
        assert_eq!(value["params"]["numTurns"], 1);
    }

    #[test]
    fn serialize_initialize_request() {
        let request = ClientRequest::Initialize {
            request_id: RequestId::Integer(4),
            params: v1::InitializeParams {
                client_info: ClientInfo {
                    name: "codex-potter".to_string(),
                    title: Some("codex-potter".to_string()),
                    version: "0.0.0".to_string(),
                },
            },
        };

        let value = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(value["method"], "initialize");
        assert_eq!(value["id"], 3);
        assert_eq!(value["params"]["clientInfo"]["name"], "codex-potter");
    }
}
