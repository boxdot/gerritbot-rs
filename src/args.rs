use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Args {
    pub hostname: String,
    pub port: u16,
    pub username: String,
    pub priv_key_path: PathBuf,
    pub spark_url: String,
    pub spark_bot_token: String,
}

const SPARK_URL: &'static str = "https://api.ciscospark.com/v1";

pub fn parse_args<Iter>(mut args: Iter) -> Result<Args, &'static str>
    where Iter: Iterator<Item = String>
{
    args.next();
    let hostname = args.next().ok_or("argument 'hostname' missing")?;
    let port = args.next().ok_or("argument 'port' is missing")?;
    let port: u16 = port.parse().map_err(|_| "cannot parse port")?;
    let username = args.next().ok_or("argument 'username' missing")?;
    let priv_key_path = args.next().ok_or("path to private key is missing")?;
    let bot_token = args.next().ok_or("bot token is missing")?;

    Ok(Args {
        hostname: hostname,
        port: port,
        username: username,
        priv_key_path: PathBuf::from(priv_key_path),
        spark_url: String::from(SPARK_URL),
        spark_bot_token: bot_token,
    })
}
