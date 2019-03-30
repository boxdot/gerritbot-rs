import collections
import hashlib
import json
import os
import queue
import random
import subprocess
import tempfile
import threading
import logging
import warnings

from urllib.parse import urljoin
from binascii import hexlify

import paramiko
import requests

from behave import use_fixture, fixture


# ssh-ed25519 signature verification is broken
paramiko.Transport._preferred_keys = tuple(
    filter(lambda s: s != "ssh-ed25519", paramiko.Transport._preferred_keys)
)

# paramiko raises some DeprecationWarnings we don't care about
warnings.filterwarnings("ignore", module="paramiko.*")


class SSHCommandError(subprocess.CalledProcessError):
    def __str__(self):
        return (
            super().__str__()
            + (f"\nSTDERR:\n{self.stderr}\n" if self.stderr else "")
            + (f"\nOUTPUT:\n{self.output}\n" if self.output else "")
        )


class SSHClient(paramiko.SSHClient):
    def ext_exec_command(self, command):
        # paramiko.SSHClient currently gives no access to a command's exit
        # status.
        chan = self._transport.open_session()
        chan.exec_command(command)

        stdout = chan.makefile("r")
        stderr = chan.makefile_stderr("r")
        exit_status = chan.recv_exit_status()
        stdout_data = stdout.read()
        stderr_data = stderr.read()

        if exit_status == 0:
            return (stdout_data, stderr_data)
        else:
            raise SSHCommandError(
                cmd=command,
                returncode=exit_status,
                output=stdout_data,
                stderr=stderr_data,
            )


class GerritHandler:
    def __init__(
        self,
        *,
        ssh_hostname,
        ssh_port,
        http_url,
        admin_username,
        admin_password,
        admin_ssh_key_filename,
    ):
        self.ssh_client = SSHClient()
        # ignore SSH host keys
        self.ssh_client.set_missing_host_key_policy(
            paramiko.client.MissingHostKeyPolicy
        )
        self.ssh_client.connect(
            ssh_hostname,
            ssh_port,
            admin_username,
            key_filename=admin_ssh_key_filename,
            look_for_keys=False,
            allow_agent=False,
        )
        self.admin_username = admin_username
        self.admin_password = admin_password
        self.http_url = http_url
        self.projects = []
        self.users = []
        self.changes = []

    @staticmethod
    def parse_json_response(text):
        # Gerrit inserts non-json symbols into the first line
        return json.loads(text.partition("\n")[2])

    def http_post(self, url, *, user, **kwds):
        r = requests.post(
            urljoin(urljoin(self.http_url, "a/"), url.lstrip("/")),
            auth=(user.username, user.http_password),
            **kwds,
        )
        r.raise_for_status()
        return self.parse_json_response(r.text)

    def cleanup(self):
        try:
            # TODO: delete created changes, users, projects
            # TODO: also add an option not to delete anything
            # XXX: probably users cannot be deleted, maybe a workaround is necessary
            pass
        finally:
            self.ssh_client.close()

    def create_project(self, project_name):
        project_group = project_name + "-owners"

        try:
            self.ssh_client.ext_exec_command(f"gerrit create-group {project_group}")
        except SSHCommandError as e:
            if b"already exists" in e.stderr.lower():
                pass
            else:
                raise e

        created = False

        try:
            self.ssh_client.ext_exec_command(
                f"gerrit create-project --empty-commit --owner {project_group} {project_name}"
            )
        except SSHCommandError as e:
            if b"project already exists" in e.stderr.lower():
                pass
            else:
                raise e
        else:
            created = True

        self.projects.append((project_name, project_group, created))

        for (person, created) in self.users:
            self.ssh_client.ext_exec_command(
                f"gerrit set-members --add {person.username} {project_group}"
            )

    def create_user(self, person):
        person.http_password = hexlify(os.urandom(16)).decode("ascii")

        common_args = fr'--full-name "{person.fullname}" --http-password {person.http_password} {person.username}'

        created = False

        try:
            self.ssh_client.ext_exec_command(
                f"gerrit create-account --email {person.email} {common_args}"
            )
        except SSHCommandError as e:
            if b"already exists" in e.stderr.lower():
                self.ssh_client.ext_exec_command(
                    f"gerrit set-account --add-email {person.email} {common_args}"
                )
            else:
                raise e
        else:
            created = True

        self.users.append((person, created))

        for (project_name, project_group, created) in self.projects:
            self.ssh_client.ext_exec_command(
                f"gerrit set-members --add {person.username} {project_group}"
            )

    def create_new_change(self, uploader, project_name):
        change_info = self.http_post(
            "/changes/",
            json={
                "project": project_name,
                "branch": "master",
                "subject": f"new change by {uploader.username}",
                "status": "NEW",
            },
            user=uploader,
        )
        self.changes.append((uploader, change_info, True))

    def get_last_change_by(self, uploader):
        try:
            return next(
                (
                    change_info
                    for (user, change_info, created) in reversed(self.changes)
                    if user.username == uploader.username
                )
            )
        except StopIteration:
            raise ValueError(f"failed to find change by {uploader.username}")

    def reply(self, change, reviewer, *, message=None, labels=None):
        review_data = {}

        if message is not None:
            review_data["message"] = message
        if labels is not None:
            review_data["labels"] = labels

        self.http_post(
            f"/changes/{change['id']}/revisions/current/review",
            json=review_data,
            user=reviewer,
        )


@fixture
def setup_gerrit(
    context,
    *,
    ssh_hostname,
    ssh_port,
    admin_username,
    admin_password,
    admin_ssh_key_filename,
    http_url,
):
    # TODO: support running gerrit container from here
    context.gerrit = GerritHandler(
        ssh_hostname=ssh_hostname,
        ssh_port=ssh_port,
        admin_ssh_key_filename=admin_ssh_key_filename,
        http_url=http_url,
        admin_username=admin_username,
        admin_password=admin_password,
    )
    yield
    context.gerrit.cleanup()


class BotHandler:
    def __init__(self, *, process, message_queue):
        self.process = process
        self.message_queue = message_queue

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
                message = self.message_queue.get(timeout=0.2)
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
def setup_bot(context, *, username, key_filename, hostname, port):
    with tempfile.TemporaryDirectory() as bot_directory:
        bot_args = "cargo run --example gerritbot-console --".split() + [
            "-C",
            bot_directory,
            "--identity-file",
            key_filename,
            "--username",
            username,
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

        message_queue = queue.Queue()

        bot = context.bot = BotHandler(process=bot_process, message_queue=message_queue)
        read_messages_thread = threading.Thread(target=bot._read_messages)
        read_messages_thread.start()
        read_logs_thread = threading.Thread(target=bot._read_logs)
        read_logs_thread.start()

        yield

        bot_process.terminate()
        read_messages_thread.join()
        read_logs_thread.join()


def before_all(context):
    context.config.setup_logging()

    userdata = context.config.userdata

    # read configuration from userdata
    gerrit_ssh_host_port = userdata.get("gerrit_ssh_host_port", "localhost:29418")
    context.gerrit_ssh_hostname, _, gerrit_ssh_port_string = gerrit_ssh_host_port.partition(
        ":"
    )
    context.gerrit_ssh_port = int(gerrit_ssh_port_string)

    context.gerrit_http_url = userdata.get("gerrit_http_url", "http://localhost:8080")

    context.gerrit_admin_username = userdata.get("gerrit_admin_username", "admin")
    context.gerrit_admin_password = userdata.get("gerrit_admin_password", "secret")
    context.gerrit_admin_ssh_key_filename = os.path.abspath(
        userdata.get("gerrit_admin_ssh_key", "testing/data/id_rsa")
    )

    context.gerrit_bot_username = userdata.get(
        "gerrit_bot_username", context.gerrit_admin_username
    )
    context.gerrit_bot_ssh_key_filename = os.path.abspath(
        userdata.get("gerrit_bot_ssh_key", context.gerrit_admin_ssh_key_filename)
    )

    # set up gerrit
    use_fixture(
        setup_gerrit,
        context,
        ssh_hostname=context.gerrit_ssh_hostname,
        ssh_port=context.gerrit_ssh_port,
        admin_username=context.gerrit_admin_username,
        admin_password=context.gerrit_admin_password,
        admin_ssh_key_filename=context.gerrit_admin_ssh_key_filename,
        http_url=context.gerrit_http_url,
    )


class Person:
    name = None
    email = None


class Persons:
    def __init__(self):
        self.persons = {}

    def get(self, name):
        try:
            return self.persons[name.lower()]
        except LookupError:
            raise ValueError(f"a person named {name} doesn't exist")

    def create(self, name, email):
        person = Person()
        person.fullname = name
        person.email = email
        person.username = name.split(None, 1)[0].lower()
        person.webex_teams_person_id = hashlib.sha1(
            person.email.encode("utf-8")
        ).hexdigest()

        if person.username in self.persons:
            raise ValueError(f"person {username} already exists")

        self.persons[person.username] = person
        return person


def before_scenario(context, scenario):
    use_fixture(
        setup_bot,
        context,
        username=context.gerrit_bot_username,
        key_filename=context.gerrit_bot_ssh_key_filename,
        hostname=context.gerrit_ssh_hostname,
        port=context.gerrit_ssh_port,
    )

    context.persons = Persons()
