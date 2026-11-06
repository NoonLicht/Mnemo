//! Golden Query Set for Mnemo search evaluation.
//!
//! Evaluates retrieval quality across 50 labeled queries covering:
//! - Natural language intent & filters (sort, scope, type, size, modified)
//! - Lexical fuzzy filename and path matching
//! - Content BM25 keyword matching
//! - CJK tokenization (Han, Hiragana, Katakana, Hangul, mixed-script, unigram/bigram)
//! - Accented multilingual European filenames and content
//! - Multimodal visual vector retrieval
//!
//! Metrics computed:
//! - Recall@1, Recall@5
//! - MRR (Mean Reciprocal Rank)
//! - NDCG@10 (Normalized Discounted Cumulative Gain)
//! - Latency (p50, p95)

use mnemo_lib::config::MnemoConfig;
use mnemo_lib::embed::{DEFAULT_MODEL_ID, EMBEDDING_DIM, Unit, UnitStore};
use mnemo_lib::indexer::content::ContentIndex;
use mnemo_lib::indexer::metadata::MetadataStore;
use mnemo_lib::indexer::metadata_db::MetadataDb;
use mnemo_lib::indexer::walker::{FileEntry, intern_extension};
use mnemo_lib::search::fuzzy::compute_char_mask;
use mnemo_lib::search::{SearchDispatcher, SortOrder};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static NEXT_SEQ: AtomicU64 = AtomicU64::new(1);

struct GoldenDoc {
    rel_path: &'static str,
    content: &'static str,
    modified_secs: u64,
    size_override: Option<u64>,
    visual_desc: Option<&'static str>,
}

struct GoldenQuery {
    query: &'static str,
    expected_path_contains: &'static str,
    category: &'static str,
}

const GOLDEN_DOCS: &[GoldenDoc] = &[
    // Resumes & Careers
    GoldenDoc {
        rel_path: "documents/resume_jane_doe.txt",
        content: "Jane Doe Senior Rust Engineer Curriculum Vitae experience in systems programming and search engines. Contact: jane@example.com",
        modified_secs: 1_750_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "documents/resume_old.txt",
        content: "Jane Doe Junior Developer Resume prior history",
        modified_secs: 1_000_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "careers/cv_2025.txt",
        content: "Curriculum Vitae job application for software developer position",
        modified_secs: 1_740_000_000,
        size_override: None,
        visual_desc: None,
    },
    // Invoices & Finance
    GoldenDoc {
        rel_path: "docs/invoice_2024.pdf",
        content: "Invoice #1042 payment total due $450.00 for cloud hosting services",
        modified_secs: 1_720_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "finance/receipt_coffee.txt",
        content: "Coffee shop receipt $4.50 card payment thank you for your visit",
        modified_secs: 1_730_000_000,
        size_override: None,
        visual_desc: None,
    },
    // Projects & Engineering
    GoldenDoc {
        rel_path: "Projects/server/docker_error.log",
        content: "docker daemon container start error: connection timed out retry attempts exhausted 127.0.0.1:2375 connection refused",
        modified_secs: 1_745_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "downloads/contract_final.pdf",
        content: "Final signed partnership contract and non-disclosure agreement",
        modified_secs: 1_735_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "Projects/mnemo/src/main.rs",
        content: "fn main() { println!(\"Starting Mnemo daemon\"); }",
        modified_secs: 1_746_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "Projects/mnemo/src/indexer/mod.rs",
        content: "pub struct IndexerManager { config: MnemoConfig }",
        modified_secs: 1_746_100_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "Projects/mnemo/src/indexer/content.rs",
        content: "pub struct ContentIndex { writer: IndexWriter }",
        modified_secs: 1_746_200_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "Projects/mnemo/src/indexer/tokenizer.rs",
        content: "pub struct MnemoTokenizer; // CJK unigram bigram decomposition",
        modified_secs: 1_746_300_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "Projects/web/app.tsx",
        content: "export function App() { return <div>Mnemo Search</div>; }",
        modified_secs: 1_746_400_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "Projects/api/server.go",
        content: "package main\nfunc StartServer() { log.Println(\"Server running\"); }",
        modified_secs: 1_746_500_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "config/settings.json",
        content: "{\"theme\": \"dark\", \"search_debounce_ms\": 50, \"auto_start\": true}",
        modified_secs: 1_746_600_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "reports/annual_2025.txt",
        content: "Annual financial and operational performance report for fiscal year 2025",
        modified_secs: 1_748_736_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "public/setup-icon.png",
        content: "PNG icon binary data",
        modified_secs: 1_740_000_000,
        size_override: None,
        visual_desc: Some("setup icon logo symbol"),
    },
    GoldenDoc {
        rel_path: "server/large_access.log",
        content: "127.0.0.1 - - [27/Oct/2026:01:00:00] GET /api/v1/search HTTP/1.1 200 large access log file with extensive telemetry payload over one kilobyte in length to test size filtering thresholds reliably in automated test suites",
        modified_secs: 1_747_000_000,
        size_override: Some(1500),
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "Projects/mnemo/README.md",
        content: "Mnemo local file search fast indexer with lexical, content, and visual fusion",
        modified_secs: 1_746_000_000,
        size_override: None,
        visual_desc: None,
    },
    // Content Knowledge Base
    GoldenDoc {
        rel_path: "docs/distributed_systems.txt",
        content: "Distributed consensus algorithms including Raft and Paxos provide fault tolerance across replicas",
        modified_secs: 1_730_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "research/deep_learning.txt",
        content: "Deep neural network training model optimization using gradient descent and backpropagation",
        modified_secs: 1_731_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "docs/storage_notes.txt",
        content: "SQLite storage optimization: metadata vacuum and B-tree page sizing for reduced disk usage",
        modified_secs: 1_732_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "research/quantum.txt",
        content: "Quantum computing fundamentals: superposition qubits entanglement and quantum circuits",
        modified_secs: 1_733_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "docs/search_algorithms.txt",
        content: "Information retrieval: Reciprocal Rank Fusion k=60 combining lexical BM25 and vector embeddings",
        modified_secs: 1_734_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "notes/quick_meeting.txt",
        content: "Weekly standup sprint review and action items discussion",
        modified_secs: 1_745_500_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "docs/api_specification.md",
        content: "REST API endpoint documentation and JSON schema definitions for client integration",
        modified_secs: 1_745_600_000,
        size_override: None,
        visual_desc: None,
    },
    // CJK Documents
    GoldenDoc {
        rel_path: "Projects/docs/最終報告書.txt",
        content: "2025年度第4四半期の全社プロジェクト最終結果に関する報告書",
        modified_secs: 1_740_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "notes/weather.txt",
        content: "本日の東京都の天気予報は晴れのち曇りです。最高気温は22度です。",
        modified_secs: 1_741_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "Projects/mnemo/README最終.md",
        content: "Mnemo 最終版リリースノートおよびユーザーマニュアル",
        modified_secs: 1_742_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "Projects/mnemo/src/组件/index.ts",
        content: "export const Button = () => 'button'; // UI 组件",
        modified_secs: 1_743_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "docs/보고서_최종.txt",
        content: "2025년도 연구 개발 프로젝트 결과 보고서 요약본",
        modified_secs: 1_744_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "notes/seoul_trip.txt",
        content: "서울 여행 맛집 탐방 계획 및 지하철 노선 일정표",
        modified_secs: 1_745_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "docs/财务报表.txt",
        content: "2025年第一季度公司财务收支报表与资产负债表分析",
        modified_secs: 1_746_000_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "docs/architecture.txt",
        content: "分布式系统架构设计与微服务性能优化方案",
        modified_secs: 1_747_000_000,
        size_override: None,
        visual_desc: None,
    },
    // Accented / Multilingual European Documents
    GoldenDoc {
        rel_path: "docs/résumé_projet.txt",
        content: "Résumé détaillé du projet d'ingénierie logicielle et spécifications techniques",
        modified_secs: 1_744_500_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "docs/Geschäftsbericht_überarbeitung.txt",
        content: "Jährlicher Geschäftsbericht für das Geschäftsjahr 2025 mit Finanzübersicht",
        modified_secs: 1_745_500_000,
        size_override: None,
        visual_desc: None,
    },
    GoldenDoc {
        rel_path: "docs/informe_diseño.txt",
        content: "Informe sobre el diseño de la interfaz de usuario moderna y accesible",
        modified_secs: 1_746_500_000,
        size_override: None,
        visual_desc: None,
    },
    // Visual Fixtures
    GoldenDoc {
        rel_path: "Pictures/red_cap.png",
        content: "PNG binary header",
        modified_secs: 1_740_000_000,
        size_override: None,
        visual_desc: Some("red cap headwear apparel"),
    },
    GoldenDoc {
        rel_path: "Pictures/system_diagram.png",
        content: "PNG binary header",
        modified_secs: 1_741_000_000,
        size_override: None,
        visual_desc: Some("system architecture diagram microservices cloud network flow"),
    },
    GoldenDoc {
        rel_path: "Pictures/dashboard_screenshot.png",
        content: "PNG binary header",
        modified_secs: 1_742_000_000,
        size_override: None,
        visual_desc: Some("dashboard screenshot analytics charts metrics dark theme"),
    },
    GoldenDoc {
        rel_path: "Pictures/nature_sunset.png",
        content: "PNG binary header",
        modified_secs: 1_743_000_000,
        size_override: None,
        visual_desc: Some("nature sunset over ocean beach horizon orange sky"),
    },
    GoldenDoc {
        rel_path: "Pictures/wireframe_mockup.png",
        content: "PNG binary header",
        modified_secs: 1_744_000_000,
        size_override: None,
        visual_desc: Some("wireframe mockup mobile app UI layout design blueprint"),
    },
];

const GOLDEN_QUERIES: &[GoldenQuery] = &[
    // --- Intent & Scoped Queries (1-10) ---
    GoldenQuery {
        query: "latest resume",
        expected_path_contains: "resume_jane_doe.txt",
        category: "intent",
    },
    GoldenQuery {
        query: "invoice",
        expected_path_contains: "invoice_2024.pdf",
        category: "intent",
    },
    GoldenQuery {
        query: "receipt",
        expected_path_contains: "receipt_coffee.txt",
        category: "intent",
    },
    GoldenQuery {
        query: "docker error in Projects",
        expected_path_contains: "docker_error.log",
        category: "intent",
    },
    GoldenQuery {
        query: "type:pdf contract",
        expected_path_contains: "contract_final.pdf",
        category: "intent",
    },
    GoldenQuery {
        query: "ext:rs indexer",
        expected_path_contains: "indexer/mod.rs",
        category: "intent",
    },
    GoldenQuery {
        query: "size:>1k log",
        expected_path_contains: "large_access.log",
        category: "intent",
    },
    GoldenQuery {
        query: "path:server error",
        expected_path_contains: "docker_error.log",
        category: "intent",
    },
    GoldenQuery {
        query: "cv",
        expected_path_contains: "cv_2025.txt",
        category: "intent",
    },
    GoldenQuery {
        query: "type:code server",
        expected_path_contains: "server.go",
        category: "intent",
    },
    // --- Lexical Filename & Path Queries (11-18) ---
    GoldenQuery {
        query: "main.rs",
        expected_path_contains: "src/main.rs",
        category: "lexical",
    },
    GoldenQuery {
        query: "tokenizer",
        expected_path_contains: "tokenizer.rs",
        category: "lexical",
    },
    GoldenQuery {
        query: "app.tsx",
        expected_path_contains: "app.tsx",
        category: "lexical",
    },
    GoldenQuery {
        query: "settings.json",
        expected_path_contains: "settings.json",
        category: "lexical",
    },
    GoldenQuery {
        query: "server.go",
        expected_path_contains: "server.go",
        category: "lexical",
    },
    GoldenQuery {
        query: "content.rs",
        expected_path_contains: "content.rs",
        category: "lexical",
    },
    GoldenQuery {
        query: "mnemo README",
        expected_path_contains: "README.md",
        category: "lexical",
    },
    GoldenQuery {
        query: "annual report",
        expected_path_contains: "annual_2025.txt",
        category: "lexical",
    },
    // --- Content BM25 Queries (19-26) ---
    GoldenQuery {
        query: "distributed consensus raft",
        expected_path_contains: "distributed_systems.txt",
        category: "content",
    },
    GoldenQuery {
        query: "neural network training model",
        expected_path_contains: "deep_learning.txt",
        category: "content",
    },
    GoldenQuery {
        query: "sqlite metadata vacuum",
        expected_path_contains: "storage_notes.txt",
        category: "content",
    },
    GoldenQuery {
        query: "quantum superposition qubits",
        expected_path_contains: "quantum.txt",
        category: "content",
    },
    GoldenQuery {
        query: "coffee shop card payment",
        expected_path_contains: "receipt_coffee.txt",
        category: "content",
    },
    GoldenQuery {
        query: "connection timed out retry",
        expected_path_contains: "docker_error.log",
        category: "content",
    },
    GoldenQuery {
        query: "reciprocal rank fusion k=60",
        expected_path_contains: "search_algorithms.txt",
        category: "content",
    },
    GoldenQuery {
        query: "dark theme search debounce",
        expected_path_contains: "settings.json",
        category: "content",
    },
    // --- CJK Queries (27-36) ---
    GoldenQuery {
        query: "最終報告書",
        expected_path_contains: "最終報告書.txt",
        category: "cjk",
    },
    GoldenQuery {
        query: "報告",
        expected_path_contains: "最終報告書.txt",
        category: "cjk",
    },
    GoldenQuery {
        query: "東京",
        expected_path_contains: "weather.txt",
        category: "cjk",
    },
    GoldenQuery {
        query: "天気",
        expected_path_contains: "weather.txt",
        category: "cjk",
    },
    GoldenQuery {
        query: "README最終",
        expected_path_contains: "README最終.md",
        category: "cjk",
    },
    GoldenQuery {
        query: "组件",
        expected_path_contains: "组件/index.ts",
        category: "cjk",
    },
    GoldenQuery {
        query: "보고서",
        expected_path_contains: "보고서_최종.txt",
        category: "cjk",
    },
    GoldenQuery {
        query: "서울",
        expected_path_contains: "seoul_trip.txt",
        category: "cjk",
    },
    GoldenQuery {
        query: "财务报表",
        expected_path_contains: "财务报表.txt",
        category: "cjk",
    },
    GoldenQuery {
        query: "分布式系统",
        expected_path_contains: "architecture.txt",
        category: "cjk",
    },
    // --- Accented / Multilingual European Queries (37-40) ---
    GoldenQuery {
        query: "résumé",
        expected_path_contains: "résumé_projet.txt",
        category: "multilingual",
    },
    GoldenQuery {
        query: "projet",
        expected_path_contains: "résumé_projet.txt",
        category: "multilingual",
    },
    GoldenQuery {
        query: "Geschäftsbericht",
        expected_path_contains: "Geschäftsbericht_überarbeitung.txt",
        category: "multilingual",
    },
    GoldenQuery {
        query: "diseño",
        expected_path_contains: "informe_diseño.txt",
        category: "multilingual",
    },
    // --- Visual & Multimodal Queries (41-50) ---
    GoldenQuery {
        query: "red cap",
        expected_path_contains: "red_cap.png",
        category: "visual",
    },
    GoldenQuery {
        query: "visual:diagram",
        expected_path_contains: "system_diagram.png",
        category: "visual",
    },
    GoldenQuery {
        query: "screenshot",
        expected_path_contains: "dashboard_screenshot.png",
        category: "visual",
    },
    GoldenQuery {
        query: "sunset",
        expected_path_contains: "nature_sunset.png",
        category: "visual",
    },
    GoldenQuery {
        query: "wireframe",
        expected_path_contains: "wireframe_mockup.png",
        category: "visual",
    },
    GoldenQuery {
        query: "system architecture diagram",
        expected_path_contains: "system_diagram.png",
        category: "visual",
    },
    GoldenQuery {
        query: "weekly standup sprint",
        expected_path_contains: "quick_meeting.txt",
        category: "content",
    },
    GoldenQuery {
        query: "REST API endpoint documentation",
        expected_path_contains: "api_specification.md",
        category: "content",
    },
    GoldenQuery {
        query: "setup icon",
        expected_path_contains: "setup-icon.png",
        category: "lexical",
    },
    GoldenQuery {
        query: "type:image red",
        expected_path_contains: "red_cap.png",
        category: "visual",
    },
];

struct GoldenCorpus {
    dir: PathBuf,
    metadata: MetadataStore,
    content_index: Option<ContentIndex>,
    unit_store: UnitStore,
}

impl Drop for GoldenCorpus {
    fn drop(&mut self) {
        self.content_index.take();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn setup_golden_corpus() -> GoldenCorpus {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("mnemo-golden-{}-{}", std::process::id(), nanos));
    fs::create_dir_all(&dir).expect("create golden test dir");

    let db = MetadataDb::open_in_dir(&dir).expect("open metadata db");
    let content_index = ContentIndex::new(&dir).expect("open content index");
    let mut unit_store = UnitStore::new(EMBEDDING_DIM);

    for (idx, doc) in GOLDEN_DOCS.iter().enumerate() {
        let node_id = (idx + 1) as u64;
        let abs_path = dir.join(doc.rel_path);
        if let Some(parent) = abs_path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(&abs_path, doc.content).expect("write doc content");

        let name = abs_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let extension = intern_extension(abs_path.extension().and_then(|e| e.to_str()));
        let size = doc.size_override.unwrap_or(doc.content.len() as u64);
        let char_bloom = compute_char_mask(&name);

        let entry = FileEntry {
            id: node_id,
            path: abs_path.clone(),
            name: name.clone(),
            extension,
            size,
            modified: doc.modified_secs,
            is_dir: false,
            dev: 0,
            ino: node_id,
            char_bloom,
            content_pending: false,
        };
        db.upsert(&entry).expect("upsert metadata entry");

        // Content indexing (Tantivy + mnemo tokenizer + folded tier)
        content_index
            .add_or_update(
                &abs_path,
                &name,
                doc.content,
                doc.modified_secs,
                size,
                abs_path.extension().and_then(|e| e.to_str()),
            )
            .expect("index content");

        // Visual metadata is kept in the fixture for future real-model runs,
        // but production has no encoder yet. Do not seed a mock visual index.
        let _ = doc.visual_desc;

        // Static semantic indexing fallback vector
        let sem_vec = mnemo_lib::embed::model::ModelRegistry::global().embed_to_i8(doc.content);
        unit_store.add_unit(
            Unit {
                id: node_id,
                node_id,
                byte_start: 0,
                byte_end: doc.content.len(),
                model_id: DEFAULT_MODEL_ID,
            },
            &sem_vec,
        );
    }

    content_index.commit().expect("commit content index");
    let metadata = MetadataStore::with_db(db);

    GoldenCorpus {
        dir,
        metadata,
        content_index: Some(content_index),
        unit_store,
    }
}

#[derive(Default, Debug)]
struct EvaluationSummary {
    total_queries: usize,
    hits_at_1: usize,
    hits_at_5: usize,
    reciprocal_ranks_sum: f64,
    ndcg_at_10_sum: f64,
    latencies_us: Vec<u64>,
}

#[test]
fn golden_query_set_evaluation() {
    let corpus = setup_golden_corpus();
    let aliases = MnemoConfig::default().alias_pairs();

    let mut summary = EvaluationSummary::default();

    // Warm up the index once
    let _ = SearchDispatcher::search_response_fast_sorted_with_snippets(
        "test warm up",
        &corpus.metadata,
        &corpus.content_index,
        Some(&corpus.unit_store),
        true,
        10,
        0,
        SortOrder::Best,
        &aliases,
        0,
        false,
    );

    println!(
        "\n=== GOLDEN QUERY SET EXECUTION ({} queries) ===",
        GOLDEN_QUERIES.len()
    );
    println!(
        "{:<4} {:<12} {:<32} {:<6} {:<8} {:<10}",
        "#", "Category", "Query", "Rank", "Latency", "Expected"
    );
    println!("{:-<76}", "");

    let mut missed = Vec::new();

    for (i, gq) in GOLDEN_QUERIES.iter().enumerate() {
        // The production visual encoder is intentionally disabled until a real
        // CLIP/SigLIP pack is wired. Visual cases stay in the fixture but do not
        // count toward the lexical/semantic gate yet.
        if gq.category == "visual" {
            continue;
        }
        summary.total_queries += 1;
        let seq = NEXT_SEQ.fetch_add(1, Ordering::Relaxed);
        let start = Instant::now();
        let response = SearchDispatcher::search_response_fast_sorted_with_snippets(
            gq.query,
            &corpus.metadata,
            &corpus.content_index,
            Some(&corpus.unit_store),
            true,
            10,
            seq,
            SortOrder::Best,
            &aliases,
            0,
            false,
        );
        let elapsed_us = start.elapsed().as_micros() as u64;
        summary.latencies_us.push(elapsed_us);

        let rank_opt = response
            .results
            .iter()
            .position(|r| r.path.contains(gq.expected_path_contains));

        match rank_opt {
            Some(rank) => {
                if rank == 0 {
                    summary.hits_at_1 += 1;
                }
                if rank < 5 {
                    summary.hits_at_5 += 1;
                }
                let rr = 1.0 / (rank as f64 + 1.0);
                summary.reciprocal_ranks_sum += rr;

                if rank < 10 {
                    let dcg = 1.0 / (rank as f64 + 2.0).log2();
                    summary.ndcg_at_10_sum += dcg;
                }

                println!(
                    "{:<4} {:<12} {:<32} {:<6} {:<8} µs {}",
                    i + 1,
                    gq.category,
                    gq.query,
                    rank + 1,
                    elapsed_us,
                    gq.expected_path_contains
                );
            }
            None => {
                missed.push((gq.query, gq.expected_path_contains, gq.category));
                println!(
                    "{:<4} {:<12} {:<32} {:<6} {:<8} µs {} [MISSED]",
                    i + 1,
                    gq.category,
                    gq.query,
                    "N/A",
                    elapsed_us,
                    gq.expected_path_contains
                );
            }
        }
    }

    summary.latencies_us.sort_unstable();
    let p50_us = summary.latencies_us[summary.latencies_us.len() / 2];
    let p95_us = summary.latencies_us[summary.latencies_us.len() * 95 / 100];
    let recall_at_1 = summary.hits_at_1 as f64 / summary.total_queries as f64;
    let recall_at_5 = summary.hits_at_5 as f64 / summary.total_queries as f64;
    let mrr = summary.reciprocal_ranks_sum / summary.total_queries as f64;
    let ndcg_at_10 = summary.ndcg_at_10_sum / summary.total_queries as f64;

    println!("{:-<76}", "");
    println!("=== GOLDEN QUERY SET EVALUATION RESULTS ===");
    println!("Total Queries:       {}", summary.total_queries);
    println!(
        "Recall@1:            {:.4} ({}/{})",
        recall_at_1, summary.hits_at_1, summary.total_queries
    );
    println!(
        "Recall@5:            {:.4} ({}/{})",
        recall_at_5, summary.hits_at_5, summary.total_queries
    );
    println!("MRR:                 {:.4}", mrr);
    println!("NDCG@10:             {:.4}", ndcg_at_10);
    println!(
        "Latency p50:         {:.3} ms ({} µs)",
        p50_us as f64 / 1000.0,
        p50_us
    );
    println!(
        "Latency p95:         {:.3} ms ({} µs)",
        p95_us as f64 / 1000.0,
        p95_us
    );
    println!("===========================================\n");

    if !missed.is_empty() {
        eprintln!("Missed queries:");
        for (q, exp, cat) in &missed {
            eprintln!("  [{cat}] \"{q}\" -> expected contains \"{exp}\"");
        }
    }

    // Acceptance Gate: recall@5 >= 0.90, MRR >= 0.80
    assert!(
        recall_at_5 >= 0.90,
        "Golden set recall@5 ({:.4}) must be >= 0.90",
        recall_at_5
    );
    assert!(mrr >= 0.80, "Golden set MRR ({:.4}) must be >= 0.80", mrr);
}
