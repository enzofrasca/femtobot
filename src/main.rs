#[tokio::main]
async fn main() -> anyhow::Result<()> {
    femtobot::run_cli().await
}
