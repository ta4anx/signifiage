//! Process entry point: logging, config, language, then dispatch.

use clap::Parser;
use gtk4::glib;

use signifiage::cli::{Cli, Command};
use signifiage::config::Config;
use signifiage::ui::Startup;
use signifiage::{i18n, logging, player, ui};

fn main() -> glib::ExitCode {
    logging::init();
    let cli = Cli::parse();
    let config = Config::load();
    i18n::apply(config.language);

    match cli.command {
        Some(Command::Player(args)) => player::run(&args, config),
        Some(Command::Publish(args)) => match player::publish(&args.project, &args.output) {
            Ok(path) => {
                println!("{}", path.display());
                glib::ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("signifiage publish: {err}");
                glib::ExitCode::FAILURE
            }
        },
        None => ui::app::run(Startup {
            project: cli.project,
            config,
        }),
    }
}
