// SPDX-License-Identifier: MIT OR Apache-2.0
//! MCP client driven by the capability_workflow acceptance harness.
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};

pub struct Client {
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
    id: u64,
}
impl Client {
    pub fn new(input: ChildStdin, output: ChildStdout) -> Self {
        Self {
            input,
            output: BufReader::new(output).lines(),
            id: 0,
        }
    }
    pub async fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = self.id;
        let request = json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params});
        tokio::time::timeout(Duration::from_secs(15), async {
            self.input
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            self.input.flush().await.unwrap();
            loop {
                let line = self
                    .output
                    .next_line()
                    .await
                    .unwrap()
                    .expect("MCP process exited");
                let response: Value = serde_json::from_str(&line).unwrap();
                if response["id"] == id {
                    assert!(response.get("error").is_none(), "{response}");
                    return response["result"].clone();
                }
                assert!(
                    response.get("method").is_some(),
                    "unexpected response: {response}"
                );
            }
        })
        .await
        .expect("bounded MCP request")
    }
    pub async fn initialize(&mut self) {
        let result = self
            .request(
                "initialize",
                json!({"protocolVersion":"2024-11-05",
            "capabilities":{}, "clientInfo":{"name":"journey-client","version":"1"}}),
            )
            .await;
        assert!(result["serverInfo"].is_object());
        self.input
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
    }
    pub async fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name":name,"arguments":arguments}))
            .await
    }
}
pub fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().expect("text result")
}
