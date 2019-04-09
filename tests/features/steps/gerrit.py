import re

LABEL_RE = re.compile(r"(?P<label_name>.*)(?P<label_value>[+-]\d+)")


@given("a Gerrit project named {project_name}")
def step_impl(context, project_name):
    context.gerrit.create_project(project_name)


@given("{uploader} uploads a new change to the {project_name} project")
def step_impl(context, uploader, project_name):
    uploader = context.persons.get(uploader)
    context.last_created_change = context.gerrit.create_new_change(
        uploader, project_name
    )


@given("{actor} adds {reviewer} as reviewer to {owner}'s change")
def step_impl(context, actor, reviewer, owner):
    actor = context.persons.get(actor)
    reviewer = context.persons.get(reviewer)
    owner = context.persons.get(owner)
    change = context.gerrit.get_last_change_by(owner)
    context.gerrit.add_reviewer(change, reviewer=reviewer, user=actor)


use_step_matcher("re")


@given(
    '(?P<reviewer>.*) replies to (?P<uploader>.*)\'s change with (?P<label_name>[^ ]*)(?P<label_value>[+-]\d+)(?: and the comment "(?P<comment>.*)")?'
)
def step_impl(context, reviewer, uploader, label_name, label_value, comment):
    reviewer = context.persons.get(reviewer)
    uploader = context.persons.get(uploader)
    change = context.gerrit.get_last_change_by(uploader)
    label_value = int(label_value)
    context.gerrit.reply(
        change, reviewer, labels={label_name: label_value}, message=comment
    )
