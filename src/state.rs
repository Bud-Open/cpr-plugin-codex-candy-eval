//! 插件运行态与持久化：凭证历史、进行中的测试进度与宿主状态存储读写。
//!
//! 宿主状态命名空间的单记录上限为 256 KiB，因此写入前会按序列化体积裁剪最旧记录，
//! 保证状态既能持久化又不会因超限而整体丢失。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use gateway_plugin_sdk::call::host::{StatePutRequest, StatePutResult};
use gateway_plugin_sdk::client::HostClient;
use tokio::sync::RwLock;

use crate::fingerprint::FingerprintResult;
use crate::host_calls;
use crate::model::*;
use crate::modeltrace::TraceResult;

pub const STATE_NAMESPACE: &str = "candy-eval";
pub const STATE_KEY: &str = "global";
/// 宿主允许 256 KiB；留出余量给序列化开销与并发写入。
const MAXIMUM_STATE_BYTES: usize = 240 * 1024;

#[derive(Debug, Default)]
struct Inner {
    credentials: BTreeMap<String, Credential>,
    results: BTreeMap<String, Vec<CandyResult>>,
    pelicans: BTreeMap<String, Vec<PelicanResult>>,
    fingerprints: BTreeMap<String, Vec<FingerprintResult>>,
    traces: BTreeMap<String, Vec<TraceResult>>,
    running: BTreeMap<String, RunProgress>,
    pelican_running: BTreeSet<String>,
    fingerprint_running: BTreeMap<String, FingerprintProgress>,
    trace_running: BTreeMap<String, TraceProgress>,
    /// 已请求取消的凭证；采集循环在每个样本前检查。
    cancelled: BTreeSet<String>,
    /// 最近一次巡检结果。
    inspection: serde_json::Value,
    /// 读取到损坏状态时暂停保存，避免覆盖用户原文件。
    load_error: Option<String>,
    storage_error: String,
    loaded: bool,
    /// 宿主状态记录的当前版本；`None` 表示键尚不存在，只能创建。
    version: Option<u64>,
}

#[derive(Clone)]
pub struct StateManager {
    inner: Arc<RwLock<Inner>>,
}

impl Default for StateManager {
    fn default() -> Self {
        Self::new()
    }
}

impl StateManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
        }
    }

    /// 首次访问时从宿主读取持久化状态；损坏时只记录错误，不阻塞页面。
    pub async fn ensure_loaded(&self, host: &HostClient) {
        {
            let inner = self.inner.read().await;
            if inner.loaded {
                return;
            }
        }
        let loaded = host_calls::get_state(host, STATE_NAMESPACE, STATE_KEY)
            .await
            .ok()
            .and_then(|result| result.record);
        let version = loaded.as_ref().map(|record| record.version);
        let parsed = loaded.map(|record| serde_json::from_value::<PersistedState>(record.value));

        let mut inner = self.inner.write().await;
        if inner.loaded {
            return;
        }
        inner.loaded = true;
        inner.version = version;
        match parsed {
            None => {}
            Some(Ok(state)) => {
                inner.results = state.results;
                inner.pelicans = state.pelicans;
                inner.fingerprints = state.fingerprints;
                inner.traces = state.traces;
                for history in inner.results.values_mut() {
                    trim(history);
                }
                for history in inner.pelicans.values_mut() {
                    trim(history);
                }
                for history in inner.fingerprints.values_mut() {
                    trim(history);
                }
                for history in inner.traces.values_mut() {
                    trim(history);
                }
            }
            Some(Err(_)) => {
                inner.load_error = Some("测试记录格式无效，为保护原记录已暂停保存。".to_owned());
                inner.storage_error = inner.load_error.clone().unwrap_or_default();
            }
        }
    }

    /// 覆盖凭证列表；CPR 账号是唯一来源，不依赖前端同步。
    pub async fn set_credentials(&self, credentials: Vec<Credential>) {
        let mut inner = self.inner.write().await;
        inner.credentials = credentials
            .into_iter()
            .map(|credential| (credential.id.clone(), credential))
            .collect();
    }

    pub async fn credential(&self, id: &str) -> Option<Credential> {
        self.inner.read().await.credentials.get(id).cloned()
    }

    pub async fn credentials(&self) -> Vec<Credential> {
        self.inner
            .read()
            .await
            .credentials
            .values()
            .cloned()
            .collect()
    }

    /// 组装配发给前端的凭证视图，含各类历史与进行中进度。
    pub async fn views(&self) -> (Vec<CredentialView>, String) {
        let inner = self.inner.read().await;
        let views = inner
            .credentials
            .values()
            .map(|credential| {
                let id = &credential.id;
                CredentialView {
                    credential: credential.clone(),
                    running: inner.running.get(id).copied(),
                    results: inner.results.get(id).cloned().unwrap_or_default(),
                    pelican_running: inner.pelican_running.contains(id).then_some(true),
                    pelicans: inner
                        .pelicans
                        .get(id)
                        .map(|records| records.iter().map(PelicanResult::summary).collect())
                        .unwrap_or_default(),
                    fingerprint_running: inner.fingerprint_running.get(id).cloned(),
                    fingerprints: inner
                        .fingerprints
                        .get(id)
                        .map(|records| records.iter().map(FingerprintResult::summary).collect())
                        .unwrap_or_default(),
                    modeltrace_running: inner.trace_running.get(id).cloned(),
                    modeltraces: inner
                        .traces
                        .get(id)
                        .map(|records| records.iter().map(TraceResult::summary).collect())
                        .unwrap_or_default(),
                }
            })
            .collect();
        (views, inner.storage_error.clone())
    }

    pub async fn record(&self, id: &str) -> Option<PelicanResult> {
        let inner = self.inner.read().await;
        inner
            .pelicans
            .values()
            .flat_map(|records| records.iter())
            .find(|record| record.id == id)
            .cloned()
    }

    pub async fn set_running(&self, id: &str, progress: Option<RunProgress>) {
        let mut inner = self.inner.write().await;
        match progress {
            Some(progress) => {
                inner.running.insert(id.to_owned(), progress);
            }
            None => {
                inner.running.remove(id);
            }
        }
    }

    pub async fn set_pelican_running(&self, id: &str, running: bool) {
        let mut inner = self.inner.write().await;
        if running {
            inner.pelican_running.insert(id.to_owned());
        } else {
            inner.pelican_running.remove(id);
        }
    }

    pub async fn set_fingerprint_running(&self, id: &str, progress: Option<FingerprintProgress>) {
        let mut inner = self.inner.write().await;
        match progress {
            Some(progress) => {
                inner.fingerprint_running.insert(id.to_owned(), progress);
            }
            None => {
                inner.fingerprint_running.remove(id);
            }
        }
    }

    pub async fn set_trace_running(&self, id: &str, progress: Option<TraceProgress>) {
        let mut inner = self.inner.write().await;
        match progress {
            Some(progress) => {
                inner.trace_running.insert(id.to_owned(), progress);
            }
            None => {
                inner.trace_running.remove(id);
            }
        }
    }

    /// 更新进行中进度；仅当该凭证仍在测试时才写入，避免结束后的回填覆盖。
    pub async fn update_fingerprint(
        &self,
        id: &str,
        done: u32,
        valid: u32,
        errors: u32,
        phase: &str,
    ) {
        let mut inner = self.inner.write().await;
        if let Some(progress) = inner.fingerprint_running.get_mut(id) {
            progress.done = done;
            progress.valid = valid;
            progress.errors = errors;
            progress.phase = phase.to_owned();
        }
    }

    pub async fn update_trace(&self, id: &str, done: u32, phase: &str) {
        let mut inner = self.inner.write().await;
        if let Some(progress) = inner.trace_running.get_mut(id) {
            progress.done = done;
            progress.phase = phase.to_owned();
        }
    }

    pub async fn cancel(&self, id: &str) {
        let mut inner = self.inner.write().await;
        inner.cancelled.insert(id.to_owned());
        if let Some(progress) = inner.fingerprint_running.get_mut(id) {
            progress.phase = "cancelling".to_owned();
        }
        if let Some(progress) = inner.trace_running.get_mut(id) {
            progress.phase = "cancelling".to_owned();
        }
    }

    pub async fn is_cancelled(&self, id: &str) -> bool {
        self.inner.read().await.cancelled.contains(id)
    }

    pub async fn clear_cancel(&self, id: &str) {
        self.inner.write().await.cancelled.remove(id);
    }

    pub async fn fingerprint_record(&self, id: &str) -> Option<FingerprintResult> {
        let inner = self.inner.read().await;
        inner
            .fingerprints
            .values()
            .flat_map(|records| records.iter())
            .find(|record| record.id == id)
            .cloned()
    }

    pub async fn trace_record(&self, id: &str) -> Option<TraceResult> {
        let inner = self.inner.read().await;
        inner
            .traces
            .values()
            .flat_map(|records| records.iter())
            .find(|record| record.id == id)
            .cloned()
    }

    pub async fn set_inspection(&self, value: serde_json::Value) {
        self.inner.write().await.inspection = value;
    }

    pub async fn inspection(&self) -> serde_json::Value {
        self.inner.read().await.inspection.clone()
    }

    /// 某凭证是否已有测试在进行中。
    pub async fn busy(&self, id: &str) -> bool {
        let inner = self.inner.read().await;
        inner.running.contains_key(id)
            || inner.pelican_running.contains(id)
            || inner.fingerprint_running.contains_key(id)
            || inner.trace_running.contains_key(id)
    }

    /// 追加糖果记录并立即持久化。
    pub async fn push_candy(&self, host: &HostClient, id: &str, record: CandyResult) {
        {
            let mut inner = self.inner.write().await;
            append_recent(inner.results.entry(id.to_owned()).or_default(), record);
        }
        self.persist(host).await;
    }

    /// 追加鹈鹕记录并立即持久化。
    pub async fn push_pelican(&self, host: &HostClient, id: &str, record: PelicanResult) {
        {
            let mut inner = self.inner.write().await;
            append_recent(inner.pelicans.entry(id.to_owned()).or_default(), record);
        }
        self.persist(host).await;
    }

    /// 追加指纹记录并立即持久化。
    pub async fn push_fingerprint(&self, host: &HostClient, id: &str, record: FingerprintResult) {
        {
            let mut inner = self.inner.write().await;
            append_recent(inner.fingerprints.entry(id.to_owned()).or_default(), record);
        }
        self.persist(host).await;
    }

    /// 追加深度追踪记录并立即持久化。
    pub async fn push_trace(&self, host: &HostClient, id: &str, record: TraceResult) {
        {
            let mut inner = self.inner.write().await;
            append_recent(inner.traces.entry(id.to_owned()).or_default(), record);
        }
        self.persist(host).await;
    }

    /// 清空指定范围的历史；未知范围返回错误。
    pub async fn clear(&self, host: &HostClient, scope: &str) -> Result<(), String> {
        {
            let mut inner = self.inner.write().await;
            match scope {
                "candy" => inner.results.clear(),
                "pelican" => inner.pelicans.clear(),
                "fingerprint" => inner.fingerprints.clear(),
                "modeltrace" => inner.traces.clear(),
                "all" => {
                    inner.results.clear();
                    inner.pelicans.clear();
                    inner.fingerprints.clear();
                    inner.traces.clear();
                }
                other => return Err(format!("未知测试类型：{other}")),
            }
        }
        self.persist(host).await;
        Ok(())
    }

    /// 快照并写入宿主；超限时裁剪最旧记录后重试，版本冲突时重读版本重试一次。
    async fn persist(&self, host: &HostClient) {
        let (value, expected_version) = {
            let mut inner = self.inner.write().await;
            if let Some(error) = inner.load_error.clone() {
                inner.storage_error = error;
                return;
            }
            let mut state = PersistedState {
                results: inner.results.clone(),
                pelicans: inner.pelicans.clone(),
                fingerprints: inner.fingerprints.clone(),
                traces: inner.traces.clone(),
            };
            let mut value = serde_json::to_value(&state).unwrap_or_default();
            // 超限时反复丢弃最长的历史中最早的一条记录，直到落在配额内。
            while encoded_len(&value) > MAXIMUM_STATE_BYTES && prune_oldest(&mut state) {
                value = serde_json::to_value(&state).unwrap_or_default();
            }
            inner.results = state.results;
            inner.pelicans = state.pelicans;
            inner.fingerprints = state.fingerprints;
            inner.traces = state.traces;
            (value, inner.version)
        };

        let mut outcome = put(host, value.clone(), expected_version).await;
        // 版本漂移（例如另一次调用刚写入）时重读版本再写一次。
        if outcome.is_err() {
            let current = host_calls::get_state(host, STATE_NAMESPACE, STATE_KEY)
                .await
                .ok()
                .and_then(|result| result.record)
                .map(|record| record.version);
            if let Some(version) = current {
                self.inner.write().await.version = Some(version);
                outcome = put(host, value, Some(version)).await;
            }
        }

        let mut inner = self.inner.write().await;
        inner.storage_error = match outcome {
            Ok(result) => {
                inner.version = Some(result.version);
                String::new()
            }
            Err(error) => format!("测试记录保存失败：{}", error.message),
        };
    }
}

async fn put(
    host: &HostClient,
    value: serde_json::Value,
    expected_version: Option<u64>,
) -> Result<StatePutResult, gateway_plugin_sdk::PluginFault> {
    host_calls::put_state(
        host,
        &StatePutRequest {
            namespace: STATE_NAMESPACE.to_owned(),
            key: STATE_KEY.to_owned(),
            value,
            expected_version,
        },
    )
    .await
}

fn trim<T>(history: &mut Vec<T>) {
    if history.len() > HISTORY_LIMIT {
        let excess = history.len() - HISTORY_LIMIT;
        history.drain(..excess);
    }
}

fn encoded_len(value: &serde_json::Value) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

#[derive(Clone, Copy)]
enum History {
    Candy,
    Pelican,
    Fingerprint,
    ModelTrace,
}

/// 丢弃长度最大的历史中最早的一条记录；全部为空时返回 false。
fn prune_oldest(state: &mut PersistedState) -> bool {
    let mut best: Option<(History, String, usize)> = None;
    let mut consider = |kind: History, id: &str, len: usize| {
        if len == 0 {
            return;
        }
        let better = match best.as_ref() {
            Some((_, _, current)) => len > *current,
            None => true,
        };
        if better {
            best = Some((kind, id.to_owned(), len));
        }
    };
    for (id, history) in &state.results {
        consider(History::Candy, id, history.len());
    }
    for (id, history) in &state.pelicans {
        consider(History::Pelican, id, history.len());
    }
    for (id, history) in &state.fingerprints {
        consider(History::Fingerprint, id, history.len());
    }
    for (id, history) in &state.traces {
        consider(History::ModelTrace, id, history.len());
    }

    let Some((kind, id, _)) = best else {
        return false;
    };
    match kind {
        History::Candy => drop_oldest(&mut state.results, &id),
        History::Pelican => drop_oldest(&mut state.pelicans, &id),
        History::Fingerprint => drop_oldest(&mut state.fingerprints, &id),
        History::ModelTrace => drop_oldest(&mut state.traces, &id),
    }
    true
}

fn drop_oldest<T>(map: &mut BTreeMap<String, Vec<T>>, id: &str) {
    if let Some(history) = map.get_mut(id) {
        history.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candy(answer: &str) -> CandyResult {
        CandyResult {
            skipped: false,
            time: "2026-01-01T00:00:00.000Z".to_owned(),
            model: "gpt-6.1-sol".to_owned(),
            effort: "low".to_owned(),
            ok: true,
            answer: answer.to_owned(),
            error: String::new(),
            input_tokens: 1,
            output_tokens: 2,
            reasoning_tokens: 3,
            duration_ms: 4,
        }
    }

    #[test]
    fn prunes_the_longest_history_first() {
        let mut state = PersistedState::default();
        state
            .results
            .insert("a".to_owned(), vec![candy("1"), candy("2")]);
        state.results.insert("b".to_owned(), vec![candy("3")]);
        assert!(prune_oldest(&mut state));
        assert_eq!(state.results["a"].len(), 1);
        assert_eq!(state.results["b"].len(), 1);
        assert!(prune_oldest(&mut state));
        assert!(prune_oldest(&mut state));
        assert!(!prune_oldest(&mut state));
    }

    #[test]
    fn oversized_state_is_pruned_until_it_fits() {
        let mut state = PersistedState::default();
        // 约 300 KiB 的正文，必然超过单记录上限。
        let big = "x".repeat(30 * 1024);
        state.results.insert(
            "a".to_owned(),
            (0..HISTORY_LIMIT).map(|_| candy(&big)).collect(),
        );
        let mut value = serde_json::to_value(&state).unwrap();
        let mut guard = 0;
        while encoded_len(&value) > MAXIMUM_STATE_BYTES && prune_oldest(&mut state) {
            value = serde_json::to_value(&state).unwrap();
            guard += 1;
            assert!(guard < 100, "裁剪未能收敛");
        }
        assert!(encoded_len(&value) <= MAXIMUM_STATE_BYTES);
        assert!(state.results["a"].len() < HISTORY_LIMIT);
    }
}
