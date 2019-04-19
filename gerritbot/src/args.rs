use std::fs::File;
use std::path::PathBuf;

use log::debug;
use rusoto_core::Region;
use serde::Deserialize;
use structopt::StructOpt;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub gerrit: GerritConfig,
    pub spark: SparkConfig,
    pub bot: BotConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GerritConfig {
    pub host: String,
    pub username: String,
    pub priv_key_path: PathBuf,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SparkConfig {
    pub bot_token: String,
    pub api_uri: String,
    pub webhook_url: String,
    pub mode: ModeConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub enum ModeConfig {
    Direct { endpoint: std::net::SocketAddr },
    Sqs { uri: String, region: Region },
}

#[derive(Debug, Deserialize, Clone)]
pub struct BotConfig {
    pub msg_expiration: u64,
    pub msg_capacity: usize,
    pub format_script: Option<String>,
}

/// Cisco Webex Teams <> Gerrit Bot
#[derive(StructOpt, Debug, Clone)]
#[structopt(rename_all = "kebab-case")]
pub struct Args {
    /// Print more
    #[structopt(short, long)]
    pub verbose: bool,
    /// Be silent
    #[structopt(short, long, conflicts_with = "verbose")]
    pub quiet: bool,
    /// YAML configuration file
    #[structopt(long, short, default_value = "config.yml")]
    pub config: PathBuf,
    /// Dump default format script and exit
    #[structopt(long)]
    pub dump_format_script: bool,
}

pub fn parse_args() -> Args {
    Args::from_args()
}

pub fn parse_config(path: PathBuf) -> Config {
    let file = File::open(path).unwrap_or_else(|e| {
        eprintln!("Could not open config file: {}", e);
        ::std::process::exit(1)
    });
    let mut config: Config = serde_yaml::from_reader(file).unwrap_or_else(|e| {
        eprintln!("Could not parse config file: {}", e);
        ::std::process::exit(2)
    });
    // tilde expand the private key path
    config.gerrit.priv_key_path =
        shellexpand::tilde(&config.gerrit.priv_key_path.to_string_lossy())
            .into_owned()
            .into();
    debug!("{:#?}", config);
    config
}
