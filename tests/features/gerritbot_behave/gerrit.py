import itertools
import json
import os
import time

from urllib.parse import urljoin
from binascii import hexlify

from behave import fixture

import requests


class AlreadyExistsError(requests.HTTPError):
    pass


class GerritHandler:
    def __init__(
        self, *, ssh_hostname, ssh_port, http_url, admin_username, admin_password
    ):
        self.admin_username = admin_username
        self.admin_password = admin_password
        self.http_url = http_url
        self.groups = []
        self.projects = []
        self.users = []
        self.changes = []

    @staticmethod
    def parse_json_response(text):
        # Gerrit inserts non-json symbols into the first line
        return json.loads(text.partition("\n")[2])

    def http_request(self, request_method, url, *, user, **kwds):
        if user == "admin":
            auth = (self.admin_username, self.admin_password)
        else:
            auth = (user.username, user.http_password)
        r = request_method(
            urljoin(urljoin(self.http_url, "a/"), url.lstrip("/")), auth=auth, **kwds
        )
        r.raise_for_status()

        if r.status_code != requests.codes.no_content:
            return self.parse_json_response(r.text)

    def http_put(self, *args, **kwds):
        try:
            return self.http_request(requests.put, *args, **kwds)
        except requests.HTTPError as e:
            if e.response.status_code == requests.codes.conflict:
                e.__class__ = AlreadyExistsError

            raise e

    def http_post(self, *args, **kwds):
        return self.http_request(requests.post, *args, **kwds)

    def cleanup(self):
        # TODO: delete created changes, users, projects
        # TODO: also add an option not to delete anything
        # XXX: probably users cannot be deleted, maybe a workaround is necessary
        pass

    def create_group(self, group_name):
        try:
            self.http_put(f"/groups/{group_name}", user="admin", json={})
            created = True
        except AlreadyExistsError:
            created = False

        self.groups.append((group_name, created))

    def add_user_to_group(self, user, group_name):
        self.http_put(f"/groups/{group_name}/members/{user.username}", user="admin")

    def create_project(self, project_name):
        project_group = project_name + "-owners"

        self.create_group(project_group)

        try:
            self.http_put(
                f"/projects/{project_name}",
                user="admin",
                json={"create_empty_commit": True, "owners": [project_group]},
            )
            created = True
        except AlreadyExistsError:
            created = False

        self.projects.append((project_name, project_group, created))

        for (person, created) in self.users:
            self.add_user_to_group(person, project_group)

    def create_user(self, person):
        created = False

        try:
            self.http_put(
                f"/accounts/{person.username}",
                user="admin",
                json={
                    "name": person.fullname,
                    "email": person.email,
                    "ssh_key": f"{person.ssh_key.get_name()} {person.ssh_key.get_base64()}",
                    "http_password": person.http_password,
                },
            )
        except AlreadyExistsError:
            self.http_put(
                f"/accounts/{person.username}/password.http",
                user="admin",
                json={"http_password": person.http_password},
            )
            self.http_post(
                f"/accounts/{person.username}/sshkeys",
                user="admin",
                headers={"content_type": "text/plain"},
                data=f"{person.ssh_key.get_name()} {person.ssh_key.get_base64()}",
            )
        else:
            created = True

        self.users.append((person, created))

        for (project_name, project_group, created) in self.projects:
            self.add_user_to_group(person, project_group)

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
        return change_info

    def add_file_to_change(self, uploader, change_info, filename, content):
        self.http_put(
            f"/changes/{change_info['_number']}/edit/{filename}",
            user=uploader,
            data=content.encode("utf-8"),
        )
        self.http_post(f"/changes/{change_info['_number']}/edit:publish", user=uploader)

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

    def reply(self, change, reviewer, *, message=None, labels=None, comments=None):
        review_data = {}

        if message is not None:
            review_data["message"] = message
        if labels is not None:
            review_data["labels"] = labels
        if comments is not None:
            review_data["comments"] = comments

        self.http_post(
            f"/changes/{change['_number']}/revisions/current/review",
            json=review_data,
            user=reviewer,
        )

    def add_reviewer(self, change, reviewer, *, user):
        self.http_post(
            f"/changes/{change['_number']}/reviewers",
            json={"reviewer": reviewer.email},
            user=user,
        )


@fixture
def setup_gerrit(
    context,
    *,
    ssh_hostname,
    ssh_port,
    admin_username,
    admin_password,
    http_url,
    gerrit_start_timeout,
):
    # TODO: support running gerrit container from here

    if gerrit_start_timeout is not None:
        t0 = time.monotonic()

        for i in itertools.count():
            try:
                requests.get(http_url).raise_for_status()
            except requests.ConnectionError:
                current_wait_time = time.monotonic() - t0

                if current_wait_time > gerrit_start_timeout:
                    raise ValueError(f"Failed to reach Gerrit at {http_url}")
                else:
                    if i == 0:
                        print("Waiting for Gerrit to start ...")
                    elif i % 10 == 0:
                        print(f"Still waiting after {current_wait_time:.2f}s ...")

                    time.sleep(0.5)
            else:
                if i != 0:
                    current_wait_time = time.monotonic() - t0
                    print(f"Gerrit up after {current_wait_time:.2f}s seconds")
                break

    context.gerrit = GerritHandler(
        ssh_hostname=ssh_hostname,
        ssh_port=ssh_port,
        http_url=http_url,
        admin_username=admin_username,
        admin_password=admin_password,
    )

    yield
    context.gerrit.cleanup()
