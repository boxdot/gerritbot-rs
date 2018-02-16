extern crate docopt;

use docopt::Docopt;
use std::path::PathBuf;
use rusoto_core::Region;

#[derive(Debug, Deserialize, Clone)]
pub struct Args {
    pub flag_gerrit_hostname: String,
    pub flag_gerrit_port: u16,
    pub flag_gerrit_username: String,
    pub flag_gerrit_priv_key_path: PathBuf,
    pub flag_spark_url: String,
    pub flag_spark_endpoint: String,
    pub flag_spark_sqs: String,
    pub flag_spark_sqs_region: Region,
    pub flag_spark_webhook_url: Option<String>,
    pub flag_spark_bot_token: String,
    pub flag_verbose: bool,
    pub flag_quiet: bool,
    pub flag_bot_msg_expiration: u64,
    pub flag_bot_msg_capacity: usize,
}

const USAGE: &'static str = "
Cisco Spark <> Gerrit Bot

Usage:
    gerritbot-rs [options]
    gerritbot-rs -h | --help

    -h --help     Show this screen.
    -v --verbose  Print more
    -q --quiet    Be silent

    --gerrit-hostname HOSTNAME    Hostname of the Gerrit instance to listen on
    --gerrit-port PORT            SSH port of the Gerrit instance [default:29418]
    --gerrit-username USERNAME    SSH username of the Gerrit account to connect with
    --gerrit-priv-key-path FILE   Path of the SSH key assigned to the Gerrit account to connect with

    --spark-url URL               Cisco Spark API URL [default: https://api.ciscospark.com/v1]
    --spark-endpoint URL          Cisco Spark endpoint
    --spark-sqs URI               SQS ARN
    --spark-sqs-region REGION     AWS region where the SQS is in [default: us-east-1]
    --spark-webhook-url URL       Webhook URL
    --spark-bot-token TOKEN       Cisco Spark API token

    --bot-msg-expiration SECS     Duration to keep the events queued [default: 2]
    --bot-msg-capacity CAPACITY   Capacity of the queue [default: 100]
";

pub fn parse_args() -> Args {
    let args: Args = Docopt::new(USAGE)
                            .and_then(|d| d.deserialize())
                            .unwrap_or_else(|e| e.exit());
    println!("{:?}", args);
    args
}
