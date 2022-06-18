use nix::unistd::{initgroups, setgid, setuid, Gid, Uid};
use rand::Rng;
use std::env;
use std::process::{Child, Command, Stdio};
use std::{thread, time};

use std::fs::File;
use std::path::PathBuf;

use log::{error, info};

use crate::auth::AuthUserInfo;

const DISPLAY: &str = ":1";
const VIRTUAL_TERMINAL: &str = "vt01";

const SYSTEM_SHELL: &str = "/bin/sh";

pub enum XSetupError {
    XAuthFileCreation,
    XAuthFilling,
    XServerStart,
}

pub enum XStartEnvError {
    UsernameConversion,
    SettingGroups,
    SettingUid,
    SettingGid,
    StartingEnvironment,
}

fn mcookie() -> String {
    // TODO: Verify that this is actually safe. Maybe just use the mcookie binary?? Is that always
    // available?
    let mut rng = rand::thread_rng();
    let cookie: u128 = rng.gen();
    format!("{:032x}", cookie)
}

pub fn setup_x(user_info: &AuthUserInfo) -> Result<Child, XSetupError> {
    info!("Start setup of X");

    // Setup xauth
    let xauth_dir = PathBuf::from(env::var("XDG_CONFIG_HOME").unwrap_or(user_info.dir.to_string()));
    let xauth_path = xauth_dir.join(".Xauthority");
    env::set_var("XAUTHORITY", xauth_path.clone());
    env::set_var("DISPLAY", DISPLAY);

    // TODO: Log the xauth path
    info!("Creating xauth file");
    File::create(xauth_path).map_err(|err| {
        error!("Creation of xauth file failed. Reason: {}", err);
        XSetupError::XAuthFileCreation
    })?;

    info!("Filling xauth file");
    Command::new(SYSTEM_SHELL)
        .arg("-c")
        .arg(format!("/usr/bin/xauth add {} . {}", DISPLAY, mcookie()))
        .stdout(Stdio::null()) // TODO: Maybe this should be logged or something?
        .stderr(Stdio::null()) // TODO: Maybe this should be logged or something?
        .status()
        .map_err(|err| {
            error!("Filling xauth file failed. Reason: {}", err);
            XSetupError::XAuthFilling
        })?;

    info!("Run X server");
    let child = Command::new(SYSTEM_SHELL)
        .arg("-c")
        .arg(format!("/usr/bin/X {} {}", DISPLAY, VIRTUAL_TERMINAL))
        .stdout(Stdio::null()) // TODO: Maybe this should be logged or something?
        .stderr(Stdio::null()) // TODO: Maybe this should be logged or something?
        .spawn()
        .map_err(|err| {
            error!("Starting X server failed. Reason: {}", err);
            XSetupError::XServerStart
        })?;

    // Wait for XServer to boot-up
    // TODO: There should be a better way of doing this.
    thread::sleep(time::Duration::from_secs(1));
    info!("X server is running");

    Ok(child)
}

pub fn start_env(user_info: &AuthUserInfo, script_path: &str) -> Result<Child, XStartEnvError> {
    let uid = Uid::from_raw(user_info.uid);
    let gid = Gid::from_raw(user_info.gid);
    let username = std::ffi::CString::new(user_info.name.clone()).map_err(|err| {
        error!(
            "Failed to convert '{}' into CString. Reason: {}",
            user_info.name, err
        );
        XStartEnvError::UsernameConversion
    })?;

    info!("Setting uid, gid and supplementary groups");
    initgroups(username.as_c_str(), gid).map_err(|err| {
        error!("Failed to set supplementary groups. Reason: {}", err);
        XStartEnvError::SettingGroups
    })?;
    setgid(gid).map_err(|err| {
        error!("Failed to set gid. Reason: {}", err);
        XStartEnvError::SettingGid
    })?;
    setuid(uid).map_err(|err| {
        error!("Failed to set uid. Reason: {}", err);
        XStartEnvError::SettingUid
    })?;

    info!("Starting specified environment");
    let child = Command::new(SYSTEM_SHELL)
        .arg("-c")
        .arg(format!("{} {}", "/etc/lemurs/xsetup.sh", script_path))
        .stdout(Stdio::null()) // TODO: Maybe this should be logged or something?
        .stderr(Stdio::null()) // TODO: Maybe this should be logged or something?
        .spawn()
        .map_err(|err| {
            error!("Failed to start specified environment. Reason: {}", err);
            XStartEnvError::StartingEnvironment
        })?;
    info!("Started specified environment");

    Ok(child)
}
