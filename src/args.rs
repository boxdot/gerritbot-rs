use std::path::PathBuf;
use rusoto_core::Region;

#[derive(Debug, Clone)]
pub struct Args {
    pub gerrit_hostname: String,
    pub gerrit_port: u16,
    pub gerrit_username: String,
    pub gerrit_priv_key_path: PathBuf,
    pub spark_url: String,
    pub spark_endpoint: String,
    pub spark_sqs: String,
    pub spark_sqs_region: Region,
    pub spark_webhook_url: Option<String>,
    pub spark_bot_token: String,
    pub verbose: bool,
    pub quiet: bool,
    pub bot_msg_expiration: u64,
    pub bot_msg_capacity: usize,
}

const SPARK_URL: &str = "https://api.ciscospark.com/v1";

pub fn parse_args() -> Args {
    let matches = clap_app!(
        gerritbot =>
            (version: crate_version!())
            (about: crate_description!())
            (author: crate_authors!("\n"))
            (@setting DeriveDisplayOrder)
            (@arg verbose: -v --verbose "Enable verbose logging.")
            (@arg quiet: -q --quiet conflicts_with[verbose] "Disable all log output.")
            (@arg gerrit_hostname: --("gerrit-hostname")
                !empty_values +required value_name("HOST")
                "Gerrit hostname.")
            (@arg gerrit_port: --("gerrit-port")
                default_value("29418") !empty_values value_name("PORT")
                "Gerrit port.")
            (@arg gerrit_username: --("gerrit-username")
                value_name("USER") +required !empty_values
                "Gerrit username")
            (@arg gerrit_priv_key_path: --("gerrit-priv-key-path")
                value_name("PATH") +required !empty_values
                "Path to the private key for authentication in Gerrit. Note: Due \
                to the limitations of `ssh2` crate only RSA and DSA are supported.")
            (@arg spark_endpoint: --("spark-endpoint")
                default_value("localhost:8888") value_name("HOST:PORT") !empty_values
                "Endpoint on which the bot will listen for incoming Spark messages.")
            (@arg spark_sqs: --("spark-sqs")
                value_name("URL") !empty_values conflicts_with[spark_endpoint]
                "AWS SQS Endpoint which should be polled for new Spark message. \
                Note: When using SQS, you need to setup the spark bot to send the \
                messages to this queue (cf. --spark-webhook-url).")
            (@arg spark_sqs_region: --("spark-sqs-region")
                value_name("REGION") !empty_values conflicts_with[spark_endpoint]
                "AWS SQS Region, e.g. us-east-1, eu-central-1, ...")
            (@arg spark_webook_url: --("spark-webhook-url")
                value_name("URL") !empty_values
                "If specified, the URL will be registered in Spark as \
                webhook endpoint. Note: this url will replace all other \
                registered webhooks.")
            (@arg spark_url: --("spark_url")
                value_name("URL") !empty_values default_value(SPARK_URL)
                "Cisco Spark API URL.")
            (@arg spark_bot_token: --("spark-bot-token")
                value_name("TOKEN") +required
                "Token of the Spark bot for authentication.")
            (@arg bot_msg_expiration: --("bot-msg-expiration")
                value_name("SECS") default_value("2") !empty_values
                "Duration to keep the events queued.")
            (@arg bot_msg_capacity: --("bot-msg-capacity")
                value_name("CAPACITY") default_value("100") !empty_values
                "Capacity of the queue.")
    ).get_matches();

    Args {
        gerrit_hostname: matches.value_of("gerrit_hostname").unwrap().to_string(),
        gerrit_port: value_t_or_exit!(matches.value_of("gerrit_port"), u16),
        gerrit_username: matches.value_of("gerrit_username").unwrap().to_string(),
        gerrit_priv_key_path: PathBuf::from(matches.value_of("gerrit_priv_key_path").unwrap()),
        spark_url: matches.value_of("spark_url").unwrap().to_string(),
        spark_endpoint: matches.value_of("spark_endpoint").unwrap().to_string(),
        spark_sqs: matches.value_of("spark_sqs").unwrap_or("").to_string(),
        spark_sqs_region: if matches.is_present("spark_sqs_region") {
            value_t_or_exit!(matches.value_of("spark_sqs_region"), Region)
        } else {
            Region::UsEast1
        },
        spark_webhook_url: matches.value_of("spark_webhook_url").map(|s| s.to_string()),
        spark_bot_token: matches.value_of("spark_bot_token").unwrap().to_string(),
        verbose: matches.is_present("verbose"),
        quiet: matches.is_present("quiet"),
        bot_msg_expiration: value_t_or_exit!(matches.value_of("bot_msg_expiration"), u64),
        bot_msg_capacity: value_t_or_exit!(matches.value_of("bot_msg_capacity"), usize),
    }
}
