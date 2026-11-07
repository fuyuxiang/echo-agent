//! Shared, bounded on-device retrieval inference. No network or chat credentials.
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, RwLock},
};

pub const EMBEDDING_MODEL: &str = "BAAI/bge-base-zh-v1.5";
pub const RERANK_MODEL: &str = "BAAI/bge-reranker-base";
pub const DIMENSIONS: usize = 768;
pub const PIPELINE_VERSION: &str = "bge-zh-int8-cls-normalized-query-v1-chunks-v3";
pub const MIN_RELEVANCE: f64 = 0.35;
pub const MODEL_ASSETS: &str = include_str!("model-assets.json");
const QUERY_INSTRUCTION: &str = "为这个句子生成表示以用于检索相关文章：";

static ROOT: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

pub fn configure(root: PathBuf) {
    *ROOT
        .get_or_init(Default::default)
        .write()
        .unwrap_or_else(|e| e.into_inner()) = Some(root);
}

pub fn model_root() -> Option<PathBuf> {
    ROOT.get_or_init(Default::default).read().ok()?.clone()
}

pub fn model_files_ready(root: &Path, reranker: bool) -> bool {
    let expected = serde_json::from_str::<serde_json::Value>(MODEL_ASSETS).ok();
    let verified = std::fs::read(root.join("verified.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let directory = if reranker { "reranker/" } else { "embedding/" };
    expected.as_ref().is_some_and(|manifest| {
        manifest
            .get("assets")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|assets| {
                assets
                    .iter()
                    .filter(|asset| {
                        asset["path"]
                            .as_str()
                            .is_some_and(|path| path.starts_with(directory))
                    })
                    .all(|asset| {
                        let Some(path) = asset["path"].as_str() else {
                            return false;
                        };
                        std::fs::symlink_metadata(root.join(path)).is_ok_and(|metadata| {
                            metadata.is_file()
                                && !metadata.file_type().is_symlink()
                                && Some(metadata.len()) == asset["size"].as_u64()
                        })
                    })
            })
    }) && expected == verified
}

pub fn device_supported() -> bool {
    if !cfg!(feature = "local-inference") {
        return false;
    }
    #[cfg(target_arch = "x86_64")]
    {
        std::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        true
    }
}

pub fn available(reranker: bool) -> bool {
    device_supported() && model_root().is_some_and(|root| model_files_ready(&root, reranker))
}

/// Until tokenizers are installed, a byte budget is a conservative offline bound.
pub fn token_count(text: &str) -> usize {
    #[cfg(feature = "local-inference")]
    if available(false) {
        if let Some(count) = inference::count_tokens(text) {
            return count;
        }
    }
    text.len()
}

pub fn fingerprint() -> String {
    format!(
        "local:{EMBEDDING_MODEL}:{DIMENSIONS}:{PIPELINE_VERSION}:{}",
        &blake3::hash(MODEL_ASSETS.as_bytes()).to_hex()[..16]
    )
}

#[derive(Clone)]
pub struct LocalEmbeddingProvider {
    permit: Option<super::embedding::MemoryRequestPermit>,
}

impl LocalEmbeddingProvider {
    pub fn new() -> Option<Self> {
        available(false).then_some(Self { permit: None })
    }
    pub fn with_request_permit(mut self, permit: super::embedding::MemoryRequestPermit) -> Self {
        self.permit = Some(permit);
        self
    }
    async fn embed(
        &self,
        texts: &[&str],
        query: bool,
    ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        if let Some(permit) = &self.permit {
            permit(texts)?;
        }
        #[cfg(feature = "local-inference")]
        {
            let result = inference::run(texts, None, query, self.permit.clone()).await?;
            if let Some(permit) = &self.permit {
                permit(texts)?;
            }
            Ok(result)
        }
        #[cfg(not(feature = "local-inference"))]
        {
            let _ = query;
            Err("local inference is unavailable in this build".into())
        }
    }
}

#[async_trait::async_trait]
impl super::embedding::EmbeddingProvider for LocalEmbeddingProvider {
    async fn embed_batch(
        &self,
        texts: &[&str],
    ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        self.embed(texts, false).await
    }
    async fn embed_query(&self, query: &str) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        self.embed(&[query], true)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| "missing query vector".into())
    }
    async fn embed_batch_with_policy(
        &self,
        texts: &[&str],
        permit: super::embedding::MemoryRequestPermit,
    ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        let previous = self.permit.clone();
        self.clone()
            .with_request_permit(Arc::new(move |texts| {
                if let Some(previous) = &previous {
                    previous(texts)?;
                }
                permit(texts)
            }))
            .embed_batch(texts)
            .await
    }
    fn model_name(&self) -> &str {
        EMBEDDING_MODEL
    }
    fn dimensions(&self) -> usize {
        DIMENSIONS
    }
    fn fingerprint(&self) -> String {
        fingerprint()
    }
}

pub async fn rerank(
    query: &str,
    documents: &[&str],
    permit: Option<super::embedding::MemoryRequestPermit>,
) -> Result<Vec<f64>, String> {
    #[cfg(feature = "local-inference")]
    {
        let values = inference::run(documents, Some(query), true, permit).await?;
        Ok(values
            .into_iter()
            .map(|value| sigmoid(value[0] as f64))
            .collect())
    }
    #[cfg(not(feature = "local-inference"))]
    {
        let _ = (query, documents, permit);
        Err("local inference is unavailable in this build".into())
    }
}

/// Validate model execution during installation so ready means usable on this CPU.
pub async fn warmup() -> Result<(), String> {
    use super::embedding::EmbeddingProvider;
    let provider = LocalEmbeddingProvider::new().ok_or("本地模型文件尚未准备完整")?;
    provider
        .embed_query("本地检索准备检查")
        .await
        .map_err(|error| error.to_string())?;
    rerank("本地检索", &["本地检索模型用于相关资料排序"], None).await?;
    Ok(())
}

fn sigmoid(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let e = value.exp();
        e / (1.0 + e)
    }
}

#[cfg(feature = "local-inference")]
mod inference {
    use super::*;
    use ort::{
        session::{RunOptions, Session},
        value::{DynValue, Tensor},
    };
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tokenizers::{Tokenizer, TruncationParams, TruncationStrategy};

    static ENV: OnceLock<Result<(), String>> = OnceLock::new();
    static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);
    static TOKENS: Mutex<Option<(PathBuf, Tokenizers)>> = Mutex::new(None);
    static PERMITS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    static QUEUED: AtomicUsize = AtomicUsize::new(0);
    static FOREGROUND: AtomicUsize = AtomicUsize::new(0);
    static WATCHDOG: OnceLock<()> = OnceLock::new();

    struct Engine {
        root: PathBuf,
        embedding: Option<Session>,
        reranker: Option<Session>,
        last_used: std::time::Instant,
    }
    fn idle_watchdog() {
        WATCHDOG.get_or_init(|| {
            tokio::spawn(async {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    if let Ok(mut engine) = ENGINE.try_lock() {
                        if engine
                            .as_ref()
                            .is_some_and(|engine| engine.last_used.elapsed().as_secs() >= 300)
                        {
                            *engine = None;
                        }
                    }
                }
            });
        });
    }
    struct QueueGuard(bool);
    impl Drop for QueueGuard {
        fn drop(&mut self) {
            QUEUED.fetch_sub(1, Ordering::AcqRel);
            if self.0 {
                FOREGROUND.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }
    struct CancelRun(Arc<RunOptions>);
    impl Drop for CancelRun {
        fn drop(&mut self) {
            let _ = self.0.terminate();
        }
    }

    fn environment() -> Result<(), String> {
        ENV.get_or_init(|| {
            if !device_supported() {
                return Err("本机 CPU 暂不支持本地语义模型，已使用关键词检索".into());
            }
            ort::init()
                .with_name("Echo local retrieval")
                .with_telemetry(false)
                .commit()
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .clone()
    }

    #[derive(Clone)]
    struct Tokenizers {
        embedding: Arc<Tokenizer>,
        reranker: Arc<Tokenizer>,
        counting_embedding: Arc<Tokenizer>,
        counting_reranker: Arc<Tokenizer>,
    }

    impl Tokenizers {
        fn new(mut embedding: Tokenizer, mut reranker: Tokenizer) -> Result<Self, String> {
            embedding.with_padding(None);
            reranker.with_padding(None);
            embedding.with_truncation(None).map_err(|e| e.to_string())?;
            reranker.with_truncation(None).map_err(|e| e.to_string())?;
            let counting_embedding = Arc::new(embedding.clone());
            let counting_reranker = Arc::new(reranker.clone());
            let truncation = TruncationParams {
                max_length: 512,
                strategy: TruncationStrategy::LongestFirst,
                ..Default::default()
            };
            embedding
                .with_truncation(Some(truncation.clone()))
                .map_err(|e| e.to_string())?;
            reranker
                .with_truncation(Some(truncation))
                .map_err(|e| e.to_string())?;
            Ok(Self {
                embedding: Arc::new(embedding),
                reranker: Arc::new(reranker),
                counting_embedding,
                counting_reranker,
            })
        }

        fn count(&self, text: &str) -> Option<usize> {
            Some(
                self.counting_embedding
                    .encode(text, true)
                    .ok()?
                    .len()
                    .max(self.counting_reranker.encode(text, true).ok()?.len()),
            )
        }
    }

    fn tokenizers(root: &Path) -> Result<Tokenizers, String> {
        let mut cached = TOKENS.lock().map_err(|_| "tokenizer lock unavailable")?;
        if cached.as_ref().is_none_or(|(path, _)| path != root) {
            let embedding = Tokenizer::from_file(root.join("embedding/tokenizer.json"))
                .map_err(|e| e.to_string())?;
            let reranker = Tokenizer::from_file(root.join("reranker/tokenizer.json"))
                .map_err(|e| e.to_string())?;
            *cached = Some((root.to_owned(), Tokenizers::new(embedding, reranker)?));
        }
        Ok(cached.as_ref().unwrap().1.clone())
    }

    pub(super) fn count_tokens(text: &str) -> Option<usize> {
        let root = model_root()?;
        tokenizers(&root).ok()?.count(text)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        fn fixture() -> Tokenizer {
            let model = tokenizers::models::wordlevel::WordLevel::builder()
                .vocab(
                    [("[UNK]".to_owned(), 0), ("word".to_owned(), 1)]
                        .into_iter()
                        .collect(),
                )
                .unk_token("[UNK]".into())
                .build()
                .unwrap();
            let mut tokenizer = Tokenizer::new(model);
            tokenizer.with_pre_tokenizer(Some(
                tokenizers::pre_tokenizers::whitespace::Whitespace::default(),
            ));
            tokenizer
                .with_truncation(Some(TruncationParams {
                    max_length: 128,
                    ..Default::default()
                }))
                .unwrap();
            tokenizer
        }

        #[test]
        fn counting_is_unbounded_while_inference_keeps_its_512_token_limit() {
            let tokens = Tokenizers::new(fixture(), fixture()).unwrap();
            let text = "word ".repeat(1202);
            assert_eq!(tokens.count(&text), Some(1202));
            assert!(tokens.count(&text).unwrap() > 700);
            assert_eq!(
                tokens.embedding.encode(text.as_str(), true).unwrap().len(),
                512
            );
            assert_eq!(
                tokens
                    .reranker
                    .encode(("word ", text.as_str()), true)
                    .unwrap()
                    .len(),
                512
            );
            assert_eq!(tokens.count("word word"), Some(2));
        }
    }

    fn compact_query(tokenizer: &Tokenizer, query: &str) -> Result<String, String> {
        let encoding = tokenizer.encode(query, false).map_err(|e| e.to_string())?;
        if encoding.len() <= 96 {
            return Ok(query.to_owned());
        }
        let ids = encoding.get_ids();
        let tail = query
            .chars()
            .rev()
            .take(96)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        let tail = tokenizer.encode(tail, false).map_err(|e| e.to_string())?;
        let tail_ids = tail.get_ids();
        let mut compact = ids[..64].to_vec();
        compact.extend_from_slice(&tail_ids[tail_ids.len().saturating_sub(32)..]);
        tokenizer.decode(&compact, true).map_err(|e| e.to_string())
    }

    pub(super) async fn run(
        texts: &[&str],
        query: Option<&str>,
        foreground: bool,
        permit: Option<super::super::embedding::MemoryRequestPermit>,
    ) -> Result<Vec<Vec<f32>>, String> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        idle_watchdog();
        let root = model_root().ok_or("本地语义模型尚未准备")?;
        if !model_files_ready(&root, query.is_some()) {
            return Err("本地语义模型尚未准备，当前可使用关键词检索".into());
        }
        let queued = QUEUED.fetch_add(1, Ordering::AcqRel);
        if foreground {
            FOREGROUND.fetch_add(1, Ordering::AcqRel);
        }
        let _queued = QueueGuard(foreground);
        if queued >= 8 {
            return Err("本地检索正在处理其他请求".into());
        }
        let texts = texts
            .iter()
            .map(|text| (*text).to_owned())
            .collect::<Vec<_>>();
        let query = query.map(str::to_owned);
        let mut all = Vec::with_capacity(texts.len());
        for batch in texts.chunks(if foreground { 4 } else { 2 }) {
            if !foreground {
                while FOREGROUND.load(Ordering::Acquire) > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
            let owned = PERMITS
                .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| "local inference stopped")?;
            environment()?;
            let options = Arc::new(RunOptions::new().map_err(|e| e.to_string())?);
            let _cancel = CancelRun(options.clone());
            let batch = batch.to_vec();
            let root = root.clone();
            let query = query.clone();
            let permit = permit.clone();
            let values = tokio::task::spawn_blocking(move || {
                let _owned = owned;
                if let Some(permit) = &permit {
                    permit(&batch.iter().map(String::as_str).collect::<Vec<_>>())
                        .map_err(|e| e.to_string())?;
                }
                let tokenizers = tokenizers(&root)?;
                let embedding_tokens = tokenizers.embedding;
                let rank_tokens = tokenizers.reranker;
                let tokens = if query.is_some() {
                    rank_tokens.clone()
                } else {
                    embedding_tokens.clone()
                };
                let query = query.map(|q| compact_query(&rank_tokens, &q)).transpose()?;
                let encodings = batch
                    .iter()
                    .map(|text| {
                        if let Some(query) = &query {
                            tokens.encode((query.as_str(), text.as_str()), true)
                        } else {
                            let text = if foreground {
                                format!(
                                    "{QUERY_INSTRUCTION}{}",
                                    compact_query(&embedding_tokens, text).unwrap_or_default()
                                )
                            } else {
                                text.clone()
                            };
                            tokens.encode(text, true)
                        }
                        .map_err(|e| e.to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let width = encodings.iter().map(|e| e.len()).max().unwrap_or(1);
                let pad = if query.is_some() { 1 } else { 0 };
                let mut ids = vec![pad; batch.len() * width];
                let mut masks = vec![0i64; ids.len()];
                let mut types = vec![0i64; ids.len()];
                for (row, encoding) in encodings.iter().enumerate() {
                    for column in 0..encoding.len() {
                        ids[row * width + column] = encoding.get_ids()[column] as i64;
                        masks[row * width + column] = encoding.get_attention_mask()[column] as i64;
                        types[row * width + column] = encoding.get_type_ids()[column] as i64;
                    }
                }
                let mut engine = ENGINE
                    .lock()
                    .map_err(|_| "local inference lock unavailable")?;
                if engine.as_ref().is_none_or(|e| e.root != root) {
                    *engine = Some(Engine {
                        root: root.clone(),
                        embedding: None,
                        reranker: None,
                        last_used: std::time::Instant::now(),
                    });
                }
                let engine = engine.as_mut().unwrap();
                engine.last_used = std::time::Instant::now();
                let target = if query.is_some() {
                    &mut engine.reranker
                } else {
                    &mut engine.embedding
                };
                if target.is_none() {
                    let threads = std::thread::available_parallelism()
                        .map_or(2, |n| n.get().saturating_sub(1).clamp(1, 4));
                    *target = Some(
                        {
                            environment()?;
                            Session::builder()
                        }
                        .map_err(|e| e.to_string())?
                        .with_intra_threads(threads)
                        .map_err(|e| e.to_string())?
                        .with_inter_threads(1)
                        .map_err(|e| e.to_string())?
                        .with_memory_pattern(false)
                        .map_err(|e| e.to_string())?
                        .commit_from_file(root.join(if query.is_some() {
                            "reranker/model.onnx"
                        } else {
                            "embedding/model.onnx"
                        }))
                        .map_err(|e| e.to_string())?,
                    );
                }
                let session = target.as_mut().unwrap();
                let tensor = |values: Vec<i64>| {
                    Tensor::from_array(([batch.len(), width], values))
                        .map(|value| value.into_dyn())
                        .map_err(|e| e.to_string())
                };
                let mut inputs: Vec<(String, DynValue)> = vec![
                    ("input_ids".into(), tensor(ids)?),
                    ("attention_mask".into(), tensor(masks)?),
                ];
                if session
                    .inputs
                    .iter()
                    .any(|input| input.name == "token_type_ids")
                {
                    inputs.push(("token_type_ids".into(), tensor(types)?));
                }
                let output = session
                    .run_with_options(inputs, &options)
                    .map_err(|e| e.to_string())?;
                let (shape, data) = output[0]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| e.to_string())?;
                let mut result = Vec::with_capacity(batch.len());
                for row in 0..batch.len() {
                    if query.is_some() {
                        result.push(vec![*data.get(row).ok_or("invalid reranker output shape")?]);
                    } else {
                        let stride = if shape.len() == 3 {
                            shape[1] as usize * DIMENSIONS
                        } else {
                            DIMENSIONS
                        };
                        let mut vector = data
                            .get(row * stride..row * stride + DIMENSIONS)
                            .ok_or("invalid embedding output shape")?
                            .to_vec();
                        let norm = vector
                            .iter()
                            .map(|v| (*v as f64).powi(2))
                            .sum::<f64>()
                            .sqrt();
                        if !norm.is_finite() || norm < 1e-12 {
                            return Err("invalid embedding norm".into());
                        }
                        for value in &mut vector {
                            *value = (*value as f64 / norm) as f32;
                        }
                        result.push(vector);
                    }
                }
                if result.iter().flatten().any(|v| !v.is_finite()) {
                    return Err("invalid local inference output".into());
                }
                Ok::<_, String>(result)
            })
            .await
            .map_err(|e| e.to_string())??;
            all.extend(values);
        }
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Run against the exact production weights, never a mocked embedding server.
    #[cfg(feature = "local-inference")]
    #[tokio::test]
    #[ignore = "requires ECHO_RETRIEVAL_MODELS pointing to verified offline assets"]
    async fn real_chinese_models_retrieve_rerank_and_reject_unrelated_evidence() {
        use crate::embedding::EmbeddingProvider;
        let directory =
            std::env::var_os("ECHO_RETRIEVAL_MODELS").expect("verified models directory");
        configure(PathBuf::from(directory));
        let provider = LocalEmbeddingProvider::new().expect("verified embedding files");
        let documents = [
            "员工出差前须在办公系统提交差旅申请，经直属主管审批后，财务方可报销交通和住宿费用。",
            "生产数据库连接失败时，先检查网络和防火墙，再核对数据库地址、端口和账号权限。",
            "年假申请应提前三个工作日提交，由部门负责人审批。",
            "离职人员须交还电脑、门禁卡，办理账号停用和工作交接。",
            "北京地区出差住宿报销上限为每晚五百元，超标部分须另行审批。",
            "公司每周五下午组织羽毛球活动，自愿参加。",
        ];
        let started = std::time::Instant::now();
        let embeddings = provider.embed_batch(&documents).await.unwrap();
        let document_ms = started.elapsed().as_millis();
        assert!(embeddings.iter().all(|vector| vector.len() == DIMENSIONS
            && (vector.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 0.001));
        let cases = [
            ("出差花的钱怎样才能报销？", 0usize),
            ("数据库连不上应该排查哪些地方？", 1),
            ("想休年假需要提前多久申请？", 2),
            ("离职时需要归还什么东西？", 3),
            ("离开公司时要归还哪些物品？", 3),
            ("去北京住酒店一晚上最多报多少钱？", 4),
        ];
        for (query, expected) in cases {
            let started = std::time::Instant::now();
            let vector = provider.embed_query(query).await.unwrap();
            let query_ms = started.elapsed().as_millis();
            let mut order = (0..documents.len()).collect::<Vec<_>>();
            order.sort_by(|a, b| {
                let score = |i: usize| {
                    vector
                        .iter()
                        .zip(&embeddings[i])
                        .map(|(x, y)| (*x as f64) * (*y as f64))
                        .sum::<f64>()
                };
                score(*b).total_cmp(&score(*a))
            });
            if query == "离开公司时要归还哪些物品？" {
                assert_eq!(
                    crate::search::lexical_query_coverage(query, documents[expected]),
                    0.0,
                    "fixture must require semantic matching"
                );
            }
            assert!(
                order[..3].contains(&expected),
                "embedding miss: {query}, {order:?}"
            );
            let started = std::time::Instant::now();
            let scores = rerank(query, &documents, None).await.unwrap();
            let rank_ms = started.elapsed().as_millis();
            let best = (0..scores.len())
                .max_by(|a, b| scores[*a].total_cmp(&scores[*b]))
                .unwrap();
            eprintln!(
                "real retrieval: query={query} embedTop1={} rerankTop1={best} scores={scores:?} queryMs={query_ms} rerank6Ms={rank_ms} docs6Ms={document_ms}",
                order[0]
            );
            assert_eq!(best, expected, "reranker miss: {query}");
            assert!(
                scores[expected] >= MIN_RELEVANCE,
                "relevant evidence rejected: {query}"
            );
        }
        // Long production-sized passages exercise dynamic padding and peak latency.
        let long_docs = (0..15)
            .map(|i| {
                format!(
                    "文档 {i}。{}{}",
                    documents[i % documents.len()],
                    "员工应按公司制度提交申请并保留相关凭据。".repeat(12)
                )
            })
            .collect::<Vec<_>>();
        let started = std::time::Instant::now();
        let long_scores = rerank(
            "北京出差住宿报销的上限是多少？",
            &long_docs.iter().map(String::as_str).collect::<Vec<_>>(),
            None,
        )
        .await
        .unwrap();
        eprintln!(
            "long rerank: candidates={} maxTokens={} elapsedMs={}",
            long_scores.len(),
            long_docs
                .iter()
                .map(|text| token_count(text))
                .max()
                .unwrap(),
            started.elapsed().as_millis()
        );
        assert_eq!(long_scores.len(), 15);
        let cancellation = tokio::time::timeout(
            std::time::Duration::from_millis(1),
            rerank(
                "取消检索",
                &long_docs.iter().map(String::as_str).collect::<Vec<_>>(),
                None,
            ),
        )
        .await;
        assert!(cancellation.is_err());
        let resumed = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            provider.embed_query("取消后继续检索"),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(resumed.len(), DIMENSIONS);
        let unrelated = rerank("火星卫星的轨道周期是多少？", &documents, None)
            .await
            .unwrap();
        eprintln!("unrelated scores: {unrelated:?}");
        assert!(
            unrelated.iter().all(|score| *score < MIN_RELEVANCE),
            "unrelated evidence accepted"
        );
    }

    #[test]
    fn sigmoid_preserves_order_without_clamping_logits() {
        assert!(sigmoid(-2.0) < sigmoid(0.0) && sigmoid(0.0) < sigmoid(2.0));
        assert_eq!(sigmoid(0.0), 0.5);
        assert!(sigmoid(-1000.0).is_finite() && sigmoid(1000.0).is_finite());
    }
}
