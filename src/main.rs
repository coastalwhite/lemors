use std::fs::File;
use std::io;
use std::{error::Error, path::Path};

use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use log::{error, info, warn};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

mod auth;
mod chvt;
mod cli;
mod config;
mod env_container;
mod info_caching;
mod post_login;
mod ui;

use auth::try_auth;
use config::Config;
use post_login::{EnvironmentStartError, PostLoginEnvironment};

use crate::{
    auth::utmpx::add_utmpx_entry,
    cli::{Cli, Commands},
};

use self::{
    auth::AuthenticationError,
    env_container::EnvironmentContainer,
    post_login::env_variables::{
        remove_xdg, set_basic_variables, set_display, set_seat_vars, set_session_params,
        set_session_vars, set_xdg_common_paths,
    },
};

const DEFAULT_VARIABLES_PATH: &str = "/etc/lemurs/variables.toml";
const DEFAULT_CONFIG_PATH: &str = "/etc/lemurs/config.toml";
const PREVIEW_LOG_PATH: &str = "lemurs.log";

fn merge_in_configuration(config: &mut Config, cli: &Cli) {
    let load_variables_path = cli
        .variables
        .as_deref()
        .unwrap_or_else(|| Path::new(DEFAULT_VARIABLES_PATH));

    let variables = match config::Variables::from_file(load_variables_path) {
        Ok(variables) => {
            info!(
                "Successfully loaded variables file from '{}'",
                load_variables_path.display()
            );

            Some(variables)
        }
        Err(err) => {
            // If we have given it a specific config path, it should crash if this file cannot be
            // loaded. If it is the default config location just put a warning in the logs.
            if let Some(variables_path) = cli.variables.as_ref() {
                eprintln!(
                    "The variables file '{}' cannot be loaded.\nReason: {}",
                    variables_path.display(),
                    err
                );
                std::process::exit(1);
            } else {
                info!(
                    "No variables file loaded from the default location ({}). Reason: {}",
                    DEFAULT_CONFIG_PATH, err
                );
            }

            None
        }
    };

    let load_config_path = cli
        .config
        .as_deref()
        .unwrap_or_else(|| Path::new(DEFAULT_CONFIG_PATH));

    match config::PartialConfig::from_file(load_config_path, variables.as_ref()) {
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
            if let Some(config_path) = cli.config.as_ref() {
                eprintln!(
                    "The config file '{}' cannot be loaded.\nReason: {}",
                    config_path.display(),
                    err
                );
                std::process::exit(1);
            } else {
                warn!(
                    "No configuration file loaded from the expected location ({}). Reason: {}",
                    DEFAULT_CONFIG_PATH, err
                );
            }
        }
    }

    if let Some(xsessions) = cli.xsessions.as_ref() {
        config.x11.xsessions_path = xsessions.display().to_string();
    }

    if let Some(wlsessions) = cli.wlsessions.as_ref() {
        config.wayland.wayland_sessions_path = wlsessions.display().to_string();
    }
}

pub fn initialize_panic_handler() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        crossterm::execute!(std::io::stderr(), crossterm::terminal::LeaveAlternateScreen).unwrap();
        crossterm::terminal::disable_raw_mode().unwrap();

        original_hook(panic_info);
    }));
}

fn setup_logger(log_path: &str) {
    let log_file = Box::new(
        File::options()
            .append(true)
            .create(true)
            .open(log_path)
            .unwrap_or_else(|_| {
                eprintln!("Failed to open log file: '{log_path}'");
                std::process::exit(1);
            }),
    );

    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .target(env_logger::Target::Pipe(log_file))
        .format_timestamp_secs()
        .init();
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse().unwrap_or_else(|err| {
        eprintln!("{err}\n");
        cli::usage();
        std::process::exit(2);
    });

    let mut config = Config::default();
    merge_in_configuration(&mut config, &cli);

    if let Some(cmd) = cli.command {
        match cmd {
            Commands::Envs => {
                let envs = post_login::get_envs(&config);

                for (env_name, _) in envs.into_iter() {
                    println!("{env_name}");
                }
            }
            Commands::Cache => {
                let cached_info = info_caching::get_cached_information(&config);

                let environment = cached_info.environment().unwrap_or("No cached value");
                let username = cached_info.username().unwrap_or("No cached value");

                println!(
                    "Information currently cached within '{}'\n",
                    config.cache_path
                );

                println!("environment: '{environment}'");
                println!("username: '{username}'");
            }
            Commands::Help => {
                cli::usage();
            }
            Commands::Version => {
                println!("{}", env!("CARGO_PKG_VERSION"));
            }
        }

        return Ok(());
    }

    // Setup the logger
    if !cli.no_log {
        setup_logger(if cli.preview {
            PREVIEW_LOG_PATH
        } else {
            &config.main_log_path
        });
        info!("Main lemurs logger is running");
    } else {
        config.do_log = false;
    }

    if !cli.preview {
        if std::env::var("XDG_SESSION_TYPE").is_ok() {
            eprintln!("Lemurs cannot be ran without `--preview` within an existing session. Namely, `XDG_SESSION_TYPE` is set.");
            error!("Lemurs cannot be started when within an existing session. Namely, `XDG_SESSION_TYPE` is set.");
            std::process::exit(1);
        }

        let uid = uzers::get_current_uid();
        if uzers::get_current_uid() != 0 {
            eprintln!("Lemurs needs to be ran as root. Found user id '{uid}'");
            error!("Lemurs not ran as root. Found user id '{uid}'");
            std::process::exit(1);
        }

        if let Some(tty) = cli.tty {
            info!("Overwritten the tty to '{tty}' with the --tty flag");
            config.tty = tty;
        }

        // Switch to the proper tty
        info!("Switching to tty {}", config.tty);

        unsafe { chvt::chvt(config.tty.into()) }.unwrap_or_else(|err| {
            error!("Failed to switch tty {}. Reason: {err}", config.tty);
        });
    }

    initialize_panic_handler();

    // Start application
    let mut terminal = tui_enable()?;
    let login_form = ui::LoginForm::new(config, cli.preview);
    login_form.run(&mut terminal)?;
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

struct Hooks<'a> {
    pre_validate: Option<&'a dyn Fn()>,
    pre_auth: Option<&'a dyn Fn()>,
    pre_environment: Option<&'a dyn Fn()>,
    pre_wait: Option<&'a dyn Fn()>,
    pre_return: Option<&'a dyn Fn()>,
}

pub enum StartSessionError {
    AuthenticationError(AuthenticationError),
    EnvironmentStartError(EnvironmentStartError),
}

impl From<EnvironmentStartError> for StartSessionError {
    fn from(value: EnvironmentStartError) -> Self {
        Self::EnvironmentStartError(value)
    }
}

impl From<AuthenticationError> for StartSessionError {
    fn from(value: AuthenticationError) -> Self {
        Self::AuthenticationError(value)
    }
}

fn start_session(
    username: &str,
    password: &str,
    post_login_env: &PostLoginEnvironment,
    hooks: &Hooks<'_>,
    config: &Config,
) -> Result<(), StartSessionError> {
    info!(
        "Starting new session for '{}' in environment '{:?}'",
        username, post_login_env
    );

    if let Some(pre_validate_hook) = hooks.pre_validate {
        pre_validate_hook();
    }

    let mut process_env = EnvironmentContainer::take_snapshot();

    if let Some(pre_auth_hook) = hooks.pre_auth {
        pre_auth_hook();
    }

    if matches!(post_login_env, PostLoginEnvironment::X { .. }) {
        set_display(&config.x11.x11_display, &mut process_env);
    }
    set_session_params(&mut process_env, post_login_env);
    remove_xdg(&mut process_env);

    let auth_session = try_auth(username, password, &config.pam_service)?;

    if let Some(pre_environment_hook) = hooks.pre_environment {
        pre_environment_hook();
    }

    let tty = config.tty;
    let uid = auth_session.uid;
    let homedir = &auth_session.home_dir;
    let shell = &auth_session.shell;

    set_seat_vars(&mut process_env, tty);
    set_session_vars(&mut process_env, uid);
    set_basic_variables(&mut process_env, username, homedir, shell);
    set_xdg_common_paths(&mut process_env, homedir);

    let spawned_environment = post_login_env.spawn(&auth_session, &mut process_env, config)?;

    let pid = spawned_environment.pid();

    let utmpx_session = add_utmpx_entry(username, tty, pid);
    drop(process_env);

    info!("Waiting for environment to terminate");

    if let Some(pre_wait_hook) = hooks.pre_wait {
        pre_wait_hook();
    }

    spawned_environment.wait();

    info!("Environment terminated. Returning to Lemurs...");

    if let Some(pre_return_hook) = hooks.pre_return {
        pre_return_hook();
    }

    drop(utmpx_session);
    drop(auth_session);

    Ok(())
}
