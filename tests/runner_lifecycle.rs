//! Real plugin-process lifecycle checks. All accounts, keys and model output are synthetic;
//! this fixture never opens a network connection or reads production configuration.
use gateway_plugin_sdk::call::host::ModelEventBatch;
use gateway_plugin_sdk::call::model::{ExecutionEvent, WireEvent, WirePayload};
use gateway_plugin_sdk::client::{read_frame, write_frame};
use gateway_plugin_sdk::{CallContext, Frame, Handshake, Message, Stage};
use serde_json::{json, Value};
use std::{collections::BTreeMap, process::Stdio, time::Duration};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

struct Fixture {
    _child: Child,
    input: ChildStdin,
    output: ChildStdout,
    states: BTreeMap<String, (Value, u64)>,
    replies: BTreeMap<u64, (u16, Value)>,
    held_reads: Vec<u64>,
    executions: usize,
}

impl Fixture {
    async fn new() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_codex-proxy-plugin-candy-eval"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let mut fixture = Self {
            _child: child,
            input,
            output,
            states: BTreeMap::new(),
            replies: BTreeMap::new(),
            held_reads: Vec::new(),
            executions: 0,
        };
        fixture
            .send(
                Message::Hello {
                    handshake: Handshake {
                        protocol_version: 2,
                        artifact_sha256: "fixture-artifact".into(),
                        plugin_id: "nmbb.codex-candy-eval".into(),
                        instance_id: "fixture-instance".into(),
                        generation: 1,
                        incarnation: "fixture-incarnation".into(),
                        configuration: json!({}),
                        contributes: codex_proxy_plugin_candy_eval::manifest()
                            .unwrap()
                            .contributes,
                    },
                },
                Vec::new(),
            )
            .await;
        let frame = fixture.read().await;
        assert!(matches!(frame.message, Message::Ready { .. }));
        fixture
    }

    async fn send(&mut self, message: Message, payload: Vec<u8>) {
        write_frame(&mut self.input, &Frame { message, payload })
            .await
            .unwrap();
    }

    async fn read(&mut self) -> Frame {
        tokio::time::timeout(Duration::from_secs(8), read_frame(&mut self.output))
            .await
            .expect("fixture deadline")
            .expect("plugin must remain alive")
    }

    async fn call(
        &mut self,
        id: u64,
        path: &str,
        body: Option<Value>,
        background: bool,
        timeout_ms: u64,
    ) {
        self.request(id, path, body, background, background, timeout_ms)
            .await;
    }

    async fn request(
        &mut self,
        id: u64,
        path: &str,
        body: Option<Value>,
        background: bool,
        confirmed: bool,
        timeout_ms: u64,
    ) {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let context = CallContext {
            call_id: id,
            instance_id: "fixture-instance".into(),
            generation: 1,
            incarnation: "fixture-incarnation".into(),
            stage: Stage::Management,
            timeout_ms,
            resource_stream: false,
            resource_scope_id: format!("fixture-scope-{id}"),
            request_id: None,
            attempt_id: None,
            account_id: None,
            credential_revision: None,
        };
        let headers = if confirmed {
            json!([{"name":"x-cpr-management-background","value":[49]}])
        } else {
            json!([])
        };
        self.send(
            Message::Call {
                id,
                method: "management.handle".into(),
                context,
                params: json!({
                    "method": if body.is_some() { "POST" } else { "GET" }, "path":path,
                    "query": if background { "__cpr_background=1" } else { query },
                    "content_type": if body.is_some() { Some("application/json") } else { None },
                    "headers":headers,
                })
                .try_into()
                .unwrap(),
            },
            body.map(|value| serde_json::to_vec(&value).unwrap())
                .unwrap_or_default(),
        )
        .await;
    }

    async fn pump(&mut self) {
        let frame = self.read().await;
        let (id, method, params) = match frame.message {
            Message::Callback {
                id, method, params, ..
            } => (id, method, params),
            Message::Result { id, result } => {
                let body = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
                self.replies
                    .insert(id, (result["status"].as_u64().unwrap() as u16, body));
                return;
            }
            Message::Error { id, .. } => {
                self.replies
                    .insert(id, (500, json!({"fixture_fault":true})));
                return;
            }
            Message::Cancelled { .. } => return,
            other => panic!("unexpected fixture frame: {other:?}"),
        };
        let mut payload = Vec::new();
        let result = match method.as_str() {
            "host.state.get" => {
                let record = self.states.get(params["key"].as_str().unwrap()).map(
                    |(value, version)| json!({"value":value,"version":version,"schema_version":1}),
                );
                json!({"record":record})
            }
            "host.state.put" => {
                let key = params["key"].as_str().unwrap().to_owned();
                let version = self.states.get(&key).map_or(1, |(_, version)| version + 1);
                self.states.insert(key, (params["value"].clone(), version));
                json!({"version":version})
            }
            "host.data.accounts.list" => {
                payload = serde_json::to_vec(&json!({"schema_version":1,"accounts":[{
                    "account_id":"fixture-account","provider_id":"openai","name":"fixture account",
                    "email":null,"group_ids":[],"enabled":true,"updated_at_ms":0,
                }],"next_cursor":null}))
                .unwrap();
                json!({})
            }
            "host.model.list" | "host.models.list" => json!({"models":["gpt-6.1-sol"]}),
            "host.keys.list" => {
                json!({"keys":[{"id":"fixture-key-id","name":"fixture","enabled":true}],"next_cursor":null})
            }
            "host.model.execute_stream" => {
                self.executions += 1;
                json!({"request_id":format!("fixture-request-{}", self.executions),"stream":format!("fixture-stream-{}", self.executions)})
            }
            "host.model.stream_read" => {
                self.held_reads.push(id);
                return;
            }
            "host.model.stream_close" => json!({}),
            other => panic!("unexpected fixture callback: {other}"),
        };
        self.send(Message::Result { id, result }, payload).await;
    }

    async fn reply(&mut self, id: u64) -> (u16, Value) {
        while !self.replies.contains_key(&id) {
            self.pump().await;
        }
        self.replies.remove(&id).unwrap()
    }

    async fn running(&mut self, id: u64) -> Value {
        self.call(id, "api/state", None, false, 5000).await;
        let (status, value) = self.reply(id).await;
        assert_eq!(status, 200);
        value["auths"][0].clone()
    }

    async fn start_trace(&mut self, timeout_ms: u64, background: bool) {
        self.call(
            1,
            "api/modeltrace/run",
            Some(json!({"model":"gpt-6.1-sol","concurrency":3,"all":true})),
            background,
            timeout_ms,
        )
        .await;
        while self.held_reads.len() < 3 {
            self.pump().await;
        }
        assert_eq!(self.executions, 3);
    }

    async fn complete_streams(&mut self) {
        let numbers = (1..=355)
            .cycle()
            .take(4096)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let event = json!({"type":"response.completed","response":{
            "id":"fixture-response","status":"completed","model":"gpt-6.1-sol",
            "output":[{"type":"message","content":[{"type":"output_text","text":numbers}]}],
            "usage":{"input_tokens":12,"output_tokens":4096}
        }});
        let payload = ModelEventBatch {
            events: vec![ExecutionEvent::wire(WireEvent {
                protocol: "openai".into(),
                payload: WirePayload::RawSse {
                    frame: format!("event: response.completed\ndata: {event}\n\n").into_bytes(),
                },
            })],
        }
        .encode()
        .unwrap();
        for id in std::mem::take(&mut self.held_reads) {
            self.send(
                Message::Result {
                    id,
                    result: json!({"events":1,"end":true}),
                },
                payload.clone(),
            )
            .await;
        }
    }
}

#[tokio::test]
async fn cancelled_management_call_does_not_leave_an_orphan_run() {
    let mut fixture = Fixture::new().await;
    fixture.start_trace(5000, false).await;
    assert!(fixture.running(3).await["modeltrace_running"].is_object());
    fixture.send(Message::Cancel { id: 1 }, Vec::new()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(fixture.running(5).await["modeltrace_running"].is_null());
    fixture
        .call(
            7,
            "api/modeltrace/cancel",
            Some(json!({"id":"fixture-account"})),
            false,
            5000,
        )
        .await;
    assert_eq!(fixture.reply(7).await.0, 200);
    assert!(fixture.running(9).await["modeltrace_running"].is_null());
}

#[tokio::test]
async fn management_deadline_releases_the_running_state() {
    let mut fixture = Fixture::new().await;
    fixture.start_trace(500, false).await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(fixture.running(3).await["modeltrace_running"].is_null());
}

#[tokio::test]
async fn retained_host_scope_continues_collecting_when_the_page_is_gone() {
    let mut fixture = Fixture::new().await;
    fixture.start_trace(5000, true).await;
    // The host's retained scope is tested independently in gateway-api. Page disposal
    // does not send Cancel to this IPC call; a newly opened page queries the same job.
    assert_eq!(fixture.running(3).await["modeltrace_running"]["done"], 0);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(fixture.running(5).await["modeltrace_running"].is_object());
    fixture.complete_streams().await;
    assert_eq!(fixture.reply(1).await.0, 200);
    let state = fixture.running(7).await;
    assert!(state["modeltrace_running"].is_null());
    assert_eq!(state["modeltraces"].as_array().unwrap().len(), 1);
    assert_eq!(state["modeltraces"][0]["status"], "completed");
    let path = format!(
        "api/modeltrace/record?id={}",
        state["modeltraces"][0]["id"].as_str().unwrap()
    );
    fixture.call(9, &path, None, false, 5000).await;
    let (status, detail) = fixture.reply(9).await;
    assert_eq!(status, 200);
    assert_eq!(detail["samples"].as_array().unwrap().len(), 3);
    let saved_states = fixture.states.clone();
    drop(fixture);
    let mut reloaded = Fixture::new().await;
    reloaded.states = saved_states;
    assert!(reloaded.running(1).await["modeltrace_running"].is_null());
    reloaded.call(3, &path, None, false, 5000).await;
    assert_eq!(reloaded.reply(3).await, (200, detail));
}

#[tokio::test]
async fn explicit_stop_still_cancels_a_retained_background_run() {
    let mut fixture = Fixture::new().await;
    fixture.start_trace(5000, true).await;
    fixture
        .call(
            3,
            "api/modeltrace/cancel",
            Some(json!({"id":"fixture-account"})),
            false,
            5000,
        )
        .await;
    assert_eq!(fixture.reply(3).await.0, 200);
    assert_eq!(fixture.reply(1).await.0, 200);
    assert!(fixture.running(5).await["modeltrace_running"].is_null());
    fixture
        .call(
            7,
            "api/modeltrace/run",
            Some(json!({"model":"gpt-6.1-sol","concurrency":3,"all":true})),
            true,
            5000,
        )
        .await;
    let old_count = fixture.executions;
    while fixture.executions == old_count {
        fixture.pump().await;
    }
    assert_eq!(fixture.executions, old_count + 1);
}

#[tokio::test]
async fn background_mode_on_an_unpatched_host_fails_before_model_execution() {
    let mut fixture = Fixture::new().await;
    fixture
        .request(
            1,
            "api/modeltrace/run",
            Some(json!({"model":"gpt-6.1-sol","all":true})),
            true,
            false,
            5000,
        )
        .await;
    let (status, _) = fixture.reply(1).await;
    assert_eq!(status, 412);
    assert_eq!(fixture.executions, 0);
    assert!(fixture.states.is_empty());
}
