#[tokio::main]
async fn main() -> anyhow::Result<()> {
    lightclaw::run_cli().await
}
