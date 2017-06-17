use clap::App;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Args {
    pub hostname: String,
    pub port: u16,
    pub username: String,
    pub priv_key_path: PathBuf,
    pub spark_url: String,
    pub spark_bot_token: String,
    pub spark_bot_id: String,
}

const SPARK_URL: &'static str = "https://api.ciscospark.com/v1";

const USAGE: &'static str = r#"
"-h, --hostname=<URL>   'Gerrit hostname'
-p, --port=<PORT>       'Gerrit port'
-u, --username=<USER>   'Gerrit username'
--priv-key-path=<PATH>  'Path to private key. Note: Due to the limitations of `ssh2` crate only RSA and DSA are supported.'
--bot-token=<TOKEN>     'Token of the Spark bot for authentication.'
--bot-id=<ID>           'Identity of the Spark bot for filtering own messages.'
-v...                   'Verbosity level.'
"#;

pub fn parse_args() -> Args {
    let matches = App::new("gerritbot")
        .version("0.1.0")
        .author("boxdot <d@zerovolt.org>")
        .about(
            "A Cisco Spark bot, which notifies you about new review approvals (i.e. \
                +2/+1/-1/-2 etc.) from Gerrit.",
        )
        .args_from_usage(USAGE)
        .get_matches();

    Args {
        hostname: String::from(matches.value_of("hostname").unwrap()),
        port: value_t_or_exit!(matches.value_of("port"), u16),
        username: String::from(matches.value_of("username").unwrap()),
        priv_key_path: PathBuf::from(matches.value_of("priv-key-path").unwrap()),
        spark_url: String::from(SPARK_URL),
        spark_bot_token: String::from(matches.value_of("bot-token").unwrap()),
        spark_bot_id: String::from(matches.value_of("bot-id").unwrap()),
    }
}
