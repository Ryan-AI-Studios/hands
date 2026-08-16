use clap::{Parser, Subcommand, ValueEnum};
use hands::{
    ActuateRequest, Detail, HandsError, ObserveRequest, actuate, allows, ensure_dpi, logs, observe,
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
    /// Grant, revoke, or list confirm-fence allows (does not install the desk lease)
    Confirm {
        #[arg(long, required_unless_present = "list")]
        domain: Option<String>,
        #[arg(long, required_unless_present = "list")]
        category: Option<String>,
        #[arg(long, value_enum, required_unless_present = "list")]
        mode: Option<ConfirmModeArg>,
        #[arg(long)]
        revoke: bool,
        #[arg(long)]
        list: bool,
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Tail or list session JSONL audit logs (does not install the desk lease; does not mint)
    Logs {
        #[arg(long, required_unless_present = "list")]
        session_id: Option<String>,
        #[arg(long)]
        list: bool,
        #[arg(long)]
        tail: Option<usize>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum DetailArg {
    Dom,
}

#[derive(Clone, Copy, ValueEnum)]
enum ConfirmModeArg {
    Once,
    Session,
    Persist,
}

impl ConfirmModeArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Session => "session",
            Self::Persist => "persist",
        }
    }
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
        Command::Confirm {
            domain,
            category,
            mode,
            revoke,
            list,
            session_id,
        } => {
            if let Err(err) = dpi {
                fail(err);
            }
            confirm_main(domain, category, mode, revoke, list, session_id)
        }
        Command::Logs {
            session_id,
            list,
            tail,
        } => {
            if let Err(err) = dpi {
                fail(err);
            }
            logs_main(session_id, list, tail)
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

fn confirm_main(
    domain: Option<String>,
    category: Option<String>,
    mode: Option<ConfirmModeArg>,
    revoke: bool,
    list: bool,
    session_id: Option<String>,
) -> Result<(), HandsError> {
    let envelope = allows::run_confirm(
        session_id.as_deref(),
        domain.as_deref(),
        category.as_deref(),
        mode.map(ConfirmModeArg::as_str),
        revoke,
        list,
    )?;
    let json = allows::serialize_confirm(&envelope)?;
    println!("{json}");
    if !envelope.ok {
        std::process::exit(1);
    }
    Ok(())
}

fn logs_main(
    session_id: Option<String>,
    list: bool,
    tail: Option<usize>,
) -> Result<(), HandsError> {
    let envelope = logs::run_logs(session_id.as_deref(), list, tail)?;
    let json = logs::serialize_logs(&envelope)?;
    println!("{json}");
    if !envelope.ok {
        std::process::exit(1);
    }
    Ok(())
}

fn input_main(command: Command) -> Result<(), HandsError> {
    // Subscribe before hooks so Pause during hover/type/scroll still wipes session allows
    // and appends pause/stop log events.
    hands::fence::ensure_installed();
    hands::logs::ensure_installed();
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
        Command::Mcp | Command::Observe { .. } | Command::Confirm { .. } | Command::Logs { .. } => {
            unreachable!()
        }
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
