import os
import warnings
import collections
import subprocess
import random
import json
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
        ssh_key_filename,
        http_url,
        admin_username,
        admin_password,
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
            key_filename=ssh_key_filename,
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
def setup_gerrit(context):
    # TODO: support running gerrit container from here
    admin_username = context.config.userdata.get("gerrit_admin_username", "admin")
    admin_password = context.config.userdata.get("gerrit_admin_password", "secret")
    ssh_host_port = context.config.userdata.get(
        "gerrit_ssh_host_port", "localhost:29418"
    )
    ssh_hostname, _, ssh_port_string = ssh_host_port.partition(":")
    ssh_port = int(ssh_port_string)
    ssh_key_filename = os.path.abspath(
        context.config.userdata.get("gerrit_ssh_key", "testing/data/id_rsa")
    )
    http_url = context.config.userdata.get("gerrit_http_url", "http://localhost:8080")

    context.gerrit = GerritHandler(
        ssh_hostname=ssh_hostname,
        ssh_port=ssh_port,
        ssh_key_filename=ssh_key_filename,
        http_url=http_url,
        admin_username=admin_username,
        admin_password=admin_password,
    )
    yield
    context.gerrit.cleanup()


class BotHandler:
    def __init__(self):
        self.messages = []

    def send_message(self, sender, message):
        pass


@fixture
def setup_bot(context):
    context.bot = BotHandler()


def before_all(context):
    context.config.setup_logging()

    use_fixture(setup_gerrit, context)
    use_fixture(setup_bot, context)


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

        if person.username in self.persons:
            raise ValueError(f"person {username} already exists")

        self.persons[person.username] = person
        return person


def before_scenario(context, scenario):
    context.persons = Persons()
