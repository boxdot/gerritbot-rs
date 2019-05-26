import re

LABEL_RE = re.compile(r"(?P<label_name>.*)(?P<label_value>[+-]\d+)")


@given("a Gerrit project named {project_name}")
def step_impl(context, project_name):
    context.gerrit.create_project(project_name)


@given("{uploader} uploads a new change to the {project_name} project")
def step_impl(context, uploader, project_name):
    uploader = context.accounts.get_person(uploader)
    context.last_created_change = context.gerrit.create_new_change(
        uploader, project_name
    )


@given(
    '{uploader} creates the file "{filename}" with the following content in the change'
)
def step_impl(context, uploader, filename):
    uploader = context.accounts.get_person(uploader)
    context.gerrit.add_file_to_change(
        uploader, context.last_created_change, filename, context.text
    )


@given("{actor} adds {reviewer} as reviewer to {owner}'s change")
def step_impl(context, actor, reviewer, owner):
    actor = context.accounts.get_person(actor)
    reviewer = context.accounts.get_person(reviewer)
    owner = context.accounts.get_person(owner)
    change = context.gerrit.get_last_change_by(owner)
    context.gerrit.add_reviewer(change, reviewer=reviewer, user=actor)


@given("{actor} submits the change")
def step_impl(context, actor):
    actor = context.accounts.get_person(actor)
    change = context.gerrit.get_last_change_by(actor)
    context.gerrit.submit_change(change, user=actor)


use_step_matcher("re")


@given('(?P<actor>.*) abandons the change(?: with the comment "(?P<comment>.*)")?')
def step_impl(context, actor, comment):
    actor = context.accounts.get_person(actor)
    change = context.gerrit.get_last_change_by(actor)
    context.gerrit.abandon_change(change, comment=comment, user=actor)


def parse_inline_comments(s):
    inline_comments = {}
    filename = None
    for m in re.finditer(
        r"(^Line (?P<line>\d+): (?P<comment>.*)|File: (?P<filename>.*))$",
        s,
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

    return inline_comments


@given(
    "(?P<reviewer>.*) replies to (?P<uploader>.*)'s change with "
    "(?:(?P<label_name>[^ ]*)(?P<label_value>[+-]\d+))?"
    "(?: and )?"
    '(?:the comment "(?P<comment>.*)")?'
    "(?: and )?"
    "(?:"
    "(?P<has_inline_comments>the following inline comments)"
    "|"
    "(?P<has_multiline_comment>the following comment)"
    ")?"
)
def step_impl(
    context,
    reviewer,
    uploader,
    label_name,
    label_value,
    comment,
    has_inline_comments,
    has_multiline_comment,
):
    reviewer = context.accounts.get_account(reviewer)
    uploader = context.accounts.get_person(uploader)
    change = context.gerrit.get_last_change_by(uploader)

    if label_value is not None:
        labels = {label_name: label_value}
    else:
        labels = None

    if has_inline_comments:
        inline_comments = parse_inline_comments(context.text)
    else:
        inline_comments = None

    if has_multiline_comment:
        if comment is not None:
            comment += "\n" + context.text
        else:
            comment = context.text

    context.gerrit.reply(
        change, reviewer, labels=labels, message=comment, comments=inline_comments
    )
