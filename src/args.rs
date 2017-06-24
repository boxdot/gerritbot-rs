use clap::App;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Args {
    pub gerrit_hostname: String,
    pub gerrit_port: u16,
    pub gerrit_username: String,
    pub gerrit_priv_key_path: PathBuf,
    pub spark_url: String,
    pub spark_endpoint: String,
    pub spark_webhook_url: Option<String>,
    pub spark_bot_token: String,
    pub verbosity: usize,
    pub quiet: bool,
}

const SPARK_URL: &'static str = "https://api.ciscospark.com/v1";

const USAGE: &'static str = r#"
--gerrit-hostname=<URL>           'Gerrit hostname'
--gerrit-port=<PORT>              'Gerrit port'
--gerrit-username=<USER>          'Gerrit username'
--gerrit-priv-key-path=<PATH>     'Path to the private key for authentication in Gerrit. Note: Due to the limitations of `ssh2` crate only RSA and DSA are supported.'
--spark-endpoint=[localhost:8888] 'Endpoint on which the bot will listen for incoming Spark messages.'
--spark-webhook-url=[URL]         'If specified, the URL will be registered in Spark as webhook endpoint. Note: this url will replace all other registered webhooks.'
--spark-bot-token=<TOKEN>         'Token of the Spark bot for authentication'

-v...                             'Verbosity level'
-q...                             'Quiet'
"#;

pub fn parse_args() -> Args {
    let matches = App::new("gerritbot")
        .version("0.1.1")
        .author("boxdot <d@zerovolt.org>")
        .about(
            "A Cisco Spark bot, which notifies you about new review approvals (i.e. \
                +2/+1/-1/-2 etc.) from Gerrit.",
        )
        .args_from_usage(USAGE)
        .get_matches();

    Args {
        gerrit_hostname: String::from(matches.value_of("gerrit-hostname").unwrap()),
        gerrit_port: value_t_or_exit!(matches.value_of("gerrit-port"), u16),
        gerrit_username: String::from(matches.value_of("gerrit-username").unwrap()),
        gerrit_priv_key_path: PathBuf::from(matches.value_of("gerrit-priv-key-path").unwrap()),
        spark_url: String::from(SPARK_URL),
        spark_endpoint: String::from(matches.value_of("spark-endpoint").unwrap_or(
            "localhost:8888",
        )),
        spark_webhook_url: if matches.is_present("spark-webhook-url") {
            Some(String::from(matches.value_of("spark-webhook-url").unwrap()))
        } else {
            None
        },
        spark_bot_token: String::from(matches.value_of("spark-bot-token").unwrap()),
        verbosity: 2 + matches.occurrences_of("v") as usize,
        quiet: matches.is_present("q"),
    }
}
