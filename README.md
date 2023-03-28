# Gerritbot [![Build Status](https://github.com/boxdot/gerritbot-rs/workflows/CI/badge.svg)](https://github.com/boxdot/gerritbot-rs/actions/workflows/ci.yaml)

**⚠️ Important**: This project is archived now because

1. Cisco WebEx Teams changed their API
2. I don't use Gerrit anymore

---

A [Cisco WebEx Teams](https://teams.webex.com) bot, which notifies you about new review approvals
(i.e. +2/+1/-1/-2 etc.) from [Gerrit](https://www.gerritcodereview.com).

![screenshot](assets/screenshot.png)

## How to use:

1. Register a developer account at https://developer.webex.com.
2. Create a new bot and write down its **api key**.
3. Build and run the bot in direct or SQS mode (cf. below).

```shell
$ cargo run -- <arguments>
```

The bot can run in two modes.

### Direct mode

The bot is listening on a specified endpoint for incoming incoming WebEx Teams messages. For that, you
need to provide the endpoint url to the bot by setting `spark.webhook_url` in the configuration file.
The bot will register the url for you through the Cisco WebEx Teams API. Alternatively, you can also register the
url yourself at [https://developer.webex.com](https://developer.webex.com). In that case,
do not provide the option `spark.webhook_url`, since otherwise it will overwrite you manually
configured url.

See configuration example file in [config-direct.yml](config-direct.yml) in the repository.

Example:

```shell
$ cargo run -- --config config-direct.yml
```

In this setup, the bot is listening for the incoming messages at `localhost:8888`, where WebEx Teams will
send the messages to the endpoint `https://endpoint.example.org`. This is useful to test the bot in
a local environment. For an easy way to get a public url connected to a local endpoint cf.
[https://ngrok.com](https://ngrok.com).


### AWS SQS mode

The bot is polling the WebEx Teams messages from an AWS SQS queue provided by the configuration
 `spark.sqs` and `spark.sqs_region`. The url of the queue can be registered in WebEx Teams in
the same way as in direct mode.

See configuration example file in [config-sqs.yml](config-sqs.yml) in the repository.

Example:

```shell
$ cargo run -- --config-sqs.yml
```

This is useful, when the bot is running in a private network and does not have a connection to the
internet. SQS is playing the role of a gateway between the internet and the internal traffic.

To forward the WebEx Teams messages to a SQS use an AWS API Gateway.

## Gerrit

To listen to Gerrit messages, you need to have a Gerrit user with `stream-api` access
capabilities. Admins and Non-interactive users should have such.

The state of the bot is stored in the `state.json` file in the same directory, where the bot is
running.

The Gerrit version which was tested is 1.14.x.

## License

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT License ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this document by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
