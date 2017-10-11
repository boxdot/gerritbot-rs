use clap::{App, AppSettings, Arg};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Args {
    pub gerrit_hostname: String,
    pub gerrit_port: u16,
    pub gerrit_username: String,
    pub gerrit_priv_key_path: PathBuf,
    pub spark_url: String,
    pub spark_endpoint: String,
    pub spark_sqs: String,
    pub spark_webhook_url: Option<String>,
    pub spark_bot_token: String,
    pub verbosity: usize,
    pub quiet: bool,
    pub bot_msg_expiration: Duration,
    pub bot_msg_capacity: usize,
}

const SPARK_URL: &'static str = "https://api.ciscospark.com/v1";

const USAGE: &'static str = r#"
-v, --verbose... 'Verbosity level'
-q, --quiet      'Quiet'
"#;

pub fn parse_args() -> Args {
    let matches = App::new("gerritbot")
        .version("0.1.1")
        .author("boxdot <d@zerovolt.org>")
        .about(
            "A Cisco Spark bot, which notifies you about new review approvals (i.e. +2/+1/-1/-2 \
            etc.) from Gerrit.",
        )
        .setting(AppSettings::DeriveDisplayOrder)
        .setting(AppSettings::NextLineHelp)
        .args_from_usage(USAGE)
        .arg(
            Arg::from_usage("--gerrit-hostname=<URL> 'Gerrit hostname'").empty_values(false),
        )
        .arg(
            Arg::from_usage("--gerrit-port=[29418] 'Gerrit port'").empty_values(false),
        )
        .arg(
            Arg::from_usage("--gerrit-username=<USER> 'Gerrit username'").empty_values(false),
        )
        .arg(
            Arg::from_usage(
                "--gerrit-priv-key-path=<PATH> 'Path to the private key for authentication in \
                Gerrit. Note: Due to the limitations of `ssh2` crate only RSA and DSA are \
                supported.'",
            ).empty_values(false),
        )
        .arg(
            Arg::from_usage(
                "--spark-endpoint=[localhost:8888] 'Endpoint on which the bot will listen for \
                incoming Spark messages.'",
            ).empty_values(false)
                .conflicts_with("spark-sqs"),
        )
        .arg(
            Arg::from_usage(
                "--spark-sqs=[URL] 'AWS SQS Endpoint which should be polled for new Spark message. \
                Note: When using SQS, you need to setup the spark bot to send the messages to this \
                queue (cf. --spark-webhook-url).'",
            ).empty_values(false)
                .conflicts_with("spark-endpoint"),
        )
        .arg(
            Arg::from_usage(
                "--spark-webhook-url=[URL] 'If specified, the URL will be registered in Spark as \
                webhook endpoint. Note: this url will replace all other registered webhooks.'",
            ).empty_values(false),
        )
        .arg(
            Arg::from_usage(
                "--spark-bot-token=<TOKEN> 'Token of the Spark bot for authentication'",
            ).empty_values(false),
        )
        .arg(
            Arg::from_usage(
                "--approval-expiration=[2] 'Approvals that are arriving repeatedly faster than \
                this value (in secs) will be dropped. This is useful when filtering approvals \
                that are sent to multiple reviews in a topic at the same time. 0 disables this \
                feature.'",
            ).empty_values(false),
        )
        .arg(
            Arg::from_usage(
                "--approvals-count=[100] 'Numbers of approvals to store a LRU cache that will be \
                consider for expiration. Cf. also --approval-expiration. 0 disables this \
                feature.'",
            ).empty_values(false),
        )
        .get_matches();

    Args {
        gerrit_hostname: String::from(matches.value_of("gerrit-hostname").unwrap()),
        gerrit_port: if matches.is_present("gerrit-port") {
            value_t_or_exit!(matches.value_of("gerrit-port"), u16)
        } else {
            29418
        },
        gerrit_username: String::from(matches.value_of("gerrit-username").unwrap()),
        gerrit_priv_key_path: PathBuf::from(matches.value_of("gerrit-priv-key-path").unwrap()),
        spark_url: String::from(SPARK_URL),
        // TODO: Do not allow to set both endpoint and sqs
        spark_endpoint: String::from(matches.value_of("spark-endpoint").unwrap_or("")),
        spark_sqs: String::from(matches.value_of("spark-sqs").unwrap_or("")),
        spark_webhook_url: if matches.is_present("spark-webhook-url") {
            Some(String::from(matches.value_of("spark-webhook-url").unwrap()))
        } else {
            None
        },
        spark_bot_token: String::from(matches.value_of("spark-bot-token").unwrap()),
        verbosity: 2 + matches.occurrences_of("verbose") as usize,
        quiet: matches.is_present("quiet"),
        bot_msg_expiration: Duration::from_secs(if matches.is_present("approval-expiration") {
            value_t_or_exit!(matches.value_of("approval-expiration"), u64)
        } else {
            2u64
        }),
        bot_msg_capacity: if matches.is_present("approvals-count") {
            value_t_or_exit!(matches.value_of("approvals-count"), usize)
        } else {
            100 as usize
        },
    }
}
