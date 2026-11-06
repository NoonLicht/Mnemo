//! K12 index-format spike.
//!
//! Usage: `cargo run --release --example index_format_bench -- <tantivy|folded|fts5> <docs> <dir>`
//!
//! Builds a synthetic corpus, indexes it with the selected backend, measures
//! index time/size/RSS and a small query mix, then optionally removes the
//! scratch directory. `KEEP=1` keeps it for inspection.
//!
//! This is a benchmark harness only. It does not touch production indexes and
//! deliberately disables SQLite durability (`journal_mode=OFF`) so index-build
//! time is comparable to Tantivy's uncommitted arena rather than dominated by
//! fsyncs. Query timings use the resulting on-disk indexes.

use mnemo_lib::indexer::content::fold;
use rusqlite::{Connection, params};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::*;
use tantivy::tokenizer::NgramTokenizer;
use tantivy::{Index, IndexReader, Term, doc};

const TOPICS: [&str; 8] = [
    "quarterly revenue",
    "docker build error",
    "invoice payment",
    "meeting notes",
    "release checklist",
    "customer support",
    "kernel panic",
    "design review",
];

fn make_doc(i: usize) -> (String, String, String, &'static str) {
    let ext = match i % 3 {
        0 => "pdf",
        1 => "md",
        _ => "txt",
    };
    let filename = format!("doc{:07}.{}", i, ext);
    let path = format!("/corpus/{:02}/{}", i % 100, filename);
    let topic = TOPICS[i % TOPICS.len()];
    let mut content = String::with_capacity(220);
    content.push_str(topic);
    content.push_str(&format!(" report {} owner team ", i / 1000));
    for _ in 0..3 {
        content.push_str(topic);
        content.push(' ');
    }
    content.push_str("alpha beta gamma delta epsilon zeta eta theta");
    if i.is_multiple_of(1000) {
        content.push_str(" needle");
    }
    if i.is_multiple_of(5000) {
        content.push_str(" donutsmp.net");
    }
    while content.len() < 220 {
        content.push_str(" alpha beta gamma delta");
    }
    (path, filename, content, ext)
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                total += dir_size(&entry_path);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

fn rss_kb() -> u64 {
    fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|statm| {
            statm
                .split_whitespace()
                .nth(1)
                .and_then(|pages| pages.parse::<u64>().ok())
        })
        .map(|pages| pages * 4)
        .unwrap_or(0)
}

fn trigram_query(field: Field, text: &str) -> Box<dyn Query> {
    let folded = fold(text);
    let mut grams = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for term in folded.split(|ch: char| !ch.is_alphanumeric()) {
        let chars: Vec<char> = term.chars().collect();
        for window in chars.windows(3) {
            let gram: String = window.iter().collect();
            if seen.insert(gram.clone()) {
                grams.push((
                    Occur::Should,
                    Box::new(TermQuery::new(
                        Term::from_field_text(field, &gram),
                        IndexRecordOption::WithFreqs,
                    )) as Box<dyn Query>,
                ));
            }
        }
    }
    Box::new(BooleanQuery::new(grams))
}

fn bench<F: FnMut() -> usize>(mut query: F) -> (u128, u128, usize) {
    for _ in 0..10 {
        query();
    }
    let mut samples = Vec::with_capacity(100);
    let mut hits = 0;
    for _ in 0..100 {
        let start = Instant::now();
        hits = query();
        samples.push(start.elapsed().as_nanos());
    }
    samples.sort_unstable();
    (samples[50], samples[95], hits)
}

struct TantivyFields {
    path: Field,
    filename: Field,
    content: Field,
    ext: Field,
    folded: Option<Field>,
}

fn build_tantivy(
    dir: &Path,
    docs: usize,
    folded: bool,
) -> Result<(Index, TantivyFields, u64, u128), String> {
    let start = Instant::now();
    let mut builder = Schema::builder();
    let path = builder.add_text_field("path", STRING | STORED);
    let filename = builder.add_text_field("filename", TEXT | STORED);
    let content = builder.add_text_field("content", TEXT);
    let ext = builder.add_text_field("ext", STRING | STORED);
    let folded_field = if folded {
        let indexing = TextFieldIndexing::default()
            .set_tokenizer("trigram")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        Some(builder.add_text_field(
            "content_folded",
            TextOptions::default().set_indexing_options(indexing),
        ))
    } else {
        None
    };
    let schema = builder.build();
    let index = Index::create_in_dir(dir, schema).map_err(|e| e.to_string())?;
    index.tokenizers().register(
        "trigram",
        NgramTokenizer::new(3, 3, false).map_err(|e| e.to_string())?,
    );
    let writer_threads = std::env::var("MNEMO_TANTIVY_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|threads| *threads > 0)
        .unwrap_or(1);
    let mut writer = index
        .writer_with_num_threads(writer_threads, 16_000_000usize * writer_threads)
        .map_err(|e| e.to_string())?;
    let mut peak_rss = rss_kb();

    for i in 0..docs {
        let (doc_path, doc_filename, content_text, doc_ext) = make_doc(i);
        let mut document = doc!(
            path => doc_path,
            filename => doc_filename,
            content => content_text.clone(),
            ext => doc_ext,
        );
        if let Some(field) = folded_field {
            document.add_text(field, fold(&content_text));
        }
        writer.add_document(document).map_err(|e| e.to_string())?;
        if i.is_multiple_of(50_000) {
            peak_rss = peak_rss.max(rss_kb());
        }
        if (i + 1).is_multiple_of(100_000) {
            writer.commit().map_err(|e| e.to_string())?;
            peak_rss = peak_rss.max(rss_kb());
        }
    }
    writer.commit().map_err(|e| e.to_string())?;
    writer.wait_merging_threads().map_err(|e| e.to_string())?;
    peak_rss = peak_rss.max(rss_kb());
    let elapsed = start.elapsed().as_millis();
    Ok((
        index,
        TantivyFields {
            path,
            filename,
            content,
            ext,
            folded: folded_field,
        },
        peak_rss,
        elapsed,
    ))
}

fn build_fts5(dir: &Path, docs: usize) -> Result<(Connection, u64, u128), String> {
    let start = Instant::now();
    let db_path = dir.join("fts5.sqlite");
    let conn = Connection::open(&db_path).map_err(|e| e.to_string())?;
    conn.execute_batch(
        "PRAGMA journal_mode=OFF;
         PRAGMA synchronous=OFF;
         PRAGMA temp_store=MEMORY;
         PRAGMA cache_size=-131072;
         CREATE VIRTUAL TABLE docs USING fts5(
             path UNINDEXED,
             filename,
             content,
             ext UNINDEXED,
             tokenize='trigram'
         );",
    )
    .map_err(|e| e.to_string())?;
    conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
    let mut peak_rss = rss_kb();
    for i in 0..docs {
        let (doc_path, doc_filename, content_text, doc_ext) = make_doc(i);
        conn.execute(
            "INSERT INTO docs(path, filename, content, ext) VALUES (?1, ?2, ?3, ?4)",
            params![doc_path, doc_filename, content_text, doc_ext],
        )
        .map_err(|e| e.to_string())?;
        if i.is_multiple_of(50_000) {
            peak_rss = peak_rss.max(rss_kb());
        }
        if (i + 1).is_multiple_of(100_000) {
            conn.execute_batch("COMMIT; BEGIN")
                .map_err(|e| e.to_string())?;
            peak_rss = peak_rss.max(rss_kb());
        }
    }
    conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
    peak_rss = peak_rss.max(rss_kb());
    Ok((conn, peak_rss, start.elapsed().as_millis()))
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let format = args.get(1).map(String::as_str).unwrap_or("tantivy");
    let docs = args
        .get(2)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10_000);
    let dir = PathBuf::from(
        args.get(3)
            .cloned()
            .unwrap_or_else(|| format!("/tmp/mnemo-k12-{}-{}", format, std::process::id())),
    );
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("remove stale benchmark directory");
    }
    fs::create_dir_all(&dir).expect("create benchmark directory");

    println!("format={} docs={} dir={}", format, docs, dir.display());
    let result = match format {
        "tantivy" => run_tantivy(&dir, docs, false),
        "folded" => run_tantivy(&dir, docs, true),
        "fts5" => run_fts5(&dir, docs),
        other => {
            eprintln!("unknown format: {other} (expected tantivy, folded, fts5)");
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        eprintln!("benchmark failed: {error}");
        std::process::exit(1);
    }
    if std::env::var_os("KEEP").is_none() {
        let _ = fs::remove_dir_all(&dir);
    }
}

fn run_tantivy(dir: &Path, docs: usize, folded: bool) -> Result<(), String> {
    let format = if folded { "folded" } else { "tantivy" };
    let (index, fields, build_peak_rss, index_ms) = build_tantivy(dir, docs, folded)?;
    let index_bytes = dir_size(dir);
    let reader: IndexReader = index.reader().map_err(|e| e.to_string())?;
    let searcher = reader.searcher();
    let parser_name = QueryParser::for_index(&index, vec![fields.filename]);
    let parser_content = QueryParser::for_index(&index, vec![fields.content]);
    let q_name = parser_name
        .parse_query("doc0000001")
        .map_err(|e| e.to_string())?;
    let q_substring = if let Some(field) = fields.folded {
        trigram_query(field, "eedl")
    } else {
        parser_content
            .parse_query("eedl")
            .map_err(|e| e.to_string())?
    };
    let q_bm25 = parser_content
        .parse_query("quarterly revenue")
        .map_err(|e| e.to_string())?;
    let ext_term = Term::from_field_text(fields.ext, "pdf");
    let q_filter: Box<dyn Query> = Box::new(BooleanQuery::new(vec![
        (Occur::Must, q_bm25.box_clone()),
        (
            Occur::Must,
            Box::new(TermQuery::new(ext_term, IndexRecordOption::Basic)),
        ),
    ]));
    let q_folded = if let Some(field) = fields.folded {
        trigram_query(field, "nutsmp")
    } else {
        parser_content
            .parse_query("nutsmp")
            .map_err(|e| e.to_string())?
    };
    let _ = fields.path;

    println!(
        "summary format={} docs={} index_ms={} index_bytes={} build_peak_rss_kb={} query_rss_kb={}",
        format,
        docs,
        index_ms,
        index_bytes,
        build_peak_rss,
        rss_kb()
    );
    let queries: [(&str, &dyn Query); 5] = [
        ("name", q_name.as_ref()),
        ("substring", q_substring.as_ref()),
        ("bm25", q_bm25.as_ref()),
        ("filter", q_filter.as_ref()),
        ("folded", q_folded.as_ref()),
    ];
    for (label, query) in queries {
        let (p50, p95, hits) = bench(|| {
            searcher
                .search(query, &TopDocs::with_limit(10).order_by_score())
                .map(|top| top.len())
                .unwrap_or(0)
        });
        println!(
            "query format={} label={} p50_us={:.3} p95_us={:.3} hits={}",
            format,
            label,
            p50 as f64 / 1_000.0,
            p95 as f64 / 1_000.0,
            hits
        );
    }
    println!("query_rss_kb={}", rss_kb());
    Ok(())
}

fn run_fts5(dir: &Path, docs: usize) -> Result<(), String> {
    let (conn, build_peak_rss, index_ms) = build_fts5(dir, docs)?;
    let index_bytes = dir_size(dir);
    println!(
        "summary format=fts5 docs={} index_ms={} index_bytes={} build_peak_rss_kb={} query_rss_kb={}",
        docs,
        index_ms,
        index_bytes,
        build_peak_rss,
        rss_kb()
    );
    let mut name = conn
        .prepare("SELECT path FROM docs WHERE filename MATCH ?1 LIMIT 10")
        .map_err(|e| e.to_string())?;
    let mut substring = conn
        .prepare("SELECT path FROM docs WHERE content MATCH ?1 LIMIT 10")
        .map_err(|e| e.to_string())?;
    let mut bm25 = conn
        .prepare("SELECT path FROM docs WHERE docs MATCH ?1 ORDER BY rank LIMIT 10")
        .map_err(|e| e.to_string())?;
    let mut filter = conn
        .prepare(
            "SELECT path FROM docs WHERE docs MATCH ?1 AND ext = 'pdf'
             ORDER BY rank LIMIT 10",
        )
        .map_err(|e| e.to_string())?;
    let mut folded = conn
        .prepare("SELECT path FROM docs WHERE content MATCH ?1 LIMIT 10")
        .map_err(|e| e.to_string())?;

    let queries: [(&str, &mut rusqlite::Statement<'_>, &str); 5] = [
        ("name", &mut name, "doc0000001"),
        ("substring", &mut substring, "needle"),
        ("bm25", &mut bm25, "quarterly revenue"),
        ("filter", &mut filter, "quarterly revenue"),
        ("folded", &mut folded, "nutsmp"),
    ];
    for (label, statement, text) in queries {
        let (p50, p95, hits) = bench(|| {
            statement
                .query_map(params![text], |row| row.get::<_, String>(0))
                .map(|rows| rows.flatten().count())
                .unwrap_or(0)
        });
        println!(
            "query format=fts5 label={} p50_us={:.3} p95_us={:.3} hits={}",
            label,
            p50 as f64 / 1_000.0,
            p95 as f64 / 1_000.0,
            hits
        );
    }
    println!("query_rss_kb={}", rss_kb());
    Ok(())
}
