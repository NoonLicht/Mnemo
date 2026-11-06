//! `searchcheck <data_dir> <query>`: headless search against an existing data directory.
fn main() {
    let mut a = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(a.next().expect("data dir"));
    let q = a.next().expect("query");
    let idx = mnemo_lib::agent::HeadlessIndex::open_at(&dir).expect("open index");
    let r = idx.search(&q, 10).expect("search");
    println!(
        "encrypted={} hits={}",
        mnemo_lib::crypt::enabled(),
        r.results.len()
    );
    for h in r.results {
        println!("{} | {:?}", h.path, h.snippet);
    }
}
