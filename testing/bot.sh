#!/bin/bash
echo "Building the bot ..."
cd /src
cargo build
cargo watch -s 'cargo run -- --config testing/config.yml'
