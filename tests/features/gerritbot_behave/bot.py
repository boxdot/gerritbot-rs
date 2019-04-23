import json
import logging
import os
import queue
import subprocess
import tempfile
import threading

from behave import fixture


class BotHandler:
    def __init__(self, *, process, message_queue, message_timeout):
        self.process = process
        self.message_queue = message_queue
        self.message_timeout = message_timeout

    def send_message(self, sender, message):
        log = logging.getLogger("bot-messages")
        serialized_message = json.dumps(
            {
                "personEmail": sender.email,
                "personId": sender.webex_teams_person_id,
                "text": message,
            }
        ).encode("utf-8")
        log.debug("sending message to bot: %r", serialized_message)
        self.process.stdin.write(serialized_message)
        self.process.stdin.write(b"\n")
        self.process.stdin.flush()

    def get_messages(self):
        messages = []

        while True:
            # XXX: we should find a better way to check if there are no more
            # messages coming
            try:
                message = self.message_queue.get(timeout=self.message_timeout)
            except queue.Empty:
                break
            else:
                messages.append(message)

        self.current_messages = messages
        return messages

    def get_messages_for_person(self, person):
        return [
            m
            for m in self.current_messages
            if m["personId"] == person.webex_teams_person_id
        ]

    def _read_messages(self):
        log = logging.getLogger("bot-messages")

        log.debug("starting to read messages from bot")

        for line in self.process.stdout:
            try:
                message = json.loads(line)
            except:
                log.exception("failed to parse JSON message: %s", line)
            else:
                log.debug("got bot message: %r", message)
                self.message_queue.put(message)

    def _read_logs(self):
        log = logging.getLogger("bot-log")

        # XXX: try to parse log level from bot output

        for line in self.process.stderr:
            log.info("%s", line.decode("utf-8").rstrip("\n"))


@fixture
def setup_bot(context, *, user, hostname, port, message_timeout):
    with tempfile.TemporaryDirectory() as bot_directory:
        user.ssh_key.write_private_key_file(os.path.join(bot_directory, "id_rsa"))
        with open(os.path.join(bot_directory, "id_rsa.pub"), "w") as f:
            f.write(f"{user.ssh_key.get_name()} {user.ssh_key.get_base64()}")

        bot_args = "cargo run --example gerritbot-console --".split() + [
            "-C",
            bot_directory,
            "--identity-file",
            "id_rsa",
            "--username",
            user.username,
            "--port",
            str(port),
            # XXX: allow enabling --verbose with a userdefine
            # "--verbose",
            "--json",
            hostname,
        ]

        bot_process = subprocess.Popen(
            args=bot_args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

        # Using cargo run above means we might actually be compiling still.
        # Wait for the bot to be ready.
        for line in bot_process.stderr:
            if b"Connected to Gerrit" in line:
                break

        message_queue = queue.Queue()

        bot = context.bot = BotHandler(
            process=bot_process,
            message_queue=message_queue,
            message_timeout=message_timeout,
        )
        read_messages_thread = threading.Thread(target=bot._read_messages)
        read_messages_thread.start()

        read_logs_thread = threading.Thread(target=bot._read_logs)
        read_logs_thread.start()

        yield

        bot_process.terminate()
        read_messages_thread.join()
        read_logs_thread.join()
