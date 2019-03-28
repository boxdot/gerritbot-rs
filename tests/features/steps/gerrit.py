@given("a Gerrit project named {project_name}")
def step_impl(context, project_name):
    context.gerrit.create_project(project_name)


@given("{uploader} uploads a new change to the {project_name} project")
def step_impl(context, uploader, project_name):
    uploader = context.persons.get(uploader)
    context.gerrit.create_new_change(uploader, project_name)


@given("{reviewer} replies to {uploader}'s change with {label}")
def step_impl(context, reviewer, uploader, label):
    reviewer = context.persons.get(reviewer)
    uploader = context.persons.get(uploader)
    change = context.gerrit.get_last_change_by(uploader)
    context.gerrit.reply(change, reviewer, label=label)
