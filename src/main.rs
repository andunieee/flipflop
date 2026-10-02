fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let dir = std::env::var_os("FLIPFLOP_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("flipflop"));
    flipflop::app::run(dir)
}
