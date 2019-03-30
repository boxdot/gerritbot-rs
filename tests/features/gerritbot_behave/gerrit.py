import os
import json

from urllib.parse import urljoin
from binascii import hexlify

from behave import fixture

import requests

from .ssh import SSHClient, SSHCommandError


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

