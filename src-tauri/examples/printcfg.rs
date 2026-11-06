fn main() {
    println!(
        "{}",
        serde_json::to_string(&mnemo_lib::config::MnemoConfig::default()).unwrap()
    );
}
