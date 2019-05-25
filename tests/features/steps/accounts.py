@given("a person named {name} with email address {email}")
def step_impl(context, name, email):
    account = context.accounts.create_person(name, email)
    context.gerrit.create_account(account)


@given("a bot named {name}")
def step_impl(context, name):
    account = context.accounts.create_bot(name)
    context.gerrit.create_account(account)


@given("the following persons")
def step_impl(context):
    for row in context.table:
        context.execute_steps(
            f"given a person named {row['name']} with email address {row['email']}"
        )
