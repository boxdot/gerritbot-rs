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


@given(
    '{uploader} creates the file "{filename}" with the following content in the change'
)
def step_impl(context, uploader, filename):
    uploader = context.persons.get(uploader)
    context.gerrit.add_file_to_change(
        uploader, context.last_created_change, filename, context.text
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
    "(?P<reviewer>.*) replies to (?P<uploader>.*)'s change with "
    "(?P<label_name>[^ ]*)(?P<label_value>[+-]\d+)"
    '(?: and the comment "(?P<comment>.*)")?'
    "(?P<has_inline_comments> and the following inline comments)?"
)
def step_impl(
    context, reviewer, uploader, label_name, label_value, comment, has_inline_comments
):
    reviewer = context.persons.get(reviewer)
    uploader = context.persons.get(uploader)
    change = context.gerrit.get_last_change_by(uploader)
    label_value = int(label_value)

    if has_inline_comments:
        inline_comments = {}
        filename = None
        for m in re.finditer(
            r"(^Line (?P<line>\d+): (?P<comment>.*)|File: (?P<filename>.*))$",
            context.text,
            re.MULTILINE,
        ):
            next_filename, line_number_str, line_comment = m.group(
                "filename", "line", "comment"
            )

            if next_filename is not None:
                filename = next_filename
            else:
                line_number = int(line_number_str)
                inline_comments.setdefault(filename, []).append(
                    {"line": line_number, "message": line_comment}
                )

    else:
        inline_comments = None

    context.gerrit.reply(
        change,
        reviewer,
        labels={label_name: label_value},
        message=comment,
        comments=inline_comments,
    )
