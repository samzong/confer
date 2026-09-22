mod adapters;
mod cli;
mod mcp;
mod mcp_host;
mod state;
mod status;
#[cfg(test)]
mod test_support;
mod types;

fn main() -> anyhow::Result<()> {
    cli::run()
}
