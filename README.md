# Gerritbot [![Build Status](https://travis-ci.org/boxdot/gerritbot-rs.svg?branch=master)](https://travis-ci.org/boxdot/gerritbot-rs)

A [Cisco Spark](https://www.ciscospark.com) bot, which notifies you about new review approvals
(i.e. +2/+1/-1/-2 etc.) from [Gerrit](https://www.gerritcodereview.com).

![screenshot](assets/screenshot.png)

## How to use:

1. Register a developer account at https://developer.ciscospark.com.
2. Create a new bot and write down its **api key**.
3. Register a new webhook listener at https://developer.ciscospark.com/resource-webhooks.html. You
   need to provide a url there, on which the bot will be listening for the new Spark messages.
4. Build and run the bot

```shell
$ cargo run -- <arguments>
```

with the following arguments:

```
--gerrit-hostname <URL>              Gerrit hostname
--gerrit-port <PORT>                 Gerrit port
--gerrit-priv-key-path <PATH>
    Path to the private key for authentication in Gerrit. Note: Due to the limitations of `ssh2`
    crate only RSA and DSA are supported.
--gerrit-username <USER>             Gerrit username
--spark-bot-token <TOKEN>            Token of the Spark bot for authentication
--spark-endpoint <localhost:8888>
    Endpoint on which the bot will listen for incoming Spark messages.

--spark-webhook-url <URL>
    If specified, the URL will be registered in Spark as webhook endpoint. Note: this url will
    replace all other registered webhooks.
```

To be able to listen to Gerrit messages, you need to have a Gerrit user with `stream-api` access
capabilities. Admins and Non-interactive users should have such.

The state of the bot is stored in the `state.json` file in the same directory, where the bot is
running.

**This is my first Rust project. Any constructive criticism is welcome.**

## License

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT License ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this document by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
