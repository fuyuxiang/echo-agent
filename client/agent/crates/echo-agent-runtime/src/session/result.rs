use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};
use serde_json::value::to_raw_value;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExtMethodError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl ExtMethodError {
    pub(crate) fn with_data<D: Serialize>(
        code: impl Into<String>,
        message: impl Into<String>,
        data: D,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            data: serde_json::to_value(data).ok(),
        }
    }
}

/// Extension method result: `{ result: T | null, error?: string | ExtMethodError }`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtMethodResult<T: Serialize> {
    pub result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
}

impl<T: Serialize> From<crate::terminal::TerminalExtError> for ExtMethodResult<T> {
    fn from(err: crate::terminal::TerminalExtError) -> Self {
        let data = err
            .terminal_id()
            .map(|id| serde_json::json!({ "terminalId": id }));
        Self {
            result: None,
            error: serde_json::to_value(ExtMethodError {
                code: err.code().to_string(),
                message: err.to_string(),
                data,
            })
            .ok(),
        }
    }
}

impl<T: Serialize> ExtMethodResult<T> {
    pub fn success(result: T) -> Self {
        Self {
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(error: impl std::fmt::Display) -> Self {
        Self {
            result: None,
            error: Some(serde_json::Value::String(error.to_string())),
        }
    }

    pub fn partial(result: T, error: impl std::fmt::Display) -> Self {
        Self {
            result: Some(result),
            error: Some(serde_json::Value::String(error.to_string())),
        }
    }

    pub fn from_result<E: std::fmt::Display>(result: Result<T, E>) -> Self {
        match result {
            Ok(value) => Self::success(value),
            Err(e) => Self::failure(e),
        }
    }

    pub fn to_ext_response(&self) -> anyhow::Result<acp::ExtResponse> {
        serde_json::to_value(self)
            .and_then(|v| to_raw_value(&v))
            .map(|raw| acp::ExtResponse::new(Arc::from(raw)))
            .map_err(|e| anyhow::anyhow!(e))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Empty {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Serialize)]
    struct TestData {
        nodes: Vec<String>,
        truncated: bool,
    }

    #[test]
    fn test_ext_method_result_serialization() {
        // Success case
        let data = TestData {
            nodes: vec!["test".to_string()],
            truncated: false,
        };
        let success: ExtMethodResult<TestData> = ExtMethodResult::from_result::<String>(Ok(data));
        let json = serde_json::to_value(&success).unwrap();

        // Should have "result" field
        assert!(
            json.get("result").is_some(),
            "Success case should have 'result' field"
        );
        assert!(
            json.get("error").is_none(),
            "Success case should not have 'error' field"
        );

        let result = json.get("result").unwrap();
        assert!(
            result.get("nodes").is_some(),
            "Result should have 'nodes' field"
        );

        println!(
            "Success JSON: {}",
            serde_json::to_string_pretty(&success).unwrap()
        );

        // Error case
        let error: ExtMethodResult<TestData> =
            ExtMethodResult::from_result::<&str>(Err("test error"));
        let json = serde_json::to_value(&error).unwrap();

        // Should have "result": null and "error" field
        assert!(
            json.get("result").is_some(),
            "Error case should have 'result' field"
        );
        assert_eq!(json.get("result").unwrap(), &serde_json::Value::Null);
        assert!(
            json.get("error").is_some(),
            "Error case should have 'error' field"
        );

        println!(
            "Error JSON: {}",
            serde_json::to_string_pretty(&error).unwrap()
        );
    }
}
