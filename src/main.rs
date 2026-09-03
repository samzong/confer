mod adapters;
mod cli;
mod mcp;
mod mcp_host;
mod state;
mod types;

fn main() -> anyhow::Result<()> {
    cli::run()
}
