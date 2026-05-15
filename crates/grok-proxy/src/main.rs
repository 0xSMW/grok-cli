use std::net::SocketAddr;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use grok_client::GrokClient;
use grok_proxy::{
    DEFAULT_PROXY_HOSTNAME, DEFAULT_PROXY_PORT, GrokClientBackend, configured_proxy_max_body_size,
    load_proxy_credentials_from_environment, router_with_max_body_size,
};

#[derive(Debug, Parser)]
#[command(name = "proxy", version, about = "OpenAI-compatible proxy for Grok")]
struct ProxyCli {
    #[command(subcommand)]
    command: ProxyCommand,
}

#[derive(Debug, Subcommand)]
enum ProxyCommand {
    /// Starts the Grok Proxy server.
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
struct ServeArgs {
    /// Set the hostname the server will run on.
    #[arg(long, short = 'H', default_value = DEFAULT_PROXY_HOSTNAME)]
    hostname: String,

    /// Set the port the server will run on.
    #[arg(long, short = 'p', default_value_t = DEFAULT_PROXY_PORT)]
    port: u16,

    /// Set the environment to run on.
    #[arg(long, short = 'e')]
    env: Option<String>,

    /// Enable verbose logging.
    #[arg(long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = ProxyCli::parse();
    match cli.command {
        ProxyCommand::Serve(args) => serve(args).await,
    }
}

async fn serve(args: ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let _environment_name = args.env.as_deref();
    let credentials = load_proxy_credentials_from_environment()?;
    let client = GrokClient::with_options(credentials.cookies, args.verbose, None)?;
    let max_body_size = configured_proxy_max_body_size(|name| std::env::var(name).ok());
    let app = router_with_max_body_size(Arc::new(GrokClientBackend::new(client)), max_body_size);
    let bind_target = format!("{}:{}", args.hostname, args.port);
    let listener = tokio::net::TcpListener::bind(&bind_target).await?;
    let address = listener
        .local_addr()
        .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], args.port)));

    println!("Server starting on http://{address}");
    axum::serve(listener, app).await?;
    Ok(())
}
