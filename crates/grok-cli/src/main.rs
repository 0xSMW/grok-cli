use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let result = grok_cli::run_from_with_status(std::env::args_os()).await?;
    if !result.output.is_empty() {
        println!("{}", result.output);
    }
    if result.exit_code != 0 {
        std::process::exit(result.exit_code);
    }
    Ok(())
}
