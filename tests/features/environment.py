import os

from behave import use_fixture

from gerritbot_behave.gerrit import setup_gerrit
from gerritbot_behave.bot import setup_bot
from gerritbot_behave.persons import Persons


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
