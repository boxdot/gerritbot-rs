import hashlib


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
        person.username = name.split(None, 1)[0].lower()
        person.webex_teams_person_id = hashlib.sha1(
            person.email.encode("utf-8")
        ).hexdigest()

        if person.username in self.persons:
            raise ValueError(f"person {username} already exists")

        self.persons[person.username] = person
        return person
