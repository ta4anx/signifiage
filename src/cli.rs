//! Command line surface.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Signage, stage playback and queue console.
#[derive(Debug, Parser)]
#[command(
    name = "signifiage",
    version,
    about = "Signage, stage playback and queue console",
    long_about = None,
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    /// Project directory to open in the operator GUI.
    #[arg(value_name = "PROJECT_DIR")]
    pub project: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the standalone signage player.
    Player(PlayerArgs),
    /// Write a distribution package (.signifiage) from a project directory.
    Publish(PublishArgs),
}

#[derive(Debug, Args)]
pub struct PlayerArgs {
    /// Project directory or .signifiage package.
    #[arg(value_name = "PROJECT")]
    pub path: PathBuf,

    /// Open the program output as a 960x540 window instead of fullscreen.
    #[arg(long)]
    pub windowed: bool,

    /// Monitor index for the program output (0-based).
    #[arg(long, value_name = "N")]
    pub monitor: Option<u32>,

    /// Force the LAN web remote on.
    #[arg(long)]
    pub remote: bool,

    /// TCP port for the web remote.
    #[arg(long, value_name = "N")]
    pub port: Option<u16>,

    /// Web remote token; generated and persisted when omitted.
    #[arg(long, value_name = "TOKEN")]
    pub token: Option<String>,
}

#[derive(Debug, Args)]
pub struct PublishArgs {
    /// Project directory to package.
    #[arg(value_name = "PROJECT_DIR")]
    pub project: PathBuf,

    /// Output package path.
    #[arg(value_name = "OUTPUT")]
    pub output: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_project_and_flags() {
        let cli = Cli::parse_from(["signifiage", "/tmp/show"]);
        assert_eq!(
            cli.project.as_deref(),
            Some(std::path::Path::new("/tmp/show"))
        );
        assert!(cli.command.is_none());

        let cli = Cli::parse_from(["signifiage"]);
        assert!(cli.project.is_none());
        assert!(cli.command.is_none());
    }

    #[test]
    fn parses_player_options() {
        let cli = Cli::parse_from([
            "signifiage",
            "player",
            "/tmp/show",
            "--windowed",
            "--monitor",
            "2",
            "--remote",
            "--port",
            "8899",
            "--token",
            "abc",
        ]);
        match cli.command {
            Some(Command::Player(args)) => {
                assert!(args.windowed);
                assert_eq!(args.monitor, Some(2));
                assert!(args.remote);
                assert_eq!(args.port, Some(8899));
                assert_eq!(args.token.as_deref(), Some("abc"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_publish_options() {
        let cli = Cli::parse_from(["signifiage", "publish", "/tmp/show", "/tmp/out.signifiage"]);
        match cli.command {
            Some(Command::Publish(args)) => {
                assert_eq!(args.project, PathBuf::from("/tmp/show"));
                assert_eq!(args.output, PathBuf::from("/tmp/out.signifiage"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
