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
$ cargo run
Usage:
gerritbot <hostname> <port> <username> <priv_key_path> <bot_token>

Arguments:
    hostname        Gerrit hostname
    port            Gerrit port
    username        Gerrit username for stream-events API
    priv_key_path   Path to private key. Note: Due to the limitations of `ssh2` crate
                    only RSA and DSA are supported.
    bot_token       Token of the Spark bot for authentication.
    bot_id          Identity of the Spark bot for filtering own messages.
```

To be able to listen to Gerrit messages, you need to have a Gerrit user with `stream-api` access
capabilities. Admins and Non-interactive users should have such.

The state of the bot is stored in the `state.json` file in the directory, where the bot is running.

**This is my first Rust project. Any constructive criticism is welcome.**

## Nice to have:

- [ ] Automatic update of webhooks URL in Spark.
- [ ] Use a more sophisticated command line parser.
- [ ] Get rid off `libssh2` dependency.
- [ ] Proper configurable logging with verbosity mode.
- [ ] Tests.

## License

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT License ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this document by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
