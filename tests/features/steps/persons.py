class Person(object):
    fullname = None
    firstname = None
    email = None


@given("a person named {name} with email address {email}")
def step_impl(context, name, email):
    person = context.persons.create(name, email)
    context.gerrit.create_user(person)
