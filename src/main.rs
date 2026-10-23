fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let dir = dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("flipflop");
    flipflop::app::run(dir)
}
