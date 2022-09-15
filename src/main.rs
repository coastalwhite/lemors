use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::process;

use clap::{Parser, Subcommand};
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use log::{error, info, warn};
use tui::backend::CrosstermBackend;
use tui::Terminal;

mod auth;
mod config;
mod info_caching;
mod post_login;
mod ui;

use auth::{try_auth, AuthUserInfo};
use config::Config;
use post_login::{EnvironmentStartError, PostLoginEnvironment};

const DEFAULT_CONFIG_PATH: &str = "/etc/lemurs/config.toml";
const PREVIEW_LOG_PATH: &str = "lemurs.log";
const DEFAULT_LOG_PATH: &str = "/var/log/lemurs.log";

fn merge_in_configuration(config: &mut Config, config_path: Option<&Path>) {
    let load_config_path = config_path.unwrap_or_else(|| Path::new(DEFAULT_CONFIG_PATH));

    match config::PartialConfig::from_file(load_config_path) {
        Ok(partial_config) => {
            info!(
                "Successfully loaded configuration file from '{}'",
                load_config_path.display()
            );
            config.merge_in_partial(partial_config)
        }
        Err(err) => {
            // If we have given it a specific config path, it should crash if this file cannot be
            // loaded. If it is the default config location just put a warning in the logs.
            if let Some(config_path) = config_path {
                eprintln!(
                    "The config file '{}' cannot be loaded.\nReason: {}",
                    config_path.display(),
                    err
                );
                process::exit(1);
            } else {
                warn!(
                    "No configuration file loaded from the expected location ({}). Reason: {}",
                    DEFAULT_CONFIG_PATH, err
                );
            }
        }
    }
}

fn setup_logger(is_preview: bool) {
    let log_path = if is_preview {
        PREVIEW_LOG_PATH
    } else {
        DEFAULT_LOG_PATH
    };

    let log_file = fern::log_file(log_path).unwrap_or_else(|err| {
        eprintln!(
            "Failed to open log file: '{}'. Check that the path is valid or activate `--no-log`. Reason: {}",
            log_path, err
        );
        process::exit(1);
    });

    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{}[{}][{}] {}",
                chrono::Local::now().format("[%Y-%m-%d][%H:%M:%S]"),
                record.target(),
                record.level(),
                message
            ))
        })
        .level(log::LevelFilter::Debug)
        .level_for("hyper", log::LevelFilter::Info)
        .chain(log_file)
        .apply()
        .unwrap_or_else(|err| {
            eprintln!(
                "Failed to setup logger. Fix the error or activate `--no-log`. Reason: {}",
                err
            );
            process::exit(1);
        });
}

#[derive(Parser)]
#[clap(name = "Lemurs", about, author, version)]
struct Cli {
    #[clap(long)]
    preview: bool,

    #[clap(long)]
    no_log: bool,

    /// Override the configured TTY number
    #[clap(long, value_name = "N")]
    tty: Option<u8>,

    /// A file to replace the default configuration
    #[clap(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    #[clap(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Envs,
    Cache,
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    if let Some(cmd) = cli.command {
        match cmd {
            Commands::Envs => {
                let envs = post_login::get_envs();

                for (env_name, _) in envs.into_iter() {
                    println!("{}", env_name);
                }
            }
            Commands::Cache => {
                let cached_info = info_caching::get_cached_information();

                let environment = cached_info
                    .environment()
                    .map(|s| format!("'{}'", s))
                    .unwrap_or(String::from("No cached value"));
                let username = cached_info
                    .username()
                    .map(|s| format!("'{}'", s))
                    .unwrap_or(String::from("No cached value"));

                println!(
                    "Information currently cached within '{}'\n",
                    info_caching::CACHE_PATH
                );

                println!("environment: {}", environment);
                println!("username: {}", username);
            }
        }

        return Ok(());
    }

    // Setup the logger
    if !cli.no_log {
        setup_logger(cli.preview);
    }

    info!("Lemurs logger is running");

    // Load and setup configuration
    let mut config = Config::default();
    merge_in_configuration(&mut config, cli.config.as_deref());

    if let Some(tty) = cli.tty {
        info!("Overwritten the tty to '{}' with the --tty flag", tty);
        config.tty = tty;
    }

    if !cli.preview {
        // Switch to the proper tty
        info!("Switching to tty {}", config.tty);

        chvt::chvt(config.tty.into()).unwrap_or_else(|err| {
            error!("Failed to switch tty {}. Reason: {}", config.tty, err);
        });
    }

    // Start application
    let mut terminal = tui_enable()?;
    let login_form = ui::LoginForm::new(config, cli.preview);
    login_form.run(&mut terminal, try_auth, post_login_env_start)?;
    tui_disable(terminal)?;

    info!("Lemurs is booting down");

    Ok(())
}

pub fn tui_enable() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;

    info!("UI booted up");

    Ok(terminal)
}

pub fn tui_disable(mut terminal: Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    info!("Reset terminal environment");

    Ok(())
}

fn post_login_env_start<'a>(
    post_login_env: &PostLoginEnvironment,
    config: &Config,
    user_info: &AuthUserInfo<'a>,
) -> Result<(), EnvironmentStartError> {
    post_login_env.start(config, user_info)
}
