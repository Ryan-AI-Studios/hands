use clap::{Parser, Subcommand, ValueEnum};
use hands::{
    ActuateRequest, Detail, HandsError, ObserveRequest, actuate, ensure_dpi, observe,
    serialize_envelope,
};

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
    /// Bézier-move and left-click a target
    Click {
        #[arg(long)]
        element_id: Option<String>,
        #[arg(long)]
        grid: Option<String>,
        #[arg(long)]
        x: Option<i32>,
        #[arg(long)]
        y: Option<i32>,
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Bézier-move and pause 100 ms
    Hover {
        #[arg(long)]
        element_id: Option<String>,
        #[arg(long)]
        grid: Option<String>,
        #[arg(long)]
        x: Option<i32>,
        #[arg(long)]
        y: Option<i32>,
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Type text (short Unicode or long clipboard paste+restore)
    Type {
        #[arg(long)]
        text: String,
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Press a named key or combo
    Key {
        #[arg(long)]
        name: String,
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Scroll the wheel (notches). Optional target hovers first.
    Scroll {
        #[arg(long)]
        dy: i32,
        #[arg(long)]
        dx: Option<i32>,
        #[arg(long)]
        element_id: Option<String>,
        #[arg(long)]
        grid: Option<String>,
        #[arg(long)]
        x: Option<i32>,
        #[arg(long)]
        y: Option<i32>,
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Wait until an ROI stops changing
    WaitSettle {
        #[arg(long)]
        x: Option<i32>,
        #[arg(long)]
        y: Option<i32>,
        #[arg(long)]
        w: Option<i32>,
        #[arg(long)]
        h: Option<i32>,
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Halt injected input (no-op unless an MCP lease is live in this process)
    Stop {
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
        other => {
            if let Err(err) = dpi {
                fail(err);
            }
            input_main(other)
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

fn input_main(command: Command) -> Result<(), HandsError> {
    let install_lease = !matches!(command, Command::Stop { .. });
    let _lease = if install_lease {
        Some(hands::lease::install()?)
    } else {
        None
    };
    let (envelope, ok) = match command {
        Command::Click {
            element_id,
            grid,
            x,
            y,
            session_id,
        } => pack(actuate::click(ActuateRequest {
            session_id,
            element_id,
            grid,
            x,
            y,
            ..ActuateRequest::default()
        }))?,
        Command::Hover {
            element_id,
            grid,
            x,
            y,
            session_id,
        } => pack(actuate::hover(ActuateRequest {
            session_id,
            element_id,
            grid,
            x,
            y,
            ..ActuateRequest::default()
        }))?,
        Command::Type { text, session_id } => pack(actuate::type_text(ActuateRequest {
            session_id,
            text: Some(text),
            ..ActuateRequest::default()
        }))?,
        Command::Key { name, session_id } => pack(actuate::key(ActuateRequest {
            session_id,
            name: Some(name),
            ..ActuateRequest::default()
        }))?,
        Command::Scroll {
            dy,
            dx,
            element_id,
            grid,
            x,
            y,
            session_id,
        } => pack(actuate::scroll(ActuateRequest {
            session_id,
            element_id,
            grid,
            x,
            y,
            dy: Some(dy),
            dx,
            ..ActuateRequest::default()
        }))?,
        Command::WaitSettle {
            x,
            y,
            w,
            h,
            session_id,
        } => pack(actuate::wait_settle(ActuateRequest {
            session_id,
            x,
            y,
            w,
            h,
            ..ActuateRequest::default()
        }))?,
        Command::Stop { session_id } => pack(actuate::stop_cli_noop(ActuateRequest {
            session_id,
            ..ActuateRequest::default()
        }))?,
        Command::Mcp | Command::Observe { .. } => unreachable!(),
    };
    println!("{envelope}");
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

fn pack(result: Result<hands::ActuateEnvelope, HandsError>) -> Result<(String, bool), HandsError> {
    let envelope = result?;
    let json = actuate::serialize_envelope(&envelope)?;
    Ok((json, envelope.ok))
}

fn fail(err: HandsError) -> ! {
    eprintln!("{}", err.tool_message());
    std::process::exit(1);
}
