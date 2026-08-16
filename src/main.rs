use clap::{Parser, Subcommand, ValueEnum};
use hands::{Detail, HandsError, ObserveRequest, ensure_dpi, observe, serialize_envelope};

#[derive(Parser)]
#[command(
    name = "hands",
    about = "Windows-first eyes-and-hands MCP/CLI",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the MCP server over stdio
    Mcp,
    /// Capture the desktop and print a compact observe envelope
    Observe {
        /// `dom` for a fuller UIA dump (still skips offscreen/zero-size)
        #[arg(long, value_enum)]
        detail: Option<DetailArg>,
        /// Explicit session id (otherwise sniff env, else mint)
        #[arg(long)]
        session_id: Option<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum DetailArg {
    Dom,
}

impl From<DetailArg> for Detail {
    fn from(_: DetailArg) -> Self {
        Detail::Dom
    }
}

#[tokio::main]
async fn main() {
    let dpi = ensure_dpi();
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Mcp => {
            if let Err(err) = &dpi {
                eprintln!("{}", err.tool_message());
            }
            mcp_main().await
        }
        Command::Observe { detail, session_id } => {
            if let Err(err) = dpi {
                fail(err);
            }
            observe_main(detail, session_id)
        }
    };
    if let Err(err) = result {
        fail(err);
    }
}

async fn mcp_main() -> Result<(), HandsError> {
    hands::mcp::serve().await
}

fn observe_main(detail: Option<DetailArg>, session_id: Option<String>) -> Result<(), HandsError> {
    let envelope = observe(ObserveRequest {
        session_id,
        detail: detail.map(Detail::from).unwrap_or(Detail::Default),
    })?;
    let json = serialize_envelope(&envelope)?;
    println!("{json}");
    Ok(())
}

fn fail(err: HandsError) -> ! {
    eprintln!("{}", err.tool_message());
    std::process::exit(1);
}
