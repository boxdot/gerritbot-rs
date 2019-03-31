class Person(object):
    fullname = None
    firstname = None
    email = None


@given("a person named {name} with email address {email}")
def step_impl(context, name, email):
    person = context.persons.create(name, email)
    context.gerrit.create_user(person)


@given("the following persons")
def step_impl(context):
    for row in context.table:
        context.execute_steps(
            f"given a person named {row['name']} with email address {row['email']}"
        )
