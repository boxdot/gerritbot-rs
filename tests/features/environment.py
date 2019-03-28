from behave import use_fixture, fixture

class Gerrit:
    def create_project(self, project_name):
        pass

    def create_new_change(self, uploader, project_name):
        pass

    def get_last_change_by(self, uploader):
        pass

    def reply(self, change, reviewer, *, message=None, label=None):
        pass

@fixture
def run_gerrit(context):
    import time
    print("Starting Gerrit ...")
    time.sleep(1)
    context.gerrit = Gerrit()

class Bot:
    def __init__(self):
        self.messages = []

    def send_message(self, sender, message):
        pass

@fixture
def run_bot(context):
    print("Starting bot ...")
    import time
    time.sleep(1)
    context.bot = Bot()

def before_all(context):
    use_fixture(run_gerrit, context)
    use_fixture(run_bot, context)

class Person:
    name = None
    email = None

class Persons:
    def __init__(self):
        self.persons = {}

    def get(self, name):
        try:
            return self.persons[name.lower()]
        except LookupError:
            raise ValueError(f"a person named {name} doesn't exist")

    def create(self, name, email):
        person = Person()
        person.fullname = name
        person.email = email
        firstname = name.split()[0]

        if firstname.lower() in self.persons:
            raise ValueError(f"a person named {firstname} already exists")

        self.persons[firstname.lower()] = person


def before_scenario(context, scenario):
    context.persons = Persons()
